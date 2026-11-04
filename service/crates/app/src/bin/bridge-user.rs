//! Execution-time wallet helper. Never reads a user's private key or signs their burn.
use alloy::primitives::{Address, B256, Bytes};
use anyhow::{Context, Result, ensure};
use app::{config::ZamaConfig, core::destination::DestinationKey, store::Store};
use serde::Deserialize;
use std::{env, io::Read, path::Path};
use zama::{
    contract::{CwzecClient, SignedTransaction},
    fhe,
};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    user: Address,
    amount: String,
    destination: String,
}
#[tokio::main]
async fn main() {
    if run().await.is_err() {
        eprintln!(
            "User request stopped; preserve request ID and signed transaction. No replacement burn is authorized."
        );
        std::process::exit(1);
    }
}
async fn run() -> Result<()> {
    let args: Vec<_> = env::args().collect();
    let command = args
        .get(1)
        .context("prepare, submit, adopt or recover required")?;
    let id = args.get(2).context("durable request ID required")?;
    let c = ZamaConfig::from_env()?;
    ensure!(c.chain_id == 11155111, "Sepolia only");
    let client = CwzecClient::connect(&c.endpoint, c.chain_id, c.contract).await?;
    let mut store = Store::connect(&c.database_url).await?;
    store.migrate().await?;
    if command == "prepare" {
        let mut input = String::new();
        std::io::stdin().take(8193).read_to_string(&mut input)?;
        ensure!(input.len() <= 8192, "request too long");
        let req: Request = serde_json::from_str(&input)?;
        let amount: u64 = req.amount.parse()?;
        let key = DestinationKey::load(Path::new(&env::var("BRIDGE_DESTINATION_KEY_PATH")?))?;
        let prepared = app::core::user_prepare::prepare_redeem(
            &mut store,
            &client,
            &fhe::Client::new(&env::var("SIDECAR_URL")?)?,
            &key,
            id,
            req.user,
            amount,
            &req.destination,
        )
        .await?;
        if let Some(signed) = prepared.signed {
            println!(
                "{}",
                serde_json::json!({"requestId":id,"transactionHash":signed.hash(),"alreadySubmitted":true})
            );
        } else {
            println!(
                "{}",
                serde_json::json!({"requestId":id,"transaction":{"from":prepared.user,"to":c.contract,"chainId":"0xaa36a7","value":"0x0","data":prepared.calldata}})
            );
        }
        return Ok(());
    }
    let saved = store
        .user_redeem_request(id)
        .await?
        .context("prepare this request first")?;
    let signed = if let Some(signed) = saved.signed {
        signed
    } else if command == "submit" {
        let raw: Bytes =
            std::fs::read_to_string(args.get(3).context("signed transaction file required")?)?
                .trim()
                .parse()?;
        SignedTransaction::restore(c.chain_id, c.contract, saved.user, raw)?
    } else if command == "adopt" {
        let hash: B256 = args
            .get(3)
            .context("wallet transaction hash required")?
            .parse()?;
        client.user_transaction(hash, saved.user).await?
    } else {
        anyhow::bail!("no signed transaction; obtain wallet signature or transaction hash")
    };
    ensure!(
        matches!(command.as_str(), "submit" | "adopt" | "recover"),
        "invalid command"
    );
    store.save_user_redeem_signed(id, &signed).await?;
    if command != "adopt" {
        let _ = client.broadcast(&signed).await;
    }
    println!(
        "{}",
        serde_json::json!({"requestId":id,"transactionHash":signed.hash(),"journaled":true})
    );
    Ok(())
}
