use alloy::signers::local::PrivateKeySigner;
use anyhow::{Context, Result, ensure};
use std::{
    fs::OpenOptions,
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
};
use zeroize::Zeroizing;
// Preserve the completed probe APIs without duplicating signing logic.
#[allow(unused_imports)]
pub use zama::contract::permit::{sign_permit, sign_permit_for_context};

// Only this Rust module opens the probe signing-key file.
pub fn load_or_create(path: &Path) -> Result<PrivateKeySigner> {
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
    {
        Ok(mut file) => {
            let signer = PrivateKeySigner::random();
            let bytes = Zeroizing::new(signer.to_bytes().0);
            file.write_all(bytes.as_ref()).context("write probe key")?;
            file.sync_all().context("sync probe key")?;
            Ok(signer)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            ensure!(
                std::fs::symlink_metadata(path)?.file_type().is_file(),
                "probe key must be a regular file"
            );
            let mut file = OpenOptions::new().read(true).open(path)?;
            ensure!(
                file.metadata()?.permissions().mode() & 0o077 == 0,
                "probe key permissions must be private"
            );
            let mut bytes = Zeroizing::new(Vec::new());
            file.read_to_end(&mut bytes).context("read probe key")?;
            ensure!(bytes.len() == 32, "invalid probe key length");
            PrivateKeySigner::from_slice(&bytes).map_err(|_| anyhow::anyhow!("invalid probe key"))
        }
        Err(_) => anyhow::bail!("cannot create probe key"),
    }
}
