use super::*;
use crate::store::{
    bridge_tests::{evm, funded},
    zama_test_support::{TestDb, contract, reference},
};
use alloy::{
    dyn_abi::TypedData,
    primitives::{Address, B256, Bytes, Signature, U256, keccak256},
};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicU64},
    },
    thread,
    time::{SystemTime, UNIX_EPOCH},
};
use zama::watcher::EventObservation;

fn burn(id: u64) -> EventObservation {
    EventObservation {
        block: reference(4, 0),
        transaction_hash: B256::repeat_byte(id as u8),
        log_index: 0,
        event: Event::RedeemRequested {
            redeem_id: U256::from(id),
            user: Address::repeat_byte(8),
            amount: B256::repeat_byte(id as u8),
            dest_ct: Bytes::from(vec![1; 48]),
        },
    }
}

#[tokio::test]
async fn bridge_redeem_actual_zero_and_bad_destination_persist_distinctly() -> Result<()> {
    let db = TestDb::new().await?;
    let (mut store, _) = funded(&db).await?;
    let zero = burn(61);
    let rejected = burn(62);
    evm(&mut store, 5, vec![zero.clone(), rejected.clone()]).await?;
    let pair = store.bridge_checkpoint_pair().await?;
    let r = store.record_bridge_redeem(&zero, pair, 0, None).await?;
    assert_eq!(r.state, RedeemState::Zero);
    let r = store
        .record_bridge_redeem(&rejected, pair, 75_000, None)
        .await?;
    assert_eq!(r.state, RedeemState::Rejected);
    assert_eq!(r.actual, 75_000);
    assert!(
        store
            .reserve_bridge_redeem(
                r.id,
                r.version,
                -1,
                zcash::core::NoteId {
                    txid: [10; 32],
                    action_index: 0
                }
            )
            .await
            .is_err()
    );
    assert!(store.settle_bridge_payment(r.id, pair).await?.is_none());
    drop(store);
    let mut store = db.connect().await?;
    store.migrate().await?;
    let records = store.bridge_redeems().await?;
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].state, RedeemState::Zero);
    assert_eq!(
        (records[1].state, records[1].actual),
        (RedeemState::Rejected, 75_000)
    );
    let duplicate = store
        .record_bridge_redeem(&rejected, pair, 75_000, None)
        .await?;
    assert_eq!(duplicate.version, records[1].version);
    assert!(
        store
            .record_bridge_redeem(&rejected, pair, 0, None)
            .await
            .is_err()
    );
    db.cleanup().await
}

#[tokio::test]
async fn bridge_redeem_fulfillment_reference_does_not_prove_payment() -> Result<()> {
    let db = TestDb::new().await?;
    let (mut store, _) = funded(&db).await?;
    let request = burn(63);
    let fulfillment = EventObservation {
        block: reference(4, 0),
        transaction_hash: B256::repeat_byte(64),
        log_index: 1,
        event: Event::RedeemFulfilled {
            redeem_id: U256::from(63),
            zcash_txid: B256::repeat_byte(91),
        },
    };
    evm(&mut store, 5, vec![request.clone(), fulfillment.clone()]).await?;
    let pair = store.bridge_checkpoint_pair().await?;
    store
        .record_bridge_redeem(&request, pair, 75_000, None)
        .await?;
    assert!(
        store
            .reconcile_bridge_fulfillment(U256::from(63), &fulfillment, pair)
            .await
            .is_err()
    );
    assert_eq!(
        store.bridge_redeems().await?[0].state,
        RedeemState::Rejected
    );
    assert!(store.get_attempt("cwzec/redeem/63").await?.is_none());
    db.cleanup().await
}

#[tokio::test]
async fn bridge_redeem_one_confirmation_cannot_record_burn() -> Result<()> {
    let db = TestDb::new().await?;
    let (mut store, _) = funded(&db).await?;
    let mut request = burn(65);
    request.block = reference(4, 0);
    evm(&mut store, 4, vec![request.clone()]).await?;
    let pair = store.bridge_checkpoint_pair().await?;
    assert!(
        store
            .record_bridge_redeem(&request, pair, 0, None)
            .await
            .is_err()
    );
    assert!(store.bridge_redeems().await?.is_empty());
    evm(&mut store, 5, vec![]).await?;
    let pair = store.bridge_checkpoint_pair().await?;
    assert_eq!(
        store
            .record_bridge_redeem(&request, pair, 0, None)
            .await?
            .state,
        RedeemState::Zero
    );
    db.cleanup().await
}

struct Server {
    url: String,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Server {
    fn new(handler: impl Fn(&str, Value) -> Option<Value> + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let worker = thread::spawn(move || {
            for stream in listener.incoming() {
                if stopping.load(Ordering::Relaxed) {
                    break;
                }
                let mut stream = stream.unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(&mut stream);
                let mut request = String::new();
                reader.read_line(&mut request).unwrap();
                let path = request.split_whitespace().nth(1).unwrap().to_owned();
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((key, value)) = line.split_once(':') {
                        if key.eq_ignore_ascii_case("content-length") {
                            length = value.trim().parse().unwrap();
                        }
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                if let Some(response) = handler(&path, serde_json::from_slice(&body).unwrap()) {
                    let response = response.to_string();
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        response.len(),
                        response
                    );
                }
            }
        });
        Self {
            url,
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(self.url.trim_start_matches("http://"));
        self.worker.take().unwrap().join().unwrap();
    }
}
fn permit(scope: &Value) -> Value {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let start = (millis / 1000).to_string();
    let mut extra = vec![1];
    extra.extend_from_slice(&U256::from(7).to_be_bytes::<32>());
    let typed = json!({"domain":{"name":"Decryption","version":"1","chainId":"11155111","verifyingContract":"0x5D8BD78e2ea6bbE41f26dFe9fdaEAa349e077478"},"primaryType":"UserDecryptRequestVerification","types":{"EIP712Domain":[{"name":"name","type":"string"},{"name":"version","type":"string"},{"name":"chainId","type":"uint256"},{"name":"verifyingContract","type":"address"}],"UserDecryptRequestVerification":[{"name":"publicKey","type":"bytes"},{"name":"contractAddresses","type":"address[]"},{"name":"startTimestamp","type":"uint256"},{"name":"durationDays","type":"uint256"},{"name":"extraData","type":"bytes"}]},"message":{"publicKey":"0xabcd","contractAddresses":[scope["contract"]],"startTimestamp":start,"durationDays":"1","extraData":Bytes::from(extra)}});
    let digest = serde_json::from_value::<TypedData>(typed.clone())
        .unwrap()
        .eip712_signing_hash()
        .unwrap();
    json!({"sessionId":"ab".repeat(32),"contract":scope["contract"],"user":scope["user"],"handle":scope["handle"],"expiresAt":(millis+290000).to_string(),"publicKey":"0xabcd","startTimestamp":start,"typedData":typed,"digest":digest})
}

#[tokio::test]
async fn bridge_redeem_hash_pinned_actual_and_destination_failures() -> Result<()> {
    let db = TestDb::new().await?;
    let (mut store, _) = funded(&db).await?;
    let directory = std::env::temp_dir().join(format!("cwzec_burn_fixture_{}", std::process::id()));
    std::fs::create_dir(&directory)?;
    let key = DestinationKey::create(&directory.join("destination.key"))?;
    let wallet = zcash::core::LocalSigner::create(&directory.join("wallet.key"))?;
    let destination = wallet.addresses()?.0;
    let mut event = burn(66);
    if let Event::RedeemRequested { dest_ct, .. } = &mut event.event {
        *dest_ct =
            key.encrypt_destination(11155111, contract(), Address::repeat_byte(8), &destination)?;
    }
    evm(&mut store, 5, vec![event.clone()]).await?;
    let status = Arc::new(AtomicU8::new(1));
    let mismatch = Arc::new(AtomicBool::new(false));
    let rpc_status = status.clone();
    let rpc_mismatch = mismatch.clone();
    let rpc = Server::new(move |_, request| {
        let p = &request["params"];
        let result = match request["method"].as_str().unwrap() {
            "eth_chainId" => json!("0xaa36a7"),
            "eth_getCode" => json!("0x6000"),
            "eth_getBlockByNumber" => {
                assert_eq!(p[0], "0x4");
                json!({"number":"0x4","hash":reference(4,0).hash,"parentHash":reference(3,0).hash})
            }
            "eth_call" => {
                let data = p[0]
                    .get("input")
                    .or_else(|| p[0].get("data"))
                    .unwrap()
                    .as_str()
                    .unwrap();
                let selector = &data[2..10];
                if selector == hex::encode(&keccak256("decimals()")[..4]) {
                    json!(format!("0x{:064x}", 8))
                } else if selector == hex::encode(&keccak256("getCurrentKmsContextId()")[..4]) {
                    json!(format!("0x{:064x}", 7))
                } else {
                    assert_eq!(
                        selector,
                        hex::encode(&keccak256("redemptions(uint256)")[..4])
                    );
                    assert!(data.ends_with(&format!("{:064x}", 66)));
                    assert_eq!(
                        p[1],
                        json!({"blockHash":reference(4,0).hash,"requireCanonical":true})
                    );
                    let mut encoded = vec![0; 12];
                    encoded.extend_from_slice(
                        Address::repeat_byte(if rpc_mismatch.load(Ordering::Relaxed) {
                            9
                        } else {
                            8
                        })
                        .as_slice(),
                    );
                    encoded.extend_from_slice(B256::repeat_byte(66).as_slice());
                    encoded.extend_from_slice(
                        &U256::from(rpc_status.load(Ordering::Relaxed)).to_be_bytes::<32>(),
                    );
                    encoded.extend_from_slice(&[0; 32]);
                    json!(Bytes::from(encoded))
                }
            }
            other => panic!("unexpected RPC {other}"),
        };
        Some(json!({"jsonrpc":"2.0","id":request["id"],"result":result}))
    });
    let failed = Arc::new(AtomicBool::new(false));
    let fail = failed.clone();
    let actual = Arc::new(AtomicU64::new(75_000));
    let output = actual.clone();
    let scope = Arc::new(Mutex::new(None::<Value>));
    let request_scope = scope.clone();
    let sdk = Server::new(move |path, body| {
        if fail.load(Ordering::Relaxed) {
            return None;
        }
        Some(match path {
            "/decrypt-request" => {
                assert_eq!(body["handle"], json!(B256::repeat_byte(66)));
                let prepared = permit(&body);
                *request_scope.lock().unwrap() = Some(prepared.clone());
                prepared
            }
            "/decrypt" => {
                let prepared = request_scope.lock().unwrap().clone().unwrap();
                for field in ["user", "handle", "contract", "sessionId"] {
                    assert_eq!(body[field], prepared[field]);
                }
                let signature: Signature = body["signature"].as_str().unwrap().parse().unwrap();
                let digest = serde_json::from_value(prepared["digest"].clone()).unwrap();
                assert_eq!(
                    json!(signature.recover_address_from_prehash(&digest).unwrap()),
                    body["user"]
                );
                json!({"value":output.load(Ordering::Relaxed).to_string()})
            }
            other => panic!("unexpected SDK {other}"),
        })
    });
    let signer: PrivateKeySigner =
        "0000000000000000000000000000000000000000000000000000000000000001".parse()?;
    let contract = zama::contract::CwzecClient::connect(&rpc.url, 11155111, contract()).await?;
    let sidecar = fhe::Client::new(&sdk.url)?;
    let pair = store.bridge_checkpoint_pair().await?;
    let (value, decoded) = read_burn(
        &contract,
        &sidecar,
        &signer,
        &key,
        &event,
        pair.checkpoint,
        None,
    )
    .await?;
    assert_eq!(value, 75_000);
    assert_eq!(
        decoded.as_deref().map(|d| d.as_str()),
        Some(destination.as_str())
    );
    failed.store(true, Ordering::Relaxed);
    assert!(
        read_burn(
            &contract,
            &sidecar,
            &signer,
            &key,
            &event,
            pair.checkpoint,
            None
        )
        .await
        .is_err()
    );
    assert!(
        store.bridge_redeems().await?.is_empty(),
        "FHE failure must remain unrecorded, never zero"
    );
    failed.store(false, Ordering::Relaxed);
    mismatch.store(true, Ordering::Relaxed);
    assert!(
        read_burn(
            &contract,
            &sidecar,
            &signer,
            &key,
            &event,
            pair.checkpoint,
            None
        )
        .await
        .is_err()
    );
    mismatch.store(false, Ordering::Relaxed);
    status.store(3, Ordering::Relaxed);
    assert!(
        read_burn(
            &contract,
            &sidecar,
            &signer,
            &key,
            &event,
            pair.checkpoint,
            None
        )
        .await
        .is_err()
    );
    status.store(1, Ordering::Relaxed);
    let mut tampered = event.clone();
    if let Event::RedeemRequested { dest_ct, .. } = &mut tampered.event {
        let mut bytes = dest_ct.to_vec();
        bytes[25] ^= 1;
        *dest_ct = bytes.into();
    }
    let (value, decoded) = read_burn(
        &contract,
        &sidecar,
        &signer,
        &key,
        &tampered,
        pair.checkpoint,
        None,
    )
    .await?;
    assert_eq!(value, 75_000);
    assert!(decoded.is_none());
    actual.store(0, Ordering::Relaxed);
    let (value, _) = read_burn(
        &contract,
        &sidecar,
        &signer,
        &key,
        &event,
        pair.checkpoint,
        None,
    )
    .await?;
    assert_eq!(
        value, 0,
        "only successful decryption may establish actual zero"
    );
    let saved = store
        .record_bridge_redeem(&event, pair, 75_000, Some(&destination))
        .await?;
    failed.store(true, Ordering::Relaxed);
    let (value, _) = read_burn(
        &contract,
        &sidecar,
        &signer,
        &key,
        &event,
        pair.checkpoint,
        Some(&saved),
    )
    .await?;
    assert_eq!(
        value, 75_000,
        "same canonical event reuses recorded decryption"
    );
    for changed in 0..4 {
        let mut stale = saved.clone();
        match changed {
            0 => stale.canonical = false,
            1 => stale.source.hash = B256::repeat_byte(99),
            2 => stale.source_tx = B256::repeat_byte(99),
            _ => stale.source_log += 1,
        }
        assert!(
            read_burn(
                &contract,
                &sidecar,
                &signer,
                &key,
                &event,
                pair.checkpoint,
                Some(&stale),
            )
            .await
            .is_err(),
            "changed source requires fresh decryption; SDK is unavailable"
        );
    }
    failed.store(false, Ordering::Relaxed);
    let mut orphaned = saved;
    orphaned.canonical = false;
    let (value, _) = read_burn(
        &contract,
        &sidecar,
        &signer,
        &key,
        &event,
        pair.checkpoint,
        Some(&orphaned),
    )
    .await?;
    assert_eq!(
        value, 0,
        "orphaned recorded actual cannot override fresh actual"
    );
    std::fs::remove_dir_all(directory)?;
    db.cleanup().await
}

#[tokio::test]
async fn bridge_recredit_auditor_supply_delta_and_isolation_are_required() -> Result<()> {
    let auditor: PrivateKeySigner =
        "0000000000000000000000000000000000000000000000000000000000000002".parse()?;
    let burner = Address::repeat_byte(8);
    let r = BridgeRedeem {
        id: U256::from(81),
        burner,
        handle: B256::repeat_byte(81),
        dest_ct: Bytes::from_static(&[1]),
        source: reference(4, 0),
        source_tx: B256::repeat_byte(4),
        source_log: 0,
        canonical: true,
        actual: 75_000,
        destination: None,
        state: RedeemState::RecreditPending,
        version: 1,
        operation_id: "cwzec/redeem/81".into(),
    };
    let event = EventObservation {
        block: reference(6, 0),
        transaction_hash: B256::repeat_byte(6),
        log_index: 0,
        event: Event::RedeemRecredited {
            redeem_id: r.id,
            user: burner,
            amount: B256::repeat_byte(99),
        },
    };
    let reorganized = Arc::new(AtomicBool::new(false));
    let rpc_reorg = reorganized.clone();
    let auditor_address = auditor.address();
    let rpc = Server::new(move |_, request| {
        let p = &request["params"];
        let result = match request["method"].as_str().unwrap() {
            "eth_chainId" => json!("0xaa36a7"),
            "eth_getCode" => json!("0x6000"),
            "eth_getBlockByNumber" => {
                let n = u64::from_str_radix(p[0].as_str().unwrap().trim_start_matches("0x"), 16)
                    .unwrap();
                json!({"number":format!("0x{n:x}"),"hash":reference(n,u64::from(rpc_reorg.load(Ordering::Relaxed))).hash,"parentHash":reference(n-1,0).hash})
            }
            "eth_call" => {
                let data = p[0]
                    .get("input")
                    .or_else(|| p[0].get("data"))
                    .unwrap()
                    .as_str()
                    .unwrap();
                let selector = &data[2..10];
                if selector == hex::encode(&keccak256("decimals()")[..4]) {
                    json!(format!("0x{:064x}", 8))
                } else if selector == hex::encode(&keccak256("getCurrentKmsContextId()")[..4]) {
                    json!(format!("0x{:064x}", 7))
                } else if selector == hex::encode(&keccak256("auditor()")[..4]) {
                    let mut encoded = vec![0; 12];
                    encoded.extend_from_slice(auditor_address.as_slice());
                    json!(Bytes::from(encoded))
                } else if selector == hex::encode(&keccak256("confidentialTotalSupply()")[..4]) {
                    let is_before = p[1]["blockHash"] == json!(reference(5, 0).hash);
                    json!(B256::repeat_byte(if is_before { 10 } else { 11 }))
                } else {
                    assert_eq!(
                        selector,
                        hex::encode(&keccak256("redemptions(uint256)")[..4])
                    );
                    let mut encoded = vec![0; 12];
                    encoded.extend_from_slice(burner.as_slice());
                    encoded.extend_from_slice(B256::repeat_byte(81).as_slice());
                    encoded.extend_from_slice(&U256::from(3).to_be_bytes::<32>());
                    encoded.extend_from_slice(&[0; 32]);
                    json!(Bytes::from(encoded))
                }
            }
            other => panic!("unexpected RPC {other}"),
        };
        Some(json!({"jsonrpc":"2.0","id":request["id"],"result":result}))
    });
    let actual = Arc::new(AtomicU64::new(75_000));
    let sdk_actual = actual.clone();
    let reorg_after = Arc::new(AtomicBool::new(false));
    let sdk_reorg_after = reorg_after.clone();
    let sdk_reorg = reorganized.clone();
    let scope = Arc::new(Mutex::new(None::<Value>));
    let sdk_scope = scope.clone();
    let sdk = Server::new(move |path, body| {
        Some(match path {
            "/decrypt-request" => {
                assert_eq!(body["user"], json!(auditor_address));
                assert_ne!(
                    body["handle"],
                    json!(B256::repeat_byte(99)),
                    "event ciphertext is burner-only"
                );
                let prepared = permit(&body);
                *sdk_scope.lock().unwrap() = Some(prepared.clone());
                prepared
            }
            "/decrypt" => {
                let prepared = sdk_scope.lock().unwrap().clone().unwrap();
                let before = prepared["handle"] == json!(B256::repeat_byte(10));
                if !before && sdk_reorg_after.load(Ordering::Relaxed) {
                    sdk_reorg.store(true, Ordering::Relaxed);
                }
                json!({"value":if before {100_000}else{100_000+sdk_actual.load(Ordering::Relaxed)}.to_string()})
            }
            other => panic!("unexpected SDK {other}"),
        })
    });
    let contract = zama::contract::CwzecClient::connect(&rpc.url, 11155111, contract()).await?;
    let sidecar = fhe::Client::new(&sdk.url)?;
    assert_eq!(
        verify_restoration(&contract, &sidecar, &auditor, &r, &event, &[event.clone()]).await?,
        75_000
    );
    actual.store(74_999, Ordering::Relaxed);
    assert!(
        verify_restoration(&contract, &sidecar, &auditor, &r, &event, &[event.clone()])
            .await
            .is_err()
    );
    actual.store(75_000, Ordering::Relaxed);
    let mut other = event.clone();
    other.log_index = 1;
    assert!(
        verify_restoration(
            &contract,
            &sidecar,
            &auditor,
            &r,
            &event,
            &[event.clone(), other]
        )
        .await
        .is_err()
    );
    let mut wrong = event.clone();
    if let Event::RedeemRecredited { user, .. } = &mut wrong.event {
        *user = auditor_address;
    }
    assert!(
        verify_restoration(&contract, &sidecar, &auditor, &r, &wrong, &[wrong.clone()])
            .await
            .is_err()
    );
    reorg_after.store(true, Ordering::Relaxed);
    assert!(
        verify_restoration(&contract, &sidecar, &auditor, &r, &event, &[event.clone()])
            .await
            .is_err(),
        "a reorg during decryption invalidates the proof"
    );
    Ok(())
}
