use super::*;
use std::{
    os::unix::fs::symlink,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "cwzec_destination_{}_{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn destination_authenticated_envelope_domain_and_tamper() -> Result<()> {
    let temp = Temp::new();
    let key = DestinationKey::create(&temp.0.join("destination.key"))?;
    let signer = zcash::core::LocalSigner::create(&temp.0.join("wallet.key"))?;
    let destination = signer.addresses()?.0;
    let contract = Address::repeat_byte(5);
    let user = Address::repeat_byte(6);
    let first = key.encrypt_destination(CHAIN_ID, contract, user, &destination)?;
    let second = key.encrypt_destination(CHAIN_ID, contract, user, &destination)?;
    assert_ne!(first, second, "random nonce must vary per preparation");
    assert_eq!(first[0], VERSION);
    assert_eq!(first.len(), destination.len() + 41);
    assert_eq!(
        &**key.decrypt_destination(CHAIN_ID, contract, user, &first)?,
        &destination
    );
    assert!(
        key.decrypt_destination(CHAIN_ID, Address::repeat_byte(7), user, &first)
            .is_err()
    );
    assert!(
        key.decrypt_destination(CHAIN_ID, contract, Address::repeat_byte(7), &first)
            .is_err()
    );
    assert!(key.decrypt_destination(1, contract, user, &first).is_err());
    let wrong = DestinationKey::create(&temp.0.join("wrong.key"))?;
    assert!(
        wrong
            .decrypt_destination(CHAIN_ID, contract, user, &first)
            .is_err()
    );
    for index in [0, 1, 24, 25, first.len() - 1] {
        let mut damaged = first.to_vec();
        damaged[index] ^= 1;
        assert!(
            key.decrypt_destination(CHAIN_ID, contract, user, &damaged)
                .is_err()
        );
    }
    for length in [0, 1, 24, 25, 40, first.len() - 1] {
        assert!(
            key.decrypt_destination(CHAIN_ID, contract, user, &first[..length])
                .is_err()
        );
    }
    assert!(
        key.encrypt_destination(CHAIN_ID, contract, user, "invalid")
            .is_err()
    );
    let loaded = DestinationKey::load(&temp.0.join("destination.key"))?;
    assert_eq!(
        &**loaded.decrypt_destination(CHAIN_ID, contract, user, &first)?,
        &destination
    );
    Ok(())
}

#[test]
fn destination_key_private_persistent_and_never_overwritten() -> Result<()> {
    let temp = Temp::new();
    let path = temp.0.join("key");
    let key = DestinationKey::load_or_create(&path)?;
    assert_eq!(
        std::fs::metadata(&path)?.permissions().mode() & 0o777,
        0o600
    );
    assert!(DestinationKey::create(&path).is_err());
    assert!(key.0.as_ref() == DestinationKey::load_or_create(&path)?.0.as_ref());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;
    assert!(DestinationKey::load(&path).is_err());
    assert!(DestinationKey::load_or_create(&path).is_err());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    let link = temp.0.join("link");
    symlink(&path, &link)?;
    assert!(DestinationKey::load(&link).is_err());
    std::fs::write(&path, [0; 31])?;
    assert!(DestinationKey::load(&path).is_err());
    std::fs::write(&path, [0; 33])?;
    assert!(DestinationKey::load(&path).is_err());
    Ok(())
}
