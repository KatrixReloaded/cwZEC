use alloy::primitives::{Address, B256, Bytes, U256};
use zama::watcher::{BlockRef, Event, EventObservation};

pub const REDEEM_FEE_ZAT: u64 = zcash::vault::FIXED_FEE_ZAT;
pub const MIN_PAYOUT_ZAT: u64 = 1;
// Keep one transaction fee of operator-funded reserve separate from user backing.
pub const MIN_FEE_RESERVE_ZAT: u64 = zcash::vault::FIXED_FEE_ZAT;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RedeemState {
    Ready,
    Zero,
    Rejected,
    Reserved,
    Paid,
    Fulfilled,
    RecreditPending,
    Recredited,
}

#[derive(Clone)]
pub struct BridgeRedeem {
    pub id: U256,
    pub burner: Address,
    pub handle: B256,
    pub dest_ct: Bytes,
    pub source: BlockRef,
    pub source_tx: B256,
    pub source_log: u64,
    pub canonical: bool,
    pub actual: u64,
    pub destination: Option<String>,
    pub state: RedeemState,
    pub version: u64,
    pub operation_id: String,
}

impl BridgeRedeem {
    pub fn event(&self) -> EventObservation {
        EventObservation {
            block: self.source,
            transaction_hash: self.source_tx,
            log_index: self.source_log,
            event: Event::RedeemRequested {
                redeem_id: self.id,
                user: self.burner,
                amount: self.handle,
                dest_ct: self.dest_ct.clone(),
            },
        }
    }
    pub fn recredit_id(&self) -> String {
        format!("cwzec/recredit/{}", self.id)
    }
    pub fn fulfillment_id(&self) -> String {
        format!("cwzec/fulfill/{}", self.id)
    }
}
