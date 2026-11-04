//! Redemption intent, burn barrier and wallet reservation share one SQL transaction.
use super::{
    Store,
    bridge::{check_event, checkpoint_pair, lock_bridge},
    fixed, read_attempt,
};
use crate::core::{bridge_types::BridgeCheckpointPair, redeem_types::*};
use alloy::primitives::{Address, B256, U256};
use anyhow::{Context, Result, ensure};
use tokio_postgres::{Row, Transaction};
use zama::watcher::{Event, EventObservation};
use zcash::core::{NoteId, decode_address};

impl Store {
    pub async fn bridge_redeems(&self) -> Result<Vec<BridgeRedeem>> {
        self.client.query("SELECT *,actual::text AS actual_text FROM bridge_redemptions ORDER BY source_number,source_log", &[]).await?.into_iter().map(record).collect()
    }

    pub async fn record_bridge_redeem(
        &mut self,
        event: &EventObservation,
        pair: BridgeCheckpointPair,
        actual: u64,
        destination: Option<&str>,
    ) -> Result<BridgeRedeem> {
        let Event::RedeemRequested {
            redeem_id,
            user,
            amount,
            dest_ct,
        } = &event.event
        else {
            anyhow::bail!("burn event required")
        };
        ensure!(!amount.is_zero() && !user.is_zero(), "missing burn result");
        let valid_destination = destination.filter(|d| decode_address(d).is_ok());
        let (state, reason) = if actual == 0 {
            ("zero", None)
        } else if valid_destination.is_none() {
            ("rejected", Some("invalid_destination"))
        } else if actual
            .checked_sub(REDEEM_FEE_ZAT)
            .is_none_or(|v| v < MIN_PAYOUT_ZAT)
        {
            ("rejected", Some("below_minimum"))
        } else {
            ("ready", None)
        };
        let tx = self.client.transaction().await?;
        lock_bridge(&tx).await?;
        check_event(&tx, event, pair.checkpoint).await?;
        let op = format!("cwzec/redeem/{redeem_id}");
        let changed = tx.execute("INSERT INTO bridge_redemptions(redeem_id,burner,handle,dest_ct,source_number,source_hash,source_tx,source_log,actual,destination,state,rejection,operation_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9::text::numeric,$10,$11,$12,$13) ON CONFLICT(redeem_id) DO UPDATE SET burner=EXCLUDED.burner,handle=EXCLUDED.handle,dest_ct=EXCLUDED.dest_ct,actual=EXCLUDED.actual,destination=EXCLUDED.destination,state=CASE WHEN NOT bridge_redemptions.canonical THEN EXCLUDED.state ELSE bridge_redemptions.state END,rejection=EXCLUDED.rejection,canonical=true,source_number=EXCLUDED.source_number,source_hash=EXCLUDED.source_hash,source_tx=EXCLUDED.source_tx,source_log=EXCLUDED.source_log,version=bridge_redemptions.version+CASE WHEN NOT bridge_redemptions.canonical THEN 1 ELSE 0 END WHERE (NOT bridge_redemptions.canonical AND bridge_redemptions.state IN ('ready','zero','rejected')) OR (bridge_redemptions.burner=EXCLUDED.burner AND bridge_redemptions.handle=EXCLUDED.handle AND bridge_redemptions.dest_ct=EXCLUDED.dest_ct AND bridge_redemptions.actual=EXCLUDED.actual AND bridge_redemptions.destination IS NOT DISTINCT FROM EXCLUDED.destination AND (bridge_redemptions.source_hash=EXCLUDED.source_hash OR bridge_redemptions.state IN ('ready','zero','rejected')))", &[&redeem_id.to_string(), &user.as_slice(), &amount.as_slice(), &dest_ct.as_ref(), &i64::try_from(event.block.number)?, &event.block.hash.as_slice(), &event.transaction_hash.as_slice(), &i64::try_from(event.log_index)?, &actual.to_string(), &valid_destination, &state, &reason, &op]).await?;
        ensure!(changed == 1, "immutable redemption evidence conflict");
        let r = load(&tx, *redeem_id).await?;
        tx.commit().await?;
        Ok(r)
    }

    /// Explicit operator admission. Even an unsigned payout reservation blocks
    /// re-credit: a signer may already be preparing bytes outside this transaction.
    pub async fn authorize_bridge_recredit(
        &mut self,
        id: U256,
        version: u64,
    ) -> Result<BridgeRedeem> {
        self.authorize_recredit(id, version, None).await
    }

    pub async fn authorize_bridge_recredit_at(
        &mut self,
        id: U256,
        version: u64,
        expected: BridgeCheckpointPair,
    ) -> Result<BridgeRedeem> {
        self.authorize_recredit(id, version, Some(expected)).await
    }

    async fn authorize_recredit(
        &mut self,
        id: U256,
        version: u64,
        expected: Option<BridgeCheckpointPair>,
    ) -> Result<BridgeRedeem> {
        let tx = self.client.transaction().await?;
        lock_bridge(&tx).await?;
        let r = load(&tx, id).await?;
        ensure!(
            r.canonical && r.actual > 0,
            "canonical nonzero burn required"
        );
        ensure!(
            r.state != RedeemState::Rejected || r.version == version,
            "stale redemption authorization"
        );
        ensure!(
            matches!(
                r.state,
                RedeemState::Rejected | RedeemState::RecreditPending | RedeemState::Recredited
            ),
            "only rejected-before-payout burns may be recredited"
        );
        let pair = checkpoint_pair(&tx).await?;
        ensure!(
            expected.is_none_or(|p| p == pair),
            "authorization checkpoint changed"
        );
        check_event(&tx, &r.event(), pair.checkpoint).await?;
        check_no_payout(&tx, &r).await?;
        if r.state == RedeemState::Rejected {
            let signed: bool = tx
                .query_one(
                    "SELECT EXISTS(SELECT 1 FROM zama_transactions WHERE operation_id=$1)",
                    &[&r.recredit_id()],
                )
                .await?
                .get(0);
            ensure!(!signed, "recredit journal exists without authorization");
            tx.execute("UPDATE bridge_redemptions SET state='recredit_pending',version=version+1 WHERE redeem_id=$1", &[&id.to_string()]).await?;
            // Losing this burn after authorizing restoration requires a halt.
            tx.execute("UPDATE zama_receipts SET acted_on=true WHERE tx_hash=$1 AND canonical AND confirmed AND success", &[&r.source_tx.as_slice()]).await?;
        }
        let result = load(&tx, id).await?;
        tx.commit().await?;
        Ok(result)
    }

    /// The coordinator supplies independently verified supply restoration. Event
    /// handles are burner-only ciphertexts and cannot prove an amount by themselves.
    pub async fn reconcile_bridge_recredit(
        &mut self,
        id: U256,
        event: &EventObservation,
        pair: BridgeCheckpointPair,
        verified_actual: u64,
    ) -> Result<()> {
        let Event::RedeemRecredited {
            redeem_id,
            user,
            amount,
        } = event.event
        else {
            anyhow::bail!("recredit event required")
        };
        let tx = self.client.transaction().await?;
        lock_bridge(&tx).await?;
        ensure!(
            checkpoint_pair(&tx).await? == pair,
            "current checkpoint required"
        );
        let r = load(&tx, id).await?;
        ensure!(
            id == redeem_id && user == r.burner && !amount.is_zero(),
            "recredit recipient or identity mismatch"
        );
        ensure!(
            r.canonical && r.actual > 0 && verified_actual == r.actual,
            "verified original burn restoration required"
        );
        ensure!(
            matches!(
                r.state,
                RedeemState::RecreditPending | RedeemState::Recredited
            ),
            "explicit recredit authorization required"
        );
        check_event(&tx, &r.event(), pair.checkpoint).await?;
        check_event(&tx, event, pair.checkpoint).await?;
        check_no_payout(&tx, &r).await?;
        let row = tx.query_one("SELECT t.raw,t.tx_hash,t.sender,r.sender AS receipt_sender,r.recipient FROM zama_transactions t JOIN zama_receipts r ON r.tx_hash=t.tx_hash WHERE t.operation_id=$1 AND t.tx_hash=$2 AND r.block_hash=$3 AND r.canonical AND r.confirmed AND r.success", &[&r.recredit_id(), &event.transaction_hash.as_slice(), &event.block.hash.as_slice()]).await?;
        let domain = tx
            .query_one("SELECT chain_id,contract FROM bridge_domain", &[])
            .await?;
        let contract = Address::from(fixed::<20>(domain.get("contract"))?);
        ensure!(
            contract.as_slice() == row.get::<_, Vec<u8>>("recipient"),
            "recredit receipt contract mismatch"
        );
        let signed = zama::contract::SignedTransaction::restore(
            u64::try_from(domain.get::<_, i64>("chain_id"))?,
            contract,
            Address::from(fixed::<20>(row.get("sender"))?),
            row.get::<_, Vec<u8>>("raw").into(),
        )?;
        ensure!(
            signed.recredit_identity() == Some(id)
                && signed.hash() == event.transaction_hash
                && signed.sender().as_slice() == row.get::<_, Vec<u8>>("receipt_sender")
                && row.get::<_, Vec<u8>>("sender") == row.get::<_, Vec<u8>>("receipt_sender"),
            "own signed recredit transaction required"
        );
        let changed = tx.execute("UPDATE bridge_redemptions SET state='recredited',recredited_number=$2,recredited_hash=$3,restored_handle=$4,version=version+CASE WHEN state='recredited' THEN 0 ELSE 1 END WHERE redeem_id=$1 AND (recredited_number IS NULL OR (recredited_number=$2 AND recredited_hash=$3 AND restored_handle=$4))", &[&id.to_string(), &i64::try_from(event.block.number)?, &event.block.hash.as_slice(), &amount.as_slice()]).await?;
        ensure!(changed == 1, "immutable restoration evidence conflict");
        tx.execute(
            "UPDATE zama_receipts SET acted_on=true WHERE tx_hash=$1 AND block_hash=$2",
            &[
                &event.transaction_hash.as_slice(),
                &event.block.hash.as_slice(),
            ],
        )
        .await?;
        tx.execute("UPDATE reservations SET released=true WHERE operation_id=$1 AND EXISTS(SELECT 1 FROM bridge_payout_death WHERE redeem_id=$2 AND acted_on AND status='complete')", &[&r.operation_id, &id.to_string()]).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn reserve_bridge_redeem(
        &mut self,
        id: U256,
        version: u64,
        accounting_id: i64,
        input: NoteId,
    ) -> Result<BridgeRedeem> {
        self.reserve_bridge_redeem_inputs(id, version, accounting_id, &[input])
            .await
    }

    pub async fn reserve_bridge_redeem_inputs(
        &mut self,
        id: U256,
        version: u64,
        accounting_id: i64,
        inputs: &[NoteId],
    ) -> Result<BridgeRedeem> {
        let network_fee = zcash::vault::fee_for_inputs(inputs.len())?;
        super::unique_inputs(inputs)?;
        let tx = self.client.transaction().await?;
        lock_bridge(&tx).await?;
        let r = load(&tx, id).await?;
        ensure!(
            r.canonical && r.version == version && r.state == RedeemState::Ready,
            "stale or ineligible redemption"
        );
        let pair = checkpoint_pair(&tx).await?;
        check_event(&tx, &r.event(), pair.checkpoint).await?;
        let pending_extra = super::bridge::pending_payout_fee_extra(&tx).await?;
        let fee_budget = MIN_FEE_RESERVE_ZAT
            .checked_add(network_fee - REDEEM_FEE_ZAT)
            .and_then(|v| v.checked_add(pending_extra))
            .context("payout fee reserve overflow")?;
        let changed = tx.execute("UPDATE bridge_checkpoints SET acted_on=true WHERE id=$1 AND id=(SELECT max(id) FROM bridge_checkpoints) AND valid AND coverage_complete AND fee_reserve >= $2::text::numeric AND zcash_height=$3 AND zcash_hash=$4 AND zama_number=$5 AND zama_hash=$6", &[&accounting_id, &fee_budget.to_string(), &i64::from(pair.zcash_height), &&pair.zcash_hash[..], &i64::try_from(pair.checkpoint.number)?, &pair.checkpoint.hash.as_slice()]).await?;
        ensure!(
            changed == 1,
            "current accounting and operator reserve must cover payout network fees"
        );
        let net = r
            .actual
            .checked_sub(REDEEM_FEE_ZAT)
            .context("invalid payout amount")?;
        let mut total = 0u64;
        let mut nullifiers = Vec::with_capacity(inputs.len());
        for input in inputs {
            let n = tx.query_one("SELECT value,canonical,spent_height,mined_height,nullifier FROM notes WHERE txid=$1 AND action_index=$2", &[&&input.txid[..], &i64::from(input.action_index)]).await?;
            ensure!(
                n.get::<_, bool>("canonical")
                    && n.get::<_, Option<i64>>("spent_height").is_none()
                    && n.get::<_, i64>("mined_height") <= i64::from(pair.zcash_height),
                "input unavailable"
            );
            total = total
                .checked_add(u64::try_from(n.get::<_, i64>("value"))?)
                .context("input value overflow")?;
            nullifiers.push(n.get::<_, Vec<u8>>("nullifier"));
        }
        ensure!(
            total > net.checked_add(network_fee).context("amount overflow")?,
            "payout inputs must cover network fee and positive change"
        );
        tx.execute(
            "INSERT INTO operations(id,destination,amount) VALUES($1,$2,$3)",
            &[
                &r.operation_id,
                &r.destination.as_ref().context("missing destination")?,
                &i64::try_from(net)?,
            ],
        )
        .await?;
        for (input, nullifier) in inputs.iter().zip(nullifiers) {
            tx.execute("INSERT INTO reservations(txid,action_index,operation_id,nullifier) VALUES($1,$2,$3,$4)", &[&&input.txid[..], &i64::from(input.action_index), &r.operation_id, &nullifier]).await?;
        }
        tx.execute("UPDATE zama_receipts SET acted_on=true WHERE tx_hash=$1 AND canonical AND confirmed AND success", &[&r.source_tx.as_slice()]).await?;
        tx.execute("UPDATE bridge_redemptions SET state='reserved',version=version+1,accounting_id=$2 WHERE redeem_id=$1", &[&id.to_string(), &accounting_id]).await?;
        let result = load(&tx, id).await?;
        tx.commit().await?;
        Ok(result)
    }

    /// Recognize all payment effects at the same confirmed checkpoint; no inclusion-only settlement.
    pub async fn settle_bridge_payment(
        &mut self,
        id: U256,
        pair: BridgeCheckpointPair,
    ) -> Result<Option<B256>> {
        let tx = self.client.transaction().await?;
        lock_bridge(&tx).await?;
        ensure!(
            checkpoint_pair(&tx).await? == pair,
            "current checkpoint required"
        );
        let r = load(&tx, id).await?;
        if !matches!(
            r.state,
            RedeemState::Reserved | RedeemState::Paid | RedeemState::Fulfilled
        ) {
            return Ok(None);
        }
        check_event(&tx, &r.event(), pair.checkpoint).await?;
        let a = read_attempt(&tx, &r.operation_id)
            .await?
            .context("missing redemption journal")?;
        let Some(p) = a.prepared else { return Ok(None) };
        let Some(height) = a.observed_height.filter(|h| *h <= pair.zcash_height) else {
            return Ok(None);
        };
        ensure!(
            p.expiry_height > 0
                && height <= p.expiry_height
                && p.fee == zcash::vault::fee_for_inputs(a.inputs.len())?,
            "invalid payment evidence"
        );
        ensure!(
            a.amount == r.actual - REDEEM_FEE_ZAT && Some(&a.destination) == r.destination.as_ref(),
            "payment intent mismatch"
        );
        let mut inputs = 0u128;
        for input in &a.inputs {
            let n = tx.query_one("SELECT value,canonical,spent_height FROM notes WHERE txid=$1 AND action_index=$2", &[&&input.txid[..], &i64::from(input.action_index)]).await?;
            ensure!(
                n.get::<_, bool>("canonical")
                    && n.get::<_, Option<i64>>("spent_height") == Some(i64::from(height)),
                "payment input spend not confirmed"
            );
            inputs += u128::try_from(n.get::<_, i64>("value"))?;
        }
        let change: u128 = tx.query_one("SELECT COALESCE(sum(value),0)::text FROM notes WHERE canonical AND internal AND txid=$1 AND mined_height=$2", &[&&p.txid[..], &i64::from(height)]).await?.get::<_,String>(0).parse()?;
        ensure!(
            change > 0 && inputs == change + u128::from(a.amount) + u128::from(p.fee),
            "payment change/fee evidence mismatch"
        );
        let hash: Vec<u8> = tx
            .query_one(
                "SELECT hash FROM checkpoints WHERE height=$1",
                &[&i64::from(height)],
            )
            .await?
            .get(0);
        tx.execute("UPDATE bridge_redemptions SET state=CASE WHEN state='fulfilled' THEN state ELSE 'paid' END,paid_height=$2,paid_hash=$3,version=version+CASE WHEN paid_height IS NULL THEN 1 ELSE 0 END WHERE redeem_id=$1", &[&id.to_string(), &i64::from(height), &hash]).await?;
        tx.commit().await?;
        Ok(Some(B256::from(p.txid)))
    }

    pub async fn reconcile_bridge_fulfillment(
        &mut self,
        id: U256,
        event: &EventObservation,
        pair: BridgeCheckpointPair,
    ) -> Result<()> {
        let Event::RedeemFulfilled {
            redeem_id,
            zcash_txid,
        } = event.event
        else {
            anyhow::bail!("fulfillment required")
        };
        ensure!(
            id == redeem_id && !zcash_txid.is_zero(),
            "invalid fulfillment"
        );
        let paid = self.settle_bridge_payment(id, pair).await?;
        ensure!(
            paid == Some(zcash_txid),
            "fulfillment lacks real confirmed payment"
        );
        let tx = self.client.transaction().await?;
        lock_bridge(&tx).await?;
        check_event(&tx, event, pair.checkpoint).await?;
        tx.execute(
            "UPDATE zama_receipts SET acted_on=true WHERE tx_hash=$1",
            &[&event.transaction_hash.as_slice()],
        )
        .await?;
        tx.execute("UPDATE bridge_redemptions SET state='fulfilled',fulfilled_number=$2,fulfilled_hash=$3,version=version+CASE WHEN state='fulfilled' THEN 0 ELSE 1 END WHERE redeem_id=$1 AND state IN ('paid','fulfilled')", &[&id.to_string(), &i64::try_from(event.block.number)?, &event.block.hash.as_slice()]).await?;
        tx.commit().await?;
        Ok(())
    }
}

pub(super) async fn load(tx: &Transaction<'_>, id: U256) -> Result<BridgeRedeem> {
    record(tx.query_one("SELECT *,actual::text AS actual_text FROM bridge_redemptions WHERE redeem_id=$1 FOR UPDATE", &[&id.to_string()]).await?)
}
fn record(r: Row) -> Result<BridgeRedeem> {
    Ok(BridgeRedeem {
        id: r.get::<_, String>("redeem_id").parse()?,
        burner: Address::from(fixed::<20>(r.get("burner"))?),
        handle: B256::from(fixed::<32>(r.get("handle"))?),
        dest_ct: r.get::<_, Vec<u8>>("dest_ct").into(),
        source: zama::watcher::BlockRef {
            number: u64::try_from(r.get::<_, i64>("source_number"))?,
            hash: B256::from(fixed::<32>(r.get("source_hash"))?),
        },
        source_tx: B256::from(fixed::<32>(r.get("source_tx"))?),
        source_log: u64::try_from(r.get::<_, i64>("source_log"))?,
        canonical: r.get("canonical"),
        actual: r.get::<_, String>("actual_text").parse()?,
        destination: r.get("destination"),
        state: match r.get::<_, &str>("state") {
            "ready" => RedeemState::Ready,
            "zero" => RedeemState::Zero,
            "rejected" => RedeemState::Rejected,
            "recredit_pending" => RedeemState::RecreditPending,
            "recredited" => RedeemState::Recredited,
            "reserved" => RedeemState::Reserved,
            "paid" => RedeemState::Paid,
            "fulfilled" => RedeemState::Fulfilled,
            _ => anyhow::bail!("invalid redemption state"),
        },
        version: u64::try_from(r.get::<_, i64>("version"))?,
        operation_id: r.get("operation_id"),
    })
}

// All payout admission paths hold the same account/chain/domain locks. A journal
// row blocks even before it has signed bytes; never erase one to make this pass.
async fn check_no_payout(tx: &Transaction<'_>, r: &BridgeRedeem) -> Result<()> {
    let issued: bool = tx.query_one("SELECT EXISTS(SELECT 1 FROM operations WHERE id=$1) OR EXISTS(SELECT 1 FROM zama_transactions WHERE operation_id=$2)", &[&r.operation_id, &r.fulfillment_id()]).await?.get(0);
    if issued {
        super::payout_death::check_recovery(tx, r).await?;
    }
    for row in tx
        .query("SELECT event FROM zama_events WHERE canonical", &[])
        .await?
    {
        if let Event::RedeemFulfilled { redeem_id, .. } =
            serde_json::from_str::<Event>(&row.get::<_, String>(0))?
        {
            ensure!(redeem_id != r.id, "fulfillment event blocks recredit");
        }
    }
    Ok(())
}
