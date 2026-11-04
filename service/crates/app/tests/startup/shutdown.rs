//! SIGINT during a real scanner backlog must stop before accounting, then resume.
use super::*;

pub(crate) async fn catchup_shutdown(db: &TestDb) -> Result<()> {
    let keys = Keys::new()?;
    let initial = seed(db, &keys).await?;
    let end = LOWER_HEIGHT + 512;
    let mut data = peers::data(end);
    for height in LOWER_HEIGHT..=end {
        data.trees.get_mut(&height).unwrap().ironwood_tree =
            initial.trees[&LOWER_HEIGHT].ironwood_tree.clone();
        data.blocks
            .get_mut(&height)
            .unwrap()
            .chain_metadata
            .as_mut()
            .unwrap()
            .ironwood_commitment_tree_size = 1;
    }
    let grpc = peers::serve(data).await;
    let facts = Arc::new(Facts::default());
    facts.head.store(3, Ordering::SeqCst);
    let evm = rpc(&keys, facts.clone()).await?;
    let sidecar = sidecar(facts.clone()).await?;
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = socket.local_addr()?.port();
    drop(socket);
    let command = || keys.command(db, &grpc.endpoint, &evm.url, &sidecar.url, port);
    fails(command(), "scanner and reconciler startup").await?;
    let sql = db.client().await?;
    sql.execute("INSERT INTO bridge_origin(singleton,deployment_number,deployment_hash,deployment_tx,runtime_hash,service,auditor) VALUES(true,1,$1,$2,$3,$4,$5)",&[&B256::from(U256::from(2)).as_slice(),&[2u8;32].as_slice(),&keccak256([0x60,0]).as_slice(),&keys.service.as_slice(),&keys.auditor.as_slice()]).await?;
    let http = reqwest::Client::builder().no_proxy().build()?;
    let url = format!("http://127.0.0.1:{port}");
    let mut child = Process(command().spawn()?);
    wait_until(
        async || {
            Ok(sql
                .query_one("SELECT current_height FROM account", &[])
                .await?
                .get::<_, i64>(0)
                > i64::from(LOWER_HEIGHT))
        },
        "catch-up committed its first block",
    )
    .await?;
    signal(&child, "-INT")?;
    exit(&mut child, true).await?;
    let saved: i64 = sql
        .query_one("SELECT current_height FROM account", &[])
        .await?
        .get(0);
    ensure!(
        saved < i64::from(end),
        "shutdown drained the entire backlog"
    );
    ensure!(
        facts.decrypts.load(Ordering::SeqCst) == 0,
        "shutdown started accounting"
    );
    ensure!(!lease(&sql).await?, "shutdown retained coordinator lease");
    ensure!(!ready(&http, &url).await?, "stopped scanner remained ready");

    // A new process must validate the saved checkpoint and continue from it.
    grpc.state.lock().unwrap().tip = saved as u32 + 1;
    let mut child = Process(command().spawn()?);
    wait_until(
        async || ready(&http, &url).await,
        "partial catch-up restart",
    )
    .await?;
    ensure!(
        sql.query_one("SELECT current_height FROM account", &[])
            .await?
            .get::<_, i64>(0)
            == saved + 1,
        "restart lost saved scanner progress"
    );
    signal(&child, "-TERM")?;
    exit(&mut child, true).await?;
    ensure!(
        facts.unexpected.lock().unwrap().is_empty(),
        "unexpected mutation RPC"
    );
    ensure!(sql.query_one("SELECT (SELECT count(*) FROM operations)+(SELECT count(*) FROM zama_transactions)+(SELECT count(*) FROM reservations)",&[]).await?.get::<_,i64>(0)==0,"catch-up shutdown changed payment journals");
    Ok(())
}
