//! Preparation fixes calldata but never signs or submits a user transaction.
use super::{bridge_types::BridgeDomain, destination::DestinationKey};
use crate::store::{Store, UserRedeemRequest};
use alloy::primitives::{Address, keccak256};
use anyhow::{Result, ensure};
use zama::{contract::CwzecClient, fhe};

pub async fn prepare_redeem(
    store: &mut Store,
    client: &CwzecClient,
    fhe: &fhe::Client,
    key: &DestinationKey,
    id: &str,
    user: Address,
    amount: u64,
    destination: &str,
) -> Result<UserRedeemRequest> {
    ensure!(
        !id.is_empty() && id.len() <= 128 && !user.is_zero() && user != client.address(),
        "invalid user request"
    );
    ensure!(amount > 0, "invalid amount");
    ensure!(
        destination.len() <= 2048 && zcash::core::decode_address(destination).is_ok(),
        "invalid destination"
    );
    ensure!(
        store.http_domain().await?
            == Some(BridgeDomain {
                chain_id: client.chain_id(),
                contract: client.address()
            }),
        "request domain mismatch"
    );
    let destination_hash = keccak256(destination.as_bytes());
    store.check_destination_key(key).await?;
    if let Some(saved) = store.user_redeem_request(id).await? {
        ensure!(
            saved.user == user
                && saved.amount == amount
                && saved.destination_hash == destination_hash,
            "request ID terms changed"
        );
        return Ok(saved);
    }
    let dest = key.encrypt_destination(client.chain_id(), client.address(), user, destination)?;
    let encrypted = fhe.encrypt(client.address(), user, amount).await?;
    let call = client
        .domain()
        .request_redeem(user, encrypted.handle, encrypted.input_proof, dest);
    // The database lock decides concurrent retries; randomized re-encryption never
    // replaces the first committed calldata. No transaction spans the SDK call.
    store
        .save_user_redeem_request(id, user, amount, destination_hash, call.calldata())
        .await
}
