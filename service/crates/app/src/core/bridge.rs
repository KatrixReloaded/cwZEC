//! Deposit reconciliation only: no encryption, transaction signing, broadcast or payout.
use super::bridge_types::*;
use crate::store::Store;
use alloy::{primitives::B256, signers::local::PrivateKeySigner};
use anyhow::{Result, ensure};
use zama::{
    contract::{CwzecClient, DepositStatus},
    fhe,
    watcher::Event,
};
use zcash::rpc::LightwalletClient;

pub struct BridgeReconciler {
    store: Store,
    contract: CwzecClient,
    sidecar: fhe::Client,
    zcash: LightwalletClient,
}

impl BridgeReconciler {
    pub async fn new(
        mut store: Store,
        contract: CwzecClient,
        sidecar: fhe::Client,
        zcash: LightwalletClient,
    ) -> Result<Self> {
        store
            .initialize_bridge(BridgeDomain {
                chain_id: contract.chain_id(),
                contract: contract.address(),
            })
            .await?;
        Ok(Self {
            store,
            contract,
            sidecar,
            zcash,
        })
    }

    pub async fn checkpoint_pair(&mut self) -> Result<BridgeCheckpointPair> {
        self.store.bridge_checkpoint_pair().await
    }

    /// Both scanners must already cover this pair. Every contract state read is
    /// pinned by canonical block hash; sidecar permits never select chain state.
    pub async fn reconcile_at(
        &mut self,
        pair: BridgeCheckpointPair,
        service: &PrivateKeySigner,
        auditor: &PrivateKeySigner,
    ) -> Result<AccountingReport> {
        self.check_pair(pair).await?;
        let deposits = self.store.ingest_bridge_deposits().await?;
        let events = self.store.zama_events().await?;
        for deposit in deposits
            .into_iter()
            .filter(|d| d.canonical && d.source_height <= pair.zcash_height)
        {
            let evidence = read_consumption(
                &self.contract,
                &self.sidecar,
                service,
                deposit.deposit_id,
                pair.checkpoint,
                &events,
            )
            .await?;
            self.check_pair(pair).await?;
            let result = self
                .store
                .reconcile_bridge_deposit(deposit.deposit_id, deposit.version, &evidence)
                .await?;
            ensure!(
                result.state != DepositState::Mismatch,
                "actual mint mismatch; durable reconciliation halt"
            );
        }
        let supply_handle = self.contract.supply_at(pair.checkpoint).await?;
        let supply = if supply_handle.is_zero() {
            let origin =
                self.store.bridge_origin().await?.ok_or_else(|| {
                    anyhow::anyhow!("verified pristine deployment evidence required")
                })?;
            ensure!(
                origin.service == service.address() && origin.auditor == auditor.address(),
                "origin signer mismatch"
            );
            self.contract.check_canonical(origin.block).await?;
            self.contract
                .verify_pristine_supply(
                    pair.checkpoint,
                    origin.runtime_hash,
                    origin.service,
                    origin.auditor,
                )
                .await?;
            0
        } else {
            decrypt(&self.contract, &self.sidecar, supply_handle, auditor).await?
        };
        self.check_pair(pair).await?;
        self.store
            .bridge_accounting(&AccountingEvidence {
                zcash_height: pair.zcash_height,
                zcash_hash: pair.zcash_hash,
                checkpoint: pair.checkpoint,
                supply_handle,
                supply,
            })
            .await
    }

    async fn check_pair(&self, pair: BridgeCheckpointPair) -> Result<()> {
        self.contract.check_canonical(pair.checkpoint).await?;
        ensure!(
            zama::watcher::Watcher::new(&self.contract)
                .latest()
                .await?
                .reference
                .number
                > pair.checkpoint.number,
            "EVM checkpoint needs two canonical confirmations"
        );
        ensure!(
            u64::from(self.zcash.tip().await?) >= u64::from(pair.zcash_height) + 2,
            "Zcash checkpoint needs three confirmations"
        );
        ensure!(
            self.zcash.block_hash(pair.zcash_height).await? == pair.zcash_hash,
            "Zcash checkpoint is not canonical"
        );
        Ok(())
    }
}

async fn read_consumption(
    contract: &CwzecClient,
    sidecar: &fhe::Client,
    signer: &PrivateKeySigner,
    id: B256,
    checkpoint: zama::watcher::BlockRef,
    events: &[zama::watcher::EventObservation],
) -> Result<ConsumptionEvidence> {
    let status = contract.consumed_at(id, checkpoint).await?;
    let matching: Vec<_> = events
        .iter()
        .filter(|e| {
            e.block.number <= checkpoint.number
                && match e.event {
                    Event::DepositMinted { deposit_id, .. }
                    | Event::DepositVoided { deposit_id } => deposit_id == id,
                    _ => false,
                }
        })
        .cloned()
        .collect();
    ensure!(
        matching.len() <= 1,
        "conflicting deposit lifecycle evidence"
    );
    let (actual_handle, actual_amount) = if status == DepositStatus::Minted {
        let handle = contract.minted_amount_at(id, checkpoint).await?;
        (
            Some(handle),
            Some(decrypt(contract, sidecar, handle, signer).await?),
        )
    } else {
        (None, None)
    };
    contract.check_canonical(checkpoint).await?;
    Ok(ConsumptionEvidence {
        checkpoint,
        status,
        event: matching.into_iter().next(),
        actual_handle,
        actual_amount,
    })
}

pub(crate) async fn decrypt(
    contract: &CwzecClient,
    sidecar: &fhe::Client,
    handle: B256,
    signer: &PrivateKeySigner,
) -> Result<u64> {
    ensure!(
        !handle.is_zero(),
        "missing encrypted result; reconciliation blocked"
    );
    let prepared = sidecar
        .prepare_decryption(fhe::Scope {
            contract: contract.address(),
            user: signer.address(),
            handle,
        })
        .await?;
    let context = contract.kms_context().await?;
    let authorized = prepared.authorize(signer, context).await?;
    sidecar.decrypt(authorized).await
}

#[cfg(test)]
#[path = "bridge_tests.rs"]
mod tests;
