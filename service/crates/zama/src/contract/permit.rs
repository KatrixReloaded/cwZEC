//! Scoped Sepolia decryption-permit signing. Custody stays in Rust.
use alloy::signers::local::PrivateKeySigner;
use alloy::{
    dyn_abi::TypedData,
    primitives::{Address, B256, Bytes, U256},
    signers::Signer,
    sol,
    sol_types::{SolStruct, eip712_domain},
};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::time::{SystemTime, UNIX_EPOCH};

sol! {
    struct UserDecryptRequestVerification {
        bytes publicKey;
        address[] contractAddresses;
        uint256 startTimestamp;
        uint256 durationDays;
        bytes extraData;
    }
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Permit {
    pub(crate) public_key: String,
    pub(crate) start_timestamp: String,
    pub(crate) typed_data: TypedData,
    pub(crate) digest: B256,
}

// Sign the fixed supported permit, never an arbitrary digest supplied by Node.
pub async fn sign_permit(
    signer: &PrivateKeySigner,
    contract: Address,
    permit: Permit,
) -> Result<String> {
    sign_permit_with_extra_data(signer, contract, permit, Bytes::from_static(&[0])).await
}

// The maintained SDK commits to a KMS context in extraData v1. The caller reads
// this public context independently from the fixed Sepolia KMSVerifier.
pub async fn sign_permit_for_context(
    signer: &PrivateKeySigner,
    contract: Address,
    permit: Permit,
    kms_context: U256,
) -> Result<String> {
    ensure!(!kms_context.is_zero(), "KMS context must be nonzero");
    let mut extra_data = vec![1];
    extra_data.extend_from_slice(&kms_context.to_be_bytes::<32>());
    sign_permit_with_extra_data(signer, contract, permit, extra_data.into()).await
}

async fn sign_permit_with_extra_data(
    signer: &PrivateKeySigner,
    contract: Address,
    permit: Permit,
    extra_data: Bytes,
) -> Result<String> {
    let expected = validate_permit(contract, &permit, extra_data)?;
    let signature = signer
        .sign_hash(&expected)
        .await
        .context("Rust permit signing failed")?;
    ensure!(
        signature.recover_address_from_prehash(&expected)? == signer.address(),
        "permit signer mismatch"
    );
    Ok(signature.to_string())
}

pub fn validate_permit_for_context(
    contract: Address,
    permit: &Permit,
    kms_context: U256,
) -> Result<B256> {
    ensure!(!kms_context.is_zero(), "KMS context must be nonzero");
    let mut extra = vec![1];
    extra.extend_from_slice(&kms_context.to_be_bytes::<32>());
    validate_permit(contract, permit, extra.into())
}

fn validate_permit(contract: Address, permit: &Permit, extra_data: Bytes) -> Result<B256> {
    ensure!(!contract.is_zero(), "zero permit contract");
    ensure!(
        permit.start_timestamp
            == permit
                .start_timestamp
                .parse::<u64>()
                .context("invalid permit timestamp")?
                .to_string(),
        "noncanonical permit timestamp"
    );
    let start: u64 = permit
        .start_timestamp
        .parse()
        .context("invalid permit timestamp")?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    ensure!(
        start <= now + 30 && now.saturating_sub(start) <= 300,
        "permit timestamp outside authorization window"
    );
    let public_key: Bytes = format!("0x{}", permit.public_key.trim_start_matches("0x"))
        .parse()
        .context("invalid ephemeral public key")?;
    // Transport keys are serialized ML-KEM keys, not 32-byte EVM keys.
    ensure!(
        !public_key.is_empty() && public_key.len() <= 4096,
        "invalid ephemeral public key length"
    );
    let request = UserDecryptRequestVerification {
        publicKey: public_key,
        contractAddresses: vec![contract],
        startTimestamp: U256::from(start),
        durationDays: U256::from(1),
        extraData: extra_data,
    };
    let domain = eip712_domain! {
        name: "Decryption", version: "1", chain_id: 11155111,
        verifying_contract: alloy::primitives::address!("5D8BD78e2ea6bbE41f26dFe9fdaEAa349e077478"),
    };
    let expected = request.eip712_signing_hash(&domain);
    ensure!(permit.digest == expected, "SDK/Rust permit digest mismatch");
    ensure!(
        permit
            .typed_data
            .eip712_signing_hash()
            .context("invalid permit schema")?
            == expected,
        "SDK permit terms mismatch"
    );
    Ok(expected)
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
