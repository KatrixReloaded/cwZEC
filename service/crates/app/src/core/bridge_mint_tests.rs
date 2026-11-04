use super::*;
use crate::store::{
    bridge_tests::{accounting, funded},
    zama_test_support::{TestDb, batch, contract, reference},
};
use alloy::primitives::{B256, keccak256};
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

struct Server {
    url: String,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Server {
    fn new(handler: impl Fn(&str, Value) -> Value + Send + 'static) -> Self {
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
                let response = handler(&path, serde_json::from_slice(&body).unwrap()).to_string();
                let _ = write!(
                    stream,
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
fn signer() -> PrivateKeySigner {
    "0000000000000000000000000000000000000000000000000000000000000001"
        .parse()
        .unwrap()
}
fn fees() -> MintFees {
    MintFees {
        gas_limit: 500_000,
        max_fee_per_gas: 2,
        max_priority_fee_per_gas: 1,
    }
}
struct Fixture {
    rpc: Server,
    sdk: Server,
    broadcasts: Arc<Mutex<Vec<String>>>,
    encryptions: Arc<Mutex<u64>>,
    head: Arc<Mutex<u64>>,
}
impl Fixture {
    fn new() -> Self {
        Self::with_heads(3, 3)
    }
    fn with_heads(initial: u64, after_encryption: u64) -> Self {
        Self::with_chain(initial, after_encryption, false)
    }
    fn with_chain(initial: u64, after_encryption: u64, reorg_after_encryption: bool) -> Self {
        let broadcasts = Arc::new(Mutex::new(Vec::new()));
        let sent = broadcasts.clone();
        let head = Arc::new(Mutex::new(initial));
        let latest = head.clone();
        let reorg = Arc::new(AtomicBool::new(false));
        let rpc_reorg = reorg.clone();
        let rpc = Server::new(move |_, request| {
            let result = match request["method"].as_str().unwrap() {
                "eth_chainId" => json!("0xaa36a7"),
                "eth_getCode" => json!("0x6000"),
                "eth_getTransactionCount" => json!("0x0"),
                "eth_getTransactionReceipt" => Value::Null,
                "eth_getLogs" => json!([]),
                "eth_getBlockByNumber" => {
                    let tag = request["params"][0].as_str().unwrap();
                    let n = if tag == "latest" {
                        *latest.lock().unwrap()
                    } else {
                        u64::from_str_radix(tag.trim_start_matches("0x"), 16).unwrap()
                    };
                    let branch = if n >= 3 && rpc_reorg.load(Ordering::Relaxed) {
                        1
                    } else {
                        0
                    };
                    json!({"number":format!("{n:#x}"),"hash":reference(n,branch).hash,"parentHash":reference(n.saturating_sub(1),0).hash})
                }
                "eth_call" => {
                    let call = &request["params"][0];
                    let data = call
                        .get("input")
                        .or_else(|| call.get("data"))
                        .unwrap()
                        .as_str()
                        .unwrap();
                    let selector = &data[2..10];
                    let value = if selector == hex::encode(&keccak256("decimals()")[..4]) {
                        8
                    } else if selector == hex::encode(&keccak256("consumed(bytes32)")[..4]) {
                        0
                    } else {
                        1
                    };
                    json!(format!("0x{value:064x}"))
                }
                "eth_sendRawTransaction" => {
                    let raw = request["params"][0].as_str().unwrap().to_owned();
                    sent.lock().unwrap().push(raw.clone());
                    // Ambiguous acknowledgement: the journal must still own the nonce.
                    return json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32000,"message":"uncertain submission"}});
                }
                method => panic!("unexpected controlled RPC {method}"),
            };
            json!({"jsonrpc":"2.0","id":request["id"],"result":result})
        });
        let encryptions = Arc::new(Mutex::new(0));
        let encrypted = encryptions.clone();
        let encryption_head = head.clone();
        let sdk = Server::new(move |path, body| {
            assert_eq!(path, "/encrypt");
            assert_eq!(body["contract"], json!(contract()));
            assert_eq!(body["user"], json!(signer().address()));
            assert_eq!(body["value"], "100");
            *encrypted.lock().unwrap() += 1;
            *encryption_head.lock().unwrap() = after_encryption;
            reorg.store(reorg_after_encryption, Ordering::Relaxed);
            json!({"handle":B256::repeat_byte(44),"inputProof":"0x0102"})
        });
        Self {
            rpc,
            sdk,
            broadcasts,
            encryptions,
            head,
        }
    }
    async fn minter(&self, db: &TestDb) -> Result<BridgeMinter> {
        BridgeMinter::new(
            db.connect().await?,
            ZamaService::open(&db.url(), &self.rpc.url, 11155111, contract(), 2).await?,
            fhe::Client::new(&self.sdk.url)?,
        )
        .await
    }
}

#[tokio::test]
async fn bridge_mint_reorg_during_encryption_prevents_signing_and_broadcast() -> Result<()> {
    let db = TestDb::new().await?;
    let (mut store, deposit) = funded(&db).await?;
    let report = store.bridge_accounting(&accounting(2, 2, 0)).await?;
    let fixture = Fixture::with_chain(3, 4, true);
    let mut minter = fixture.minter(&db).await?;
    let error = minter
        .mint_once(report.id, &signer(), fees())
        .await
        .err()
        .expect("reorg must block signing");
    assert!(error.to_string().contains("not canonical"));
    assert!(
        store
            .zama_transaction(&deposit.operation_id)
            .await?
            .is_none()
    );
    assert!(fixture.broadcasts.lock().unwrap().is_empty());
    assert_eq!(*fixture.encryptions.lock().unwrap(), 1);
    let pending = store.bridge_deposit(deposit.deposit_id).await?.unwrap();
    assert_eq!(pending.state, DepositState::MintPending);
    assert!(pending.claimed);
    db.cleanup().await
}

#[tokio::test]
async fn bridge_mint_head_growth_preserves_accounting_through_signing() -> Result<()> {
    let db = TestDb::new().await?;
    let (mut store, deposit) = funded(&db).await?;
    let report = store.bridge_accounting(&accounting(2, 2, 0)).await?;
    // Accounting used tip 3. Blocks arrive before claiming and during encryption.
    let fixture = Fixture::with_heads(4, 5);
    let mut minter = fixture.minter(&db).await?;
    let saved = minter
        .mint_once(report.id, &signer(), fees())
        .await?
        .unwrap();
    assert_eq!(
        saved.signed.mint_identity(),
        Some((deposit.recipient.unwrap(), deposit.deposit_id))
    );
    assert_eq!(minter.zama.cursor().await?.tip.number, 5);
    assert_eq!(*fixture.encryptions.lock().unwrap(), 1);
    assert_eq!(fixture.broadcasts.lock().unwrap().len(), 1);
    let recovered = minter.mint_once(-1, &signer(), fees()).await?.unwrap();
    assert_eq!(recovered.signed.raw(), saved.signed.raw());
    assert_eq!(*fixture.encryptions.lock().unwrap(), 1);
    db.cleanup().await
}

#[tokio::test]
async fn bridge_mint_duplicate_restart_recovers_journal_without_encrypting_again() -> Result<()> {
    let db = TestDb::new().await?;
    let (mut store, deposit) = funded(&db).await?;
    let report = store.bridge_accounting(&accounting(2, 2, 0)).await?;
    let fixture = Fixture::new();
    let mut minter = fixture.minter(&db).await?;
    let first = minter
        .mint_once(report.id, &signer(), fees())
        .await?
        .unwrap();
    assert_eq!(
        first.signed.mint_identity(),
        Some((deposit.recipient.unwrap(), deposit.deposit_id))
    );
    assert_eq!(first.signed.nonce(), 0);
    drop(minter);
    let mut minter = fixture.minter(&db).await?;
    // Even a stale caller checkpoint cannot cause re-encryption during recovery.
    let restored = minter.mint_once(-1, &signer(), fees()).await?.unwrap();
    let duplicate = minter
        .mint_once(report.id, &signer(), fees())
        .await?
        .unwrap();
    assert_eq!(restored.signed.raw(), first.signed.raw());
    assert_eq!(duplicate.signed.hash(), first.signed.hash());
    assert_eq!(store.zama_next_nonce(signer().address()).await?, 1);
    assert_eq!(*fixture.encryptions.lock().unwrap(), 1);
    let sent = fixture.broadcasts.lock().unwrap();
    assert!(sent.len() >= 3);
    assert!(
        sent.iter()
            .all(|raw| raw == &format!("0x{}", hex::encode(first.signed.raw())))
    );
    drop(sent);
    db.cleanup().await
}

#[tokio::test]
async fn bridge_mint_claimed_unsigned_restart_requires_accounting_before_encryption() -> Result<()>
{
    let db = TestDb::new().await?;
    let (mut store, deposit) = funded(&db).await?;
    let report = store.bridge_accounting(&accounting(2, 2, 0)).await?;
    store
        .claim_bridge_mint(deposit.deposit_id, deposit.version, report.id)
        .await?;
    drop(store);
    let fixture = Fixture::new();
    let mut minter = fixture.minter(&db).await?;
    assert!(minter.mint_once(-1, &signer(), fees()).await.is_err());
    assert_eq!(*fixture.encryptions.lock().unwrap(), 0);
    assert!(fixture.broadcasts.lock().unwrap().is_empty());
    let saved = minter
        .mint_once(report.id, &signer(), fees())
        .await?
        .unwrap();
    assert_eq!(saved.signed.nonce(), 0);
    assert_eq!(*fixture.encryptions.lock().unwrap(), 1);
    assert!(
        minter
            .store
            .bridge_deposit(deposit.deposit_id)
            .await?
            .unwrap()
            .claimed
    );
    db.cleanup().await
}

#[tokio::test]
async fn bridge_mint_confirmed_revert_retains_claim_and_signed_identity() -> Result<()> {
    let db = TestDb::new().await?;
    let (mut store, deposit) = funded(&db).await?;
    let report = store.bridge_accounting(&accounting(2, 2, 0)).await?;
    let fixture = Fixture::new();
    let mut minter = fixture.minter(&db).await?;
    let saved = minter
        .mint_once(report.id, &signer(), fees())
        .await?
        .unwrap();
    drop(minter);
    let mut scan = batch(reference(3, 0), 4, 0);
    scan.chain_id = 11155111;
    scan.receipts.push(zama::watcher::ReceiptObservation {
        transaction_hash: saved.signed.hash(),
        block: reference(3, 0),
        from: signer().address(),
        to: contract(),
        success: false,
    });
    store.apply_zama_scan(&scan).await?;
    *fixture.head.lock().unwrap() = 4;
    let mut minter = fixture.minter(&db).await?;
    let recovered = minter.mint_once(-1, &signer(), fees()).await?.unwrap();
    assert!(recovered.confirmed);
    assert_eq!(recovered.state, crate::store::TransactionState::Reverted);
    assert_eq!(recovered.signed.raw(), saved.signed.raw());
    let deposit = store.bridge_deposit(deposit.deposit_id).await?.unwrap();
    assert_eq!(deposit.state, DepositState::MintPending);
    assert!(deposit.claimed);
    assert_eq!(*fixture.encryptions.lock().unwrap(), 1);
    assert_eq!(fixture.broadcasts.lock().unwrap().len(), 1);
    assert_eq!(store.zama_next_nonce(signer().address()).await?, 1);
    db.cleanup().await
}
