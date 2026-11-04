//! HTTP acceptance uses real loopback sockets and disposable PostgreSQL schemas.
//! All chain facts and signed payment bytes below are controlled fixtures.
use super::{
    Store,
    bridge_tests::{accounting, evm, note, scan, setup},
    zama_test_support::{TestDb, contract, reference},
};
use crate::core::{destination::DestinationKey, redeem_types::RedeemState};
use alloy::{
    primitives::{Address, B256, Bytes, U256},
    signers::local::PrivateKeySigner,
    sol_types::SolCall,
    transports::http::reqwest,
};
use anyhow::Result;
use axum::{Json, Router, routing::post};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use zama::{
    contract::{CwzecClient, TransactionDomain, TransactionFees, bindings::requestRedeemCall},
    fhe,
    watcher::{Event, EventObservation},
};
use zcash::core::{LocalSigner, NoteId, PreparedTransaction};

struct Keys {
    directory: std::path::PathBuf,
    destination: String,
    key: Arc<DestinationKey>,
}

struct Listener {
    url: String,
    task: tokio::task::JoinHandle<()>,
}
impl Listener {
    async fn start(router: Router) -> Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("http://{}", listener.local_addr()?);
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Ok(Self { url, task })
    }
}
impl Drop for Listener {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Fixtures {
    _rpc: Listener,
    _sdk: Listener,
    encrypted: Arc<tokio::sync::Mutex<Vec<Value>>>,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    block: Arc<AtomicBool>,
    fail: Arc<AtomicBool>,
    contract: Arc<CwzecClient>,
    fhe: Arc<fhe::Client>,
}
impl Fixtures {
    async fn new() -> Result<Self> {
        let rpc = Listener::start(Router::new().route("/", post(|Json(v): Json<Value>| async move {
            let result = match v["method"].as_str().unwrap() {
                "eth_chainId" => json!("0xaa36a7"),
                "eth_getCode" => json!("0x6000"),
                "eth_call" => json!(format!("0x{:064x}", 8)),
                "eth_getBlockByNumber" => {
                    let n = v["params"][0].as_str().unwrap();
                    let n = if n == "latest" { 50 } else { u64::from_str_radix(n.trim_start_matches("0x"), 16).unwrap() };
                    json!({"number":format!("0x{n:x}"),"hash":reference(n,0).hash,"parentHash":reference(n-1,0).hash})
                },
                method => panic!("unexpected RPC mutation/read: {method}"),
            };
            Json(json!({"jsonrpc":"2.0","id":v["id"],"result":result}))
        }))).await?;
        let encrypted = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let block = Arc::new(AtomicBool::new(false));
        let fail = Arc::new(AtomicBool::new(false));
        let (captured, arrived, proceed, blocked, failing) = (
            encrypted.clone(),
            entered.clone(),
            release.clone(),
            block.clone(),
            fail.clone(),
        );
        let sdk = Listener::start(Router::new().route(
            "/encrypt",
            post(move |Json(v): Json<Value>| {
                let (captured, arrived, proceed, blocked, failing) = (
                    captured.clone(),
                    arrived.clone(),
                    proceed.clone(),
                    blocked.clone(),
                    failing.clone(),
                );
                async move {
                    captured.lock().await.push(v);
                    arrived.notify_one();
                    if blocked.load(Ordering::Acquire) {
                        proceed.notified().await;
                    }
                    if failing.load(Ordering::Acquire) {
                        return Json(json!({"private_backend_error":"never-echo-this-marker"}));
                    }
                    Json(json!({"handle":B256::repeat_byte(44),"inputProof":"0x1234"}))
                }
            }),
        ))
        .await?;
        let contract = Arc::new(CwzecClient::connect(&rpc.url, 11155111, contract()).await?);
        let fhe = Arc::new(fhe::Client::new(&sdk.url)?);
        Ok(Self {
            _rpc: rpc,
            _sdk: sdk,
            encrypted,
            entered,
            release,
            block,
            fail,
            contract,
            fhe,
        })
    }
    async fn api(&self, db: &TestDb, keys: &Keys, grpc: &str, ready: bool) -> Result<Listener> {
        let api = crate::api::BridgeApi::new(
            db.connect().await?,
            self.contract.clone(),
            self.fhe.clone(),
            keys.key.clone(),
            grpc.into(),
            "public-deposit-address".into(),
            "private-admin-marker".into(),
            Arc::new(AtomicBool::new(ready)),
        );
        Listener::start(crate::api::bridge_router(Arc::new(api))).await
    }
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap()
}
async fn get(server: &Listener, path: &str, auth: bool) -> Result<(u16, Value)> {
    let mut request = http().get(format!("{}{path}", server.url));
    if auth {
        request = request.header("x-api-key", "private-admin-marker");
    }
    let response = request.send().await?;
    Ok((response.status().as_u16(), response.json().await?))
}
async fn submit(server: &Listener, path: &str, value: &Value, auth: bool) -> Result<(u16, Value)> {
    let mut request = http().post(format!("{}{path}", server.url)).json(value);
    if auth {
        request = request.header("x-api-key", "private-admin-marker");
    }
    let response = request.send().await?;
    Ok((response.status().as_u16(), response.json().await?))
}
fn private_fields_absent(v: &Value) {
    match v {
        Value::Object(map) => {
            for (key, value) in map {
                assert!(
                    ![
                        "amount",
                        "actual",
                        "value",
                        "balance",
                        "balances",
                        "supply",
                        "destination",
                        "zcash_address",
                        "dest_ct",
                        "memo",
                        "signed_bytes",
                        "admin_key",
                        "funding_address",
                        "fvk",
                        "input_proof"
                    ]
                    .contains(&key.as_str()),
                    "private field {key}"
                );
                private_fields_absent(value);
            }
        }
        Value::Array(values) => {
            for value in values {
                private_fields_absent(value);
            }
        }
        Value::String(value) => assert!(
            !value.contains("private-admin-marker")
                && !value.contains("private opaque destination")
                && !value.contains("never-echo-this-marker")
        ),
        _ => {}
    }
}
fn prepare(keys: &Keys, user: Address, operation: &str) -> Value {
    json!({"operation_id":operation,"user":user,"amount":"60000","zcash_address":keys.destination,"chain_id":"11155111","contract":contract()})
}

#[derive(Clone, PartialEq, prost::Message)]
struct LightdInfo {
    #[prost(string, tag = "4")]
    chain_name: String,
    #[prost(string, tag = "6")]
    consensus_branch_id: String,
    #[prost(uint64, tag = "7")]
    block_height: u64,
}
#[derive(Clone)]
struct GrpcFixture;
impl tonic::server::NamedService for GrpcFixture {
    const NAME: &'static str = "cash.z.wallet.sdk.rpc.CompactTxStreamer";
}
impl tonic::codegen::Service<tonic::codegen::http::Request<tonic::body::Body>> for GrpcFixture {
    type Response = tonic::codegen::http::Response<tonic::body::Body>;
    type Error = std::convert::Infallible;
    type Future = tonic::codegen::BoxFuture<Self::Response, Self::Error>;
    fn poll_ready(
        &mut self,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }
    fn call(&mut self, req: tonic::codegen::http::Request<tonic::body::Body>) -> Self::Future {
        Box::pin(async move {
            match req.uri().path() {
                "/cash.z.wallet.sdk.rpc.CompactTxStreamer/GetLightdInfo" => {
                    struct Info;
                    impl tonic::server::UnaryService<zcash::rpc::Empty> for Info {
                        type Response = LightdInfo;
                        type Future = tonic::codegen::BoxFuture<
                            tonic::Response<Self::Response>,
                            tonic::Status,
                        >;
                        fn call(&mut self, _: tonic::Request<zcash::rpc::Empty>) -> Self::Future {
                            Box::pin(async {
                                Ok(tonic::Response::new(LightdInfo {
                                    chain_name: "test".into(),
                                    consensus_branch_id: "77190ad9".into(),
                                    block_height: 50,
                                }))
                            })
                        }
                    }
                    Ok(tonic::server::Grpc::new(tonic_prost::ProstCodec::default())
                        .unary(Info, req)
                        .await)
                }
                "/cash.z.wallet.sdk.rpc.CompactTxStreamer/GetBlock" => {
                    struct Block;
                    impl tonic::server::UnaryService<zcash::rpc::BlockId> for Block {
                        type Response = zcash::rpc::CompactBlock;
                        type Future = tonic::codegen::BoxFuture<
                            tonic::Response<Self::Response>,
                            tonic::Status,
                        >;
                        fn call(
                            &mut self,
                            req: tonic::Request<zcash::rpc::BlockId>,
                        ) -> Self::Future {
                            let height = req.into_inner().height;
                            Box::pin(async move {
                                Ok(tonic::Response::new(zcash::rpc::CompactBlock {
                                    height,
                                    hash: vec![height as u8; 32],
                                    ..Default::default()
                                }))
                            })
                        }
                    }
                    Ok(tonic::server::Grpc::new(tonic_prost::ProstCodec::default())
                        .unary(Block, req)
                        .await)
                }
                path => panic!("unexpected lightwallet mutation/read: {path}"),
            }
        })
    }
}
async fn grpc() -> Result<Listener> {
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}", socket.local_addr()?);
    let task = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(GrpcFixture)
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(socket))
            .await
            .unwrap();
    });
    Ok(Listener { url, task })
}

#[tokio::test]
async fn actual_http_authentication_validation_and_private_error_boundary() -> Result<()> {
    let db = TestDb::new().await?;
    let mut store = setup(&db).await?;
    let keys = Keys::new()?;
    let fixtures = Fixtures::new().await?;
    let server = fixtures.api(&db, &keys, "http://127.0.0.1:1", true).await?;
    for path in [
        "/v1/admin/status",
        "/v1/admin/reconcile",
        "/v1/admin/recovery",
        "/v1/deposits?recipient=bad",
        "/v1/payouts/private",
    ] {
        assert_eq!(
            get(&server, path, false).await?,
            (401, json!({"error":"unauthorized"}))
        );
        let response = http()
            .get(format!("{}{path}", server.url))
            .header("x-api-key", "wrong")
            .send()
            .await?;
        assert_eq!(response.status().as_u16(), 401);
        assert_eq!(
            response.json::<Value>().await?,
            json!({"error":"unauthorized"})
        );
    }
    for path in ["/v1/admin/refund", "/v1/admin/recredit", "/v1/admin/payout"] {
        assert_eq!(submit(&server, path, &json!({}), false).await?.0, 401);
    }
    let (code, info) = get(&server, "/v1/info", false).await?;
    assert_eq!(code, 200);
    assert_eq!(info["scanner_ready"], false);
    assert_eq!(info["chain_id"], "11155111");
    private_fields_absent(&info);
    assert_eq!(
        submit(
            &server,
            "/v1/admin/payout",
            &json!({"operation_id":"private"}),
            true
        )
        .await?,
        (
            409,
            json!({"error":"arbitrary_payout_disabled_in_bridge_domain"})
        )
    );
    assert_eq!(
        get(&server, "/v1/admin/recovery", true).await?.1["enabled"],
        false
    );
    for path in [
        "/v1/redeems/01",
        "/v1/redeems/0x1",
        "/v1/deposits/not-a-txid",
        "/v1/deposits?recipient=not-an-address",
    ] {
        let (code, error) = get(&server, path, true).await?;
        assert_eq!(code, 400);
        assert_eq!(error, json!({"error":"invalid_request"}));
    }
    assert_eq!(get(&server, "/v1/redeems/9", false).await?.0, 404);
    assert_eq!(
        get(&server, "/missing", false).await?.1,
        json!({"error":"not_found"})
    );
    let response = http()
        .post(format!("{}/v1/memo", server.url))
        .send()
        .await?;
    assert_eq!(response.status().as_u16(), 405);
    assert_eq!(
        response.json::<Value>().await?,
        json!({"error":"method_not_allowed"})
    );
    let response = http().get(format!("{}/v1/memo", server.url)).send().await?;
    assert_eq!(response.status().as_u16(), 400);
    assert_eq!(
        response.json::<Value>().await?,
        json!({"error":"invalid_request"})
    );
    assert_eq!(
        get(
            &server,
            "/v1/memo?recipient=0x0707070707070707070707070707070707070707&secret=private",
            false
        )
        .await?
        .0,
        400
    );
    assert_eq!(
        get(
            &server,
            "/v1/memo?recipient=0x0707070707070707070707070707070707070707",
            false
        )
        .await?
        .0,
        200
    );
    let (_, status) = get(&server, "/v1/admin/status", true).await?;
    private_fields_absent(&status);
    // A private SDK payload is never forwarded, even through nested anyhow context.
    fixtures.fail.store(true, Ordering::Release);
    let (code, error) = submit(
        &server,
        "/v1/redeem/prepare",
        &prepare(&keys, Address::repeat_byte(7), "fails"),
        false,
    )
    .await?;
    assert_eq!(code, 503);
    assert_eq!(error, json!({"error":"service_unavailable"}));
    assert!(store.user_redeem_request("fails").await?.is_none());
    store.halt_bridge("private diagnostic marker").await?;
    let (_, info) = get(&server, "/v1/info", false).await?;
    assert_eq!(info["persisted"]["halted"], true);
    assert_eq!(info["scanner_ready"], false);
    private_fields_absent(&info);
    db.cleanup().await
}

#[tokio::test]
async fn preparation_binds_terms_converges_concurrent_retries_and_survives_listener_restart()
-> Result<()> {
    let db = TestDb::new().await?;
    let mut store = setup(&db).await?;
    let keys = Keys::new()?;
    let fixtures = Fixtures::new().await?;
    let first = fixtures
        .api(&db, &keys, "http://127.0.0.1:1", false)
        .await?;
    let second = fixtures
        .api(&db, &keys, "http://127.0.0.1:1", false)
        .await?;
    let user = PrivateKeySigner::random();
    let request = prepare(&keys, user.address(), "durable");
    for amount in [
        json!(60000),
        json!("060000"),
        json!("+60000"),
        json!("60000.0"),
        json!("6e4"),
        json!("0"),
        json!("10000"),
        json!("18446744073709551616"),
    ] {
        let mut invalid = request.clone();
        invalid["amount"] = amount;
        assert_eq!(
            submit(&first, "/v1/redeem/prepare", &invalid, false)
                .await?
                .0,
            400
        );
    }
    for (field, value) in [
        ("chain_id", json!("1")),
        ("contract", json!(Address::repeat_byte(99))),
    ] {
        let mut invalid = request.clone();
        invalid[field] = value;
        assert_eq!(
            submit(&first, "/v1/redeem/prepare", &invalid, false)
                .await?
                .0,
            409
        );
    }
    assert!(fixtures.encrypted.lock().await.is_empty());
    let (a, b) = tokio::join!(
        submit(&first, "/v1/redeem/prepare", &request, false),
        submit(&second, "/v1/redeem/prepare", &request, false)
    );
    let a = a?;
    let b = b?;
    assert_eq!(a.0, 200);
    assert_eq!(a, b);
    assert_eq!(a.1["transaction"]["from"], json!(user.address()));
    assert_eq!(a.1["transaction"]["to"], json!(contract()));
    assert_eq!(a.1["transaction"]["chainId"], "0xaa36a7");
    assert_eq!(a.1["transaction"]["value"], "0x0");
    assert_eq!(a.1["already_submitted"], false);
    for captured in fixtures.encrypted.lock().await.iter() {
        assert_eq!(
            captured,
            &json!({"contract":contract(),"user":user.address(),"value":"60000"})
        );
    }
    let saved = store.user_redeem_request("durable").await?.unwrap();
    let decoded = requestRedeemCall::abi_decode(&saved.calldata)?;
    assert_eq!(decoded.amount, B256::repeat_byte(44));
    assert_eq!(decoded.proof, Bytes::from_static(&[0x12, 0x34]));
    assert_eq!(
        &**keys
            .key
            .decrypt_destination(11155111, contract(), user.address(), &decoded.destCt)?,
        keys.destination
    );
    assert!(
        keys.key
            .decrypt_destination(
                11155111,
                contract(),
                Address::repeat_byte(99),
                &decoded.destCt
            )
            .is_err()
    );
    for (field, value) in [
        ("amount", json!("60001")),
        ("user", json!(Address::repeat_byte(99))),
        (
            "zcash_address",
            json!(
                LocalSigner::create(&keys.directory.join("other.key"))?
                    .addresses()?
                    .0
            ),
        ),
    ] {
        let mut conflict = request.clone();
        conflict[field] = value;
        let (code, error) = submit(&second, "/v1/redeem/prepare", &conflict, false).await?;
        assert_eq!(code, 409);
        assert_eq!(
            error,
            json!({"error":"operation_blocked_or_terms_conflict"})
        );
    }
    // Conflicting new terms race on separate DB connections; exactly one becomes durable.
    let competing = prepare(&keys, user.address(), "competing");
    let mut changed = competing.clone();
    changed["amount"] = json!("70000");
    let (x, y) = tokio::join!(
        submit(&first, "/v1/redeem/prepare", &competing, false),
        submit(&second, "/v1/redeem/prepare", &changed, false)
    );
    let (x, y) = (x?, y?);
    assert!((x.0 == 200 && y.0 == 409) || (x.0 == 409 && y.0 == 200));
    let bound = store.user_redeem_request("competing").await?.unwrap();
    assert_eq!(bound.amount, if x.0 == 200 { 60000 } else { 70000 });
    drop(first);
    drop(second);
    let restarted = fixtures
        .api(&db, &keys, "http://127.0.0.1:1", false)
        .await?;
    assert_eq!(
        submit(&restarted, "/v1/redeem/prepare", &request, false).await?,
        a
    );
    let domain = TransactionDomain::new(11155111, contract())?;
    let call = domain.request_redeem(
        user.address(),
        decoded.amount,
        decoded.proof,
        decoded.destCt,
    );
    let signed = domain.sign_user_redeem(
        &user,
        &call,
        TransactionFees {
            nonce: 7,
            gas_limit: 100_000,
            max_fee_per_gas: 20,
            max_priority_fee_per_gas: 2,
        },
    )?;
    store.save_user_redeem_signed("durable", &signed).await?;
    drop(restarted);
    let restarted = fixtures
        .api(&db, &keys, "http://127.0.0.1:1", false)
        .await?;
    let (code, reply) = submit(&restarted, "/v1/redeem/prepare", &request, false).await?;
    assert_eq!(code, 200);
    assert_eq!(reply["already_submitted"], true);
    assert_eq!(reply["transaction_hash"], json!(signed.hash()));
    assert!(reply.get("transaction").is_none());
    assert_eq!(
        store
            .user_redeem_request("durable")
            .await?
            .unwrap()
            .signed
            .unwrap()
            .raw(),
        signed.raw()
    );
    assert_eq!(
        store
            .user_redeem_request("durable")
            .await?
            .unwrap()
            .calldata,
        saved.calldata
    );
    store
        .client
        .execute(
            "UPDATE bridge_domain SET contract=$1",
            &[&Address::repeat_byte(99).as_slice()],
        )
        .await?;
    assert_eq!(
        submit(&restarted, "/v1/redeem/prepare", &request, false)
            .await?
            .0,
        409
    );
    db.cleanup().await
}

#[tokio::test]
async fn accepted_preparation_survives_client_disconnect() -> Result<()> {
    let db = TestDb::new().await?;
    let store = setup(&db).await?;
    let keys = Keys::new()?;
    let fixtures = Fixtures::new().await?;
    fixtures.block.store(true, Ordering::Release);
    let server = fixtures
        .api(&db, &keys, "http://127.0.0.1:1", false)
        .await?;
    let request = prepare(&keys, Address::repeat_byte(7), "disconnected");
    let url = format!("{}/v1/redeem/prepare", server.url);
    let pending = request.clone();
    let client = tokio::spawn(async move { http().post(url).json(&pending).send().await });
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        fixtures.entered.notified(),
    )
    .await?;
    client.abort();
    let _ = client.await;
    fixtures.block.store(false, Ordering::Release);
    fixtures.release.notify_one();
    let retry = submit(&server, "/v1/redeem/prepare", &request, false).await?;
    assert_eq!(retry.0, 200);
    let saved = store.user_redeem_request("disconnected").await?.unwrap();
    assert_eq!(retry.1["transaction"]["data"], json!(saved.calldata));
    assert_eq!(fixtures.encrypted.lock().await.len(), 1);
    db.cleanup().await
}

#[tokio::test]
async fn deposits_in_one_transaction_keep_note_identity_and_confirmation_state() -> Result<()> {
    let db = TestDb::new().await?;
    let mut store = setup(&db).await?;
    let keys = Keys::new()?;
    let fixtures = Fixtures::new().await?;
    let server = fixtures.api(&db, &keys, "http://127.0.0.1:1", true).await?;
    let first = note(10, 60_000, false);
    let mut second = note(12, 70_000, false);
    second.id.txid = first.id.txid;
    second.id.action_index = 1;
    scan(
        &mut store,
        2,
        vec![first.clone(), second.clone(), note(13, 999_999, true)],
        vec![],
    )
    .await?;
    let path = format!("/v1/deposits/{}", crate::api::txid_text(first.id.txid));
    let (code, early) = get(&server, &path, false).await?;
    assert_eq!(code, 200);
    let deposits = early["deposits"].as_array().unwrap();
    assert_eq!(deposits.len(), 2);
    for d in deposits {
        assert_eq!(d["status"], "awaiting_confirmations");
        assert_eq!(d["confirmations"], 1);
    }
    assert_ne!(deposits[0]["deposit_id"], deposits[1]["deposit_id"]);
    assert_eq!(deposits[0]["txid"], deposits[1]["txid"]);
    assert_ne!(deposits[0]["action_index"], deposits[1]["action_index"]);
    private_fields_absent(&early);
    scan(&mut store, 3, vec![], vec![]).await?;
    scan(&mut store, 4, vec![], vec![]).await?;
    evm(&mut store, 3, vec![]).await?;
    let ingested = store.ingest_bridge_deposits().await?;
    assert_eq!(ingested.len(), 2);
    let (_, confirmed) = get(&server, &path, false).await?;
    for d in confirmed["deposits"].as_array().unwrap() {
        assert_eq!(d["status"], "detected");
        assert_eq!(d["confirmations"], 3);
        assert_eq!(d["version"], "0");
    }
    let recipient = format!("/v1/deposits?recipient={}", Address::repeat_byte(7));
    assert_eq!(get(&server, &recipient, true).await?.1, confirmed);
    assert_eq!(
        get(
            &server,
            &format!("/v1/deposits?recipient={}", Address::repeat_byte(8)),
            true
        )
        .await?
        .1,
        json!({"deposits":[]})
    );
    assert_eq!(
        get(&server, &format!("/v1/deposits/{}", "ff".repeat(32)), false)
            .await?
            .1,
        json!({"deposits":[]})
    );
    db.cleanup().await
}

async fn snapshot(store: &Store) -> Result<Vec<(String, Vec<String>)>> {
    let mut all = vec![];
    for row in store.client.query("SELECT table_name FROM information_schema.tables WHERE table_schema=current_schema() AND table_type='BASE TABLE' ORDER BY table_name",&[]).await? {
        let name:String=row.get(0);
        let sql=format!("SELECT row_to_json(t)::text FROM \"{}\" t ORDER BY row_to_json(t)::text",name.replace('"',"\"\""));
        all.push((name,store.client.query(&sql,&[]).await?.iter().map(|r|r.get(0)).collect()));
    }
    Ok(all)
}

#[tokio::test]
async fn reconciliation_is_exact_saved_report_and_reads_do_not_change_any_table() -> Result<()> {
    let db = TestDb::new().await?;
    let mut store = funded(&db).await?;
    let keys = Keys::new()?;
    let fixtures = Fixtures::new().await?;
    let server = fixtures.api(&db, &keys, "http://127.0.0.1:1", true).await?;
    assert_eq!(
        get(&server, "/v1/admin/reconcile", true).await?.1["report"],
        Value::Null
    );
    let info = get(&server, "/v1/info", false).await?.1;
    assert_eq!(info["persisted"]["report_valid"], false);
    assert_eq!(info["scanner_ready"], false);
    let a = store.bridge_accounting(&accounting(2, 2, 0)).await?;
    let before = snapshot(&store).await?;
    for _ in 0..3 {
        let (code, reply) = get(&server, "/v1/admin/reconcile", true).await?;
        assert_eq!(code, 200);
        assert_eq!(
            reply["report"],
            json!({"checkpoint_id":a.id.to_string(),"zcash_height":2,"evm_height":2,"zcash_hash":hex::encode([2u8;32]),"evm_hash":format!("{:#x}",reference(2,0).hash),"supply_zat":"0","assets_zat":"260000","deposit_liability_zat":"60000","unpaid_redemptions_zat":"0","fee_reserve_zat":"200000","coverage_complete":true,"valid":true})
        );
        assert_eq!(reply["persisted"]["report_current"], true);
        assert_eq!(
            get(&server, "/v1/info", false).await?.1["scanner_ready"],
            true
        );
        private_fields_absent(&get(&server, "/v1/admin/status", true).await?.1);
    }
    assert_eq!(snapshot(&store).await?, before);
    // A later invalid report must not borrow readiness from an older complete one.
    let latest = store.bridge_accounting(&accounting(2, 2, 0)).await?;
    store
        .client
        .execute(
            "UPDATE bridge_checkpoints SET valid=false WHERE id=$1",
            &[&latest.id],
        )
        .await?;
    assert_eq!(
        get(&server, "/v1/info", false).await?.1["scanner_ready"],
        false
    );
    store
        .client
        .execute(
            "UPDATE bridge_checkpoints SET valid=true WHERE id=$1",
            &[&latest.id],
        )
        .await?;
    scan(&mut store, 5, vec![], vec![]).await?;
    let info = get(&server, "/v1/info", false).await?.1;
    assert_eq!(info["persisted"]["report_current"], false);
    assert_eq!(info["persisted"]["report_valid"], true);
    assert_eq!(info["scanner_ready"], true);
    assert_eq!(
        get(&server, "/v1/admin/reconcile", true).await?.1["report"]["checkpoint_id"],
        latest.id.to_string()
    );
    let stale = store.bridge_accounting(&accounting(3, 2, 0)).await?;
    store.halt_bridge("private-halt-reason").await?;
    let before = snapshot(&store).await?;
    let (_, reply) = get(&server, "/v1/admin/reconcile", true).await?;
    assert_eq!(reply["persisted"]["halted"], true);
    assert_eq!(
        get(&server, "/v1/admin/status", true).await?.1["coordinator_ready"],
        false
    );
    assert_eq!(reply["report"]["checkpoint_id"], stale.id.to_string());
    assert!(!reply.to_string().contains("private-halt-reason"));
    assert_eq!(snapshot(&store).await?, before);
    db.cleanup().await
}

fn input() -> NoteId {
    NoteId {
        txid: [11; 32],
        action_index: 0,
    }
}
fn prepared() -> PreparedTransaction {
    PreparedTransaction {
        bytes: b"private controlled signed payout bytes".to_vec(),
        txid: [80; 32],
        inputs: vec![input()],
        expiry_height: 20,
        fee: 10_000,
    }
}
async fn expect_stage(server: &Listener, id: u64, stage: &str) -> Result<Value> {
    let (code, reply) = get(server, &format!("/v1/redeems/{id}"), false).await?;
    assert_eq!(code, 200);
    assert_eq!(reply["status"], stage);
    private_fields_absent(&reply);
    Ok(reply)
}

#[tokio::test]
async fn redemption_http_observes_burn_payment_and_confirmed_fulfillment() -> Result<()> {
    let db = TestDb::new().await?;
    let mut store = funded(&db).await?;
    let keys = Keys::new()?;
    let fixtures = Fixtures::new().await?;
    let grpc = grpc().await?;
    let server = fixtures.api(&db, &keys, &grpc.url, true).await?;
    let requested = burn(1, 4);
    let zero = burn(2, 4);
    let rejected = burn(3, 4);
    evm(
        &mut store,
        4,
        vec![requested.clone(), zero.clone(), rejected.clone()],
    )
    .await?;
    expect_stage(&server, 1, "awaiting_confirmations").await?;
    evm(&mut store, 5, vec![]).await?;
    expect_stage(&server, 1, "requested").await?;
    let pair = store.bridge_checkpoint_pair().await?;
    store.record_bridge_redeem(&zero, pair, 0, None).await?;
    store
        .record_bridge_redeem(&rejected, pair, 50_000, None)
        .await?;
    let r = store
        .record_bridge_redeem(&requested, pair, 60_000, Some(&keys.destination))
        .await?;
    expect_stage(&server, 2, "completed_zero").await?;
    expect_stage(&server, 3, "rejected_before_payout").await?;
    let report = store.bridge_accounting(&accounting(2, 4, 0)).await?;
    store
        .reserve_bridge_redeem(r.id, r.version, report.id, input())
        .await?;
    expect_stage(&server, 1, "reserved").await?;
    let recredit = json!({"redeem_id":"1","version":"0"});
    assert_eq!(
        submit(&server, "/v1/admin/recredit", &recredit, true)
            .await?
            .0,
        409
    );
    store.save_prepared(&r.operation_id, &prepared()).await?;
    expect_stage(&server, 1, "payout_prepared").await?;
    store.mark_broadcast(&r.operation_id).await?;
    expect_stage(&server, 1, "broadcast").await?;
    assert_eq!(
        get(
            &server,
            &format!("/v1/payouts/{}", r.operation_id.replace('/', "%2F")),
            true
        )
        .await?
        .1["status"],
        "broadcast"
    );
    // Controlled scan evidence, not generated Zcash proofs or a real broadcast.
    let mut change = note(80, 140_000, true);
    change.mined_height = 5;
    store
        .commit_scan(&zcash::core::ScanUpdate {
            checkpoint: super::bridge_tests::cp(5),
            received: vec![change],
            witnesses: vec![(
                NoteId {
                    txid: [10; 32],
                    action_index: 0,
                },
                vec![1],
            )],
            spent_nullifiers: vec![[11; 32]],
            txids: vec![[80; 32]],
        })
        .await?;
    expect_stage(&server, 1, "awaiting_confirmations").await?;
    scan(&mut store, 6, vec![], vec![]).await?;
    scan(&mut store, 7, vec![], vec![]).await?;
    expect_stage(&server, 1, "confirmed_observed").await?;
    let pair = store.bridge_checkpoint_pair().await?;
    assert_eq!(
        store.settle_bridge_payment(r.id, pair).await?,
        Some(B256::repeat_byte(80))
    );
    let paid = expect_stage(&server, 1, "paid").await?;
    assert_eq!(paid["payment"]["status"], "paid");
    let signer = PrivateKeySigner::random();
    let domain = TransactionDomain::new(11155111, contract())?;
    store.initialize_zama_lane(signer.address(), 0, 0).await?;
    let signed = domain.sign_operator(
        &signer,
        &domain.fulfill(r.id, B256::repeat_byte(80)),
        TransactionFees {
            nonce: 0,
            gas_limit: 100_000,
            max_fee_per_gas: 20,
            max_priority_fee_per_gas: 2,
        },
    )?;
    store.save_zama_signed(&r.fulfillment_id(), &signed).await?;
    let event = EventObservation {
        block: reference(6, 0),
        transaction_hash: signed.hash(),
        log_index: 0,
        event: Event::RedeemFulfilled {
            redeem_id: r.id,
            zcash_txid: B256::repeat_byte(80),
        },
    };
    evm(&mut store, 6, vec![event.clone()]).await?;
    assert_eq!(
        expect_stage(&server, 1, "paid").await?["fulfillment"]["settled"],
        false
    );
    evm(&mut store, 7, vec![]).await?;
    let pair = store.bridge_checkpoint_pair().await?;
    store
        .reconcile_bridge_fulfillment(r.id, &event, pair)
        .await?;
    let fulfilled = expect_stage(&server, 1, "fulfilled").await?;
    assert_eq!(fulfilled["fulfillment"]["settled"], true);
    assert_eq!(fulfilled["fulfillment"]["confirmations"], 2);
    drop(server);
    let restarted = fixtures.api(&db, &keys, &grpc.url, false).await?;
    expect_stage(&restarted, 1, "fulfilled").await?;
    assert_eq!(
        store
            .get_attempt(&r.operation_id)
            .await?
            .unwrap()
            .prepared
            .unwrap()
            .bytes,
        prepared().bytes
    );
    db.cleanup().await
}

#[tokio::test]
async fn expired_broadcast_is_blocked_and_keeps_same_reservation_after_http_restart() -> Result<()>
{
    let db = TestDb::new().await?;
    let mut store = funded(&db).await?;
    let keys = Keys::new()?;
    let fixtures = Fixtures::new().await?;
    let request = burn(1, 4);
    evm(&mut store, 5, vec![request.clone()]).await?;
    let pair = store.bridge_checkpoint_pair().await?;
    let r = store
        .record_bridge_redeem(&request, pair, 60_000, Some(&keys.destination))
        .await?;
    let report = store.bridge_accounting(&accounting(2, 4, 0)).await?;
    store
        .reserve_bridge_redeem(r.id, r.version, report.id, input())
        .await?;
    store.save_prepared(&r.operation_id, &prepared()).await?;
    store.mark_broadcast(&r.operation_id).await?;
    let server = fixtures
        .api(&db, &keys, "http://127.0.0.1:1", false)
        .await?;
    for height in 5..=20 {
        scan(&mut store, height, vec![], vec![]).await?;
    }
    let blocked = expect_stage(&server, 1, "blocked_historical_absence_unproven").await?;
    assert_eq!(blocked["payment"]["broadcast_acknowledged"], true);
    drop(server);
    let restarted = fixtures
        .api(&db, &keys, "http://127.0.0.1:1", false)
        .await?;
    assert_eq!(
        expect_stage(&restarted, 1, "blocked_historical_absence_unproven").await?,
        blocked
    );
    assert_eq!(store.reserved_inputs().await?.len(), 1);
    assert_eq!(
        store
            .get_attempt(&r.operation_id)
            .await?
            .unwrap()
            .prepared
            .unwrap()
            .bytes,
        prepared().bytes
    );
    db.cleanup().await
}
impl Keys {
    fn new() -> Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "cwzec-http-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory)?;
        let wallet = LocalSigner::create(&directory.join("wallet.key"))?;
        let destination = wallet.addresses()?.0;
        let key = Arc::new(DestinationKey::create(&directory.join("destination.key"))?);
        Ok(Self {
            directory,
            destination,
            key,
        })
    }
}
impl Drop for Keys {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn burn(id: u64, height: u64) -> EventObservation {
    EventObservation {
        block: reference(height, 0),
        transaction_hash: B256::repeat_byte(40 + id as u8),
        log_index: 0,
        event: Event::RedeemRequested {
            redeem_id: U256::from(id),
            user: Address::repeat_byte(7),
            amount: B256::repeat_byte(44),
            dest_ct: Bytes::from_static(b"private opaque destination"),
        },
    }
}

async fn funded(db: &TestDb) -> Result<Store> {
    let mut store = setup(db).await?;
    scan(
        &mut store,
        2,
        vec![note(10, 60_000, false), note(11, 200_000, true)],
        vec![],
    )
    .await?;
    scan(&mut store, 3, vec![], vec![]).await?;
    scan(&mut store, 4, vec![], vec![]).await?;
    evm(&mut store, 3, vec![]).await?;
    store.ingest_bridge_deposits().await?;
    Ok(store)
}

#[tokio::test]
async fn authorization_rechecks_exact_pair_after_either_chain_advances() -> Result<()> {
    for zcash_drift in [true, false] {
        let db = TestDb::new().await?;
        let keys = Keys::new()?;
        let mut store = funded(&db).await?;
        let event = burn(1, 4);
        evm(&mut store, 5, vec![event.clone()]).await?;
        let pair = store.bridge_checkpoint_pair().await?;
        let redeem = store
            .record_bridge_redeem(&event, pair, 50_000, None)
            .await?;
        let deposit = store.bridge_deposits().await?.remove(0);
        if zcash_drift {
            scan(&mut store, 5, vec![], vec![]).await?;
        } else {
            evm(&mut store, 6, vec![]).await?;
        }
        for error in [
            store
                .authorize_bridge_refund_at(
                    deposit.deposit_id,
                    deposit.version,
                    "alice",
                    &keys.destination,
                    pair,
                )
                .await
                .err(),
            store
                .authorize_bridge_recredit_at(redeem.id, redeem.version, pair)
                .await
                .err(),
        ] {
            assert_eq!(
                error.unwrap().to_string(),
                "authorization checkpoint changed"
            );
        }
        assert!(store.bridge_refunds().await?.is_empty());
        assert_eq!(
            store.bridge_redeems().await?[0].state,
            RedeemState::Rejected
        );
        let current = store.bridge_checkpoint_pair().await?;
        assert!(
            store
                .authorize_bridge_refund_at(
                    deposit.deposit_id,
                    deposit.version,
                    "alice",
                    &keys.destination,
                    current
                )
                .await
                .is_ok()
        );
        assert!(
            store
                .authorize_bridge_recredit_at(redeem.id, redeem.version, current)
                .await
                .is_ok()
        );
        db.cleanup().await?;
    }
    Ok(())
}

#[tokio::test]
async fn coordinator_lease_is_exclusive_and_reacquired_after_connection_close() -> Result<()> {
    let db = TestDb::new().await?;
    let first = db.connect().await?;
    let second = db.connect().await?;
    first.claim_coordinator().await?;
    assert_eq!(
        second.claim_coordinator().await.unwrap_err().to_string(),
        "bridge coordinator already running"
    );
    drop(first);
    let mut acquired = false;
    for _ in 0..100 {
        if second.claim_coordinator().await.is_ok() {
            acquired = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        acquired,
        "closing lease connection must release advisory lock"
    );
    drop(second);
    db.cleanup().await
}

#[tokio::test]
async fn admin_http_admission_is_immutable_concurrent_and_blocks_issued_payouts() -> Result<()> {
    let db = TestDb::new().await?;
    let mut store = funded(&db).await?;
    let keys = Keys::new()?;
    let fixtures = Fixtures::new().await?;
    let grpc = grpc().await?;
    let first = fixtures.api(&db, &keys, &grpc.url, true).await?;
    let second = fixtures.api(&db, &keys, &grpc.url, true).await?;
    let d = store.bridge_deposits().await?.remove(0);
    let a = json!({"deposit_id":d.deposit_id,"version":d.version.to_string(),"participant":"alice","destination":keys.destination});
    let mut b = a.clone();
    b["participant"] = json!("bob");
    let (x, y) = tokio::join!(
        submit(&first, "/v1/admin/refund", &a, true),
        submit(&second, "/v1/admin/refund", &b, true)
    );
    let (x, y) = (x?, y?);
    assert!(
        (x.0 == 202 && y.0 == 409) || (x.0 == 409 && y.0 == 202),
        "{x:?} {y:?}"
    );
    let winner = if x.0 == 202 { &a } else { &b };
    let accepted = if x.0 == 202 { &x } else { &y };
    assert_eq!(
        &submit(&second, "/v1/admin/refund", winner, true).await?,
        accepted
    );
    let refund = store.bridge_refunds().await?.remove(0);
    assert_eq!(refund.participant, winner["participant"].as_str().unwrap());
    assert_eq!(refund.destination, keys.destination);
    assert!(store.get_attempt(&refund.operation_id).await?.is_none());
    let report = store.bridge_accounting(&accounting(2, 2, 0)).await?;
    let current = store.bridge_deposit(d.deposit_id).await?.unwrap();
    assert!(
        store
            .claim_bridge_mint(d.deposit_id, current.version, report.id)
            .await
            .is_err()
    );
    let mut changed = winner.clone();
    changed["destination"] = json!(
        LocalSigner::create(&keys.directory.join("different.key"))?
            .addresses()?
            .0
    );
    assert_eq!(
        submit(&first, "/v1/admin/refund", &changed, true).await?.0,
        409
    );
    let mut invalid = winner.clone();
    invalid["version"] = json!(0);
    assert_eq!(
        submit(&first, "/v1/admin/refund", &invalid, true).await?.0,
        400
    );
    let mut invalid = winner.clone();
    invalid["participant"] = json!("unknown");
    assert_eq!(
        submit(&first, "/v1/admin/refund", &invalid, true).await?.0,
        409
    );
    let event = burn(1, 4);
    let blocked = burn(2, 4);
    evm(&mut store, 5, vec![event.clone(), blocked.clone()]).await?;
    let pair = store.bridge_checkpoint_pair().await?;
    let r = store
        .record_bridge_redeem(&event, pair, 50_000, None)
        .await?;
    let rejected = store
        .record_bridge_redeem(&blocked, pair, 40_000, None)
        .await?;
    // Historical unsigned reservation is sufficient to exclude restoration.
    store.client.execute("INSERT INTO operations(id,destination,amount) VALUES($1,'private legacy destination',30000)",&[&rejected.operation_id]).await?;
    store
        .client
        .execute(
            "INSERT INTO reservations(txid,action_index,operation_id,nullifier) VALUES($1,0,$2,$3)",
            &[&&[11u8; 32][..], &rejected.operation_id, &&[11u8; 32][..]],
        )
        .await?;
    assert_eq!(
        submit(
            &first,
            "/v1/admin/recredit",
            &json!({"redeem_id":"2","version":rejected.version.to_string()}),
            true
        )
        .await?
        .0,
        409
    );
    assert_eq!(store.reserved_inputs().await?.len(), 1);
    let terms = json!({"redeem_id":"1","version":r.version.to_string()});
    let (x, y) = tokio::join!(
        submit(&first, "/v1/admin/recredit", &terms, true),
        submit(&second, "/v1/admin/recredit", &terms, true)
    );
    let (x, y) = (x?, y?);
    assert_eq!(x.0, 202);
    assert_eq!(x, y);
    expect_stage(&first, 1, "recredit_pending").await?;
    assert_eq!(
        submit(
            &second,
            "/v1/admin/recredit",
            &json!({"redeem_id":"1","version":"999"}),
            true
        )
        .await?,
        x
    );
    let operator = PrivateKeySigner::random();
    let domain = TransactionDomain::new(11155111, contract())?;
    store.initialize_zama_lane(operator.address(), 0, 0).await?;
    let signed = domain.sign_operator(
        &operator,
        &domain.recredit(r.id),
        TransactionFees {
            nonce: 0,
            gas_limit: 100_000,
            max_fee_per_gas: 20,
            max_priority_fee_per_gas: 2,
        },
    )?;
    store.save_zama_signed(&r.recredit_id(), &signed).await?;
    let restored = EventObservation {
        block: reference(6, 0),
        transaction_hash: signed.hash(),
        log_index: 0,
        event: Event::RedeemRecredited {
            redeem_id: r.id,
            user: r.burner,
            amount: B256::repeat_byte(88),
        },
    };
    let mut batch = super::zama_test_support::batch(store.zama_cursor().await?.unwrap().tip, 7, 0);
    batch.chain_id = 11155111;
    batch.events = vec![restored.clone()];
    batch.receipts = vec![zama::watcher::ReceiptObservation {
        transaction_hash: signed.hash(),
        block: restored.block,
        from: operator.address(),
        to: contract(),
        success: true,
    }];
    store.apply_zama_scan(&batch).await?;
    let pair = store.bridge_checkpoint_pair().await?;
    store
        .reconcile_bridge_recredit(r.id, &restored, pair, 50_000)
        .await?;
    expect_stage(&first, 1, "re_credited").await?;
    drop(first);
    drop(second);
    let restarted = fixtures.api(&db, &keys, &grpc.url, false).await?;
    assert_eq!(
        store.bridge_refunds().await?[0].operation_id,
        refund.operation_id
    );
    assert_eq!(
        &submit(&restarted, "/v1/admin/refund", winner, true).await?,
        accepted
    );
    expect_stage(&restarted, 1, "re_credited").await?;
    private_fields_absent(&get(&restarted, "/v1/admin/status", true).await?.1);
    db.cleanup().await
}
