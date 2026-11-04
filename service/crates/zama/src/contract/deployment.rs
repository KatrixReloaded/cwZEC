//! Verify a fresh deployment against the exact locally compiled creation bytes.
use super::{CwzecClient, bindings::*};
use crate::watcher::{BlockRef, Watcher};
use alloy::{
    consensus::Transaction,
    eips::BlockId,
    primitives::{Address, B256, Bytes, U256, keccak256},
    providers::Provider,
};
use anyhow::{Context, Result, ensure};

pub struct DeploymentEvidence {
    pub block: BlockRef,
    pub transaction: B256,
    pub runtime_hash: B256,
    pub service: Address,
    pub auditor: Address,
}
impl CwzecClient {
    pub async fn verify_deployment(
        &self,
        transaction: B256,
        expected_initcode: &Bytes,
        service: Address,
        auditor: Address,
    ) -> Result<DeploymentEvidence> {
        ensure!(
            self.chain_id() == 11155111 && !service.is_zero() && !auditor.is_zero(),
            "testnet deployment identities required"
        );
        let tx = self
            .provider
            .get_transaction_by_hash(transaction)
            .await?
            .context("deployment transaction missing")?;
        let receipt = self
            .provider
            .get_transaction_receipt(transaction)
            .await?
            .context("deployment receipt missing")?;
        ensure!(
            tx.is_create()
                && tx.input() == expected_initcode
                && receipt.status()
                && receipt.contract_address == Some(self.address()),
            "deployment does not match compiled contract and constructor"
        );
        let block = BlockRef {
            number: receipt.block_number.context("deployment not included")?,
            hash: receipt.block_hash.context("deployment block missing")?,
        };
        self.check_canonical(block).await?;
        ensure!(
            Watcher::new(self).latest().await?.reference.number > block.number,
            "deployment needs two confirmations"
        );
        let role = self.read_at(MINTER_ROLECall {}, block).await?;
        ensure!(
            self.read_at(serviceCall {}, block).await? == service
                && self.read_at(auditorCall {}, block).await? == auditor
                && self
                    .read_at(
                        hasRoleCall {
                            role,
                            account: service
                        },
                        block
                    )
                    .await?,
            "deployment permissions mismatch"
        );
        let admin = self.read_at(DEFAULT_ADMIN_ROLECall {}, block).await?;
        ensure!(
            self.read_at(
                hasRoleCall {
                    role: admin,
                    account: receipt.from
                },
                block
            )
            .await?,
            "deployment admin mismatch"
        );
        ensure!(
            self.supply_at(block).await?.is_zero()
                && self.read_at(nextRedeemIdCall {}, block).await? == U256::from(1),
            "deployment not pristine"
        );
        let code = self
            .provider
            .get_code_at(self.address())
            .block_id(BlockId::hash_canonical(block.hash))
            .await?;
        ensure!(!code.is_empty(), "deployed code missing");
        Ok(DeploymentEvidence {
            block,
            transaction,
            runtime_hash: keccak256(code),
            service,
            auditor,
        })
    }

    pub async fn verify_pristine_supply(
        &self,
        block: BlockRef,
        runtime_hash: B256,
        service: Address,
        auditor: Address,
    ) -> Result<()> {
        self.check_canonical(block).await?;
        let code = self
            .provider
            .get_code_at(self.address())
            .block_id(BlockId::hash_canonical(block.hash))
            .await?;
        ensure!(
            keccak256(code) == runtime_hash
                && self.supply_at(block).await?.is_zero()
                && self.read_at(nextRedeemIdCall {}, block).await? == U256::from(1),
            "uninitialized supply lacks pristine contract evidence"
        );
        ensure!(
            self.read_at(serviceCall {}, block).await? == service
                && self.read_at(auditorCall {}, block).await? == auditor,
            "pristine roles changed"
        );
        Ok(())
    }
}
