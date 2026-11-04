//! Public-fixture Ironwood verification; no spending keys, signing or submission.

use std::convert::Infallible;

use incrementalmerkletree::{frontier::CommitmentTree, witness::IncrementalWitness};
use orchard::{
    Address, NoteVersion,
    keys::{PreparedIncomingViewingKey, Scope},
    note::{ExtractedNoteCommitment, Nullifier, TransmittedNoteCiphertext},
    note_encryption::{CompactAction, IronwoodDomain, IronwoodNoteEncryption, OrchardDomain},
    primitives::redpallas::{SpendAuth, VerificationKey},
    tree::{MerkleHashOrchard, MerklePath},
    value::ValueCommitment,
};
use sha3::{Digest, Keccak256};
use zcash_address::unified::{self, Container, Encoding};
use zcash_note_encryption::{Domain, try_compact_note_decryption, try_note_decryption};
use zcash_primitives::transaction::{
    builder::{BuildConfig, Builder, BundlePadding},
    fees::zip317,
};
use zcash_protocol::{
    consensus::{BlockHeight, NetworkType, TEST_NETWORK},
    memo::MemoBytes,
    value::Zatoshis,
};

#[path = "signing_api.rs"]
mod signing_api;

pub fn verify() -> Result<(), Box<dyn std::error::Error>> {
    let fvk = super::fixture::viewing_key();
    let note = super::fixture::note(&fvk, NoteVersion::V3);
    let rho = note.rho().to_bytes();
    let rseed = *note.rseed().as_bytes();
    let mut preimage = [0_u8; 80];
    preimage[..16].copy_from_slice(b"cwzec/deposit/v1");
    preimage[16..48].copy_from_slice(&rho);
    preimage[48..].copy_from_slice(&rseed);
    // Independently checked with `cast keccak 0x<preimage>` (Keccak, not SHA3-256).
    let expected = super::fixture::hex::<32>(
        "66fee47de74062b5f8b5d2ee291d8726558301999ee18e8c1695bb6c7c0ab514",
    );
    assert_eq!(Keccak256::digest(preimage).as_slice(), expected);

    let cmx = ExtractedNoteCommitment::from(note.commitment());
    let mut tree = CommitmentTree::<MerkleHashOrchard, 32>::empty();
    tree.append(MerkleHashOrchard::from_cmx(&cmx))
        .map_err(|_| super::invalid("tree"))?;
    let witness = IncrementalWitness::from_tree(tree.clone()).unwrap();
    let path = MerklePath::from(witness.path().unwrap());
    let config = BuildConfig::Standard {
        sapling_anchor: None,
        orchard_anchor: None,
        ironwood_anchor: Some(path.root(cmx)),
        orchard_padding: BundlePadding::DEFAULT,
        ironwood_padding: BundlePadding::DEFAULT,
    };
    let mut builder = Builder::new(TEST_NETWORK, BlockHeight::from_u32(4_465_026), config);
    assert_eq!(u32::from(builder.expiry_height()), 4_465_146);
    builder.add_ironwood_spend::<Infallible>(fvk.clone(), note, path.clone())?;
    assert!(
        builder
            .add_ironwood_spend::<Infallible>(
                fvk.clone(),
                super::fixture::note(&fvk, NoteVersion::V2),
                path,
            )
            .is_err()
    );
    builder.add_ironwood_output::<Infallible>(
        Some(fvk.to_ovk(Scope::Internal)),
        fvk.address_at(0_u32, Scope::Internal),
        Zatoshis::from_u64(1)?,
        MemoBytes::empty(),
    )?;
    assert!(
        builder
            .get_fee(&zip317::FeeRule::standard())
            .map_err(|e| super::invalid(&format!("fee: {e}")))?
            .into_u64()
            > 0
    );

    // Encrypt and decrypt the public fixture using upstream V3 policy. The unused
    // outgoing ciphertext is zeroed: this fixture does not claim output recovery.
    let note = super::fixture::note(&fvk, NoteVersion::V3);
    let mut memo = [0_u8; 512];
    memo[..19].copy_from_slice(b"cwzec fixture memo.");
    let encryptor = IronwoodNoteEncryption::new(None, note, memo);
    let rk = VerificationKey::<SpendAuth>::try_from(
        <[u8; 32]>::try_from(&fvk.to_bytes()[..32]).unwrap(),
    )?;
    let action = orchard::Action::from_parts(
        Option::<Nullifier>::from(Nullifier::from_bytes(&rho)).unwrap(),
        rk,
        cmx,
        TransmittedNoteCiphertext {
            epk_bytes: IronwoodDomain::epk_bytes(encryptor.epk()).0,
            enc_ciphertext: encryptor.encrypt_note_plaintext(),
            out_ciphertext: [0; 80],
        },
        Option::<ValueCommitment>::from(ValueCommitment::from_bytes(
            &fvk.to_bytes()[..32].try_into().unwrap(),
        ))
        .unwrap(),
        (),
    )?;
    let ivk = PreparedIncomingViewingKey::new(&fvk.to_ivk(Scope::External));
    let compact = CompactAction::from(&action);
    let domain = IronwoodDomain::for_compact_action(&compact);
    let (decrypted, recipient) = try_compact_note_decryption(&domain, &ivk, &compact)
        .ok_or_else(|| super::invalid("compact V3 decryption"))?;
    assert_eq!(decrypted, super::fixture::note(&fvk, NoteVersion::V3));
    assert_eq!(recipient, decrypted.recipient());
    let (_, _, full_memo) =
        try_note_decryption(&IronwoodDomain::for_action(&action), &ivk, &action)
            .ok_or_else(|| super::invalid("full V3 decryption"))?;
    assert_eq!(full_memo, memo);
    assert!(try_note_decryption(&OrchardDomain::for_action(&action), &ivk, &action).is_none());
    let mut tampered = action.encrypted_note().clone();
    tampered.enc_ciphertext.0[52] ^= 1;
    let tampered = orchard::Action::from_parts(
        *action.nullifier(),
        action.rk().clone(),
        *action.cmx(),
        tampered,
        action.cv_net().clone(),
        (),
    )?;
    assert!(try_note_decryption(&IronwoodDomain::for_action(&tampered), &ivk, &tampered).is_none());

    let encoded = unified::Address::try_from_items(
        unified::Revision::R0,
        vec![unified::Uitem::Data(unified::Receiver::Orchard(
            recipient.to_raw_address_bytes(),
        ))],
    )?
    .encode(&NetworkType::Test);
    verify_address(&encoded)?;
    if let Some(address) = std::env::args().nth(1) {
        verify_address(&address)?;
        println!("Runtime testnet UA has a valid Orchard-protocol receiver usable in Ironwood.");
    }
    println!(
        "Ironwood V3 compact/full memo decryption, V2 rejection and memo tamper rejection verified."
    );
    println!(
        "Ironwood Builder spend/output/fee APIs and NU7 expiry verified; signing API compile-checked only."
    );
    println!("80-byte depositId public fixture vector verified; no live note or payout claimed.");
    Ok(())
}

fn verify_address(encoded: &str) -> Result<(), Box<dyn std::error::Error>> {
    let (network, _, ua) = unified::Address::decode(encoded)?;
    if network != NetworkType::Test {
        return Err(super::invalid("expected testnet UA").into());
    }
    let receiver = ua
        .items_as_parsed()
        .iter()
        .find_map(|r| match r {
            unified::Uitem::Data(unified::Receiver::Orchard(bytes)) => Some(bytes),
            _ => None,
        })
        .ok_or_else(|| super::invalid("missing Orchard-protocol receiver"))?;
    Option::<Address>::from(Address::from_raw_address_bytes(receiver))
        .ok_or_else(|| super::invalid("invalid Orchard-protocol receiver"))?;
    Ok(())
}
