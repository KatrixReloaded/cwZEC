use super::*;
use alloy::{
    primitives::{Bytes, U256},
    providers::ProviderBuilder,
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

mod catchup;
mod concurrency;
mod log_chunks;
mod read_retries;

fn contract() -> Address {
    Address::repeat_byte(42)
}
fn hash(n: u64) -> B256 {
    B256::from(U256::from(n + 1))
}
fn reference(n: u64) -> BlockRef {
    BlockRef {
        number: n,
        hash: hash(n),
    }
}
fn log<E: SolEvent>(event: E) -> Log {
    Log {
        inner: alloy::primitives::Log {
            address: contract(),
            data: event.encode_log_data(),
        },
        block_hash: Some(hash(2)),
        block_number: Some(2),
        transaction_hash: Some(hash(50)),
        transaction_index: Some(0),
        log_index: Some(0),
        ..Default::default()
    }
}
fn minted() -> Log {
    log(DepositMinted {
        depositId: hash(8),
        to: contract(),
        amount: hash(9),
    })
}

#[test]
fn real_lifecycle_abi_and_metadata_validation() {
    let cases = [
        (
            minted(),
            Event::DepositMinted {
                deposit_id: hash(8),
                to: contract(),
                amount: hash(9),
            },
        ),
        (
            log(DepositVoided { depositId: hash(8) }),
            Event::DepositVoided {
                deposit_id: hash(8),
            },
        ),
        (
            log(RedeemRequested {
                redeemId: U256::from(3),
                user: contract(),
                amount: hash(9),
                destCt: Bytes::from_static(b"opaque"),
            }),
            Event::RedeemRequested {
                redeem_id: U256::from(3),
                user: contract(),
                amount: hash(9),
                dest_ct: Bytes::from_static(b"opaque"),
            },
        ),
        (
            log(RedeemFulfilled {
                redeemId: U256::from(3),
                zcashTxid: hash(9),
            }),
            Event::RedeemFulfilled {
                redeem_id: U256::from(3),
                zcash_txid: hash(9),
            },
        ),
        (
            log(RedeemRecredited {
                redeemId: U256::from(3),
                user: contract(),
                amount: hash(9),
            }),
            Event::RedeemRecredited {
                redeem_id: U256::from(3),
                user: contract(),
                amount: hash(9),
            },
        ),
    ];
    for (entry, event) in cases {
        assert!(decode_log(&entry, contract(), 1, 2).unwrap().event == event);
    }
    let mut bad = minted();
    bad.removed = true;
    assert!(decode_log(&bad, contract(), 1, 2).is_err());
    bad = minted();
    bad.inner.address = Address::ZERO;
    assert!(decode_log(&bad, contract(), 1, 2).is_err());
    bad = minted();
    bad.block_hash = None;
    assert!(decode_log(&bad, contract(), 1, 2).is_err());
    assert!(decode_log(&minted(), contract(), 2, 3).is_err());
    assert!(decode_log(&minted(), contract(), 0, 1).is_err());
    bad = minted();
    bad.inner.data = alloy::primitives::LogData::new_unchecked(
        vec![DepositMinted::SIGNATURE_HASH],
        Bytes::new(),
    );
    assert!(decode_log(&bad, contract(), 1, 2).is_err());
}

struct Fixture {
    watcher: Watcher,
    requests: Arc<Mutex<Vec<Value>>>,
    stop: Arc<AtomicBool>,
    address: std::net::SocketAddr,
    thread: Option<thread::JoinHandle<()>>,
}
impl Fixture {
    fn new(reply: impl Fn(&Value) -> Value + Send + Sync + 'static) -> Self {
        Self::with_response(
            move |request| json!({"jsonrpc":"2.0", "id":request["id"], "result":reply(request)}),
        )
    }
    fn with_response(reply: impl Fn(&Value) -> Value + Send + Sync + 'static) -> Self {
        // Binding failures are test failures: do not silently skip fixture coverage.
        let listener = TcpListener::bind("127.0.0.1:0").expect("HTTP fixture bind");
        let address = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (stopping, seen) = (stop.clone(), requests.clone());
        let reply = Arc::new(reply);
        let thread = thread::spawn(move || {
            let mut handlers = vec![];
            for connection in listener.incoming() {
                if stopping.load(Ordering::Relaxed) {
                    break;
                }
                let mut socket = connection.unwrap();
                let reply = reply.clone();
                let seen = seen.clone();
                handlers.push(thread::spawn(move || {
                socket
                    .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                    .unwrap();
                let mut reader = BufReader::new(&mut socket);
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap() == 0 {
                        return;
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
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let request: Value = serde_json::from_slice(&body).unwrap();
                seen.lock().unwrap().push(request.clone());
                let body = reply(&request).to_string();
                // A failed scan cancels sibling requests; their sockets may already be closed.
                let _ = write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
                }));
            }
            for handler in handlers {
                handler.join().unwrap();
            }
        });
        let provider = ProviderBuilder::new()
            .disable_recommended_fillers()
            .connect_http(format!("http://{address}").parse().unwrap())
            .erased();
        Self {
            watcher: Watcher {
                provider,
                chain_id: 1,
                contract: contract(),
            },
            requests,
            stop,
            address,
            thread: Some(thread),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(self.address);
        self.thread.take().unwrap().join().unwrap();
    }
}
fn normal(request: &Value) -> Value {
    match request["method"].as_str().unwrap() {
        "eth_chainId" => json!("0x1"),
        "eth_getBlockByNumber" => {
            let tag = request["params"][0].as_str().unwrap();
            let n = if tag == "latest" {
                2
            } else {
                assert!(tag.starts_with("0x"), "unsupported block tag: {tag}");
                u64::from_str_radix(&tag[2..], 16).unwrap()
            };
            json!({"number":format!("{n:#x}"), "hash":hash(n), "parentHash":if n == 0 { B256::ZERO } else { hash(n - 1) }})
        }
        "eth_getLogs" => json!([]),
        "eth_getTransactionReceipt" => Value::Null,
        _ => panic!("unexpected fixture method"),
    }
}
fn receipt(success: bool) -> Value {
    json!({"transactionHash":hash(50), "transactionIndex":"0x0", "blockHash":hash(2), "blockNumber":"0x2",
        "from":Address::repeat_byte(7), "to":contract(), "gasUsed":"0x1", "effectiveGasPrice":"0x1",
        "cumulativeGasUsed":"0x1", "status":if success {"0x1"} else {"0x0"}, "logs":[],
        "logsBloom":format!("0x{}", "00".repeat(256)), "type":"0x2"})
}

#[tokio::test]
async fn duplicate_logs_are_deduplicated_and_filter_is_bounded() {
    let fixture = Fixture::new(|r| match r["method"].as_str().unwrap() {
        "eth_getLogs" => json!([minted(), minted()]),
        "eth_getTransactionReceipt" => receipt(true),
        _ => normal(r),
    });
    let batch = fixture
        .watcher
        .scan(reference(1), 2, &[hash(50), hash(50)])
        .await
        .unwrap();
    assert_eq!(batch.events.len(), 1);
    assert_eq!(batch.receipts.len(), 1);
    let requests = fixture.requests.lock().unwrap();
    let filter = &requests
        .iter()
        .find(|r| r["method"] == "eth_getLogs")
        .unwrap()["params"][0];
    assert_eq!(filter["fromBlock"], "0x2");
    assert_eq!(filter["toBlock"], "0x2");
    assert_eq!(filter["topics"][0].as_array().unwrap().len(), 5);
    assert!(filter["address"] == json!(contract()) || filter["address"] == json!([contract()]));
}

#[tokio::test]
async fn conflicting_duplicates_and_noncanonical_events_fail() {
    for mode in 0..3 {
        let fixture = Fixture::new(move |r| match r["method"].as_str().unwrap() {
            "eth_getLogs" => {
                let first = minted();
                let mut second = first.clone();
                if mode == 0 {
                    second.transaction_index = Some(1);
                    json!([first, second])
                } else if mode == 1 {
                    second.block_hash = Some(hash(99));
                    json!([second])
                } else {
                    json!([first])
                }
            }
            "eth_getTransactionReceipt" => receipt(false),
            _ => normal(r),
        });
        assert!(fixture.watcher.scan(reference(1), 2, &[]).await.is_err());
    }
}

#[tokio::test]
async fn null_receipt_is_unknown_and_reverted_receipt_is_observed() {
    let fixture = Fixture::new(normal);
    assert!(fixture.watcher.receipt(hash(50)).await.unwrap().is_none());
    let fixture = Fixture::new(|r| {
        if r["method"] == "eth_getTransactionReceipt" {
            receipt(false)
        } else {
            normal(r)
        }
    });
    assert!(
        !fixture
            .watcher
            .receipt(hash(50))
            .await
            .unwrap()
            .unwrap()
            .success
    );
}

#[tokio::test]
async fn receipt_hash_target_and_canonical_block_are_checked() {
    for key in ["transactionHash", "to", "blockHash"] {
        let fixture = Fixture::new(move |r| {
            if r["method"] == "eth_getTransactionReceipt" {
                let mut value = receipt(true);
                value[key] = if key == "to" {
                    json!(Address::ZERO)
                } else {
                    json!(hash(99))
                };
                value
            } else {
                normal(r)
            }
        });
        assert!(fixture.watcher.receipt(hash(50)).await.is_err());
    }
}

#[tokio::test]
async fn empty_scan_refreshes_receipts_without_logs() {
    let fixture = Fixture::new(|r| {
        if r["method"] == "eth_getTransactionReceipt" {
            receipt(false)
        } else {
            normal(r)
        }
    });
    let batch = fixture
        .watcher
        .scan(reference(2), 2, &[hash(50)])
        .await
        .unwrap();
    assert!(batch.events.is_empty());
    assert!(batch.blocks.is_empty());
    assert_eq!(batch.receipts.len(), 1);
    assert!(!batch.receipts[0].success);
    let requests = fixture.requests.lock().unwrap();
    assert!(!requests.iter().any(|r| r["method"] == "eth_getLogs"));
}

#[tokio::test]
async fn backfill_contains_every_consecutive_header() {
    let fixture = Fixture::new(normal);
    assert_eq!(
        fixture.watcher.latest().await.unwrap().reference,
        reference(2)
    );
    let batch = fixture.watcher.scan(reference(0), 128, &[]).await.unwrap();
    assert_eq!(batch.blocks.len(), 128);
    let mut previous = reference(0);
    for block in batch.blocks {
        assert_eq!(block.reference.number, previous.number + 1);
        assert_eq!(block.parent_hash, previous.hash);
        previous = block.reference;
    }
    assert_eq!(previous, reference(128));
    // The fixture rejects every tag except latest and numbered blocks.
    assert!(fixture.requests.lock().unwrap().iter().all(|r| {
        r["method"] != "eth_getBlockByNumber"
            || r["params"][0]
                .as_str()
                .is_some_and(|tag| tag == "latest" || tag.starts_with("0x"))
    }));
}

#[tokio::test]
async fn missing_tip_is_distinct_from_invalid_rpc_response() {
    let fixture = Fixture::new(|r| {
        if r["method"] == "eth_getBlockByNumber" && r["params"][0] == "0x3" {
            Value::Null
        } else {
            normal(r)
        }
    });
    assert_eq!(
        fixture.watcher.latest().await.unwrap().reference,
        reference(2)
    );
    assert!(fixture.watcher.optional_block(3).await.unwrap().is_none());
    assert!(fixture.watcher.block(3).await.is_err());
    let fixture = Fixture::new(|r| {
        if r["method"] == "eth_getBlockByNumber" {
            json!({"invalid":"response"})
        } else {
            normal(r)
        }
    });
    assert!(fixture.watcher.optional_block(3).await.is_err());
}

#[tokio::test]
async fn mid_request_reorg_and_broken_parent_are_rejected() {
    use std::sync::atomic::AtomicUsize;
    // Each anchor is checked again after logs and receipts have been fetched.
    for changed in [1, 2] {
        let reads = AtomicUsize::new(0);
        let fixture = Fixture::new(move |r| {
            let mut value = normal(r);
            if r["method"] == "eth_getBlockByNumber"
                && r["params"][0] == format!("{changed:#x}")
                && reads.fetch_add(1, Ordering::Relaxed) > 0
            {
                value["hash"] = json!(hash(99));
            }
            value
        });
        assert!(fixture.watcher.scan(reference(1), 2, &[]).await.is_err());
    }
    let fixture = Fixture::new(|r| {
        let mut value = normal(r);
        if r["method"] == "eth_getBlockByNumber" && r["params"][0] == "0x2" {
            value["parentHash"] = json!(hash(99));
        }
        value
    });
    assert!(fixture.watcher.scan(reference(1), 3, &[]).await.is_err());
}

#[tokio::test]
async fn invalid_scan_ranges_fail_before_any_rpc() {
    let provider = ProviderBuilder::new()
        .disable_recommended_fillers()
        .connect_http("http://127.0.0.1:1".parse().unwrap())
        .erased();
    let watcher = Watcher {
        provider,
        chain_id: 1,
        contract: contract(),
    };
    for (previous, end) in [(reference(1), 0), (reference(1), SCAN_BLOCKS + 2)] {
        let error = watcher.scan(previous, end, &[]).await.err().unwrap();
        assert_eq!(error.to_string(), "invalid scan range");
    }
}

#[tokio::test]
async fn log_and_unique_receipt_limits_fail_closed() {
    let fixture = Fixture::new(|r| {
        if r["method"] == "eth_getLogs" {
            json!(vec![minted(); MAX_LOGS + 1])
        } else {
            normal(r)
        }
    });
    assert!(fixture.watcher.scan(reference(1), 2, &[]).await.is_err());
    let fixture = Fixture::new(normal);
    let tracked: Vec<_> = (0..=MAX_RECEIPTS as u64).map(hash).collect();
    assert!(
        fixture
            .watcher
            .scan(reference(1), 2, &tracked)
            .await
            .is_err()
    );
}
