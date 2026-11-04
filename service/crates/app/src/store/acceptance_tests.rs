//! Bounded PostgreSQL regressions with controlled chain facts, never live evidence.
use super::{
    Store,
    bridge_tests::{accounting, evidence, evm, note, scan, setup},
    zama_test_support::{TestDb, batch, contract, reference},
};
use crate::core::{bridge_types::*, redeem_types::*};
use alloy::{
    primitives::{Address, B256, Bytes, U256},
    signers::local::PrivateKeySigner,
};
use anyhow::Result;
use zama::{
    contract::{SignedTransaction, TransactionDomain, TransactionFees},
    watcher::{Event, EventObservation, ReceiptObservation},
};
use zcash::core::{LocalSigner, NoteId};

fn input() -> NoteId {
    NoteId {
        txid: [11; 32],
        action_index: 0,
    }
}
fn destination() -> Result<String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "bridge-acceptance-regression-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let signer = LocalSigner::create(&path)?;
    let address = signer.addresses()?.0;
    std::fs::remove_file(path)?;
    Ok(address)
}
async fn funded(db: &TestDb) -> Result<(Store, BridgeDeposit)> {
    let mut s = setup(db).await?;
    scan(
        &mut s,
        2,
        vec![note(10, 60_000, false), note(11, 200_000, true)],
        vec![],
    )
    .await?;
    scan(&mut s, 3, vec![], vec![]).await?;
    scan(&mut s, 4, vec![], vec![]).await?;
    evm(&mut s, 3, vec![]).await?;
    let d = s.ingest_bridge_deposits().await?.remove(0);
    Ok((s, d))
}
// Corrupt only the newest persisted report, retaining the older valid report at
// precisely the same scanner pair. This isolates latest-report admission policy.
async fn supersede(s: &mut Store, evm_height: u64, incomplete: bool) -> Result<i64> {
    let newer = s.bridge_accounting(&accounting(2, evm_height, 0)).await?;
    s.client
        .execute(
            "UPDATE bridge_checkpoints SET valid=$2,coverage_complete=$3 WHERE id=$1",
            &[&newer.id, &incomplete, &!incomplete],
        )
        .await?;
    Ok(newer.id)
}
async fn unacted(s: &Store, id: i64) -> Result<()> {
    assert!(
        !s.client
            .query_one(
                "SELECT acted_on FROM bridge_checkpoints WHERE id=$1",
                &[&id]
            )
            .await?
            .get::<_, bool>(0)
    );
    Ok(())
}
fn mint(d: &BridgeDeposit) -> Result<SignedTransaction> {
    let signer: PrivateKeySigner =
        "0000000000000000000000000000000000000000000000000000000000000001".parse()?;
    let domain = TransactionDomain::new(11155111, contract())?;
    Ok(domain.sign_operator(
        &signer,
        &domain.mint(
            d.recipient.unwrap(),
            B256::repeat_byte(44),
            Bytes::from_static(&[1]),
            d.deposit_id,
        ),
        TransactionFees {
            nonce: 0,
            gas_limit: 100_000,
            max_fee_per_gas: 2,
            max_priority_fee_per_gas: 1,
        },
    )?)
}

#[tokio::test]
async fn latest_invalid_or_incomplete_report_blocks_old_mint_admission() -> Result<()> {
    for incomplete in [false, true] {
        let db = TestDb::new().await?;
        let (mut s, d) = funded(&db).await?;
        let old = s.bridge_accounting(&accounting(2, 2, 0)).await?;
        let newer = supersede(&mut s, 2, incomplete).await?;
        assert!(newer > old.id);
        for id in [old.id, newer] {
            assert!(
                s.authorize_bridge_mint(d.deposit_id, d.version, id)
                    .await
                    .is_err()
            );
            unacted(&s, id).await?;
        }
        let unchanged = s.bridge_deposit(d.deposit_id).await?.unwrap();
        assert_eq!(unchanged.version, d.version);
        assert!(!unchanged.claimed);
        let fresh = s.bridge_accounting(&accounting(2, 2, 0)).await?;
        assert!(
            s.authorize_bridge_mint(d.deposit_id, d.version, fresh.id)
                .await?
                .claimed
        );
        db.cleanup().await?;
    }
    Ok(())
}

#[tokio::test]
async fn latest_invalid_or_incomplete_report_blocks_new_mint_signature() -> Result<()> {
    for incomplete in [false, true] {
        let db = TestDb::new().await?;
        let (mut s, d) = funded(&db).await?;
        let old = s.bridge_accounting(&accounting(2, 2, 0)).await?;
        let claimed = s
            .authorize_bridge_mint(d.deposit_id, d.version, old.id)
            .await?;
        let signed = mint(&d)?;
        s.initialize_zama_lane(signed.sender(), 0, 0).await?;
        supersede(&mut s, 2, incomplete).await?;
        assert!(s.save_zama_signed(&d.operation_id, &signed).await.is_err());
        assert!(s.zama_transaction(&d.operation_id).await?.is_none());
        assert_eq!(s.zama_next_nonce(signed.sender()).await?, 0);
        let fresh = s.bridge_accounting(&accounting(2, 2, 0)).await?;
        s.authorize_bridge_mint(d.deposit_id, claimed.version, fresh.id)
            .await?;
        s.save_zama_signed(&d.operation_id, &signed).await?;
        assert_eq!(
            s.zama_transaction(&d.operation_id)
                .await?
                .unwrap()
                .signed
                .raw(),
            signed.raw()
        );
        db.cleanup().await?;
    }
    Ok(())
}

#[tokio::test]
async fn latest_invalid_or_incomplete_report_blocks_old_redeem_reservation() -> Result<()> {
    for incomplete in [false, true] {
        let db = TestDb::new().await?;
        let (mut s, _) = funded(&db).await?;
        let event = EventObservation {
            block: reference(4, 0),
            transaction_hash: B256::repeat_byte(40),
            log_index: 0,
            event: Event::RedeemRequested {
                redeem_id: U256::from(1),
                user: Address::repeat_byte(7),
                amount: B256::repeat_byte(44),
                dest_ct: Bytes::from_static(&[1, 2]),
            },
        };
        evm(&mut s, 5, vec![event.clone()]).await?;
        let pair = s.bridge_checkpoint_pair().await?;
        let r = s
            .record_bridge_redeem(&event, pair, 60_000, Some(&destination()?))
            .await?;
        let old = s.bridge_accounting(&accounting(2, 4, 0)).await?;
        let newer = supersede(&mut s, 4, incomplete).await?;
        for id in [old.id, newer] {
            assert!(
                s.reserve_bridge_redeem(r.id, r.version, id, input())
                    .await
                    .is_err()
            );
            unacted(&s, id).await?;
        }
        assert!(s.reserved_inputs().await?.is_empty());
        assert!(s.get_attempt(&r.operation_id).await?.is_none());
        assert_eq!(s.bridge_redeems().await?[0].version, r.version);
        let fresh = s.bridge_accounting(&accounting(2, 4, 0)).await?;
        assert_eq!(
            s.reserve_bridge_redeem(r.id, r.version, fresh.id, input())
                .await?
                .state,
            RedeemState::Reserved
        );
        db.cleanup().await?;
    }
    Ok(())
}

#[tokio::test]
async fn latest_invalid_or_incomplete_report_blocks_old_refund_reservation() -> Result<()> {
    for incomplete in [false, true] {
        let db = TestDb::new().await?;
        let (mut s, d) = funded(&db).await?;
        let r = s
            .authorize_bridge_refund(d.deposit_id, d.version, "alice", &destination()?)
            .await?;
        let signer: PrivateKeySigner =
            "0000000000000000000000000000000000000000000000000000000000000001".parse()?;
        let domain = TransactionDomain::new(11155111, contract())?;
        let signed = domain.sign_operator(
            &signer,
            &domain.void_deposit(d.deposit_id),
            TransactionFees {
                nonce: 0,
                gas_limit: 100_000,
                max_fee_per_gas: 2,
                max_priority_fee_per_gas: 1,
            },
        )?;
        s.initialize_zama_lane(signer.address(), 0, 0).await?;
        s.save_zama_signed(&r.void_operation_id, &signed).await?;
        let event = EventObservation {
            block: reference(4, 0),
            transaction_hash: signed.hash(),
            log_index: 0,
            event: Event::DepositVoided {
                deposit_id: d.deposit_id,
            },
        };
        let mut b = batch(s.zama_cursor().await?.unwrap().tip, 5, 0);
        b.chain_id = 11155111;
        b.events = vec![event.clone()];
        b.receipts = vec![ReceiptObservation {
            transaction_hash: signed.hash(),
            block: event.block,
            from: signer.address(),
            to: contract(),
            success: true,
        }];
        s.apply_zama_scan(&b).await?;
        let current = s.bridge_deposit(d.deposit_id).await?.unwrap();
        s.reconcile_bridge_deposit(d.deposit_id, current.version, &evidence(event, None))
            .await?;
        let old = s.bridge_accounting(&accounting(2, 4, 0)).await?;
        let newer = supersede(&mut s, 4, incomplete).await?;
        for id in [old.id, newer] {
            assert!(
                s.reserve_bridge_refund(d.deposit_id, r.version, id, input())
                    .await
                    .is_err()
            );
            unacted(&s, id).await?;
        }
        assert!(s.reserved_inputs().await?.is_empty());
        assert!(s.get_attempt(&r.operation_id).await?.is_none());
        assert_eq!(s.bridge_refunds().await?[0].version, r.version);
        let fresh = s.bridge_accounting(&accounting(2, 4, 0)).await?;
        assert_eq!(
            s.reserve_bridge_refund(d.deposit_id, r.version, fresh.id, input())
                .await?
                .state,
            RefundState::Reserved
        );
        db.cleanup().await?;
    }
    Ok(())
}

#[tokio::test]
async fn evm_only_advance_requires_fresh_accounting_and_halt_survives_reopen() -> Result<()> {
    let db = TestDb::new().await?;
    let (mut s, _) = funded(&db).await?;
    s.bridge_accounting(&accounting(2, 2, 0)).await?;
    assert_eq!(s.http_health().await?["report_current"], true);
    evm(&mut s, 4, vec![]).await?;
    assert_eq!(s.http_health().await?["report_current"], false);
    s.bridge_accounting(&accounting(2, 3, 0)).await?;
    let health = s.http_health().await?;
    assert_eq!(health["report_current"], true);
    assert_eq!(health["halted"], false);
    s.halt_bridge("acceptance-regression-persisted-halt")
        .await?;
    drop(s);
    let mut s = db.connect().await?;
    s.migrate().await?;
    assert_eq!(s.http_health().await?["halted"], true);
    assert!(s.bridge_checkpoint_pair().await.is_err());
    assert!(s.ingest_bridge_deposits().await.is_err());
    db.cleanup().await
}

#[tokio::test]
async fn empty_database_repeated_migrations_preserve_empty_state() -> Result<()> {
    let db = TestDb::new().await?;
    let mut s = db.connect().await?;
    for _ in 0..3 {
        s.migrate().await?;
        assert!(!s.is_initialized().await?);
        assert!(!s.bridge_enabled().await?);
        assert!(s.zama_cursor().await?.is_none());
        assert!(s.bridge_deposits().await?.is_empty());
        assert!(s.bridge_redeems().await?.is_empty());
        assert!(s.bridge_refunds().await?.is_empty());
        assert!(s.issued_attempts().await?.is_empty());
        assert_eq!(
            s.client
                .query_one("SELECT count(*) FROM bridge_checkpoints", &[])
                .await?
                .get::<_, i64>(0),
            0
        );
    }
    db.cleanup().await
}

fn destination_key() -> Result<crate::core::destination::DestinationKey> {
    let path = std::env::temp_dir().join(format!(
        "bridge-acceptance-key-{:#x}",
        PrivateKeySigner::random().address()
    ));
    let key = crate::core::destination::DestinationKey::create(&path)?;
    std::fs::remove_file(path)?;
    Ok(key)
}

#[tokio::test]
async fn destination_key_is_bound_by_saved_preparation_across_reopen() -> Result<()> {
    use alloy::primitives::keccak256;
    let db = TestDb::new().await?;
    let mut s = setup(&db).await?;
    let a = destination_key()?;
    let b = destination_key()?;
    s.check_destination_key(&a).await?;
    s.check_destination_key(&b).await?;
    let user = Address::repeat_byte(7);
    let destination = destination()?;
    let envelope = a.encrypt_destination(11155111, contract(), user, &destination)?;
    let domain = TransactionDomain::new(11155111, contract())?;
    let call = domain.request_redeem(user, B256::repeat_byte(44), Bytes::new(), envelope);
    let saved = s
        .save_user_redeem_request(
            "key-bound",
            user,
            60_000,
            keccak256(destination.as_bytes()),
            call.calldata(),
        )
        .await?;
    drop(s);
    let s = db.connect().await?;
    assert!(s.check_destination_key(&b).await.is_err());
    s.check_destination_key(&a).await?;
    let restored = s.user_redeem_request("key-bound").await?.unwrap();
    assert_eq!(restored.calldata, saved.calldata);
    assert_eq!(restored.destination_hash, saved.destination_hash);
    assert_eq!(restored.user, saved.user);
    assert_eq!(restored.amount, saved.amount);
    assert!(restored.signed.is_none());
    db.cleanup().await
}

#[tokio::test]
async fn destination_key_is_bound_by_recorded_external_burn_across_reopen() -> Result<()> {
    let db = TestDb::new().await?;
    let (mut s, _) = funded(&db).await?;
    let a = destination_key()?;
    let b = destination_key()?;
    let destination = destination()?;
    let user = Address::repeat_byte(7);
    let event = EventObservation {
        block: reference(4, 0),
        transaction_hash: B256::repeat_byte(43),
        log_index: 0,
        event: Event::RedeemRequested {
            redeem_id: U256::from(1),
            user,
            amount: B256::repeat_byte(44),
            dest_ct: a.encrypt_destination(11155111, contract(), user, &destination)?,
        },
    };
    evm(&mut s, 5, vec![event.clone()]).await?;
    let pair = s.bridge_checkpoint_pair().await?;
    let saved = s
        .record_bridge_redeem(&event, pair, 60_000, Some(&destination))
        .await?;
    drop(s);
    let s = db.connect().await?;
    assert!(s.check_destination_key(&b).await.is_err());
    s.check_destination_key(&a).await?;
    let restored = s.bridge_redeems().await?.remove(0);
    assert_eq!(restored.destination, saved.destination);
    assert_eq!(restored.version, saved.version);
    assert_eq!(restored.state, saved.state);
    db.cleanup().await
}

#[tokio::test]
async fn latest_report_gates_new_payment_bytes_but_preserves_identical_replay() -> Result<()> {
    use zcash::core::PreparedTransaction;
    for incomplete in [false, true] {
        let db = TestDb::new().await?;
        let (mut s, _) = funded(&db).await?;
        let event = EventObservation {
            block: reference(4, 0),
            transaction_hash: B256::repeat_byte(45),
            log_index: 0,
            event: Event::RedeemRequested {
                redeem_id: U256::from(1),
                user: Address::repeat_byte(7),
                amount: B256::repeat_byte(44),
                dest_ct: Bytes::from_static(&[1, 2]),
            },
        };
        evm(&mut s, 5, vec![event.clone()]).await?;
        let pair = s.bridge_checkpoint_pair().await?;
        let r = s
            .record_bridge_redeem(&event, pair, 60_000, Some(&destination()?))
            .await?;
        let good = s.bridge_accounting(&accounting(2, 4, 0)).await?;
        let r = s
            .reserve_bridge_redeem(r.id, r.version, good.id, input())
            .await?;
        let prepared = PreparedTransaction {
            bytes: b"controlled payment signing regression bytes".to_vec(),
            txid: [80; 32],
            inputs: vec![input()],
            expiry_height: 20,
            fee: REDEEM_FEE_ZAT,
        };
        supersede(&mut s, 4, incomplete).await?;
        assert!(s.check_payment_accounting().await.is_err());
        assert!(s.save_prepared(&r.operation_id, &prepared).await.is_err());
        assert!(
            s.get_attempt(&r.operation_id)
                .await?
                .unwrap()
                .prepared
                .is_none()
        );
        assert_eq!(s.reserved_inputs().await?.len(), 1);
        let current = s.bridge_redeems().await?.remove(0);
        assert_eq!(current.state, RedeemState::Reserved);
        assert_eq!(current.version, r.version);

        let fresh = s.bridge_accounting(&accounting(2, 4, 0)).await?;
        s.check_payment_accounting().await?;
        unacted(&s, fresh.id).await?;
        s.save_prepared(&r.operation_id, &prepared).await?;
        assert!(
            s.client
                .query_one(
                    "SELECT acted_on FROM bridge_checkpoints WHERE id=$1",
                    &[&fresh.id]
                )
                .await?
                .get::<_, bool>(0)
        );
        supersede(&mut s, 4, incomplete).await?;
        assert!(s.check_payment_accounting().await.is_err());
        s.save_prepared(&r.operation_id, &prepared).await?;
        let retained = s
            .get_attempt(&r.operation_id)
            .await?
            .unwrap()
            .prepared
            .unwrap();
        assert_eq!(retained.bytes, prepared.bytes);
        assert_eq!(retained.txid, prepared.txid);
        assert!(retained.inputs == prepared.inputs);
        assert_eq!(retained.expiry_height, prepared.expiry_height);
        assert_eq!(retained.fee, prepared.fee);
        assert_eq!(s.reserved_inputs().await?.len(), 1);
        db.cleanup().await?;
    }
    Ok(())
}
