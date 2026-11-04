//! Deposit-only bridge foundation. Every SQL mutation shares the chain serialization barrier.
use super::{Store, fixed, lock_account};
use crate::core::{bridge_types::*, deposit_id, parse_memo};
use alloy::primitives::{Address, B256};
use anyhow::{Context, Result, ensure};
use tokio_postgres::{Row, Transaction};
use zama::{
    contract::{DepositStatus, SignedTransaction},
    watcher::{BlockRef, Event, EventObservation},
};

impl Store {
    /// Recheck an unsigned reserved payment after restart and immediately before signing.
    pub async fn check_payment_accounting(&mut self) -> Result<()> {
        let tx = self.client.transaction().await?;
        lock_bridge(&tx).await?;
        check_payment_accounting(&tx).await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn record_bridge_origin(
        &mut self,
        e: &zama::contract::DeploymentEvidence,
    ) -> Result<()> {
        let tx = self.client.transaction().await?;
        lock_bridge(&tx).await?;
        check_evm_checkpoint(&tx, e.block).await?;
        let deployment: i64 = tx
            .query_one("SELECT deployment_block FROM zama_chain", &[])
            .await?
            .get(0);
        ensure!(
            deployment == num(e.block.number)?,
            "deployment block mismatch"
        );
        let changed = tx.execute("INSERT INTO bridge_origin VALUES(true,$1,$2,$3,$4,$5,$6) ON CONFLICT(singleton) DO UPDATE SET singleton=true WHERE bridge_origin.deployment_number=EXCLUDED.deployment_number AND bridge_origin.deployment_hash=EXCLUDED.deployment_hash AND bridge_origin.deployment_tx=EXCLUDED.deployment_tx AND bridge_origin.runtime_hash=EXCLUDED.runtime_hash AND bridge_origin.service=EXCLUDED.service AND bridge_origin.auditor=EXCLUDED.auditor", &[&num(e.block.number)?, &e.block.hash.as_slice(), &e.transaction.as_slice(), &e.runtime_hash.as_slice(), &e.service.as_slice(), &e.auditor.as_slice()]).await?;
        ensure!(changed == 1, "immutable origin mismatch");
        tx.commit().await?;
        Ok(())
    }

    pub async fn bridge_origin(&self) -> Result<Option<zama::contract::DeploymentEvidence>> {
        self.client
            .query_opt("SELECT * FROM bridge_origin", &[])
            .await?
            .map(|r| {
                Ok(zama::contract::DeploymentEvidence {
                    block: BlockRef {
                        number: u64::try_from(r.get::<_, i64>("deployment_number"))?,
                        hash: B256::from(fixed::<32>(r.get("deployment_hash"))?),
                    },
                    transaction: B256::from(fixed::<32>(r.get("deployment_tx"))?),
                    runtime_hash: B256::from(fixed::<32>(r.get("runtime_hash"))?),
                    service: Address::from(fixed::<20>(r.get("service"))?),
                    auditor: Address::from(fixed::<20>(r.get("auditor"))?),
                })
            })
            .transpose()
    }

    /// Irreversible reconciliation stop; deliberately provides no automatic reset.
    pub async fn halt_bridge(&mut self, reason: &str) -> Result<()> {
        ensure!(
            !reason.is_empty() && reason.len() <= 128,
            "invalid bridge halt reason"
        );
        let tx = self.client.transaction().await?;
        tx.query_opt("SELECT singleton FROM zama_chain FOR UPDATE", &[])
            .await?;
        if tx
            .query_opt("SELECT singleton FROM bridge_domain FOR UPDATE", &[])
            .await?
            .is_some()
        {
            halt(&tx, reason).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn initialize_bridge(&mut self, domain: BridgeDomain) -> Result<()> {
        ensure!(
            domain.chain_id == 11155111 && !domain.contract.is_zero(),
            "unsupported bridge domain"
        );
        let tx = self.client.transaction().await?;
        lock_account(&tx).await?;
        let chain = tx
            .query_one("SELECT * FROM zama_chain FOR UPDATE", &[])
            .await?;
        ensure!(
            chain.get::<_, i64>("chain_id") == num(domain.chain_id)?
                && chain.get::<_, Vec<u8>>("contract") == domain.contract.as_slice(),
            "bridge/Zama domain mismatch"
        );
        tx.batch_execute("LOCK TABLE bridge_domain IN EXCLUSIVE MODE")
            .await?;
        if let Some(r) = tx.query_opt("SELECT * FROM bridge_domain", &[]).await? {
            ensure!(
                r.get::<_, i64>("chain_id") == num(domain.chain_id)?
                    && r.get::<_, Vec<u8>>("contract") == domain.contract.as_slice(),
                "immutable bridge domain mismatch"
            );
        } else {
            tx.execute(
                "INSERT INTO bridge_domain(singleton,chain_id,contract) VALUES(true,$1,$2)",
                &[&num(domain.chain_id)?, &domain.contract.as_slice()],
            )
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn bridge_checkpoint_pair(&mut self) -> Result<BridgeCheckpointPair> {
        let tx = self.client.transaction().await?;
        lock_bridge(&tx).await?;
        let pair = checkpoint_pair(&tx).await?;
        tx.commit().await?;
        Ok(pair)
    }

    /// Only scanner-accepted external notes at three confirmations enter the machine.
    pub async fn ingest_bridge_deposits(&mut self) -> Result<Vec<BridgeDeposit>> {
        let tx = self.client.transaction().await?;
        lock_bridge(&tx).await?;
        let rows = tx.query("SELECT n.*,c.hash AS source_hash FROM notes n JOIN checkpoints c ON c.height=n.mined_height JOIN account a ON true WHERE n.canonical AND NOT n.internal AND n.mined_height<=a.current_height-2 ORDER BY n.mined_height,n.position", &[]).await?;
        for r in rows {
            let rho = fixed::<32>(r.get("rho"))?;
            let rseed = fixed::<32>(r.get("rseed"))?;
            let id = B256::from(deposit_id(&rho, &rseed));
            let memo = fixed::<512>(r.get("memo"))?;
            let recipient = parse_memo(&memo)
                .ok()
                .map(Address::from)
                .filter(|a| !a.is_zero() && r.get::<_, i64>("value") > 0);
            let recipient_bytes = recipient.map(|a| a.to_vec());
            let state = if recipient.is_some() {
                "eligible"
            } else {
                "refund_pending"
            };
            let op = format!("cwzec/mint/{}", hex::encode(id));
            let changed = tx.execute("INSERT INTO bridge_deposits(deposit_id,txid,action_index,amount,recipient,source_height,source_hash,canonical,state,operation_id) VALUES($1,$2,$3,$4,$5,$6,$7,true,$8,$9) ON CONFLICT(deposit_id) DO UPDATE SET canonical=true,source_height=EXCLUDED.source_height,source_hash=EXCLUDED.source_hash,version=bridge_deposits.version+CASE WHEN NOT bridge_deposits.canonical OR bridge_deposits.source_height<>EXCLUDED.source_height OR bridge_deposits.source_hash<>EXCLUDED.source_hash THEN 1 ELSE 0 END WHERE bridge_deposits.txid=EXCLUDED.txid AND bridge_deposits.action_index=EXCLUDED.action_index AND bridge_deposits.amount=EXCLUDED.amount AND bridge_deposits.recipient IS NOT DISTINCT FROM EXCLUDED.recipient AND (NOT bridge_deposits.claimed OR (bridge_deposits.canonical AND bridge_deposits.source_height=EXCLUDED.source_height AND bridge_deposits.source_hash=EXCLUDED.source_hash))", &[&id.as_slice(), &r.get::<_,Vec<u8>>("txid"), &r.get::<_,i64>("action_index"), &r.get::<_,i64>("value"), &recipient_bytes, &r.get::<_,i64>("mined_height"), &r.get::<_,Vec<u8>>("source_hash"), &state, &op]).await?;
            ensure!(changed == 1, "conflicting immutable deposit");
        }
        let result = tx
            .query(
                "SELECT * FROM bridge_deposits ORDER BY source_height,deposit_id",
                &[],
            )
            .await?
            .into_iter()
            .map(deposit)
            .collect::<Result<Vec<_>>>()?;
        tx.commit().await?;
        Ok(result)
    }

    pub async fn bridge_deposits(&self) -> Result<Vec<BridgeDeposit>> {
        self.client
            .query(
                "SELECT * FROM bridge_deposits ORDER BY source_height,deposit_id",
                &[],
            )
            .await?
            .into_iter()
            .map(deposit)
            .collect()
    }
    pub async fn bridge_deposit(&self, id: B256) -> Result<Option<BridgeDeposit>> {
        self.client
            .query_opt(
                "SELECT * FROM bridge_deposits WHERE deposit_id=$1",
                &[&id.as_slice()],
            )
            .await?
            .map(deposit)
            .transpose()
    }

    /// Claim is a durable source/accounting barrier, before encryption or signing.
    pub async fn claim_bridge_mint(
        &mut self,
        id: B256,
        version: u64,
        accounting_id: i64,
    ) -> Result<BridgeDeposit> {
        let d = self.bridge_deposit(id).await?.context("unknown deposit")?;
        ensure!(!d.claimed, "mint already claimed");
        self.authorize_bridge_mint(id, version, accounting_id).await
    }

    pub async fn authorize_bridge_mint(
        &mut self,
        id: B256,
        version: u64,
        accounting_id: i64,
    ) -> Result<BridgeDeposit> {
        let tx = self.client.transaction().await?;
        lock_bridge(&tx).await?;
        let d = load_deposit(&tx, id).await?;
        ensure!(
            d.version == version
                && ((d.state == DepositState::Eligible && !d.claimed)
                    || (d.state == DepositState::MintPending && d.claimed)),
            "stale or ineligible mint claim"
        );
        let refund: bool = tx
            .query_one(
                "SELECT EXISTS(SELECT 1 FROM bridge_refunds WHERE deposit_id=$1)",
                &[&id.as_slice()],
            )
            .await?
            .get(0);
        ensure!(!refund, "refund authorization excludes minting");
        check_source(&tx, &d).await?;
        let pair = checkpoint_pair(&tx).await?;
        let capacity: bool = tx.query_one("SELECT c.supply + COALESCE((SELECT sum(actual) FROM bridge_redemptions WHERE canonical AND source_number<=$2 AND (fulfilled_number IS NULL OR fulfilled_number>$2) AND (recredited_number IS NULL OR recredited_number>$2)),0) + COALESCE((SELECT sum(amount) FROM bridge_deposits WHERE canonical AND claimed AND state='mint_pending' AND deposit_id<>$3),0) + $4::text::numeric <= 18446744073709551615 FROM bridge_checkpoints c WHERE c.id=$1", &[&accounting_id,&num(pair.checkpoint.number)?,&id.as_slice(),&d.amount.to_string()]).await?.get(0);
        ensure!(capacity, "insufficient committed-supply mint capacity");
        let changed=tx.execute("UPDATE bridge_checkpoints SET acted_on=true WHERE id=$1 AND id=(SELECT max(id) FROM bridge_checkpoints) AND valid AND coverage_complete AND fee_reserve>=CASE WHEN EXISTS(SELECT 1 FROM bridge_origin) THEN 10000 ELSE 0 END AND zcash_height=$2 AND zcash_hash=$3 AND zama_number=$4 AND zama_hash=$5", &[&accounting_id,&i64::from(pair.zcash_height),&&pair.zcash_hash[..],&num(pair.checkpoint.number)?,&pair.checkpoint.hash.as_slice()]).await?;
        ensure!(
            changed == 1,
            "current complete accounting checkpoint required"
        );
        tx.execute("UPDATE bridge_deposits SET state='mint_pending',claimed=true,version=version+1,accounting_id=$2 WHERE deposit_id=$1", &[&id.as_slice(), &accounting_id]).await?;
        let result = load_deposit(&tx, id).await?;
        tx.commit().await?;
        Ok(result)
    }

    pub(crate) async fn reconcile_bridge_deposit(
        &mut self,
        id: B256,
        version: u64,
        evidence: &ConsumptionEvidence,
    ) -> Result<BridgeDeposit> {
        let tx = self.client.transaction().await?;
        lock_bridge(&tx).await?;
        let d = load_deposit(&tx, id).await?;
        ensure!(d.version == version, "stale deposit version");
        check_source(&tx, &d).await?;
        check_evm_checkpoint(&tx, evidence.checkpoint).await?;
        if evidence.status == DepositStatus::None {
            ensure!(
                evidence.event.is_none()
                    && evidence.actual_amount.is_none()
                    && evidence.actual_handle.is_none(),
                "contradictory empty consumption evidence"
            );
            ensure!(
                !matches!(
                    d.state,
                    DepositState::Minted | DepositState::Voided | DepositState::Mismatch
                ),
                "consumption regressed"
            );
            tx.commit().await?;
            return Ok(d); // A signed/claimed operation is never released by None.
        }
        ensure!(
            d.state != DepositState::Voided || evidence.status == DepositStatus::Refunded,
            "voided deposit cannot become minted without canonical rewind"
        );
        let event = evidence
            .event
            .as_ref()
            .context("lifecycle event required")?;
        check_event(&tx, event, evidence.checkpoint).await?;
        let (state, settled, mismatch) = match (&evidence.status, &event.event) {
            (
                DepositStatus::Minted,
                Event::DepositMinted {
                    deposit_id,
                    to,
                    amount,
                },
            ) => {
                ensure!(*deposit_id == id, "wrong deposit lifecycle event");
                let actual = evidence
                    .actual_amount
                    .context("decrypted actual minted amount required")?;
                let handle = evidence
                    .actual_handle
                    .context("actual minted handle required")?;
                let refund: bool = tx
                    .query_one(
                        "SELECT EXISTS(SELECT 1 FROM bridge_refunds WHERE deposit_id=$1)",
                        &[&id.as_slice()],
                    )
                    .await?
                    .get(0);
                let exact = !refund
                    && d.recipient == Some(*to)
                    && *amount == handle
                    && !handle.is_zero()
                    && actual == d.amount
                    && actual > 0;
                (if exact { "minted" } else { "mismatch" }, exact, !exact)
            }
            (DepositStatus::Refunded, Event::DepositVoided { deposit_id }) => {
                ensure!(
                    *deposit_id == id
                        && evidence.actual_amount.is_none()
                        && evidence.actual_handle.is_none(),
                    "wrong void evidence"
                );
                ("voided", false, false)
            }
            _ => anyhow::bail!("consumption/lifecycle disagreement"),
        };
        let old=tx.query_one("SELECT settled_number,settled_hash,actual_amount::text,actual_handle,observed_number,observed_hash FROM bridge_deposits WHERE deposit_id=$1", &[&id.as_slice()]).await?;
        if d.acted_on || (d.state == DepositState::Voided && state == "voided") {
            let same_state = matches!(
                (d.state, state),
                (DepositState::Minted, "minted")
                    | (DepositState::Voided, "voided")
                    | (DepositState::Mismatch, "mismatch")
            );
            ensure!(
                same_state
                    && old.get::<_, Option<String>>(2)
                        == evidence.actual_amount.map(|v| v.to_string())
                    && old.get::<_, Option<Vec<u8>>>(3)
                        == evidence.actual_handle.map(|h| h.to_vec()),
                "conflicting settled evidence"
            );
            tx.commit().await?;
            return Ok(d);
        }
        let actual = evidence.actual_amount.map(|v| v.to_string());
        let handle = evidence.actual_handle.map(|v| v.to_vec());
        let settlement = settled.then(|| num(event.block.number)).transpose()?;
        let settlement_hash = settled.then(|| event.block.hash.to_vec());
        tx.execute("UPDATE bridge_deposits SET state=$2,version=version+1,acted_on=$9,observed_number=$3,observed_hash=$4,settled_number=$5,settled_hash=$6,actual_amount=$7::text::numeric,actual_handle=$8 WHERE deposit_id=$1", &[&id.as_slice(),&state,&num(event.block.number)?,&event.block.hash.as_slice(),&settlement,&settlement_hash,&actual,&handle,&(settled || mismatch)]).await?;
        if settled || mismatch {
            tx.execute("UPDATE zama_receipts SET acted_on=true WHERE tx_hash=$1 AND block_hash=$2 AND canonical", &[&event.transaction_hash.as_slice(),&event.block.hash.as_slice()]).await?;
        }
        if mismatch {
            halt(&tx, "mint_amount_mismatch").await?;
        }
        let result = load_deposit(&tx, id).await?;
        tx.commit().await?;
        Ok(result)
    }

    /// Both chains are evaluated at explicit checkpoints; pending burns remain liabilities.
    pub(crate) async fn bridge_accounting(
        &mut self,
        evidence: &AccountingEvidence,
    ) -> Result<AccountingReport> {
        let tx = self.client.transaction().await?;
        lock_bridge(&tx).await?;
        check_evm_checkpoint(&tx, evidence.checkpoint).await?;
        let valid:bool=tx.query_one("SELECT EXISTS(SELECT 1 FROM checkpoints c JOIN account a ON true WHERE c.height=$1 AND c.hash=$2 AND c.height<=a.current_height-2)", &[&i64::from(evidence.zcash_height),&&evidence.zcash_hash[..]]).await?.get(0);
        ensure!(
            valid,
            "canonical three-confirmation Zcash checkpoint required"
        );
        let pair = BridgeCheckpointPair {
            zcash_height: evidence.zcash_height,
            zcash_hash: evidence.zcash_hash,
            checkpoint: evidence.checkpoint,
        };
        if evidence.supply_handle.is_zero() {
            let pristine: bool = tx.query_one("SELECT EXISTS(SELECT 1 FROM bridge_origin o WHERE o.deployment_number<=$1) AND NOT EXISTS(SELECT 1 FROM zama_events WHERE canonical AND block_number<=$1)", &[&num(pair.checkpoint.number)?]).await?.get(0);
            ensure!(
                pristine && evidence.supply == 0,
                "verified pristine deployment required for uninitialized supply"
            );
        }
        for row in tx
            .query(
                "SELECT redeem_id FROM bridge_payout_death WHERE acted_on",
                &[],
            )
            .await?
        {
            let r = super::redeem::load(&tx, row.get::<_, String>(0).parse()?).await?;
            super::payout_death::check_recovery(&tx, &r).await?;
        }
        let h = i64::from(pair.zcash_height);
        let missing_source: bool = tx.query_one(
            "SELECT EXISTS(SELECT 1 FROM notes n WHERE n.canonical AND n.mined_height<=$1 AND (NOT EXISTS(SELECT 1 FROM checkpoints c WHERE c.height=n.mined_height) OR (n.spent_height<=$1 AND NOT EXISTS(SELECT 1 FROM checkpoints c WHERE c.height=n.spent_height))))",
            &[&h],
        ).await?.get(0);
        ensure!(
            !missing_source,
            "wallet accounting source checkpoint missing"
        );
        let r=tx.query_one("SELECT COALESCE(sum(value),0)::text FROM notes WHERE canonical AND mined_height<=$1 AND (spent_height IS NULL OR spent_height>$1)", &[&h]).await?;
        let assets = r.get::<_, String>(0).parse::<u128>()?;
        let r=tx.query_one("SELECT COALESCE(sum(n.value),0)::text FROM notes n LEFT JOIN bridge_deposits d ON d.txid=n.txid AND d.action_index=n.action_index AND d.canonical WHERE n.canonical AND NOT n.internal AND n.mined_height<=$1 AND (d.settled_number IS NULL OR d.settled_number>$2) AND NOT EXISTS(SELECT 1 FROM bridge_refunds f WHERE f.deposit_id=d.deposit_id AND f.state='paid' AND f.paid_height<=$1)", &[&h,&num(pair.checkpoint.number)?]).await?;
        let liability = r.get::<_, String>(0).parse::<u128>()?;
        let mut unsupported = Vec::new();
        let outgoing: bool = tx
            .query_one("SELECT EXISTS(SELECT 1 FROM operations o WHERE NOT EXISTS(SELECT 1 FROM bridge_redemptions r WHERE r.operation_id=o.id AND r.canonical AND (r.state IN ('reserved','paid','fulfilled') OR (r.state IN ('recredit_pending','recredited') AND EXISTS(SELECT 1 FROM bridge_payout_death d WHERE d.redeem_id=r.redeem_id AND d.acted_on AND d.status='complete')))) AND NOT EXISTS(SELECT 1 FROM bridge_refunds f JOIN bridge_deposits d USING(deposit_id) WHERE f.operation_id=o.id AND d.canonical AND f.state IN ('reserved','paid'))) ", &[])
            .await?
            .get(0);
        if outgoing {
            unsupported.push("outgoing payment lacks a supported canonical liability".into());
        }
        let mut unknown_mint = false;
        for r in tx
            .query(
                "SELECT event FROM zama_events WHERE canonical AND block_number<=$1",
                &[&num(pair.checkpoint.number)?],
            )
            .await?
        {
            match serde_json::from_str::<Event>(r.get(0))? {
                Event::DepositMinted {
                    deposit_id,
                    to,
                    amount,
                } => {
                    let exact:bool=tx.query_one("SELECT EXISTS(SELECT 1 FROM bridge_deposits WHERE deposit_id=$1 AND canonical AND state='minted' AND recipient=$2 AND actual_handle=$3 AND settled_number<=$4 AND source_height<=$5)", &[&deposit_id.as_slice(),&to.as_slice(),&amount.as_slice(),&num(pair.checkpoint.number)?,&h]).await?.get(0);
                    if !exact {
                        unknown_mint = true;
                    }
                }
                Event::DepositVoided { deposit_id } => {
                    let known:bool=tx.query_one("SELECT EXISTS(SELECT 1 FROM bridge_deposits WHERE deposit_id=$1 AND canonical)", &[&deposit_id.as_slice()]).await?.get(0);
                    if !known {
                        unsupported.push("historical void has no canonical deposit".into());
                    }
                }
                Event::RedeemRequested {
                    redeem_id,
                    user,
                    amount,
                    dest_ct,
                } => {
                    let known: bool = tx.query_one("SELECT EXISTS(SELECT 1 FROM bridge_redemptions WHERE redeem_id=$1 AND canonical AND burner=$2 AND handle=$3 AND dest_ct=$4 AND source_number<=$5)", &[&redeem_id.to_string(), &user.as_slice(), &amount.as_slice(), &dest_ct.as_ref(), &num(pair.checkpoint.number)?]).await?.get(0);
                    if !known {
                        unsupported.push("burn lacks actual-amount reconciliation".into());
                    }
                }
                Event::RedeemFulfilled {
                    redeem_id,
                    zcash_txid,
                } => {
                    let known: bool = tx.query_one("SELECT EXISTS(SELECT 1 FROM bridge_redemptions r JOIN operations o ON o.id=r.operation_id WHERE r.redeem_id=$1 AND r.canonical AND r.state='fulfilled' AND r.fulfilled_number<=$2 AND r.paid_height<=$3 AND o.txid=$4)", &[&redeem_id.to_string(), &num(pair.checkpoint.number)?, &h, &zcash_txid.as_slice()]).await?.get(0);
                    if !known {
                        unsupported.push("fulfillment lacks confirmed payment evidence".into());
                    }
                }
                Event::RedeemRecredited {
                    redeem_id,
                    user,
                    amount,
                } => {
                    let known: bool = tx.query_one("SELECT EXISTS(SELECT 1 FROM bridge_redemptions WHERE redeem_id=$1 AND canonical AND state='recredited' AND burner=$2 AND restored_handle=$3 AND recredited_number<=$4)", &[&redeem_id.to_string(), &user.as_slice(), &amount.as_slice(), &num(pair.checkpoint.number)?]).await?.get(0);
                    if !known {
                        unsupported.push("re-credit restoration is not reconciled".into());
                    }
                }
            }
        }
        if unknown_mint {
            unsupported
                .push("historical or unreconciled mint lacks exact canonical backing".into());
        }
        unsupported.sort();
        unsupported.dedup();
        let unpaid: u128 = tx.query_one("SELECT COALESCE(sum(actual),0)::text FROM bridge_redemptions WHERE canonical AND source_number<=$1 AND (paid_height IS NULL OR paid_height>$2) AND (recredited_number IS NULL OR recredited_number>$1)", &[&num(pair.checkpoint.number)?, &h]).await?.get::<_,String>(0).parse()?;
        let premature: bool = tx.query_one("SELECT EXISTS(SELECT 1 FROM bridge_redemptions r JOIN operations o ON o.id=r.operation_id JOIN observed_transactions t ON t.txid=o.txid WHERE r.canonical AND t.height<=$1 AND (r.paid_height IS NULL OR r.source_number>$2))", &[&h,&num(pair.checkpoint.number)?]).await?.get(0);
        let premature_refund: bool = tx.query_one("SELECT EXISTS(SELECT 1 FROM bridge_refunds f JOIN operations o ON o.id=f.operation_id JOIN observed_transactions t ON t.txid=o.txid JOIN bridge_deposits d USING(deposit_id) WHERE t.height<=$1 AND (f.paid_height IS NULL OR d.observed_number>$2 OR NOT d.canonical))", &[&h,&num(pair.checkpoint.number)?]).await?.get(0);
        if premature || premature_refund {
            unsupported.push("payment effects not reconciled at checkpoint pair".into());
        }
        let required = liability
            .checked_add(u128::from(evidence.supply))
            .and_then(|v| v.checked_add(unpaid))
            .context("accounting sum overflow")?;
        let reserve = assets.checked_sub(required);
        let complete = unsupported.is_empty();
        let reserve_signed = reserve
            .map(|v| v.to_string())
            .unwrap_or_else(|| format!("-{}", required - assets));
        let row=tx.query_one("INSERT INTO bridge_checkpoints(zcash_height,zcash_hash,zama_number,zama_hash,supply_handle,supply,assets,deposit_liability,fee_reserve,coverage_complete) VALUES($1,$2,$3,$4,$5,$6::text::numeric,$7::text::numeric,$8::text::numeric,$9::text::numeric,$10) RETURNING id", &[&h,&&pair.zcash_hash[..],&num(pair.checkpoint.number)?,&pair.checkpoint.hash.as_slice(),&evidence.supply_handle.as_slice(),&evidence.supply.to_string(),&assets.to_string(),&liability.to_string(),&reserve_signed,&complete]).await?;
        tx.execute(
            "UPDATE bridge_checkpoints SET unpaid_redemptions=$2::text::numeric WHERE id=$1",
            &[&row.get::<_, i64>(0), &unpaid.to_string()],
        )
        .await?;
        if reserve.is_none() {
            halt(&tx, "accounting_deficit").await?;
        }
        let report = AccountingReport {
            id: row.get(0),
            assets,
            supply: evidence.supply,
            deposit_liability: liability,
            unpaid_redemptions: unpaid,
            fee_reserve: reserve,
            coverage_complete: complete,
            unsupported,
        };
        tx.commit().await?;
        Ok(report)
    }
}

fn num(n: u64) -> Result<i64> {
    Ok(i64::try_from(n)?)
}
fn deposit(r: Row) -> Result<BridgeDeposit> {
    Ok(BridgeDeposit {
        deposit_id: B256::from(fixed::<32>(r.get("deposit_id"))?),
        note_id: zcash::core::NoteId {
            txid: fixed(r.get("txid"))?,
            action_index: u32::try_from(r.get::<_, i64>("action_index"))?,
        },
        amount: u64::try_from(r.get::<_, i64>("amount"))?,
        recipient: r
            .get::<_, Option<Vec<u8>>>("recipient")
            .map(fixed::<20>)
            .transpose()?
            .map(Address::from),
        source_height: u32::try_from(r.get::<_, i64>("source_height"))?,
        source_hash: fixed(r.get("source_hash"))?,
        canonical: r.get("canonical"),
        state: match r.get::<_, &str>("state") {
            "eligible" => DepositState::Eligible,
            "refund_pending" => DepositState::RefundPending,
            "mint_pending" => DepositState::MintPending,
            "minted" => DepositState::Minted,
            "voided" => DepositState::Voided,
            "mismatch" => DepositState::Mismatch,
            _ => anyhow::bail!("invalid deposit state"),
        },
        version: u64::try_from(r.get::<_, i64>("version"))?,
        operation_id: r.get("operation_id"),
        claimed: r.get("claimed"),
        acted_on: r.get("acted_on"),
    })
}
pub(super) async fn load_deposit(tx: &Transaction<'_>, id: B256) -> Result<BridgeDeposit> {
    deposit(
        tx.query_one(
            "SELECT * FROM bridge_deposits WHERE deposit_id=$1 FOR UPDATE",
            &[&id.as_slice()],
        )
        .await?,
    )
}
pub(super) async fn lock_bridge(tx: &Transaction<'_>) -> Result<()> {
    lock_account(tx).await?;
    let c = tx
        .query_one("SELECT * FROM zama_chain FOR UPDATE", &[])
        .await?;
    let b = tx
        .query_one("SELECT * FROM bridge_domain FOR UPDATE", &[])
        .await?;
    ensure!(
        !c.get::<_, bool>("halted") && !b.get::<_, bool>("halted"),
        "bridge or Zama halted"
    );
    ensure!(
        c.get::<_, i64>("chain_id") == b.get::<_, i64>("chain_id")
            && c.get::<_, Vec<u8>>("contract") == b.get::<_, Vec<u8>>("contract"),
        "bridge domain changed"
    );
    Ok(())
}
pub(super) async fn checkpoint_pair(tx: &Transaction<'_>) -> Result<BridgeCheckpointPair> {
    let r=tx.query_one("SELECT c.height,c.hash FROM checkpoints c JOIN account a ON c.height=a.current_height-2", &[]).await.context("confirmed Zcash checkpoint unavailable")?;
    let z=tx.query_one("SELECT b.number,b.hash FROM zama_blocks b JOIN zama_chain c ON b.number=c.tip_number-1 WHERE b.number>=c.deployment_block", &[]).await.context("confirmed EVM checkpoint unavailable")?;
    Ok(BridgeCheckpointPair {
        zcash_height: u32::try_from(r.get::<_, i64>(0))?,
        zcash_hash: fixed(r.get(1))?,
        checkpoint: BlockRef {
            number: u64::try_from(z.get::<_, i64>(0))?,
            hash: B256::from(fixed::<32>(z.get(1))?),
        },
    })
}
// Pending payouts have not reduced the confirmed accounting snapshot yet. Reserve
// their extra network fees too, including unsigned reservations after restart.
pub(super) async fn pending_payout_fee_extra(tx: &Transaction<'_>) -> Result<u64> {
    let extra: String = tx.query_one("SELECT COALESCE(sum(GREATEST((SELECT count(*) FROM reservations x WHERE x.operation_id=r.operation_id),2)*5000-10000),0)::text FROM bridge_redemptions r WHERE r.canonical AND r.state='reserved'", &[]).await?.get(0);
    Ok(extra.parse()?)
}

pub(super) async fn check_payment_accounting(tx: &Transaction<'_>) -> Result<()> {
    let pair = checkpoint_pair(tx).await?;
    let budget = 10_000u64
        .checked_add(pending_payout_fee_extra(tx).await?)
        .context("payout fee reserve overflow")?;
    let valid: bool = tx.query_one("SELECT EXISTS(SELECT 1 FROM bridge_checkpoints WHERE id=(SELECT max(id) FROM bridge_checkpoints) AND valid AND coverage_complete AND fee_reserve>=$5::text::numeric AND zcash_height=$1 AND zcash_hash=$2 AND zama_number=$3 AND zama_hash=$4)", &[&i64::from(pair.zcash_height), &&pair.zcash_hash[..], &num(pair.checkpoint.number)?, &pair.checkpoint.hash.as_slice(), &budget.to_string()]).await?.get(0);
    ensure!(valid, "new payment needs latest complete accounting");
    Ok(())
}
pub(super) async fn check_source(tx: &Transaction<'_>, d: &BridgeDeposit) -> Result<()> {
    let valid:bool=tx.query_one("SELECT EXISTS(SELECT 1 FROM notes n JOIN checkpoints c ON c.height=n.mined_height JOIN account a ON true WHERE n.txid=$1 AND n.action_index=$2 AND n.canonical AND NOT n.internal AND n.value=$3 AND n.mined_height=$4 AND c.hash=$5 AND n.mined_height<=a.current_height-2)", &[&&d.note_id.txid[..],&i64::from(d.note_id.action_index),&num(d.amount)?,&i64::from(d.source_height),&&d.source_hash[..]]).await?.get(0);
    ensure!(
        d.canonical && valid,
        "deposit source no longer canonical/confirmed"
    );
    Ok(())
}
async fn check_evm_checkpoint(tx: &Transaction<'_>, checkpoint: BlockRef) -> Result<()> {
    let valid:bool=tx.query_one("SELECT EXISTS(SELECT 1 FROM zama_blocks b JOIN zama_chain c ON true WHERE b.number=$1 AND b.hash=$2 AND b.number<c.tip_number AND b.number>=c.deployment_block)", &[&num(checkpoint.number)?,&checkpoint.hash.as_slice()]).await?.get(0);
    ensure!(valid, "canonical two-confirmation checkpoint required");
    Ok(())
}
pub(super) async fn check_event(
    tx: &Transaction<'_>,
    e: &EventObservation,
    checkpoint: BlockRef,
) -> Result<()> {
    ensure!(
        e.block.number <= checkpoint.number,
        "lifecycle event after checkpoint"
    );
    let valid:bool=tx.query_one("SELECT EXISTS(SELECT 1 FROM zama_events e JOIN zama_receipts r ON r.tx_hash=e.tx_hash AND r.block_hash=e.block_hash WHERE e.block_number=$1 AND e.block_hash=$2 AND e.tx_hash=$3 AND e.log_index=$4 AND e.event=$5 AND e.canonical AND e.confirmed AND r.canonical AND r.confirmed AND r.success AND r.block_number=e.block_number)", &[&num(e.block.number)?,&e.block.hash.as_slice(),&e.transaction_hash.as_slice(),&num(e.log_index)?,&serde_json::to_string(&e.event)?]).await?.get(0);
    ensure!(
        valid,
        "stored matching lifecycle event and successful confirmed receipt required"
    );
    Ok(())
}
pub(super) async fn halt(tx: &Transaction<'_>, reason: &str) -> Result<()> {
    tx.execute(
        "UPDATE bridge_domain SET halted=true,halt_reason=COALESCE(halt_reason,$1)",
        &[&reason],
    )
    .await?;
    Ok(())
}

/// Called while the caller owns the Zama chain lock; never locks account after chain.
pub(super) async fn validate_bridge_signed(
    tx: &Transaction<'_>,
    operation_id: &str,
    signed: &SignedTransaction,
) -> Result<()> {
    let Some(domain) = tx
        .query_opt("SELECT halted FROM bridge_domain FOR UPDATE", &[])
        .await?
    else {
        return Ok(());
    };
    ensure!(!domain.get::<_, bool>(0), "bridge halted");
    if let Some(id) = signed.void_identity() {
        let valid: bool = tx.query_one("SELECT EXISTS(SELECT 1 FROM bridge_refunds f JOIN bridge_deposits d USING(deposit_id) WHERE f.deposit_id=$1 AND f.void_operation_id=$2 AND d.canonical AND NOT d.claimed AND d.state IN ('eligible','refund_pending','voided') AND NOT EXISTS(SELECT 1 FROM zama_transactions t WHERE t.operation_id=d.operation_id))", &[&id.as_slice(), &operation_id]).await?.get(0);
        ensure!(
            valid,
            "void requires authenticated durable refund authorization"
        );
        return Ok(());
    }
    ensure!(
        !operation_id.starts_with("cwzec/void/"),
        "void operation cannot bind another intent"
    );
    if let Some(id) = signed.recredit_identity() {
        let valid: bool = tx.query_one("SELECT EXISTS(SELECT 1 FROM bridge_redemptions r JOIN zama_receipts e ON e.tx_hash=r.source_tx WHERE r.redeem_id=$1 AND r.canonical AND r.state='recredit_pending' AND r.actual>0 AND e.canonical AND e.confirmed AND e.success AND e.acted_on)", &[&id.to_string()]).await?.get(0);
        ensure!(
            valid && operation_id == format!("cwzec/recredit/{id}"),
            "re-credit requires durable exclusive authorization"
        );
        let r = super::redeem::load(tx, id).await?;
        let issued: bool = tx
            .query_one(
                "SELECT EXISTS(SELECT 1 FROM operations WHERE id=$1)",
                &[&r.operation_id],
            )
            .await?
            .get(0);
        if issued {
            super::payout_death::check_recovery(tx, &r).await?;
        }
        return Ok(());
    }
    ensure!(
        !operation_id.starts_with("cwzec/recredit/"),
        "re-credit operation cannot bind another intent"
    );
    if let Some((id, txid)) = signed.fulfillment_identity() {
        let valid: bool = tx.query_one("SELECT EXISTS(SELECT 1 FROM bridge_redemptions r JOIN operations o ON o.id=r.operation_id JOIN checkpoints c ON c.height=r.paid_height AND c.hash=r.paid_hash JOIN account a ON true WHERE r.redeem_id=$1 AND r.canonical AND r.state='paid' AND o.txid=$2 AND r.paid_height<=a.current_height-2)", &[&id.to_string(), &txid.as_slice()]).await?.get(0);
        ensure!(
            valid && operation_id == format!("cwzec/fulfill/{id}"),
            "fulfillment requires confirmed journal payment"
        );
        return Ok(());
    }
    ensure!(
        !operation_id.starts_with("cwzec/fulfill/"),
        "fulfillment operation cannot bind other intent"
    );
    let Some((recipient, id)) = signed.mint_identity() else {
        let reserved: bool = tx
            .query_one(
                "SELECT EXISTS(SELECT 1 FROM bridge_deposits WHERE operation_id=$1)",
                &[&operation_id],
            )
            .await?
            .get(0);
        ensure!(
            !reserved && !operation_id.starts_with("cwzec/mint/"),
            "mint operation cannot bind a nonmint transaction"
        );
        return Ok(());
    };
    let d = load_deposit(tx, id).await?;
    ensure!(
        d.operation_id == operation_id
            && d.state == DepositState::MintPending
            && d.claimed
            && d.recipient == Some(recipient),
        "signed mint lacks matching durable claim"
    );
    let refund: bool = tx
        .query_one(
            "SELECT EXISTS(SELECT 1 FROM bridge_refunds WHERE deposit_id=$1)",
            &[&id.as_slice()],
        )
        .await?
        .get(0);
    ensure!(!refund, "refund authorization excludes signed mint");
    check_source(tx, &d).await?;
    let pair = checkpoint_pair(tx).await?;
    let valid: bool = tx.query_one("SELECT EXISTS(SELECT 1 FROM bridge_deposits d JOIN bridge_checkpoints c ON c.id=d.accounting_id WHERE d.deposit_id=$1 AND c.id=(SELECT max(id) FROM bridge_checkpoints) AND c.valid AND c.coverage_complete AND c.fee_reserve>=0 AND c.zcash_height=$2 AND c.zcash_hash=$3 AND c.zama_number=$4 AND c.zama_hash=$5)", &[&id.as_slice(), &i64::from(pair.zcash_height), &&pair.zcash_hash[..], &num(pair.checkpoint.number)?, &pair.checkpoint.hash.as_slice()]).await?.get(0);
    ensure!(valid, "signed mint needs current complete accounting");
    Ok(())
}

/// Caller owns account; acquire chain before bridge. true means commit halt before returning error.
pub(super) async fn bridge_zcash_rewind(tx: &Transaction<'_>, height: u32) -> Result<bool> {
    tx.query_opt("SELECT singleton FROM zama_chain FOR UPDATE", &[])
        .await?;
    if tx
        .query_opt("SELECT singleton FROM bridge_domain FOR UPDATE", &[])
        .await?
        .is_none()
    {
        return Ok(false);
    }
    let affected:bool=tx.query_one("SELECT EXISTS(SELECT 1 FROM bridge_deposits WHERE canonical AND source_height>$1 AND (claimed OR acted_on)) OR EXISTS(SELECT 1 FROM bridge_checkpoints WHERE valid AND acted_on AND zcash_height>$1)", &[&i64::from(height)]).await?.get(0);
    let paid: bool = tx.query_one("SELECT EXISTS(SELECT 1 FROM bridge_redemptions WHERE paid_height>$1) OR EXISTS(SELECT 1 FROM bridge_redemptions r JOIN reservations v ON v.operation_id=r.operation_id JOIN notes n USING(txid,action_index) WHERE n.mined_height>$1)", &[&i64::from(height)]).await?.get(0);
    let refund: bool = tx.query_one("SELECT EXISTS(SELECT 1 FROM bridge_refunds f JOIN bridge_deposits d USING(deposit_id) WHERE d.source_height>$1 OR f.paid_height>$1) OR EXISTS(SELECT 1 FROM bridge_refunds f JOIN reservations v ON v.operation_id=f.operation_id JOIN notes n USING(txid,action_index) WHERE n.mined_height>$1)", &[&i64::from(height)]).await?.get(0);
    let death: bool = tx.query_one("SELECT EXISTS(SELECT 1 FROM bridge_payout_death WHERE acted_on AND checkpoint_height>$1)", &[&i64::from(height)]).await?.get(0);
    if affected || paid || refund || death {
        halt(tx, "acted_source_reorg").await?;
        return Ok(true);
    }
    tx.execute("UPDATE bridge_payout_death SET status='invalid',revision=revision+1 WHERE NOT acted_on AND checkpoint_height>$1", &[&i64::from(height)]).await?;
    tx.execute("UPDATE bridge_deposits SET canonical=false,version=version+1 WHERE canonical AND source_height>$1", &[&i64::from(height)]).await?;
    tx.execute(
        "UPDATE bridge_checkpoints SET valid=false WHERE zcash_height>$1",
        &[&i64::from(height)],
    )
    .await?;
    Ok(false)
}
/// Caller owns chain; no account locks are taken in this hook.
pub(super) async fn bridge_zama_rewind(tx: &Transaction<'_>, ancestor: BlockRef) -> Result<bool> {
    if tx
        .query_opt("SELECT singleton FROM bridge_domain FOR UPDATE", &[])
        .await?
        .is_none()
    {
        return Ok(false);
    }
    let affected:bool=tx.query_one("SELECT EXISTS(SELECT 1 FROM bridge_deposits WHERE acted_on AND observed_number>$1) OR EXISTS(SELECT 1 FROM bridge_checkpoints WHERE valid AND acted_on AND zama_number>$1)", &[&num(ancestor.number)?]).await?.get(0);
    if affected {
        halt(tx, "acted_checkpoint_reorg").await?;
        return Ok(true);
    }
    tx.execute("UPDATE bridge_deposits SET state=CASE WHEN claimed THEN 'mint_pending' WHEN recipient IS NULL THEN 'refund_pending' ELSE 'eligible' END,observed_number=NULL,observed_hash=NULL,version=version+1 WHERE NOT acted_on AND observed_number>$1", &[&num(ancestor.number)?]).await?;
    tx.execute("UPDATE bridge_redemptions SET canonical=false,version=version+1 WHERE source_number>$1 AND state IN ('ready','zero','rejected')", &[&num(ancestor.number)?]).await?;
    tx.execute(
        "UPDATE bridge_checkpoints SET valid=false WHERE zama_number>$1",
        &[&num(ancestor.number)?],
    )
    .await?;
    Ok(false)
}
