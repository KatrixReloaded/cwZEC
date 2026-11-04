//! Bounded read-only chain observer; no operator key or bridge automation.
use app::{config::ZamaConfig, core::zama::ZamaService};
#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("Zama observer stopped: {error}");
        std::process::exit(1);
    }
}
async fn run() -> anyhow::Result<()> {
    let c = ZamaConfig::from_env()?;
    let mut service = ZamaService::open(
        &c.database_url,
        &c.endpoint,
        c.chain_id,
        c.contract,
        c.deployment_block,
    )
    .await?;
    let once = std::env::args().nth(1).as_deref() == Some("once");
    loop {
        let cursor = service.scan_once().await?;
        let (events, confirmed, receipts) = service.observation_counts().await?;
        println!(
            "scan={} events={} confirmed_events={} confirmed_receipts={}",
            cursor.tip.number, events, confirmed, receipts
        );
        if once {
            return Ok(());
        }
        // Fast bounded backfill; ordinary steady-state polling.
        tokio::time::sleep(std::time::Duration::from_secs(
            if cursor.tip.number < service.latest_height().await? {
                1
            } else {
                12
            },
        ))
        .await;
    }
}
