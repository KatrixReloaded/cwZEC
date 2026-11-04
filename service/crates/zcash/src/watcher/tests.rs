use super::*;
use crate::{
    core::Account,
    rpc::{ChainMetadata, CompactTx},
};
use orchard::{
    bundle::{Authorized, BundleVersion, EffectsOnly, Flags},
    note::{RandomSeed, Rho, TransmittedNoteCiphertext},
    note_encryption::IronwoodNoteEncryption,
    primitives::redpallas::{SpendAuth, VerificationKey},
    value::{NoteValue, ValueCommitment},
};
use zcash_note_encryption::Domain;
use zcash_primitives::transaction::TransactionData;
use zcash_protocol::value::ZatBalance;

#[path = "../../examples/feasibility/fixture.rs"]
mod fixture;

mod profile;

fn base_state() -> WalletState {
    let mut frontier = vec![];
    write_commitment_tree(&Tree::empty(), &mut frontier).unwrap();
    WalletState {
        account: Account {
            birthday: NU7_HEIGHT,
            fvk: fixture::viewing_key().to_bytes(),
            use_qsk: false,
        },
        checkpoint: Checkpoint {
            height: NU7_HEIGHT - 1,
            hash: [7; 32],
            parent: [6; 32],
            frontier,
        },
        notes: vec![],
    }
}

fn action(scope: Scope, rho: [u8; 32], value: u64) -> orchard::Action<()> {
    let fvk = fixture::viewing_key();
    let rho = Rho::from_bytes(&rho).unwrap();
    let rseed = RandomSeed::from_bytes(
        *fixture::note(&fvk, NoteVersion::V3).rseed().as_bytes(),
        &rho,
    )
    .unwrap();
    let note = Note::from_parts(
        fvk.address_at(0_u32, scope),
        NoteValue::from_raw(value),
        rho,
        rseed,
        NoteVersion::V3,
    )
    .unwrap();
    let mut memo = [0_u8; 512];
    memo[..12].copy_from_slice(b"public memo!");
    let enc = IronwoodNoteEncryption::new(None, note, memo);
    orchard::Action::from_parts(
        Nullifier::from_bytes(&rho.to_bytes()).unwrap(),
        VerificationKey::<SpendAuth>::try_from(fixed::<32>(&fvk.to_bytes()[..32], "rk").unwrap())
            .unwrap(),
        ExtractedNoteCommitment::from(note.commitment()),
        TransmittedNoteCiphertext {
            epk_bytes: IronwoodDomain::epk_bytes(enc.epk()).0,
            enc_ciphertext: enc.encrypt_note_plaintext(),
            out_ciphertext: [0; 80],
        },
        ValueCommitment::from_bytes(&fixed::<32>(&fvk.to_bytes()[..32], "cv").unwrap()).unwrap(),
        (),
    )
    .unwrap()
}

fn full(actions: Vec<orchard::Action<()>>) -> Transaction {
    // Dummy authorization is deliberate: these fixtures test scanning and parsing,
    // never consensus validity, proving, signing, submission, or a funded payment.
    let count = actions.len();
    let effects = orchard::Bundle::from_parts(
        nonempty::NonEmpty::from_vec(actions).unwrap(),
        Flags::ENABLED,
        ZatBalance::zero(),
        orchard::Anchor::empty_tree(),
        EffectsOnly,
        BundleVersion::ironwood_v3(),
    )
    .unwrap();
    let bundle = effects.map_authorization(
        &mut (),
        |_, _, _| [0_u8; 64].into(),
        |_, _| {
            Authorized::from_parts(
                orchard::Proof::new(vec![0; orchard::Proof::expected_proof_size(count)]),
                [0_u8; 64].into(),
            )
        },
    );
    TransactionData::from_parts_v6(
        BranchId::for_height(&TEST_NETWORK, NU7_HEIGHT.into()),
        0,
        (NU7_HEIGHT + 120).into(),
        None,
        None,
        None,
        Some(bundle),
    )
    .freeze()
    .unwrap()
}

fn make_block(state: &WalletState, tx: &Transaction) -> CompactBlock {
    let bundle = tx.ironwood_bundle().unwrap();
    CompactBlock {
        height: u64::from(state.checkpoint.height + 1),
        hash: [8; 32].to_vec(),
        prev_hash: state.checkpoint.hash.to_vec(),
        vtx: vec![CompactTx {
            index: 1,
            txid: tx.txid().as_ref().to_vec(),
            ironwood_actions: bundle
                .actions()
                .iter()
                .map(|a| CompactIronwoodAction {
                    nullifier: a.nullifier().to_bytes().to_vec(),
                    cmx: a.cmx().to_bytes().to_vec(),
                    ephemeral_key: a.encrypted_note().epk_bytes.to_vec(),
                    ciphertext: a.encrypted_note().enc_ciphertext.0[..52].to_vec(),
                })
                .collect(),
        }],
        chain_metadata: Some(ChainMetadata {
            ironwood_commitment_tree_size: (decode_tree(&state.checkpoint.frontier).unwrap().size()
                + bundle.actions().len()) as u32,
        }),
    }
}

fn scan(state: &WalletState, block: &CompactBlock, tx: Transaction) -> Result<ScanUpdate> {
    let mut transactions = HashMap::new();
    transactions.insert(fixed(&block.vtx[0].txid, "txid").unwrap(), tx);
    Scan::start(state, block)?.finish(block, &transactions)
}

fn apply(state: &mut WalletState, update: ScanUpdate) {
    for (id, bytes) in update.witnesses {
        state.notes.iter_mut().find(|n| n.id == id).unwrap().witness = bytes;
    }
    state.notes.extend(update.received);
    state
        .notes
        .retain(|n| !update.spent_nullifiers.contains(&n.nullifier));
    state.checkpoint = update.checkpoint;
}

#[test]
fn receives_both_scopes_continues_witnesses_after_reload_and_detects_spend() {
    let mut state = base_state();
    let tx = full(vec![
        action(Scope::External, [1; 32], 51_000),
        action(Scope::Internal, [2; 32], 52_000),
    ]);
    let block = make_block(&state, &tx);
    let update = scan(&state, &block, tx).unwrap();
    assert_eq!(update.received.len(), 2);
    assert!(!update.received[0].internal);
    assert!(update.received[1].internal);
    assert_eq!(update.received[0].position, 0);
    assert_eq!(update.received[1].position, 1);
    assert_eq!(&update.received[0].memo[..12], b"public memo!");
    apply(&mut state, update);
    // A subsequent action spends the first received note and creates change.
    let tx = full(vec![action(
        Scope::Internal,
        state.notes[0].nullifier,
        40_000,
    )]);
    let block = make_block(&state, &tx);
    let update = scan(&state, &block, tx).unwrap();
    assert_eq!(update.spent_nullifiers, vec![state.notes[0].nullifier]);
    assert_eq!(update.witnesses.len(), 2);
    apply(&mut state, update);
    let empty = CompactBlock {
        height: u64::from(state.checkpoint.height + 1),
        hash: [9; 32].to_vec(),
        prev_hash: state.checkpoint.hash.to_vec(),
        vtx: vec![],
        chain_metadata: Some(ChainMetadata {
            ironwood_commitment_tree_size: 3,
        }),
    };
    let update = Scan::start(&state, &empty)
        .unwrap()
        .finish(&empty, &HashMap::new())
        .unwrap();
    assert!(update.received.is_empty());
    assert_eq!(update.witnesses.len(), 2);
    apply(&mut state, update);
    // Cross several witness cursor/subtree boundaries and reload every block.
    for byte in 3..20 {
        let tx = full(vec![action(Scope::Internal, [byte; 32], 30_000)]);
        let block = make_block(&state, &tx);
        let update = scan(&state, &block, tx).unwrap();
        apply(&mut state, update);
    }
    assert_eq!(state.notes.len(), 19);
}

#[test]
fn refuses_wrong_parent_size_and_corrupt_loaded_witness() {
    let mut state = base_state();
    let tx = full(vec![action(Scope::External, [1; 32], 51_000)]);
    let mut block = make_block(&state, &tx);
    block.prev_hash[0] ^= 1;
    assert!(
        Scan::start(&state, &block)
            .err()
            .unwrap()
            .downcast_ref::<Reorg>()
            .is_some()
    );
    block.prev_hash = state.checkpoint.hash.to_vec();
    block
        .chain_metadata
        .as_mut()
        .unwrap()
        .ironwood_commitment_tree_size += 1;
    assert!(scan(&state, &block, tx.clone()).is_err());
    block
        .chain_metadata
        .as_mut()
        .unwrap()
        .ironwood_commitment_tree_size -= 1;
    apply(&mut state, scan(&base_state(), &block, tx).unwrap());
    let tx = full(vec![action(Scope::Internal, [2; 32], 30_000)]);
    let block = make_block(&state, &tx);
    state.notes[0].witness.push(0);
    assert!(Scan::start(&state, &block).is_err());
    state.notes[0].witness.pop();
    state.notes[0].position += 1;
    assert!(Scan::start(&state, &block).is_err());
}

#[test]
fn binds_full_transaction_action_and_authenticated_memo() {
    let state = base_state();
    let good_action = action(Scope::External, [1; 32], 51_000);
    let tx = full(vec![good_action.clone()]);
    let mut block = make_block(&state, &tx);
    let mut bytes = vec![];
    tx.write(&mut bytes).unwrap();
    let raw = RawTransaction {
        height: block.height,
        data: bytes.clone(),
    };
    assert!(parse_transaction(&raw, *tx.txid().as_ref(), NU7_HEIGHT).is_ok());
    assert!(parse_transaction(&raw, [0; 32], NU7_HEIGHT).is_err());
    let mut trailing = raw.clone();
    trailing.data.push(0);
    assert!(parse_transaction(&trailing, *tx.txid().as_ref(), NU7_HEIGHT).is_err());
    let mut wrong_height = raw;
    wrong_height.height += 1;
    assert!(parse_transaction(&wrong_height, *tx.txid().as_ref(), NU7_HEIGHT).is_err());
    // Memo bytes are beyond the compact prefix. A valid compact decryption must
    // never produce a received note if full authenticated decryption fails.
    let mut cipher = good_action.encrypted_note().clone();
    cipher.enc_ciphertext.0[90] ^= 1;
    let tampered_action = orchard::Action::from_parts(
        *good_action.nullifier(),
        good_action.rk().clone(),
        *good_action.cmx(),
        cipher,
        good_action.cv_net().clone(),
        (),
    )
    .unwrap();
    let bad = full(vec![tampered_action]);
    let parsed = Scan::start(&state, &block).unwrap();
    let note = fixture_note_for_test(&good_action, &parsed);
    assert!(
        authenticated_memo(
            &bad,
            0,
            &block.vtx[0].ironwood_actions[0],
            &parsed.ivks,
            Scope::External,
            &note
        )
        .unwrap_err()
        .to_string()
        .contains("authentication failed")
    );
    let mut txs = HashMap::new();
    txs.insert(fixed(&block.vtx[0].txid, "txid").unwrap(), bad);
    assert!(parsed.finish(&block, &txs).is_err());
    block.vtx[0].ironwood_actions[0].cmx = action(Scope::External, [2; 32], 51_000)
        .cmx()
        .to_bytes()
        .to_vec();
    let scan = Scan::start(&state, &block).unwrap();
    let note = fixture_note_for_test(&good_action, &scan);
    assert!(
        authenticated_memo(
            &tx,
            0,
            &block.vtx[0].ironwood_actions[0],
            &scan.ivks,
            Scope::External,
            &note
        )
        .is_err()
    );
}

fn fixture_note_for_test(action: &orchard::Action<()>, scan: &Scan) -> Note {
    scan.decrypt(&CompactAction::from(action)).unwrap().1
}

#[test]
fn rejects_bounded_malformed_codecs_without_panics() {
    // lightwalletd legitimately preserves empty parent slots in the legacy codec.
    let mut empty_with_slots = vec![0, 0, 31];
    empty_with_slots.extend([0; 31]);
    assert_eq!(decode_tree(&empty_with_slots).unwrap().size(), 0);
    assert!(decode_tree(&[0, 0, 253, 255, 255]).is_err());
    let mut tree = vec![0, 1];
    tree.extend([0; 32]);
    tree.push(0);
    assert!(decode_tree(&tree).is_err());
    let mut valid = vec![];
    write_commitment_tree(&Tree::empty(), &mut valid).unwrap();
    valid.push(0);
    assert!(decode_tree(&valid).is_err());
    assert!(decode_witness(&vec![0; 3301]).is_err());
    for length in 0..100 {
        let bytes = vec![255; length];
        assert!(decode_tree(&bytes).is_err());
        assert!(decode_witness(&bytes).is_err());
    }
}

#[tokio::test]
#[ignore = "explicit public-testnet read-only verification; no funds or private keys"]
async fn live_public_fixture_checkpoint_and_next_block() {
    let client = LightwalletClient::connect("https://testnet.zec.rocks:443")
        .await
        .unwrap();
    let tip = client.tip().await.unwrap();
    let watcher = Watcher::new(client);
    let mut state = base_state();
    state.account.birthday = tip;
    state.checkpoint = watcher.initial_checkpoint(tip - 1).await.unwrap();
    let update = watcher.scan_next(&state).await.unwrap();
    assert_eq!(update.checkpoint.height, tip);
    assert!(
        update.received.is_empty(),
        "public fixture unexpectedly found a note; investigate"
    );
    println!(
        "Public testnet checkpoint {} and next block {} verified; no funded-wallet claim",
        tip - 1,
        tip
    );
}
