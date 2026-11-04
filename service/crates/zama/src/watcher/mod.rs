//! Bounded canonical observations from the configured CWZEC endpoint.
mod rpc;
mod types;
pub use types::*;

use crate::contract::{CwzecClient, bindings::*};
use alloy::{
    primitives::{Address, B256},
    providers::{DynProvider, Provider},
    rpc::types::Log,
};
use anyhow::{Result, anyhow, ensure};
use std::collections::{BTreeMap, BTreeSet};
const MAX_RECEIPTS: usize = 256;
const HEADER_CONCURRENCY: usize = 8;

#[derive(Clone)]
pub struct Watcher {
    provider: DynProvider,
    chain_id: u64,
    contract: Address,
}
impl Watcher {
    pub fn new(client: &CwzecClient) -> Self {
        Self {
            provider: client.provider.clone(),
            chain_id: client.chain_id(),
            contract: client.address(),
        }
    }
    /// All observations remain provisional until the checkpoint and end anchor rereads succeed.
    pub async fn scan(&self, previous: BlockRef, end: u64, tracked: &[B256]) -> Result<ScanBatch> {
        ensure!(
            end >= previous.number && end - previous.number <= SCAN_BLOCKS,
            "invalid scan range"
        );
        ensure!(
            rpc::read_with_retry("RPC chain lookup failed", || self.provider.get_chain_id())
                .await?
                == self.chain_id,
            "RPC chain mismatch"
        );
        ensure!(
            self.block(previous.number).await?.reference == previous,
            "previous checkpoint changed"
        );
        let end_block = self.block(end).await?;
        if end == previous.number {
            ensure!(end_block.reference == previous, "range anchor changed");
        }
        let logs = if end > previous.number {
            self.logs(previous.number + 1, end).await?
        } else {
            Vec::new()
        };
        ensure!(logs.len() <= MAX_LOGS, "RPC log bound exceeded");
        let mut unique = BTreeMap::new();
        for log in logs {
            let e = decode_log(&log, self.contract, previous.number, end)?;
            let key = (e.block.hash, e.transaction_hash, e.log_index);
            if let Some(existing) = unique.get(&key) {
                ensure!(existing == &log, "conflicting duplicate log");
            } else {
                unique.insert(key, log);
            }
        }
        // Read every log range, but avoid fetching headers for old empty blocks.
        // Keep a complete rewind window at each durable checkpoint.
        let mut events = unique
            .values()
            .map(|log| decode_log(log, self.contract, previous.number, end))
            .collect::<Result<Vec<_>>>()?;
        let mut needed: BTreeSet<u64> = events.iter().map(|event| event.block.number).collect();
        if end > previous.number {
            needed.extend((previous.number + 1).max(end.saturating_sub(RETAIN_BLOCKS))..end);
        }
        needed.remove(&end);
        let needed: Vec<_> = needed.into_iter().collect();
        let mut blocks = BTreeMap::new();
        for chunk in needed.chunks(HEADER_CONCURRENCY) {
            let mut tasks = tokio::task::JoinSet::new();
            for &number in chunk {
                let watcher = self.clone();
                tasks.spawn(async move { watcher.block(number).await });
            }
            while let Some(result) = tasks.join_next().await {
                let block = result.map_err(|_| anyhow!("header fetch task failed"))??;
                blocks.insert(block.reference.number, block);
            }
        }
        if end > previous.number {
            blocks.insert(end, end_block);
        }
        let mut hashes: BTreeSet<B256> = tracked.iter().copied().collect();
        for event in &events {
            let header = blocks
                .get(&event.block.number)
                .ok_or_else(|| anyhow!("log header unavailable"))?;
            ensure!(header.reference == event.block, "log is not canonical");
            hashes.insert(event.transaction_hash);
        }
        ensure!(hashes.len() <= MAX_RECEIPTS, "receipt bound exceeded");
        let mut receipts = BTreeMap::new();
        for hash in hashes {
            if let Some(r) = self.receipt(hash).await? {
                if r.block.number <= end {
                    receipts.insert(hash, r);
                }
            }
        }
        for event in &events {
            let r = receipts
                .get(&event.transaction_hash)
                .ok_or_else(|| anyhow!("event receipt unavailable"))?;
            ensure!(
                r.success && r.block == event.block,
                "event receipt failed or changed block"
            );
        }
        events.sort_by_key(|e| (e.block.number, e.log_index, e.transaction_hash));
        ensure!(
            self.block(previous.number).await?.reference == previous,
            "previous checkpoint changed during scan"
        );
        ensure!(
            self.block(end).await?.reference == end_block.reference,
            "range end changed during scan"
        );
        let batch = ScanBatch {
            chain_id: self.chain_id,
            contract: self.contract,
            previous,
            end: end_block.reference,
            blocks: blocks.into_values().collect(),
            events,
            receipts: receipts.into_values().collect(),
        };
        batch.validate_headers()?;
        Ok(batch)
    }
}

fn decode_log(log: &Log, contract: Address, previous: u64, end: u64) -> Result<EventObservation> {
    ensure!(!log.removed, "removed lifecycle log");
    ensure!(log.address() == contract, "log contract mismatch");
    let number = log
        .block_number
        .ok_or_else(|| anyhow!("log missing block number"))?;
    ensure!(number > previous && number <= end, "log outside scan range");
    let block = BlockRef {
        number,
        hash: log
            .block_hash
            .ok_or_else(|| anyhow!("log missing block hash"))?,
    };
    let transaction_hash = log
        .transaction_hash
        .ok_or_else(|| anyhow!("log missing transaction hash"))?;
    let log_index = log.log_index.ok_or_else(|| anyhow!("log missing index"))?;
    use alloy::sol_types::SolEvent;
    let topic = log
        .topic0()
        .ok_or_else(|| anyhow!("log missing signature"))?;
    let event = if *topic == DepositMinted::SIGNATURE_HASH {
        let e = log.log_decode_validate::<DepositMinted>()?.inner.data;
        Event::DepositMinted {
            deposit_id: e.depositId,
            to: e.to,
            amount: e.amount,
        }
    } else if *topic == DepositVoided::SIGNATURE_HASH {
        let e = log.log_decode_validate::<DepositVoided>()?.inner.data;
        Event::DepositVoided {
            deposit_id: e.depositId,
        }
    } else if *topic == RedeemRequested::SIGNATURE_HASH {
        let e = log.log_decode_validate::<RedeemRequested>()?.inner.data;
        Event::RedeemRequested {
            redeem_id: e.redeemId,
            user: e.user,
            amount: e.amount,
            dest_ct: e.destCt,
        }
    } else if *topic == RedeemFulfilled::SIGNATURE_HASH {
        let e = log.log_decode_validate::<RedeemFulfilled>()?.inner.data;
        Event::RedeemFulfilled {
            redeem_id: e.redeemId,
            zcash_txid: e.zcashTxid,
        }
    } else if *topic == RedeemRecredited::SIGNATURE_HASH {
        let e = log.log_decode_validate::<RedeemRecredited>()?.inner.data;
        Event::RedeemRecredited {
            redeem_id: e.redeemId,
            user: e.user,
            amount: e.amount,
        }
    } else {
        return Err(anyhow!("unknown lifecycle signature"));
    };
    Ok(EventObservation {
        block,
        transaction_hash,
        log_index,
        event,
    })
}

#[cfg(test)]
mod tests;
