use super::{TransactionState, zama_test_support::*};
use anyhow::Result;
use zama::watcher::{Event, EventObservation, ReceiptObservation};

#[tokio::test]
async fn zama_catchup_preserves_old_events_and_recent_rewind_window() -> Result<()> {
    use zama::watcher::{RETAIN_BLOCKS, SCAN_BLOCKS};
    let db = TestDb::new().await?;
    let mut store = initialized(&db).await?;
    let end = 1 + SCAN_BLOCKS;
    let mut scan = batch(reference(1, 0), end, 0);
    scan.blocks
        .retain(|b| b.reference.number == 2 || b.reference.number >= end - RETAIN_BLOCKS);
    for n in [2, end] {
        scan.events.push(EventObservation {
            block: reference(n, 0),
            transaction_hash: hash(n + 10_000, 0),
            log_index: 0,
            event: Event::DepositVoided {
                deposit_id: hash(n + 20_000, 0),
            },
        });
    }
    let removed = scan.blocks.remove(50);
    assert!(store.apply_zama_scan(&scan).await.is_err());
    assert_eq!(store.zama_cursor().await?.unwrap().tip, reference(1, 0));
    assert_eq!(fact_counts(&store).await?, (0, 0, 0));
    scan.blocks.insert(50, removed);
    store.apply_zama_scan(&scan).await?;
    drop(store);
    let mut store = db.connect().await?;
    assert_eq!(store.zama_cursor().await?.unwrap().tip, reference(end, 0));
    let history = store.zama_history().await?;
    assert_eq!(history.len() as u64, RETAIN_BLOCKS + 1);
    assert_eq!(history[1], reference(end - 1, 0)); // Accounting checkpoint remains available.
    assert_eq!(history.last().unwrap(), &reference(end - RETAIN_BLOCKS, 0));
    assert_eq!(store.zama_observation_counts().await?, (2, 1, 0));
    assert!(store.apply_zama_scan(&scan).await.is_err());
    store
        .rewind_zama(reference(end, 0), reference(end - 1, 0))
        .await?;
    assert_eq!(store.zama_observation_counts().await?, (1, 1, 0));
    assert_eq!(store.zama_events().await?[0].block, reference(2, 0));
    db.cleanup().await
}

#[tokio::test]
async fn zama_signed_restart_preserves_bytes_hash_and_nonce_ownership() -> Result<()> {
    let db = TestDb::new().await?;
    let mut store = initialized(&db).await?;
    let first = signed(7, 10);
    store.initialize_zama_lane(first.sender(), 7, 7).await?;
    store.save_zama_signed("durable", &first).await?;
    store.save_zama_signed("durable", &first).await?;
    assert!(
        store
            .save_zama_signed("durable", &signed(7, 11))
            .await
            .is_err()
    );
    assert!(
        store
            .save_zama_signed("other", &signed(7, 11))
            .await
            .is_err()
    );
    drop(store);
    let mut store = db.connect().await?;
    let restored = store.zama_transaction("durable").await?.unwrap();
    assert_eq!(restored.signed.raw(), first.raw());
    assert_eq!(restored.signed.hash(), first.hash());
    assert_eq!(restored.signed.nonce(), 7);
    assert_eq!(restored.state, TransactionState::Unresolved);
    store.initialize_zama_lane(first.sender(), 99, 100).await?;
    assert_eq!(store.zama_next_nonce(first.sender()).await?, 8);
    assert!(
        store
            .save_zama_signed("next", &signed(8, 12))
            .await
            .is_err()
    );
    assert_eq!(fact_counts(&store).await?, (0, 0, 1));
    db.cleanup().await
}

#[tokio::test]
async fn zama_duplicate_replay_and_late_failure_are_atomic() -> Result<()> {
    let db = TestDb::new().await?;
    let mut store = initialized(&db).await?;
    let event = EventObservation {
        block: reference(2, 0),
        transaction_hash: hash(20, 0),
        log_index: 0,
        event: Event::DepositVoided {
            deposit_id: hash(30, 0),
        },
    };
    let mut scan = batch(reference(1, 0), 2, 0);
    scan.events = vec![event.clone(), event.clone()];
    scan.chain_id = 2;
    assert!(store.apply_zama_scan(&scan).await.is_err());
    scan.chain_id = 1;
    scan.contract = alloy::primitives::Address::repeat_byte(43);
    assert!(store.apply_zama_scan(&scan).await.is_err());
    assert_eq!(fact_counts(&store).await?, (0, 0, 0));
    assert_eq!(store.zama_cursor().await?.unwrap().tip, reference(1, 0));
    scan.contract = contract();
    store.apply_zama_scan(&scan).await?;
    assert_eq!(store.zama_observation_counts().await?, (1, 0, 0));
    assert!(store.apply_zama_scan(&scan).await.is_err());
    let mut failing = batch(reference(2, 0), 3, 0);
    let mut e = event.clone();
    e.block = reference(3, 0);
    e.transaction_hash = hash(21, 0);
    let mut conflicting = e.clone();
    conflicting.event = Event::DepositVoided {
        deposit_id: hash(31, 0),
    };
    failing.events = vec![e, conflicting];
    assert!(store.apply_zama_scan(&failing).await.is_err());
    assert_eq!(store.zama_cursor().await?.unwrap().tip, reference(2, 0));
    assert_eq!(fact_counts(&store).await?, (1, 0, 0));
    assert_eq!(store.zama_history().await?.len(), 2);
    failing.events.clear();
    store.apply_zama_scan(&failing).await?;
    assert_eq!(store.zama_cursor().await?.unwrap().tip, reference(3, 0));
    assert_eq!(store.zama_observation_counts().await?, (1, 1, 0));
    db.cleanup().await
}

#[tokio::test]
async fn zama_configuration_identity_survives_restart() -> Result<()> {
    let db = TestDb::new().await?;
    let mut store = initialized(&db).await?;
    assert!(
        store
            .initialize_zama(2, contract(), 2, reference(1, 0))
            .await
            .is_err()
    );
    assert!(
        store
            .initialize_zama(
                1,
                alloy::primitives::Address::repeat_byte(43),
                2,
                reference(1, 0)
            )
            .await
            .is_err()
    );
    assert!(
        store
            .initialize_zama(1, contract(), 3, reference(2, 0))
            .await
            .is_err()
    );
    assert!(
        store
            .initialize_zama_lane(signed(0, 10).sender(), 0, 1)
            .await
            .is_err()
    );
    assert_eq!(store.zama_cursor().await?.unwrap().tip, reference(1, 0));
    db.cleanup().await
}

fn receipt(
    tx: &zama::contract::SignedTransaction,
    branch: u64,
    success: bool,
) -> ReceiptObservation {
    ReceiptObservation {
        transaction_hash: tx.hash(),
        block: reference(2, branch),
        from: tx.sender(),
        to: contract(),
        success,
    }
}

#[tokio::test]
async fn zama_success_and_revert_release_lane_only_after_two_confirmations() -> Result<()> {
    for success in [true, false] {
        let db = TestDb::new().await?;
        let mut store = initialized(&db).await?;
        let tx = signed(0, 10);
        store.initialize_zama_lane(tx.sender(), 0, 0).await?;
        store.save_zama_signed("one", &tx).await?;
        store.mark_zama_submission(tx.hash(), true).await?;
        assert_eq!(
            store.zama_transaction("one").await?.unwrap().state,
            TransactionState::Pending
        );
        let mut scan = batch(reference(1, 0), 2, 0);
        scan.receipts.push(receipt(&tx, 0, success));
        store.apply_zama_scan(&scan).await?;
        let included = store.zama_transaction("one").await?.unwrap();
        assert_eq!(
            included.state,
            if success {
                TransactionState::Successful
            } else {
                TransactionState::Reverted
            }
        );
        assert!(!included.confirmed);
        assert!(store.mark_zama_acted(tx.hash()).await.is_err());
        assert!(
            store
                .save_zama_signed("next", &signed(1, 11))
                .await
                .is_err()
        );
        drop(store);
        let mut store = db.connect().await?;
        assert!(!store.zama_transaction("one").await?.unwrap().confirmed);
        store.apply_zama_scan(&batch(reference(2, 0), 3, 0)).await?;
        assert!(store.zama_transaction("one").await?.unwrap().confirmed);
        assert!(store.zama_unfinished().await?.is_empty());
        assert_eq!(store.mark_zama_acted(tx.hash()).await.is_ok(), success);
        assert!(
            store
                .save_zama_signed("reuse", &signed(0, 12))
                .await
                .is_err()
        );
        store.save_zama_signed("next", &signed(1, 11)).await?;
        assert_eq!(store.zama_next_nonce(tx.sender()).await?, 2);
        db.cleanup().await?;
    }
    Ok(())
}

#[tokio::test]
async fn zama_confirmed_unacted_rewinds_and_reincluded_identity_reconfirms() -> Result<()> {
    let db = TestDb::new().await?;
    let mut store = initialized(&db).await?;
    let tx = signed(7, 10);
    store.initialize_zama_lane(tx.sender(), 7, 7).await?;
    store.save_zama_signed("one", &tx).await?;
    let mut scan = batch(reference(1, 0), 3, 0);
    scan.receipts.push(receipt(&tx, 0, true));
    store.apply_zama_scan(&scan).await?;
    assert!(store.zama_transaction("one").await?.unwrap().confirmed);
    store.rewind_zama(reference(3, 0), reference(1, 0)).await?;
    let orphan = store.zama_transaction("one").await?.unwrap();
    assert_eq!(orphan.state, TransactionState::Unresolved);
    assert!(!orphan.confirmed);
    assert_eq!(orphan.signed.raw(), tx.raw());
    assert_eq!(store.zama_next_nonce(tx.sender()).await?, 8);
    assert!(store.mark_zama_acted(tx.hash()).await.is_err());
    let mut scan = batch(reference(1, 0), 2, 1);
    scan.receipts.push(receipt(&tx, 1, true));
    store.apply_zama_scan(&scan).await?;
    assert!(!store.zama_transaction("one").await?.unwrap().confirmed);
    store.apply_zama_scan(&batch(reference(2, 1), 3, 1)).await?;
    assert!(store.zama_transaction("one").await?.unwrap().confirmed);
    assert_eq!(fact_counts(&store).await?, (0, 2, 1));
    db.cleanup().await
}

#[tokio::test]
async fn zama_acted_or_later_nonce_dependency_reorg_halts_durably() -> Result<()> {
    for acted in [true, false] {
        let db = TestDb::new().await?;
        let mut store = initialized(&db).await?;
        let tx = signed(0, 10);
        store.initialize_zama_lane(tx.sender(), 0, 0).await?;
        store.save_zama_signed("one", &tx).await?;
        let mut scan = batch(reference(1, 0), 3, 0);
        scan.receipts.push(receipt(&tx, 0, true));
        store.apply_zama_scan(&scan).await?;
        if acted {
            store.mark_zama_acted(tx.hash()).await?;
            store.mark_zama_acted(tx.hash()).await?;
        } else {
            store.save_zama_signed("later", &signed(1, 11)).await?;
        }
        drop(store);
        let mut store = db.connect().await?;
        assert!(
            store
                .rewind_zama(reference(3, 0), reference(1, 0))
                .await
                .is_err()
        );
        assert_eq!(
            halt_reason(&store).await?.as_deref(),
            Some("acted_operation_reorg")
        );
        assert_eq!(store.zama_cursor().await?.unwrap().tip, reference(3, 0));
        drop(store);
        let mut store = db.connect().await?;
        assert!(store.zama_cursor().await?.unwrap().halted);
        assert!(
            store
                .apply_zama_scan(&batch(reference(3, 0), 4, 0))
                .await
                .is_err()
        );
        assert_eq!(
            store.zama_transaction("one").await?.unwrap().signed.raw(),
            tx.raw()
        );
        db.cleanup().await?;
    }
    Ok(())
}

#[tokio::test]
async fn zama_legacy_migration_preserves_all_facts_and_resets_eligibility_once() -> Result<()> {
    let db = TestDb::legacy().await?;
    let mut store = db.connect().await?;
    let tx = signed(7, 10);
    store.client.execute("INSERT INTO zama_chain(singleton,chain_id,contract,deployment_block,tip_number,tip_hash,finalized_number,finalized_hash) VALUES(true,1,$1,2,3,$2,3,$2)", &[&contract().as_slice(), &reference(3,0).hash.as_slice()]).await?;
    for n in 1..=3 {
        store
            .client
            .execute(
                "INSERT INTO zama_blocks VALUES($1,$2,$3)",
                &[
                    &(n as i64),
                    &reference(n, 0).hash.as_slice(),
                    &hash(n - 1, 0).as_slice(),
                ],
            )
            .await?;
    }
    store
        .client
        .execute(
            "INSERT INTO zama_lane VALUES(true,$1,8)",
            &[&tx.sender().as_slice()],
        )
        .await?;
    store.client.execute("INSERT INTO zama_transactions(operation_id,sender,nonce,tx_hash,raw,state,finalized) VALUES('legacy',$1,7,$2,$3,'successful',true)", &[&tx.sender().as_slice(), &tx.hash().as_slice(), &tx.raw().as_ref()]).await?;
    let event = serde_json::to_string(&Event::DepositVoided {
        deposit_id: hash(30, 0),
    })?;
    for (branch, canonical) in [(0, true), (1, false)] {
        store
            .client
            .execute(
                "INSERT INTO zama_events VALUES(2,$1,$2,0,$3,$4,$4)",
                &[
                    &reference(2, branch).hash.as_slice(),
                    &tx.hash().as_slice(),
                    &event,
                    &canonical,
                ],
            )
            .await?;
        store
            .client
            .execute(
                "INSERT INTO zama_receipts VALUES($1,2,$2,$3,$4,true,$5,$5)",
                &[
                    &tx.hash().as_slice(),
                    &reference(2, branch).hash.as_slice(),
                    &tx.sender().as_slice(),
                    &contract().as_slice(),
                    &canonical,
                ],
            )
            .await?;
    }
    let before = legacy_facts(&store).await?;
    store.migrate().await?;
    assert_eq!(legacy_facts(&store).await?, before);
    assert_eq!(fact_counts(&store).await?, (2, 2, 1));
    assert_eq!(store.zama_history().await?.len(), 3);
    assert_eq!(store.zama_next_nonce(tx.sender()).await?, 8);
    let restored = store.zama_transaction("legacy").await?.unwrap();
    assert_eq!(restored.signed.raw(), tx.raw());
    assert_eq!(restored.state, TransactionState::Successful);
    assert!(!restored.confirmed);
    assert_eq!(store.zama_observation_counts().await?, (1, 0, 0));
    assert!(store.mark_zama_acted(tx.hash()).await.is_err());
    store.apply_zama_scan(&batch(reference(3, 0), 3, 0)).await?;
    store.mark_zama_acted(tx.hash()).await?;
    store.migrate().await?;
    assert!(store.zama_transaction("legacy").await?.unwrap().confirmed);
    assert_eq!(store.zama_observation_counts().await?, (1, 1, 1));
    assert_eq!(fact_counts(&store).await?, (2, 2, 1));
    drop(store);
    let mut store = db.connect().await?;
    assert!(
        store
            .rewind_zama(reference(3, 0), reference(1, 0))
            .await
            .is_err()
    );
    assert_eq!(
        halt_reason(&store).await?.as_deref(),
        Some("acted_operation_reorg")
    );
    db.cleanup().await
}

async fn legacy_facts(store: &super::Store) -> Result<String> {
    Ok(store.client.query_one("SELECT jsonb_build_object(
        'events',(SELECT jsonb_agg(to_jsonb(e)-'finalized'-'confirmed' ORDER BY block_hash,tx_hash,log_index) FROM zama_events e),
        'receipts',(SELECT jsonb_agg(to_jsonb(r)-'finalized'-'confirmed'-'acted_on' ORDER BY block_hash,tx_hash) FROM zama_receipts r),
        'blocks',(SELECT jsonb_agg(to_jsonb(b) ORDER BY number) FROM zama_blocks b),
        'lane',(SELECT jsonb_agg(to_jsonb(l)) FROM zama_lane l),
        'transactions',(SELECT jsonb_agg(to_jsonb(t)-'finalized'-'confirmed' ORDER BY operation_id) FROM zama_transactions t),
        'chain',(SELECT jsonb_agg(to_jsonb(c)-'finalized_number'-'finalized_hash') FROM zama_chain c))::text", &[]).await?.get(0))
}
