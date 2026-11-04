//! Cross-path acceptance with disposable PostgreSQL and controlled chain facts.
use super::{
    Store,
    bridge_tests::{accounting, cp, evidence, evm, note, scan, setup},
    zama_test_support::{TestDb, batch, contract, reference},
};
use alloy::{
    primitives::{Address, B256, Bytes, U256},
    signers::local::PrivateKeySigner,
};
use anyhow::Result;
use zama::{
    contract::{SignedTransaction, TransactionDomain, TransactionFees},
    watcher::{Event, EventObservation, ReceiptObservation},
};
use zcash::core::{LocalSigner, NoteId, PreparedTransaction, ScanUpdate};

fn operator() -> PrivateKeySigner {
    "0000000000000000000000000000000000000000000000000000000000000001"
        .parse()
        .unwrap()
}
fn sign(nonce: u64, intent: &zama::contract::BoundCall) -> SignedTransaction {
    TransactionDomain::new(11155111, contract())
        .unwrap()
        .sign_operator(
            &operator(),
            intent,
            TransactionFees {
                nonce,
                gas_limit: 100_000,
                max_fee_per_gas: 2,
                max_priority_fee_per_gas: 1,
            },
        )
        .unwrap()
}
async fn observed(
    s: &mut Store,
    signed: &SignedTransaction,
    height: u64,
    event: Event,
) -> Result<EventObservation> {
    let event = EventObservation {
        block: reference(height, 0),
        transaction_hash: signed.hash(),
        log_index: 0,
        event,
    };
    let mut b = batch(s.zama_cursor().await?.unwrap().tip, height + 1, 0);
    b.chain_id = 11155111;
    b.events.push(event.clone());
    b.receipts.push(ReceiptObservation {
        transaction_hash: signed.hash(),
        block: event.block,
        from: operator().address(),
        to: contract(),
        success: true,
    });
    s.apply_zama_scan(&b).await?;
    Ok(event)
}
fn destination() -> Result<String> {
    let path = std::env::temp_dir().join(format!(
        "recovery-acceptance-address-{}",
        std::process::id()
    ));
    let signer = LocalSigner::create(&path)?;
    let address = signer.addresses()?.0;
    std::fs::remove_file(path)?;
    Ok(address)
}

#[tokio::test]
async fn refund_and_recredit_conserve_reserve_across_pending_settled_and_old_checkpoints()
-> Result<()> {
    let db = TestDb::new().await?;
    let mut s = setup(&db).await?;
    scan(
        &mut s,
        2,
        vec![note(10, 60_000, false), note(11, 500_000, true)],
        vec![],
    )
    .await?;
    scan(&mut s, 3, vec![], vec![]).await?;
    scan(&mut s, 4, vec![], vec![]).await?;
    evm(&mut s, 3, vec![]).await?;
    let deposit = s.ingest_bridge_deposits().await?.remove(0);
    let burn = EventObservation {
        block: reference(4, 0),
        transaction_hash: B256::repeat_byte(71),
        log_index: 0,
        event: Event::RedeemRequested {
            redeem_id: U256::from(71),
            user: Address::repeat_byte(7),
            amount: B256::repeat_byte(44),
            dest_ct: Bytes::from_static(&[1]),
        },
    };
    evm(&mut s, 5, vec![burn.clone()]).await?;
    let pair = s.bridge_checkpoint_pair().await?;
    let redeem = s.record_bridge_redeem(&burn, pair, 75_000, None).await?;
    let refund = s
        .authorize_bridge_refund(
            deposit.deposit_id,
            deposit.version,
            "alice",
            &destination()?,
        )
        .await?;
    s.authorize_bridge_recredit(redeem.id, redeem.version)
        .await?;
    let pending = s.bridge_accounting(&accounting(2, 4, 0)).await?;
    assert_eq!(
        (
            pending.assets,
            pending.deposit_liability,
            pending.unpaid_redemptions,
            pending.fee_reserve
        ),
        (560_000, 60_000, 75_000, Some(425_000))
    );
    assert!(pending.coverage_complete);

    s.initialize_zama_lane(operator().address(), 0, 0).await?;
    let domain = TransactionDomain::new(11155111, contract())?;
    let void = sign(0, &domain.void_deposit(deposit.deposit_id));
    // Both identities are authorized, but the journal must reject a cross-bind.
    assert!(
        s.save_zama_signed(&redeem.recredit_id(), &void)
            .await
            .is_err()
    );
    assert_eq!(s.zama_next_nonce(operator().address()).await?, 0);
    s.save_zama_signed(&refund.void_operation_id, &void).await?;
    let event = observed(
        &mut s,
        &void,
        6,
        Event::DepositVoided {
            deposit_id: deposit.deposit_id,
        },
    )
    .await?;
    let current = s.bridge_deposit(deposit.deposit_id).await?.unwrap();
    s.reconcile_bridge_deposit(deposit.deposit_id, current.version, &evidence(event, None))
        .await?;
    let restore = sign(1, &domain.recredit(redeem.id));
    assert!(
        s.save_zama_signed(&refund.void_operation_id, &restore)
            .await
            .is_err()
    );
    s.save_zama_signed(&redeem.recredit_id(), &restore).await?;
    let event = observed(
        &mut s,
        &restore,
        8,
        Event::RedeemRecredited {
            redeem_id: redeem.id,
            user: redeem.burner,
            amount: B256::repeat_byte(81),
        },
    )
    .await?;
    let pair = s.bridge_checkpoint_pair().await?;
    s.reconcile_bridge_recredit(redeem.id, &event, pair, 75_000)
        .await?;
    let restored = s.bridge_accounting(&accounting(2, 8, 75_000)).await?;
    assert_eq!(
        (
            restored.deposit_liability,
            restored.unpaid_redemptions,
            restored.fee_reserve
        ),
        (60_000, 0, pending.fee_reserve)
    );
    assert!(restored.coverage_complete);
    let input = NoteId {
        txid: [11; 32],
        action_index: 0,
    };
    s.reserve_bridge_refund(deposit.deposit_id, refund.version, restored.id, input)
        .await?;
    let signed = PreparedTransaction {
        bytes: b"controlled refund bytes".to_vec(),
        txid: [80; 32],
        inputs: vec![input],
        expiry_height: 20,
        fee: 10_000,
    };
    s.save_prepared(&refund.operation_id, &signed).await?;
    let mut change = note(80, 440_000, true);
    change.mined_height = 5;
    s.commit_scan(&ScanUpdate {
        checkpoint: cp(5),
        received: vec![change],
        witnesses: vec![(deposit.note_id, vec![1])],
        spent_nullifiers: vec![[11; 32]],
        txids: vec![[80; 32]],
    })
    .await?;
    scan(&mut s, 6, vec![], vec![]).await?;
    scan(&mut s, 7, vec![], vec![]).await?;
    // Crash after chain success, before either broadcast acknowledgement or
    // liability settlement. Restart must recover the same issued transaction.
    drop(s);
    let mut s = db.connect().await?;
    let recovered = s.issued_attempts().await?.remove(0);
    assert!(!recovered.broadcast);
    assert_eq!(recovered.observed_height, Some(5));
    assert_eq!(recovered.prepared.unwrap().bytes, signed.bytes);
    let unsettled = s.bridge_accounting(&accounting(5, 8, 75_000)).await?;
    assert_eq!(unsettled.deposit_liability, 60_000);
    assert!(!unsettled.coverage_complete);
    assert!(
        s.recovery_evidence(redeem.id, deposit.deposit_id, None)
            .await
            .is_err()
    );
    let pair = s.bridge_checkpoint_pair().await?;
    assert_eq!(
        s.settle_bridge_refund(deposit.deposit_id, pair).await?,
        Some(B256::repeat_byte(80))
    );
    let settled = s.bridge_accounting(&accounting(5, 8, 75_000)).await?;
    assert_eq!(
        (
            settled.assets,
            settled.deposit_liability,
            settled.unpaid_redemptions,
            settled.fee_reserve
        ),
        (500_000, 0, 0, pending.fee_reserve)
    );
    assert!(settled.coverage_complete);
    let verified = s
        .recovery_evidence(redeem.id, deposit.deposit_id, None)
        .await?;
    assert_eq!(verified.1["accounting_checkpoint"], settled.id);
    assert!(
        verified.1["checks"]
            .as_object()
            .unwrap()
            .values()
            .all(|v| v == true)
    );
    // Scanner progress need not have a matching completed accounting report yet.
    scan(&mut s, 8, vec![], vec![]).await?;
    assert_ne!(s.bridge_checkpoint_pair().await?, verified.0);
    assert_eq!(
        s.recovery_evidence(redeem.id, deposit.deposit_id, None)
            .await?,
        verified
    );
    let newer = s.bridge_accounting(&accounting(6, 8, 75_000)).await?;
    assert_eq!(
        s.recovery_evidence(redeem.id, deposit.deposit_id, None)
            .await?
            .1["accounting_checkpoint"],
        newer.id
    );
    // The RPC-validated report identity survives newer scanner/report progress.
    assert_eq!(
        s.recovery_evidence(redeem.id, deposit.deposit_id, Some(settled.id))
            .await?,
        verified
    );
    let old = s.bridge_accounting(&accounting(2, 4, 0)).await?;
    assert_eq!(
        (
            old.assets,
            old.deposit_liability,
            old.unpaid_redemptions,
            old.fee_reserve
        ),
        (
            pending.assets,
            pending.deposit_liability,
            pending.unpaid_redemptions,
            pending.fee_reserve
        )
    );
    assert!(old.coverage_complete);
    assert!(
        s.rewind_zama(reference(9, 0), reference(5, 0))
            .await
            .is_err()
    );
    assert!(
        s.recovery_evidence(redeem.id, deposit.deposit_id, Some(settled.id))
            .await
            .is_err()
    );
    assert_eq!(
        s.zama_transaction(&refund.void_operation_id)
            .await?
            .unwrap()
            .signed
            .raw(),
        void.raw()
    );
    assert_eq!(
        s.zama_transaction(&redeem.recredit_id())
            .await?
            .unwrap()
            .signed
            .raw(),
        restore.raw()
    );
    assert_eq!(
        s.issued_attempts().await?.remove(0).prepared.unwrap().bytes,
        signed.bytes
    );
    db.cleanup().await
}

#[tokio::test]
async fn issued_inventory_keeps_unacknowledged_observed_and_legacy_bytes_through_migration()
-> Result<()> {
    let db = TestDb::legacy().await?;
    let legacy = db.connect().await?;
    for migration in [
        include_str!("../../migrations/001_wallet.sql"),
        include_str!("../../migrations/003_zama_confirmations.sql"),
        include_str!("../../migrations/004_bridge.sql"),
        include_str!("../../migrations/005_roundtrip.sql"),
    ] {
        legacy.client.batch_execute(migration).await?;
    }
    drop(legacy);
    let mut s = setup(&db).await?;
    scan(
        &mut s,
        2,
        vec![note(10, 200_000, true), note(11, 200_000, true)],
        vec![],
    )
    .await?;
    // Historical entries deliberately lack a bridge liability. Startup diagnostics
    // must still inspect every issued byte; unfinished() excludes observed entries.
    for (id, n, broadcast) in [("unacknowledged", 10u8, false), ("observed", 11, true)] {
        s.client.execute("INSERT INTO operations(id,destination,amount,signed_bytes,txid,expiry_height,fee,broadcast) VALUES($1,'controlled destination',50000,$2,$3,20,10000,$4)", &[&id, &vec![n; 7], &vec![n + 60; 32], &broadcast]).await?;
        s.client.execute("INSERT INTO reservations(txid,action_index,operation_id,nullifier) VALUES($1,0,$2,$1)", &[&vec![n; 32], &id]).await?;
    }
    s.client.execute("INSERT INTO operations(id,destination,amount) VALUES('unsigned','controlled destination',50000)", &[]).await?;
    s.client
        .execute(
            "INSERT INTO observed_transactions(txid,height) VALUES($1,2)",
            &[&vec![71u8; 32]],
        )
        .await?;
    let before = s.issued_attempts().await?;
    assert_eq!(before.len(), 2);
    assert!(
        before
            .iter()
            .any(|a| !a.broadcast && a.observed_height.is_none())
    );
    assert!(
        before
            .iter()
            .any(|a| a.broadcast && a.observed_height == Some(2))
    );
    // Read the legacy schema with its original query before applying migration007.
    let unfinished: i64 = s.client.query_one("SELECT count(*) FROM operations o WHERE NOT EXISTS(SELECT 1 FROM observed_transactions t WHERE t.txid=o.txid)", &[]).await?.get(0);
    assert_eq!(unfinished, 2);
    drop(s);
    let mut s = db.connect().await?;
    s.migrate().await?;
    s.migrate().await?;
    let after = s.issued_attempts().await?;
    assert_eq!(s.unfinished().await?.len(), 2);
    assert_eq!(after.len(), before.len());
    for (old, new) in before.iter().zip(after.iter()) {
        assert_eq!(old.operation_id, new.operation_id);
        assert_eq!(old.destination, new.destination);
        assert_eq!(old.amount, new.amount);
        assert!(old.inputs == new.inputs);
        assert_eq!(old.broadcast, new.broadcast);
        assert_eq!(old.observed_height, new.observed_height);
        assert_eq!(
            old.prepared.as_ref().unwrap().bytes,
            new.prepared.as_ref().unwrap().bytes
        );
        assert_eq!(
            old.prepared.as_ref().unwrap().txid,
            new.prepared.as_ref().unwrap().txid
        );
    }
    assert_eq!(s.reserved_inputs().await?.len(), 2);
    db.cleanup().await
}
