//! One checkpoint reconciliation pass. Does not sign or submit chain transactions.
use anyhow::{Context, Result, ensure};
use app::{config::ZamaConfig, core::bridge::BridgeReconciler, store::Store};
use std::{env, path::PathBuf};
use zama::{
    contract::{CwzecClient, load_signer},
    fhe,
};
use zcash::rpc::LightwalletClient;

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("Bridge reconciliation stopped: {error}");
        std::process::exit(1);
    }
}
async fn run() -> Result<()> {
    let c = ZamaConfig::from_env()?;
    let mut store = Store::connect(&c.database_url).await?;
    store.migrate().await?;
    let cursor = store
        .zama_cursor()
        .await?
        .context("start Zama observer first")?;
    ensure!(
        cursor.deployment_block == c.deployment_block,
        "deployment identity mismatch"
    );
    let service_path: PathBuf = env::var("BRIDGE_SERVICE_KEY_PATH")
        .context("BRIDGE_SERVICE_KEY_PATH required")?
        .into();
    let auditor_path: PathBuf = env::var("BRIDGE_AUDITOR_KEY_PATH")
        .context("BRIDGE_AUDITOR_KEY_PATH required")?
        .into();
    let service = load_signer(&service_path)?;
    let auditor = load_signer(&auditor_path)?;
    let contract = CwzecClient::connect(&c.endpoint, c.chain_id, c.contract).await?;
    let sidecar = fhe::Client::new(&env::var("SIDECAR_URL").context("SIDECAR_URL required")?)?;
    let zcash = LightwalletClient::connect(
        &env::var("ZCASH_LIGHTWALLETD_URL").context("ZCASH_LIGHTWALLETD_URL required")?,
    )
    .await?;
    let mut bridge = BridgeReconciler::new(store, contract, sidecar, zcash).await?;
    let pair = bridge.checkpoint_pair().await?;
    let report = bridge.reconcile_at(pair, &service, &auditor).await?;
    println!(
        "checkpoint={} zcash_height={} evm_height={} coverage_complete={} solvent={}",
        report.id,
        pair.zcash_height,
        pair.checkpoint.number,
        report.coverage_complete,
        report.fee_reserve.is_some()
    );
    for reason in report.unsupported {
        eprintln!("Authorization blocked: {reason}");
    }
    ensure!(
        report.coverage_complete && report.fee_reserve.is_some(),
        "checkpoint does not authorize bridge progress"
    );
    Ok(())
}
