use super::*;
use crate::store::zama_test_support::*;
use alloy::{
    primitives::{B256, keccak256},
    rpc::types::Log,
    sol_types::SolEvent,
};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};
use zama::contract::bindings::DepositVoided;

struct Chain {
    latest: u64,
    rejected_tags: u64,
    fork_at: u64,
    branch: u64,
    emit: bool,
    broadcasts: Vec<String>,
    lose_ack: bool,
    nonce: u64,
}
impl Default for Chain {
    fn default() -> Self {
        Self {
            latest: 3,
            rejected_tags: 0,
            fork_at: u64::MAX,
            branch: 0,
            emit: false,
            broadcasts: vec![],
            lose_ack: false,
            nonce: 0,
        }
    }
}
impl Chain {
    fn reference(&self, n: u64) -> zama::watcher::BlockRef {
        reference(n, if n >= self.fork_at { self.branch } else { 0 })
    }
    fn reply(&mut self, r: &Value) -> Option<Value> {
        Some(match r["method"].as_str().unwrap() {
            "eth_chainId" => json!("0x1"),
            "eth_getCode" => json!("0x6000"),
            "eth_call" => json!(format!("0x{:064x}", 8)),
            "eth_getTransactionCount" => json!(format!("{:#x}", self.nonce)),
            "eth_getBlockByNumber" => {
                let tag = r["params"][0].as_str().unwrap();
                let n = match tag {
                    "latest" => self.latest,
                    _ => u64::from_str_radix(tag.trim_start_matches("0x"), 16).unwrap(),
                };
                if n > self.latest {
                    return Some(Value::Null);
                }
                json!({"number":format!("{n:#x}"),"hash":self.reference(n).hash,"parentHash":if n>0 {self.reference(n-1).hash} else {B256::ZERO}})
            }
            "eth_getLogs" => {
                let filter = &r["params"][0];
                let start = u64::from_str_radix(
                    filter["fromBlock"]
                        .as_str()
                        .unwrap()
                        .trim_start_matches("0x"),
                    16,
                )
                .unwrap();
                let end = u64::from_str_radix(
                    filter["toBlock"].as_str().unwrap().trim_start_matches("0x"),
                    16,
                )
                .unwrap();
                if self.emit && start <= 3 && end >= 3 {
                    let entry = Log {
                        inner: alloy::primitives::Log {
                            address: contract(),
                            data: DepositVoided {
                                depositId: self.reference(3).hash,
                            }
                            .encode_log_data(),
                        },
                        block_hash: Some(self.reference(3).hash),
                        block_number: Some(3),
                        transaction_hash: Some(self.reference(50).hash),
                        transaction_index: Some(0),
                        log_index: Some(0),
                        ..Default::default()
                    };
                    json!([entry.clone(), entry])
                } else {
                    json!([])
                }
            }
            "eth_getTransactionReceipt" => {
                if self.emit && r["params"][0] == json!(self.reference(50).hash) {
                    json!({"transactionHash":self.reference(50).hash,"transactionIndex":"0x0","blockHash":self.reference(3).hash,"blockNumber":"0x3",
                        "from":Address::repeat_byte(7),"to":contract(),"gasUsed":"0x1","effectiveGasPrice":"0x1","cumulativeGasUsed":"0x1",
                        "status":"0x1","logs":[],"logsBloom":format!("0x{}","00".repeat(256)),"type":"0x2"})
                } else {
                    Value::Null
                }
            }
            "eth_sendRawTransaction" => {
                let raw = r["params"][0].as_str().unwrap().to_owned();
                self.broadcasts.push(raw.clone());
                if self.lose_ack {
                    return None;
                }
                json!(keccak256(
                    hex::decode(raw.trim_start_matches("0x")).unwrap()
                ))
            }
            method => panic!("unexpected fixture RPC {method}"),
        })
    }
}
struct Rpc {
    state: Arc<Mutex<Chain>>,
    address: std::net::SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Rpc {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind controlled RPC");
        let address = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(Chain::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (shared, stopping) = (state.clone(), stop.clone());
        let thread = thread::spawn(move || {
            for connection in listener.incoming() {
                if stopping.load(Ordering::Relaxed) {
                    break;
                }
                let mut socket = connection.unwrap();
                socket
                    .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                    .unwrap();
                let mut reader = BufReader::new(&mut socket);
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':') {
                        if name.eq_ignore_ascii_case("content-length") {
                            length = value.trim().parse().unwrap();
                        }
                    }
                }
                if length == 0 {
                    continue;
                }
                let mut body = vec![0; length];
                if reader.read_exact(&mut body).is_err() {
                    continue;
                }
                let request: Value = serde_json::from_slice(&body).unwrap();
                let response = if request["method"] == "eth_getBlockByNumber" && request["params"][0] == "finalized" {
                    shared.lock().unwrap().rejected_tags += 1;
                    json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32602,"message":"unsupported block tag"}})
                } else {
                    let Some(result) = shared.lock().unwrap().reply(&request) else { continue; };
                    json!({"jsonrpc":"2.0","id":request["id"],"result":result})
                }.to_string();
                let _ = write!(
                    socket,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response.len(),
                    response
                );
            }
        });
        Self {
            state,
            address,
            stop,
            thread: Some(thread),
        }
    }
    fn endpoint(&self) -> String {
        format!("http://{}", self.address)
    }
    async fn service(&self, db: &TestDb) -> Result<ZamaService> {
        let client = CwzecClient::connect(&self.endpoint(), 1, contract()).await?;
        let watcher = Watcher::new(&client);
        let store = initialized(db).await?;
        Ok(ZamaService {
            store,
            client,
            watcher,
        })
    }
}
impl Drop for Rpc {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(self.address);
        self.thread.take().unwrap().join().unwrap();
    }
}

#[tokio::test]
async fn zama_restart_backfill_and_reorg_replays_observations_preserving_signed_identity()
-> Result<()> {
    let db = TestDb::new().await?;
    let rpc = Rpc::new();
    rpc.state.lock().unwrap().emit = true;
    let mut service = rpc.service(&db).await?;
    let signed = signed(0, 10);
    service
        .store
        .initialize_zama_lane(signed.sender(), 0, 0)
        .await?;
    service.store.save_zama_signed("durable", &signed).await?;
    assert_eq!(service.scan_once().await?.tip, reference(3, 0));
    assert_eq!(service.observation_counts().await?, (1, 0, 0));
    drop(service);
    let mut service = rpc.service(&db).await?;
    assert_eq!(service.scan_once().await?.tip, reference(3, 0));
    assert_eq!(service.observation_counts().await?, (1, 0, 0));
    {
        let mut chain = rpc.state.lock().unwrap();
        chain.fork_at = 3;
        chain.branch = 1;
        chain.latest = 4;
    }
    assert_eq!(service.scan_once().await?.tip, reference(4, 1));
    let events = service.events().await?;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].block, reference(3, 1));
    assert_eq!(fact_counts(&service.store).await?, (2, 2, 1));
    let restored = service.transaction("durable").await?.unwrap();
    assert_eq!(restored.signed.hash(), signed.hash());
    assert_eq!(restored.signed.raw(), signed.raw());
    assert_eq!(service.store.zama_next_nonce(signed.sender()).await?, 1);
    db.cleanup().await
}

#[tokio::test]
async fn zama_lost_broadcast_ack_restart_rebroadcasts_exact_persisted_bytes() -> Result<()> {
    let db = TestDb::new().await?;
    let rpc = Rpc::new();
    let mut service = rpc.service(&db).await?;
    let signed = signed(0, 10);
    service
        .store
        .initialize_zama_lane(signed.sender(), 0, 0)
        .await?;
    service.store.save_zama_signed("ambiguous", &signed).await?;
    rpc.state.lock().unwrap().lose_ack = true;
    assert_eq!(
        service.recover_operator("ambiguous").await?.state,
        TransactionState::Unresolved
    );
    drop(service);
    rpc.state.lock().unwrap().lose_ack = false;
    let mut service = rpc.service(&db).await?;
    assert_eq!(
        service.recover_operator("ambiguous").await?.state,
        TransactionState::Pending
    );
    {
        let chain = rpc.state.lock().unwrap();
        assert!(chain.broadcasts.len() >= 2);
        for raw in &chain.broadcasts {
            assert_eq!(raw, &format!("0x{}", hex::encode(signed.raw())));
        }
    }
    assert_eq!(service.store.zama_next_nonce(signed.sender()).await?, 1);
    rpc.state.lock().unwrap().nonce = 1;
    assert!(service.recover_operator("ambiguous").await.is_err());
    assert_eq!(
        service.transaction("ambiguous").await?.unwrap().state,
        TransactionState::Unresolved
    );
    db.cleanup().await
}

#[tokio::test]
async fn zama_reorg_beyond_retained_history_halts_instead_of_guessing() -> Result<()> {
    let db = TestDb::new().await?;
    let rpc = Rpc::new();
    let mut service = rpc.service(&db).await?;
    rpc.state.lock().unwrap().latest = 260;
    assert_eq!(service.scan_once().await?.tip.number, 260);
    {
        let mut chain = rpc.state.lock().unwrap();
        chain.fork_at = 2;
        chain.branch = 1;
    }
    assert!(service.scan_once().await.is_err());
    assert!(service.cursor().await?.halted);
    assert_eq!(
        halt_reason(&service.store).await?.as_deref(),
        Some("history_exhausted")
    );
    assert_eq!(service.cursor().await?.tip, reference(260, 0));
    db.cleanup().await
}

#[tokio::test]
async fn zama_startup_accepts_latest_predecessor_without_finalized_rpc() -> Result<()> {
    let db = TestDb::new().await?;
    let rpc = Rpc::new();
    rpc.state.lock().unwrap().latest = 1;
    let mut service = ZamaService::open(&db.url(), &rpc.endpoint(), 1, contract(), 2).await?;
    assert_eq!(service.cursor().await?.tip, reference(1, 0));
    assert_eq!(service.scan_once().await?.tip, reference(1, 0));
    assert_eq!(rpc.state.lock().unwrap().rejected_tags, 0);
    rpc.state.lock().unwrap().latest = 3;
    assert_eq!(service.scan_once().await?.tip, reference(3, 0));
    db.cleanup().await
}

#[tokio::test]
async fn zama_backfill_is_bounded_and_resumes_durably() -> Result<()> {
    let db = TestDb::new().await?;
    let rpc = Rpc::new();
    let mut service = rpc.service(&db).await?;
    let end = SCAN_BLOCKS * 2 + 5;
    rpc.state.lock().unwrap().latest = end;
    assert_eq!(
        service.scan_once().await?.tip,
        reference(1 + SCAN_BLOCKS, 0)
    );
    drop(service);
    let mut service = rpc.service(&db).await?;
    assert_eq!(service.cursor().await?.tip, reference(1 + SCAN_BLOCKS, 0));
    assert_eq!(
        service.scan_once().await?.tip,
        reference(1 + SCAN_BLOCKS * 2, 0)
    );
    assert_eq!(service.scan_once().await?.tip, reference(end, 0));
    assert_eq!(rpc.state.lock().unwrap().rejected_tags, 0);
    db.cleanup().await
}

#[tokio::test]
async fn zama_shortened_head_with_missing_saved_tip_rewinds_and_reincludes() -> Result<()> {
    let db = TestDb::new().await?;
    let rpc = Rpc::new();
    {
        let mut chain = rpc.state.lock().unwrap();
        chain.emit = true;
        chain.latest = 4;
    }
    let mut service = rpc.service(&db).await?;
    assert_eq!(service.scan_once().await?.tip, reference(4, 0));
    assert_eq!(service.observation_counts().await?, (1, 1, 1));
    rpc.state.lock().unwrap().latest = 2;
    assert_eq!(service.scan_once().await?.tip, reference(2, 0));
    assert!(service.events().await?.is_empty());
    assert!(!service.cursor().await?.halted);
    {
        let mut chain = rpc.state.lock().unwrap();
        chain.latest = 4;
        chain.fork_at = 3;
        chain.branch = 1;
    }
    assert_eq!(service.scan_once().await?.tip, reference(4, 1));
    assert_eq!(service.observation_counts().await?, (1, 1, 1));
    assert_eq!(service.events().await?[0].block, reference(3, 1));
    assert_eq!(fact_counts(&service.store).await?, (2, 2, 0));
    db.cleanup().await
}
