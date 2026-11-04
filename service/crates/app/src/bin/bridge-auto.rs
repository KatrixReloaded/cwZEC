//! Single-process testnet coordinator. Scanners precede every accounting/action pass.
use anyhow::{Context, Result, ensure};
use app::{
    config::{Config, ZamaConfig},
    core::{
        Service,
        bridge::BridgeReconciler,
        bridge_mint::{BridgeMinter, MintFees},
        bridge_redeem::BridgeRedeemer,
        bridge_refund::BridgeRefunder,
        destination::DestinationKey,
        zama::ZamaService,
    },
    store::Store,
};
use std::{
    env,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use zama::{
    contract::{CwzecClient, load_signer},
    fhe,
};
use zcash::rpc::LightwalletClient;

#[tokio::main]
async fn main() {
    let mut stage = "configuration";
    if let Err(error) = run(&mut stage).await {
        eprintln!(
            "Bridge stopped during {stage}; durable journals retained. Inspect configuration and reconciliation state."
        );
        report_failure(&error);
        std::process::exit(1);
    }
}
async fn run(stage: &mut &'static str) -> Result<()> {
    eprintln!(
        "bridge startup: configuration (optimized={})",
        !cfg!(debug_assertions)
    );
    let config = Config::from_env()?;
    ensure!(
        config.confirmations == 3,
        "bridge requires three Zcash confirmations"
    );
    let c = ZamaConfig::from_env()?;
    ensure!(c.chain_id == 11155111, "Sepolia only");
    let ready = Arc::new(AtomicBool::new(false));
    let (shutdown_tx, mut shutdown) = tokio::sync::watch::channel(false);
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let signal_ready = ready.clone();
    let signal_task = tokio::spawn(async move {
        tokio::select! {
            _ = interrupt.recv() => {},
            _ = terminate.recv() => {},
        }
        let _ = shutdown_tx.send(true);
        signal_ready.store(false, Ordering::Release);
        eprintln!(
            "Bridge shutdown requested; finishing the current scan batch or action pass and draining HTTP requests."
        );
    });
    // One connection holds the coordinator lease for the entire process lifetime.
    let mut store = Store::connect(&c.database_url).await?;
    store.claim_coordinator().await?;
    let endpoint = config.endpoint.clone();
    *stage = "wallet startup";
    eprintln!("bridge startup: wallet");
    let wallet = Service::open(config).await?;
    *stage = "EVM startup and role checks";
    eprintln!("bridge startup: EVM and role checks");
    let signer = load_signer(Path::new(&env::var("BRIDGE_SERVICE_KEY_PATH")?))?;
    let auditor = load_signer(Path::new(&env::var("BRIDGE_AUDITOR_KEY_PATH")?))?;
    let key = Arc::new(DestinationKey::load(Path::new(&env::var(
        "BRIDGE_DESTINATION_KEY_PATH",
    )?))?);
    let sidecar = env::var("SIDECAR_URL")?;
    let contract = CwzecClient::connect(&c.endpoint, c.chain_id, c.contract).await?;
    ensure!(
        contract.service().await? == signer.address()
            && contract.auditor().await? == auditor.address()
            && contract
                .has_role(contract.minter_role().await?, signer.address())
                .await?,
        "bridge roles do not match configured keys"
    );
    *stage = "scanner and reconciler startup";
    eprintln!("bridge startup: scanners and reconcilers");
    let mut observer = ZamaService::open(
        &c.database_url,
        &c.endpoint,
        c.chain_id,
        c.contract,
        c.deployment_block,
    )
    .await?;
    store
        .initialize_bridge(app::core::bridge_types::BridgeDomain {
            chain_id: c.chain_id,
            contract: c.contract,
        })
        .await?;
    let origin = store
        .bridge_origin()
        .await?
        .context("verified clean deployment required")?;
    ensure!(
        origin.service == signer.address()
            && origin.auditor == auditor.address()
            && origin.block.number == c.deployment_block,
        "origin identity mismatch"
    );
    store.check_destination_key(&key).await?;
    let once = env::args().any(|a| a == "once");
    let http_task = if once {
        None
    } else {
        let port: u16 = env::var("BRIDGE_HTTP_PORT")
            .unwrap_or("8787".into())
            .parse()?;
        ensure!(port != 0, "invalid HTTP port");
        let host: std::net::IpAddr = env::var("BRIDGE_HTTP_HOST")
            .unwrap_or_else(|_| "127.0.0.1".into())
            .parse()
            .context("invalid HTTP host")?;
        let listener = tokio::net::TcpListener::bind((host, port)).await?;
        let api = app::api::BridgeApi::new(
            Store::connect(&c.database_url).await?,
            Arc::new(CwzecClient::connect(&c.endpoint, c.chain_id, c.contract).await?),
            Arc::new(fhe::Client::new(&sidecar)?),
            key.clone(),
            endpoint.clone(),
            wallet.deposit_address.clone(),
            wallet.admin_key.clone(),
            ready.clone(),
        )
        .with_payment_lock(wallet.payment_lock.clone());
        eprintln!("cWZEC bridge HTTP listening on {}", listener.local_addr()?);
        let mut http_shutdown = shutdown.clone();
        Some(tokio::spawn(async move {
            axum::serve(listener, app::api::bridge_router(Arc::new(api)))
                .with_graceful_shutdown(async move {
                    let _ = http_shutdown.wait_for(|requested| *requested).await;
                })
                .await
        }))
    };
    let mut reconciler = BridgeReconciler::new(
        Store::connect(&c.database_url).await?,
        contract,
        fhe::Client::new(&sidecar)?,
        LightwalletClient::connect(&endpoint).await?,
    )
    .await?;
    let mut minter = BridgeMinter::new(
        Store::connect(&c.database_url).await?,
        ZamaService::open(
            &c.database_url,
            &c.endpoint,
            c.chain_id,
            c.contract,
            c.deployment_block,
        )
        .await?,
        fhe::Client::new(&sidecar)?,
    )
    .await?;
    let mut redeemer = BridgeRedeemer::new(
        Store::connect(&c.database_url).await?,
        ZamaService::open(
            &c.database_url,
            &c.endpoint,
            c.chain_id,
            c.contract,
            c.deployment_block,
        )
        .await?,
        fhe::Client::new(&sidecar)?,
        wallet.clone(),
    )
    .await?;
    let mut refunder = BridgeRefunder::new(
        Store::connect(&c.database_url).await?,
        ZamaService::open(
            &c.database_url,
            &c.endpoint,
            c.chain_id,
            c.contract,
            c.deployment_block,
        )
        .await?,
        wallet.clone(),
    )
    .await?;
    let fees = MintFees {
        gas_limit: env::var("BRIDGE_GAS_LIMIT")
            .unwrap_or("3000000".into())
            .parse()?,
        max_fee_per_gas: env::var("BRIDGE_MAX_FEE_WEI")
            .unwrap_or("100000000".into())
            .parse()?,
        max_priority_fee_per_gas: env::var("BRIDGE_PRIORITY_FEE_WEI")
            .unwrap_or("1000000".into())
            .parse()?,
    };
    ensure!(
        fees.gas_limit <= 16_000_000 && fees.max_fee_per_gas <= 20_000_000_000,
        "testnet fee bound exceeded"
    );
    'coordinator: loop {
        if *shutdown.borrow() {
            break;
        }
        if http_task.as_ref().is_some_and(|task| task.is_finished()) {
            ensure!(*shutdown.borrow(), "HTTP server stopped");
            break;
        }
        // Preserve operational readiness during routine refresh. A failed pass,
        // shutdown, or persisted halt still clears it; startup begins false.
        store.check_coordinator_connection()?;
        // No action is attempted if either scan fails or detects a reconciliation halt.
        *stage = "Zcash scan";
        eprintln!("bridge scan: Zcash starting");
        if !wallet.scan_until(|| *shutdown.borrow()).await? {
            break;
        }
        *stage = "EVM scan";
        eprintln!("bridge scan: Zcash complete; EVM starting");
        loop {
            if *shutdown.borrow() {
                break 'coordinator;
            }
            let scanned = observer
                .scan_once()
                .await
                .context("EVM scan: batch")?
                .tip
                .number;
            let head = observer.latest_height().await.context("EVM scan: head")?;
            eprintln!("bridge scan: evm={scanned} head={head}");
            if scanned >= head {
                break;
            }
        }
        if *shutdown.borrow() {
            break;
        }
        let pass: Result<()> = async {
            *stage = "payout death evidence";
            redeemer.collect_death_once().await?;
            *stage = "checkpoint selection";
            let pair = reconciler.checkpoint_pair().await?;
            *stage = "redemption reconciliation";
            redeemer.reconcile_at(pair, &signer, &auditor, &key).await?;
            *stage = "refund reconciliation";
            refunder.reconcile_at(pair).await?;
            *stage = "deposit reconciliation and accounting";
            let report = reconciler.reconcile_at(pair, &signer, &auditor).await?;
            ensure!(
                report.coverage_complete && report.fee_reserve.is_some(),
                "accounting does not authorize progress"
            );
            store.check_coordinator_connection()?;
            *stage = "redemption advancement";
            if !redeemer.advance_once(report.id, &signer, fees).await? {
                *stage = "refund advancement";
                if !refunder.advance_once(report.id, &signer, fees).await? {
                    *stage = "mint advancement";
                    minter.mint_once(report.id, &signer, fees).await?;
                }
            }
            println!(
                "bridge pass reconciled: zcash={} evm={} checkpoint={}",
                pair.zcash_height, pair.checkpoint.number, report.id
            );
            Ok(())
        }
        .await;
        ready.store(pass.is_ok(), Ordering::Release);
        if *shutdown.borrow() {
            ready.store(false, Ordering::Release);
        }
        if let Err(error) = &pass {
            // A lagging checkpoint or transient SDK failure leaves all claims/bytes intact.
            eprintln!(
                "Bridge pass blocked during {stage}; journal retained; next pass will reconcile before action."
            );
            report_failure(error);
            if once {
                return pass;
            }
        }
        if once {
            return Ok(());
        }
        tokio::select! {
            _ = shutdown.changed() => {},
            _ = tokio::time::sleep(Duration::from_secs(12)) => {},
        }
    }
    ready.store(false, Ordering::Release);
    if let Some(task) = http_task {
        task.await??;
    }
    // Retain the database lease until the current pass and HTTP drain have ended.
    signal_task.abort();
    eprintln!("Bridge shutdown complete; durable journals retained.");
    Ok(())
}

// Only typed codes and exact local messages may leave the error chain. Remote
// messages, SQL details, endpoints and SDK response bodies remain private.
fn report_failure(error: &anyhow::Error) {
    if let Some(code) = zcash::rpc::failure_code(error) {
        eprintln!("bridge diagnostic: grpc_code={code}");
    }
    if let Some(error) = error.downcast_ref::<tokio_postgres::Error>() {
        if let Some(code) = error.code() {
            eprintln!("bridge diagnostic: sqlstate={}", code.code());
        } else {
            eprintln!("bridge diagnostic: database transport or decoding failure");
        }
    }
    for cause in error.chain() {
        if let Some(message) = safe_message(&cause.to_string()) {
            eprintln!("bridge diagnostic: {message}");
        }
    }
}

fn safe_message(message: &str) -> Option<&str> {
    match message {
        "scan: tip"
        | "scan: retry tip"
        | "scan: retry checkpoint hash"
        | "near-tip GetBlock availability retries exhausted"
        | "near-tip GetBlock retry tip changed"
        | "near-tip GetBlock retry checkpoint changed"
        | "EVM scan: batch"
        | "EVM scan: head"
        | "scan: load"
        | "scan: checkpoint hash"
        | "scan: next block"
        | "scan: commit"
        | "scan: reload"
        | "lightwalletd GetLightdInfo"
        | "lightwalletd GetBlock"
        | "lightwalletd GetTreeState"
        | "lightwalletd GetTransaction"
        | "lightwalletd readiness"
        | "connect lightwalletd"
        | "lightwalletd is not testnet"
        | "unsupported lightwalletd consensus branch"
        | "scanned Ironwood tree disagrees with chain tree"
        | "tree block hash mismatch"
        | "wrong tree network or height"
        | "Ironwood tree size mismatch"
        | "missing Ironwood tree size"
        | "witness checkpoint size mismatch"
        | "note authentication path does not match checkpoint"
        | "missing checkpoint witness"
        | "nonconsecutive checkpoint"
        | "checkpoint parent mismatch"
        | "conflicting received note"
        | "RPC block lookup failed"
        | "RPC receipt lookup failed"
        | "RPC log lookup failed"
        | "RPC chain lookup failed"
        | "RPC block has zero hash"
        | "RPC block number mismatch"
        | "RPC block unavailable"
        | "RPC receipt transaction mismatch"
        | "RPC receipt target mismatch"
        | "receipt missing block number"
        | "receipt missing block hash"
        | "receipt is not canonical"
        | "RPC chain mismatch"
        | "invalid scan range"
        | "previous checkpoint changed"
        | "range anchor changed"
        | "header fetch task failed"
        | "nonconsecutive canonical headers"
        | "redemption: revalidate recovery"
        | "redemption: checkpoint"
        | "redemption: validate checkpoint"
        | "redemption: recredit"
        | "redemption: recover payout"
        | "redemption: fulfill"
        | "redemption: select payout input"
        | "redemption: reserve payout input"
        | "redemption: payment accounting"
        | "redemption: proving task"
        | "redemption: sign payout"
        | "insufficient confirmed unreserved single-note liquidity; fragmented liquidity is unsupported"
        | "single input requires positive change"
        | "insufficient confirmed unreserved liquidity within payout input limit"
        | "current accounting and operator reserve must cover payout network fees"
        | "payout inputs must cover network fee and positive change"
        | "payout network fee mismatch"
        | "new payment needs latest complete accounting"
        | "unsupported payout input count"
        | "current accounting and operator fee reserve required"
        | "Ironwood transaction proving/signing failed"
        | "header outside scan or duplicated"
        | "missing canonical header"
        | "missing scan end header"
        | "invalid empty scan"
        | "event outside canonical scan"
        | "RPC log bound exceeded"
        | "conflicting duplicate log"
        | "log header unavailable"
        | "log is not canonical"
        | "receipt bound exceeded"
        | "event receipt unavailable"
        | "event receipt failed or changed block"
        | "previous checkpoint changed during scan"
        | "range end changed during scan"
        | "removed lifecycle log"
        | "log contract mismatch"
        | "log missing block number"
        | "log outside scan range"
        | "log missing block hash"
        | "log missing transaction hash"
        | "log missing index"
        | "log missing signature"
        | "unknown lifecycle signature"
        | "Zama watcher halted; manual investigation required"
        | "reorg exceeds retained history; watcher halted"
        | "inconsistent RPC head"
        | "Zcash reorg affects bridge action; reconciliation halted"
        | "reorg exceeds 100-block recovery limit"
        | "reorg exceeds retained birthday or 100-block recovery limit" => Some(message),
        _ => None,
    }
}

#[cfg(test)]
#[path = "bridge-auto/tests.rs"]
mod tests;
