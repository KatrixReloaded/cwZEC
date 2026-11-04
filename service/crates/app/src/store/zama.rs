//! Durable chain facts and signed operator identities. None of these rows authorize payment.
use super::{Store, fixed};
use alloy::primitives::{Address, B256};
use anyhow::{Result, ensure};
use tokio_postgres::{Row, Transaction};
use zama::{
    contract::SignedTransaction,
    watcher::{BlockRef, EventObservation, RETAIN_BLOCKS, ReceiptObservation, ScanBatch},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZamaCursor {
    pub chain_id: u64,
    pub contract: Address,
    pub deployment_block: u64,
    pub tip: BlockRef,
    pub halted: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransactionState {
    Pending,
    Successful,
    Reverted,
    Unresolved,
}

pub struct ZamaTransaction {
    pub operation_id: String,
    pub signed: SignedTransaction,
    pub state: TransactionState,
    pub confirmed: bool,
}

impl Store {
    pub async fn zama_cursor(&self) -> Result<Option<ZamaCursor>> {
        self.client
            .query_opt("SELECT * FROM zama_chain", &[])
            .await?
            .map(cursor)
            .transpose()
    }

    pub async fn initialize_zama(
        &mut self,
        chain_id: u64,
        contract: Address,
        deployment_block: u64,
        anchor: BlockRef,
    ) -> Result<()> {
        ensure!(
            chain_id > 0
                && !contract.is_zero()
                && deployment_block > 0
                && anchor.number.checked_add(1) == Some(deployment_block),
            "invalid Zama scan identity"
        );
        let tx = self.client.transaction().await?;
        tx.batch_execute("LOCK TABLE zama_chain IN EXCLUSIVE MODE")
            .await?;
        if let Some(row) = tx.query_opt("SELECT * FROM zama_chain", &[]).await? {
            let existing = cursor(row)?;
            ensure!(
                existing.chain_id == chain_id
                    && existing.contract == contract
                    && existing.deployment_block == deployment_block,
                "persisted Zama configuration mismatch"
            );
        } else {
            tx.execute("INSERT INTO zama_chain(singleton,chain_id,contract,deployment_block,tip_number,tip_hash) VALUES(true,$1,$2,$3,$4,$5)", &[&num(chain_id)?, &contract.as_slice(), &num(deployment_block)?, &num(anchor.number)?, &anchor.hash.as_slice()]).await?;
            tx.execute(
                "INSERT INTO zama_blocks(number,hash,parent_hash) VALUES($1,$2,$3)",
                &[
                    &num(anchor.number)?,
                    &anchor.hash.as_slice(),
                    &B256::ZERO.as_slice(),
                ],
            )
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn halt_zama(&mut self, reason: &str) -> Result<()> {
        ensure!(
            matches!(reason, "history_exhausted" | "acted_operation_reorg"),
            "invalid halt reason"
        );
        self.client
            .execute(
                "UPDATE zama_chain SET halted=true,halt_reason=$1",
                &[&reason],
            )
            .await?;
        Ok(())
    }

    pub async fn zama_history(&self) -> Result<Vec<BlockRef>> {
        self.client
            .query(
                "SELECT number,hash FROM zama_blocks ORDER BY number DESC",
                &[],
            )
            .await?
            .into_iter()
            .map(|r| {
                Ok(BlockRef {
                    number: r.get::<_, i64>(0) as u64,
                    hash: B256::from(fixed::<32>(r.get(1))?),
                })
            })
            .collect()
    }

    /// Cursor and all facts commit together; replay of a stale batch changes nothing.
    pub async fn apply_zama_scan(&mut self, batch: &ScanBatch) -> Result<()> {
        let tx = self.client.transaction().await?;
        let old = lock_chain(&tx).await?;
        ensure!(!old.halted, "Zama watcher halted");
        ensure!(
            old.chain_id == batch.chain_id
                && old.contract == batch.contract
                && old.tip == batch.previous,
            "stale or wrong-domain scan"
        );
        batch.validate_headers()?;
        for block in &batch.blocks {
            tx.execute(
                "INSERT INTO zama_blocks(number,hash,parent_hash) VALUES($1,$2,$3)",
                &[
                    &num(block.reference.number)?,
                    &block.reference.hash.as_slice(),
                    &block.parent_hash.as_slice(),
                ],
            )
            .await?;
        }
        for event in &batch.events {
            insert_event(&tx, event, event.block.number < batch.end.number).await?;
        }
        for receipt in &batch.receipts {
            ensure!(
                receipt.to == old.contract && receipt.block.number <= batch.end.number,
                "receipt outside configured scan"
            );
            // A historical receipt may be refreshed after inclusion. Its canonical
            // hash is checked by the watcher before this atomic commit.
            insert_receipt(&tx, receipt, receipt.block.number < batch.end.number).await?;
        }
        tx.execute(
            "UPDATE zama_events SET confirmed=(canonical AND block_number < $1)",
            &[&num(batch.end.number)?],
        )
        .await?;
        tx.execute(
            "UPDATE zama_receipts SET confirmed=(canonical AND block_number < $1)",
            &[&num(batch.end.number)?],
        )
        .await?;
        tx.execute("UPDATE zama_transactions t SET state=CASE WHEN r.success THEN 'successful' ELSE 'reverted' END,confirmed=r.confirmed FROM zama_receipts r WHERE r.tx_hash=t.tx_hash AND r.canonical AND r.sender=t.sender", &[]).await?;
        tx.execute(
            "UPDATE zama_chain SET tip_number=$1,tip_hash=$2",
            &[&num(batch.end.number)?, &batch.end.hash.as_slice()],
        )
        .await?;
        tx.execute(
            "DELETE FROM zama_blocks WHERE number < $1",
            &[&num(batch.end.number.saturating_sub(RETAIN_BLOCKS))?],
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Orphan facts without deleting transaction identities or releasing nonce ownership.
    pub async fn rewind_zama(&mut self, expected_tip: BlockRef, ancestor: BlockRef) -> Result<()> {
        let tx = self.client.transaction().await?;
        let old = lock_chain(&tx).await?;
        ensure!(
            !old.halted && old.tip == expected_tip,
            "stale or halted rewind"
        );
        ensure!(
            ancestor.number >= old.deployment_block - 1 && ancestor.number < old.tip.number,
            "invalid rewind range"
        );
        let row = tx
            .query_opt(
                "SELECT hash FROM zama_blocks WHERE number=$1",
                &[&num(ancestor.number)?],
            )
            .await?
            .ok_or_else(|| anyhow::anyhow!("rewind history exhausted"))?;
        ensure!(
            row.get::<_, Vec<u8>>(0) == ancestor.hash.as_slice(),
            "rewind hash mismatch"
        );
        // An external action, or using the released lane for a later nonce,
        if super::bridge::bridge_zama_rewind(&tx, ancestor).await? {
            tx.commit().await?;
            anyhow::bail!("EVM reorg affects bridge action; reconciliation halted");
        }
        // makes a removed receipt require reconciliation rather than replay.
        let acted: bool = tx.query_one("SELECT EXISTS(SELECT 1 FROM zama_receipts r LEFT JOIN zama_transactions t ON t.tx_hash=r.tx_hash WHERE r.canonical AND r.block_number > $1 AND (r.acted_on OR EXISTS(SELECT 1 FROM zama_transactions later WHERE later.sender=t.sender AND later.nonce>t.nonce)))", &[&num(ancestor.number)?]).await?.get(0);
        if acted {
            tx.execute(
                "UPDATE zama_chain SET halted=true,halt_reason='acted_operation_reorg'",
                &[],
            )
            .await?;
            tx.commit().await?;
            anyhow::bail!("reorg affects an acted-on operation; watcher halted");
        }
        tx.execute("UPDATE zama_transactions SET state='unresolved',confirmed=false WHERE tx_hash IN (SELECT tx_hash FROM zama_receipts WHERE canonical AND block_number > $1)", &[&num(ancestor.number)?]).await?;
        tx.execute(
            "UPDATE zama_events SET canonical=false,confirmed=false WHERE canonical AND block_number > $1",
            &[&num(ancestor.number)?],
        )
        .await?;
        tx.execute(
            "UPDATE zama_receipts SET canonical=false,confirmed=false WHERE canonical AND block_number > $1",
            &[&num(ancestor.number)?],
        )
        .await?;
        tx.execute(
            "UPDATE zama_events SET confirmed=(canonical AND block_number < $1)",
            &[&num(ancestor.number)?],
        )
        .await?;
        tx.execute(
            "UPDATE zama_receipts SET confirmed=(canonical AND block_number < $1)",
            &[&num(ancestor.number)?],
        )
        .await?;
        tx.execute("UPDATE zama_transactions t SET confirmed=r.confirmed FROM zama_receipts r WHERE r.tx_hash=t.tx_hash AND r.canonical", &[]).await?;
        tx.execute(
            "DELETE FROM zama_blocks WHERE number > $1",
            &[&num(ancestor.number)?],
        )
        .await?;
        tx.execute(
            "UPDATE zama_chain SET tip_number=$1,tip_hash=$2",
            &[&num(ancestor.number)?, &ancestor.hash.as_slice()],
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// One operator owns this lane. Never adopt a changed network nonce on restart.
    pub async fn initialize_zama_lane(
        &mut self,
        sender: Address,
        latest_nonce: u64,
        pending_nonce: u64,
    ) -> Result<()> {
        ensure!(!sender.is_zero(), "zero lane sender");
        let tx = self.client.transaction().await?;
        let chain = lock_chain(&tx).await?;
        ensure!(!chain.halted, "Zama watcher halted");
        if let Some(row) = tx
            .query_opt("SELECT sender FROM zama_lane FOR UPDATE", &[])
            .await?
        {
            ensure!(
                row.get::<_, Vec<u8>>(0) == sender.as_slice(),
                "operator lane sender mismatch"
            );
        } else {
            ensure!(
                latest_nonce == pending_nonce,
                "operator has unknown pending transactions"
            );
            tx.execute(
                "INSERT INTO zama_lane(singleton,sender,next_nonce) VALUES(true,$1,$2)",
                &[&sender.as_slice(), &num(latest_nonce)?],
            )
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn zama_next_nonce(&self, sender: Address) -> Result<u64> {
        let row = self
            .client
            .query_one(
                "SELECT next_nonce FROM zama_lane WHERE sender=$1",
                &[&sender.as_slice()],
            )
            .await?;
        Ok(row.get::<_, i64>(0) as u64)
    }

    /// The commit is the broadcast boundary. Both operation and nonce are immutable.
    pub async fn save_zama_signed(
        &mut self,
        operation_id: &str,
        signed: &SignedTransaction,
    ) -> Result<()> {
        ensure!(
            !operation_id.is_empty() && operation_id.len() <= 128,
            "invalid operation identity"
        );
        let tx = self.client.transaction().await?;
        let chain = lock_chain(&tx).await?;
        ensure!(
            !chain.halted
                && chain.chain_id == signed.chain_id()
                && chain.contract == signed.contract(),
            "signed domain mismatch or halted"
        );
        if let Some(row) = tx
            .query_opt(
                "SELECT raw FROM zama_transactions WHERE operation_id=$1",
                &[&operation_id],
            )
            .await?
        {
            ensure!(
                row.get::<_, Vec<u8>>(0) == signed.raw().as_ref(),
                "operation already owns a different transaction"
            );
        } else {
            super::bridge::validate_bridge_signed(&tx, operation_id, signed).await?;
            let lane = tx
                .query_one("SELECT sender,next_nonce FROM zama_lane FOR UPDATE", &[])
                .await?;
            ensure!(
                lane.get::<_, Vec<u8>>(0) == signed.sender().as_slice()
                    && lane.get::<_, i64>(1) == num(signed.nonce())?,
                "nonce ownership conflict"
            );
            let busy: bool = tx
                .query_one(
                    "SELECT EXISTS(SELECT 1 FROM zama_transactions WHERE NOT confirmed)",
                    &[],
                )
                .await?
                .get(0);
            ensure!(!busy, "operator lane has an unresolved transaction");
            tx.execute("INSERT INTO zama_transactions(operation_id,sender,nonce,tx_hash,raw,state) VALUES($1,$2,$3,$4,$5,'unresolved')", &[&operation_id, &signed.sender().as_slice(), &num(signed.nonce())?, &signed.hash().as_slice(), &signed.raw().as_ref()]).await?;
            tx.execute(
                "UPDATE zama_lane SET next_nonce=$1",
                &[&num(signed
                    .nonce()
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("nonce overflow"))?)?],
            )
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn mark_zama_submission(&mut self, hash: B256, accepted: bool) -> Result<()> {
        self.client.execute("UPDATE zama_transactions SET state=$2 WHERE tx_hash=$1 AND state IN ('pending','unresolved')", &[&hash.as_slice(), &if accepted {"pending"} else {"unresolved"}]).await?;
        Ok(())
    }

    pub async fn zama_transaction(&self, operation_id: &str) -> Result<Option<ZamaTransaction>> {
        let Some(chain) = self.zama_cursor().await? else {
            return Ok(None);
        };
        self.client
            .query_opt(
                "SELECT * FROM zama_transactions WHERE operation_id=$1",
                &[&operation_id],
            )
            .await?
            .map(|r| transaction(r, &chain))
            .transpose()
    }

    pub async fn zama_unfinished(&self) -> Result<Vec<ZamaTransaction>> {
        let chain = self
            .zama_cursor()
            .await?
            .ok_or_else(|| anyhow::anyhow!("Zama not initialized"))?;
        self.client
            .query(
                "SELECT * FROM zama_transactions WHERE NOT confirmed ORDER BY nonce",
                &[],
            )
            .await?
            .into_iter()
            .map(|r| transaction(r, &chain))
            .collect()
    }

    /// Durable pre-action marker, not a payment API. Only canonical successful
    /// receipts with two confirmations qualify; a revert never qualifies.
    pub async fn mark_zama_acted(&mut self, hash: B256) -> Result<()> {
        let tx = self.client.transaction().await?;
        ensure!(!lock_chain(&tx).await?.halted, "Zama watcher halted");
        let changed = tx.execute("UPDATE zama_receipts SET acted_on=true WHERE tx_hash=$1 AND canonical AND confirmed AND success", &[&hash.as_slice()]).await?;
        ensure!(changed == 1, "successful two-confirmation receipt required");
        tx.commit().await?;
        Ok(())
    }

    pub async fn zama_observation_counts(&self) -> Result<(u64, u64, u64)> {
        let r = self.client.query_one("SELECT (SELECT count(*) FROM zama_events WHERE canonical), (SELECT count(*) FROM zama_events WHERE confirmed), (SELECT count(*) FROM zama_receipts WHERE confirmed)", &[]).await?;
        Ok((
            r.get::<_, i64>(0) as u64,
            r.get::<_, i64>(1) as u64,
            r.get::<_, i64>(2) as u64,
        ))
    }

    pub async fn zama_events(&self) -> Result<Vec<EventObservation>> {
        self.client
            .query(
                "SELECT * FROM zama_events WHERE canonical ORDER BY block_number,log_index",
                &[],
            )
            .await?
            .into_iter()
            .map(|r| {
                Ok(EventObservation {
                    block: BlockRef {
                        number: r.get::<_, i64>("block_number") as u64,
                        hash: B256::from(fixed::<32>(r.get("block_hash"))?),
                    },
                    transaction_hash: B256::from(fixed::<32>(r.get("tx_hash"))?),
                    log_index: r.get::<_, i64>("log_index") as u64,
                    event: serde_json::from_str(r.get("event"))?,
                })
            })
            .collect()
    }
}

fn num(n: u64) -> Result<i64> {
    Ok(i64::try_from(n)?)
}
fn cursor(r: Row) -> Result<ZamaCursor> {
    Ok(ZamaCursor {
        chain_id: r.get::<_, i64>("chain_id") as u64,
        contract: Address::from(fixed::<20>(r.get("contract"))?),
        deployment_block: r.get::<_, i64>("deployment_block") as u64,
        tip: BlockRef {
            number: r.get::<_, i64>("tip_number") as u64,
            hash: B256::from(fixed::<32>(r.get("tip_hash"))?),
        },
        halted: r.get("halted"),
    })
}
async fn lock_chain(tx: &Transaction<'_>) -> Result<ZamaCursor> {
    cursor(
        tx.query_one("SELECT * FROM zama_chain FOR UPDATE", &[])
            .await?,
    )
}
fn transaction(r: Row, chain: &ZamaCursor) -> Result<ZamaTransaction> {
    let signed = SignedTransaction::restore(
        chain.chain_id,
        chain.contract,
        Address::from(fixed::<20>(r.get("sender"))?),
        r.get::<_, Vec<u8>>("raw").into(),
    )?;
    ensure!(
        signed.hash().as_slice() == r.get::<_, Vec<u8>>("tx_hash")
            && signed.nonce() == r.get::<_, i64>("nonce") as u64,
        "stored signed identity corrupt"
    );
    let state = match r.get::<_, &str>("state") {
        "pending" => TransactionState::Pending,
        "successful" => TransactionState::Successful,
        "reverted" => TransactionState::Reverted,
        "unresolved" => TransactionState::Unresolved,
        _ => anyhow::bail!("invalid transaction state"),
    };
    Ok(ZamaTransaction {
        operation_id: r.get("operation_id"),
        signed,
        state,
        confirmed: r.get("confirmed"),
    })
}
async fn insert_event(tx: &Transaction<'_>, e: &EventObservation, confirmed: bool) -> Result<()> {
    let data = serde_json::to_string(&e.event)?;
    if let Some(r)=tx.query_opt("SELECT event,block_number FROM zama_events WHERE block_hash=$1 AND tx_hash=$2 AND log_index=$3", &[&e.block.hash.as_slice(), &e.transaction_hash.as_slice(), &num(e.log_index)?]).await? {
        ensure!(r.get::<_,String>(0)==data && r.get::<_,i64>(1)==num(e.block.number)?,"conflicting duplicate event");
    }
    tx.execute("INSERT INTO zama_events(block_number,block_hash,tx_hash,log_index,event,canonical,confirmed) VALUES($1,$2,$3,$4,$5,true,$6) ON CONFLICT(block_hash,tx_hash,log_index) DO UPDATE SET canonical=true,confirmed=EXCLUDED.confirmed", &[&num(e.block.number)?, &e.block.hash.as_slice(), &e.transaction_hash.as_slice(), &num(e.log_index)?, &data, &confirmed]).await?;
    Ok(())
}
async fn insert_receipt(
    tx: &Transaction<'_>,
    r: &ReceiptObservation,
    confirmed: bool,
) -> Result<()> {
    if let Some(old)=tx.query_opt("SELECT block_number,sender,recipient,success,confirmed FROM zama_receipts WHERE tx_hash=$1 AND block_hash=$2", &[&r.transaction_hash.as_slice(), &r.block.hash.as_slice()]).await? {
        ensure!(old.get::<_,i64>(0)==num(r.block.number)? && old.get::<_,Vec<u8>>(1)==r.from.as_slice() && old.get::<_,Vec<u8>>(2)==r.to.as_slice() && old.get::<_,bool>(3)==r.success,"conflicting receipt");
    }
    tx.execute("INSERT INTO zama_receipts(tx_hash,block_number,block_hash,sender,recipient,success,canonical,confirmed) VALUES($1,$2,$3,$4,$5,$6,true,$7) ON CONFLICT(tx_hash,block_hash) DO UPDATE SET canonical=true,confirmed=EXCLUDED.confirmed", &[&r.transaction_hash.as_slice(), &num(r.block.number)?, &r.block.hash.as_slice(), &r.from.as_slice(), &r.to.as_slice(), &r.success, &confirmed]).await?;
    Ok(())
}
