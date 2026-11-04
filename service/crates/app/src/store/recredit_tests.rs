//! Real PostgreSQL with controlled EVM facts; no live transactions are submitted.
use super::{
    bridge_tests::{accounting, evm, funded, note, scan, setup},
    zama_test_support::{TestDb, batch, contract, reference},
};
use crate::core::redeem_types::*;
use alloy::{
    primitives::{Address, B256, Bytes, U256},
    signers::local::PrivateKeySigner,
};
use anyhow::Result;
use zama::{
    contract::{TransactionDomain, TransactionFees},
    watcher::{Event, EventObservation, ReceiptObservation},
};

fn burn(id: u64) -> EventObservation {
    EventObservation {
        block: reference(4, 0),
        transaction_hash: B256::repeat_byte(id as u8),
        log_index: 0,
        event: Event::RedeemRequested {
            redeem_id: U256::from(id),
            user: Address::repeat_byte(7),
            amount: B256::repeat_byte(44),
            dest_ct: Bytes::from_static(&[1, 2]),
        },
    }
}
fn signer() -> PrivateKeySigner {
    "0000000000000000000000000000000000000000000000000000000000000001"
        .parse()
        .unwrap()
}

#[tokio::test]
async fn recredit_explicit_admission_duplicate_restart_and_pending_liability() -> Result<()> {
    let db = TestDb::new().await?;
    let mut s = setup(&db).await?;
    // The shared funded fixture backs only its 100-zatoshi deposit liability.
    // This scenario also needs assets backing its synthetic outstanding burn.
    scan(
        &mut s,
        2,
        vec![note(10, 100, false), note(11, 100_000, true)],
        vec![],
    )
    .await?;
    scan(&mut s, 3, vec![], vec![]).await?;
    scan(&mut s, 4, vec![], vec![]).await?;
    s.ingest_bridge_deposits().await?;
    let e = burn(71);
    evm(&mut s, 5, vec![e.clone()]).await?;
    let pair = s.bridge_checkpoint_pair().await?;
    let r = s.record_bridge_redeem(&e, pair, 75_000, None).await?;
    let before = s.bridge_accounting(&accounting(2, 4, 0)).await?;
    assert_eq!(before.unpaid_redemptions, 75_000);
    assert_eq!(before.assets, 100_100);
    assert_eq!(before.deposit_liability, 100);
    assert_eq!(before.fee_reserve, Some(25_000));
    assert!(before.coverage_complete);
    let pending = s.authorize_bridge_recredit(r.id, r.version).await?;
    assert_eq!(pending.state, RedeemState::RecreditPending);
    assert_eq!(pending.recredit_id(), "cwzec/recredit/71");
    assert_eq!(
        s.authorize_bridge_recredit(r.id, r.version).await?.version,
        pending.version
    );
    drop(s);
    let mut s = db.connect().await?;
    s.migrate().await?;
    let pending = s.bridge_redeems().await?.remove(0);
    assert_eq!(pending.state, RedeemState::RecreditPending);
    assert_eq!(pending.burner, Address::repeat_byte(7));
    let after = s.bridge_accounting(&accounting(2, 4, 0)).await?;
    assert_eq!(before.unpaid_redemptions, after.unpaid_redemptions);
    assert_eq!(before.fee_reserve, after.fee_reserve);
    assert!(after.coverage_complete);
    assert!(
        s.reserve_bridge_redeem(
            pending.id,
            pending.version,
            after.id,
            zcash::core::NoteId {
                txid: [10; 32],
                action_index: 0
            }
        )
        .await
        .is_err()
    );
    assert!(s.get_attempt(&pending.operation_id).await?.is_none());
    assert!(s.settle_bridge_payment(pending.id, pair).await?.is_none());
    db.cleanup().await
}

#[tokio::test]
async fn recredit_zero_and_any_payout_intent_block_admission() -> Result<()> {
    let db = TestDb::new().await?;
    let (mut s, _) = funded(&db).await?;
    let zero = burn(72);
    let rejected = burn(73);
    evm(&mut s, 5, vec![zero.clone(), rejected.clone()]).await?;
    let pair = s.bridge_checkpoint_pair().await?;
    let z = s.record_bridge_redeem(&zero, pair, 0, None).await?;
    assert!(s.authorize_bridge_recredit(z.id, z.version).await.is_err());
    let r = s
        .record_bridge_redeem(&rejected, pair, 75_000, None)
        .await?;
    // An unsigned operation is already unsafe: its signer can be running.
    s.client
        .execute(
            "INSERT INTO operations(id,destination,amount) VALUES($1,'controlled',65000)",
            &[&r.operation_id],
        )
        .await?;
    assert!(s.authorize_bridge_recredit(r.id, r.version).await.is_err());
    drop(s);
    let mut s = db.connect().await?;
    assert!(s.authorize_bridge_recredit(r.id, r.version).await.is_err());
    assert!(
        s.get_attempt(&r.operation_id)
            .await?
            .unwrap()
            .prepared
            .is_none()
    );
    assert_eq!(s.bridge_redeems().await?[1].state, RedeemState::Rejected);
    // Preserve a historical signed-but-never-broadcast attempt as well. Fixture
    // SQL models an older persisted journal; production guards prohibit creation
    // for this rejected row now, and authorization must never erase the bytes.
    s.client
        .execute(
            "UPDATE operations SET signed_bytes=$2,txid=$3,expiry_height=20,fee=10000 WHERE id=$1",
            &[
                &r.operation_id,
                &&b"legacy signed fixture"[..],
                &&[90u8; 32][..],
            ],
        )
        .await?;
    assert!(s.authorize_bridge_recredit(r.id, r.version).await.is_err());
    drop(s);
    let mut s = db.connect().await?;
    assert!(s.authorize_bridge_recredit(r.id, r.version).await.is_err());
    let journal = s.get_attempt(&r.operation_id).await?.unwrap();
    assert!(!journal.broadcast);
    assert_eq!(journal.prepared.unwrap().bytes, b"legacy signed fixture");

    db.cleanup().await
}

#[tokio::test]
async fn recredit_settlement_requires_own_confirmed_actual_and_original_burner() -> Result<()> {
    let db = TestDb::new().await?;
    let (mut s, _) = funded(&db).await?;
    let e = burn(74);
    evm(&mut s, 5, vec![e.clone()]).await?;
    let pair = s.bridge_checkpoint_pair().await?;
    let r = s.record_bridge_redeem(&e, pair, 75_000, None).await?;
    let operator = signer();
    s.initialize_zama_lane(operator.address(), 0, 0).await?;
    let domain = TransactionDomain::new(11155111, contract())?;
    let signed = domain.sign_operator(
        &operator,
        &domain.recredit(r.id),
        TransactionFees {
            nonce: 0,
            gas_limit: 100_000,
            max_fee_per_gas: 2,
            max_priority_fee_per_gas: 1,
        },
    )?;
    assert!(
        s.save_zama_signed(&r.recredit_id(), &signed).await.is_err(),
        "unsigned authorization must precede signing"
    );
    let pending = s.authorize_bridge_recredit(r.id, r.version).await?;
    s.save_zama_signed(&r.recredit_id(), &signed).await?;
    let original_bytes = signed.raw().clone();
    drop(s);
    let mut s = db.connect().await?;
    assert_eq!(
        s.zama_transaction(&r.recredit_id())
            .await?
            .unwrap()
            .signed
            .raw(),
        &original_bytes
    );
    s.save_zama_signed(&r.recredit_id(), &signed).await?;
    let restoration = EventObservation {
        block: reference(6, 0),
        transaction_hash: signed.hash(),
        log_index: 0,
        event: Event::RedeemRecredited {
            redeem_id: r.id,
            user: r.burner,
            amount: B256::repeat_byte(81),
        },
    };
    let mut b = batch(s.zama_cursor().await?.unwrap().tip, 6, 0);
    b.chain_id = 11155111;
    b.events.push(restoration.clone());
    b.receipts.push(ReceiptObservation {
        transaction_hash: signed.hash(),
        block: restoration.block,
        from: operator.address(),
        to: contract(),
        success: true,
    });
    s.apply_zama_scan(&b).await?;
    let pair = s.bridge_checkpoint_pair().await?;
    assert!(
        s.reconcile_bridge_recredit(r.id, &restoration, pair, 75_000)
            .await
            .is_err(),
        "one confirmation cannot settle"
    );
    evm(&mut s, 7, vec![]).await?;
    let pair = s.bridge_checkpoint_pair().await?;
    let mut wrong = restoration.clone();
    if let Event::RedeemRecredited { user, .. } = &mut wrong.event {
        *user = operator.address();
    }
    assert!(
        s.reconcile_bridge_recredit(r.id, &wrong, pair, 75_000)
            .await
            .is_err()
    );
    assert!(
        s.reconcile_bridge_recredit(r.id, &restoration, pair, 74_999)
            .await
            .is_err()
    );
    assert_eq!(s.bridge_redeems().await?[0].state, pending.state);
    s.reconcile_bridge_recredit(r.id, &restoration, pair, 75_000)
        .await?;
    let done = s.bridge_redeems().await?.remove(0);
    assert_eq!(done.state, RedeemState::Recredited);
    assert_eq!(
        s.authorize_bridge_recredit(r.id, r.version).await?.state,
        RedeemState::Recredited
    );

    s.reconcile_bridge_recredit(r.id, &restoration, pair, 75_000)
        .await?;
    assert_eq!(s.bridge_redeems().await?[0].version, done.version);
    assert!(s.get_attempt(&r.operation_id).await?.is_none());
    assert_eq!(
        s.bridge_accounting(&accounting(2, 6, 75_000))
            .await?
            .unpaid_redemptions,
        0
    );
    assert!(
        s.rewind_zama(reference(7, 0), reference(5, 0))
            .await
            .is_err(),
        "acted restoration reorg halts"
    );
    assert_eq!(
        s.zama_transaction(&r.recredit_id())
            .await?
            .unwrap()
            .signed
            .raw(),
        &original_bytes
    );
    db.cleanup().await
}

#[tokio::test]
async fn recredit_and_payout_reservation_concurrently_admit_only_eligible_action() -> Result<()> {
    use super::bridge_tests::{note, scan, setup};
    for ready in [true, false] {
        let db = TestDb::new().await?;
        let mut s = setup(&db).await?;
        scan(&mut s, 2, vec![note(10, 200_000, true)], vec![]).await?;
        scan(&mut s, 3, vec![], vec![]).await?;
        scan(&mut s, 4, vec![], vec![]).await?;
        s.ingest_bridge_deposits().await?;
        let e = burn(76);
        evm(&mut s, 5, vec![e.clone()]).await?;
        let destination_path = std::env::temp_dir().join(format!(
            "cwzec-recredit-race-{}-{ready}",
            std::process::id()
        ));
        let wallet = zcash::core::LocalSigner::create(&destination_path)?;
        let destination = wallet.addresses()?.0;
        std::fs::remove_file(destination_path)?;
        let pair = s.bridge_checkpoint_pair().await?;
        let r = s
            .record_bridge_redeem(&e, pair, 75_000, ready.then_some(destination.as_str()))
            .await?;
        let a = s.bridge_accounting(&accounting(2, 4, 0)).await?;
        assert_eq!(a.fee_reserve, Some(125_000));
        let mut other = db.connect().await?;
        let (payout, recredit) = tokio::join!(
            s.reserve_bridge_redeem(
                r.id,
                r.version,
                a.id,
                zcash::core::NoteId {
                    txid: [10; 32],
                    action_index: 0
                }
            ),
            other.authorize_bridge_recredit(r.id, r.version)
        );
        assert_eq!(payout.is_ok(), ready);
        assert_eq!(recredit.is_ok(), !ready);
        drop(s);
        drop(other);
        let mut restarted = db.connect().await?;
        let stored = restarted.bridge_redeems().await?.remove(0);
        assert_eq!(
            stored.state,
            if ready {
                RedeemState::Reserved
            } else {
                RedeemState::RecreditPending
            }
        );
        assert_eq!(
            restarted.get_attempt(&r.operation_id).await?.is_some(),
            ready
        );
        assert_eq!(restarted.reserved_inputs().await?.len(), usize::from(ready));
        assert!(
            restarted
                .authorize_bridge_recredit(r.id, r.version)
                .await
                .is_ok()
                != ready
        );
        db.cleanup().await?;
    }
    Ok(())
}
