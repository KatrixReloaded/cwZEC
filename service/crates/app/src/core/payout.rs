use super::{Service, deposit_id, parse_memo};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    sync::{Arc, atomic::Ordering},
};
use zcash::{
    core::{Signer, decode_address},
    vault,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PayoutRequest {
    pub operation_id: String,
    pub deposit_id: String,
    pub destination: String,
    /// Authorized portion of this test deposit, including the fixed published fee.
    pub gross_amount_zat: u64,
}

impl Service {
    pub async fn prepare_payout(self: &Arc<Self>, request: PayoutRequest) -> Result<()> {
        let _payment = self.payment_lock.lock().await;
        ensure!(
            !self.store.lock().await.bridge_enabled().await?,
            "bridge payouts require bridge coordinator authorization"
        );
        ensure!(
            !request.operation_id.is_empty() && request.operation_id.len() <= 128,
            "invalid operation ID"
        );
        decode_address(&request.destination)?;
        let amount = request
            .gross_amount_zat
            .checked_sub(vault::FIXED_FEE_ZAT)
            .filter(|v| *v > 0)
            .context("payout does not cover the fixed fee and a positive payment")?;
        let mut tag = [0; 32];
        hex::decode_to_slice(&request.deposit_id, &mut tag).context("invalid deposit ID")?;
        self.scan_once().await?;
        ensure!(self.healthy.load(Ordering::Acquire), "scanner unavailable");
        let mut store = self.store.lock().await;
        let existing = store.get_attempt(&request.operation_id).await?;
        if let Some(attempt) = &existing {
            ensure!(
                attempt.destination == request.destination && attempt.amount == amount,
                "operation terms mismatch"
            );
            let records = store.deposits().await?;
            ensure!(
                records
                    .iter()
                    .any(|d| attempt.inputs.contains(&d.id) && deposit_id(&d.rho, &d.rseed) == tag),
                "operation source mismatch or orphaned source"
            );
            if attempt.prepared.is_some() {
                drop(store);
                return self.submit_recorded(&request.operation_id).await;
            }
        }
        let mut state = store.load().await?;
        state.notes.retain(|n| {
            !n.internal && deposit_id(&n.rho, &n.rseed) == tag && parse_memo(&n.memo).is_ok()
        });
        let reserved: HashSet<_> = store
            .reserved_inputs()
            .await?
            .into_iter()
            .filter(|(id, _)| existing.as_ref().is_none_or(|a| !a.inputs.contains(id)))
            .map(|(_, nf)| nf)
            .collect();
        let input = vault::select_input(&state, &reserved, self.confirmations, amount)?.clone();
        store
            .reserve(
                &request.operation_id,
                &request.destination,
                amount,
                &[input.id],
                self.confirmations,
            )
            .await?;
        let checkpoint = state.checkpoint;
        let expected_height = checkpoint.height;
        let expected_hash = checkpoint.hash;
        drop(store);
        let signer = self.signer.clone();
        let destination = request.destination;
        let prepared = tokio::task::spawn_blocking(move || {
            signer.sign(&checkpoint, &input, &destination, amount)
        })
        .await??;
        let mut store = self.store.lock().await;
        ensure!(
            store
                .checkpoint(expected_height)
                .await?
                .is_some_and(|c| c.hash == expected_hash),
            "signing anchor was reorganized"
        );
        ensure!(
            store.load().await?.checkpoint.height < prepared.expiry_height,
            "signed transaction already expired"
        );
        store
            .save_prepared(&request.operation_id, &prepared)
            .await?;
        drop(store);
        interrupt("journal", &prepared.bytes);
        self.submit_recorded(&request.operation_id).await
    }

    /// A network failure is ambiguous. Only stored bytes may be retried.
    async fn submit_recorded(&self, id: &str) -> Result<()> {
        let attempt = self
            .store
            .lock()
            .await
            .get_attempt(id)
            .await?
            .context("missing payment operation")?;
        if attempt.observed_height.is_some() {
            return Ok(());
        }
        let prepared = attempt.prepared.context("payment is not prepared")?;
        let tip = self.watcher.client().tip().await?;
        if tip >= prepared.expiry_height {
            // No replacement or release: historical absence/death is not established.
            return Ok(());
        }
        if vault::broadcast(self.watcher.client(), &prepared)
            .await
            .is_ok()
        {
            interrupt("broadcast", &prepared.bytes);
            self.store.lock().await.mark_broadcast(id).await?;
        }
        Ok(())
    }

    pub async fn recover(&self) -> Result<()> {
        let _payment = self.payment_lock.lock().await;
        ensure!(
            !self.store.lock().await.bridge_enabled().await?,
            "bridge payouts require bridge coordinator recovery"
        );
        ensure!(
            self.healthy.load(Ordering::Acquire),
            "scan required before recovery"
        );
        let attempts = self.store.lock().await.unfinished().await?;
        for attempt in attempts {
            if attempt.prepared.is_some() {
                self.submit_recorded(&attempt.operation_id).await?;
            }
        }
        // Reserved-but-unsigned operations wait for an identical authenticated request.
        Ok(())
    }
}

/// Development failure injection for the journal and ambiguous-submission checks.
pub(super) fn interrupt(stage: &str, signed_bytes: &[u8]) {
    if std::env::var("CWZEC_INTERRUPT_AFTER").as_deref() == Ok(stage) {
        // Public fingerprint only, never the payload, recipient, or amount.
        eprintln!(
            "CWZEC_INTERRUPTED {stage} {}",
            hex::encode(Sha256::digest(signed_bytes))
        );
        std::process::exit(75);
    }
}
