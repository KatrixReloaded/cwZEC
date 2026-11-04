//! Read-only maintained SDK gate using an existing service-authorized actual burn.
#[path = "zama_probe/sdk.rs"]
mod sdk;
#[path = "zama_probe/signing.rs"]
mod signing;
use alloy::{
    primitives::{B256, U256, address},
    providers::{Provider, ProviderBuilder},
    rpc::types::TransactionRequest,
    sol,
    sol_types::SolCall,
};
use anyhow::{Context, Result, ensure};
use serde_json::json;
use std::{path::Path, time::Duration};
sol! {
    function redemptions(uint256 id) external view returns (address user, bytes32 amount, uint8 status, bytes32 zcashTxid);
    function service() external view returns (address);
    function getCurrentKmsContextId() external view returns (uint256);
}
#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("SDK compatibility stopped: {error}");
        std::process::exit(1);
    }
}
async fn run() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()?;
    let key = root.join(".local/cwzec-service.key");
    ensure!(key.is_file(), "existing service test key missing");
    let signer = signing::load_or_create(&key)?;
    let contract = address!("d80158ec981dc0cfc580ff479058d6e0f2743da2");
    let rpc = std::env::var("SEPOLIA_RPC_URL")
        .unwrap_or_else(|_| "https://ethereum-sepolia-rpc.publicnode.com".into());
    let client = alloy::transports::http::reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let provider =
        ProviderBuilder::new().connect_reqwest(client, rpc.parse().context("invalid RPC URL")?);
    ensure!(
        provider.get_chain_id().await.context("read chain")? == 11155111,
        "RPC chain mismatch"
    );
    let role = provider
        .call(
            TransactionRequest::default()
                .to(contract)
                .input(serviceCall {}.abi_encode().into()),
        )
        .await
        .context("read service role")?;
    ensure!(
        serviceCall::abi_decode_returns(&role)? == signer.address(),
        "service role mismatch"
    );
    let bytes = provider
        .call(
            TransactionRequest::default()
                .to(contract)
                .input(redemptionsCall { id: U256::from(1) }.abi_encode().into()),
        )
        .await
        .context("read existing redemption")?;
    let record = redemptionsCall::abi_decode_returns(&bytes)?;
    let expected: B256 =
        "0x87dbe772b72b4aedbf5273b6dc8de5e8c08a2d79a7ff0000000000aa36a70500".parse()?;
    ensure!(
        record.amount == expected && record.status == 3,
        "existing fixture changed"
    );
    let mut sdk = sdk::Sdk::start(&root, "sidecar/scripts/compatibility.ts")?;
    sdk.request(json!({"command":"init","rpcUrl":rpc})).await?;
    println!("Checking maintained SDK permit against Rust scope validation");
    let permit = sdk.request(json!({"command":"permit","contract":contract,"user":signer.address(),"handle":record.amount})).await.context("prepare maintained SDK permit")?;
    let context_bytes = provider
        .call(
            TransactionRequest::default()
                .to(address!("bE0E383937d564D7FF0BC3b46c51f0bF8d5C311A"))
                .input(getCurrentKmsContextIdCall {}.abi_encode().into()),
        )
        .await
        .context("read current KMS context")?;
    let kms_context = getCurrentKmsContextIdCall::abi_decode_returns(&context_bytes)
        .context("decode current KMS context")?;
    let signature = signing::sign_permit_for_context(
        &signer,
        contract,
        serde_json::from_value(permit)?,
        kms_context,
    )
    .await?;
    println!("Permit scope and current KMS context match; checking private decryption");
    let decrypted = sdk
        .request(json!({"command":"decrypt","signature":signature}))
        .await
        .context("decrypt existing actual burn")?;
    ensure!(
        decrypted["matchesExpected"] == true,
        "existing result mismatch"
    );
    println!("Private result matched; loading public encryption artifacts (120-second limit)");
    sdk.request(json!({"command":"encryption-key"}))
        .await
        .context("load public encryption artifacts")?;
    println!("Public encryption artifacts ready; checking uint64-boundary encryption");
    let encrypted = sdk
        .request(json!({"command":"encrypt","contract":contract,"user":signer.address()}))
        .await
        .context("encrypt uint64 boundary")?;
    ensure!(encrypted["valid"] == true, "invalid encrypted input");
    println!("Input proof verified; closing SDK process");
    sdk.close().await.context("SDK child shutdown failed")?;
    std::fs::write(
        root.join(".local/sidecar-sdk-compatibility.json"),
        serde_json::to_vec_pretty(
            &json!({"sdk":"3.7.1","backend":"0.13.2","chain_id":11155111,"contract":contract,"handle":record.amount,"checks":["existing_redemption_and_service_role","rust_permit_scope_and_digest","private_actual_result_comparison","uint64_max_encryption"],"transactions_sent":0}),
        )?,
    )?;
    println!("PASS: maintained SDK compatibility; no transactions sent");
    Ok(())
}
