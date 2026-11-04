//! Restartable acceptance driver. Wallet deposits and signatures are execution-time inputs.
use alloy::primitives::{Address, B256};
use anyhow::{Context, Result, ensure};
use app::{
    config::{Config, ZamaConfig},
    core::{Service, bridge_types::DepositState, make_memo, redeem_types::MIN_FEE_RESERVE_ZAT},
    store::{ACCEPTANCE_REQUEST, Store},
};
use serde_json::{Value, json};
use std::{
    env,
    fs::{File, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::Path,
};
use zama::{contract::CwzecClient, fhe};
#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("Acceptance pending: {error}");
        std::process::exit(1);
    }
}
async fn run() -> Result<()> {
    let args: Vec<_> = env::args().collect();
    let command = args.get(1).map(String::as_str).unwrap_or("status");
    let c = ZamaConfig::from_env()?;
    ensure!(c.chain_id == 11155111, "Sepolia only");
    let mut store = Store::connect(&c.database_url).await?;
    store.migrate().await?;
    if command == "start" {
        let user: Address = args.get(2).context("recipient required")?.parse()?;
        let destination = args
            .get(3)
            .context("shielded payout destination required")?;
        let amount: u64 = args
            .get(4)
            .context("gross redeem amount in zatoshis required")?
            .parse()?;
        store
            .start_bridge_acceptance(user, destination, amount)
            .await?;
    }
    let a = store.bridge_acceptance().await?;
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()?;
    if matches!(command, "start" | "status") {
        let wallet = Service::open(Config::from_env()?).await?;
        let minted = a
            .deposit
            .as_ref()
            .is_some_and(|d| d.state == DepositState::Minted);
        println!(
            "{}",
            json!({"recipient":a.user,"depositAddress":wallet.deposit_address,"operatorReserveAddress":wallet.funding_address,"minimumOperatorReserveZat":MIN_FEE_RESERVE_ZAT,"memo":make_memo(&a.user.to_string())?,"depositRecorded":a.deposit.is_some(),"exactMintSettled":minted,"userBalanceVerified":a.balance_handle.is_some(),"instruction":if a.deposit.is_none(){"Fund operator reserve separately; send one memo deposit, then wait. Rerunning never sends a deposit."}else{"Original deposit is recorded. Do not repeat it; continue its balance/signature steps."}})
        );
        return Ok(());
    }
    let d = a.deposit.context("waiting for original deposit")?;
    ensure!(
        d.state == DepositState::Minted,
        "waiting for automatic exact mint"
    );
    let client = CwzecClient::connect(&c.endpoint, c.chain_id, c.contract).await?;
    let sidecar = fhe::Client::new(&env::var("SIDECAR_URL")?)?;
    let permit_path = root.join(".local/backed-balance-session.json");
    if command == "balance-prepare" {
        ensure!(
            a.balance_handle.is_none(),
            "initial user balance already verified"
        );
        let handle = client.balance(a.user).await?;
        let request = sidecar
            .prepare_decryption(fhe::Scope {
                contract: c.contract,
                user: a.user,
                handle,
            })
            .await?;
        let response = request.wallet_request(client.kms_context().await?)?;
        write_private(&permit_path, &response)?;
        println!(
            "{}",
            json!({"method":"eth_signTypedData_v4","params":[a.user,response["typedData"].to_string()]})
        );
    } else if command == "balance-complete" {
        let response: Value = serde_json::from_reader(File::open(&permit_path)?)?;
        let handle: B256 = serde_json::from_value(response["handle"].clone())?;
        ensure!(
            client.balance(a.user).await? == handle,
            "balance handle changed; prepare a new user permit"
        );
        let prepared = fhe::PreparedDecryption::restore_wallet(
            fhe::Scope {
                contract: c.contract,
                user: a.user,
                handle,
            },
            response,
        )?;
        let signature =
            std::fs::read_to_string(args.get(2).context("wallet signature file required")?)?;
        let actual = sidecar
            .decrypt(
                prepared.authorize_wallet(signature.trim().into(), client.kms_context().await?)?,
            )
            .await?;
        store.record_acceptance_balance(handle, actual).await?;
        println!("Private user balance equals exact deposit issuance: true");
    } else if matches!(command, "prepare-redeem" | "prepare-rejected") {
        ensure!(
            a.balance_handle.is_some() && a.redeem_amount <= d.amount,
            "verified initial balance and bounded redeem required"
        );
        let rejected = command == "prepare-rejected";
        let request_id = if rejected {
            "recovery/rejected"
        } else {
            ACCEPTANCE_REQUEST
        };
        let amount = if rejected { 1 } else { a.redeem_amount };
        let mut child =
            std::process::Command::new(env::current_exe()?.with_file_name("bridge-user"))
                .args(["prepare", request_id])
                .stdin(std::process::Stdio::piped())
                .spawn()?;
        child
            .stdin
            .take()
            .context("request pipe missing")?
            .write_all(&serde_json::to_vec(
                &json!({"user":a.user,"amount":amount.to_string(),"destination":a.destination}),
            )?)?;
        ensure!(child.wait()?.success(), "user preparation failed");
    } else if command == "finish" {
        let pair = store.bridge_checkpoint_pair().await?;
        client.check_canonical(pair.checkpoint).await?;
        ensure!(
            zama::watcher::Watcher::new(&client)
                .latest()
                .await?
                .reference
                .number
                > pair.checkpoint.number,
            "EVM checkpoint needs two canonical confirmations"
        );
        let zcash =
            zcash::rpc::LightwalletClient::connect(&env::var("ZCASH_LIGHTWALLETD_URL")?).await?;
        ensure!(
            u64::from(zcash.tip().await?) >= u64::from(pair.zcash_height) + 2
                && zcash.block_hash(pair.zcash_height).await? == pair.zcash_hash,
            "Zcash checkpoint needs three canonical confirmations"
        );
        let evidence = store.acceptance_evidence().await?;
        write_private(
            &root.join(".local/backed-roundtrip-evidence.json"),
            &evidence,
        )?;
        println!("{}", evidence);
    } else {
        anyhow::bail!(
            "use start, status, balance-prepare, balance-complete, prepare-redeem, prepare-rejected or finish"
        )
    }
    Ok(())
}
fn write_private(path: &Path, value: &Value) -> Result<()> {
    let temp = path.with_extension("tmp");
    let mut f = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temp)?;
    f.write_all(&serde_json::to_vec_pretty(value)?)?;
    f.sync_all()?;
    std::fs::rename(temp, path)?;
    File::open(path.parent().context("missing parent")?)?.sync_all()?;
    Ok(())
}
