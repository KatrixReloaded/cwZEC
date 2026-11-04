//! Real executable startup boundaries, with disposable SQL state and loopback peers.
//! Run explicitly with DATABASE_URL pointing at a disposable PostgreSQL database:
//! `cargo test -p app --test startup`
//! Synthetic origins bypass deployment verification for local lifecycle coverage.
//! No consensus-valid transaction, funded payment, or live deployment is involved.
use alloy::{
    primitives::{Address, B256, U256, keccak256},
    signers::local::PrivateKeySigner,
};
use anyhow::{Context, Result, ensure};
use app::{core::destination::DestinationKey, store::Store};
use axum::{Json, Router, routing::post};
use serde_json::{Value, json};
use std::{
    fs::OpenOptions,
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    process::{Child, Command, Output, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio_postgres::{Client, NoTls};

// Reuse the strict unary gRPC fixture (unexpected transaction RPCs panic).
#[allow(dead_code)]
#[path = "../src/store/payout_death_test_support.rs"]
mod peers;
#[path = "startup/mod.rs"]
mod startup;
// The reused helper's optional EVM helper refers to this block constructor.
mod zama_test_support {
    pub(crate) fn reference(height: u64, branch: u64) -> zama::watcher::BlockRef {
        zama::watcher::BlockRef {
            number: height,
            hash: alloy::primitives::B256::from(alloy::primitives::U256::from(
                height + 1 + branch * 100_000,
            )),
        }
    }
}

struct TestDb {
    admin: Client,
    schema: String,
    url: String,
}
impl TestDb {
    async fn new() -> Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let url = std::env::var("DATABASE_URL").context("disposable DATABASE_URL required")?;
        ensure!(
            url.starts_with("postgres://") || url.starts_with("postgresql://"),
            "DATABASE_URL must be a PostgreSQL URL"
        );
        let config: tokio_postgres::Config =
            url.parse().context("invalid test database configuration")?;
        // Do not accidentally override an existing search_path or other URL options.
        ensure!(
            config.get_options().is_none(),
            "test DATABASE_URL must omit options"
        );
        let (admin, connection) = config
            .connect(NoTls)
            .await
            .context("test database unavailable")?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        let schema = format!(
            "startup_test_{}_{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        admin
            .batch_execute(&format!("CREATE SCHEMA {schema}"))
            .await?;
        let scoped = format!(
            "{url}{}options=-csearch_path%3D{schema}",
            if url.contains('?') { '&' } else { '?' }
        );
        Ok(Self {
            admin,
            schema,
            url: scoped,
        })
    }
    async fn client(&self) -> Result<Client> {
        let (client, connection) = tokio_postgres::connect(&self.url, NoTls).await?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        Ok(client)
    }
    async fn cleanup(self) -> Result<()> {
        self.admin
            .batch_execute(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .await?;
        Ok(())
    }
}

struct Keys {
    dir: PathBuf,
    service: Address,
    auditor: Address,
}
impl Keys {
    fn new() -> Result<Self> {
        let dir = std::env::temp_dir().join(format!(
            "bridge-startup-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        std::fs::create_dir(&dir)?;
        let mut addresses = vec![];
        for name in ["service", "auditor"] {
            let mut bytes = [0; 32];
            rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut bytes);
            let signer =
                PrivateKeySigner::from_slice(&bytes).context("generate ephemeral signer")?;
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(dir.join(name))?
                .write_all(&bytes)?;
            zeroize::Zeroize::zeroize(&mut bytes);
            addresses.push(signer.address());
        }
        DestinationKey::create(&dir.join("destination"))?;
        Ok(Self {
            dir,
            service: addresses[0],
            auditor: addresses[1],
        })
    }
    fn command(&self, db: &TestDb, grpc: &str, evm: &str, sidecar: &str, port: u16) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_bridge-auto"));
        // No inherited credentials, endpoints, proxies or signing configuration.
        cmd.env_clear()
            .env(
                "DATABASE_URL",
                format!("{}&application_name={}", db.url, db.schema),
            )
            .env(
                "ADMIN_API_KEY",
                "startup-only-unfunded-test-admin-credential",
            )
            .env("CONFIRMATIONS", "3")
            .env("ZCASH_LIGHTWALLETD_URL", grpc)
            .env("VAULT_KEY_PATH", self.dir.join("vault"))
            .env("ZAMA_RPC_URL", evm)
            .env("ZAMA_CHAIN_ID", "11155111")
            .env("ZAMA_CONTRACT", Address::repeat_byte(42).to_string())
            .env("ZAMA_DEPLOYMENT_BLOCK", "1")
            .env("BRIDGE_SERVICE_KEY_PATH", self.dir.join("service"))
            .env("BRIDGE_AUDITOR_KEY_PATH", self.dir.join("auditor"))
            .env("BRIDGE_DESTINATION_KEY_PATH", self.dir.join("destination"))
            .env("SIDECAR_URL", sidecar)
            .env("BRIDGE_HTTP_PORT", port.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }
}
impl Drop for Keys {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
async fn fails(mut command: Command, stage: &str) -> Result<Output> {
    stopped(&mut command, stage, false).await
}
async fn stopped(command: &mut Command, stage: &str, listening: bool) -> Result<Output> {
    let mut child = Process(command.spawn().context("spawn bridge-auto")?);
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.0.try_wait()? {
            break status;
        }
        ensure!(
            Instant::now() < deadline,
            "bridge-auto exceeded startup deadline"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let mut stdout = vec![];
    let mut stderr = vec![];
    child.0.stdout.take().unwrap().read_to_end(&mut stdout)?;
    child.0.stderr.take().unwrap().read_to_end(&mut stderr)?;
    ensure!(
        !status.success(),
        "bridge-auto unexpectedly accepted invalid startup"
    );
    let diagnostic = String::from_utf8_lossy(&stderr);
    // Never echo child diagnostics or credentials on assertion failure.
    ensure!(
        diagnostic.contains(&format!("Bridge stopped during {stage};")),
        "bridge-auto failed at an unexpected startup boundary"
    );
    ensure!(
        diagnostic.contains("HTTP listening") == listening
            && !String::from_utf8_lossy(&stdout).contains("bridge pass reconciled"),
        "failed startup exposed a coordinator"
    );
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

async fn evm(
    service: Address,
    auditor: Address,
) -> Result<(peers::HttpServer, Arc<Mutex<Vec<String>>>)> {
    let unexpected = Arc::new(Mutex::new(vec![]));
    let captured = unexpected.clone();
    let router = Router::new().route("/", post(move |Json(v): Json<Value>| {
        let unexpected = captured.clone();
        async move {
            let method = v["method"].as_str().unwrap_or("malformed");
            let result = match method {
                "eth_chainId" => Some(json!("0xaa36a7")),
                "eth_getCode" => Some(json!("0x6000")),
                "eth_call" => {
                    let input = v["params"][0]["input"].as_str().or_else(|| v["params"][0]["data"].as_str()).unwrap_or("");
                    let word = if input.starts_with(&selector("decimals()")) { Some(U256::from(8)) }
                        else if input.starts_with(&selector("service()")) { Some(U256::from_be_slice(service.as_slice())) }
                        else if input.starts_with(&selector("auditor()")) { Some(U256::from_be_slice(auditor.as_slice())) }
                        else if input.starts_with(&selector("MINTER_ROLE()")) || input.starts_with(&selector("hasRole(bytes32,address)")) { Some(U256::from(1)) }
                        else { None };
                    word.map(|w| json!(format!("0x{w:064x}")))
                }
                "eth_getBlockByNumber" if v["params"][0] == "0x0" => Some(json!({"number":"0x0", "hash":B256::from(U256::from(1)), "parentHash":B256::ZERO})),
                // Explicit dependency interruption after HTTP binds, before any
                // accounting/action pass. No successful chain scan is invented.
                "eth_getBlockByNumber" if v["params"][0] == "latest" => return Json(json!({"jsonrpc":"2.0", "id":v["id"], "error":{"code":-32000,"message":"controlled header outage"}})),
                _ => None,
            };
            if let Some(result) = result { Json(json!({"jsonrpc":"2.0", "id":v["id"], "result":result})) }
            else {
                // Includes all signing/broadcast RPCs. Capture method only, never payload.
                unexpected.lock().unwrap().push(method.to_string());
                Json(json!({"jsonrpc":"2.0", "id":v["id"], "error":{"code":-32601,"message":"startup fixture forbids method"}}))
            }
        }
    }));
    Ok((peers::HttpServer::start(router).await?, unexpected))
}
fn selector(signature: &str) -> String {
    format!("0x{}", hex::encode(&keccak256(signature.as_bytes())[..4]))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_auto_rejects_unsafe_startup_and_preserves_durable_identity() -> Result<()> {
    let db = TestDb::new().await?;
    let result = startup_cases(&db).await;
    // Always attempt schema cleanup, including on failed boundary assertions.
    let cleanup = db.cleanup().await;
    result?;
    cleanup?;
    // Keep process scenarios sequential: the coordinator lease is database-wide,
    // even though each scenario isolates its application data in a fresh schema.
    let db = TestDb::new().await?;
    let result = tokio::time::timeout(Duration::from_secs(90), startup::lifecycle(&db))
        .await
        .context("coordinator lifecycle exceeded 90 seconds")
        .and_then(|result| result);
    let cleanup = db.cleanup().await;
    result?;
    cleanup?;
    let db = TestDb::new().await?;
    let result = tokio::time::timeout(Duration::from_secs(60), startup::catchup_shutdown(&db))
        .await
        .context("catch-up shutdown exceeded 60 seconds")
        .and_then(|result| result);
    let cleanup = db.cleanup().await;
    result?;
    cleanup
}

async fn startup_cases(db: &TestDb) -> Result<()> {
    let keys = Keys::new()?;
    let grpc = peers::serve(peers::data(zcash::rpc::history::LOWER_HEIGHT)).await;
    let (evm, unexpected) = evm(keys.service, keys.auditor).await?;
    let sidecar_calls = Arc::new(AtomicU64::new(0));
    let calls = sidecar_calls.clone();
    let sidecar = peers::HttpServer::start(Router::new().fallback(move || {
        let calls = calls.clone();
        async move {
            calls.fetch_add(1, Ordering::Relaxed);
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        }
    }))
    .await?;
    let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = occupied.local_addr()?.port();
    let command = || keys.command(db, &grpc.endpoint, &evm.url, &sidecar.url, port);

    let mut invalid = command();
    invalid.env("ADMIN_API_KEY", "short");
    fails(invalid, "configuration").await?;
    let sql = db.client().await?;
    ensure!(
        sql.query_one("SELECT to_regclass('account') IS NULL", &[])
            .await?
            .get::<_, bool>(0),
        "invalid authentication mutated empty database"
    );
    ensure!(
        !keys.dir.join("vault").exists(),
        "invalid authentication created a vault"
    );

    // Empty DB must migrate and initialize exactly once, then reject absent origin.
    fails(command(), "scanner and reconciler startup").await?;
    let original: (Vec<u8>, i64) = {
        let row = sql
            .query_one("SELECT fvk,birthday FROM account", &[])
            .await?;
        (row.get(0), row.get(1))
    };
    ensure!(
        sql.query_one("SELECT count(*) FROM bridge_origin", &[])
            .await?
            .get::<_, i64>(0)
            == 0,
        "startup invented deployment evidence"
    );
    fails(command(), "scanner and reconciler startup").await?;
    let row = sql
        .query_one("SELECT fvk,birthday FROM account", &[])
        .await?;
    ensure!(
        row.get::<_, Vec<u8>>(0) == original.0 && row.get::<_, i64>(1) == original.1,
        "restart changed wallet identity"
    );
    ensure!(
        sql.query_one("SELECT count(*) FROM checkpoints", &[])
            .await?
            .get::<_, i64>(0)
            == 1,
        "restart changed wallet checkpoints"
    );

    let wrong = keys.dir.join("wrong-vault");
    zcash::core::LocalSigner::create(&wrong)?;
    let mut wrong_key = command();
    wrong_key.env("VAULT_KEY_PATH", wrong);
    fails(wrong_key, "wallet startup").await?;
    ensure!(
        sql.query_one("SELECT fvk FROM account", &[])
            .await?
            .get::<_, Vec<u8>>(0)
            == original.0,
        "wrong key changed durable identity"
    );

    // Hold the same public lease used by bridge-auto; another executable must exit
    // before even opening the wallet. PostgreSQL advisory locks are database-wide.
    let lease = Store::connect(&db.url).await?;
    lease.claim_coordinator().await?;
    fails(command(), "configuration").await?;
    drop(lease);
    // Closing Store closes the lease asynchronously; observe release explicitly.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let acquired: bool = sql
            .query_one("SELECT pg_try_advisory_lock(1668774501,1)", &[])
            .await?
            .get(0);
        if acquired {
            sql.query_one("SELECT pg_advisory_unlock(1668774501,1)", &[])
                .await?;
            break;
        }
        ensure!(
            Instant::now() < deadline,
            "test coordinator lease did not release"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // Synthetic local-only evidence reaches the HTTP bind boundary. No deployment,
    // accounting, readiness, signing or transaction acceptance is claimed.
    sql.execute("INSERT INTO bridge_origin(singleton,deployment_number,deployment_hash,deployment_tx,runtime_hash,service,auditor) VALUES(true,1,$1,$2,$3,$4,$5)", &[&[1u8;32].as_slice(), &[2u8;32].as_slice(), &[3u8;32].as_slice(), &keys.service.as_slice(), &keys.auditor.as_slice()]).await?;
    fails(command(), "scanner and reconciler startup").await?;
    // Paired positive bind control: identical startup inputs, only the occupied
    // socket changes. The process must now publish HTTP and reach the EVM outage.
    drop(occupied);
    stopped(&mut command(), "EVM scan", true).await?;
    // Repeat against the durable state after the dependency failure: the same
    // outage must remain fail-closed, with neither accounting nor broadcasts.
    stopped(&mut command(), "EVM scan", true).await?;
    ensure!(
        sql.query_one("SELECT count(*) FROM bridge_checkpoints", &[])
            .await?
            .get::<_, i64>(0)
            == 0,
        "bind failure performed accounting"
    );
    ensure!(
        sql.query_one(
            "SELECT (SELECT count(*) FROM zama_transactions) + (SELECT count(*) FROM operations)",
            &[]
        )
        .await?
        .get::<_, i64>(0)
            == 0,
        "startup or dependency failure created a transaction journal"
    );
    ensure!(
        unexpected.lock().unwrap().is_empty(),
        "startup attempted an unapproved EVM RPC"
    );
    ensure!(
        sidecar_calls.load(Ordering::Relaxed) == 0,
        "startup contacted the FHE sidecar"
    );
    Ok(())
}
