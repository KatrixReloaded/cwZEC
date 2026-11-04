//! Independent Sepolia feasibility probe; no bridge workflow or Zcash calls.
#[path = "zama_probe/sdk.rs"]
mod sdk;

#[path = "zama_probe/signing.rs"]
mod signing;
#[cfg(test)]
#[path = "zama_probe/transaction_tests.rs"]
mod transaction_tests;

use alloy::{
    network::{EthereumWallet, TransactionBuilder},
    primitives::{Address, B256, Bytes},
    providers::{DynProvider, Provider, ProviderBuilder},
    rpc::types::TransactionRequest,
    signers::local::PrivateKeySigner,
    sol,
    sol_types::SolCall,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::Path,
    time::Duration,
};

sol! {
    function submit(bytes32 input, bytes proof);
    function result() external view returns (bytes32);
    function owner() external view returns (address);
    function persistAllowed(bytes32 handle, address account) external view returns (bool);
    function isAllowedForDecryption(bytes32 handle) external view returns (bool);
}

#[tokio::main]
async fn main() {
    // Error chains may include RPC URLs or payloads; print only the outer stage.
    if let Err(error) = run().await {
        eprintln!("Probe stopped: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()?;
    let signer = signing::load_or_create(&root.join(".local/zama-probe.key"))?;
    let command = std::env::args().nth(1).unwrap_or_else(|| "init".into());
    println!("Probe signer: {}", signer.address());
    if command == "init" {
        return Ok(());
    }
    ensure!(command == "run", "use init or run");
    let rpc_url = std::env::var("SEPOLIA_RPC_URL")
        .unwrap_or_else(|_| "https://ethereum-sepolia-rpc.publicnode.com".into());
    let client = alloy::transports::http::reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .context("create RPC client")?;
    let provider = ProviderBuilder::new()
        .wallet(EthereumWallet::from(signer.clone()))
        .connect_reqwest(client, rpc_url.parse().context("invalid Sepolia RPC URL")?)
        .erased();
    ensure!(
        provider
            .get_chain_id()
            .await
            .context("Sepolia RPC chain check failed")?
            == 11155111,
        "RPC is not Sepolia"
    );
    ensure!(
        !provider
            .get_balance(signer.address())
            .await
            .context("read probe gas balance")?
            .is_zero(),
        "probe signer needs Sepolia gas funding"
    );
    let artifact: Value = serde_json::from_reader(
        File::open(root.join("probes/zama/artifacts/FheProbe.json"))
            .context("compile FheProbe first")?,
    )
    .context("read contract artifact")?;
    let bytecode: Bytes = artifact["bytecode"]
        .as_str()
        .context("missing bytecode")?
        .parse()
        .context("invalid bytecode")?;
    let runtime: Bytes = artifact["deployedBytecode"]
        .as_str()
        .context("missing runtime")?
        .parse()
        .context("invalid runtime")?;
    // Deliberately refuse unattended re-deployment after an interrupted run.
    let mut progress = OpenOptions::new().write(true).create_new(true)
        .open(root.join(".local/zama-progress.jsonl")).context("probe progress already exists or cannot be created; inspect it before another deployment")?;
    let mut sdk = sdk::Sdk::start(&root, "probes/zama/sdk.mjs")?;
    println!("Checking SDK configuration and network contracts");
    let config = sdk
        .request(json!({"command":"init","rpcUrl":rpc_url}))
        .await
        .context("SDK initialization failed")?;
    let acl: Address = config["aclAddress"]
        .as_str()
        .context("missing ACL")?
        .parse()?;
    for name in [
        "aclAddress",
        "kmsAddress",
        "inputVerifierAddress",
        "executorAddress",
    ] {
        let address: Address = config[name]
            .as_str()
            .context("SDK configuration missing address")?
            .parse()?;
        ensure!(
            !provider
                .get_code_at(address)
                .await
                .context("read Zama infrastructure code")?
                .is_empty(),
            "Zama infrastructure missing on selected chain"
        );
    }
    println!("Deploying minimal FHEVM contract");
    let deployment = send(
        &provider,
        deployment_request(signer.address(), bytecode),
        "deployment",
        &mut progress,
    )
    .await?;
    ensure!(
        deployment.from == signer.address() && deployment.to.is_none(),
        "deployment sender mismatch"
    );
    let contract = deployment
        .contract_address
        .context("deployment has no contract address")?;
    // An immutable owner occupies runtime slots, so Solidity metadata/runtime cannot be byte-compared here.
    ensure!(!runtime.is_empty(), "empty compiled runtime");
    ensure!(
        !provider
            .get_code_at(contract)
            .await
            .context("read deployment code")?
            .is_empty(),
        "deployed contract has no code"
    );
    let owner_bytes = provider
        .call(
            TransactionRequest::default()
                .to(contract)
                .input(ownerCall {}.abi_encode().into()),
        )
        .await
        .context("read probe owner")?;
    ensure!(
        ownerCall::abi_decode_returns(&owner_bytes)? == signer.address(),
        "deployed owner mismatch"
    );
    println!("Encrypting contract/caller-bound input");
    let encrypted = sdk
        .request(json!({"command":"encrypt","contract":contract,"user":signer.address()}))
        .await
        .context("SDK encryption failed")?;
    let input: B256 = encrypted["handle"]
        .as_str()
        .context("missing input handle")?
        .parse()
        .context("invalid input handle")?;
    let proof: Bytes = encrypted["inputProof"]
        .as_str()
        .context("missing input proof")?
        .parse()
        .context("invalid input proof")?;
    let calldata = submitCall { input, proof }.abi_encode();
    println!("Submitting encrypted input with Rust signer");
    let submitted = send(
        &provider,
        TransactionRequest::default()
            .from(signer.address())
            .to(contract)
            .input(calldata.into()),
        "input",
        &mut progress,
    )
    .await?;
    ensure!(
        submitted.from == signer.address() && submitted.to == Some(contract),
        "input transaction binding mismatch"
    );
    let output = provider
        .call(
            TransactionRequest::default()
                .to(contract)
                .input(resultCall {}.abi_encode().into()),
        )
        .await
        .context("read encrypted result")?;
    let handle = resultCall::abi_decode_returns(&output).context("decode result handle")?;
    ensure!(
        handle != B256::ZERO && handle != input,
        "missing or unchanged encrypted result"
    );
    let outsider = PrivateKeySigner::random();
    ensure!(
        allowed(&provider, acl, handle, signer.address()).await?,
        "authorized user lacks ACL permission"
    );
    ensure!(
        allowed(&provider, acl, handle, contract).await?,
        "contract lacks ACL permission"
    );
    ensure!(
        !allowed(&provider, acl, handle, outsider.address()).await?,
        "unauthorized user has ACL permission"
    );
    let public = provider
        .call(
            TransactionRequest::default()
                .to(acl)
                .input(isAllowedForDecryptionCall { handle }.abi_encode().into()),
        )
        .await
        .context("read public-decryption ACL")?;
    ensure!(
        !isAllowedForDecryptionCall::abi_decode_returns(&public)?,
        "result is publicly decryptable"
    );
    println!("Checking authorized user decryption");
    let authorized = decrypt(&mut sdk, &signer, contract, handle)
        .await
        .context("authorized decryption failed")?;
    ensure!(
        authorized["authorized"] == true && authorized["matchesExpected"] == true,
        "authorized result did not match private input computation"
    );
    println!("Checking unauthorized user decryption");
    let denied = decrypt(&mut sdk, &outsider, contract, handle)
        .await
        .context("unauthorized flow failed unexpectedly")?;
    ensure!(
        denied["authorized"] == false && denied["reason"] == "sdk_acl_denied",
        "unauthorized flow did not return the expected ACL denial"
    );
    sdk.close().await.context("close SDK probe")?;
    let evidence = json!({
        "chain_id":11155111,"contract":contract,"authorized_user":signer.address(),"unauthorized_user":outsider.address(),
        "deployment_tx":deployment.transaction_hash,"deployment_block":deployment.block_number,
        "input_tx":submitted.transaction_hash,"input_block":submitted.block_number,"result_handle":handle,
        "checks":{"rust_signed_transactions":true,"rust_signed_both_permits":true,"sdk_and_rust_permit_digests_match":true,
            "node_recovered_permit_signers":true,"contract_acl":true,"authorized_acl":true,"unauthorized_acl":false,
            "publicly_decryptable":false,"authorized_result_matches":true,"unauthorized_result":"sdk_acl_denied"},
        "versions":{"relayer_sdk":"0.4.5","fhevm_solidity":"0.13.4","ethers":"6.15.0","solc":"0.8.28","alloy":"1.8.3","rust":"1.92.0"},
        "confirmation_policy":"one included block; not finality or reorg evidence",
        "boundary":"Rust signs; Node receives public context and signatures only; SDK ephemeral decryption private keys stay in Node memory"
    });
    let mut file = File::create(root.join(".local/zama-evidence.json"))?;
    serde_json::to_writer_pretty(&mut file, &evidence)?;
    file.sync_all()?;
    println!(
        "PASS: authorized plaintext matched privately; unauthorized supported flow denied by SDK ACL preflight"
    );
    println!("Sanitized evidence: .local/zama-evidence.json");
    Ok(())
}

fn deployment_request(sender: Address, bytecode: Bytes) -> TransactionRequest {
    TransactionRequest::default()
        .from(sender)
        .with_deploy_code(bytecode)
}

async fn send(
    provider: &DynProvider,
    tx: TransactionRequest,
    stage: &str,
    progress: &mut File,
) -> Result<alloy::rpc::types::TransactionReceipt> {
    let pending = provider
        .send_transaction(tx)
        .await
        .context("transaction submission failed; inspect sender nonce before retrying")?;
    let hash = *pending.tx_hash();
    writeln!(progress, "{}", json!({"stage":stage,"tx":hash}))?;
    progress.sync_all()?;
    println!("{stage} transaction: {hash}");
    let receipt = pending
        .with_required_confirmations(1)
        .with_timeout(Some(Duration::from_secs(240)))
        .get_receipt()
        .await
        .context("transaction receipt unresolved; do not redeploy automatically")?;
    ensure!(receipt.status(), "Sepolia transaction reverted");
    Ok(receipt)
}

async fn allowed(
    provider: &DynProvider,
    acl: Address,
    handle: B256,
    account: Address,
) -> Result<bool> {
    let bytes = provider
        .call(
            TransactionRequest::default()
                .to(acl)
                .input(persistAllowedCall { handle, account }.abi_encode().into()),
        )
        .await
        .context("ACL permission lookup failed")?;
    persistAllowedCall::abi_decode_returns(&bytes).context("decode ACL permission")
}

async fn decrypt(
    sdk: &mut sdk::Sdk,
    signer: &PrivateKeySigner,
    contract: Address,
    handle: B256,
) -> Result<Value> {
    let request = sdk
        .request(
            json!({"command":"permit","contract":contract,"user":signer.address(),"handle":handle}),
        )
        .await?;
    let permit = serde_json::from_value(request).context("decode SDK permit")?;
    let signature = signing::sign_permit(signer, contract, permit).await?;
    sdk.request(json!({"command":"decrypt","signature":signature}))
        .await
}
