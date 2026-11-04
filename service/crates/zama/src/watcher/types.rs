use alloy::primitives::{Address, B256, Bytes, U256};
use serde::{Deserialize, Serialize};

pub const SCAN_BLOCKS: u64 = 2048;
pub const RETAIN_BLOCKS: u64 = 128;
pub const MAX_LOGS: usize = 2048;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockRef {
    pub number: u64,
    pub hash: B256,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Block {
    pub reference: BlockRef,
    pub parent_hash: B256,
}

/// Actual ABI facts. Amounts are ciphertext handles, not settlement assertions.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Event {
    DepositMinted {
        deposit_id: B256,
        to: Address,
        amount: B256,
    },
    DepositVoided {
        deposit_id: B256,
    },
    RedeemRequested {
        redeem_id: U256,
        user: Address,
        amount: B256,
        dest_ct: Bytes,
    },
    RedeemFulfilled {
        redeem_id: U256,
        zcash_txid: B256,
    },
    RedeemRecredited {
        redeem_id: U256,
        user: Address,
        amount: B256,
    },
}

#[derive(Clone, PartialEq, Eq)]
pub struct EventObservation {
    pub block: BlockRef,
    pub transaction_hash: B256,
    pub log_index: u64,
    pub event: Event,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReceiptObservation {
    pub transaction_hash: B256,
    pub block: BlockRef,
    pub from: Address,
    pub to: Address,
    pub success: bool,
}

/// Trusted-RPC snapshot: event headers plus a consecutive suffix for rewind.
pub struct ScanBatch {
    pub chain_id: u64,
    pub contract: Address,
    pub previous: BlockRef,
    pub end: BlockRef,
    pub blocks: Vec<Block>,
    pub events: Vec<EventObservation>,
    pub receipts: Vec<ReceiptObservation>,
}

impl ScanBatch {
    /// Historical gaps rely on the RPC's canonical-by-number responses and complete
    /// logs. The entire retained suffix, including the two-confirmation checkpoint,
    /// must still be present. Both watcher and store enforce this before committing.
    pub fn validate_headers(&self) -> anyhow::Result<()> {
        use anyhow::ensure;
        ensure!(
            self.end.number >= self.previous.number
                && self.end.number - self.previous.number <= SCAN_BLOCKS,
            "invalid scan range"
        );
        if self.end.number == self.previous.number {
            ensure!(
                self.end == self.previous && self.blocks.is_empty() && self.events.is_empty(),
                "invalid empty scan"
            );
            return Ok(());
        }
        let mut headers = std::collections::BTreeMap::new();
        for block in &self.blocks {
            ensure!(
                block.reference.number > self.previous.number
                    && block.reference.number <= self.end.number
                    && headers.insert(block.reference.number, block).is_none(),
                "header outside scan or duplicated"
            );
        }
        let start = (self.previous.number + 1).max(self.end.number.saturating_sub(RETAIN_BLOCKS));
        for height in start..=self.end.number {
            ensure!(headers.contains_key(&height), "missing canonical header");
        }
        ensure!(
            headers[&self.end.number].reference == self.end,
            "missing scan end header"
        );
        let mut parent = self.previous;
        for block in headers.values() {
            if block.reference.number == parent.number + 1 {
                ensure!(
                    block.parent_hash == parent.hash,
                    "nonconsecutive canonical headers"
                );
            }
            parent = block.reference;
        }
        for event in &self.events {
            ensure!(
                headers
                    .get(&event.block.number)
                    .is_some_and(|b| b.reference == event.block),
                "event outside canonical scan"
            );
        }
        Ok(())
    }
}
