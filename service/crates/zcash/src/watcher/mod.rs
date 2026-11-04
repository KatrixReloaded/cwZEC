//! Ironwood-only consecutive-block scanner. Persistence and rewind decisions belong to app.
use crate::core::note::{decode_tree, decode_witness, restore_note};

mod availability;

use std::{
    collections::{HashMap, HashSet},
    io::Cursor,
};

use anyhow::{Context, Result, ensure};
use incrementalmerkletree::{frontier::CommitmentTree, witness::IncrementalWitness};
use orchard::{
    Note, NoteVersion,
    keys::{FullViewingKey, PreparedIncomingViewingKey, Scope},
    note::{ExtractedNoteCommitment, Nullifier},
    note_encryption::{CompactAction, IronwoodDomain},
    tree::{MerkleHashOrchard, MerklePath},
};
use zcash_note_encryption::{EphemeralKeyBytes, try_compact_note_decryption, try_note_decryption};
use zcash_primitives::{
    merkle_tree::{write_commitment_tree, write_incremental_witness},
    transaction::Transaction,
};
use zcash_protocol::{
    consensus::{BlockHeight, BranchId, TEST_NETWORK},
    value::Zatoshis,
};

use crate::{
    core::{Checkpoint, NoteId, ScanUpdate, WalletNote, WalletState},
    rpc::{
        CompactBlock, CompactIronwoodAction, LightwalletClient, RawTransaction, TreeState, fixed,
    },
};

type Tree = CommitmentTree<MerkleHashOrchard, 32>;
type Witness = IncrementalWitness<MerkleHashOrchard, 32>;
const NU7_HEIGHT: u32 = 4_465_026;
/// Bound both buffered block/tree data and concurrent requests during catch-up.
pub const SCAN_BATCH_BLOCKS: u32 = 8;

/// Returned without mutating state when the next block no longer extends the checkpoint.
#[derive(Debug)]
pub struct Reorg {
    pub height: u32,
    pub expected_parent: [u8; 32],
    pub actual_parent: [u8; 32],
}
impl std::fmt::Display for Reorg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "chain parent changed at height {}", self.height)
    }
}
impl std::error::Error for Reorg {}

#[derive(Clone)]
pub struct Watcher {
    client: LightwalletClient,
}

impl Watcher {
    pub fn new(client: LightwalletClient) -> Self {
        Self { client }
    }

    pub fn client(&self) -> &LightwalletClient {
        &self.client
    }

    /// Caller chooses the explicit baseline; no birthday or historical range is inferred.
    pub async fn initial_checkpoint(&self, height: u32) -> Result<Checkpoint> {
        ensure!(
            height >= NU7_HEIGHT - 1,
            "checkpoint predates supported NU7 history"
        );
        let block = self.client.block(height).await?;
        let tree_state = self.client.tree(height).await?;
        let tree = verified_tree(&self.client, &block, &tree_state).await?;
        checkpoint(&block, &tree)
    }

    pub async fn scan_next(&self, state: &WalletState) -> Result<ScanUpdate> {
        let height = state
            .checkpoint
            .height
            .checked_add(1)
            .context("scan height overflow")?;
        let (block, tree) = tokio::try_join!(self.client.block(height), self.client.tree(height))?;
        self.scan_prefetched(state, &block, &tree).await
    }

    /// Fetch independent RPC data concurrently; validation and persistence stay sequential.
    pub async fn fetch_batch(
        &self,
        start: u32,
        end: u32,
    ) -> Result<Vec<(CompactBlock, TreeState)>> {
        ensure!(
            start <= end && end - start < SCAN_BATCH_BLOCKS,
            "invalid scan batch range"
        );
        let mut tasks = tokio::task::JoinSet::new();
        for height in start..=end {
            let client = self.client.clone();
            tasks.spawn(async move { tokio::try_join!(client.block(height), client.tree(height)) });
        }
        let mut blocks = Vec::with_capacity((end - start + 1) as usize);
        while let Some(result) = tasks.join_next().await {
            blocks.push(result.context("scan fetch task")??);
        }
        blocks.sort_unstable_by_key(|(block, _)| block.height);
        Ok(blocks)
    }

    pub async fn scan_prefetched(
        &self,
        state: &WalletState,
        block: &CompactBlock,
        evidence: &TreeState,
    ) -> Result<ScanUpdate> {
        let height = state
            .checkpoint
            .height
            .checked_add(1)
            .context("scan height overflow")?;
        // Validate local state and continuity before requesting any private-interest transactions.
        let scan = Scan::start(state, block)?;
        let expected = verified_tree(&self.client, block, evidence).await?;
        let mut transactions = HashMap::new();
        for tx in &block.vtx {
            let mut relevant = false;
            for action in &tx.ironwood_actions {
                let action = compact_action(action)?;
                if scan.decrypt(&action).is_some() {
                    relevant = true;
                }
            }
            if relevant {
                let txid = fixed(&tx.txid, "txid")?;
                let raw = self.client.transaction(txid).await?;
                transactions.insert(txid, parse_transaction(&raw, txid, height)?);
            }
        }
        let update = scan.finish(block, &transactions)?;
        let actual = decode_tree(&update.checkpoint.frontier)?;
        ensure!(
            actual.size() == expected.size() && actual.root() == expected.root(),
            "scanned Ironwood tree disagrees with chain tree"
        );
        Ok(update)
    }
}

fn checkpoint(block: &CompactBlock, tree: &Tree) -> Result<Checkpoint> {
    let mut frontier = vec![];
    write_commitment_tree(tree, &mut frontier)?;
    Ok(Checkpoint {
        height: u32::try_from(block.height)?,
        hash: fixed(&block.hash, "block hash")?,
        parent: fixed(&block.prev_hash, "parent hash")?,
        frontier,
    })
}

fn tree_matches_block(block: &CompactBlock, evidence: &TreeState) -> Result<bool> {
    ensure!(
        evidence.network == "test" && evidence.height == block.height,
        "wrong tree network or height"
    );
    let mut hash: [u8; 32] = fixed(&hex::decode(&evidence.hash)?, "tree block hash")?;
    hash.reverse(); // TreeState uses display hex; CompactBlock uses protocol bytes.
    Ok(hash == fixed::<32>(&block.hash, "block hash")?)
}

/// A stale height lookup may name an orphan. Retry once using the compact block's
/// exact identity; never relabel the old response or accept unchecked tree data.
pub(crate) async fn verified_tree(
    client: &LightwalletClient,
    block: &CompactBlock,
    evidence: &TreeState,
) -> Result<Tree> {
    if !tree_matches_block(block, evidence)? {
        let retry = client
            .tree_by_hash(fixed(&block.hash, "block hash")?)
            .await?;
        return chain_tree(block, &retry);
    }
    chain_tree(block, evidence)
}

pub(crate) fn chain_tree(block: &CompactBlock, evidence: &TreeState) -> Result<Tree> {
    ensure!(
        tree_matches_block(block, evidence)?,
        "tree block hash mismatch"
    );
    ensure!(
        evidence.ironwood_tree.len() <= 2200,
        "oversized Ironwood frontier"
    );
    let tree = decode_tree(&hex::decode(&evidence.ironwood_tree)?)?;
    check_size(block, tree.size())?;
    Ok(tree)
}

fn check_size(block: &CompactBlock, size: usize) -> Result<()> {
    let metadata = block
        .chain_metadata
        .as_ref()
        .context("missing Ironwood tree size")?;
    ensure!(
        u64::try_from(size)? == u64::from(metadata.ironwood_commitment_tree_size),
        "Ironwood tree size mismatch"
    );
    Ok(())
}

pub(crate) fn compact_action(action: &CompactIronwoodAction) -> Result<CompactAction> {
    let nullifier = Option::from(Nullifier::from_bytes(&fixed(
        &action.nullifier,
        "nullifier",
    )?))
    .context("noncanonical nullifier")?;
    let cmx = Option::from(ExtractedNoteCommitment::from_bytes(&fixed(
        &action.cmx,
        "cmx",
    )?))
    .context("noncanonical cmx")?;
    Ok(CompactAction::from_parts(
        nullifier,
        cmx,
        EphemeralKeyBytes(fixed(&action.ephemeral_key, "ephemeral key")?),
        fixed(&action.ciphertext, "compact ciphertext")?,
    ))
}

fn parse_transaction(raw: &RawTransaction, txid: [u8; 32], height: u32) -> Result<Transaction> {
    ensure!(
        raw.height == u64::from(height),
        "full transaction mined height mismatch"
    );
    ensure!(
        !raw.data.is_empty() && raw.data.len() <= 2 * 1024 * 1024,
        "invalid full transaction size"
    );
    let mut reader = Cursor::new(&raw.data);
    let tx = Transaction::read(
        &mut reader,
        BranchId::for_height(&TEST_NETWORK, BlockHeight::from_u32(height)),
    )?;
    ensure!(
        reader.position() == raw.data.len() as u64,
        "trailing full transaction bytes"
    );
    ensure!(
        tx.txid().as_ref() == &txid,
        "full transaction txid mismatch"
    );
    Ok(tx)
}

struct Scan {
    height: u32,
    fvk: FullViewingKey,
    ivks: [(Scope, PreparedIncomingViewingKey); 2],
    tree: Tree,
    witnesses: Vec<(NoteId, Witness)>,
    owned_nullifiers: HashSet<[u8; 32]>,
}

impl Scan {
    fn start(state: &WalletState, block: &CompactBlock) -> Result<Self> {
        let height = state
            .checkpoint
            .height
            .checked_add(1)
            .context("scan height overflow")?;
        ensure!(
            state.account.birthday >= NU7_HEIGHT,
            "unsupported account birthday"
        );
        ensure!(
            height >= state.account.birthday && block.height == u64::from(height),
            "nonconsecutive scan height"
        );
        let parent = fixed(&block.prev_hash, "parent hash")?;
        if parent != state.checkpoint.hash {
            return Err(Reorg {
                height,
                expected_parent: state.checkpoint.hash,
                actual_parent: parent,
            }
            .into());
        }
        let _ = fixed::<32>(&block.hash, "block hash")?;
        let fvk = FullViewingKey::from_bytes(&state.account.fvk).context("invalid viewing key")?;
        let ivks = [Scope::External, Scope::Internal]
            .map(|scope| (scope, PreparedIncomingViewingKey::new(&fvk.to_ivk(scope))));
        let tree = decode_tree(&state.checkpoint.frontier)?;
        let checkpoint_root = tree.root();
        let mut witnesses = vec![];
        let mut owned_nullifiers = HashSet::new();
        let mut ids = HashSet::new();
        let mut positions = HashSet::new();
        for stored in &state.notes {
            ensure!(
                stored.spent_height.is_none() && stored.mined_height <= state.checkpoint.height,
                "loaded note is not live at checkpoint"
            );
            ensure!(
                ids.insert(stored.id) && positions.insert(stored.position),
                "duplicate loaded note"
            );
            let note = restore_note(stored, &fvk)?;
            ensure!(
                owned_nullifiers.insert(stored.nullifier),
                "duplicate note nullifier"
            );
            let witness = decode_witness(&stored.witness)?;
            ensure!(
                u64::from(witness.witnessed_position()) == stored.position,
                "witness position mismatch"
            );
            ensure!(
                u64::from(witness.tip_position()) + 1 == tree.size() as u64,
                "witness checkpoint size mismatch"
            );
            let path = MerklePath::from(witness.path().context("missing witness path")?);
            let cmx = ExtractedNoteCommitment::from(note.commitment());
            ensure!(
                path.root(cmx) == checkpoint_root.into() && witness.root() == checkpoint_root,
                "note authentication path does not match checkpoint"
            );
            witnesses.push((stored.id, witness));
        }
        Ok(Self {
            height,
            fvk,
            ivks,
            tree,
            witnesses,
            owned_nullifiers,
        })
    }

    fn decrypt(&self, action: &CompactAction) -> Option<(Scope, Note)> {
        let domain = IronwoodDomain::for_compact_action(action);
        self.ivks.iter().find_map(|(scope, ivk)| {
            try_compact_note_decryption(&domain, ivk, action).map(|(note, _)| (*scope, note))
        })
    }

    fn finish(
        mut self,
        block: &CompactBlock,
        transactions: &HashMap<[u8; 32], Transaction>,
    ) -> Result<ScanUpdate> {
        let mut received = vec![];
        let mut spent_nullifiers = vec![];
        let mut txids = vec![];
        let mut seen_txids = HashSet::new();
        let mut seen_nullifiers = HashSet::new();
        let mut last_index = None;
        for tx in &block.vtx {
            ensure!(
                last_index.is_none_or(|last| tx.index > last),
                "unordered compact transactions"
            );
            last_index = Some(tx.index);
            let txid = fixed(&tx.txid, "txid")?;
            ensure!(seen_txids.insert(txid), "duplicate compact transaction");
            txids.push(txid);
            for (index, wire) in tx.ironwood_actions.iter().enumerate() {
                let action = compact_action(wire)?;
                let nullifier = action.nullifier().to_bytes();
                ensure!(
                    seen_nullifiers.insert(nullifier),
                    "duplicate block nullifier"
                );
                if self.owned_nullifiers.contains(&nullifier) {
                    spent_nullifiers.push(nullifier);
                }
                let leaf = MerkleHashOrchard::from_cmx(&action.cmx());
                self.tree
                    .append(leaf)
                    .map_err(|_| anyhow::anyhow!("Ironwood tree full"))?;
                for (_, witness) in &mut self.witnesses {
                    witness
                        .append(leaf)
                        .map_err(|_| anyhow::anyhow!("witness full"))?;
                }
                if let Some((scope, note)) = self.decrypt(&action) {
                    let full = transactions
                        .get(&txid)
                        .context("missing detected full transaction")?;
                    ensure!(
                        full.txid().as_ref() == &txid,
                        "full transaction txid mismatch"
                    );
                    let memo = authenticated_memo(full, index, wire, &self.ivks, scope, &note)?;
                    let id = NoteId {
                        txid,
                        action_index: u32::try_from(index)?,
                    };
                    let witness = Witness::from_tree(self.tree.clone())
                        .context("empty received-note tree")?;
                    let nullifier = note.nullifier(&self.fvk).to_bytes();
                    ensure!(
                        self.owned_nullifiers.insert(nullifier),
                        "duplicate received nullifier"
                    );
                    received.push(WalletNote {
                        id,
                        recipient: note.recipient().to_raw_address_bytes(),
                        value: note.value().inner(),
                        rho: note.rho().to_bytes(),
                        rseed: *note.rseed().as_bytes(),
                        nullifier,
                        position: self.tree.size() as u64 - 1,
                        internal: scope == Scope::Internal,
                        memo,
                        mined_height: self.height,
                        spent_height: None,
                        witness: vec![],
                    });
                    self.witnesses.push((id, witness));
                }
            }
        }
        // A spend can precede a received output in compact order; use the final owned set.
        for nullifier in seen_nullifiers {
            if self.owned_nullifiers.contains(&nullifier) && !spent_nullifiers.contains(&nullifier)
            {
                spent_nullifiers.push(nullifier);
            }
        }
        check_size(block, self.tree.size())?;
        let mut witnesses = vec![];
        for (id, witness) in self.witnesses {
            let mut bytes = vec![];
            write_incremental_witness(&witness, &mut bytes)?;
            if let Some(note) = received.iter_mut().find(|note| note.id == id) {
                note.witness = bytes;
            } else {
                witnesses.push((id, bytes));
            }
        }
        Ok(ScanUpdate {
            checkpoint: checkpoint(block, &self.tree)?,
            received,
            witnesses,
            spent_nullifiers,
            txids,
        })
    }
}

fn authenticated_memo(
    tx: &Transaction,
    index: usize,
    compact: &CompactIronwoodAction,
    ivks: &[(Scope, PreparedIncomingViewingKey); 2],
    scope: Scope,
    expected: &Note,
) -> Result<[u8; 512]> {
    let bundle = tx
        .ironwood_bundle()
        .context("missing full Ironwood bundle")?;
    let action = bundle
        .actions()
        .get(index)
        .context("full action index mismatch")?;
    ensure!(
        action.nullifier().to_bytes().as_slice() == compact.nullifier
            && action.cmx().to_bytes().as_slice() == compact.cmx
            && action.encrypted_note().epk_bytes.as_slice() == compact.ephemeral_key
            && action.encrypted_note().enc_ciphertext.0[..52] == compact.ciphertext,
        "compact/full action mismatch"
    );
    let ivk = &ivks
        .iter()
        .find(|(s, _)| *s == scope)
        .context("missing scope")?
        .1;
    let (note, recipient, memo) =
        try_note_decryption(&IronwoodDomain::for_action(action), ivk, action)
            .context("full note/memo authentication failed")?;
    ensure!(
        &note == expected && recipient == expected.recipient() && note.version() == NoteVersion::V3,
        "full decrypted note mismatch"
    );
    Zatoshis::from_u64(note.value().inner()).context("note value out of range")?;
    Ok(memo)
}

#[cfg(test)]
mod tests;
