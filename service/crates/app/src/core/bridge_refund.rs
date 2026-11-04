//! Known-participant refunds use the existing operator and wallet journals.
use super::{Service, bridge_mint::MintFees, bridge_types::*, zama::ZamaService};
use crate::store::{Store, TransactionState};
use alloy::signers::local::PrivateKeySigner;
use anyhow::{Context, Result, ensure};
use std::{
    collections::HashSet,
    sync::{Arc, atomic::Ordering},
};
use zama::contract::DepositStatus;
use zcash::{core::Signer, vault};

pub struct BridgeRefunder {
    store: Store,
    zama: ZamaService,
    wallet: Arc<Service>,
}
impl BridgeRefunder {
    pub async fn new(mut store: Store, zama: ZamaService, wallet: Arc<Service>) -> Result<Self> {
        store
            .initialize_bridge(BridgeDomain {
                chain_id: zama.client().chain_id(),
                contract: zama.client().address(),
            })
            .await?;
        Ok(Self {
            store,
            zama,
            wallet,
        })
    }
    /// Settle existing reservations before accounting removes paid assets.
    pub async fn reconcile_at(&mut self, pair: BridgeCheckpointPair) -> Result<()> {
        self.check_pair(pair).await?;
        for refund in self.store.bridge_refunds().await? {
            if refund.state != RefundState::Authorized {
                self.check_pair(pair).await?;
                self.store
                    .settle_bridge_refund(refund.deposit_id, pair)
                    .await?;
            }
        }
        Ok(())
    }
    /// At most one void submission or wallet transaction per pass.
    pub async fn advance_once(
        &mut self,
        accounting_id: i64,
        signer: &PrivateKeySigner,
        fees: MintFees,
    ) -> Result<bool> {
        let wallet = self.wallet.clone();
        let _payment = wallet.payment_lock.lock().await;
        let pair = self.store.bridge_checkpoint_pair().await?;
        self.check_pair(pair).await?;
        let refunds = self.store.bridge_refunds().await?;
        for r in refunds.iter().filter(|r| r.state == RefundState::Reserved) {
            let attempt = self
                .wallet
                .store
                .lock()
                .await
                .get_attempt(&r.operation_id)
                .await?
                .context("reserved refund journal missing")?;
            if attempt.prepared.is_some() && self.recover_payment(r, pair).await? {
                return Ok(true);
            }
        }
        for r in refunds.iter().filter(|r| r.state != RefundState::Paid) {
            self.check_pair(pair).await?;
            let status = self
                .zama
                .client()
                .consumed_at(r.deposit_id, pair.checkpoint)
                .await?;
            if status == DepositStatus::Minted {
                self.store.halt_bridge("refund_deposit_minted").await?;
                anyhow::bail!("refund deposit minted; liability retained");
            }
            if status == DepositStatus::None {
                if r.state != RefundState::Authorized {
                    self.store.halt_bridge("refund_void_missing").await?;
                    anyhow::bail!("reserved refund lost void");
                }
                if self.advance_void(r, pair, signer, fees).await? {
                    return Ok(true);
                }
                continue;
            }
            ensure!(
                status == DepositStatus::Refunded,
                "unsupported consumed status"
            );
            // The reconciler must ingest the same confirmed DepositVoided first.
            let d = self
                .store
                .bridge_deposits()
                .await?
                .into_iter()
                .find(|d| d.deposit_id == r.deposit_id)
                .context("refund deposit missing")?;
            if d.state != DepositState::Voided {
                continue;
            }
            let mut store = self.wallet.store.lock().await;
            let mut state = store.load().await?;
            let amount = r
                .gross_amount
                .checked_sub(r.fee)
                .filter(|n| *n > 0)
                .context("invalid refund amount")?;
            let input = if r.state == RefundState::Reserved {
                let attempt = store
                    .get_attempt(&r.operation_id)
                    .await?
                    .context("refund journal missing")?;
                if attempt.prepared.is_some() {
                    continue;
                }
                ensure!(
                    attempt.inputs.len() == 1
                        && attempt.amount == amount
                        && attempt.destination == r.destination,
                    "reserved refund intent mismatch"
                );
                state
                    .notes
                    .iter()
                    .find(|n| n.id == attempt.inputs[0] && n.spent_height.is_none())
                    .context("reserved refund input unavailable; reservation retained")?
                    .clone()
            } else {
                state.notes.retain(|n| n.mined_height <= pair.zcash_height);
                let reserved: HashSet<_> = store
                    .reserved_inputs()
                    .await?
                    .into_iter()
                    .map(|(_, nf)| nf)
                    .collect();
                vault::select_input(&state, &reserved, 3, amount)?.clone()
            };
            drop(store);
            self.check_pair(pair).await?;
            if r.state == RefundState::Authorized {
                self.store
                    .reserve_bridge_refund(r.deposit_id, r.version, accounting_id, input.id)
                    .await?;
            }
            let destination = r.destination.clone();
            let anchor = state.checkpoint;
            let expected_height = anchor.height;
            let expected_hash = anchor.hash;
            let wallet_signer = self.wallet.signer.clone();
            self.store.check_payment_accounting().await?;
            let prepared = tokio::task::spawn_blocking(move || {
                wallet_signer.sign(&anchor, &input, &destination, amount)
            })
            .await??;
            self.check_pair(pair).await?;
            let mut store = self.wallet.store.lock().await;
            ensure!(
                store
                    .checkpoint(expected_height)
                    .await?
                    .is_some_and(|c| c.hash == expected_hash),
                "refund payment anchor reorganized"
            );
            ensure!(
                prepared.expiry_height > 0
                    && store.load().await?.checkpoint.height < prepared.expiry_height,
                "refund expired while signing; reservation retained"
            );
            store.save_prepared(&r.operation_id, &prepared).await?;
            drop(store);
            super::payout::interrupt("refund-journal", &prepared.bytes);
            self.recover_payment(r, pair).await?;
            return Ok(true);
        }
        Ok(false)
    }
    async fn recover_payment(
        &mut self,
        r: &BridgeRefund,
        pair: BridgeCheckpointPair,
    ) -> Result<bool> {
        self.check_pair(pair).await?;
        let a = self
            .wallet
            .store
            .lock()
            .await
            .get_attempt(&r.operation_id)
            .await?
            .context("refund journal missing")?;
        ensure!(
            a.destination == r.destination && a.amount == r.gross_amount - r.fee,
            "saved refund intent mismatch"
        );
        if a.observed_height.is_some() {
            return Ok(false);
        }
        let p = a.prepared.context("refund is not prepared")?;
        if self.wallet.watcher.client().tip().await? >= p.expiry_height {
            return Ok(false);
        }
        // Revalidate the void immediately before rebroadcasting saved bytes.
        self.store.settle_bridge_refund(r.deposit_id, pair).await?;
        self.check_pair(pair).await?;
        if vault::broadcast(self.wallet.watcher.client(), &p)
            .await
            .is_ok()
        {
            super::payout::interrupt("refund-broadcast", &p.bytes);
            self.wallet
                .store
                .lock()
                .await
                .mark_broadcast(&r.operation_id)
                .await?;
        }
        Ok(true)
    }
    async fn advance_void(
        &mut self,
        r: &BridgeRefund,
        pair: BridgeCheckpointPair,
        signer: &PrivateKeySigner,
        fees: MintFees,
    ) -> Result<bool> {
        let call = self.zama.client().domain().void_deposit(r.deposit_id);
        if let Some(saved) = self.zama.transaction(&r.void_operation_id).await? {
            ensure!(
                saved.signed.sender() == signer.address()
                    && saved.signed.matches_call(&call)
                    && saved.signed.void_identity() == Some(r.deposit_id),
                "saved void identity mismatch"
            );
            if saved.confirmed
                || matches!(
                    saved.state,
                    TransactionState::Successful | TransactionState::Reverted
                )
            {
                return Ok(false);
            }
        } else {
            ensure!(
                fees.gas_limit > 0
                    && fees.max_fee_per_gas > 0
                    && fees.max_priority_fee_per_gas <= fees.max_fee_per_gas,
                "invalid void fees"
            );
            self.check_pair(pair).await?;
            self.zama
                .prepare_operator(
                    &r.void_operation_id,
                    signer,
                    &call,
                    fees.gas_limit,
                    fees.max_fee_per_gas,
                    fees.max_priority_fee_per_gas,
                )
                .await?;
        }
        self.zama.recover_operator(&r.void_operation_id).await?;
        Ok(true)
    }
    async fn check_pair(&mut self, pair: BridgeCheckpointPair) -> Result<()> {
        ensure!(
            self.wallet.healthy.load(Ordering::Acquire),
            "wallet scanner unavailable"
        );
        ensure!(
            self.store.bridge_checkpoint_pair().await? == pair,
            "checkpoint pair changed; reconcile and account again"
        );
        self.zama.client().check_canonical(pair.checkpoint).await?;
        ensure!(
            self.zama.latest_height().await? > pair.checkpoint.number,
            "EVM checkpoint needs two confirmations"
        );
        ensure!(
            u64::from(self.wallet.watcher.client().tip().await?)
                >= u64::from(pair.zcash_height) + 2,
            "Zcash checkpoint needs three confirmations"
        );
        ensure!(
            self.wallet
                .watcher
                .client()
                .block_hash(pair.zcash_height)
                .await?
                == pair.zcash_hash,
            "Zcash checkpoint is not canonical"
        );
        Ok(())
    }
}

#[cfg(test)]
#[path = "bridge_refund_tests.rs"]
mod tests;
