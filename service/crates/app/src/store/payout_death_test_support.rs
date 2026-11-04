//! Controlled unary gRPC facts only. No consensus-valid or live payout is made.
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use zcash::rpc::history::LOWER_HEIGHT;
use zcash::rpc::{ChainMetadata, CompactBlock, Empty, LightwalletClient, TreeState};
#[derive(Clone, PartialEq, prost::Message)]
struct BlockId {
    #[prost(uint64, tag = "1")]
    height: u64,
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
#[derive(Default)]
pub(crate) struct Data {
    pub(crate) tip: u32,
    pub(crate) blocks: BTreeMap<u32, CompactBlock>,
    pub(crate) trees: BTreeMap<u32, TreeState>,
    pub(crate) fail: Option<(bool, u32, tonic::Code)>,
    pub(crate) reads: BTreeMap<u32, usize>,
    pub(crate) reorg: Option<(u32, usize)>,
}
pub(crate) fn data(end: u32) -> Data {
    let mut data = Data {
        tip: end,
        ..Default::default()
    };
    for height in LOWER_HEIGHT - 1..=end {
        let mut display = hash(height);
        display.reverse();
        data.blocks.insert(
            height,
            CompactBlock {
                height: height.into(),
                hash: hash(height).to_vec(),
                prev_hash: hash(height - 1).to_vec(),
                vtx: vec![],
                chain_metadata: Some(ChainMetadata {
                    ironwood_commitment_tree_size: 0,
                }),
            },
        );
        data.trees.insert(
            height,
            TreeState {
                network: "test".into(),
                height: height.into(),
                hash: hex::encode(display),
                ironwood_tree: "000000".into(),
            },
        );
    }
    data
}
pub(crate) fn hash(height: u32) -> [u8; 32] {
    let mut hash = [0; 32];
    hash[..4].copy_from_slice(&height.to_le_bytes());
    hash
}
#[derive(Clone)]
struct Server(Arc<Mutex<Data>>);
impl tonic::server::NamedService for Server {
    const NAME: &'static str = "cash.z.wallet.sdk.rpc.CompactTxStreamer";
}
macro_rules! unary {
    ($req:expr, $state:expr, $q:ty, $r:ty, $body:expr) => {{
        struct Method(Arc<Mutex<Data>>);
        impl tonic::server::UnaryService<$q> for Method {
            type Response = $r;
            type Future = tonic::codegen::BoxFuture<tonic::Response<$r>, tonic::Status>;
            fn call(&mut self, req: tonic::Request<$q>) -> Self::Future {
                let state = self.0.clone();
                Box::pin(async move {
                    let mut data = state.lock().unwrap();
                    ($body)(&mut *data, req.into_inner()).map(tonic::Response::new)
                })
            }
        }
        Ok(tonic::server::Grpc::new(tonic_prost::ProstCodec::default())
            .unary(Method($state), $req)
            .await)
    }};
}
impl tonic::codegen::Service<tonic::codegen::http::Request<tonic::body::Body>> for Server {
    type Response = tonic::codegen::http::Response<tonic::body::Body>;
    type Error = std::convert::Infallible;
    type Future = tonic::codegen::BoxFuture<Self::Response, Self::Error>;
    fn poll_ready(
        &mut self,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::result::Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }
    fn call(&mut self, req: tonic::codegen::http::Request<tonic::body::Body>) -> Self::Future {
        let state = self.0.clone();
        Box::pin(async move {
            match req.uri().path().rsplit('/').next().unwrap() {
                "GetLightdInfo" => unary!(
                    req,
                    state,
                    Empty,
                    LightdInfo,
                    |d: &mut Data, _: Empty| Ok::<_, tonic::Status>(LightdInfo {
                        chain_name: "test".into(),
                        consensus_branch_id: "77190ad9".into(),
                        block_height: d.tip.into()
                    })
                ),
                "GetBlock" => unary!(
                    req,
                    state,
                    BlockId,
                    CompactBlock,
                    |d: &mut Data, q: BlockId| {
                        let h = q.height as u32;
                        if let Some((false, height, code)) = d.fail {
                            if height == h {
                                return Err(tonic::Status::new(code, "fixture failure"));
                            }
                        }
                        let reads = d.reads.entry(h).or_default();
                        *reads += 1;
                        if d.reorg == Some((h, *reads)) {
                            d.blocks.get_mut(&h).unwrap().hash[31] ^= 1;
                        }
                        d.blocks
                            .get(&h)
                            .cloned()
                            .ok_or_else(|| tonic::Status::not_found("gap"))
                    }
                ),
                "GetTreeState" => unary!(
                    req,
                    state,
                    BlockId,
                    TreeState,
                    |d: &mut Data, q: BlockId| {
                        if let Some((true, h, code)) = d.fail {
                            if h == q.height as u32 {
                                return Err(tonic::Status::new(code, "fixture failure"));
                            }
                        }
                        d.trees
                            .get(&(q.height as u32))
                            .cloned()
                            .ok_or_else(|| tonic::Status::not_found("gap"))
                    }
                ),
                path => panic!("unexpected RPC (including forbidden GetTransaction): {path}"),
            }
        })
    }
}
pub(crate) struct Running {
    pub(crate) endpoint: String,
    pub(crate) client: LightwalletClient,
    pub(crate) state: Arc<Mutex<Data>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Running {
    fn drop(&mut self) {
        self.task.abort();
    }
}
pub(crate) async fn serve(data: Data) -> Running {
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", socket.local_addr().unwrap());
    let state = Arc::new(Mutex::new(data));
    let service = Server(state.clone());
    let task = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(service)
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(socket))
            .await
            .unwrap();
    });
    let client = LightwalletClient::connect(&endpoint).await.unwrap();
    Running {
        endpoint,
        client,
        state,
        task,
    }
}

pub(crate) struct HttpServer {
    pub(crate) url: String,
    task: tokio::task::JoinHandle<()>,
}
impl HttpServer {
    pub(crate) async fn start(router: axum::Router) -> anyhow::Result<Self> {
        let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("http://{}", socket.local_addr()?);
        let task = tokio::spawn(async move {
            axum::serve(socket, router).await.unwrap();
        });
        Ok(Self { url, task })
    }
}
impl Drop for HttpServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
#[derive(Default)]
pub(crate) struct EvmFacts {
    pub(crate) drift_on_nonce: Option<(Arc<Mutex<Data>>, u32)>,
    pub(crate) lose_ack: bool,
    pub(crate) broadcasts: Vec<String>,
}
pub(crate) async fn evm_rpc() -> anyhow::Result<(HttpServer, Arc<Mutex<EvmFacts>>)> {
    use super::zama_test_support::reference;
    use axum::{Json, Router, routing::post};
    use serde_json::{Value, json};
    let facts = Arc::new(Mutex::new(EvmFacts::default()));
    let captured = facts.clone();
    let server = HttpServer::start(Router::new().route("/",post(move |Json(v):Json<Value>| {
        let facts = captured.clone();
        async move {
            let result = match v["method"].as_str().unwrap() {
                "eth_chainId" => json!("0xaa36a7"),
                "eth_getCode" => json!("0x6000"),
                "eth_call" => json!(format!("0x{:064x}", if v["params"][0]["input"].as_str().or_else(||v["params"][0]["data"].as_str()).unwrap_or("").starts_with("0x313ce567") {8} else {1})),
                "eth_getTransactionCount" => {
                    if let Some((chain,height)) = facts.lock().unwrap().drift_on_nonce.take() {
                        chain.lock().unwrap().blocks.get_mut(&height).unwrap().hash[31] ^= 1;
                    }
                    json!("0x0")
                },
                "eth_getBlockByNumber" => {
                    let tag = v["params"][0].as_str().unwrap();
                    let n = if tag == "latest" {3} else {u64::from_str_radix(tag.trim_start_matches("0x"),16).unwrap()};
                    json!({"number":format!("0x{n:x}"),"hash":reference(n,0).hash,"parentHash":reference(n.saturating_sub(1),0).hash})
                },
                "eth_getLogs" => json!([]),
                "eth_getTransactionReceipt" => Value::Null,
                "eth_sendRawTransaction" => {
                    let raw = v["params"][0].as_str().unwrap().to_string();
                    let mut facts = facts.lock().unwrap();
                    facts.broadcasts.push(raw.clone());
                    if facts.lose_ack {
                        return Json(json!({"jsonrpc":"2.0","id":v["id"],"error":{"code":-32000,"message":"controlled uncertain submission"}}));
                    }
                    json!(alloy::primitives::keccak256(hex::decode(raw.trim_start_matches("0x")).unwrap()))
                },
                method => panic!("unexpected controlled EVM RPC {method}"),
            };
            Json(json!({"jsonrpc":"2.0","id":v["id"],"result":result}))
        }
    }))).await?;
    Ok((server, facts))
}
