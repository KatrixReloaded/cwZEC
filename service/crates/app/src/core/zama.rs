//! Application-owned Zama checkpoint, rewind and durable submission policy.
#[cfg(test)]
mod tests;
use crate::store::{Store, TransactionState, ZamaCursor, ZamaTransaction};
use alloy::{primitives::Address, signers::local::PrivateKeySigner};
use anyhow::{Context, Result, ensure};
use zama::{
    contract::{BoundCall, CwzecClient, TransactionFees},
    watcher::{SCAN_BLOCKS, Watcher},
};

pub struct ZamaService {
    store: Store,
    client: CwzecClient,
    watcher: Watcher,
}

impl ZamaService {
    pub async fn open(
        database_url: &str,
        endpoint: &str,
        chain_id: u64,
        contract: Address,
        deployment_block: u64,
    ) -> Result<Self> {
        ensure!(
            deployment_block > 0,
            "explicit nonzero deployment block required"
        );
        let client = CwzecClient::connect(endpoint, chain_id, contract)
            .await
            .context("Zama RPC connection failed")?;
        let watcher = Watcher::new(&client);
        let mut store = Store::connect(database_url)
            .await
            .context("PostgreSQL connection failed")?;
        store.migrate().await?;
        let anchor = watcher.block(deployment_block - 1).await?.reference;
        store
            .initialize_zama(chain_id, contract, deployment_block, anchor)
            .await?;
        Ok(Self {
            store,
            client,
            watcher,
        })
    }
    pub fn client(&self) -> &CwzecClient {
        &self.client
    }
    pub async fn cursor(&self) -> Result<ZamaCursor> {
        self.store
            .zama_cursor()
            .await?
            .ok_or_else(|| anyhow::anyhow!("Zama not initialized"))
    }
    pub async fn latest_height(&self) -> Result<u64> {
        Ok(self.watcher.latest().await?.reference.number)
    }
    pub async fn observation_counts(&self) -> Result<(u64, u64, u64)> {
        self.store.zama_observation_counts().await
    }
    pub async fn transaction(&self, id: &str) -> Result<Option<ZamaTransaction>> {
        self.store.zama_transaction(id).await
    }

    /// At most one bounded range. No bridge action is triggered by these observations.
    pub async fn scan_once(&mut self) -> Result<ZamaCursor> {
        let started = std::time::Instant::now();
        let mut cursor = self.cursor().await?;
        ensure!(
            !cursor.halted,
            "Zama watcher halted; manual investigation required"
        );
        let latest = self.watcher.latest().await?.reference;
        if self
            .watcher
            .optional_block(cursor.tip.number)
            .await?
            .map(|b| b.reference)
            != Some(cursor.tip)
        {
            let mut ancestor = None;
            for old in self.store.zama_history().await? {
                if old.number >= cursor.tip.number || old.number > latest.number {
                    continue;
                }
                if self
                    .watcher
                    .optional_block(old.number)
                    .await?
                    .map(|b| b.reference)
                    == Some(old)
                {
                    ancestor = Some(old);
                    break;
                }
            }
            let Some(ancestor) = ancestor else {
                self.store.halt_zama("history_exhausted").await?;
                anyhow::bail!("reorg exceeds retained history; watcher halted");
            };
            self.store.rewind_zama(cursor.tip, ancestor).await?;
            cursor = self.cursor().await?;
        }
        ensure!(latest.number >= cursor.tip.number, "inconsistent RPC head");
        let end = latest.number.min(
            cursor
                .tip
                .number
                .checked_add(SCAN_BLOCKS)
                .ok_or_else(|| anyhow::anyhow!("height overflow"))?,
        );
        let tracked = self
            .store
            .zama_unfinished()
            .await?
            .iter()
            .map(|t| t.signed.hash())
            .collect::<Vec<_>>();
        let batch = self.watcher.scan(cursor.tip, end, &tracked).await?;
        let observed = started.elapsed();
        let saving = std::time::Instant::now();
        // A reorg racing the fetch leaves the cursor untouched and is retried next poll.
        self.store.apply_zama_scan(&batch).await?;
        let saved = self.cursor().await?;
        eprintln!(
            "evm scan: saved={} target={} blocks={} observe_ms={} store_ms={}",
            saved.tip.number,
            latest.number,
            end - cursor.tip.number,
            observed.as_millis(),
            saving.elapsed().as_millis()
        );
        Ok(saved)
    }

    /// Prepare and commit once. An existing operation resumes its exact signed identity.
    /// Callers must use a new operation ID only for a genuinely new authorized operation.
    pub async fn prepare_operator(
        &mut self,
        id: &str,
        signer: &PrivateKeySigner,
        call: &BoundCall,
        gas_limit: u64,
        max_fee_per_gas: u128,
        max_priority_fee_per_gas: u128,
    ) -> Result<ZamaTransaction> {
        self.prepare_operator_checked(
            id,
            signer,
            call,
            gas_limit,
            max_fee_per_gas,
            max_priority_fee_per_gas,
            None,
        )
        .await
    }

    /// Issued-payout recovery revalidates after EVM RPC and before signing.
    pub async fn prepare_operator_after_death(
        &mut self,
        id: &str,
        signer: &PrivateKeySigner,
        call: &BoundCall,
        gas_limit: u64,
        max_fee_per_gas: u128,
        max_priority_fee_per_gas: u128,
        client: &zcash::rpc::LightwalletClient,
    ) -> Result<ZamaTransaction> {
        self.prepare_operator_checked(
            id,
            signer,
            call,
            gas_limit,
            max_fee_per_gas,
            max_priority_fee_per_gas,
            Some(client),
        )
        .await
    }

    async fn prepare_operator_checked(
        &mut self,
        id: &str,
        signer: &PrivateKeySigner,
        call: &BoundCall,
        gas_limit: u64,
        max_fee_per_gas: u128,
        max_priority_fee_per_gas: u128,
        death_client: Option<&zcash::rpc::LightwalletClient>,
    ) -> Result<ZamaTransaction> {
        ensure!(
            call.user().is_none(),
            "user redemption cannot use operator lane"
        );
        if let Some(saved) = self.store.zama_transaction(id).await? {
            ensure!(
                saved.signed.sender() == signer.address() && saved.signed.matches_call(call),
                "operation identity already bound to different intent"
            );
            return Ok(saved);
        }
        // Preserve the caller's accounting snapshot through signing. New blocks
        // are scanned by recovery before broadcast; a changed ancestry fails now.
        let cursor = self.cursor().await?;
        ensure!(!cursor.halted, "Zama watcher halted");
        ensure!(
            self.latest_height().await? >= cursor.tip.number,
            "inconsistent RPC head"
        );
        self.client.check_canonical(cursor.tip).await?;
        let role = self.client.minter_role().await?;
        ensure!(
            self.client.has_role(role, signer.address()).await?,
            "operator lacks minter role"
        );
        let latest = self.client.latest_nonce(signer.address()).await?;
        let pending = self.client.pending_nonce(signer.address()).await?;
        self.store
            .initialize_zama_lane(signer.address(), latest, pending)
            .await?;
        let nonce = self.store.zama_next_nonce(signer.address()).await?;
        ensure!(
            latest == nonce && pending == nonce,
            "operator nonce differs from durable lane; reconcile first"
        );
        self.check_death_recredit(id, death_client).await?;
        let signed = self.client.domain().sign_operator(
            signer,
            call,
            TransactionFees {
                nonce,
                gas_limit,
                max_fee_per_gas,
                max_priority_fee_per_gas,
            },
        )?;
        self.store.save_zama_signed(id, &signed).await?;
        self.store
            .zama_transaction(id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("signed record missing"))
    }

    /// Reconcile first, then optionally rebroadcast the identical bytes. Missing receipt
    /// never proves failure; every ambiguous result leaves the lane occupied.
    pub async fn recover_operator(&mut self, id: &str) -> Result<ZamaTransaction> {
        self.recover_operator_checked(id, None).await
    }

    pub async fn recover_operator_after_death(
        &mut self,
        id: &str,
        client: &zcash::rpc::LightwalletClient,
    ) -> Result<ZamaTransaction> {
        self.recover_operator_checked(id, Some(client)).await
    }

    async fn recover_operator_checked(
        &mut self,
        id: &str,
        death_client: Option<&zcash::rpc::LightwalletClient>,
    ) -> Result<ZamaTransaction> {
        self.scan_once().await?;
        let saved = self
            .store
            .zama_transaction(id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("unknown signed operation"))?;
        if saved.confirmed
            || matches!(
                saved.state,
                TransactionState::Successful | TransactionState::Reverted
            )
        {
            return Ok(saved);
        }
        if let Some(receipt) = self.watcher.receipt(saved.signed.hash()).await? {
            ensure!(
                receipt.from == saved.signed.sender(),
                "receipt sender mismatch"
            );
            // The bounded scanner will persist this once it reaches the inclusion block.
            return Ok(saved);
        }
        let latest = self.client.latest_nonce(saved.signed.sender()).await?;
        if latest > saved.signed.nonce() {
            self.store
                .mark_zama_submission(saved.signed.hash(), false)
                .await?;
            anyhow::bail!("nonce consumed but receipt unavailable; operation remains unresolved");
        }
        self.check_death_recredit(id, death_client).await?;
        let accepted = self.client.broadcast(&saved.signed).await.is_ok();
        self.store
            .mark_zama_submission(saved.signed.hash(), accepted)
            .await?;
        self.store
            .zama_transaction(id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("signed record missing"))
    }

    async fn check_death_recredit(
        &mut self,
        operation: &str,
        client: Option<&zcash::rpc::LightwalletClient>,
    ) -> Result<()> {
        let Some(id) = operation.strip_prefix("cwzec/recredit/") else {
            return Ok(());
        };
        let id = id.parse()?;
        let Some(d) = self.store.payout_history(id).await? else {
            return Ok(());
        };
        let client = client
            .ok_or_else(|| anyhow::anyhow!("issued recovery requires fresh payout evidence"))?;
        super::payout_death::revalidate_collection(&mut self.store, client, &d).await?;
        self.store
            .recheck_proven_recredit(
                id,
                d.revision,
                d.history
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("history missing"))?,
            )
            .await
    }

    pub async fn events(&self) -> Result<Vec<zama::watcher::EventObservation>> {
        self.store.zama_events().await
    }
}
