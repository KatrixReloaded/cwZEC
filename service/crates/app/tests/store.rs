use anyhow::Result;
use app::store::Store;
use zcash::core::{Account, Checkpoint, NoteId, PreparedTransaction, ScanUpdate, WalletNote};
fn schema_url(url: &str, schema: &str) -> String {
    if url.starts_with("postgres://") || url.starts_with("postgresql://") {
        format!(
            "{url}{}options=-csearch_path%3D{schema}",
            if url.contains('?') { "&" } else { "?" }
        )
    } else {
        format!("{url} options='-csearch_path={schema}'")
    }
}
fn cp(height: u32) -> Checkpoint {
    Checkpoint {
        height,
        hash: [height as u8; 32],
        parent: [(height - 1) as u8; 32],
        frontier: vec![1, height as u8],
    }
}
fn note(tag: u8, position: u64) -> WalletNote {
    WalletNote {
        id: NoteId {
            txid: [tag; 32],
            action_index: 0,
        },
        recipient: [2; 43],
        value: 100000,
        rho: [tag; 32],
        rseed: [4; 32],
        nullifier: [tag + 1; 32],
        position,
        internal: false,
        memo: [7; 512],
        mined_height: 101,
        spent_height: None,
        witness: vec![1, tag],
    }
}
fn scan(height: u32) -> ScanUpdate {
    ScanUpdate {
        checkpoint: cp(height),
        received: vec![],
        witnesses: vec![],
        spent_nullifiers: vec![],
        txids: vec![],
    }
}
#[tokio::test]
async fn postgres_wallet_and_payment_recovery() -> Result<()> {
    let url = std::env::var("DATABASE_URL")?;
    let schema = format!(
        "store_test_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    );
    let (admin, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls).await?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    admin
        .batch_execute(&format!("CREATE SCHEMA \"{schema}\""))
        .await?;
    let result = lifecycle(&url, &schema).await;
    admin
        .batch_execute(&format!("DROP SCHEMA \"{schema}\" CASCADE"))
        .await?;
    result
}
async fn lifecycle(url: &str, schema: &str) -> Result<()> {
    let mut s = Store::connect(&schema_url(url, schema)).await?;
    s.migrate().await?;
    s.migrate().await?;
    assert!(!s.is_initialized().await?);
    let account = Account {
        birthday: 101,
        fvk: [9; 96],
        use_qsk: true,
    };
    let mut bad_birthday = account.clone();
    bad_birthday.birthday = 100;
    assert!(s.initialize(&bad_birthday, &cp(100)).await.is_err());
    s.initialize(&account, &cp(100)).await?;
    assert!(s.is_initialized().await?);
    let mut wrong = account.clone();
    wrong.use_qsk = false;
    assert!(s.initialize(&wrong, &cp(100)).await.is_err());
    let a = note(10, 0);
    let b = note(20, 1);
    let c = note(30, 2);
    let mut u = scan(101);
    u.received = vec![a.clone(), b.clone(), c.clone()];
    s.commit_scan(&u).await?;
    drop(s);
    let mut s = Store::connect(&schema_url(url, schema)).await?;
    let state = s.load().await?;
    assert!(state.account == account && state.checkpoint == cp(101));
    assert!(state.notes == vec![a.clone(), b.clone(), c.clone()]);
    s.initialize(&account, &cp(100)).await?;
    assert!(s.load().await?.checkpoint.height == 101);
    assert!(
        s.reserve("unconfirmed", "recipient", 80000, &[a.id], 2)
            .await
            .is_err()
    );
    assert!(s.get_attempt("unconfirmed").await?.is_none());
    s.reserve("payment", "recipient", 80000, &[a.id], 1).await?;
    s.reserve("payment", "recipient", 80000, &[a.id], 1).await?;
    assert!(
        s.reserve("payment", "other", 80000, &[a.id], 1)
            .await
            .is_err()
    );
    assert!(
        s.reserve("payment", "recipient", 80001, &[a.id], 1)
            .await
            .is_err()
    );
    assert!(
        s.reserve("payment", "recipient", 80000, &[b.id], 1)
            .await
            .is_err()
    );
    assert!(
        s.reserve("conflict", "recipient", 80000, &[b.id, a.id], 1)
            .await
            .is_err()
    );
    assert!(s.get_attempt("conflict").await?.is_none());
    s.reserve("reserved_only", "recipient", 80000, &[b.id], 1)
        .await?;
    assert!(s.reserved_inputs().await?.len() == 2);
    let mut p = PreparedTransaction {
        bytes: vec![1, 2, 3],
        txid: [90; 32],
        inputs: vec![b.id],
        expiry_height: 120,
        fee: 10000,
    };
    assert!(s.save_prepared("payment", &p).await.is_err());
    assert!(s.get_attempt("payment").await?.unwrap().prepared.is_none());
    p.inputs = vec![a.id];
    p.expiry_height = 0;
    assert!(s.save_prepared("payment", &p).await.is_err());
    p.expiry_height = 120;
    s.save_prepared("payment", &p).await?;
    s.save_prepared("payment", &p).await?;
    p.bytes.push(4);
    assert!(s.save_prepared("payment", &p).await.is_err());
    p.bytes.pop();
    p.fee += 1;
    assert!(s.save_prepared("payment", &p).await.is_err());
    p.fee -= 1;
    s.mark_broadcast("payment").await?;
    assert!(s.mark_broadcast("reserved_only").await.is_err());
    assert!(s.unfinished().await?.len() == 2);
    // A missing witness rolls back spends, observations, and the checkpoint together.
    let mut u = scan(102);
    u.spent_nullifiers = vec![a.nullifier];
    u.txids = vec![p.txid];
    assert!(s.commit_scan(&u).await.is_err());
    assert!(s.load().await?.checkpoint.height == 101);
    assert!(
        s.get_attempt("payment")
            .await?
            .unwrap()
            .observed_height
            .is_none()
    );
    u.witnesses = vec![(b.id, vec![2, 20]), (c.id, vec![2, 30])];
    s.commit_scan(&u).await?;
    let mut expected_b = b.clone();
    expected_b.witness = vec![2, 20];
    let mut expected_c = c.clone();
    expected_c.witness = vec![2, 30];
    assert!(s.load().await?.notes == vec![expected_b, expected_c]);
    assert!(s.get_attempt("payment").await?.unwrap().observed_height == Some(102));
    assert!(s.unfinished().await?.len() == 1);
    assert!(
        s.reserve("spent", "recipient", 80000, &[a.id], 1)
            .await
            .is_err()
    );
    drop(s);
    let mut s = Store::connect(&schema_url(url, schema)).await?;
    let attempt = s.get_attempt("payment").await?.unwrap();
    assert!(attempt.broadcast && attempt.prepared.unwrap().bytes == p.bytes);
    assert!(s.rewind(101, [0; 32]).await.is_err());
    s.rewind(101, cp(101).hash).await?;
    assert!(s.load().await?.notes == vec![a.clone(), b.clone(), c.clone()]);
    assert!(s.unfinished().await?.len() == 2);
    s.save_prepared("payment", &p).await?;
    assert!(
        s.reserve("after_rewind", "recipient", 80000, &[a.id], 1)
            .await
            .is_err()
    );
    assert!(
        s.reserve("after_restart", "recipient", 80000, &[b.id], 1)
            .await
            .is_err()
    );
    s.rewind(100, cp(100).hash).await?;
    assert!(s.load().await?.notes.is_empty());
    p.inputs = vec![b.id];
    assert!(s.save_prepared("reserved_only", &p).await.is_err());
    assert!(
        s.reserve("orphan", "recipient", 80000, &[c.id], 1)
            .await
            .is_err()
    );
    p.inputs = vec![a.id];
    s.save_prepared("payment", &p).await?;
    // An orphaned reserved spend authority cannot be claimed through an alias ID.
    let mut alias = a.clone();
    alias.id.txid = [99; 32];
    let mut alias_scan = scan(101);
    alias_scan.received = vec![alias.clone()];
    s.commit_scan(&alias_scan).await?;
    assert!(
        s.reserve("alias", "recipient", 80000, &[alias.id], 1)
            .await
            .is_err()
    );
    assert!(s.get_attempt("alias").await?.is_none());
    s.rewind(100, cp(100).hash).await?;
    let mut u = scan(101);
    u.received = vec![a, b, c];
    s.commit_scan(&u).await?;
    let mut duplicate = note(40, 0);
    duplicate.mined_height = 102;
    let mut u = scan(102);
    u.received = vec![duplicate.clone()];
    assert!(s.commit_scan(&u).await.is_err());
    duplicate.position = 3;
    duplicate.nullifier = [11; 32];
    u.received = vec![duplicate];
    assert!(s.commit_scan(&u).await.is_err());
    let mut u = scan(102);
    u.checkpoint.parent = [0; 32];
    assert!(s.commit_scan(&u).await.is_err());
    assert!(s.load().await?.checkpoint.height == 101);
    Ok(())
}
