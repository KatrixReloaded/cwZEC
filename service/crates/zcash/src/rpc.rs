//! Concrete, bounded lightwalletd calls for the single configured endpoint.
//!
//! Wire tags follow lightwallet-protocol ec53075c706e78a4e107e7d3c92eea4e70262647,
//! retrieved 2026-10-25 (service.proto and compact_formats.proto):
//! https://github.com/zcash/lightwallet-protocol/tree/ec53075c706e78a4e107e7d3c92eea4e70262647/walletrpc
//! These are a narrow wire projection, not a wallet SDK. Unknown fields are ignored.

use std::time::Duration;

use anyhow::{Context, Result, ensure};
use prost::Message;
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};

pub mod history;

#[derive(Clone)]
pub struct LightwalletClient {
    channel: Channel,
}

impl LightwalletClient {
    pub async fn connect(endpoint: &str) -> Result<Self> {
        let mut transport = Endpoint::from_shared(endpoint.to_owned())?;
        let uri = transport.uri();
        let local = uri.scheme_str() == Some("http")
            && uri.host() == Some("127.0.0.1")
            && uri.port_u16().is_some_and(|port| port != 0)
            && uri.path() == "/"
            && uri.query().is_none();
        ensure!(
            uri.scheme_str() == Some("https") || local,
            "lightwalletd requires HTTPS or explicit IPv4 loopback"
        );
        if !local {
            transport = transport.tls_config(ClientTlsConfig::new().with_native_roots())?;
        }
        let channel = transport
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(30))
            .connect()
            .await
            .context("connect lightwalletd")?;
        let client = Self { channel };
        client.tip().await?;
        Ok(client)
    }

    async fn call<Q, R>(&self, method: &'static str, request: Q) -> Result<R>
    where
        Q: Message + Default + Send + Sync + 'static,
        R: Message + Default + Send + Sync + 'static,
    {
        let mut grpc = tonic::client::Grpc::new(self.channel.clone())
            .max_decoding_message_size(8 * 1024 * 1024);
        grpc.ready().await.context("lightwalletd readiness")?;
        let path = format!("/cash.z.wallet.sdk.rpc.CompactTxStreamer/{method}");
        let response = grpc
            .unary(
                tonic::Request::new(request),
                path.parse()?,
                tonic_prost::ProstCodec::<Q, R>::default(),
            )
            .await
            .with_context(|| format!("lightwalletd {method}"))?;
        Ok(response.into_inner())
    }

    pub async fn tip(&self) -> Result<u32> {
        let info: LightdInfo = self.call("GetLightdInfo", Empty {}).await?;
        ensure!(info.chain_name == "test", "lightwalletd is not testnet");
        ensure!(
            info.consensus_branch_id.eq_ignore_ascii_case("77190ad9"),
            "unsupported lightwalletd consensus branch"
        );
        Ok(u32::try_from(info.block_height)?)
    }

    pub async fn block(&self, height: u32) -> Result<CompactBlock> {
        let block: CompactBlock =
            self.call("GetBlock", BlockId::at(height))
                .await
                .map_err(|error| {
                    if failure_code(&error) == Some(tonic::Code::OutOfRange as i32) {
                        error.context(BlockOutOfRange { height })
                    } else {
                        error
                    }
                })?;
        ensure!(
            block.height == u64::from(height),
            "wrong compact block height"
        );
        Ok(block)
    }

    pub async fn block_hash(&self, height: u32) -> Result<[u8; 32]> {
        fixed(&self.block(height).await?.hash, "block hash")
    }

    pub async fn tree(&self, height: u32) -> Result<TreeState> {
        self.call("GetTreeState", BlockId::at(height)).await
    }

    /// GetTreeState's hash selector uses display order, unlike CompactBlock.hash.
    pub async fn tree_by_hash(&self, mut protocol_hash: [u8; 32]) -> Result<TreeState> {
        protocol_hash.reverse();
        self.call(
            "GetTreeState",
            BlockId {
                height: 0,
                hash: protocol_hash.to_vec(),
            },
        )
        .await
    }

    /// Txids use protocol byte order, the same order as Transaction::txid().as_ref().
    pub async fn transaction(&self, txid: [u8; 32]) -> Result<RawTransaction> {
        self.call(
            "GetTransaction",
            TxFilter {
                hash: txid.to_vec(),
            },
        )
        .await
    }

    /// Diagnostic lookup only. In particular, lightwalletd can map backend RPC
    /// failures to NotFound; neither that status nor an empty reply proves death.
    pub async fn inspect_transaction(&self, txid: [u8; 32]) -> TransactionLookup {
        classify_transaction(self.transaction(txid).await)
    }

    /// Submit exactly the supplied, already-journaled signed bytes. Every error is
    /// an uncertain outcome; callers must reconcile or retry these same bytes.
    pub async fn submit(&self, bytes: &[u8]) -> Result<()> {
        ensure!(
            !bytes.is_empty() && bytes.len() <= 2_000_000,
            "invalid transaction size"
        );
        let response: SendResponse = self
            .call(
                "SendTransaction",
                RawTransaction {
                    data: bytes.to_vec(),
                    height: 0,
                },
            )
            .await
            .context("transaction submission outcome uncertain")?;
        // Do not include arbitrary server text or transaction data in errors.
        ensure!(
            response.error_code == 0,
            "transaction submission outcome uncertain (server code {})",
            response.error_code
        );
        Ok(())
    }
}

/// Identifies the method and requested height without trusting remote status text.
/// This alone does not establish that an unavailable block is safe to retry.
#[derive(Debug)]
pub(crate) struct BlockOutOfRange {
    pub height: u32,
}
impl std::fmt::Display for BlockOutOfRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GetBlock out of range at height {}", self.height)
    }
}
impl std::error::Error for BlockOutOfRange {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransactionLookup {
    Returned { height: u64 },
    NotFoundUnverified,
    RpcFailure,
    EmptyResponse,
}

/// Operational diagnostic only: never exposes the remote message or metadata.
/// A gRPC status, including NotFound, is not transaction-death evidence.
pub fn failure_code(error: &anyhow::Error) -> Option<i32> {
    error
        .downcast_ref::<tonic::Status>()
        .map(|s| s.code() as i32)
}

fn classify_transaction(result: Result<RawTransaction>) -> TransactionLookup {
    match result {
        Ok(tx) if tx.data.is_empty() => TransactionLookup::EmptyResponse,
        Ok(tx) => TransactionLookup::Returned { height: tx.height },
        Err(error)
            if error
                .downcast_ref::<tonic::Status>()
                .is_some_and(|status| status.code() == tonic::Code::NotFound) =>
        {
            TransactionLookup::NotFoundUnverified
        }
        Err(_) => TransactionLookup::RpcFailure,
    }
}

#[cfg(test)]
#[path = "rpc/death_tests.rs"]
mod death_tests;

#[cfg(test)]
#[path = "rpc/availability_tests.rs"]
mod availability_tests;

pub(crate) fn fixed<const N: usize>(bytes: &[u8], what: &str) -> Result<[u8; N]> {
    bytes
        .try_into()
        .with_context(|| format!("invalid {what} length"))
}

#[derive(Clone, PartialEq, Message)]
pub struct Empty {}

#[derive(Clone, PartialEq, Message)]
pub struct BlockId {
    #[prost(uint64, tag = "1")]
    pub height: u64,
    #[prost(bytes = "vec", tag = "2")]
    pub hash: Vec<u8>,
}
impl BlockId {
    fn at(height: u32) -> Self {
        Self {
            height: height.into(),
            hash: vec![],
        }
    }
}

#[derive(Clone, PartialEq, Message)]
struct TxFilter {
    #[prost(bytes = "vec", tag = "3")]
    hash: Vec<u8>,
}

#[derive(Clone, PartialEq, Message)]
pub struct RawTransaction {
    #[prost(bytes = "vec", tag = "1")]
    pub data: Vec<u8>,
    #[prost(uint64, tag = "2")]
    pub height: u64,
}

#[derive(Clone, PartialEq, Message)]
struct SendResponse {
    #[prost(int32, tag = "1")]
    error_code: i32,
    #[prost(string, tag = "2")]
    error_message: String,
}

#[derive(Clone, PartialEq, Message)]
struct LightdInfo {
    #[prost(string, tag = "4")]
    chain_name: String,
    #[prost(string, tag = "6")]
    consensus_branch_id: String,
    #[prost(uint64, tag = "7")]
    block_height: u64,
}

#[derive(Clone, PartialEq, Message)]
pub struct TreeState {
    #[prost(string, tag = "1")]
    pub network: String,
    #[prost(uint64, tag = "2")]
    pub height: u64,
    #[prost(string, tag = "3")]
    pub hash: String,
    #[prost(string, tag = "7")]
    pub ironwood_tree: String,
}

#[derive(Clone, PartialEq, Message)]
pub struct CompactBlock {
    #[prost(uint64, tag = "2")]
    pub height: u64,
    #[prost(bytes = "vec", tag = "3")]
    pub hash: Vec<u8>,
    #[prost(bytes = "vec", tag = "4")]
    pub prev_hash: Vec<u8>,
    #[prost(message, repeated, tag = "7")]
    pub vtx: Vec<CompactTx>,
    #[prost(message, optional, tag = "8")]
    pub chain_metadata: Option<ChainMetadata>,
}

#[derive(Clone, PartialEq, Message)]
pub struct ChainMetadata {
    #[prost(uint32, tag = "3")]
    pub ironwood_commitment_tree_size: u32,
}

#[derive(Clone, PartialEq, Message)]
pub struct CompactTx {
    #[prost(uint64, tag = "1")]
    pub index: u64,
    #[prost(bytes = "vec", tag = "2")]
    pub txid: Vec<u8>,
    #[prost(message, repeated, tag = "9")]
    pub ironwood_actions: Vec<CompactIronwoodAction>,
}

#[derive(Clone, PartialEq, Message)]
pub struct CompactIronwoodAction {
    #[prost(bytes = "vec", tag = "1")]
    pub nullifier: Vec<u8>,
    #[prost(bytes = "vec", tag = "2")]
    pub cmx: Vec<u8>,
    #[prost(bytes = "vec", tag = "3")]
    pub ephemeral_key: Vec<u8>,
    #[prost(bytes = "vec", tag = "4")]
    pub ciphertext: Vec<u8>,
}
