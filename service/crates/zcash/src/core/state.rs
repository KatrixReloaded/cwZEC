//! State passed between chain operations and application-owned PostgreSQL storage.
//! Every note is Ironwood V3; this service does not manage other pools.

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct NoteId {
    pub txid: [u8; 32],
    pub action_index: u32,
}

#[derive(Clone, PartialEq, Eq)]
pub struct Account {
    pub birthday: u32,
    pub fvk: [u8; 96],
    pub use_qsk: bool,
}

#[derive(Clone, PartialEq, Eq)]
pub struct Checkpoint {
    pub height: u32,
    pub hash: [u8; 32],
    pub parent: [u8; 32],
    /// Upstream commitment-tree encoding, version 1 of our codec selection.
    pub frontier: Vec<u8>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct WalletNote {
    pub id: NoteId,
    pub recipient: [u8; 43],
    pub value: u64,
    pub rho: [u8; 32],
    pub rseed: [u8; 32],
    pub nullifier: [u8; 32],
    pub position: u64,
    pub internal: bool,
    pub memo: [u8; 512],
    pub mined_height: u32,
    pub spent_height: Option<u32>,
    /// Upstream incremental-witness encoding at the loaded checkpoint.
    pub witness: Vec<u8>,
}

pub struct WalletState {
    pub account: Account,
    pub checkpoint: Checkpoint,
    pub notes: Vec<WalletNote>,
}

pub struct ScanUpdate {
    pub checkpoint: Checkpoint,
    pub received: Vec<WalletNote>,
    pub witnesses: Vec<(NoteId, Vec<u8>)>,
    pub spent_nullifiers: Vec<[u8; 32]>,
    /// Transaction identifiers in the block, including outgoing vault transactions.
    pub txids: Vec<[u8; 32]>,
}

pub struct PreparedTransaction {
    pub bytes: Vec<u8>,
    pub txid: [u8; 32],
    pub inputs: Vec<NoteId>,
    pub expiry_height: u32,
    pub fee: u64,
}
