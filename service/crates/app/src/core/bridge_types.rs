//! Application evidence and durable deposit state; no payment authorization.
use alloy::primitives::{Address, B256};
use zama::{
    contract::DepositStatus,
    watcher::{BlockRef, EventObservation},
};
use zcash::core::NoteId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BridgeDomain {
    pub chain_id: u64,
    pub contract: Address,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DepositState {
    Eligible,
    RefundPending,
    MintPending,
    Minted,
    Voided,
    Mismatch,
}
#[derive(Clone)]
pub struct BridgeDeposit {
    pub deposit_id: B256,
    pub note_id: NoteId,
    pub amount: u64,
    pub recipient: Option<Address>,
    pub source_height: u32,
    pub source_hash: [u8; 32],
    pub canonical: bool,
    pub state: DepositState,
    pub version: u64,
    pub operation_id: String,
    pub claimed: bool,
    pub acted_on: bool,
}
pub(crate) struct ConsumptionEvidence {
    pub checkpoint: BlockRef,
    pub status: DepositStatus,
    pub event: Option<EventObservation>,
    pub actual_amount: Option<u64>,
    pub actual_handle: Option<B256>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BridgeCheckpointPair {
    pub zcash_height: u32,
    pub zcash_hash: [u8; 32],
    pub checkpoint: BlockRef,
}
pub(crate) struct AccountingEvidence {
    pub zcash_height: u32,
    pub zcash_hash: [u8; 32],
    pub checkpoint: BlockRef,
    pub supply_handle: B256,
    pub supply: u64,
}
#[derive(Debug)]
pub struct AccountingReport {
    pub id: i64,
    pub assets: u128,
    pub supply: u64,
    pub deposit_liability: u128,
    pub unpaid_redemptions: u128,
    pub fee_reserve: Option<u128>,
    pub coverage_complete: bool,
    pub unsupported: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefundState {
    Authorized,
    Reserved,
    Paid,
}

#[derive(Clone)]
pub struct BridgeRefund {
    pub deposit_id: B256,
    pub participant: String,
    pub destination: String,
    pub gross_amount: u64,
    pub fee: u64,
    pub state: RefundState,
    pub operation_id: String,
    pub void_operation_id: String,
    pub version: u64,
}
