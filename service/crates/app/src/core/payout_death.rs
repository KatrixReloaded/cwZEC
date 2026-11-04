//! Issued-payout diagnostics and explicitly requested complete-history collection.
//! Diagnostics never authorize recovery. Only durable completed absence evidence,
//! freshly revalidated under the shared payment lane, can authorize re-credit.

use crate::store::Attempt;
use serde::Serialize;
use zcash::rpc::{LightwalletClient, TransactionLookup};

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeathBlocker {
    SignedTransactionMissing,
    ExpiryDisabled,
    ExpiryNotBuried,
    TipUnavailable,
    JournalRecordsInclusion,
    TransactionReturned,
    LookupNotFoundUnverified,
    LookupFailed,
    LookupEmptyResponse,
    CompleteCanonicalHistoryUnavailable,
}

#[derive(Debug, Serialize)]
pub struct AttemptDeathInspection {
    pub operation_id: String,
    pub txid: Option<String>,
    pub expiry_height: Option<u32>,
    pub tip_height: Option<u32>,
    pub recovery_enabled: bool,
    pub reasons: Vec<DeathBlocker>,
}

/// Callers must supply the entire immutable issued journal, including attempts
/// with no broadcast acknowledgement and already observed/paid attempts.
/// Each RPC is bounded by LightwalletClient's endpoint deadline. Failure of one
/// lookup does not suppress inspection of later attempts.
pub async fn inspect_issued_attempts(
    client: &LightwalletClient,
    attempts: &[Attempt],
) -> Vec<AttemptDeathInspection> {
    let tip = client.tip().await.ok();
    let mut inspections = Vec::with_capacity(attempts.len());
    for attempt in attempts {
        let lookup = match &attempt.prepared {
            Some(prepared) => Some(client.inspect_transaction(prepared.txid).await),
            None => None,
        };
        inspections.push(assess_attempt(attempt, tip, lookup));
    }
    inspections
}

fn assess_attempt(
    attempt: &Attempt,
    tip: Option<u32>,
    lookup: Option<TransactionLookup>,
) -> AttemptDeathInspection {
    let mut reasons = vec![DeathBlocker::CompleteCanonicalHistoryUnavailable];
    if tip.is_none() {
        reasons.push(DeathBlocker::TipUnavailable);
    }
    if attempt.observed_height.is_some() {
        reasons.push(DeathBlocker::JournalRecordsInclusion);
    }
    if let Some(prepared) = &attempt.prepared {
        if prepared.expiry_height == 0 {
            reasons.push(DeathBlocker::ExpiryDisabled);
        } else if tip.is_some_and(|tip| u64::from(tip) < u64::from(prepared.expiry_height) + 3) {
            reasons.push(DeathBlocker::ExpiryNotBuried);
        }
        reasons.push(match lookup {
            Some(TransactionLookup::Returned { .. }) => DeathBlocker::TransactionReturned,
            Some(TransactionLookup::NotFoundUnverified) => DeathBlocker::LookupNotFoundUnverified,
            Some(TransactionLookup::EmptyResponse) => DeathBlocker::LookupEmptyResponse,
            Some(TransactionLookup::RpcFailure) | None => DeathBlocker::LookupFailed,
        });
    } else {
        reasons.push(DeathBlocker::SignedTransactionMissing);
    }
    AttemptDeathInspection {
        operation_id: attempt.operation_id.clone(),
        txid: attempt.prepared.as_ref().map(|tx| {
            let mut display = tx.txid;
            display.reverse();
            hex::encode(display)
        }),
        expiry_height: attempt.prepared.as_ref().map(|tx| tx.expiry_height),
        tip_height: tip,
        recovery_enabled: false,
        reasons,
    }
}

#[cfg(test)]
#[path = "payout_death_tests.rs"]
mod tests;

/// Revalidate the exact durable evidence. Only a proven chain change invalidates
/// progress; transport uncertainty leaves it intact and prevents the caller acting.
pub(crate) async fn revalidate_collection(
    store: &mut crate::store::Store,
    client: &LightwalletClient,
    collection: &crate::store::DeathCollection,
) -> anyhow::Result<()> {
    anyhow::ensure!(collection.status != "invalid", "payout history invalidated");
    let history = collection
        .history
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("payout history incomplete"))?;
    let result = zcash::rpc::history::revalidate(client, &collection.attempts, history).await;
    if result.as_ref().is_err_and(|e| {
        e.downcast_ref::<zcash::rpc::history::HistoryReorg>()
            .is_some()
    }) {
        store
            .invalidate_payout_history(collection.redeem_id, collection.revision)
            .await?;
    }
    result
}

/// All acted evidence is checked, even if an earlier request failed. A single
/// uncertain or invalid result blocks every subsequent coordinator side effect.
pub(crate) async fn revalidate_acted(
    store: &mut crate::store::Store,
    client: &LightwalletClient,
) -> anyhow::Result<()> {
    let mut failure = None;
    for collection in store
        .payout_histories()
        .await?
        .into_iter()
        .filter(|d| d.acted_on)
    {
        if let Err(error) = revalidate_collection(store, client, &collection).await {
            failure.get_or_insert(error);
        }
    }
    match failure {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// At most one requested redemption and sixteen consecutive blocks per pass.
/// A failed batch is never saved. Invalid unacted evidence requires a new
/// explicit operator request; it cannot silently restart here.
pub(crate) async fn collect_once(
    store: &mut crate::store::Store,
    client: &LightwalletClient,
) -> anyhow::Result<bool> {
    revalidate_acted(store, client).await?;
    let Some(collection) = store
        .payout_histories()
        .await?
        .into_iter()
        .find(|d| !d.acted_on && d.status == "collecting")
    else {
        return Ok(false);
    };
    let result = async {
        let history = match &collection.history {
            Some(h) => h.clone(),
            None => zcash::rpc::history::start(client, &collection.attempts).await?,
        };
        zcash::rpc::history::advance(client, &collection.attempts, &history, 16).await
    }
    .await;
    match result {
        Ok(history) => {
            store
                .save_payout_history(collection.redeem_id, collection.revision, &history)
                .await?
        }
        Err(error) => {
            if error
                .downcast_ref::<zcash::rpc::history::HistoryReorg>()
                .is_some()
            {
                store
                    .invalidate_payout_history(collection.redeem_id, collection.revision)
                    .await?;
            }
            return Err(error);
        }
    }
    Ok(true)
}

/// The caller holds the shared payment lane throughout this RPC and store CAS.
pub(crate) async fn authorize_recredit(
    store: &mut crate::store::Store,
    client: &LightwalletClient,
    id: alloy::primitives::U256,
    version: u64,
    pair: super::bridge_types::BridgeCheckpointPair,
) -> anyhow::Result<super::redeem_types::BridgeRedeem> {
    revalidate_acted(store, client).await?;
    if let Some(collection) = store.payout_history(id).await? {
        revalidate_collection(store, client, &collection).await?;
        let history = collection
            .history
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("payout history incomplete"))?;
        store
            .authorize_proven_recredit(id, version, pair, collection.revision, history)
            .await
    } else {
        store.authorize_bridge_recredit_at(id, version, pair).await
    }
}
