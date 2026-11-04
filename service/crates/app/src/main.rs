use app::{api, config::Config, core::Service};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let service = Service::open(Config::from_env()?).await?;
    service.scan_once().await?;
    service.recover().await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:8787").await?;
    tokio::spawn(api::scan_loop(service.clone()));
    eprintln!("cWZEC Ironwood testnet HTTP listening on 127.0.0.1:8787");
    axum::serve(listener, api::router(service)).await?;
    Ok(())
}
