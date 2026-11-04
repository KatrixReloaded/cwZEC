use super::*;
use crate::core::NoteId;
use incrementalmerkletree::{frontier::CommitmentTree, witness::IncrementalWitness};
use orchard::{
    Note, NoteVersion,
    keys::PreparedIncomingViewingKey,
    note::{RandomSeed, Rho},
    tree::MerkleHashOrchard,
    value::NoteValue,
};
use zcash_note_encryption::try_note_decryption;
use zcash_primitives::{
    merkle_tree::{write_commitment_tree, write_incremental_witness},
    transaction::{
        Transaction,
        sighash::{PrecomputedSighashData, SignableInput},
    },
};

// The parsed transaction has no transparent bundle. Give the upstream sighash
// API its required context type without inventing any transparent inputs.
struct ShieldedOnly;
impl zcash_primitives::transaction::Authorization for ShieldedOnly {
    type TransparentAuth = zcash_transparent::builder::Unauthorized;
    type SaplingAuth = sapling_crypto::bundle::Authorized;
    type OrchardAuth = orchard::bundle::Authorized;
}

fn fixture() -> (SpendingKey, FullViewingKey, Checkpoint, WalletNote, String) {
    // Synthetic, test-only spending key. It is never used on any network.
    let key = SpendingKey::from_zip32_seed(&[7; 32], 1, zip32::AccountId::ZERO).unwrap();
    let fvk = FullViewingKey::from(&key);
    let rho = Rho::from_bytes(&[0; 32]).unwrap();
    let rseed = RandomSeed::from_bytes([9; 32], &rho).unwrap();
    let note = Note::from_parts(
        fvk.address_at(0_u32, Scope::External),
        NoteValue::from_raw(100_000),
        rho,
        rseed,
        NoteVersion::V3,
    )
    .unwrap();
    let mut tree = CommitmentTree::<MerkleHashOrchard, 32>::empty();
    tree.append(MerkleHashOrchard::from_cmx(&ExtractedNoteCommitment::from(
        note.commitment(),
    )))
    .unwrap();
    let witness = IncrementalWitness::from_tree(tree.clone()).unwrap();
    let mut frontier = vec![];
    write_commitment_tree(&tree, &mut frontier).unwrap();
    let mut witness_bytes = vec![];
    write_incremental_witness(&witness, &mut witness_bytes).unwrap();
    let checkpoint = Checkpoint {
        height: 4_540_638,
        hash: [1; 32],
        parent: [2; 32],
        frontier,
    };
    let stored = WalletNote {
        id: NoteId {
            txid: [3; 32],
            action_index: 0,
        },
        recipient: note.recipient().to_raw_address_bytes(),
        value: note.value().inner(),
        rho: rho.to_bytes(),
        rseed: *rseed.as_bytes(),
        nullifier: note.nullifier(&fvk).to_bytes(),
        position: 0,
        internal: false,
        memo: [0; 512],
        mined_height: checkpoint.height - 5,
        spent_height: None,
        witness: witness_bytes,
    };
    let destination = super::super::encode_address(fvk.address_at(1_u32, Scope::External)).unwrap();
    (key, fvk, checkpoint, stored, destination)
}

#[test]
fn rejects_invalid_amount_note_witness_and_height_before_proving() {
    let (_, fvk, checkpoint, note, destination) = fixture();
    for amount in [0, 90_000, 90_001, u64::MAX] {
        assert!(
            payout_builder(
                &fvk,
                &checkpoint,
                std::slice::from_ref(&note),
                &destination,
                amount
            )
            .is_err()
        );
    }
    assert!(
        payout_builder(
            &fvk,
            &checkpoint,
            std::slice::from_ref(&note),
            "not a UA",
            10_000
        )
        .is_err()
    );
    for mutation in 0..7 {
        let mut bad = note.clone();
        match mutation {
            0 => bad.nullifier[0] ^= 1,
            1 => bad.position = 1,
            2 => bad.spent_height = Some(checkpoint.height),
            3 => bad.internal = true,
            4 => bad.rseed = [0; 32],
            5 => bad.witness.push(0),
            _ => bad.mined_height = checkpoint.height + 1,
        }
        assert!(
            payout_builder(
                &fvk,
                &checkpoint,
                std::slice::from_ref(&bad),
                &destination,
                10_000
            )
            .is_err()
        );
    }
    for height in [0, 4_465_025, 499_999_900, u32::MAX] {
        let mut bad = checkpoint.clone();
        bad.height = height;
        assert!(
            payout_builder(
                &fvk,
                &bad,
                std::slice::from_ref(&note),
                &destination,
                10_000
            )
            .is_err()
        );
    }
    let mut bad = checkpoint.clone();
    bad.frontier = vec![0, 0, 0];
    assert!(
        payout_builder(
            &fvk,
            &bad,
            std::slice::from_ref(&note),
            &destination,
            10_000
        )
        .err()
        .unwrap()
        .to_string()
        .contains("checkpoint size mismatch")
    );
    let other = SpendingKey::from_zip32_seed(&[8; 32], 1, zip32::AccountId::ZERO).unwrap();
    assert!(
        payout_builder(
            &FullViewingKey::from(&other),
            &checkpoint,
            std::slice::from_ref(&note),
            &destination,
            10_000
        )
        .is_err()
    );
    assert_eq!(
        payout_builder(
            &fvk,
            &checkpoint,
            std::slice::from_ref(&note),
            &destination,
            10_000
        )
        .unwrap()
        .get_fee(&zip317::FeeRule::standard())
        .unwrap()
        .into_u64(),
        FIXED_FEE_ZAT
    );
}

#[test]
#[ignore = "executes real Halo2 proving; run explicitly once with --release"]
fn synthetic_payout_proves_signs_decrypts_and_validates() {
    let (key, fvk, checkpoint, note, destination) = fixture();
    verify_payout(key, fvk, checkpoint, vec![note], destination);
}

fn verify_payout(
    key: SpendingKey,
    fvk: FullViewingKey,
    checkpoint: Checkpoint,
    notes: Vec<WalletNote>,
    destination: String,
) {
    let start = std::time::Instant::now();
    let fee = fee_for_inputs(notes.len()).unwrap();
    let change = notes.iter().map(|n| n.value).sum::<u64>() - 10_000 - fee;
    let prepared = sign(&key, &checkpoint, &notes, &destination, 10_000).unwrap();
    eprintln!(
        "actual Ironwood build/prove/sign elapsed: {:?}",
        start.elapsed()
    );
    let tx = Transaction::read(std::io::Cursor::new(&prepared.bytes), BranchId::Nu7).unwrap();
    assert!(tx.txid().as_ref() == &prepared.txid);
    assert!(prepared.inputs == notes.iter().map(|n| n.id).collect::<Vec<_>>());
    assert_eq!(prepared.fee, fee);
    assert_eq!(prepared.expiry_height, checkpoint.height + 121);
    assert_eq!(u32::from(tx.expiry_height()), prepared.expiry_height);
    assert_eq!(tx.version(), TxVersion::V6);
    assert_eq!(tx.consensus_branch_id(), BranchId::Nu7);
    assert!(
        tx.transparent_bundle().is_none()
            && tx.sapling_bundle().is_none()
            && tx.orchard_bundle().is_none()
    );
    let bundle = tx.ironwood_bundle().unwrap();
    assert_eq!(bundle.actions().len(), notes.len().max(2));
    assert_eq!(i64::from(*bundle.value_balance()), fee as i64);
    for note in &notes {
        assert_eq!(
            bundle
                .actions()
                .iter()
                .filter(|a| a.nullifier().to_bytes() == note.nullifier)
                .count(),
            1
        );
    }
    for (scope, recipient, expected) in [
        (
            Scope::External,
            decode_address(&destination).unwrap(),
            10_000,
        ),
        (
            Scope::Internal,
            fvk.address_at(0_u32, Scope::Internal),
            change,
        ),
    ] {
        let ivk = PreparedIncomingViewingKey::new(&fvk.to_ivk(scope));
        let outputs: Vec<_> = bundle
            .actions()
            .iter()
            .filter_map(|action| {
                try_note_decryption(
                    &orchard::note_encryption::IronwoodDomain::for_action(action),
                    &ivk,
                    action,
                )
            })
            .collect();
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].0.version(), NoteVersion::V3);
        assert_eq!(outputs[0].0.value().inner(), expected);
        assert_eq!(outputs[0].1, recipient);
        assert_eq!(outputs[0].2, *MemoBytes::empty().as_array());
    }
    let sighash_data = tx.clone().into_data().map_bundles::<ShieldedOnly>(
        |bundle| {
            assert!(bundle.is_none());
            None
        },
        |b| b,
        |b| b,
    );
    let sighash =
        PrecomputedSighashData::new(sighash_data).signature_hash(&SignableInput::Shielded);
    let vk =
        orchard::circuit::VerifyingKey::build(orchard::circuit::OrchardCircuitVersion::PostNu6_3);
    let mut validator = orchard::bundle::BatchValidator::new(&vk);
    validator.add_bundle(bundle, *sighash.as_ref()).unwrap();
    assert!(validator.validate(UnwrapErr(getrandom::SysRng)));
    eprintln!(
        "actual Ironwood proof and authorization validation total elapsed: {:?}",
        start.elapsed()
    );
    // Invalid metadata is rejected without calling any RPC.
    crate::vault::tests::validate_fixture(prepared);
}

fn four_input_fixture() -> (
    SpendingKey,
    FullViewingKey,
    Checkpoint,
    Vec<WalletNote>,
    String,
) {
    let (key, fvk, mut checkpoint, template, destination) = fixture();
    let mut tree = CommitmentTree::<MerkleHashOrchard, 32>::empty();
    let mut witnesses: Vec<IncrementalWitness<MerkleHashOrchard, 32>> = vec![];
    let mut notes = vec![];
    for i in 0..4u8 {
        let mut rho_bytes = [0; 32];
        rho_bytes[0] = i;
        let rho = Rho::from_bytes(&rho_bytes).unwrap();
        let rseed = RandomSeed::from_bytes([9 + i; 32], &rho).unwrap();
        let note = Note::from_parts(
            fvk.address_at(0_u32, Scope::External),
            NoteValue::from_raw(10_000),
            rho,
            rseed,
            NoteVersion::V3,
        )
        .unwrap();
        let cm = MerkleHashOrchard::from_cmx(&ExtractedNoteCommitment::from(note.commitment()));
        tree.append(cm).unwrap();
        for witness in &mut witnesses {
            witness.append(cm).unwrap();
        }
        witnesses.push(IncrementalWitness::from_tree(tree.clone()).unwrap());
        notes.push(WalletNote {
            id: NoteId {
                txid: [i; 32],
                action_index: 0,
            },
            value: 10_000,
            rho: rho.to_bytes(),
            rseed: *rseed.as_bytes(),
            nullifier: note.nullifier(&fvk).to_bytes(),
            position: u64::from(i),
            ..template.clone()
        });
    }
    checkpoint.frontier.clear();
    write_commitment_tree(&tree, &mut checkpoint.frontier).unwrap();
    for (note, witness) in notes.iter_mut().zip(witnesses) {
        note.witness.clear();
        write_incremental_witness(&witness, &mut note.witness).unwrap();
    }
    (key, fvk, checkpoint, notes, destination)
}

#[test]
fn four_input_builder_checks_each_witness_duplicates_fee_and_limit() {
    let (_, fvk, checkpoint, notes, destination) = four_input_fixture();
    assert_eq!(
        payout_builder(&fvk, &checkpoint, &notes, &destination, 10_000)
            .unwrap()
            .get_fee(&zip317::FeeRule::standard())
            .unwrap()
            .into_u64(),
        20_000
    );
    for index in 0..4 {
        let mut bad = notes.clone();
        bad[index].witness.push(0);
        assert!(payout_builder(&fvk, &checkpoint, &bad, &destination, 10_000).is_err());
    }
    let mut bad = notes.clone();
    bad[3] = bad[0].clone();
    assert!(payout_builder(&fvk, &checkpoint, &bad, &destination, 10_000).is_err());
    assert!(fee_for_inputs(0).is_err());
    assert!(fee_for_inputs(MAX_INPUTS + 1).is_err());
    assert_eq!(fee_for_inputs(3).unwrap(), 15_000);
}

#[test]
#[ignore = "executes real Halo2 proving; run explicitly once with --release"]
fn four_input_payout_proves_signs_decrypts_and_validates() {
    let (key, fvk, checkpoint, notes, destination) = four_input_fixture();
    verify_payout(key, fvk, checkpoint, notes, destination);
}
