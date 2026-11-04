//! Unfunded coordinator lifecycle, using only disposable SQL and loopback peers.
use super::*;
use alloy::{
    dyn_abi::TypedData,
    primitives::{Bytes, Signature},
    transports::http::reqwest,
};
use incrementalmerkletree::{frontier::CommitmentTree, witness::IncrementalWitness};
use orchard::{
    Note, NoteVersion,
    keys::Scope,
    note::{RandomSeed, Rho},
    tree::MerkleHashOrchard,
    value::NoteValue,
};
use std::sync::atomic::AtomicBool;
use zcash::{
    core::{Account, Checkpoint, LocalSigner, NoteId, ScanUpdate, Signer, WalletNote},
    rpc::history::LOWER_HEIGHT,
};
use zcash_primitives::merkle_tree::{write_commitment_tree, write_incremental_witness};

mod shutdown;
pub(super) use shutdown::catchup_shutdown;

#[derive(Default)]
struct Facts {
    head: AtomicU64,
    outage: AtomicBool,
    sidecar_outage: AtomicBool,
    decrypts: AtomicU64,
    failures: AtomicU64,
    hold: AtomicBool,
    entered: AtomicBool,
    unexpected: Mutex<Vec<String>>,
    permit: Mutex<Option<Value>>,
}
fn block(height: u64) -> Value {
    json!({"number":format!("{height:#x}"),"hash":B256::from(U256::from(height+1)),"parentHash":if height==0 {B256::ZERO} else {B256::from(U256::from(height))}})
}
async fn rpc(keys: &Keys, facts: Arc<Facts>) -> Result<peers::HttpServer> {
    let (service, auditor) = (keys.service, keys.auditor);
    peers::HttpServer::start(Router::new().route("/", post(move |Json(v): Json<Value>| {
        let f = facts.clone();
        async move {
            let method = v["method"].as_str().unwrap_or("malformed");
            let result = match method {
                "eth_chainId" => Some(json!("0xaa36a7")),
                "eth_getCode" => Some(json!("0x6000")),
                "eth_getLogs" => Some(json!([])),
                "eth_getBlockByNumber" => {
                    if v["params"][0] == "latest" && f.outage.load(Ordering::SeqCst) {
                        return Json(json!({"jsonrpc":"2.0","id":v["id"],"error":{"code":-32000,"message":"private-fixture-outage"}}));
                    }
                    let tag = v["params"][0].as_str().unwrap();
                    let h = if tag == "latest" { f.head.load(Ordering::SeqCst) } else { u64::from_str_radix(tag.trim_start_matches("0x"),16).unwrap() };
                    Some(block(h))
                }
                "eth_call" => {
                    let input = v["params"][0]["input"].as_str().or_else(||v["params"][0]["data"].as_str()).unwrap_or("");
                    let value = if input.starts_with(&selector("decimals()")) {Some(U256::from(8))}
                        else if input.starts_with(&selector("service()")) {Some(U256::from_be_slice(service.as_slice()))}
                        else if input.starts_with(&selector("auditor()")) {Some(U256::from_be_slice(auditor.as_slice()))}
                        else if ["MINTER_ROLE()", "hasRole(bytes32,address)", "nextRedeemId()"].iter().any(|s|input.starts_with(&selector(s))) {Some(U256::from(1))}
                        else if input.starts_with(&selector("getCurrentKmsContextId()")) {Some(U256::from(7))}
                        else if input.starts_with(&selector("confidentialTotalSupply()")) {Some(U256::from_be_slice(B256::repeat_byte(45).as_slice()))}
                        else {None};
                    value.map(|w|json!(format!("0x{w:064x}")))
                }
                _ => None,
            };
            match result {
                Some(result) => Json(json!({"jsonrpc":"2.0","id":v["id"],"result":result})),
                None => {f.unexpected.lock().unwrap().push(method.to_owned()); Json(json!({"jsonrpc":"2.0","id":v["id"],"error":{"code":-32601,"message":"forbidden fixture RPC"}}))}
            }
        }
    }))).await
}
fn permit(s: &Value) -> Value {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let start = (now / 1000).to_string();
    let mut extra = vec![1];
    extra.extend_from_slice(&U256::from(7).to_be_bytes::<32>());
    let typed = json!({"domain":{"name":"Decryption","version":"1","chainId":"11155111","verifyingContract":"0x5D8BD78e2ea6bbE41f26dFe9fdaEAa349e077478"},"primaryType":"UserDecryptRequestVerification","types":{"EIP712Domain":[{"name":"name","type":"string"},{"name":"version","type":"string"},{"name":"chainId","type":"uint256"},{"name":"verifyingContract","type":"address"}],"UserDecryptRequestVerification":[{"name":"publicKey","type":"bytes"},{"name":"contractAddresses","type":"address[]"},{"name":"startTimestamp","type":"uint256"},{"name":"durationDays","type":"uint256"},{"name":"extraData","type":"bytes"}]},"message":{"publicKey":"0xabcd","contractAddresses":[s["contract"]],"startTimestamp":start,"durationDays":"1","extraData":Bytes::from(extra)}});
    let digest = serde_json::from_value::<TypedData>(typed.clone())
        .unwrap()
        .eip712_signing_hash()
        .unwrap();
    json!({"sessionId":"ab".repeat(32),"contract":s["contract"],"user":s["user"],"handle":s["handle"],"expiresAt":(now+290000).to_string(),"publicKey":"0xabcd","startTimestamp":start,"typedData":typed,"digest":digest})
}
async fn sidecar(facts: Arc<Facts>) -> Result<peers::HttpServer> {
    peers::HttpServer::start(Router::new().route(
        "/{path}",
        post(
            move |axum::extract::Path(path): axum::extract::Path<String>, Json(v): Json<Value>| {
                let f = facts.clone();
                async move {
                    use axum::response::IntoResponse;
                    if f.sidecar_outage.load(Ordering::SeqCst) {
                        f.failures.fetch_add(1, Ordering::SeqCst);
                        return (
                            axum::http::StatusCode::SERVICE_UNAVAILABLE,
                            Json(json!({"error":{"code":"fixture_unavailable"}})),
                        )
                            .into_response();
                    }
                    match path.as_str() {
                        "decrypt-request" => {
                            f.entered.store(true, Ordering::SeqCst);
                            while f.hold.load(Ordering::SeqCst) {
                                tokio::time::sleep(Duration::from_millis(10)).await;
                            }
                            let p = permit(&v);
                            *f.permit.lock().unwrap() = Some(p.clone());
                            Json(p).into_response()
                        }
                        "decrypt" => {
                            let p = f.permit.lock().unwrap().clone().unwrap();
                            for key in ["contract", "user", "handle", "sessionId"] {
                                assert_eq!(v[key], p[key]);
                            }
                            let sig: Signature = v["signature"].as_str().unwrap().parse().unwrap();
                            let digest = serde_json::from_value(p["digest"].clone()).unwrap();
                            assert_eq!(
                                json!(sig.recover_address_from_prehash(&digest).unwrap()),
                                v["user"]
                            );
                            f.decrypts.fetch_add(1, Ordering::SeqCst);
                            Json(json!({"value":"0"})).into_response()
                        }
                        _ => {
                            f.unexpected.lock().unwrap().push(path);
                            axum::http::StatusCode::NOT_FOUND.into_response()
                        }
                    }
                }
            },
        ),
    ))
    .await
}

async fn seed(db: &TestDb, keys: &Keys) -> Result<peers::Data> {
    let signer = LocalSigner::create(&keys.dir.join("vault"))?;
    let fvk = signer.full_viewing_key();
    let rho = Rho::from_bytes(&[0; 32]).unwrap();
    let seed = RandomSeed::from_bytes([7; 32], &rho).unwrap();
    let note = Note::from_parts(
        fvk.address_at(0_u32, Scope::Internal),
        NoteValue::from_raw(20_000),
        rho,
        seed,
        NoteVersion::V3,
    )
    .unwrap();
    let mut tree = CommitmentTree::<MerkleHashOrchard, 32>::empty();
    let mut empty = vec![];
    write_commitment_tree(&tree, &mut empty)?;
    tree.append(MerkleHashOrchard::from_cmx(&note.commitment().into()))
        .unwrap();
    let mut frontier = vec![];
    write_commitment_tree(&tree, &mut frontier)?;
    let mut witness = vec![];
    write_incremental_witness(&IncrementalWitness::from_tree(tree).unwrap(), &mut witness)?;
    let mut store = Store::connect(&db.url).await?;
    store.migrate().await?;
    store
        .initialize(
            &Account {
                birthday: LOWER_HEIGHT,
                fvk: fvk.to_bytes(),
                use_qsk: false,
            },
            &Checkpoint {
                height: LOWER_HEIGHT - 1,
                hash: peers::hash(LOWER_HEIGHT - 1),
                parent: peers::hash(LOWER_HEIGHT - 2),
                frontier: empty,
            },
        )
        .await?;
    store
        .commit_scan(&ScanUpdate {
            checkpoint: Checkpoint {
                height: LOWER_HEIGHT,
                hash: peers::hash(LOWER_HEIGHT),
                parent: peers::hash(LOWER_HEIGHT - 1),
                frontier: frontier.clone(),
            },
            received: vec![WalletNote {
                id: NoteId {
                    txid: [9; 32],
                    action_index: 0,
                },
                recipient: note.recipient().to_raw_address_bytes(),
                value: 20_000,
                rho: note.rho().to_bytes(),
                rseed: *note.rseed().as_bytes(),
                nullifier: note.nullifier(&fvk).to_bytes(),
                position: 0,
                internal: true,
                memo: [0; 512],
                mined_height: LOWER_HEIGHT,
                spent_height: None,
                witness,
            }],
            witnesses: vec![],
            spent_nullifiers: vec![],
            txids: vec![],
        })
        .await?;
    let mut data = peers::data(LOWER_HEIGHT + 4);
    data.tip = LOWER_HEIGHT + 3;
    for height in LOWER_HEIGHT..=LOWER_HEIGHT + 4 {
        data.trees.get_mut(&height).unwrap().ironwood_tree = hex::encode(&frontier);
        data.blocks
            .get_mut(&height)
            .unwrap()
            .chain_metadata
            .as_mut()
            .unwrap()
            .ironwood_commitment_tree_size = 1;
    }
    Ok(data)
}
async fn wait_until(mut condition: impl AsyncFnMut() -> Result<bool>, label: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if condition().await? {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "coordinator lifecycle timed out: {label}"
        );
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}
async fn exit(child: &mut Process, success: bool) -> Result<()> {
    wait_until(async || Ok(child.0.try_wait()?.is_some()), "process exit").await?;
    ensure!(
        child.0.try_wait()?.unwrap().success() == success,
        "unexpected coordinator exit status"
    );
    Ok(())
}
fn signal(child: &Process, signal: &str) -> Result<()> {
    ensure!(
        Command::new("/bin/kill")
            .args([signal, &child.0.id().to_string()])
            .status()?
            .success(),
        "signal owned child"
    );
    Ok(())
}
async fn lease(sql: &Client) -> Result<bool> {
    Ok(sql.query_one("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE locktype='advisory' AND classid=1668774501 AND objid=1 AND database=(SELECT oid FROM pg_database WHERE datname=current_database()) AND granted)",&[]).await?.get(0))
}
async fn ready(http: &reqwest::Client, url: &str) -> Result<bool> {
    let Ok(response) = http.get(format!("{url}/v1/info")).send().await else {
        return Ok(false);
    };
    if !response.status().is_success() {
        return Ok(false);
    }
    let v: Value = response.json().await?;
    // Public projection has only nonsecret wallet/address metadata and readiness.
    ensure!(
        v.get("report").is_none() && v.get("deposits").is_none() && v.get("recovery").is_none(),
        "private accounting leaked through public info"
    );
    ensure!(
        !v.to_string().contains("private-fixture-outage"),
        "private diagnostic leaked"
    );
    Ok(v["scanner_ready"] == true)
}
pub(super) async fn lifecycle(db: &TestDb) -> Result<()> {
    let keys = Keys::new()?;
    let facts = Arc::new(Facts::default());
    facts.head.store(3, Ordering::SeqCst);
    let grpc = peers::serve(seed(db, &keys).await?).await;
    let evm = rpc(&keys, facts.clone()).await?;
    let sidecar = sidecar(facts.clone()).await?;
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = socket.local_addr()?.port();
    drop(socket);
    let url = format!("http://127.0.0.1:{port}");
    let command = || keys.command(db, &grpc.endpoint, &evm.url, &sidecar.url, port);
    // Startup establishes chain/domain before the synthetic local origin is inserted.
    fails(command(), "scanner and reconciler startup").await?;
    let sql = db.client().await?;
    sql.execute("INSERT INTO bridge_origin(singleton,deployment_number,deployment_hash,deployment_tx,runtime_hash,service,auditor) VALUES(true,1,$1,$2,$3,$4,$5)",&[&B256::from(U256::from(2)).as_slice(),&[2u8;32].as_slice(),&keccak256([0x60,0]).as_slice(),&keys.service.as_slice(),&keys.auditor.as_slice()]).await?;
    let http = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()?;

    // Signal while an accounting RPC is outstanding: the same pass drains, with
    // its exclusive lease retained, then exits successfully without another pass.
    facts.hold.store(true, Ordering::SeqCst);
    let mut child = Process(command().spawn()?);
    wait_until(
        async || Ok(facts.entered.load(Ordering::SeqCst)),
        "held accounting pass",
    )
    .await?;
    signal(&child, "-TERM")?;
    wait_until(
        async || Ok(http.get(format!("{url}/v1/info")).send().await.is_err()),
        "signal stops accepting HTTP requests",
    )
    .await?;
    ensure!(
        child.0.try_wait()?.is_none() && lease(&sql).await?,
        "signal abandoned in-flight pass or coordinator lease"
    );
    facts.hold.store(false, Ordering::SeqCst);
    exit(&mut child, true).await?;
    wait_until(async || Ok(!lease(&sql).await?), "graceful lease release").await?;
    let count = facts.decrypts.load(Ordering::SeqCst);
    ensure!(count == 1, "shutdown started another accounting pass");

    let mut child = Process(command().spawn()?);
    wait_until(async || ready(&http, &url).await, "initial readiness").await?;
    let public = http
        .get(format!("{url}/v1/info"))
        .send()
        .await?
        .text()
        .await?;
    let fvk: Vec<u8> = sql.query_one("SELECT fvk FROM account", &[]).await?.get(0);
    ensure!(
        !public.contains(&hex::encode(fvk))
            && !public.contains(&db.schema)
            && !public.contains("startup-only-unfunded-test-admin-credential"),
        "public info exposed private state"
    );
    let denied = http.get(format!("{url}/v1/admin/reconcile")).send().await?;
    ensure!(
        denied.status() == reqwest::StatusCode::UNAUTHORIZED
            && denied.json::<Value>().await? == json!({"error":"unauthorized"}),
        "anonymous accounting exposed private state"
    );
    let report: Value = http
        .get(format!("{url}/v1/admin/reconcile"))
        .header("x-api-key", "startup-only-unfunded-test-admin-credential")
        .send()
        .await?
        .json()
        .await?;
    ensure!(
        report["report"]["fee_reserve_zat"] == "20000"
            && report["report"]["coverage_complete"] == true
            && report["report"]["supply_zat"] == "0",
        "readiness lacks controlled accounting evidence"
    );
    let original: i64 = sql
        .query_one("SELECT max(id) FROM bridge_checkpoints", &[])
        .await?
        .get(0);
    // Hold the next routine accounting refresh after scanners advance. A valid
    // prior report keeps the healthy coordinator available while it refreshes.
    facts.entered.store(false, Ordering::SeqCst);
    facts.hold.store(true, Ordering::SeqCst);
    facts.head.store(4, Ordering::SeqCst);
    grpc.state.lock().unwrap().tip = LOWER_HEIGHT + 4;
    wait_until(
        async || Ok(facts.entered.load(Ordering::SeqCst)),
        "routine accounting refresh",
    )
    .await?;
    let refreshing: Value = http
        .get(format!("{url}/v1/info"))
        .send()
        .await?
        .json()
        .await?;
    ensure!(
        refreshing["scanner_ready"] == true
            && refreshing["persisted"]["report_valid"] == true
            && refreshing["persisted"]["report_current"] == false,
        "routine refresh made healthy bridge unavailable"
    );
    facts.hold.store(false, Ordering::SeqCst);
    wait_until(async || {
        let row=sql.query_one("SELECT zcash_height,zama_number,id FROM bridge_checkpoints ORDER BY id DESC LIMIT 1",&[]).await?;
        Ok(row.get::<_,i64>(0)==i64::from(LOWER_HEIGHT+2) && row.get::<_,i64>(1)==3 && row.get::<_,i64>(2)>original && ready(&http,&url).await?)
    },"scanner advance and report refresh").await?;
    facts.sidecar_outage.store(true, Ordering::SeqCst);
    wait_until(
        async || Ok(facts.failures.load(Ordering::SeqCst) > 0 && !ready(&http, &url).await?),
        "sidecar outage clears readiness",
    )
    .await?;
    ensure!(
        child.0.try_wait()?.is_none(),
        "sidecar pass failure exited coordinator"
    );
    facts.sidecar_outage.store(false, Ordering::SeqCst);
    wait_until(
        async || ready(&http, &url).await,
        "sidecar recovery retries pass",
    )
    .await?;
    facts.outage.store(true, Ordering::SeqCst);
    exit(&mut child, false).await?;
    ensure!(
        !ready(&http, &url).await?,
        "scanner outage left readiness published"
    );
    facts.outage.store(false, Ordering::SeqCst);
    let mut child = Process(command().spawn()?);
    wait_until(
        async || ready(&http, &url).await,
        "scanner restart recovery",
    )
    .await?;

    // Terminate only the connection owning this test executable's advisory lock.
    let killed:bool=sql.query_one("SELECT pg_terminate_backend(l.pid) FROM pg_locks l JOIN pg_stat_activity a ON a.pid=l.pid WHERE l.locktype='advisory' AND l.classid=1668774501 AND l.objid=1 AND l.database=(SELECT oid FROM pg_database WHERE datname=current_database()) AND l.granted AND a.application_name=$1",&[&db.schema]).await?.get(0);
    ensure!(killed, "controlled coordinator lease disconnect failed");
    exit(&mut child, false).await?;
    ensure!(
        !ready(&http, &url).await?,
        "lease loss left coordinator ready"
    );

    let mut store = Store::connect(&db.url).await?;
    store
        .halt_bridge("startup_acceptance_persisted_halt")
        .await?;
    drop(store);
    let mut child = Process(command().spawn()?);
    wait_until(
        async || {
            let Ok(response) = http.get(format!("{url}/v1/info")).send().await else {
                return Ok(false);
            };
            let v: Value = response.json().await?;
            Ok(v["scanner_ready"] == false && v["persisted"]["halted"] == true)
        },
        "halt survives executable restart",
    )
    .await?;
    signal(&child, "-INT")?;
    exit(&mut child, true).await?;
    ensure!(
        facts.unexpected.lock().unwrap().is_empty(),
        "coordinator attempted mutation or unapproved RPC"
    );
    ensure!(sql.query_one("SELECT (SELECT count(*) FROM operations)+(SELECT count(*) FROM zama_transactions)+(SELECT count(*) FROM reservations)",&[]).await?.get::<_,i64>(0)==0,"lifecycle changed payment journals");
    ensure!(
        sql.query_one("SELECT halted FROM bridge_domain", &[])
            .await?
            .get::<_, bool>(0),
        "restart cleared durable halt"
    );
    Ok(())
}
