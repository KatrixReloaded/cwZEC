//! Read-only resumption of the already-submitted synthetic void. Never signs or broadcasts.
use alloy::primitives::{B256, U256, keccak256};
use anyhow::{Result, ensure};
use app::{core::zama::ZamaService, store::TransactionState};
use std::{path::Path, time::Duration};
use zama::{contract::DepositStatus, watcher::Event};
const OPERATION: &str = "client-milestone-synthetic-void-v1";
const TRANSACTION: &str = "0x62fe2cb76847d1ced5e845fba70e79cc01d5b2927d4b2593175714964f2a0511";
const DEPLOYMENT: u64 = 11992179;
const CHAIN: u64 = 11155111;
const CONTRACT: &str = "0xd80158ec981dc0cfc580ff479058d6e0f2743da2";
#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("Zama acceptance stopped: {error}");
        std::process::exit(1);
    }
}
async fn open(db: &str, rpc: &str) -> Result<ZamaService> {
    ZamaService::open(db, rpc, CHAIN, CONTRACT.parse()?, DEPLOYMENT)
        .await
        .map_err(|_| anyhow::anyhow!("open persisted observer"))
}
async fn catch_up(s: &mut ZamaService) -> Result<()> {
    // Freeze a bounded target for this backfill; later polls observe newer heads.
    let target = s.latest_height().await?;
    for _ in 0..256 {
        let cursor = s.scan_once().await?;
        if cursor.tip.number >= target {
            return Ok(());
        }
    }
    anyhow::bail!("bounded backfill incomplete; rerun to resume")
}
async fn run() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()?;
    let db = std::env::var("DATABASE_URL").map_err(|_| anyhow::anyhow!("DATABASE_URL required"))?;
    let rpc = std::env::var("ZAMA_RPC_URL")
        .unwrap_or_else(|_| "https://ethereum-sepolia-rpc.publicnode.com".into());
    let mut s = open(&db, &rpc).await?;
    let saved = s.transaction(OPERATION).await?.ok_or_else(|| {
        anyhow::anyhow!("existing signed void required; this runner never creates one")
    })?;
    let expected: B256 = TRANSACTION.parse()?;
    ensure!(
        saved.signed.hash() == expected && saved.signed.nonce() == 17,
        "unexpected saved transaction identity"
    );
    let original_bytes = saved.signed.raw().clone();
    let supply_before = s.client().supply().await?;
    println!("Resuming existing synthetic void; no signing or broadcast");
    catch_up(&mut s).await?;
    let before = s.observation_counts().await?;
    ensure!(
        before.0 >= 14 && before.1 >= 13 && before.2 >= 13,
        "existing lifecycle history missing"
    );
    let ids = s
        .events()
        .await?
        .iter()
        .map(|e| (e.transaction_hash, e.log_index))
        .collect::<std::collections::BTreeSet<_>>();
    ensure!(
        ids.len() as u64 == before.0,
        "duplicate canonical logical events"
    );
    drop(s);
    let mut s = open(&db, &rpc).await?;
    for poll in 0..20 {
        s.scan_once().await?;
        let saved = s
            .transaction(OPERATION)
            .await?
            .ok_or_else(|| anyhow::anyhow!("saved transaction missing"))?;
        ensure!(
            saved.signed.hash() == expected && saved.signed.raw() == &original_bytes,
            "restart changed signed identity"
        );
        if saved.confirmed {
            ensure!(
                saved.state == TransactionState::Successful,
                "synthetic void reverted; business action not authorized"
            );
            let tag = keccak256("cwzec/client-milestone/synthetic-void/v1");
            ensure!(
                s.client().consumed(tag).await? == DepositStatus::Refunded,
                "void record missing"
            );
            ensure!(
                s.client().next_redeem_id().await? == U256::from(5),
                "fixture redemption state changed"
            );
            ensure!(
                s.client().supply().await? == supply_before,
                "supply handle changed during verification"
            );
            let matching = s
                .events()
                .await?
                .into_iter()
                .filter(|e| {
                    e.transaction_hash == expected
                        && matches!(e.event, Event::DepositVoided{deposit_id} if deposit_id == tag)
                })
                .count();
            ensure!(matching == 1, "expected one canonical void event");
            let counts = s.observation_counts().await?;
            ensure!(counts.0 == before.0, "restart duplicated history");
            let cursor = s.cursor().await?;
            let evidence = serde_json::json!({"chain_id":CHAIN,"contract":CONTRACT,"deployment_block":DEPLOYMENT,"transaction":expected,"operator_nonce":17,"confirmation_policy":{"required":2,"inclusion_counts_as":1},"canonical_checkpoint":cursor.tip,"counts":{"canonical_events":counts.0,"confirmed_events":counts.1,"confirmed_receipts":counts.2},"checks":["existing_history_preserved","restart_no_duplicate_events","signed_bytes_and_nonce_preserved","confirmed_success_receipt","synthetic_void_record_and_event","supply_handle_unchanged_during_verification"],"transactions_submitted_by_this_run":0,"limits":["trusted_single_rpc","controlled_reorg_tests_only","two_confirmations_are_reorgable","no_zcash_payment","no_deposit_settlement"]});
            std::fs::write(
                root.join(".local/zama-client-evidence.json"),
                serde_json::to_vec_pretty(&evidence)?,
            )?;
            println!(
                "PASS: two-confirmation CWZEC acceptance; existing transaction {}, no new submissions",
                expected
            );
            return Ok(());
        }
        if poll % 5 == 0 {
            println!("awaiting_confirmations: saved identity retained");
        }
        tokio::time::sleep(Duration::from_secs(12)).await;
    }
    anyhow::bail!("confirmation wait bounded out; rerun this read-only verifier")
}
