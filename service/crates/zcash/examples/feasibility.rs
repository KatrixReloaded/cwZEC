//! Offline feasibility only: public fixtures, no wallet creation, RPC, SQL, or signing.
//! Run with `cargo run -p zcash --example feasibility` from `service/`.

use std::io::{self, Cursor};

use incrementalmerkletree::{frontier::CommitmentTree, witness::IncrementalWitness};
use orchard::{
    Address, Note,
    keys::{FullViewingKey, Scope},
    note::{ExtractedNoteCommitment, NoteVersion, RandomSeed, Rho},
    tree::{MerkleHashOrchard, MerklePath},
    value::NoteValue,
};
use pczt::{Pczt, roles::creator::Creator};
use zcash_primitives::merkle_tree::{
    read_commitment_tree, read_incremental_witness, write_commitment_tree,
    write_incremental_witness,
};
use zcash_protocol::consensus::{BlockHeight, BranchId, TEST_NETWORK};

#[path = "feasibility/fixture.rs"]
mod fixture;
#[path = "feasibility/ironwood.rs"]
mod ironwood;

type Tree = CommitmentTree<MerkleHashOrchard, 32>;
type Witness = IncrementalWitness<MerkleHashOrchard, 32>;

// These fields map directly to constrained PostgreSQL BYTEA/SMALLINT/BIGINT columns.
// This is a probe record, not a new wallet serialization format.
struct NoteColumns {
    recipient: [u8; 43],
    value: i64,
    rho: [u8; 32],
    rseed: [u8; 32],
    version: u8,
}

impl NoteColumns {
    fn from_note(note: &Note) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            recipient: note.recipient().to_raw_address_bytes(),
            value: i64::try_from(note.value().inner())?,
            rho: note.rho().to_bytes(),
            rseed: *note.rseed().as_bytes(),
            version: match note.version() {
                NoteVersion::V2 => 2,
                NoteVersion::V3 => 3,
            },
        })
    }

    fn into_note(self) -> Result<Note, Box<dyn std::error::Error>> {
        let recipient = Option::<Address>::from(Address::from_raw_address_bytes(&self.recipient))
            .ok_or_else(|| invalid("recipient"))?;
        let rho = Option::<Rho>::from(Rho::from_bytes(&self.rho)).ok_or_else(|| invalid("rho"))?;
        let rseed = Option::<RandomSeed>::from(RandomSeed::from_bytes(self.rseed, &rho))
            .ok_or_else(|| invalid("rseed"))?;
        let version = match self.version {
            2 => NoteVersion::V2,
            3 => NoteVersion::V3,
            _ => return Err(invalid("note version").into()),
        };
        Option::<Note>::from(Note::from_parts(
            recipient,
            NoteValue::from_raw(u64::try_from(self.value)?),
            rho,
            rseed,
            version,
        ))
        .ok_or_else(|| invalid("note").into())
    }
}

fn invalid(field: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, field)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    ironwood::verify()?;
    let fvk = fixture::viewing_key();
    let fvk_bytes = fvk.to_bytes();
    let restored_fvk = FullViewingKey::from_bytes(&fvk_bytes).ok_or_else(|| invalid("fvk"))?;
    assert_eq!(restored_fvk.to_bytes(), fvk_bytes);

    // Both note versions use the same primitives; this does not authorize a pool change.
    for version in [NoteVersion::V2, NoteVersion::V3] {
        let note = fixture::note(&fvk, version);
        let nullifier = note.nullifier(&fvk).to_bytes();
        let restored = NoteColumns::from_note(&note)?.into_note()?;
        assert_eq!(restored, note);
        assert_eq!(restored.nullifier(&restored_fvk).to_bytes(), nullifier);
        assert_eq!(
            fvk.scope_for_address(&restored.recipient()),
            Some(Scope::External)
        );

        let cmx = ExtractedNoteCommitment::from(note.commitment());
        let leaf = MerkleHashOrchard::from_cmx(&cmx);
        let mut tree = Tree::empty();
        tree.append(leaf).map_err(|_| invalid("full tree"))?;
        let mut witness = Witness::from_tree(tree.clone()).ok_or_else(|| invalid("witness"))?;
        // A later commitment must update both tree and every retained note witness.
        tree.append(leaf).map_err(|_| invalid("full tree"))?;
        witness.append(leaf).map_err(|_| invalid("full witness"))?;

        let mut tree_bytes = Vec::new();
        let mut witness_bytes = Vec::new();
        write_commitment_tree(&tree, &mut tree_bytes)?;
        write_incremental_witness(&witness, &mut witness_bytes)?;
        let mut tree_reader = Cursor::new(tree_bytes.as_slice());
        let mut witness_reader = Cursor::new(witness_bytes.as_slice());
        let mut restored_tree: Tree = read_commitment_tree(&mut tree_reader)?;
        let mut restored_witness: Witness = read_incremental_witness(&mut witness_reader)?;
        assert_eq!(tree_reader.position(), tree_bytes.len() as u64);
        assert_eq!(witness_reader.position(), witness_bytes.len() as u64);
        assert_eq!(restored_tree.root(), restored_witness.root());
        assert_eq!(restored_tree.root(), tree.root());
        assert_eq!(
            restored_witness.witnessed_position(),
            witness.witnessed_position()
        );
        let path = MerklePath::from(restored_witness.path().ok_or_else(|| invalid("path"))?);
        assert_eq!(path.root(cmx).to_bytes(), restored_tree.root().to_bytes());
        // Continue scanning after reload, preserving the original note's witness.
        let previous_root = restored_tree.root();
        restored_tree
            .append(leaf)
            .map_err(|_| invalid("full restored tree"))?;
        restored_witness
            .append(leaf)
            .map_err(|_| invalid("full restored witness"))?;
        assert_ne!(restored_tree.root(), previous_root);
        assert_eq!(restored_witness.root(), restored_tree.root());
        let continued_path = MerklePath::from(
            restored_witness
                .path()
                .ok_or_else(|| invalid("continued path"))?,
        );
        assert_eq!(
            continued_path.root(cmx).to_bytes(),
            restored_tree.root().to_bytes()
        );
        assert!(
            read_incremental_witness::<MerkleHashOrchard, _, 32>(
                &witness_bytes[..witness_bytes.len() - 1]
            )
            .is_err()
        );
    }

    // Use a public protocol activation height, not a fabricated live tip.
    let branch = BranchId::for_height(&TEST_NETWORK, BlockHeight::from_u32(4_465_026));
    assert_eq!(u32::from(branch), 0x7719_0ad9);
    let pczt = Creator::new(u32::from(branch), 4_465_066, 1, None, None)
        .map_err(|e| invalid(&format!("PCZT creator: {e:?}")))?
        .build()
        .map_err(|e| invalid(&format!("PCZT build: {e:?}")))?;
    assert_eq!(*pczt.global().tx_version(), 6);
    let bytes = pczt
        .serialize()
        .map_err(|e| invalid(&format!("PCZT encoding: {e:?}")))?;
    let restored = Pczt::parse(&bytes)?;
    assert_eq!(*restored.global().consensus_branch_id(), 0x7719_0ad9);
    assert_eq!(
        restored
            .serialize()
            .map_err(|e| invalid(&format!("PCZT re-encoding: {e:?}")))?,
        bytes
    );
    println!("Public-fixture note/FVK/nullifier and tree/witness reload verified (V2, V3).");
    println!("NU7 V6 empty PCZT round trip verified; spend/prove/sign/extract unverified.");
    println!("PostgreSQL byte boundary only; no database durability or public payment claimed.");
    Ok(())
}
