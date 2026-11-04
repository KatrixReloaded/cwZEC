//! Real PostgreSQL fixtures; wallet bytes are controlled journal evidence.
use super::*;
use crate::store::{
    bridge_tests::{accounting, cp, evidence, evm, note, scan, setup},
    zama_test_support::{TestDb, batch, contract, reference},
};
use alloy::signers::local::PrivateKeySigner;
use zama::{
    contract::{TransactionDomain, TransactionFees},
    watcher::ReceiptObservation,
};
use zcash::core::{LocalSigner, PreparedTransaction, ScanUpdate};
fn operator() -> PrivateKeySigner {
    "0000000000000000000000000000000000000000000000000000000000000001"
        .parse()
        .unwrap()
}
fn destination() -> Result<String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "refund-address-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let signer = LocalSigner::create(&path)?;
    let destination = signer.addresses()?.0;
    std::fs::remove_file(path)?;
    Ok(destination)
}
async fn ready(db: &TestDb) -> Result<(Store, BridgeDeposit, String)> {
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
    Ok((s, d, destination()?))
}
async fn voided(s: &mut Store, r: &BridgeRefund) -> Result<EventObservation> {
    let signer = operator();
    s.initialize_zama_lane(signer.address(), 0, 0).await?;
    let domain = TransactionDomain::new(11155111, contract())?;
    let signed = domain.sign_operator(
        &signer,
        &domain.void_deposit(r.deposit_id),
        TransactionFees {
            nonce: 0,
            gas_limit: 100_000,
            max_fee_per_gas: 2,
            max_priority_fee_per_gas: 1,
        },
    )?;
    s.save_zama_signed(&r.void_operation_id, &signed).await?;
    let event = EventObservation {
        block: reference(4, 0),
        transaction_hash: signed.hash(),
        log_index: 0,
        event: Event::DepositVoided {
            deposit_id: r.deposit_id,
        },
    };
    let mut b = batch(s.zama_cursor().await?.unwrap().tip, 5, 0);
    b.chain_id = 11155111;
    b.events.push(event.clone());
    b.receipts.push(ReceiptObservation {
        transaction_hash: signed.hash(),
        block: event.block,
        from: signer.address(),
        to: contract(),
        success: true,
    });
    s.apply_zama_scan(&b).await?;
    let d = s.bridge_deposit(r.deposit_id).await?.unwrap();
    s.reconcile_bridge_deposit(d.deposit_id, d.version, &evidence(event.clone(), None))
        .await?;
    Ok(event)
}
fn input() -> NoteId {
    NoteId {
        txid: [11; 32],
        action_index: 0,
    }
}
fn prepared() -> PreparedTransaction {
    PreparedTransaction {
        bytes: b"controlled refund signed bytes".to_vec(),
        txid: [80; 32],
        inputs: vec![input()],
        expiry_height: 20,
        fee: FEE,
    }
}
async fn reserved(db: &TestDb) -> Result<(Store, BridgeRefund)> {
    let (mut s, d, dest) = ready(db).await?;
    let r = s
        .authorize_bridge_refund(d.deposit_id, d.version, "participant-assertion", &dest)
        .await?;
    voided(&mut s, &r).await?;
    let a = s.bridge_accounting(&accounting(2, 4, 0)).await?;
    let r = s
        .reserve_bridge_refund(d.deposit_id, r.version, a.id, input())
        .await?;
    Ok((s, r))
}
#[tokio::test]
async fn authorization_is_immutable_idempotent_and_blocks_mint() -> Result<()> {
    let db = TestDb::new().await?;
    let (mut s, d, dest) = ready(&db).await?;
    let r = s
        .authorize_bridge_refund(d.deposit_id, d.version, "alice", &dest)
        .await?;
    assert_eq!(
        s.authorize_bridge_refund(d.deposit_id, d.version, "alice", &dest)
            .await?
            .version,
        r.version
    );
    assert!(
        s.authorize_bridge_refund(d.deposit_id, d.version, "bob", &dest)
            .await
            .is_err()
    );
    let a = s.bridge_accounting(&accounting(2, 2, 0)).await?;
    let current = s.bridge_deposit(d.deposit_id).await?.unwrap();
    assert!(
        s.claim_bridge_mint(d.deposit_id, current.version, a.id)
            .await
            .is_err()
    );
    drop(s);
    let s = db.connect().await?;
    assert_eq!(s.bridge_refunds().await?.remove(0).gross_amount, 60_000);
    db.cleanup().await
}
#[tokio::test]
async fn unknown_invalid_and_stale_authorizations_fail_closed() -> Result<()> {
    let db = TestDb::new().await?;
    let (mut s, d, dest) = ready(&db).await?;
    for participant in ["", " ", "unknown", "Unknown", " alice "] {
        assert!(
            s.authorize_bridge_refund(d.deposit_id, d.version, participant, &dest)
                .await
                .is_err()
        );
    }
    assert!(
        s.authorize_bridge_refund(d.deposit_id, d.version, "alice", "not an address")
            .await
            .is_err()
    );
    assert!(
        s.authorize_bridge_refund(d.deposit_id, d.version + 1, "alice", &dest)
            .await
            .is_err()
    );
    assert!(s.bridge_refunds().await?.is_empty());
    db.cleanup().await
}
#[tokio::test]
async fn mint_claim_wins_and_refund_authorization_cannot_follow() -> Result<()> {
    let db = TestDb::new().await?;
    let (mut s, d, dest) = ready(&db).await?;
    let a = s.bridge_accounting(&accounting(2, 2, 0)).await?;
    let claimed = s.claim_bridge_mint(d.deposit_id, d.version, a.id).await?;
    assert!(
        s.authorize_bridge_refund(d.deposit_id, claimed.version, "alice", &dest)
            .await
            .is_err()
    );
    db.cleanup().await
}
#[tokio::test]
async fn payment_requires_confirmed_void_and_retains_exact_bytes_after_restart() -> Result<()> {
    let db = TestDb::new().await?;
    let (mut s, d, dest) = ready(&db).await?;
    let r = s
        .authorize_bridge_refund(d.deposit_id, d.version, "alice", &dest)
        .await?;
    let a = s.bridge_accounting(&accounting(2, 2, 0)).await?;
    assert!(
        s.reserve_bridge_refund(d.deposit_id, r.version, a.id, input())
            .await
            .is_err()
    );
    assert!(s.get_attempt(&r.operation_id).await?.is_none());
    voided(&mut s, &r).await?;
    let a = s.bridge_accounting(&accounting(2, 4, 0)).await?;
    let r = s
        .reserve_bridge_refund(d.deposit_id, r.version, a.id, input())
        .await?;
    s.save_prepared(&r.operation_id, &prepared()).await?;
    let old = prepared().bytes;
    drop(s);
    let mut s = db.connect().await?;
    assert_eq!(
        s.get_attempt(&r.operation_id)
            .await?
            .unwrap()
            .prepared
            .unwrap()
            .bytes,
        old
    );
    s.save_prepared(&r.operation_id, &prepared()).await?;
    let mut changed = prepared();
    changed.bytes.push(1);
    assert!(s.save_prepared(&r.operation_id, &changed).await.is_err());
    assert!(
        s.reserve_bridge_refund(d.deposit_id, r.version, a.id, input())
            .await
            .is_err()
    );
    db.cleanup().await
}
#[tokio::test]
async fn payment_settles_only_after_three_confirmations_with_spend_change_and_fee() -> Result<()> {
    let db = TestDb::new().await?;
    let (mut s, r) = reserved(&db).await?;
    s.save_prepared(&r.operation_id, &prepared()).await?;
    let mut change = note(80, 140_000, true);
    change.mined_height = 5;
    s.commit_scan(&ScanUpdate {
        checkpoint: cp(5),
        received: vec![change],
        witnesses: vec![(
            NoteId {
                txid: [10; 32],
                action_index: 0,
            },
            vec![1],
        )],
        spent_nullifiers: vec![[11; 32]],
        txids: vec![[80; 32]],
    })
    .await?;
    for height in 5..=7 {
        if height > 5 {
            scan(&mut s, height, vec![], vec![]).await?;
        }
        let pair = s.bridge_checkpoint_pair().await?;
        assert_eq!(
            s.settle_bridge_refund(r.deposit_id, pair).await?,
            if height == 7 {
                Some(B256::repeat_byte(80))
            } else {
                None
            }
        );
    }
    let a = s.bridge_accounting(&accounting(5, 4, 0)).await?;
    assert_eq!(a.deposit_liability, 0);
    assert_eq!(s.bridge_refunds().await?.remove(0).state, RefundState::Paid);
    db.cleanup().await
}
#[tokio::test]
async fn acted_void_and_source_reorganizations_halt_and_keep_reservation() -> Result<()> {
    for source in [false, true] {
        let db = TestDb::new().await?;
        let (mut s, r) = reserved(&db).await?;
        s.save_prepared(&r.operation_id, &prepared()).await?;
        if source {
            let _ = s.rewind(1, cp(1).hash).await;
        } else {
            let _ = s.rewind_zama(reference(5, 0), reference(3, 0)).await;
        }
        assert!(s.bridge_checkpoint_pair().await.is_err());
        assert!(
            s.get_attempt(&r.operation_id)
                .await?
                .unwrap()
                .prepared
                .is_some()
        );
        assert_eq!(s.reserved_inputs().await?.len(), 1);
        db.cleanup().await?;
    }
    Ok(())
}

#[tokio::test]
async fn concurrent_mint_and_refund_share_one_admission_lock() -> Result<()> {
    let db = TestDb::new().await?;
    let (mut s, d, dest) = ready(&db).await?;
    let a = s.bridge_accounting(&accounting(2, 2, 0)).await?;
    let mut other = db.connect().await?;
    let (mint, refund) = tokio::join!(
        s.claim_bridge_mint(d.deposit_id, d.version, a.id),
        other.authorize_bridge_refund(d.deposit_id, d.version, "alice", &dest)
    );
    assert_ne!(mint.is_ok(), refund.is_ok());
    let current = s.bridge_deposit(d.deposit_id).await?.unwrap();
    assert_eq!(current.claimed, mint.is_ok());
    assert_eq!(s.bridge_refunds().await?.len(), usize::from(refund.is_ok()));
    db.cleanup().await
}

#[tokio::test]
async fn one_confirmation_void_cannot_authorize_refund_payment() -> Result<()> {
    let db = TestDb::new().await?;
    let (mut s, d, dest) = ready(&db).await?;
    let r = s
        .authorize_bridge_refund(d.deposit_id, d.version, "alice", &dest)
        .await?;
    let signer = operator();
    s.initialize_zama_lane(signer.address(), 0, 0).await?;
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
    s.save_zama_signed(&r.void_operation_id, &signed).await?;
    let event = EventObservation {
        block: reference(4, 0),
        transaction_hash: signed.hash(),
        log_index: 0,
        event: Event::DepositVoided {
            deposit_id: d.deposit_id,
        },
    };
    let mut b = batch(s.zama_cursor().await?.unwrap().tip, 4, 0);
    b.chain_id = 11155111;
    b.events.push(event.clone());
    b.receipts.push(ReceiptObservation {
        transaction_hash: signed.hash(),
        block: event.block,
        from: signer.address(),
        to: contract(),
        success: true,
    });
    s.apply_zama_scan(&b).await?;
    let d = s.bridge_deposit(d.deposit_id).await?.unwrap();
    assert!(
        s.reconcile_bridge_deposit(d.deposit_id, d.version, &evidence(event.clone(), None))
            .await
            .is_err()
    );
    let a = s.bridge_accounting(&accounting(2, 3, 0)).await?;
    assert!(
        s.reserve_bridge_refund(d.deposit_id, r.version, a.id, input())
            .await
            .is_err()
    );
    evm(&mut s, 5, vec![]).await?;
    s.reconcile_bridge_deposit(d.deposit_id, d.version, &evidence(event, None))
        .await?;
    let a = s.bridge_accounting(&accounting(2, 4, 0)).await?;
    assert_eq!(
        s.reserve_bridge_refund(d.deposit_id, r.version, a.id, input())
            .await?
            .state,
        RefundState::Reserved
    );
    db.cleanup().await
}
