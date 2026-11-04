//! Explicit participant authorization and a confirmed void precede a wallet reservation.
use super::{
    Store,
    bridge::{check_event, check_source, checkpoint_pair, load_deposit, lock_bridge},
    fixed, read_attempt,
};
use crate::core::bridge_types::*;
use alloy::primitives::{Address, B256};
use anyhow::{Context, Result, ensure};
use tokio_postgres::{Row, Transaction};
use zama::watcher::{BlockRef, Event, EventObservation};
use zcash::core::{NoteId, decode_address};

const FEE: u64 = zcash::vault::FIXED_FEE_ZAT;
impl Store {
    pub async fn bridge_refunds(&self) -> Result<Vec<BridgeRefund>> {
        self.client
            .query("SELECT * FROM bridge_refunds ORDER BY deposit_id", &[])
            .await?
            .into_iter()
            .map(record)
            .collect()
    }
    pub async fn authorize_bridge_refund(
        &mut self,
        id: B256,
        version: u64,
        participant: &str,
        destination: &str,
    ) -> Result<BridgeRefund> {
        self.authorize_refund(id, version, participant, destination, None)
            .await
    }

    pub async fn authorize_bridge_refund_at(
        &mut self,
        id: B256,
        version: u64,
        participant: &str,
        destination: &str,
        expected: BridgeCheckpointPair,
    ) -> Result<BridgeRefund> {
        self.authorize_refund(id, version, participant, destination, Some(expected))
            .await
    }

    async fn authorize_refund(
        &mut self,
        id: B256,
        version: u64,
        participant: &str,
        destination: &str,
        expected: Option<BridgeCheckpointPair>,
    ) -> Result<BridgeRefund> {
        validate_terms(participant, destination)?;
        let tx = self.client.transaction().await?;
        lock_bridge(&tx).await?;
        if let Some(expected) = expected {
            ensure!(
                checkpoint_pair(&tx).await? == expected,
                "authorization checkpoint changed"
            );
        }
        let d = load_deposit(&tx, id).await?;
        check_source(&tx, &d).await?;
        ensure!(
            !d.claimed
                && matches!(
                    d.state,
                    DepositState::Eligible | DepositState::RefundPending | DepositState::Voided
                ),
            "deposit cannot be refunded"
        );
        ensure!(d.amount > FEE, "refund must exceed fee");
        let mint: bool = tx
            .query_one(
                "SELECT EXISTS(SELECT 1 FROM zama_transactions WHERE operation_id=$1)",
                &[&d.operation_id],
            )
            .await?
            .get(0);
        ensure!(!mint, "mint journal blocks refund");
        if let Some(row) = tx
            .query_opt(
                "SELECT * FROM bridge_refunds WHERE deposit_id=$1 FOR UPDATE",
                &[&id.as_slice()],
            )
            .await?
        {
            let r = record(row)?;
            ensure!(
                r.participant == participant
                    && r.destination == destination
                    && r.gross_amount == d.amount
                    && r.fee == FEE,
                "immutable refund authorization conflict"
            );
            tx.commit().await?;
            return Ok(r);
        }
        ensure!(d.version == version, "stale refund authorization");
        let op = format!("cwzec/refund/{id}");
        let void_op = format!("cwzec/void/{id}");
        tx.execute("INSERT INTO bridge_refunds(deposit_id,participant,destination,gross_amount,fee,state,operation_id,void_operation_id) VALUES($1,$2,$3,$4,$5,'authorized',$6,$7)", &[&id.as_slice(), &participant, &destination, &i64::try_from(d.amount)?, &i64::try_from(FEE)?, &op, &void_op]).await?;
        tx.execute("UPDATE bridge_deposits SET state=CASE WHEN state='voided' THEN state ELSE 'refund_pending' END,version=version+1 WHERE deposit_id=$1", &[&id.as_slice()]).await?;
        let r = load(&tx, id).await?;
        tx.commit().await?;
        Ok(r)
    }
    pub async fn reserve_bridge_refund(
        &mut self,
        id: B256,
        version: u64,
        accounting_id: i64,
        input: NoteId,
    ) -> Result<BridgeRefund> {
        let tx = self.client.transaction().await?;
        lock_bridge(&tx).await?;
        let r = load(&tx, id).await?;
        ensure!(
            r.state == RefundState::Authorized && r.version == version,
            "stale or ineligible refund"
        );
        let pair = checkpoint_pair(&tx).await?;
        let event = check_void(&tx, &r, pair, false).await?;
        let changed = tx.execute("UPDATE bridge_checkpoints SET acted_on=true WHERE id=$1 AND id=(SELECT max(id) FROM bridge_checkpoints) AND valid AND coverage_complete AND fee_reserve >= 10000 AND zcash_height=$2 AND zcash_hash=$3 AND zama_number=$4 AND zama_hash=$5", &[&accounting_id, &i64::from(pair.zcash_height), &&pair.zcash_hash[..], &i64::try_from(pair.checkpoint.number)?, &pair.checkpoint.hash.as_slice()]).await?;
        ensure!(
            changed == 1,
            "current accounting and operator fee reserve required"
        );
        let n = tx
            .query_one(
                "SELECT * FROM notes WHERE txid=$1 AND action_index=$2",
                &[&&input.txid[..], &i64::from(input.action_index)],
            )
            .await?;
        let net = r
            .gross_amount
            .checked_sub(r.fee)
            .context("invalid refund amount")?;
        ensure!(
            n.get::<_, bool>("canonical")
                && n.get::<_, Option<i64>>("spent_height").is_none()
                && n.get::<_, i64>("mined_height") <= i64::from(pair.zcash_height),
            "input unavailable"
        );
        ensure!(
            u64::try_from(n.get::<_, i64>("value"))?
                > net
                    .checked_add(zcash::vault::FIXED_FEE_ZAT)
                    .context("amount overflow")?,
            "single input requires positive change"
        );
        tx.execute(
            "INSERT INTO operations(id,destination,amount) VALUES($1,$2,$3)",
            &[&r.operation_id, &r.destination, &i64::try_from(net)?],
        )
        .await?;
        tx.execute("INSERT INTO reservations(txid,action_index,operation_id,nullifier) VALUES($1,$2,$3,$4)", &[&&input.txid[..], &i64::from(input.action_index), &r.operation_id, &n.get::<_,Vec<u8>>("nullifier")]).await?;
        tx.execute(
            "UPDATE bridge_deposits SET acted_on=true WHERE deposit_id=$1",
            &[&id.as_slice()],
        )
        .await?;
        tx.execute(
            "UPDATE zama_receipts SET acted_on=true WHERE tx_hash=$1 AND block_hash=$2",
            &[
                &event.transaction_hash.as_slice(),
                &event.block.hash.as_slice(),
            ],
        )
        .await?;
        tx.execute("UPDATE bridge_refunds SET state='reserved',version=version+1,accounting_id=$2 WHERE deposit_id=$1", &[&id.as_slice(), &accounting_id]).await?;
        let result = load(&tx, id).await?;
        tx.commit().await?;
        Ok(result)
    }
    pub async fn settle_bridge_refund(
        &mut self,
        id: B256,
        pair: BridgeCheckpointPair,
    ) -> Result<Option<B256>> {
        let tx = self.client.transaction().await?;
        lock_bridge(&tx).await?;
        ensure!(
            checkpoint_pair(&tx).await? == pair,
            "current checkpoint required"
        );
        let r = load(&tx, id).await?;
        if r.state == RefundState::Authorized {
            return Ok(None);
        }
        check_void(&tx, &r, pair, true).await?;
        let a = read_attempt(&tx, &r.operation_id)
            .await?
            .context("refund journal missing")?;
        let Some(p) = a.prepared else { return Ok(None) };
        let Some(height) = a.observed_height.filter(|h| *h <= pair.zcash_height) else {
            return Ok(None);
        };
        ensure!(
            p.expiry_height > 0
                && height <= p.expiry_height
                && p.fee == zcash::vault::FIXED_FEE_ZAT
                && a.inputs.len() == 1
                && a.amount == r.gross_amount - r.fee
                && a.destination == r.destination,
            "refund payment intent mismatch"
        );
        let input = a.inputs[0];
        let n = tx
            .query_one(
                "SELECT value,canonical,spent_height FROM notes WHERE txid=$1 AND action_index=$2",
                &[&&input.txid[..], &i64::from(input.action_index)],
            )
            .await?;
        ensure!(
            n.get::<_, bool>("canonical")
                && n.get::<_, Option<i64>>("spent_height") == Some(i64::from(height)),
            "refund input spend not confirmed"
        );
        let change: u128 = tx.query_one("SELECT COALESCE(sum(value),0)::text FROM notes WHERE canonical AND internal AND txid=$1 AND mined_height=$2", &[&&p.txid[..], &i64::from(height)]).await?.get::<_,String>(0).parse()?;
        ensure!(
            change > 0
                && u128::try_from(n.get::<_, i64>("value"))?
                    == change + u128::from(a.amount) + u128::from(p.fee),
            "refund change/fee evidence mismatch"
        );
        let hash: Vec<u8> = tx
            .query_one(
                "SELECT hash FROM checkpoints WHERE height=$1",
                &[&i64::from(height)],
            )
            .await?
            .get(0);
        tx.execute("UPDATE bridge_refunds SET state='paid',paid_height=$2,paid_hash=$3,version=version+CASE WHEN paid_height IS NULL THEN 1 ELSE 0 END WHERE deposit_id=$1", &[&id.as_slice(), &i64::from(height), &hash]).await?;
        tx.commit().await?;
        Ok(Some(B256::from(p.txid)))
    }
}
fn validate_terms(participant: &str, destination: &str) -> Result<()> {
    ensure!(
        !participant.trim().is_empty()
            && participant == participant.trim()
            && participant.len() <= 128
            && !participant.eq_ignore_ascii_case("unknown"),
        "explicit known participant required"
    );
    decode_address(destination)?;
    Ok(())
}
async fn load(tx: &Transaction<'_>, id: B256) -> Result<BridgeRefund> {
    record(
        tx.query_one(
            "SELECT * FROM bridge_refunds WHERE deposit_id=$1 FOR UPDATE",
            &[&id.as_slice()],
        )
        .await?,
    )
}
fn record(r: Row) -> Result<BridgeRefund> {
    Ok(BridgeRefund {
        deposit_id: B256::from(fixed::<32>(r.get("deposit_id"))?),
        participant: r.get("participant"),
        destination: r.get("destination"),
        gross_amount: u64::try_from(r.get::<_, i64>("gross_amount"))?,
        fee: u64::try_from(r.get::<_, i64>("fee"))?,
        state: match r.get::<_, &str>("state") {
            "authorized" => RefundState::Authorized,
            "reserved" => RefundState::Reserved,
            "paid" => RefundState::Paid,
            _ => anyhow::bail!("invalid refund state"),
        },
        operation_id: r.get("operation_id"),
        void_operation_id: r.get("void_operation_id"),
        version: u64::try_from(r.get::<_, i64>("version"))?,
    })
}
/// Called inside the account lock and the same transaction that saves wallet bytes.
pub(super) async fn validate_prepared(tx: &Transaction<'_>, operation_id: &str) -> Result<()> {
    let Some(row) = tx
        .query_opt(
            "SELECT * FROM bridge_refunds WHERE operation_id=$1 FOR UPDATE",
            &[&operation_id],
        )
        .await?
    else {
        return Ok(());
    };
    lock_bridge(tx).await?;
    let r = record(row)?;
    ensure!(
        r.state == RefundState::Reserved,
        "refund reservation required"
    );
    check_void(tx, &r, checkpoint_pair(tx).await?, true).await?;
    Ok(())
}
async fn check_void(
    tx: &Transaction<'_>,
    r: &BridgeRefund,
    pair: BridgeCheckpointPair,
    acted: bool,
) -> Result<EventObservation> {
    let d = load_deposit(tx, r.deposit_id).await?;
    check_source(tx, &d).await?;
    ensure!(
        !d.claimed
            && d.state == DepositState::Voided
            && d.amount == r.gross_amount
            && (!acted || d.acted_on),
        "canonical unclaimed voided deposit required"
    );
    let row = tx.query_one("SELECT t.raw,t.tx_hash,t.sender,r.sender AS receipt_sender,r.block_number,r.block_hash,r.acted_on FROM zama_transactions t JOIN zama_receipts r ON r.tx_hash=t.tx_hash JOIN bridge_domain d ON r.recipient=d.contract WHERE t.operation_id=$1 AND r.canonical AND r.confirmed AND r.success", &[&r.void_operation_id]).await.context("own confirmed successful void required")?;
    ensure!(
        !acted || row.get::<_, bool>("acted_on"),
        "void receipt must be acted on"
    );
    let domain = tx
        .query_one("SELECT chain_id,contract FROM bridge_domain", &[])
        .await?;
    let signed = zama::contract::SignedTransaction::restore(
        u64::try_from(domain.get::<_, i64>("chain_id"))?,
        Address::from(fixed::<20>(domain.get("contract"))?),
        Address::from(fixed::<20>(row.get("sender"))?),
        row.get::<_, Vec<u8>>("raw").into(),
    )?;
    let hash = B256::from(fixed::<32>(row.get("tx_hash"))?);
    ensure!(
        signed.void_identity() == Some(r.deposit_id)
            && signed.hash() == hash
            && signed.sender().as_slice() == row.get::<_, Vec<u8>>("receipt_sender"),
        "void journal identity mismatch"
    );
    let event_row = tx.query_one("SELECT log_index,event FROM zama_events WHERE tx_hash=$1 AND block_hash=$2 AND canonical AND confirmed AND event=$3", &[&hash.as_slice(), &row.get::<_,Vec<u8>>("block_hash"), &serde_json::to_string(&Event::DepositVoided { deposit_id: r.deposit_id })?]).await?;
    let e = EventObservation {
        block: BlockRef {
            number: u64::try_from(row.get::<_, i64>("block_number"))?,
            hash: B256::from(fixed::<32>(row.get("block_hash"))?),
        },
        transaction_hash: hash,
        log_index: u64::try_from(event_row.get::<_, i64>("log_index"))?,
        event: serde_json::from_str(&event_row.get::<_, String>("event"))?,
    };
    check_event(tx, &e, pair.checkpoint).await?;
    let observed = tx
        .query_one(
            "SELECT observed_number,observed_hash FROM bridge_deposits WHERE deposit_id=$1",
            &[&r.deposit_id.as_slice()],
        )
        .await?;
    ensure!(
        observed.get::<_, Option<i64>>(0) == Some(i64::try_from(e.block.number)?)
            && observed.get::<_, Option<Vec<u8>>>(1) == Some(e.block.hash.to_vec()),
        "void consumption evidence mismatch"
    );
    Ok(e)
}
#[cfg(test)]
#[path = "refund_tests.rs"]
mod tests;
