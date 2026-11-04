use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
};

use anyhow::{Context, Result, ensure};
use orchard::keys::{FullViewingKey, SpendingKey};
use zeroize::Zeroizing;
use zip32::AccountId;

pub trait Signer: Send + Sync {
    fn full_viewing_key(&self) -> FullViewingKey;
    fn sign(
        &self,
        checkpoint: &super::Checkpoint,
        input: &super::WalletNote,
        destination: &str,
        amount: u64,
    ) -> Result<super::PreparedTransaction>;
}

/// The custody key remains inside signing code. Derivation uses testnet ZIP32
/// coin type 1, account 0, and legacy (use_qsk=false) viewing-key derivation.
pub struct LocalSigner {
    key: SpendingKey,
}

impl LocalSigner {
    pub fn sign_inputs(
        &self,
        checkpoint: &super::Checkpoint,
        inputs: &[super::WalletNote],
        destination: &str,
        amount: u64,
    ) -> Result<super::PreparedTransaction> {
        super::transaction::sign(&self.key, checkpoint, inputs, destination, amount)
    }

    pub fn addresses(&self) -> Result<(String, String)> {
        use orchard::keys::Scope;
        let fvk = self.full_viewing_key();
        Ok((
            super::encode_address(fvk.address_at(0_u32, Scope::External))?,
            super::encode_address(fvk.address_at(0_u32, Scope::Internal))?,
        ))
    }
    /// Explicit provisioning for a new vault only. Never replaces an existing seed.
    pub fn create(path: &Path) -> Result<Self> {
        let mut seed = Zeroizing::new([0u8; 32]);
        getrandom::fill(seed.as_mut())
            .map_err(|_| anyhow::anyhow!("system entropy unavailable"))?;
        let signer = Self::derive(seed.as_ref())?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .context("create vault seed file")?;
        file.write_all(seed.as_ref())
            .context("write vault seed file")?;
        file.sync_all().context("sync vault seed file")?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        File::open(parent)?.sync_all()?;
        Ok(signer)
    }

    pub fn load(path: &Path) -> Result<Self> {
        let metadata = fs::symlink_metadata(path).context("read vault seed metadata")?;
        ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "vault seed must be a regular file"
        );
        ensure!(
            metadata.permissions().mode() & 0o077 == 0,
            "vault seed permissions must exclude group and other users"
        );
        let mut seed = Zeroizing::new(Vec::with_capacity(33));
        File::open(path)?.take(33).read_to_end(&mut seed)?;
        ensure!(seed.len() == 32, "vault seed must contain exactly 32 bytes");
        Self::derive(&seed)
    }

    fn derive(seed: &[u8]) -> Result<Self> {
        let key = SpendingKey::from_zip32_seed(seed, 1, AccountId::ZERO)
            .map_err(|_| anyhow::anyhow!("vault ZIP32 derivation failed"))?;
        Ok(Self { key })
    }
}

impl Signer for LocalSigner {
    fn full_viewing_key(&self) -> FullViewingKey {
        FullViewingKey::from(&self.key)
    }

    fn sign(
        &self,
        checkpoint: &super::Checkpoint,
        input: &super::WalletNote,
        destination: &str,
        amount: u64,
    ) -> Result<super::PreparedTransaction> {
        self.sign_inputs(checkpoint, std::slice::from_ref(input), destination, amount)
    }
}
