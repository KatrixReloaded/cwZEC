use crate::{config::Config, store::Store};
use anyhow::{Context, Result, bail, ensure};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use zcash::{
    core::{Account, LocalSigner, Signer},
    rpc::LightwalletClient,
    watcher::Watcher,
};

pub struct Service {
    pub store: Mutex<Store>,
    pub watcher: Watcher,
    pub signer: Arc<LocalSigner>,
    pub deposit_address: String,
    pub funding_address: String,
    pub admin_key: String,
    pub confirmations: u32,
    pub healthy: AtomicBool,
    pub payment_lock: Arc<Mutex<()>>,
    pub scan_lock: Mutex<()>,
}

impl Service {
    pub async fn open(config: Config) -> Result<Arc<Self>> {
        let mut store = Store::connect(&config.database_url).await?;
        store.migrate().await?;
        let initialized = store.is_initialized().await?;
        let signer = if initialized || config.key_path.exists() {
            LocalSigner::load(&config.key_path)?
        } else {
            LocalSigner::create(&config.key_path)?
        };
        let fvk = signer.full_viewing_key();
        let client = LightwalletClient::connect(&config.endpoint).await?;
        let watcher = Watcher::new(client);
        if initialized {
            let state = store.load().await?;
            ensure!(
                state.account.fvk == fvk.to_bytes() && !state.account.use_qsk,
                "runtime key does not match stored vault identity"
            );
        } else {
            let tip = watcher.client().tip().await?;
            let checkpoint = watcher.initial_checkpoint(tip).await?;
            store
                .initialize(
                    &Account {
                        birthday: tip.checked_add(1).context("height overflow")?,
                        fvk: fvk.to_bytes(),
                        use_qsk: false,
                    },
                    &checkpoint,
                )
                .await?;
        }
        // Key scope addresses are derived in signing code; only public addresses leave it.
        let (deposit_address, funding_address) = signer.addresses()?;
        Ok(Arc::new(Self {
            store: Mutex::new(store),
            watcher,
            signer: Arc::new(signer),
            deposit_address,
            funding_address,
            admin_key: config.admin_key,
            confirmations: config.confirmations,
            healthy: AtomicBool::new(false),
            payment_lock: Arc::new(Mutex::new(())),
            scan_lock: Mutex::new(()),
        }))
    }

    pub async fn scan_once(&self) -> Result<()> {
        self.scan_until(|| false).await.map(|_| ())
    }

    /// Returns false on a requested stop, leaving readiness false and saved progress intact.
    /// Only observe the stop between durable operations; never cancel a database write.
    pub async fn scan_until(&self, stopping: impl Fn() -> bool) -> Result<bool> {
        let _scan = self.scan_lock.lock().await;
        self.healthy.store(false, Ordering::Release);
        if stopping() {
            return Ok(false);
        }
        let tip = self.watcher.client().tip().await.context("scan: tip")?;
        let mut state = self.store.lock().await.load().await.context("scan: load")?;
        eprintln!(
            "wallet scan: saved={} target={tip}",
            state.checkpoint.height
        );
        if state.checkpoint.height > tip
            || self
                .watcher
                .client()
                .block_hash(state.checkpoint.height)
                .await
                .context("scan: checkpoint hash")?
                != state.checkpoint.hash
        {
            let upper = state.checkpoint.height.min(tip);
            if state.checkpoint.height - upper > 100 {
                self.store
                    .lock()
                    .await
                    .halt_bridge("zcash_history_exhausted")
                    .await?;
                bail!("reorg exceeds 100-block recovery limit");
            }
            let mut common = None;
            for depth in 0..=100 {
                if stopping() {
                    return Ok(false);
                }
                let Some(height) = upper.checked_sub(depth) else {
                    break;
                };
                if state.checkpoint.height - height > 100 {
                    break;
                }
                let Some(saved) = self.store.lock().await.checkpoint(height).await? else {
                    break;
                };
                if self.watcher.client().block_hash(height).await? == saved.hash {
                    common = Some(saved);
                    break;
                }
            }
            let Some(common) = common else {
                self.store
                    .lock()
                    .await
                    .halt_bridge("zcash_history_exhausted")
                    .await?;
                bail!("reorg exceeds retained birthday or 100-block recovery limit");
            };
            self.store
                .lock()
                .await
                .rewind(common.height, common.hash)
                .await?;
            state = self.store.lock().await.load().await?;
        }
        while state.checkpoint.height < tip {
            if stopping() {
                return Ok(false);
            }
            let start_height = state.checkpoint.height;
            let started = Instant::now();
            let end = tip.min(
                state
                    .checkpoint
                    .height
                    .saturating_add(zcash::watcher::SCAN_BATCH_BLOCKS),
            );
            let blocks = self
                .watcher
                .fetch_batch(state.checkpoint.height + 1, end)
                .await
                .context("scan: fetch batch")?;
            let mut rpc_time = started.elapsed();
            let mut scan_time = Duration::ZERO;
            let mut store_time = Duration::ZERO;
            for (block, tree) in blocks {
                let scanning = Instant::now();
                let update = self
                    .watcher
                    .scan_prefetched(&state, &block, &tree)
                    .await
                    .context("scan: next block")?;
                scan_time += scanning.elapsed();
                let saving = Instant::now();
                let mut store = self.store.lock().await;
                store.commit_scan(&update).await.context("scan: commit")?;
                state = store.load().await.context("scan: reload")?;
                store_time += saving.elapsed();
            }
            // Prefetch must not make a changed branch eligible for bridge actions.
            let checking = Instant::now();
            ensure!(
                self.watcher.client().block_hash(end).await? == state.checkpoint.hash,
                "scan batch checkpoint changed"
            );
            rpc_time += checking.elapsed();
            if end / 25 != start_height / 25 || end == tip {
                eprintln!(
                    "wallet scan: saved={end} target={tip} blocks={} elapsed_ms={} rpc_ms={} scan_ms={} store_ms={}",
                    end - start_height,
                    started.elapsed().as_millis(),
                    rpc_time.as_millis(),
                    scan_time.as_millis(),
                    store_time.as_millis()
                );
            }
        }
        if stopping() {
            return Ok(false);
        }
        self.healthy.store(true, Ordering::Release);
        Ok(true)
    }
}
