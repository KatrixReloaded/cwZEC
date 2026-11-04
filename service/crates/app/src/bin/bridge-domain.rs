//! Restartable clean testnet deployment. `prepare` journals only; `submit` recovers the same bytes.
use alloy::{
    consensus::{SignableTransaction, Transaction, TxEip1559, TxEnvelope},
    eips::eip2718::{Decodable2718, Encodable2718},
    primitives::{Address, B256, Bytes, TxKind, U256, keccak256},
    providers::{Provider, ProviderBuilder},
    rpc::types::TransactionRequest,
    signers::SignerSync,
    sol_types::SolValue,
};
use anyhow::{Context, Result, ensure};
use app::{
    config::Config,
    core::{Service, bridge_types::BridgeDomain, destination::DestinationKey, zama::ZamaService},
    store::Store,
};
use serde::{Deserialize, Serialize};
use std::{
    env,
    fs::{File, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::Path,
};
use zama::contract::{CwzecClient, load_signer};

#[derive(Serialize, Deserialize)]
struct Journal {
    chain_id: u64,
    sender: Address,
    auditor: Address,
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
            "Clean domain stopped: {error} ({}). Preserve backed-deployment.json before retrying.",
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
    let path = root.join(".local/backed-deployment.json");
    let config = Config::from_env()?;
    ensure!(
        config.database_url.contains("/cwzec_backed_roundtrip")
            && config.key_path == root.join(".local/backed-vault.seed"),
        "use dedicated backed database and vault"
    );
    let wallet = Service::open(config).await?;
    let key_path = env::var("BRIDGE_DESTINATION_KEY_PATH")?;
    DestinationKey::load_or_create(Path::new(&key_path))?;
    let signer = load_signer(Path::new(&env::var("BRIDGE_SERVICE_KEY_PATH")?))?;
    let auditor = load_signer(Path::new(&env::var("BRIDGE_AUDITOR_KEY_PATH")?))?;
    let http = alloy::transports::http::reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .no_proxy()
        .build()?;
    let provider = ProviderBuilder::new()
        .disable_recommended_fillers()
        .connect_reqwest(http, env::var("ZAMA_RPC_URL")?.parse()?);
    ensure!(provider.get_chain_id().await? == 11155111, "Sepolia only");
    let artifact: serde_json::Value = serde_json::from_reader(File::open(
        root.join("contracts/artifacts/contracts/CWZEC.sol/CWZEC.json"),
    )?)?;
    let mut initcode = artifact["bytecode"]
        .as_str()
        .context("compiled artifact required")?
        .parse::<Bytes>()?
        .to_vec();
    initcode.extend((signer.address(), signer.address(), auditor.address()).abi_encode());
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
            "insufficient operator gas reserve for round trip"
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
            auditor: auditor.address(),
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
            && journal.auditor == auditor.address()
            && journal.initcode_hash == keccak256(&initcode)
            && journal.hash == keccak256(&journal.raw)
            && journal.contract == signer.address().create(journal.nonce),
        "deployment journal identity mismatch"
    );
    let mut raw = journal.raw.as_ref();
    let envelope = TxEnvelope::decode_2718(&mut raw)?;
    ensure!(
        raw.is_empty()
            && envelope.is_create()
            && envelope.input() == &initcode
            && envelope.nonce() == journal.nonce
            && envelope.chain_id() == Some(11155111),
        "deployment signed intent mismatch"
    );
    if command == "prepare" {
        println!(
            "Prepared clean Sepolia deployment: {} (journal committed; no broadcast)",
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
    let client =
        CwzecClient::connect(&env::var("ZAMA_RPC_URL")?, 11155111, journal.contract).await?;
    let origin = client
        .verify_deployment(journal.hash, &initcode, signer.address(), auditor.address())
        .await?;
    let url = env::var("DATABASE_URL")?;
    let mut observer = ZamaService::open(
        &url,
        &env::var("ZAMA_RPC_URL")?,
        11155111,
        journal.contract,
        block,
    )
    .await?;
    // Commit origin before a long backfill can prune its deployment header.
    if observer.cursor().await?.tip.number <= block {
        observer.scan_once().await?;
    }
    let mut store = Store::connect(&url).await?;
    store
        .initialize_bridge(BridgeDomain {
            chain_id: 11155111,
            contract: journal.contract,
        })
        .await?;
    if let Some(saved) = store.bridge_origin().await? {
        ensure!(
            saved.block == origin.block
                && saved.transaction == origin.transaction
                && saved.runtime_hash == origin.runtime_hash
                && saved.service == origin.service
                && saved.auditor == origin.auditor,
            "saved clean origin changed"
        );
    } else {
        store.record_bridge_origin(&origin).await?;
    }
    while observer.scan_once().await?.tip.number < observer.latest_height().await? {}
    journal.deployment_block = Some(block);
    let temp = path.with_extension("tmp");
    if temp.exists() {
        anyhow::bail!("deployment temporary file exists; inspect without removing journal");
    }
    write_new(&temp, &journal)?;
    std::fs::rename(temp, &path)?;
    File::open(path.parent().unwrap())?.sync_all()?;
    println!(
        "Clean domain verified: chain=11155111 contract={} deployment={} initial_supply_zero=true",
        journal.contract, block
    );
    println!(
        "Deposit address: {}\nOperator reserve address: {}",
        wallet.deposit_address, wallet.funding_address
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

#[cfg(test)]
#[path = "bridge-domain/tests.rs"]
mod tests;
