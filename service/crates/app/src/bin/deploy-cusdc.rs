//! Independent cUSDC deployment; no database or bridge state is opened.
use alloy::{
    consensus::{
        SignableTransaction, Transaction, TxEip1559, TxEnvelope, transaction::SignerRecoverable,
    },
    eips::eip2718::{Decodable2718, Encodable2718},
    primitives::{Address, B256, Bytes, TxKind, U256, keccak256},
    providers::{Provider, ProviderBuilder},
    rpc::types::TransactionRequest,
    signers::SignerSync,
    sol_types::SolValue,
};
use anyhow::{Context, Result, ensure};

use alloy::sol;
use serde::{Deserialize, Serialize};
use std::{
    env,
    fs::{File, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::Path,
};
use zama::contract::load_signer;
sol! { #[sol(rpc)] interface Wrapper { function underlying() external view returns(address); function decimals() external view returns(uint8); function rate() external view returns(uint256); function name() external view returns(string); function symbol() external view returns(string); } }

#[derive(Serialize, Deserialize)]
struct Journal {
    chain_id: u64,
    sender: Address,
    nonce: u64,
    contract: Address,
    raw: Bytes,
    hash: B256,
    initcode_hash: B256,
    deployment_block: Option<u64>,
}
#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!(
            "cUSDC deployment stopped: {error} ({}). Preserve cusdc-deployment.json before retrying.",
            error.root_cause()
        );
        std::process::exit(1);
    }
}
async fn run() -> Result<()> {
    let command = env::args().nth(1).context("use prepare or submit")?;
    ensure!(
        command == "prepare" || command == "submit",
        "unknown domain action"
    );
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()?;
    let path = root.join(".local/cusdc-deployment.json");
    let signer = load_signer(Path::new(&env::var("BRIDGE_SERVICE_KEY_PATH")?))?;
    let http = alloy::transports::http::reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .no_proxy()
        .build()?;
    let provider = ProviderBuilder::new()
        .disable_recommended_fillers()
        .connect_reqwest(http, env::var("ZAMA_RPC_URL")?.parse()?);
    ensure!(provider.get_chain_id().await? == 11155111, "Sepolia only");
    let artifact: serde_json::Value = serde_json::from_reader(File::open(
        root.join("contracts/artifacts/contracts/CUSDC.sol/CUSDC.json"),
    )?)?;
    let mut initcode = artifact["bytecode"]
        .as_str()
        .context("compiled artifact required")?
        .parse::<Bytes>()?
        .to_vec();
    let usdc: Address = "0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238".parse()?;
    ensure!(
        !provider.get_code_at(usdc).await?.is_empty(),
        "underlying code missing"
    );
    ensure!(
        Wrapper::new(usdc, &provider).decimals().call().await? == 6,
        "USDC decimals mismatch"
    );
    println!(
        "Circle Sepolia USDC={usdc}, decimals=6, code_hash={}",
        keccak256(provider.get_code_at(usdc).await?)
    );
    initcode.extend((usdc,).abi_encode());
    let initcode: Bytes = initcode.into();
    let mut journal: Journal = if path.exists() {
        serde_json::from_reader(File::open(&path)?)?
    } else {
        ensure!(command == "prepare", "prepare deployment first");
        let nonce = provider.get_transaction_count(signer.address()).await?;
        ensure!(
            provider
                .get_transaction_count(signer.address())
                .pending()
                .await?
                == nonce,
            "operator has pending transaction"
        );
        let estimate = provider
            .estimate_gas(
                TransactionRequest::default()
                    .from(signer.address())
                    .input(initcode.clone().into()),
            )
            .await?;
        let gas = deployment_gas_limit(estimate)?;
        println!("Deployment gas estimate={estimate}, signed limit={gas}");
        let max_fee = provider
            .get_gas_price()
            .await?
            .saturating_mul(2)
            .max(100_000_000);
        ensure!(max_fee <= 20_000_000_000, "deployment fee cap exceeded");
        ensure!(
            provider.get_balance(signer.address()).await?
                > U256::from((gas as u128 + 6_000_000) * max_fee),
            "insufficient operator gas reserve"
        );
        let tx = TxEip1559 {
            chain_id: 11155111,
            nonce,
            gas_limit: gas,
            max_fee_per_gas: max_fee,
            max_priority_fee_per_gas: 1_000_000,
            to: TxKind::Create,
            value: U256::ZERO,
            input: initcode.clone(),
            access_list: Default::default(),
        };
        let signature = signer.sign_hash_sync(&tx.signature_hash())?;
        let envelope: TxEnvelope = tx.into_signed(signature).into();
        let raw: Bytes = envelope.encoded_2718().into();
        let j = Journal {
            chain_id: 11155111,
            sender: signer.address(),
            nonce,
            contract: signer.address().create(nonce),
            hash: keccak256(&raw),
            raw,
            initcode_hash: keccak256(&initcode),
            deployment_block: None,
        };
        write_new(&path, &j)?;
        j
    };
    ensure!(
        journal.chain_id == 11155111
            && journal.sender == signer.address()
            && journal.initcode_hash == keccak256(&initcode)
            && journal.hash == keccak256(&journal.raw)
            && journal.contract == signer.address().create(journal.nonce),
        "deployment journal identity mismatch"
    );
    let mut raw = journal.raw.as_ref();
    let envelope = TxEnvelope::decode_2718(&mut raw)?;
    ensure!(
        envelope.recover_signer()? == signer.address()
            && envelope.value() == U256::ZERO
            && raw.is_empty()
            && envelope.is_create()
            && envelope.input() == &initcode
            && envelope.nonce() == journal.nonce
            && envelope.chain_id() == Some(11155111),
        "deployment signed intent mismatch"
    );
    if command == "prepare" {
        println!(
            "Prepared cUSDC Sepolia deployment: {} (journal committed; no broadcast)",
            journal.contract
        );
        return Ok(());
    }
    let receipt = provider.get_transaction_receipt(journal.hash).await?;
    if receipt.is_none() {
        ensure!(
            provider.get_transaction_count(signer.address()).await? <= journal.nonce,
            "nonce consumed with unavailable receipt; preserve journal"
        );
        let _ = provider.send_raw_transaction(&journal.raw).await;
        println!(
            "Deployment pending: {}; rerun submit to reconcile identical bytes",
            journal.hash
        );
        return Ok(());
    }
    let receipt = receipt.unwrap();
    ensure!(
        receipt.status() && receipt.contract_address == Some(journal.contract),
        "deployment reverted or identity mismatch"
    );
    let block = receipt
        .block_number
        .context("deployment receipt not included")?;
    if provider.get_block_number().await? <= block {
        println!(
            "Deployment awaiting second canonical confirmation: {}",
            journal.hash
        );
        return Ok(());
    }
    let canonical = provider
        .get_block_by_number(block.into())
        .await?
        .context("missing canonical block")?;
    ensure!(
        Some(canonical.header.hash) == receipt.block_hash,
        "noncanonical deployment"
    );
    let code = provider.get_code_at(journal.contract).await?;
    ensure!(!code.is_empty(), "runtime missing");
    // Immutable slots are checked through getters; all other bytes must match the artifact.
    let build_dir = root.join("contracts/artifacts/build-info");
    let mut references = None;
    for entry in std::fs::read_dir(build_dir)? {
        let build: serde_json::Value = serde_json::from_reader(File::open(entry?.path())?)?;
        let r = &build["output"]["contracts"]["contracts/CUSDC.sol"]["CUSDC"]["evm"]["deployedBytecode"]
            ["immutableReferences"];
        if r.is_object() && build["output"]["contracts"]["contracts/CUSDC.sol"]["CUSDC"]["evm"]["bytecode"]["object"].as_str() == artifact["bytecode"].as_str().map(|s| s.trim_start_matches("0x")) {
            references = Some(r.clone());
        }
    }
    let mut expected = artifact["deployedBytecode"]
        .as_str()
        .context("runtime artifact")?
        .parse::<Bytes>()?
        .to_vec();
    let mut observed = code.to_vec();
    ensure!(expected.len() == observed.len(), "runtime length mismatch");
    for spans in references
        .context("immutable references missing")?
        .as_object()
        .unwrap()
        .values()
    {
        for span in spans.as_array().unwrap() {
            let start = span["start"].as_u64().unwrap() as usize;
            let length = span["length"].as_u64().unwrap() as usize;
            expected[start..start + length].fill(0);
            observed[start..start + length].fill(0);
        }
    }
    ensure!(expected == observed, "runtime bytecode mismatch");
    let wrapper = Wrapper::new(journal.contract, &provider);
    ensure!(
        wrapper.underlying().call().await? == usdc
            && wrapper.decimals().call().await? == 6
            && wrapper.rate().call().await? == U256::from(1),
        "wrapper configuration mismatch"
    );
    ensure!(
        wrapper.name().call().await? == "Confidential USD Coin"
            && wrapper.symbol().call().await? == "cUSDC",
        "metadata mismatch"
    );
    println!("Verified runtime_hash={}", keccak256(code));
    journal.deployment_block = Some(block);
    let temp = path.with_extension("tmp");
    if temp.exists() {
        anyhow::bail!("deployment temporary file exists; inspect without removing journal");
    }
    write_new(&temp, &journal)?;
    std::fs::rename(temp, &path)?;
    File::open(path.parent().unwrap())?.sync_all()?;
    println!(
        "cUSDC verified: chain=11155111 contract={} deployment={} underlying_verified=true",
        journal.contract, block
    );
    Ok(())
}
fn write_new(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut f = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(&serde_json::to_vec_pretty(value)?)?;
    f.sync_all()?;
    File::open(path.parent().context("missing parent")?)?.sync_all()?;
    Ok(())
}

// The accepted CWZEC deployment used 17,982,718 gas; the same creation
// code currently estimates 18,126,605. Bound the estimate before padding.
fn deployment_gas_limit(estimate: u64) -> Result<u64> {
    ensure!(
        (1..=20_000_000).contains(&estimate),
        "unexpected deployment gas estimate: {estimate} (maximum 20000000)"
    );
    Ok(estimate * 120 / 100)
}
