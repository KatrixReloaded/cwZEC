use super::*;
use orchard::keys::Scope;
use std::{fs, os::unix::fs::PermissionsExt};

#[test]
fn runtime_key_reloads_without_replacement_and_requires_private_permissions() {
    let mut suffix = [0; 8];
    getrandom::fill(&mut suffix).unwrap();
    let dir = std::env::temp_dir().join(format!("cwzec-key-{}", u64::from_le_bytes(suffix)));
    fs::create_dir(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    let path = dir.join("vault.seed");
    let first = LocalSigner::create(&path).unwrap();
    let loaded = LocalSigner::load(&path).unwrap();
    assert!(first.full_viewing_key().to_bytes() == loaded.full_viewing_key().to_bytes());
    assert!(LocalSigner::create(&path).is_err());
    let receiver = loaded.full_viewing_key().address_at(0_u32, Scope::External);
    let address = encode_address(receiver).unwrap();
    assert!(decode_address(&address).unwrap() == receiver);
    assert!(decode_address("invalid").is_err());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(LocalSigner::load(&path).is_err());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&path, [0u8; 33]).unwrap();
    assert!(LocalSigner::load(&path).is_err());
    fs::remove_dir_all(dir).unwrap();
}
