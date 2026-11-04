use super::*;
use alloy::{
    primitives::U64,
    providers::Provider,
    rpc::types::{Filter, Log},
    sol_types::SolEvent,
    transports::TransportError,
};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Header {
    number: U64,
    hash: B256,
    parent_hash: B256,
}

impl Watcher {
    async fn header(&self, tag: &str) -> Result<Option<Block>> {
        let header: Option<Header> = read_with_retry("RPC block lookup failed", || {
            self.provider
                .client()
                .request("eth_getBlockByNumber", (tag, false))
        })
        .await?;
        let Some(h) = header else {
            return Ok(None);
        };
        ensure!(!h.hash.is_zero(), "RPC block has zero hash");
        Ok(Some(Block {
            reference: BlockRef {
                number: h.number.to(),
                hash: h.hash,
            },
            parent_hash: h.parent_hash,
        }))
    }
    /// A missing numbered block can indicate that the canonical tip shortened.
    /// Transport and malformed-response failures remain errors.
    pub async fn optional_block(&self, number: u64) -> Result<Option<Block>> {
        let block = self.header(&format!("{number:#x}")).await?;
        if let Some(block) = block {
            ensure!(
                block.reference.number == number,
                "RPC block number mismatch"
            );
        }
        Ok(block)
    }
    pub async fn block(&self, number: u64) -> Result<Block> {
        self.optional_block(number)
            .await?
            .ok_or_else(|| anyhow!("RPC block unavailable"))
    }
    pub async fn latest(&self) -> Result<Block> {
        self.header("latest")
            .await?
            .ok_or_else(|| anyhow!("RPC block unavailable"))
    }
    pub async fn receipt(&self, hash: B256) -> Result<Option<ReceiptObservation>> {
        let Some(r) = read_with_retry("RPC receipt lookup failed", || {
            self.provider.get_transaction_receipt(hash)
        })
        .await?
        else {
            return Ok(None);
        };
        ensure!(
            r.transaction_hash == hash,
            "RPC receipt transaction mismatch"
        );
        ensure!(r.to == Some(self.contract), "RPC receipt target mismatch");
        let block = BlockRef {
            number: r
                .block_number
                .ok_or_else(|| anyhow!("receipt missing block number"))?,
            hash: r
                .block_hash
                .ok_or_else(|| anyhow!("receipt missing block hash"))?,
        };
        ensure!(
            self.block(block.number).await?.reference == block,
            "receipt is not canonical"
        );
        Ok(Some(ReceiptObservation {
            transaction_hash: hash,
            block,
            from: r.from,
            to: self.contract,
            success: r.status(),
        }))
    }
    pub(super) async fn logs(&self, start: u64, end: u64) -> Result<Vec<Log>> {
        // Alchemy Free accepts at most 10 inclusive blocks per log query.
        // Keep the larger scanner checkpoint; a failed chunk rejects the whole batch.
        let mut logs = Vec::new();
        for first in (start..=end).step_by(10) {
            let filter = Filter::new()
                .address(self.contract)
                .from_block(first)
                .to_block(end.min(first.saturating_add(9)))
                .event_signature(vec![
                    DepositMinted::SIGNATURE_HASH,
                    DepositVoided::SIGNATURE_HASH,
                    RedeemRequested::SIGNATURE_HASH,
                    RedeemFulfilled::SIGNATURE_HASH,
                    RedeemRecredited::SIGNATURE_HASH,
                ]);
            let chunk =
                read_with_retry("RPC log lookup failed", || self.provider.get_logs(&filter))
                    .await?;
            ensure!(
                logs.len() + chunk.len() <= MAX_LOGS,
                "RPC log bound exceeded"
            );
            logs.extend(chunk);
        }
        Ok(logs)
    }
}

// Scanner reads only. Retry the same request after explicit throttling, never a
// validation failure or transaction submission. Exhaustion rejects the batch.
pub(super) async fn read_with_retry<T, F, R>(message: &'static str, mut request: F) -> Result<T>
where
    F: FnMut() -> R,
    R: std::future::IntoFuture<Output = Result<T, TransportError>>,
{
    let mut retries = 0;
    loop {
        match request().await {
            Ok(value) => return Ok(value),
            Err(error) => {
                let (http, rpc) = rpc_codes(&error);
                if (http == Some(429) || rpc == Some(429)) && retries < 3 {
                    let seconds = 2 << retries;
                    eprintln!("evm RPC throttled; retrying same request in {seconds}s ({message})");
                    tokio::time::sleep(std::time::Duration::from_secs(seconds)).await;
                    retries += 1;
                    continue;
                }
                return Err(rpc_failure(message, error));
            }
        }
    }
}

fn rpc_codes(error: &TransportError) -> (Option<u16>, Option<i64>) {
    (
        error
            .as_transport_err()
            .and_then(|e| e.as_http_error())
            .map(|e| e.status),
        error.as_error_resp().map(|e| e.code),
    )
}

// Never log remote messages, response bodies or endpoint URLs (which may contain keys).
pub(super) fn rpc_failure(message: &'static str, error: TransportError) -> anyhow::Error {
    let (http, rpc) = rpc_codes(&error);
    eprintln!(
        "bridge diagnostic: evm_http_status={http:?} evm_rpc_code={rpc:?} timeout={}",
        error
            .as_transport_err()
            .and_then(|e| e.as_custom())
            .and_then(|e| e.downcast_ref::<alloy::transports::http::reqwest::Error>())
            .is_some_and(|e| e.is_timeout())
    );
    anyhow!(message)
}
