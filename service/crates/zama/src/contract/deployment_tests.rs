//! Controlled HTTP evidence; no deployment or external endpoint is used.
use super::{CwzecClient, bindings::*};
use crate::watcher::BlockRef;
use alloy::{
    primitives::{Address, B256, Bytes, U256, keccak256},
    sol_types::{SolCall, SolValue},
};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
#[derive(Clone, Copy, Debug, Default)]
enum Fault {
    #[default]
    None,
    Initcode,
    Receipt,
    Address,
    Confirmation,
    Service,
    Auditor,
    Minter,
    Admin,
    Supply,
    NextId,
    Rpc,
    Code,
}
struct Rpc {
    endpoint: String,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
fn address(n: u8) -> Address {
    Address::repeat_byte(n)
}
fn block() -> BlockRef {
    BlockRef {
        number: 10,
        hash: B256::repeat_byte(10),
    }
}
fn encoded<T: SolValue>(value: T) -> Value {
    json!(format!("0x{}", alloy::hex::encode(value.abi_encode())))
}
fn response(value: &Value, fault: Fault) -> Value {
    match value["method"].as_str().unwrap() {
        "eth_chainId" => json!("0xaa36a7"),
        "eth_getCode" => {
            if value["params"][1].is_object() {
                assert_eq!(value["params"][1]["blockHash"], json!(block().hash));
                assert_eq!(value["params"][1]["requireCanonical"], true);
            }
            json!(if matches!(fault, Fault::Code) {
                "0x6001"
            } else {
                "0x6000"
            })
        }
        "eth_getTransactionByHash" => {
            json!({"type":"0x2","chainId":"0xaa36a7","nonce":"0x0","gas":"0x100000","maxFeePerGas":"0x2","maxPriorityFeePerGas":"0x1","to":null,"value":"0x0","input":if matches!(fault,Fault::Initcode){"0x6001"}else{"0x6000"},"accessList":[],"v":"0x0","yParity":"0x0","r":"0x1","s":"0x1","hash":B256::repeat_byte(20),"from":address(1),"blockNumber":"0xa","blockHash":block().hash,"transactionIndex":"0x0"})
        }
        "eth_getTransactionReceipt" => {
            json!({"type":"0x2","transactionHash":B256::repeat_byte(20),"transactionIndex":"0x0","blockHash":block().hash,"blockNumber":"0xa","from":address(1),"to":null,"cumulativeGasUsed":"0x100","gasUsed":"0x100","effectiveGasPrice":"0x1","contractAddress":if matches!(fault,Fault::Address){address(99)}else{address(42)},"logs":[],"logsBloom":format!("0x{}", "00".repeat(256)),"status":if matches!(fault,Fault::Receipt){"0x0"}else{"0x1"}})
        }
        "eth_getBlockByNumber" => {
            json!({"number":if value["params"][0] == "latest" && !matches!(fault,Fault::Confirmation){"0xb"}else{"0xa"},"hash":block().hash,"parentHash":B256::repeat_byte(9)})
        }
        "eth_call" => {
            let data = value["params"][0]["input"]
                .as_str()
                .or_else(|| value["params"][0]["data"].as_str())
                .unwrap();
            let bytes = alloy::hex::decode(data).unwrap();
            let selector = &bytes[..4];
            if selector == decimalsCall::SELECTOR {
                return encoded(U256::from(8));
            }
            assert_eq!(value["params"][1]["blockHash"], json!(block().hash));
            assert_eq!(value["params"][1]["requireCanonical"], true);
            if selector == MINTER_ROLECall::SELECTOR {
                encoded(B256::repeat_byte(5))
            } else if selector == DEFAULT_ADMIN_ROLECall::SELECTOR {
                encoded(B256::ZERO)
            } else if selector == serviceCall::SELECTOR {
                encoded(if matches!(fault, Fault::Service) {
                    address(99)
                } else {
                    address(2)
                })
            } else if selector == auditorCall::SELECTOR {
                encoded(if matches!(fault, Fault::Auditor) {
                    address(99)
                } else {
                    address(3)
                })
            } else if selector == hasRoleCall::SELECTOR {
                let call = hasRoleCall::abi_decode(&bytes).unwrap();
                assert_eq!(
                    call.account,
                    if call.role.is_zero() {
                        address(1)
                    } else {
                        address(2)
                    }
                );
                encoded(
                    !(if call.role.is_zero() {
                        matches!(fault, Fault::Admin)
                    } else {
                        matches!(fault, Fault::Minter)
                    }),
                )
            } else if selector == confidentialTotalSupplyCall::SELECTOR {
                encoded(if matches!(fault, Fault::Supply) {
                    B256::repeat_byte(1)
                } else {
                    B256::ZERO
                })
            } else if selector == nextRedeemIdCall::SELECTOR {
                encoded(U256::from(if matches!(fault, Fault::NextId) {
                    2
                } else {
                    1
                }))
            } else {
                panic!("unexpected call selector");
            }
        }
        other => panic!("unexpected RPC {other}"),
    }
}
impl Rpc {
    fn new(fault: Fault) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let worker = std::thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                let (mut stream, _) = match listener.accept() {
                    Ok(value) => value,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(1));
                        continue;
                    }
                    Err(e) => panic!("{e}"),
                };
                // BSD sockets may inherit the listener's nonblocking mode.
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buffer = [0; 8192];
                let value: Value = loop {
                    let n = stream.read(&mut buffer).unwrap();
                    assert_ne!(n, 0);
                    request.extend_from_slice(&buffer[..n]);
                    if let Some(split) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&request[..split]);
                        let size: usize = headers
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse().unwrap())
                            })
                            .unwrap();
                        if request.len() >= split + 4 + size {
                            break serde_json::from_slice(&request[split + 4..split + 4 + size])
                                .unwrap();
                        }
                    }
                };
                let reply = if matches!(fault,Fault::Rpc) && value["method"] == "eth_getBlockByNumber" {
                    json!({"jsonrpc":"2.0","id":value["id"],"error":{"code":-32000,"message":"controlled failure"}})
                } else { json!({"jsonrpc":"2.0","id":value["id"],"result":response(&value,fault)}) }.to_string();
                write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",reply.len(),reply).unwrap();
            }
        });
        Self {
            endpoint,
            stop,
            worker: Some(worker),
        }
    }
    async fn client(&self) -> CwzecClient {
        CwzecClient::connect(&self.endpoint, 11155111, address(42))
            .await
            .unwrap()
    }
}
impl Drop for Rpc {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Err(panic) = self.worker.take().unwrap().join() {
            if !std::thread::panicking() {
                std::panic::resume_unwind(panic);
            }
        }
    }
}
#[tokio::test]
async fn deployment_requires_exact_creation_success_two_confirmations_roles_and_pristine_state() {
    for fault in [
        Fault::None,
        Fault::Initcode,
        Fault::Receipt,
        Fault::Address,
        Fault::Confirmation,
        Fault::Service,
        Fault::Auditor,
        Fault::Minter,
        Fault::Admin,
        Fault::Supply,
        Fault::NextId,
        Fault::Rpc,
    ] {
        let rpc = Rpc::new(fault);
        let result = rpc
            .client()
            .await
            .verify_deployment(
                B256::repeat_byte(20),
                &Bytes::from_static(&[0x60, 0]),
                address(2),
                address(3),
            )
            .await;
        assert_eq!(
            result.is_ok(),
            matches!(fault, Fault::None),
            "fault {fault:?}: {}",
            result
                .as_ref()
                .err()
                .map(ToString::to_string)
                .unwrap_or_default()
        );
        if let Ok(e) = result {
            assert_eq!(e.block, block());
            assert_eq!(e.transaction, B256::repeat_byte(20));
            assert_eq!(e.runtime_hash, keccak256([0x60, 0]));
            assert_eq!(e.service, address(2));
            assert_eq!(e.auditor, address(3));
        }
    }
}
#[tokio::test]
async fn pristine_recheck_rejects_changed_code_state_roles_and_rpc_failure() {
    for fault in [
        Fault::None,
        Fault::Code,
        Fault::Supply,
        Fault::NextId,
        Fault::Service,
        Fault::Auditor,
        Fault::Rpc,
    ] {
        let rpc = Rpc::new(fault);
        let result = rpc
            .client()
            .await
            .verify_pristine_supply(block(), keccak256([0x60, 0]), address(2), address(3))
            .await;
        assert_eq!(
            result.is_ok(),
            matches!(fault, Fault::None),
            "fault {fault:?}"
        );
    }
}

#[test]
fn controlled_deployment_rpc_fixtures_decode() {
    let tx: alloy::rpc::types::Transaction = serde_json::from_value(response(
        &json!({"method":"eth_getTransactionByHash"}),
        Fault::None,
    ))
    .unwrap();
    use alloy::consensus::Transaction;
    assert!(tx.is_create());
    assert_eq!(tx.input(), &Bytes::from_static(&[0x60, 0]));
    let receipt: alloy::rpc::types::TransactionReceipt = serde_json::from_value(response(
        &json!({"method":"eth_getTransactionReceipt"}),
        Fault::None,
    ))
    .unwrap();
    assert!(receipt.status());
    assert_eq!(receipt.contract_address, Some(address(42)));
}
