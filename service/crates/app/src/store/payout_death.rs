//! Durable complete-history progress, bound to the existing immutable payout journal.
use super::{
    Store,
    bridge::{check_event, checkpoint_pair, lock_bridge},
    read_attempt, redeem,
};
use crate::core::{
    bridge_types::BridgeCheckpointPair,
    redeem_types::{BridgeRedeem, RedeemState},
};
use alloy::primitives::U256;
use anyhow::{Context, Result, ensure};
use tokio_postgres::{Row, Transaction};
use zcash::{
    core::PreparedTransaction,
    rpc::history::{History, validate_attempts},
};

pub struct DeathCollection {
    pub redeem_id: U256,
    pub operation_id: String,
    pub attempts: Vec<PreparedTransaction>,
    pub history: Option<History>,
    pub revision: i64,
    pub status: String,
    pub acted_on: bool,
}

impl Store {
    /// Collection is not recovery authorization. Paid and unsigned intents are ineligible.
    pub async fn request_payout_history(
        &mut self,
        id: U256,
        version: u64,
        pair: BridgeCheckpointPair,
    ) -> Result<DeathCollection> {
        let tx = self.client.transaction().await?;
        lock_bridge(&tx).await?;
        ensure!(
            checkpoint_pair(&tx).await? == pair,
            "authorization checkpoint changed"
        );
        let r = redeem::load(&tx, id).await?;
        ensure!(
            r.canonical && r.actual > 0 && r.state == RedeemState::Reserved && r.version == version,
            "issued redemption required"
        );
        check_event(&tx, &r.event(), pair.checkpoint).await?;
        let attempt = unpaid_attempt(&tx, &r).await?;
        validate_attempts(std::slice::from_ref(&attempt))?;
        let tip: i64 = tx
            .query_one("SELECT current_height FROM account", &[])
            .await?
            .get(0);
        ensure!(
            u64::try_from(tip)? >= u64::from(attempt.expiry_height) + 3,
            "issued expiry not buried"
        );
        let domain = tx
            .query_one("SELECT chain_id,contract FROM bridge_domain", &[])
            .await?;
        tx.execute("INSERT INTO bridge_payout_death(redeem_id,operation_id,chain_id,contract,signed_bytes) VALUES($1,$2,$3,$4,$5) ON CONFLICT(redeem_id) DO NOTHING", &[&id.to_string(), &r.operation_id, &domain.get::<_,i64>(0), &domain.get::<_,Vec<u8>>(1), &attempt.bytes]).await?;
        let d = load(&tx, id).await?.context("history request missing")?;
        ensure!(!d.acted_on, "recovery already authorized");
        // A proven reorg may restart unacted collection, never weaken its interval.
        if d.status == "invalid" {
            tx.execute("UPDATE bridge_payout_death SET history=NULL,status='collecting',checkpoint_height=NULL,checkpoint_hash=NULL,revision=revision+1 WHERE redeem_id=$1", &[&id.to_string()]).await?;
        }
        let d = load(&tx, id).await?.unwrap();
        tx.commit().await?;
        Ok(d)
    }

    pub async fn payout_history(&mut self, id: U256) -> Result<Option<DeathCollection>> {
        let tx = self.client.transaction().await?;
        let d = load(&tx, id).await?;
        tx.commit().await?;
        Ok(d)
    }

    pub async fn payout_histories(&mut self) -> Result<Vec<DeathCollection>> {
        let tx = self.client.transaction().await?;
        let rows = tx
            .query(
                "SELECT redeem_id FROM bridge_payout_death ORDER BY redeem_id",
                &[],
            )
            .await?;
        let mut result = Vec::new();
        for row in rows {
            result.push(
                load(&tx, row.get::<_, String>(0).parse()?)
                    .await?
                    .context("history disappeared")?,
            );
        }
        tx.commit().await?;
        Ok(result)
    }

    pub async fn save_payout_history(
        &mut self,
        id: U256,
        revision: i64,
        history: &History,
    ) -> Result<()> {
        let tx = self.client.transaction().await?;
        lock_bridge(&tx).await?;
        let r = redeem::load(&tx, id).await?;
        ensure!(
            r.canonical && r.state == RedeemState::Reserved,
            "redemption no longer collecting"
        );
        unpaid_attempt(&tx, &r).await?;
        let d = load(&tx, id).await?.context("history request missing")?;
        ensure!(
            !d.acted_on && d.revision == revision && d.status != "invalid",
            "stale history progress"
        );
        history.matches_attempts(&d.attempts)?;
        if let Some(old) = d.history {
            ensure!(
                history.cursor_height() >= old.cursor_height()
                    && history.checkpoint_height() == old.checkpoint_height()
                    && history.checkpoint_hash() == old.checkpoint_hash(),
                "history continuity changed"
            );
        }
        let status = if history.complete_absence() {
            "complete"
        } else if !history.inclusions().is_empty() {
            "included"
        } else {
            "collecting"
        };
        tx.execute("UPDATE bridge_payout_death SET history=$2,status=$3,checkpoint_height=$4,checkpoint_hash=$5,revision=revision+1 WHERE redeem_id=$1", &[&id.to_string(), &serde_json::to_string(history)?, &status, &i64::from(history.checkpoint_height()), &&history.checkpoint_hash()[..]]).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Only a detected canonical change calls this, not transient transport failures.
    pub async fn invalidate_payout_history(&mut self, id: U256, revision: i64) -> Result<()> {
        let tx = self.client.transaction().await?;
        // Allow the halt to be persisted even when another reconciliation already halted.
        super::lock_account(&tx).await?;
        tx.query_one("SELECT singleton FROM zama_chain FOR UPDATE", &[])
            .await?;
        tx.query_one("SELECT singleton FROM bridge_domain FOR UPDATE", &[])
            .await?;
        let d = load(&tx, id).await?.context("history missing")?;
        ensure!(d.revision == revision, "stale history invalidation");
        tx.execute("UPDATE bridge_payout_death SET status='invalid',revision=revision+1 WHERE redeem_id=$1", &[&id.to_string()]).await?;
        if d.acted_on {
            super::bridge::halt(&tx, "acted_payout_death_reorg").await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub(crate) async fn recheck_proven_recredit(
        &mut self,
        id: U256,
        revision: i64,
        checked: &History,
    ) -> Result<()> {
        let tx = self.client.transaction().await?;
        lock_bridge(&tx).await?;
        let r = redeem::load(&tx, id).await?;
        let d = load(&tx, id).await?.context("death history missing")?;
        ensure!(
            d.revision == revision && d.history.as_ref() == Some(checked),
            "death history changed before side effect"
        );
        check_recovery(&tx, &r).await?;
        tx.commit().await?;
        Ok(())
    }

    /// The caller freshly revalidates this exact history while holding the shared
    /// payment lane. The locked comparison prevents stale scans authorizing work.
    pub(crate) async fn authorize_proven_recredit(
        &mut self,
        id: U256,
        version: u64,
        pair: BridgeCheckpointPair,
        revision: i64,
        checked: &History,
    ) -> Result<BridgeRedeem> {
        let tx = self.client.transaction().await?;
        lock_bridge(&tx).await?;
        ensure!(
            checkpoint_pair(&tx).await? == pair,
            "authorization checkpoint changed"
        );
        let r = redeem::load(&tx, id).await?;
        ensure!(
            r.canonical && r.actual > 0,
            "canonical nonzero burn required"
        );
        check_event(&tx, &r.event(), pair.checkpoint).await?;
        let d = load(&tx, id)
            .await?
            .context("complete payout history required")?;
        ensure!(
            d.revision == revision
                && d.status == "complete"
                && d.history.as_ref() == Some(checked)
                && checked.complete_absence(),
            "complete unchanged payout history required"
        );
        checked.matches_attempts(&d.attempts)?;
        unpaid_attempt(&tx, &r).await?;
        check_checkpoint(&tx, checked).await?;
        if !d.acted_on {
            ensure!(
                r.state == RedeemState::Reserved && r.version == version,
                "stale recovery authorization"
            );
            let signed: bool = tx
                .query_one(
                    "SELECT EXISTS(SELECT 1 FROM zama_transactions WHERE operation_id=$1)",
                    &[&r.recredit_id()],
                )
                .await?
                .get(0);
            ensure!(!signed, "recredit journal lacks authorization");
            tx.execute(
                "UPDATE bridge_payout_death SET acted_on=true WHERE redeem_id=$1",
                &[&id.to_string()],
            )
            .await?;
            tx.execute("UPDATE bridge_redemptions SET state='recredit_pending',version=version+1 WHERE redeem_id=$1", &[&id.to_string()]).await?;
            tx.execute("UPDATE zama_receipts SET acted_on=true WHERE tx_hash=$1 AND canonical AND confirmed AND success", &[&r.source_tx.as_slice()]).await?;
        } else {
            ensure!(
                matches!(
                    r.state,
                    RedeemState::RecreditPending | RedeemState::Recredited
                ),
                "invalid recovery transition"
            );
        }
        let r = redeem::load(&tx, id).await?;
        tx.commit().await?;
        Ok(r)
    }
}

async fn load(tx: &Transaction<'_>, id: U256) -> Result<Option<DeathCollection>> {
    let Some(row) = tx.query_opt("SELECT d.*,b.chain_id AS active_chain,b.contract AS active_contract FROM bridge_payout_death d CROSS JOIN bridge_domain b WHERE redeem_id=$1", &[&id.to_string()]).await? else { return Ok(None) };
    decode(tx, row).await.map(Some)
}
async fn decode(tx: &Transaction<'_>, row: Row) -> Result<DeathCollection> {
    ensure!(
        row.get::<_, i64>("chain_id") == row.get::<_, i64>("active_chain")
            && row.get::<_, Vec<u8>>("contract") == row.get::<_, Vec<u8>>("active_contract"),
        "history domain mismatch"
    );
    let op: String = row.get("operation_id");
    let attempt = read_attempt(tx, &op)
        .await?
        .context("history journal missing")?
        .prepared
        .context("history signed bytes missing")?;
    ensure!(
        attempt.bytes == row.get::<_, Vec<u8>>("signed_bytes"),
        "history signed identity changed"
    );
    Ok(DeathCollection {
        redeem_id: row.get::<_, String>("redeem_id").parse()?,
        operation_id: op,
        attempts: vec![attempt],
        history: row
            .get::<_, Option<String>>("history")
            .map(|s| serde_json::from_str(&s))
            .transpose()?,
        revision: row.get("revision"),
        status: row.get("status"),
        acted_on: row.get("acted_on"),
    })
}
async fn unpaid_attempt(tx: &Transaction<'_>, r: &BridgeRedeem) -> Result<PreparedTransaction> {
    let paid: bool = tx.query_one("SELECT paid_height IS NOT NULL OR EXISTS(SELECT 1 FROM zama_transactions WHERE operation_id=$2) FROM bridge_redemptions WHERE redeem_id=$1", &[&r.id.to_string(), &r.fulfillment_id()]).await?.get(0);
    ensure!(
        !paid && !matches!(r.state, RedeemState::Paid | RedeemState::Fulfilled),
        "paid attempts cannot recover"
    );
    for row in tx
        .query("SELECT event FROM zama_events WHERE canonical", &[])
        .await?
    {
        if let zama::watcher::Event::RedeemFulfilled { redeem_id, .. } =
            serde_json::from_str(row.get::<_, &str>(0))?
        {
            ensure!(redeem_id != r.id, "fulfillment blocks recovery");
        }
    }
    let a = read_attempt(tx, &r.operation_id)
        .await?
        .context("issued journal required")?;
    ensure!(
        a.observed_height.is_none()
            && a.amount
                == r.actual
                    .checked_sub(crate::core::redeem_types::REDEEM_FEE_ZAT)
                    .context("invalid burn")?
            && Some(&a.destination) == r.destination.as_ref(),
        "payment observation or intent blocks recovery"
    );
    let p = a.prepared.context("unsigned intent blocks recovery")?;
    ensure!(
        super::same_inputs(&a.inputs, &p.inputs),
        "issued input mismatch"
    );
    Ok(p)
}
async fn check_checkpoint(tx: &Transaction<'_>, h: &History) -> Result<()> {
    let canonical: bool = tx.query_one("SELECT EXISTS(SELECT 1 FROM checkpoints c JOIN account a ON true WHERE c.height=$1 AND c.hash=$2 AND a.current_height>=c.height)", &[&i64::from(h.checkpoint_height()), &&h.checkpoint_hash()[..]]).await?.get(0);
    ensure!(canonical, "death checkpoint not in scanner history");
    Ok(())
}
/// Existing re-credit signing, rebroadcast and settlement barriers all call this.
pub(super) async fn check_recovery(tx: &Transaction<'_>, r: &BridgeRedeem) -> Result<()> {
    ensure!(
        matches!(
            r.state,
            RedeemState::RecreditPending | RedeemState::Recredited
        ),
        "issued recovery not authorized"
    );
    let d = load(tx, r.id)
        .await?
        .context("payout journal blocks recredit")?;
    ensure!(
        d.operation_id == r.operation_id && d.acted_on && d.status == "complete",
        "complete acted death evidence required"
    );
    let h = d.history.context("history missing")?;
    ensure!(h.complete_absence(), "history does not prove absence");
    h.matches_attempts(&d.attempts)?;
    unpaid_attempt(tx, r).await?;
    check_checkpoint(tx, &h).await
}
