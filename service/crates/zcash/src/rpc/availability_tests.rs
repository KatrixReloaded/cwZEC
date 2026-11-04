//! Local-only scan availability fixture; never submits transactions or uses a database.
use super::*;
use crate::{core::Checkpoint, watcher::Watcher};
use std::sync::{Arc, Mutex};

const SAVED: u32 = 4_582_230;
const TIP: u32 = SAVED + 2;

struct Data {
    failures: usize,
    code: tonic::Code,
    failed_height: u32,
    tree_failure: bool,
    wrong_height: bool,
    changed_checkpoint: bool,
    refreshed_tip: u32,
    block_calls: Vec<u32>,
    tip_calls: usize,
}
impl Default for Data {
    fn default() -> Self {
        Self {
            failures: 1,
            code: tonic::Code::OutOfRange,
            failed_height: TIP,
            tree_failure: false,
            wrong_height: false,
            changed_checkpoint: false,
            refreshed_tip: TIP,
            block_calls: vec![],
            tip_calls: 0,
        }
    }
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
                    ($body)(&mut *state.lock().unwrap(), req.into_inner()).map(tonic::Response::new)
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
                "GetLightdInfo" => {
                    unary!(req, state, Empty, LightdInfo, |d: &mut Data, _: Empty| {
                        d.tip_calls += 1;
                        Ok::<_, tonic::Status>(LightdInfo {
                            chain_name: "test".into(),
                            consensus_branch_id: "77190ad9".into(),
                            block_height: d.refreshed_tip.into(),
                        })
                    })
                }
                "GetBlock" => unary!(
                    req,
                    state,
                    BlockId,
                    CompactBlock,
                    |d: &mut Data, q: BlockId| {
                        let h = q.height as u32;
                        d.block_calls.push(h);
                        if !d.tree_failure && h == d.failed_height && d.failures > 0 {
                            d.failures -= 1;
                            return Err(tonic::Status::new(d.code, "private server text"));
                        }
                        Ok(CompactBlock {
                            height: q.height + u64::from(d.wrong_height && h != SAVED),
                            hash: vec![
                                if d.changed_checkpoint && h == SAVED {
                                    8
                                } else {
                                    7
                                };
                                32
                            ],
                            ..Default::default()
                        })
                    }
                ),
                "GetTreeState" => unary!(
                    req,
                    state,
                    BlockId,
                    TreeState,
                    |d: &mut Data, q: BlockId| {
                        if d.tree_failure && q.height == u64::from(d.failed_height) {
                            return Err(tonic::Status::new(d.code, "private tree text"));
                        }
                        Ok(TreeState {
                            height: q.height,
                            ..Default::default()
                        })
                    }
                ),
                method => panic!("unexpected RPC: {method}"),
            }
        })
    }
}
struct Running {
    watcher: Watcher,
    data: Arc<Mutex<Data>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Running {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn serve(data: Data) -> Running {
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", socket.local_addr().unwrap());
    let data = Arc::new(Mutex::new(data));
    let service = Server(data.clone());
    let task = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(service)
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(socket))
            .await
            .unwrap();
    });
    let watcher = Watcher::new(LightwalletClient::connect(&endpoint).await.unwrap());
    Running {
        watcher,
        data,
        task,
    }
}
fn checkpoint() -> Checkpoint {
    Checkpoint {
        height: SAVED,
        hash: [7; 32],
        parent: [0; 32],
        frontier: vec![],
    }
}

#[tokio::test]
async fn near_tip_recovers_complete_ordered_batch_after_revalidating_checkpoint() {
    let server = serve(Data::default()).await;
    let batch = server
        .watcher
        .fetch_scan_batch_until(&checkpoint(), TIP, TIP, || false)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        batch.iter().map(|(b, _)| b.height).collect::<Vec<_>>(),
        vec![u64::from(SAVED + 1), u64::from(TIP)]
    );
    let d = server.data.lock().unwrap();
    assert_eq!(d.tip_calls, 2); // connect plus retry validation
    let missing = d.block_calls.iter().position(|h| *h == TIP).unwrap();
    let check = d.block_calls.iter().position(|h| *h == SAVED).unwrap();
    let retry = d.block_calls.iter().rposition(|h| *h == TIP).unwrap();
    assert!(missing < check && check < retry);
}

#[tokio::test]
async fn near_tip_exhaustion_is_bounded_and_diagnostic_keeps_grpc_code() {
    let server = serve(Data {
        failures: usize::MAX,
        ..Data::default()
    })
    .await;
    let error = server
        .watcher
        .fetch_scan_batch_until(&checkpoint(), TIP, TIP, || false)
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "near-tip GetBlock availability retries exhausted"
    );
    assert!(!error.to_string().contains("private"));
    assert_eq!(failure_code(&error), Some(11));
    let d = server.data.lock().unwrap();
    assert_eq!(d.block_calls.iter().filter(|h| **h == TIP).count(), 5);
    assert_eq!(d.block_calls.iter().filter(|h| **h == SAVED).count(), 4);
    assert_eq!(d.tip_calls, 5);
}

#[tokio::test]
async fn validation_unrelated_status_tree_failure_and_historical_gap_never_retry() {
    for data in [
        Data {
            wrong_height: true,
            failures: 0,
            ..Data::default()
        },
        Data {
            code: tonic::Code::Unavailable,
            ..Data::default()
        },
        Data {
            tree_failure: true,
            ..Data::default()
        },
        Data {
            failed_height: SAVED + 1,
            ..Data::default()
        },
    ] {
        let historical = data.failed_height == SAVED + 1;
        let server = serve(data).await;
        assert!(
            server
                .watcher
                .fetch_scan_batch_until(
                    &checkpoint(),
                    TIP,
                    if historical { TIP + 2 } else { TIP },
                    || false
                )
                .await
                .is_err()
        );
        let d = server.data.lock().unwrap();
        assert_eq!(d.tip_calls, 1);
        assert!(!d.block_calls.contains(&SAVED));
    }
}

#[tokio::test]
async fn changed_checkpoint_and_retreating_or_advanced_tip_fail_closed() {
    for (data, message) in [
        (
            Data {
                changed_checkpoint: true,
                ..Data::default()
            },
            "near-tip GetBlock retry checkpoint changed",
        ),
        (
            Data {
                refreshed_tip: TIP - 1,
                ..Data::default()
            },
            "near-tip GetBlock retry tip changed",
        ),
        (
            Data {
                refreshed_tip: TIP + 3,
                ..Data::default()
            },
            "near-tip GetBlock retry tip changed",
        ),
    ] {
        let server = serve(data).await;
        let error = server
            .watcher
            .fetch_scan_batch_until(&checkpoint(), TIP, TIP, || false)
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), message);
        assert_eq!(
            server
                .data
                .lock()
                .unwrap()
                .block_calls
                .iter()
                .filter(|h| **h == TIP)
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn shutdown_during_backoff_returns_no_batch_without_retrying() {
    let server = serve(Data::default()).await;
    let started = tokio::time::Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        server
            .watcher
            .fetch_scan_batch_until(&checkpoint(), TIP, TIP, || {
                started.elapsed() >= Duration::from_millis(200)
            }),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(result.is_none());
    let d = server.data.lock().unwrap();
    assert_eq!(d.tip_calls, 1);
    assert_eq!(d.block_calls.iter().filter(|h| **h == TIP).count(), 1);
}

#[tokio::test]
async fn initial_shutdown_fetches_nothing() {
    let server = serve(Data::default()).await;
    assert!(
        server
            .watcher
            .fetch_scan_batch_until(&checkpoint(), TIP, TIP, || true)
            .await
            .unwrap()
            .is_none()
    );
    assert!(server.data.lock().unwrap().block_calls.is_empty());
}
