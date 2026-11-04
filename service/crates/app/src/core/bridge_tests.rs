use super::*;
use crate::store::{
    bridge_tests::*,
    zama_test_support::{TestDb, contract, reference},
};
use alloy::{
    dyn_abi::TypedData,
    primitives::{Bytes, Signature, U256, keccak256},
};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{SystemTime, UNIX_EPOCH},
};
struct Server {
    url: String,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Server {
    fn new(handler: impl Fn(&str, Value) -> Value + Send + 'static) -> Self {
        let l = TcpListener::bind("127.0.0.1:0").expect("loopback bind required");
        let url = format!("http://{}", l.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let worker = thread::spawn(move || {
            for connection in l.incoming() {
                if stopped.load(Ordering::Relaxed) {
                    break;
                }
                let mut socket = connection.unwrap();
                socket
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = vec![];
                let mut buf = [0; 4096];
                let end = loop {
                    let n = socket.read(&mut buf).unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&buf[..n]);
                    if let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                        let h = String::from_utf8_lossy(&bytes[..end]);
                        let len = h
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if bytes.len() >= end + 4 + len {
                            break end;
                        }
                    }
                };
                let h = String::from_utf8_lossy(&bytes[..end]);
                let path = h.split_whitespace().nth(1).unwrap();
                let response =
                    handler(path, serde_json::from_slice(&bytes[end + 4..]).unwrap()).to_string();
                let _ = write!(
                    socket,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response.len(),
                    response
                );
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
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
fn permit(s: &Value) -> Value {
    let start = (now() / 1000).to_string();
    let mut extra = vec![1];
    extra.extend_from_slice(&U256::from(7).to_be_bytes::<32>());
    let typed = json!({"domain":{"name":"Decryption","version":"1","chainId":"11155111","verifyingContract":"0x5D8BD78e2ea6bbE41f26dFe9fdaEAa349e077478"},"primaryType":"UserDecryptRequestVerification","types":{"EIP712Domain":[{"name":"name","type":"string"},{"name":"version","type":"string"},{"name":"chainId","type":"uint256"},{"name":"verifyingContract","type":"address"}],"UserDecryptRequestVerification":[{"name":"publicKey","type":"bytes"},{"name":"contractAddresses","type":"address[]"},{"name":"startTimestamp","type":"uint256"},{"name":"durationDays","type":"uint256"},{"name":"extraData","type":"bytes"}]},"message":{"publicKey":"0xabcd","contractAddresses":[s["contract"]],"startTimestamp":start,"durationDays":"1","extraData":Bytes::from(extra)}});
    let digest = serde_json::from_value::<TypedData>(typed.clone())
        .unwrap()
        .eip712_signing_hash()
        .unwrap();
    json!({"sessionId":"ab".repeat(32),"contract":s["contract"],"user":s["user"],"handle":s["handle"],"expiresAt":(now()+290000).to_string(),"publicKey":"0xabcd","startTimestamp":start,"typedData":typed,"digest":digest})
}
#[tokio::test]
async fn bridge_real_hash_pinned_rpc_sdk_authorization_actual_decryption_and_restart() -> Result<()>
{
    let db = TestDb::new().await?;
    let (mut store, d) = funded(&db).await?;
    store.rewind_zama(reference(3, 0), reference(2, 0)).await?;
    let e = minted(&d);
    evm(&mut store, 4, vec![e.clone()]).await?;
    let count = Arc::new(Mutex::new(0));
    let calls = count.clone();
    let id = d.deposit_id;
    let rpc = Server::new(move |_, r| {
        let p = &r["params"];
        let result = match r["method"].as_str().unwrap() {
            "eth_chainId" => json!("0xaa36a7"),
            "eth_getCode" => {
                assert_eq!(p[0], json!(contract()));
                json!("0x6000")
            }
            "eth_getBlockByNumber" => {
                assert_eq!(p[0], "0x3");
                json!({"number":"0x3","hash":reference(3,0).hash,"parentHash":reference(2,0).hash})
            }
            "eth_call" => {
                let input = p[0]
                    .get("input")
                    .or_else(|| p[0].get("data"))
                    .unwrap()
                    .as_str()
                    .unwrap();
                let selector = &input[2..10];
                if selector == hex::encode(&keccak256("decimals()")[..4]) {
                    assert_eq!(p[0]["to"], json!(contract()));
                    json!(format!("0x{:064x}", 8))
                } else if selector == hex::encode(&keccak256("getCurrentKmsContextId()")[..4]) {
                    assert_eq!(
                        p[0]["to"].as_str().unwrap().to_lowercase(),
                        "0xbe0e383937d564d7ff0bc3b46c51f0bf8d5c311a"
                    );
                    json!(format!("0x{:064x}", 7))
                } else {
                    assert_eq!(
                        p[1],
                        json!({"blockHash":reference(3,0).hash,"requireCanonical":true})
                    );
                    assert_eq!(p[0]["to"], json!(contract()));
                    *calls.lock().unwrap() += 1;
                    if selector == hex::encode(&keccak256("confidentialTotalSupply()")[..4]) {
                        json!(B256::repeat_byte(45))
                    } else if selector == hex::encode(&keccak256("consumed(bytes32)")[..4]) {
                        assert!(input.ends_with(&hex::encode(id)));
                        json!(format!("0x{:064x}", 1))
                    } else {
                        assert!(input.ends_with(&hex::encode(id)));
                        assert_eq!(
                            selector,
                            hex::encode(&keccak256("mintedAmounts(bytes32)")[..4])
                        );
                        json!(B256::repeat_byte(44))
                    }
                }
            }
            other => panic!("unexpected RPC {other}"),
        };
        json!({"jsonrpc":"2.0","id":r["id"],"result":result})
    });
    let prepared = Arc::new(Mutex::new(None::<Value>));
    let fixture = prepared.clone();
    let decrypted = Arc::new(Mutex::new(0));
    let dc = decrypted.clone();
    let sdk = Server::new(move |path, body| match path {
        "/decrypt-request" => {
            let p = permit(&body);
            *fixture.lock().unwrap() = Some(p.clone());
            p
        }
        "/decrypt" => {
            let p = fixture.lock().unwrap().clone().unwrap();
            for k in ["contract", "user", "handle", "sessionId"] {
                assert_eq!(body[k], p[k]);
            }
            let sig: Signature = body["signature"].as_str().unwrap().parse().unwrap();
            let digest = serde_json::from_value(p["digest"].clone()).unwrap();
            assert_eq!(
                json!(sig.recover_address_from_prehash(&digest).unwrap()),
                body["user"]
            );
            *dc.lock().unwrap() += 1;
            json!({"value":"100"})
        }
        _ => panic!("unexpected SDK path"),
    });
    let signer: PrivateKeySigner =
        "0000000000000000000000000000000000000000000000000000000000000001".parse()?;
    let client = CwzecClient::connect(&rpc.url, 11155111, contract()).await?;
    let sidecar = fhe::Client::new(&sdk.url)?;
    let actual = read_consumption(&client, &sidecar, &signer, id, reference(3, 0), &[e]).await?;
    assert_eq!(actual.actual_amount, Some(100));
    assert_eq!(*count.lock().unwrap(), 2);
    assert_eq!(*decrypted.lock().unwrap(), 1);
    let d = store
        .reconcile_bridge_deposit(id, d.version, &actual)
        .await?;
    assert_eq!(d.state, DepositState::Minted);
    drop(store);
    let mut store = db.connect().await?;
    store.migrate().await?;
    assert_eq!(
        store.bridge_deposit(id).await?.unwrap().state,
        DepositState::Minted
    );
    let auditor: PrivateKeySigner =
        "0000000000000000000000000000000000000000000000000000000000000002".parse()?;
    let supply_handle = client.supply_at(reference(3, 0)).await?;
    let supply = decrypt(&client, &sidecar, supply_handle, &auditor).await?;
    let r = store
        .bridge_accounting(&AccountingEvidence {
            supply_handle,
            supply,
            ..accounting(2, 3, 100)
        })
        .await?;
    assert_eq!(*count.lock().unwrap(), 3);
    assert_eq!(*decrypted.lock().unwrap(), 2);
    assert_eq!((r.deposit_liability, r.fee_reserve), (0, Some(0)));
    db.cleanup().await
}
