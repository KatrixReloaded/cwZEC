//! Complete Ironwood enumeration under the existing trusted-provider assumption.
//! This is evidence collection only; it grants no recovery authorization.
use std::{collections::HashSet, io::Cursor};

use anyhow::{Context, Result, ensure};
use orchard::tree::MerkleHashOrchard;
use serde::{Deserialize, Serialize};
use zcash_primitives::{
    merkle_tree::write_commitment_tree,
    transaction::{Transaction, TxVersion},
};
use zcash_protocol::consensus::BranchId;

use super::{LightwalletClient, fixed};
use crate::{
    core::{PreparedTransaction, note::decode_tree},
    watcher::{compact_action, verified_tree},
};

pub const LOWER_HEIGHT: u32 = 4_465_026;
pub const MAX_BATCH_BLOCKS: u32 = 16;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Identity {
    txid: [u8; 32],
    expiry: u32,
    // Exact bytes bind authorization as well as transaction effects across restarts.
    bytes: Vec<u8>,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("txid", &self.txid)
            .field("expiry", &self.expiry)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inclusion {
    pub txid: [u8; 32],
    pub height: u32,
    pub block_hash: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct History {
    attempts: Vec<Identity>,
    baseline_hash: [u8; 32],
    cursor_height: u32,
    cursor_hash: [u8; 32],
    frontier: Vec<u8>,
    expiry_max: u32,
    checkpoint_height: u32,
    checkpoint_hash: [u8; 32],
    inclusions: Vec<Inclusion>,
}
impl History {
    pub fn cursor_height(&self) -> u32 {
        self.cursor_height
    }
    pub fn checkpoint_height(&self) -> u32 {
        self.checkpoint_height
    }
    pub fn checkpoint_hash(&self) -> [u8; 32] {
        self.checkpoint_hash
    }
    pub fn complete_absence(&self) -> bool {
        self.cursor_height == self.checkpoint_height && self.inclusions.is_empty()
    }
    pub fn inclusions(&self) -> &[Inclusion] {
        &self.inclusions
    }
    pub fn matches_attempts(&self, attempts: &[PreparedTransaction]) -> Result<()> {
        let (identities, _) = validate(attempts)?;
        ensure!(
            identities == self.attempts,
            "history issued attempts changed"
        );
        Ok(())
    }
}

/// A previously pinned canonical identity changed, including a tip below C.
#[derive(Debug)]
pub struct HistoryReorg {
    pub height: u32,
}
impl std::fmt::Display for HistoryReorg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "history canonical identity changed at {}", self.height)
    }
}
impl std::error::Error for HistoryReorg {}

fn parse(attempt: &PreparedTransaction) -> Result<Transaction> {
    ensure!(
        !attempt.bytes.is_empty() && attempt.bytes.len() <= 2 * 1024 * 1024,
        "invalid issued transaction size"
    );
    let mut reader = Cursor::new(&attempt.bytes);
    let tx = Transaction::read(&mut reader, BranchId::Nu7)?;
    ensure!(
        reader.position() == attempt.bytes.len() as u64,
        "trailing issued transaction bytes"
    );
    ensure!(
        tx.version() == TxVersion::V6 && tx.consensus_branch_id() == BranchId::Nu7,
        "issued transaction must be V6 NU7"
    );
    ensure!(
        tx.txid().as_ref() == &attempt.txid
            && u32::from(tx.expiry_height()) == attempt.expiry_height,
        "issued transaction identity mismatch"
    );
    ensure!(
        tx.ironwood_bundle()
            .is_some_and(|bundle| !bundle.actions().is_empty()),
        "issued transaction requires Ironwood actions"
    );
    ensure!(
        attempt.expiry_height >= LOWER_HEIGHT,
        "unsupported issued expiry"
    );
    attempt
        .expiry_height
        .checked_add(3)
        .context("issued burial overflow")?;
    Ok(tx)
}
fn validate(attempts: &[PreparedTransaction]) -> Result<(Vec<Identity>, u32)> {
    ensure!(!attempts.is_empty(), "no issued attempts");
    let mut identities = Vec::new();
    let mut seen = HashSet::new();
    let mut max = 0;
    for attempt in attempts {
        parse(attempt)?;
        ensure!(seen.insert(attempt.txid), "duplicate issued txid");
        max = max.max(attempt.expiry_height);
        identities.push(Identity {
            txid: attempt.txid,
            expiry: attempt.expiry_height,
            bytes: attempt.bytes.clone(),
        });
    }
    Ok((identities, max))
}
pub fn validate_attempts(attempts: &[PreparedTransaction]) -> Result<()> {
    validate(attempts).map(|_| ())
}

pub async fn start(
    client: &LightwalletClient,
    attempts: &[PreparedTransaction],
) -> Result<History> {
    let (identities, expiry_max) = validate(attempts)?;
    let checkpoint_height = expiry_max.checked_add(3).context("burial overflow")?;
    ensure!(
        client.tip().await? >= checkpoint_height,
        "issued expiry not buried"
    );
    let checkpoint_hash = client.block_hash(checkpoint_height).await?;
    let baseline = client.block(LOWER_HEIGHT - 1).await?;
    let baseline_hash = fixed(&baseline.hash, "baseline hash")?;
    fixed::<32>(&baseline.prev_hash, "baseline parent")?;
    let tree = verified_tree(client, &baseline, &client.tree(LOWER_HEIGHT - 1).await?).await?;
    let mut frontier = vec![];
    write_commitment_tree(&tree, &mut frontier)?;
    let history = History {
        attempts: identities,
        baseline_hash,
        cursor_height: LOWER_HEIGHT - 1,
        cursor_hash: baseline_hash,
        frontier,
        expiry_max,
        checkpoint_height,
        checkpoint_hash,
        inclusions: vec![],
    };
    revalidate(client, attempts, &history).await?;
    Ok(history)
}

pub async fn revalidate(
    client: &LightwalletClient,
    attempts: &[PreparedTransaction],
    history: &History,
) -> Result<()> {
    history.matches_attempts(attempts)?;
    ensure!(
        history.expiry_max
            == attempts
                .iter()
                .map(|a| a.expiry_height)
                .max()
                .context("no attempts")?
            && history.expiry_max.checked_add(3) == Some(history.checkpoint_height)
            && (LOWER_HEIGHT - 1..=history.checkpoint_height).contains(&history.cursor_height),
        "invalid persisted history bounds"
    );
    if client.tip().await? < history.checkpoint_height {
        return Err(HistoryReorg {
            height: history.checkpoint_height,
        }
        .into());
    }
    for (height, expected) in [
        (LOWER_HEIGHT - 1, history.baseline_hash),
        (history.cursor_height, history.cursor_hash),
        (history.checkpoint_height, history.checkpoint_hash),
    ] {
        if client.block_hash(height).await? != expected {
            return Err(HistoryReorg { height }.into());
        }
    }
    let block = client.block(history.cursor_height).await?;
    if fixed::<32>(&block.hash, "cursor hash")? != history.cursor_hash {
        return Err(HistoryReorg {
            height: history.cursor_height,
        }
        .into());
    }
    let expected =
        verified_tree(client, &block, &client.tree(history.cursor_height).await?).await?;
    let tree = decode_tree(&history.frontier)?;
    ensure!(
        expected.size() == tree.size() && expected.root() == tree.root(),
        "persisted history frontier mismatch"
    );
    if client.tip().await? < history.checkpoint_height {
        return Err(HistoryReorg {
            height: history.checkpoint_height,
        }
        .into());
    }
    Ok(())
}

/// Returns new progress only after the entire bounded batch and canonical rereads succeed.
pub async fn advance(
    client: &LightwalletClient,
    attempts: &[PreparedTransaction],
    history: &History,
    max_blocks: u32,
) -> Result<History> {
    ensure!(
        (1..=MAX_BATCH_BLOCKS).contains(&max_blocks),
        "invalid history batch budget"
    );
    revalidate(client, attempts, history).await?;
    let mut next = history.clone();
    let mut tree = decode_tree(&history.frontier)?;
    let end = history
        .cursor_height
        .saturating_add(max_blocks)
        .min(history.checkpoint_height);
    while next.cursor_height < end {
        let height = next
            .cursor_height
            .checked_add(1)
            .context("history height overflow")?;
        let block = client.block(height).await?;
        let hash = fixed(&block.hash, "block hash")?;
        if fixed::<32>(&block.prev_hash, "block parent")? != next.cursor_hash {
            return Err(HistoryReorg { height }.into());
        }
        let mut last = None;
        let mut txids = HashSet::new();
        let mut nullifiers = HashSet::new();
        for compact in &block.vtx {
            ensure!(
                last.is_none_or(|i| compact.index > i),
                "unordered compact transaction index"
            );
            last = Some(compact.index);
            let txid = fixed::<32>(&compact.txid, "compact txid")?;
            ensure!(txids.insert(txid), "duplicate compact txid");
            if let Some(attempt) = attempts.iter().find(|a| a.txid == txid) {
                ensure!(
                    height <= attempt.expiry_height,
                    "issued transaction observed after expiry"
                );
                ensure!(
                    !next.inclusions.iter().any(|i| i.txid == txid),
                    "issued transaction included twice"
                );
                let full = parse(attempt)?;
                let bundle = full.ironwood_bundle().context("missing issued bundle")?;
                ensure!(
                    bundle.actions().len() == compact.ironwood_actions.len(),
                    "issued compact action count mismatch"
                );
                for (full, wire) in bundle.actions().iter().zip(&compact.ironwood_actions) {
                    ensure!(
                        full.nullifier().to_bytes().as_slice() == wire.nullifier
                            && full.cmx().to_bytes().as_slice() == wire.cmx
                            && full.encrypted_note().epk_bytes.as_slice() == wire.ephemeral_key
                            && full.encrypted_note().enc_ciphertext.0[..52] == wire.ciphertext,
                        "issued compact action mismatch"
                    );
                }
                next.inclusions.push(Inclusion {
                    txid,
                    height,
                    block_hash: hash,
                });
            }
            for wire in &compact.ironwood_actions {
                let action = compact_action(wire)?;
                ensure!(
                    nullifiers.insert(action.nullifier().to_bytes()),
                    "duplicate block nullifier"
                );
                tree.append(MerkleHashOrchard::from_cmx(&action.cmx()))
                    .map_err(|_| anyhow::anyhow!("Ironwood tree full"))?;
            }
        }
        let expected = verified_tree(client, &block, &client.tree(height).await?).await?;
        ensure!(
            expected.size() == tree.size() && expected.root() == tree.root(),
            "history Ironwood tree mismatch"
        );
        next.cursor_height = height;
        next.cursor_hash = hash;
    }
    next.frontier.clear();
    write_commitment_tree(&tree, &mut next.frontier)?;
    revalidate(client, attempts, &next).await?;
    Ok(next)
}

#[cfg(test)]
#[path = "history_tests.rs"]
mod tests;
