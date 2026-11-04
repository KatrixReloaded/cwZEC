//! Canonical Ironwood note and upstream tree/witness decoding shared by scanning and signing.
use super::WalletNote;
use anyhow::{Context, Result, bail, ensure};
use incrementalmerkletree::{frontier::CommitmentTree, witness::IncrementalWitness};
use orchard::{
    Address, Note, NoteVersion,
    keys::{FullViewingKey, Scope},
    note::{RandomSeed, Rho},
    tree::MerkleHashOrchard,
    value::NoteValue,
};
use std::io::Cursor;
use zcash_primitives::merkle_tree::{
    read_commitment_tree, read_incremental_witness, write_commitment_tree,
};
use zcash_protocol::value::Zatoshis;
type Tree = CommitmentTree<MerkleHashOrchard, 32>;
type Witness = IncrementalWitness<MerkleHashOrchard, 32>;

pub(crate) fn restore_note(stored: &WalletNote, fvk: &FullViewingKey) -> Result<Note> {
    Zatoshis::from_u64(stored.value).context("stored note value out of range")?;
    let recipient = Option::from(Address::from_raw_address_bytes(&stored.recipient))
        .context("invalid stored recipient")?;
    let scope = if stored.internal {
        Scope::Internal
    } else {
        Scope::External
    };
    ensure!(
        fvk.scope_for_address(&recipient) == Some(scope),
        "stored note scope mismatch"
    );
    let rho = Option::from(Rho::from_bytes(&stored.rho)).context("invalid stored rho")?;
    let rseed =
        Option::from(RandomSeed::from_bytes(stored.rseed, &rho)).context("invalid stored rseed")?;
    let note = Option::<Note>::from(Note::from_parts(
        recipient,
        NoteValue::from_raw(stored.value),
        rho,
        rseed,
        NoteVersion::V3,
    ))
    .context("invalid stored V3 note")?;
    ensure!(
        note.nullifier(fvk).to_bytes() == stored.nullifier,
        "stored nullifier mismatch"
    );
    Ok(note)
}

// Preflight the upstream legacy codec before it allocates vectors. At depth 32 all
// vector lengths fit a canonical single-byte CompactSize; optional nodes are 0/1.
fn codec_node(bytes: &[u8], pos: &mut usize) -> Result<()> {
    let flag = *bytes.get(*pos).context("truncated optional node")?;
    *pos += 1;
    match flag {
        0 => (),
        1 => {
            *pos = pos.checked_add(32).context("codec overflow")?;
            ensure!(*pos <= bytes.len(), "truncated node");
        }
        _ => bail!("invalid optional-node flag"),
    }
    Ok(())
}

fn codec_count(bytes: &[u8], pos: &mut usize) -> Result<usize> {
    let count = usize::from(*bytes.get(*pos).context("missing codec length")?);
    *pos += 1;
    ensure!(count <= 32, "oversized tree/witness vector");
    Ok(count)
}

fn codec_tree(bytes: &[u8], pos: &mut usize) -> Result<()> {
    let left = *bytes.get(*pos).context("missing left node")?;
    codec_node(bytes, pos)?;
    let right = *bytes.get(*pos).context("missing right node")?;
    codec_node(bytes, pos)?;
    let count = codec_count(bytes, pos)?;
    ensure!(count < 32, "tree exceeds depth");
    let mut occupied_parent = false;
    for _ in 0..count {
        occupied_parent |= *bytes.get(*pos).context("missing parent node")? == 1;
        codec_node(bytes, pos)?;
    }
    ensure!(
        left == 1 || (right == 0 && !occupied_parent),
        "invalid empty tree structure"
    );
    Ok(())
}

pub(crate) fn decode_tree(bytes: &[u8]) -> Result<Tree> {
    ensure!(bytes.len() <= 1100, "oversized frontier codec");
    let mut pos = 0;
    codec_tree(bytes, &mut pos)?;
    ensure!(pos == bytes.len(), "trailing frontier bytes");
    let tree: Tree = read_commitment_tree(Cursor::new(bytes))?;
    // Reject malformed gaps/canonical forms, which could cause legacy tree panics.
    let mut encoded = vec![];
    write_commitment_tree(&tree, &mut encoded)?;
    ensure!(encoded == bytes, "noncanonical frontier codec");
    Ok(tree)
}

pub(crate) fn decode_witness(bytes: &[u8]) -> Result<Witness> {
    ensure!(bytes.len() <= 3300, "oversized witness codec");
    let mut pos = 0;
    codec_tree(bytes, &mut pos)?;
    let count = codec_count(bytes, &mut pos)?;
    pos += count * 32;
    let flag = *bytes.get(pos).context("truncated witness cursor")?;
    pos += 1;
    match flag {
        0 => (),
        1 => codec_tree(bytes, &mut pos)?,
        _ => bail!("invalid cursor flag"),
    }
    ensure!(pos == bytes.len(), "trailing witness bytes");
    let witness: Witness = read_incremental_witness(Cursor::new(bytes))?;
    ensure!(witness.tree().size() > 0, "empty witness tree");
    let position = witness.tree().size() as u64 - 1;
    let future_depths: Vec<_> = (0..32)
        .filter(|depth| position & (1_u64 << depth) == 0)
        .collect();
    ensure!(
        witness.filled().len() <= future_depths.len(),
        "too many filled witness subtrees"
    );
    if let Some(cursor) = witness.cursor() {
        let depth = *future_depths
            .get(witness.filled().len())
            .context("unexpected full witness cursor")?;
        ensure!(
            cursor.size() > 0 && (cursor.size() as u64) < (1_u64 << depth),
            "invalid witness cursor size"
        );
    }
    Ok(witness)
}
