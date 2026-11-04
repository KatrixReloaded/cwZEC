//! Canonical hash-pinned reads for application reconciliation. Never falls back to latest.
use super::{CwzecClient, DepositStatus, bindings::*};
use crate::watcher::{BlockRef, Watcher};
use alloy::{
    eips::BlockId,
    primitives::{B256, U256, address},
    providers::Provider,
    rpc::types::TransactionRequest,
    sol,
    sol_types::SolCall,
};
use anyhow::{Result, anyhow, ensure};

sol! { function getCurrentKmsContextId() external view returns (uint256); }

impl CwzecClient {
    pub async fn redemption_at(&self, id: U256, block: BlockRef) -> Result<super::Redemption> {
        let r = self
            .read_at(redemptionsCall { redeemId: id }, block)
            .await?;
        Ok(super::Redemption {
            user: r.user,
            amount: r.amount,
            status: super::RedemptionStatus::try_from(r.status)?,
            zcash_txid: r.zcashTxid,
        })
    }

    pub async fn check_canonical(&self, block: BlockRef) -> Result<()> {
        ensure!(
            Watcher::new(self).block(block.number).await?.reference == block,
            "reconciliation checkpoint is not canonical"
        );
        Ok(())
    }

    pub(super) async fn read_at<C: SolCall>(&self, call: C, block: BlockRef) -> Result<C::Return> {
        self.check_canonical(block).await?;
        let bytes = self
            .provider
            .call(
                TransactionRequest::default()
                    .to(self.address())
                    .input(call.abi_encode().into()),
            )
            .block(BlockId::hash_canonical(block.hash))
            .await
            .map_err(|_| anyhow!("checkpoint CWZEC read unavailable"))?;
        C::abi_decode_returns(&bytes).map_err(|_| anyhow!("invalid checkpoint CWZEC response"))
    }

    pub async fn consumed_at(&self, deposit_id: B256, block: BlockRef) -> Result<DepositStatus> {
        DepositStatus::try_from(
            self.read_at(
                consumedCall {
                    depositId: deposit_id,
                },
                block,
            )
            .await?,
        )
    }

    pub async fn minted_amount_at(&self, deposit_id: B256, block: BlockRef) -> Result<B256> {
        self.read_at(
            mintedAmountsCall {
                depositId: deposit_id,
            },
            block,
        )
        .await
    }

    pub async fn supply_at(&self, block: BlockRef) -> Result<B256> {
        self.read_at(confidentialTotalSupplyCall {}, block).await
    }

    /// Permit authentication uses the current KMS context, not historical accounting state.
    /// It is independently fetched from the fixed Sepolia verifier, never from Node.
    pub async fn kms_context(&self) -> Result<U256> {
        ensure!(self.chain_id() == 11155111, "unsupported KMS network");
        let bytes = self
            .provider
            .call(
                TransactionRequest::default()
                    .to(address!("bE0E383937d564D7FF0BC3b46c51f0bF8d5C311A"))
                    .input(getCurrentKmsContextIdCall {}.abi_encode().into()),
            )
            .await
            .map_err(|_| anyhow!("KMS context lookup failed"))?;
        let context = getCurrentKmsContextIdCall::abi_decode_returns(&bytes)
            .map_err(|_| anyhow!("invalid KMS context response"))?;
        ensure!(!context.is_zero(), "missing KMS context");
        Ok(context)
    }
}
