use anyhow::{Context, Result, ensure};
use std::{env, path::PathBuf};

pub struct Config {
    pub database_url: String,
    pub endpoint: String,
    pub key_path: PathBuf,
    pub admin_key: String,
    pub confirmations: u32,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let admin_key = env::var("ADMIN_API_KEY").context("ADMIN_API_KEY is required")?;
        ensure!(
            admin_key.len() >= 32,
            "ADMIN_API_KEY must have at least 32 characters"
        );
        let confirmations = env::var("CONFIRMATIONS")
            .unwrap_or_else(|_| "3".into())
            .parse()?;
        ensure!(confirmations > 0, "CONFIRMATIONS must be nonzero");
        Ok(Self {
            database_url: env::var("DATABASE_URL").context("DATABASE_URL is required")?,
            endpoint: env::var("ZCASH_LIGHTWALLETD_URL")
                .unwrap_or_else(|_| "https://testnet.zec.rocks:443".into()),
            key_path: env::var("VAULT_KEY_PATH")
                .context("VAULT_KEY_PATH is required")?
                .into(),
            admin_key,
            confirmations,
        })
    }
}

/// Separate observer startup: no Zcash key, API credential, or signing key required.
pub struct ZamaConfig {
    pub database_url: String,
    pub endpoint: String,
    pub chain_id: u64,
    pub contract: alloy::primitives::Address,
    pub deployment_block: u64,
}
impl ZamaConfig {
    pub fn from_env() -> Result<Self> {
        let chain_id = env::var("ZAMA_CHAIN_ID")
            .context("ZAMA_CHAIN_ID is required")?
            .parse()?;
        let contract = env::var("ZAMA_CONTRACT")
            .context("ZAMA_CONTRACT is required")?
            .parse()?;
        let deployment_block = env::var("ZAMA_DEPLOYMENT_BLOCK")
            .context("ZAMA_DEPLOYMENT_BLOCK is required")?
            .parse()?;
        ensure!(
            chain_id > 0 && deployment_block > 0,
            "invalid Zama configuration"
        );
        Ok(Self {
            database_url: env::var("DATABASE_URL").context("DATABASE_URL is required")?,
            endpoint: env::var("ZAMA_RPC_URL").context("ZAMA_RPC_URL is required")?,
            chain_id,
            contract,
            deployment_block,
        })
    }
}
