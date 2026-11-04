//! Rust-only destination encryption. The sidecar never receives this key.
use alloy::primitives::{Address, Bytes};
use anyhow::{Context, Result, ensure};
use chacha20poly1305::{
    KeyInit, XChaCha20Poly1305, XNonce,
    aead::{Aead, Payload},
};
use rand_core::{OsRng, RngCore};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
};
use zeroize::Zeroizing;

const CHAIN_ID: u64 = 11155111;
const VERSION: u8 = 1;
const MAX_DESTINATION: usize = 2048;

pub struct DestinationKey(Zeroizing<[u8; 32]>);

impl DestinationKey {
    pub fn load(path: &Path) -> Result<Self> {
        ensure!(
            !path.symlink_metadata()?.file_type().is_symlink(),
            "destination key must not be a symlink"
        );
        let mut file = File::open(path).context("open destination key")?;
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file()
                && metadata.permissions().mode() & 0o777 == 0o600
                && metadata.len() == 32,
            "destination key must be a private 0600 32-byte file"
        );
        let mut key = Zeroizing::new([0; 32]);
        file.read_exact(key.as_mut())
            .context("read destination key")?;
        Ok(Self(key))
    }

    pub fn create(path: &Path) -> Result<Self> {
        let mut key = Zeroizing::new([0; 32]);
        OsRng
            .try_fill_bytes(key.as_mut())
            .map_err(|_| anyhow::anyhow!("system entropy unavailable"))?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .context("create destination key")?;
        file.write_all(key.as_ref())?;
        file.sync_all()?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        File::open(parent)?.sync_all()?;
        Ok(Self(key))
    }

    pub fn load_or_create(path: &Path) -> Result<Self> {
        match path.symlink_metadata() {
            Ok(_) => Self::load(path),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::create(path),
            Err(e) => Err(e.into()),
        }
    }

    /// Preparation helper for the user-authorized burn calldata.
    pub fn encrypt_destination(
        &self,
        chain: u64,
        contract: Address,
        user: Address,
        destination: &str,
    ) -> Result<Bytes> {
        ensure!(destination.len() <= MAX_DESTINATION, "destination too long");
        zcash::core::decode_address(destination)?;
        let aad = aad(chain, contract, user)?;
        let mut nonce = [0; 24];
        OsRng
            .try_fill_bytes(&mut nonce)
            .map_err(|_| anyhow::anyhow!("system entropy unavailable"))?;
        let cipher = XChaCha20Poly1305::new_from_slice(self.0.as_ref())
            .map_err(|_| anyhow::anyhow!("invalid destination key"))?;
        let encrypted = cipher
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: destination.as_bytes(),
                    aad: &aad,
                },
            )
            .map_err(|_| anyhow::anyhow!("destination encryption failed"))?;
        let mut envelope = Vec::with_capacity(25 + encrypted.len());
        envelope.push(VERSION);
        envelope.extend_from_slice(&nonce);
        envelope.extend_from_slice(&encrypted);
        Ok(envelope.into())
    }

    pub fn decrypt_destination(
        &self,
        chain: u64,
        contract: Address,
        user: Address,
        envelope: &[u8],
    ) -> Result<Zeroizing<String>> {
        ensure!(
            (41..=MAX_DESTINATION + 41).contains(&envelope.len()) && envelope[0] == VERSION,
            "invalid destination envelope"
        );
        let aad = aad(chain, contract, user)?;
        let cipher = XChaCha20Poly1305::new_from_slice(self.0.as_ref())
            .map_err(|_| anyhow::anyhow!("invalid destination key"))?;
        let nonce = XNonce::try_from(&envelope[1..25])
            .map_err(|_| anyhow::anyhow!("invalid destination nonce"))?;
        let plaintext = Zeroizing::new(
            cipher
                .decrypt(
                    &nonce,
                    Payload {
                        msg: &envelope[25..],
                        aad: &aad,
                    },
                )
                .map_err(|_| anyhow::anyhow!("destination authentication failed"))?,
        );
        let destination = Zeroizing::new(
            std::str::from_utf8(&plaintext)
                .context("invalid destination encoding")?
                .to_owned(),
        );
        zcash::core::decode_address(&destination)?;
        Ok(destination)
    }
}

fn aad(chain: u64, contract: Address, user: Address) -> Result<Vec<u8>> {
    ensure!(
        chain == CHAIN_ID && !contract.is_zero() && !user.is_zero(),
        "invalid destination domain"
    );
    let mut aad = b"cwzec/destination/v1".to_vec();
    aad.extend_from_slice(&chain.to_be_bytes());
    aad.extend_from_slice(contract.as_slice());
    aad.extend_from_slice(user.as_slice());
    Ok(aad)
}

#[cfg(test)]
#[path = "destination_tests.rs"]
mod tests;
