//! One bounded deposit-to-mint pass through the existing durable operator journal.
use super::{bridge_types::*, zama::ZamaService};
use crate::store::{Store, ZamaTransaction};
use alloy::signers::local::PrivateKeySigner;
use anyhow::{Result, ensure};
use zama::{contract::DepositStatus, fhe};

#[derive(Clone, Copy)]
pub struct MintFees {
    pub gas_limit: u64,
    pub max_fee_per_gas: u128,
    pub max_priority_fee_per_gas: u128,
}

pub struct BridgeMinter {
    store: Store,
    zama: ZamaService,
    sidecar: fhe::Client,
}

impl BridgeMinter {
    pub async fn new(mut store: Store, zama: ZamaService, sidecar: fhe::Client) -> Result<Self> {
        store
            .initialize_bridge(BridgeDomain {
                chain_id: zama.client().chain_id(),
                contract: zama.client().address(),
            })
            .await?;
        Ok(Self {
            store,
            zama,
            sidecar,
        })
    }

    /// Run after reconciliation. A claimed operation takes precedence over fresh work.
    /// A receipt, including a revert, never settles the deposit here: only the
    /// reconciler can compare actual issuance and release its liability.
    pub async fn mint_once(
        &mut self,
        accounting_id: i64,
        signer: &PrivateKeySigner,
        fees: MintFees,
    ) -> Result<Option<ZamaTransaction>> {
        let deposits = self.store.bridge_deposits().await?;
        // Search all pending claims before choosing unsigned work: concurrent
        // claims may exist, but the signed operator lane must recover first.
        for deposit in deposits
            .iter()
            .filter(|d| d.state == DepositState::MintPending)
        {
            let Some(saved) = self.zama.transaction(&deposit.operation_id).await? else {
                continue;
            };
            ensure!(
                deposit.canonical && deposit.claimed,
                "saved mint lacks canonical pending claim"
            );
            self.store.bridge_checkpoint_pair().await?;
            ensure!(
                saved.signed.sender() == signer.address()
                    && saved.signed.mint_identity()
                        == deposit.recipient.map(|to| (to, deposit.deposit_id)),
                "saved mint identity differs from deposit"
            );
            return Ok(Some(
                self.zama.recover_operator(&deposit.operation_id).await?,
            ));
        }
        let candidate = deposits
            .iter()
            .find(|d| d.state == DepositState::MintPending)
            .or_else(|| deposits.iter().find(|d| d.state == DepositState::Eligible));
        let Some(deposit) = candidate else {
            return Ok(None);
        };
        ensure!(deposit.canonical, "mint source is not canonical");

        ensure!(
            fees.gas_limit > 0
                && fees.max_fee_per_gas > 0
                && fees.max_priority_fee_per_gas <= fees.max_fee_per_gas,
            "invalid mint transaction fees"
        );
        // The coordinator scanned before accounting. Advancing that scan here
        // would invalidate the report whenever a new block arrives.
        let deposit = self
            .store
            .authorize_bridge_mint(deposit.deposit_id, deposit.version, accounting_id)
            .await?;
        let pair = self.store.bridge_checkpoint_pair().await?;
        ensure!(
            self.zama.latest_height().await? > pair.checkpoint.number,
            "mint checkpoint needs two canonical confirmations"
        );
        // Reconciliation validated the Zcash checkpoint through lightwalletd;
        // store authorization revalidates that same scanner/source dependency.
        // Also refuse already consumed tags before generating any new input.
        ensure!(
            self.zama
                .client()
                .consumed_at(deposit.deposit_id, pair.checkpoint)
                .await?
                == DepositStatus::None,
            "deposit consumed; reconcile before minting"
        );
        let encrypted = self
            .sidecar
            .encrypt(
                self.zama.client().address(),
                signer.address(),
                deposit.amount,
            )
            .await?;
        self.zama.client().check_canonical(pair.checkpoint).await?;
        self.store
            .authorize_bridge_mint(deposit.deposit_id, deposit.version, accounting_id)
            .await?;
        let call = self.zama.client().domain().mint(
            deposit
                .recipient
                .ok_or_else(|| anyhow::anyhow!("mint recipient missing"))?,
            encrypted.handle,
            encrypted.input_proof,
            deposit.deposit_id,
        );
        // prepare_operator persists signed bytes and owns nonce exclusion. The
        // store repeats source/accounting authorization in that atomic journal write.
        let prepared = self
            .zama
            .prepare_operator(
                &deposit.operation_id,
                signer,
                &call,
                fees.gas_limit,
                fees.max_fee_per_gas,
                fees.max_priority_fee_per_gas,
            )
            .await?;
        super::payout::interrupt("mint-journal", prepared.signed.raw());
        Ok(Some(
            self.zama.recover_operator(&deposit.operation_id).await?,
        ))
    }
}

#[cfg(test)]
#[path = "bridge_mint_tests.rs"]
mod tests;
