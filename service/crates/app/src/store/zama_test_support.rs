//! Disposable real PostgreSQL schemas; all test SQL remains in the store layer.
use super::Store;
use alloy::{
    primitives::{Address, B256, U256},
    signers::local::PrivateKeySigner,
};
use anyhow::Result;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio_postgres::{Client, NoTls};
use zama::{
    contract::{SignedTransaction, TransactionDomain, TransactionFees},
    watcher::{Block, BlockRef, ScanBatch},
};

pub(crate) struct TestDb {
    config: tokio_postgres::Config,
    admin: Option<Client>,
    schema: String,
    url: String,
}
impl TestDb {
    pub(crate) async fn new() -> Result<Self> {
        Self::create(false).await
    }
    pub(crate) async fn legacy() -> Result<Self> {
        Self::create(true).await
    }
    async fn create(legacy: bool) -> Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let url = std::env::var("DATABASE_URL")?;
        let config: tokio_postgres::Config = url.parse()?;
        let (admin, connection) = config.connect(NoTls).await?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        let schema = format!(
            "zama_test_{}_{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        admin
            .batch_execute(&format!("CREATE SCHEMA {schema}"))
            .await?;
        let db = Self {
            config,
            admin: Some(admin),
            schema,
            url,
        };
        let mut store = db.connect().await?;
        if legacy {
            store
                .client
                .batch_execute(include_str!("../../migrations/002_zama.sql"))
                .await?;
        } else {
            store.migrate().await?;
        }
        Ok(db)
    }
    pub(crate) fn url(&self) -> String {
        format!(
            "{}{}options=-csearch_path%3D{}",
            self.url,
            if self.url.contains('?') { "&" } else { "?" },
            self.schema
        )
    }
    pub(crate) async fn connect(&self) -> Result<Store> {
        let mut config = self.config.clone();
        config.options(&format!("-c search_path={}", self.schema));
        let (client, connection) = config.connect(NoTls).await?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        Ok(Store { client })
    }
    pub(crate) async fn cleanup(mut self) -> Result<()> {
        self.admin
            .as_ref()
            .unwrap()
            .batch_execute(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .await?;
        self.admin.take();
        Ok(())
    }
}
impl Drop for TestDb {
    fn drop(&mut self) {
        if let Some(admin) = self.admin.take() {
            let sql = format!("DROP SCHEMA {} CASCADE", self.schema);
            tokio::spawn(async move {
                let _ = admin.batch_execute(&sql).await;
            });
        }
    }
}
pub(crate) fn contract() -> Address {
    Address::repeat_byte(42)
}
pub(crate) fn hash(height: u64, branch: u64) -> B256 {
    B256::from(U256::from(height + 1 + branch * 100_000))
}
pub(crate) fn reference(height: u64, branch: u64) -> BlockRef {
    BlockRef {
        number: height,
        hash: hash(height, branch),
    }
}
pub(crate) fn signed(nonce: u64, id: u64) -> SignedTransaction {
    let signer: PrivateKeySigner =
        "0000000000000000000000000000000000000000000000000000000000000001"
            .parse()
            .unwrap();
    let domain = TransactionDomain::new(1, contract()).unwrap();
    domain
        .sign_operator(
            &signer,
            &domain.void_deposit(hash(id, 0)),
            TransactionFees {
                nonce,
                gas_limit: 100_000,
                max_fee_per_gas: 2,
                max_priority_fee_per_gas: 1,
            },
        )
        .unwrap()
}
pub(crate) async fn initialized(db: &TestDb) -> Result<Store> {
    let mut store = db.connect().await?;
    store
        .initialize_zama(1, contract(), 2, reference(1, 0))
        .await?;
    Ok(store)
}
pub(crate) fn batch(previous: BlockRef, end: u64, branch: u64) -> ScanBatch {
    ScanBatch {
        chain_id: 1,
        contract: contract(),
        previous,
        end: reference(end, branch),
        blocks: ((previous.number + 1)..=end)
            .map(|n| Block {
                reference: reference(n, branch),
                parent_hash: if n == previous.number + 1 {
                    previous.hash
                } else {
                    hash(n - 1, branch)
                },
            })
            .collect(),
        events: vec![],
        receipts: vec![],
    }
}
pub(crate) async fn halt_reason(store: &Store) -> Result<Option<String>> {
    Ok(store
        .client
        .query_one("SELECT halt_reason FROM zama_chain", &[])
        .await?
        .get(0))
}
pub(crate) async fn fact_counts(store: &Store) -> Result<(i64, i64, i64)> {
    let row=store.client.query_one("SELECT (SELECT count(*) FROM zama_events),(SELECT count(*) FROM zama_receipts),(SELECT count(*) FROM zama_transactions)",&[]).await?;
    Ok((row.get(0), row.get(1), row.get(2)))
}
