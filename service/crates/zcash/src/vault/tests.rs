use super::*;
use crate::core::{Account, Checkpoint, NoteId};

fn note(index: u8, value: u64, mined_height: u32) -> WalletNote {
    WalletNote {
        id: NoteId {
            txid: [index; 32],
            action_index: 0,
        },
        recipient: [0; 43],
        value,
        rho: [0; 32],
        rseed: [0; 32],
        nullifier: [index; 32],
        position: u64::from(index),
        internal: false,
        memo: [0; 512],
        mined_height,
        spent_height: None,
        witness: vec![],
    }
}

#[test]
fn selection_rejects_reserved_spent_unconfirmed_fragmented_and_zero_change() {
    let mut state = WalletState {
        account: Account {
            birthday: 1,
            fvk: [0; 96],
            use_qsk: false,
        },
        checkpoint: Checkpoint {
            height: 100,
            hash: [0; 32],
            parent: [0; 32],
            frontier: vec![],
        },
        notes: vec![
            note(1, 50_000, 90),
            note(2, 40_000, 99),
            note(3, 30_000, 90),
        ],
    };
    let mut reserved = HashSet::new();
    assert!(select_input(&state, &reserved, 3, 20_000).unwrap().id == state.notes[0].id);
    reserved.insert(state.notes[0].nullifier);
    assert!(select_input(&state, &reserved, 3, 20_000).is_err());
    assert!(select_input(&state, &reserved, 2, 20_000).unwrap().id == state.notes[1].id);
    state.notes[1].spent_height = Some(100);
    assert!(select_input(&state, &reserved, 2, 20_000).is_err());
    assert!(select_input(&state, &HashSet::new(), 1, 90_000).is_err());
    assert!(select_input(&state, &HashSet::new(), 0, 1).is_err());
    assert!(select_input(&state, &HashSet::new(), 1, 0).is_err());
    assert!(select_input(&state, &HashSet::new(), 1, u64::MAX).is_err());
}

// Called by the real proving test so its single expensive fixture also verifies
// the broadcast boundary. No RPC client is created and no submission occurs.
pub(crate) fn validate_fixture(mut prepared: PreparedTransaction) {
    validate_prepared(&prepared).unwrap();
    prepared.txid[0] ^= 1;
    assert!(validate_prepared(&prepared).is_err());
    prepared.txid[0] ^= 1;
    prepared.expiry_height += 1;
    assert!(validate_prepared(&prepared).is_err());
    prepared.expiry_height -= 1;
    prepared.fee += 5_000;
    assert!(validate_prepared(&prepared).is_err());
    prepared.fee -= 5_000;
    let original = prepared.inputs.clone();
    prepared.inputs.push(prepared.inputs[0]);
    assert!(validate_prepared(&prepared).is_err());
    prepared.inputs = original;
    prepared.bytes.push(0);
    assert!(validate_prepared(&prepared).is_err());
}

#[test]
fn fragmented_selection_counts_fee_and_preserves_single_note_preference() {
    let mut state = WalletState {
        account: Account {
            birthday: 1,
            fvk: [0; 96],
            use_qsk: false,
        },
        checkpoint: Checkpoint {
            height: 100,
            hash: [0; 32],
            parent: [0; 32],
            frontier: vec![],
        },
        notes: (1..=4).map(|i| note(i, 30_000, 90)).collect(),
    };
    let empty = HashSet::new();
    let selected = select_inputs(&state, &empty, 3, 90_000).unwrap();
    assert_eq!(selected.len(), 4);
    assert_eq!(fee_for_inputs(selected.len()).unwrap(), 20_000);
    assert!(select_inputs(&state, &HashSet::from([[1; 32]]), 3, 90_000).is_err());
    state.notes[0].mined_height = 99;
    assert!(select_inputs(&state, &empty, 3, 90_000).is_err());
    state.notes[0].mined_height = 90;
    state.notes[0].spent_height = Some(99);
    assert!(select_inputs(&state, &empty, 3, 90_000).is_err());
    state.notes.push(note(5, 110_000, 90));
    assert_eq!(select_inputs(&state, &empty, 3, 90_000).unwrap().len(), 1);
    state.notes = (1..=17).map(|i| note(i, 10_000, 90)).collect();
    assert!(select_inputs(&state, &empty, 3, 80_000).is_err()); // 16 inputs leave zero change.
    assert!(select_inputs(&state, &empty, 0, 1).is_err());
    assert!(select_inputs(&state, &empty, 3, 0).is_err());
}
