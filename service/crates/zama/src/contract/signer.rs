use alloy::signers::local::PrivateKeySigner;
use anyhow::{Result, ensure};
use std::{fs::File, io::Read, os::unix::fs::PermissionsExt, path::Path};
use zeroize::Zeroizing;

/// Load an explicitly provisioned Rust signing key. Never creates or replaces keys.
pub fn load_signer(path: &Path) -> Result<PrivateKeySigner> {
    let metadata = std::fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "signing key must be a regular file"
    );
    let file = File::open(path)?;
    ensure!(
        file.metadata()?.permissions().mode() & 0o077 == 0,
        "signing key must be private"
    );
    let mut bytes = Zeroizing::new(Vec::with_capacity(33));
    file.take(33).read_to_end(&mut bytes)?;
    ensure!(bytes.len() == 32, "invalid signing key length");
    PrivateKeySigner::from_slice(&bytes).map_err(|_| anyhow::anyhow!("invalid signing key"))
}
