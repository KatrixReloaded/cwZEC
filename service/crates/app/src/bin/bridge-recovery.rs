//! Authenticated local operator helper; signing and payment remain in the coordinator.
use alloy::primitives::{B256, U256};
use anyhow::{Context, Result, ensure};
use app::{
    config::{Config, ZamaConfig},
    core::bridge_types::BridgeDomain,
    store::Store,
};
use serde::Deserialize;
use serde_json::json;
use std::io::Read;
use subtle::ConstantTimeEq;
use zama::contract::CwzecClient;
use zcash::rpc::LightwalletClient;

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
enum Action {
    Status,
    Death,
    Finish {
        redeem_id: U256,
        deposit_id: B256,
    },
    Recredit {
        redeem_id: U256,
        version: u64,
    },
    Refund {
        deposit_id: B256,
        version: u64,
        participant: String,
        destination: String,
    },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    admin_key: String,
    request: Action,
}

#[tokio::main]
async fn main() {
    let mut stage = "configuration and authentication";
    if let Err(error) = run(&mut stage).await {
        eprintln!(
            "Recovery request blocked during {stage}. All journals and liabilities retained."
        );
        if let Some(code) = zcash::rpc::failure_code(&error) {
            eprintln!("Recovery diagnostic: grpc_code={code}");
        }
        if let Some(code) = error
            .downcast_ref::<tokio_postgres::Error>()
            .and_then(|e| e.code())
        {
            eprintln!("Recovery diagnostic: sqlstate={}", code.code());
        }
        std::process::exit(1);
    }
}
async fn run(stage: &mut &'static str) -> Result<()> {
    let config = Config::from_env()?;
    ensure!(
        config.confirmations == 3,
        "three Zcash confirmations required"
    );
    let mut input = String::new();
    std::io::stdin().take(8193).read_to_string(&mut input)?;
    ensure!(input.len() <= 8192, "request too large");
    let request = authenticate(&input, &config.admin_key)?;
    *stage = "stored state lookup";
    let c = ZamaConfig::from_env()?;
    let mut store = Store::connect(&c.database_url).await?;
    store.migrate().await?;
    store
        .initialize_bridge(BridgeDomain {
            chain_id: c.chain_id,
            contract: c.contract,
        })
        .await?;
    if matches!(request.request, Action::Status) {
        let redeems = store.bridge_redeems().await?;
        let deposits = store.bridge_deposits().await?;
        let refunds = store.bridge_refunds().await?;
        let zcash_height = store.load().await?.checkpoint.height;
        let evm = store.zama_cursor().await?;
        let mut refund_status = Vec::with_capacity(refunds.len());
        for refund in refunds {
            let attempt = store.get_attempt(&refund.operation_id).await?;
            refund_status.push(json!({
                "id":refund.deposit_id,"state":format!("{:?}",refund.state),"version":refund.version,
                "payment":attempt.as_ref().map(|a| json!({
                    "signed":a.prepared.is_some(),
                    "broadcastAcknowledged":a.broadcast,
                    "observedHeight":a.observed_height,
                    "expiryHeight":a.prepared.as_ref().map(|p|p.expiry_height)
                }))
            }));
        }
        println!(
            "{}",
            json!({
                "redemptions": redeems.iter().map(|r| json!({"id":r.id,"state":format!("{:?}",r.state),"version":r.version,"canonical":r.canonical})).collect::<Vec<_>>(),
                "deposits": deposits.iter().map(|d| json!({"id":d.deposit_id,"state":format!("{:?}",d.state),"version":d.version,"canonical":d.canonical})).collect::<Vec<_>>(),
                "refunds": refund_status,
                "scanned": {"zcash":zcash_height,"evm":evm.as_ref().map(|c|c.tip.number),"evmHalted":evm.as_ref().map(|c|c.halted)}
            })
        );
        return Ok(());
    }
    if matches!(request.request, Action::Death) {
        let client = LightwalletClient::connect(&config.endpoint).await?;
        let attempts = store.issued_attempts().await?;
        let reports = app::core::payout_death::inspect_issued_attempts(&client, &attempts).await;
        println!("{}", serde_json::to_string(&reports)?);
        return Ok(());
    }
    // Evidence uses one completed accounting report, even while scanners advance.
    // Authorizations still require the current scanner pair.
    *stage = "completed accounting and settlement evidence selection";
    let finish = if let Action::Finish {
        redeem_id,
        deposit_id,
    } = &request.request
    {
        Some(
            store
                .recovery_evidence(*redeem_id, *deposit_id, None)
                .await?,
        )
    } else {
        None
    };
    let pair = if let Some((pair, _)) = &finish {
        *pair
    } else {
        store.bridge_checkpoint_pair().await?
    };
    *stage = "canonical EVM checkpoint validation";
    let contract = CwzecClient::connect(&c.endpoint, c.chain_id, c.contract).await?;
    contract.check_canonical(pair.checkpoint).await?;
    ensure!(
        zama::watcher::Watcher::new(&contract)
            .latest()
            .await?
            .reference
            .number
            > pair.checkpoint.number,
        "two EVM confirmations required"
    );
    *stage = "canonical Zcash checkpoint validation";
    let zcash = LightwalletClient::connect(&config.endpoint).await?;
    ensure!(
        u64::from(zcash.tip().await?) >= u64::from(pair.zcash_height) + 2
            && zcash.block_hash(pair.zcash_height).await? == pair.zcash_hash,
        "canonical Zcash checkpoint required"
    );
    match request.request {
        Action::Recredit { redeem_id, version } => {
            *stage = "re-credit authorization";
            let r = store
                .authorize_bridge_recredit_at(redeem_id, version, pair)
                .await?;
            println!(
                "{}",
                json!({"id":r.id,"state":format!("{:?}",r.state),"version":r.version})
            );
        }
        Action::Refund {
            deposit_id,
            version,
            participant,
            destination,
        } => {
            *stage = "refund authorization";
            let f = store
                .authorize_bridge_refund_at(deposit_id, version, &participant, &destination, pair)
                .await?;
            println!(
                "{}",
                json!({"id":f.deposit_id,"state":format!("{:?}",f.state),"version":f.version})
            );
        }
        Action::Finish {
            redeem_id,
            deposit_id,
        } => {
            use std::{io::Write, os::unix::fs::OpenOptionsExt};
            *stage = "pinned accounting and settlement evidence recheck";
            let (_, evidence) = finish.context("recovery evidence not prepared")?;
            let id = evidence["accounting_checkpoint"]
                .as_i64()
                .context("checkpoint identity missing")?;
            // Recheck the same report and all settlement facts after RPC checks.
            let (verified_pair, verified) = store
                .recovery_evidence(redeem_id, deposit_id, Some(id))
                .await?;
            ensure!(
                verified_pair == pair && verified == evidence,
                "recovery evidence changed during verification"
            );
            *stage = "sanitized evidence file save";
            let path = ".local/bridge-recovery-evidence.json";
            if std::path::Path::new(path).exists() {
                let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
                ensure!(
                    saved["redeem_id"] == evidence["redeem_id"]
                        && saved["deposit_id"] == evidence["deposit_id"]
                        && saved["checks"] == evidence["checks"],
                    "existing evidence belongs to different results"
                );
                println!("Recovery comparisons passed; existing evidence retained at {path}");
            } else {
                let temporary = format!("{path}.{}.tmp", std::process::id());
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&temporary)?;
                file.write_all(serde_json::to_string_pretty(&evidence)?.as_bytes())?;
                file.sync_all()?;
                std::fs::hard_link(&temporary, path)?; // atomic creation, never overwrites evidence
                std::fs::remove_file(temporary)?;
                println!("Recovery comparisons passed; sanitized evidence saved to {path}");
            }
        }
        Action::Status | Action::Death => unreachable!(),
    }
    Ok(())
}

fn authenticate(input: &str, expected: &str) -> Result<Request> {
    let request: Request = serde_json::from_str(input)?;
    ensure!(
        bool::from(request.admin_key.as_bytes().ct_eq(expected.as_bytes())),
        "operator authentication required"
    );
    Ok(request)
}

#[cfg(test)]
#[path = "bridge-recovery/tests.rs"]
mod tests;
