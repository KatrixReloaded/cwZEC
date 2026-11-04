//! Non-spending components from the first public Orchard key-components test vector.
//! https://github.com/zcash/zcash-test-vectors/blob/master/zcash_test_vectors/orchard/key_components.py
//! No seed or spending key is used. The value below is synthetic and never printed.

use orchard::{
    Note,
    keys::{FullViewingKey, Scope},
    note::{NoteVersion, RandomSeed, Rho},
    value::NoteValue,
};

pub(super) fn hex<const N: usize>(s: &str) -> [u8; N] {
    assert_eq!(s.len(), N * 2);
    std::array::from_fn(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap())
}

pub fn viewing_key() -> FullViewingKey {
    FullViewingKey::from_bytes(&hex(concat!(
        "740bbe5d0580b2cad430180d02cc128b9a140d5e07c151721dc16d25d4e20f15",
        "9f2f826738945ad01f47f70db0c367c246c20c61ff5583948c39dea968fefd1b",
        "021ccf89604f5f7cc6e034b32d338908b819fbe325fee6458b56b4ca71a7e43d"
    )))
    .unwrap()
}

pub fn note(fvk: &FullViewingKey, version: NoteVersion) -> Note {
    let rho = Rho::from_bytes(&hex(
        "2cb5b406ed8985e18130ab33362697b0e4e4c763ccb8f676495c222f7fba1e31",
    ))
    .unwrap();
    let seed = RandomSeed::from_bytes(
        hex("defa3d5a57efc2e1e9b01a035587d5fb1a38e01d94903d3c3e0ad3360c1d3710"),
        &rho,
    )
    .unwrap();
    Note::from_parts(
        fvk.address_at(0_u32, Scope::External),
        NoteValue::from_raw(50_000),
        rho,
        seed,
        version,
    )
    .unwrap()
}
