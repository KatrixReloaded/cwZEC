//! Real PostgreSQL, controlled chain events. Prepared bytes are journal fixtures,
//! not proof-bearing transactions or live-chain acceptance evidence.
use super::{
    Store,
    bridge_tests::{accounting, cp, evm, note, scan, setup},
    zama_test_support::{TestDb, batch, contract, reference},
};
use crate::core::redeem_types::*;
use alloy::{
    primitives::{Address, B256, Bytes, U256},
    signers::local::PrivateKeySigner,
};
use anyhow::Result;
use zama::{
    contract::{DeploymentEvidence, TransactionDomain, TransactionFees},
    watcher::{Event, EventObservation, ReceiptObservation},
};
use zcash::core::{LocalSigner, NoteId, PreparedTransaction, ScanUpdate};
const ACTUAL: u64 = 60_000;
const INPUT: u64 = 200_000;
fn input() -> NoteId {
    NoteId {
        txid: [10; 32],
        action_index: 0,
    }
}
fn burn(id: u64, height: u64) -> EventObservation {
    EventObservation {
        block: reference(height, 0),
        transaction_hash: B256::repeat_byte(40 + id as u8),
        log_index: 0,
        event: Event::RedeemRequested {
            redeem_id: U256::from(id),
            user: Address::repeat_byte(7),
            amount: B256::repeat_byte(44),
            dest_ct: Bytes::from_static(&[1, 2]),
        },
    }
}
fn destination() -> Result<String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "cwzec-roundtrip-address-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let signer = LocalSigner::create(&path)?;
    let address = signer.addresses()?.0;
    std::fs::remove_file(path)?;
    Ok(address)
}
async fn ready(db: &TestDb, backing: u64) -> Result<(Store, BridgeRedeem)> {
    let mut s = setup(db).await?;
    scan(
        &mut s,
        2,
        vec![note(10, backing, true), note(11, 5_000, false)],
        vec![],
    )
    .await?;
    scan(&mut s, 3, vec![], vec![]).await?;
    scan(&mut s, 4, vec![], vec![]).await?;
    s.ingest_bridge_deposits().await?;
    let event = burn(1, 2);
    evm(&mut s, 3, vec![event.clone()]).await?;
    let pair = s.bridge_checkpoint_pair().await?;
    let r = s
        .record_bridge_redeem(&event, pair, ACTUAL, Some(&destination()?))
        .await?;
    Ok((s, r))
}
async fn reserve(s: &mut Store, r: &BridgeRedeem) -> Result<BridgeRedeem> {
    let a = s.bridge_accounting(&accounting(2, 2, 40_000)).await?;
    assert!(a.coverage_complete);
    assert_eq!(a.deposit_liability, 5_000);
    assert_eq!(a.unpaid_redemptions, u128::from(ACTUAL));
    s.reserve_bridge_redeem(r.id, r.version, a.id, input())
        .await
}
fn prepared() -> PreparedTransaction {
    PreparedTransaction {
        bytes: b"controlled signed payout fixture".to_vec(),
        txid: [80; 32],
        inputs: vec![input()],
        expiry_height: 20,
        fee: REDEEM_FEE_ZAT,
    }
}
async fn confirm_payment(s: &mut Store, id: U256) -> Result<()> {
    let mut change = note(80, INPUT - ACTUAL, true);
    change.mined_height = 5;
    let witnesses = s
        .deposits()
        .await?
        .into_iter()
        .filter(|n| n.id != input() && n.spent_height.is_none())
        .map(|n| (n.id, vec![1]))
        .collect();
    s.commit_scan(&ScanUpdate {
        checkpoint: cp(5),
        received: vec![change],
        witnesses,
        spent_nullifiers: vec![[10; 32]],
        txids: vec![[80; 32]],
    })
    .await?;
    for height in 5..=7 {
        if height > 5 {
            scan(s, height, vec![], vec![]).await?;
        }
        let pair = s.bridge_checkpoint_pair().await?;
        assert_eq!(
            s.settle_bridge_payment(id, pair).await?,
            if height == 7 {
                Some(B256::repeat_byte(80))
            } else {
                None
            }
        );
    }
    Ok(())
}
#[tokio::test]
async fn roundtrip_reserve_requires_capacity_and_cas_blocks_duplicate_payment() -> Result<()> {
    let db = TestDb::new().await?;
    let (mut s, r) = ready(&db, 105_000).await?;
    let a = s.bridge_accounting(&accounting(2, 2, 40_000)).await?;
    assert_eq!(a.fee_reserve, Some(5_000));
    assert!(
        s.reserve_bridge_redeem(r.id, r.version, a.id, input())
            .await
            .is_err()
    );
    assert!(s.get_attempt(&r.operation_id).await?.is_none());
    assert!(
        !s.client
            .query_one(
                "SELECT acted_on FROM bridge_checkpoints WHERE id=$1",
                &[&a.id]
            )
            .await?
            .get::<_, bool>(0)
    );
    db.cleanup().await?;
    let db = TestDb::new().await?;
    let (mut s, r) = ready(&db, INPUT).await?;
    assert_eq!(reserve(&mut s, &r).await?.state, RedeemState::Reserved);
    let mut other = db.connect().await?;
    assert!(reserve(&mut other, &r).await.is_err());
    assert_eq!(s.reserved_inputs().await?.len(), 1);
    assert_eq!(
        s.client
            .query_one("SELECT count(*) FROM operations", &[])
            .await?
            .get::<_, i64>(0),
        1
    );
    db.cleanup().await
}
#[tokio::test]
async fn roundtrip_signed_payout_restart_and_ambiguous_broadcast_keep_same_journal() -> Result<()> {
    let db = TestDb::new().await?;
    let (mut s, r) = ready(&db, INPUT).await?;
    reserve(&mut s, &r).await?;
    s.save_prepared(&r.operation_id, &prepared()).await?;
    for ambiguous in [false, true] {
        drop(s);
        s = db.connect().await?;
        let a = s.get_attempt(&r.operation_id).await?.unwrap();
        assert_eq!(a.broadcast, ambiguous);
        let p = a.prepared.unwrap();
        assert_eq!(p.bytes, prepared().bytes);
        assert_eq!(p.txid, prepared().txid);
        assert_eq!(p.expiry_height, 20);
        assert_eq!(s.reserved_inputs().await?.len(), 1);
        s.save_prepared(&r.operation_id, &prepared()).await?;
        let mut replacement = prepared();
        replacement.bytes.push(1);
        assert!(
            s.save_prepared(&r.operation_id, &replacement)
                .await
                .is_err()
        );
        if !ambiguous {
            s.mark_broadcast(&r.operation_id).await?;
        }
    }
    db.cleanup().await
}
#[tokio::test]
async fn roundtrip_payment_three_confirmations_accounts_spend_change_and_unpaid_burn() -> Result<()>
{
    let db = TestDb::new().await?;
    let (mut s, r) = ready(&db, INPUT).await?;
    reserve(&mut s, &r).await?;
    s.save_prepared(&r.operation_id, &prepared()).await?;
    confirm_payment(&mut s, r.id).await?;
    let old = s.bridge_accounting(&accounting(2, 2, 40_000)).await?;
    let paid = s.bridge_accounting(&accounting(5, 2, 40_000)).await?;
    assert!(old.coverage_complete && paid.coverage_complete);
    assert_eq!(old.assets, 205_000);
    assert_eq!(old.unpaid_redemptions, 60_000);
    assert_eq!(paid.assets, 145_000);
    assert_eq!(paid.unpaid_redemptions, 0);
    assert_eq!(paid.deposit_liability, 5_000);
    assert_eq!(old.fee_reserve, paid.fee_reserve);
    drop(s);
    let mut s = db.connect().await?;
    assert_eq!(s.bridge_redeems().await?[0].state, RedeemState::Paid);
    assert_eq!(
        s.get_attempt(&r.operation_id)
            .await?
            .unwrap()
            .prepared
            .unwrap()
            .bytes,
        prepared().bytes
    );
    let pair = s.bridge_checkpoint_pair().await?;
    assert_eq!(
        s.settle_bridge_payment(r.id, pair).await?,
        Some(B256::repeat_byte(80))
    );
    db.cleanup().await
}
#[tokio::test]
async fn roundtrip_fulfillment_requires_real_payment_and_two_evm_confirmations() -> Result<()> {
    let db = TestDb::new().await?;
    let (mut s, r) = ready(&db, INPUT).await?;
    reserve(&mut s, &r).await?;
    let signer: PrivateKeySigner =
        "0000000000000000000000000000000000000000000000000000000000000001".parse()?;
    let domain = TransactionDomain::new(11155111, contract())?;
    s.initialize_zama_lane(signer.address(), 0, 0).await?;
    let sign = |txid| {
        domain.sign_operator(
            &signer,
            &domain.fulfill(r.id, txid),
            TransactionFees {
                nonce: 0,
                gas_limit: 100_000,
                max_fee_per_gas: 2,
                max_priority_fee_per_gas: 1,
            },
        )
    };
    assert!(
        s.save_zama_signed(&r.fulfillment_id(), &sign(B256::repeat_byte(80))?)
            .await
            .is_err()
    );
    s.save_prepared(&r.operation_id, &prepared()).await?;
    confirm_payment(&mut s, r.id).await?;
    assert!(
        s.save_zama_signed(&r.fulfillment_id(), &sign(B256::repeat_byte(81))?)
            .await
            .is_err()
    );
    let signed = sign(B256::repeat_byte(80))?;
    s.save_zama_signed(&r.fulfillment_id(), &signed).await?;
    drop(s);
    let mut s = db.connect().await?;
    assert_eq!(
        s.zama_transaction(&r.fulfillment_id())
            .await?
            .unwrap()
            .signed
            .raw(),
        signed.raw()
    );
    let e = EventObservation {
        block: reference(4, 0),
        transaction_hash: signed.hash(),
        log_index: 0,
        event: Event::RedeemFulfilled {
            redeem_id: r.id,
            zcash_txid: B256::repeat_byte(80),
        },
    };
    evm(&mut s, 4, vec![e.clone()]).await?;
    let pair = s.bridge_checkpoint_pair().await?;
    assert!(
        s.reconcile_bridge_fulfillment(r.id, &e, pair)
            .await
            .is_err()
    );
    evm(&mut s, 5, vec![]).await?;
    let pair = s.bridge_checkpoint_pair().await?;
    s.reconcile_bridge_fulfillment(r.id, &e, pair).await?;
    assert_eq!(s.bridge_redeems().await?[0].state, RedeemState::Fulfilled);
    assert!(
        s.bridge_accounting(&accounting(5, 4, 40_000))
            .await?
            .coverage_complete
    );
    db.cleanup().await
}
#[tokio::test]
async fn roundtrip_acted_burn_and_paid_reorgs_halt_without_releasing_journal() -> Result<()> {
    for paid in [false, true] {
        let db = TestDb::new().await?;
        let (mut s, r) = ready(&db, INPUT).await?;
        reserve(&mut s, &r).await?;
        s.save_prepared(&r.operation_id, &prepared()).await?;
        if paid {
            confirm_payment(&mut s, r.id).await?;
            assert!(s.rewind(4, cp(4).hash).await.is_err());
        } else {
            assert!(
                s.rewind_zama(reference(3, 0), reference(1, 0))
                    .await
                    .is_err()
            );
        }
        assert!(
            s.client
                .query_one("SELECT halted FROM bridge_domain", &[])
                .await?
                .get::<_, bool>(0)
        );
        drop(s);
        let mut s = db.connect().await?;
        assert_eq!(s.reserved_inputs().await?.len(), 1);
        assert_eq!(
            s.get_attempt(&r.operation_id)
                .await?
                .unwrap()
                .prepared
                .unwrap()
                .bytes,
            prepared().bytes
        );
        assert!(s.bridge_checkpoint_pair().await.is_err());
        db.cleanup().await?;
    }
    Ok(())
}
#[tokio::test]
async fn roundtrip_pristine_origin_and_rejected_burn_liabilities_require_evidence() -> Result<()> {
    let db = TestDb::new().await?;
    let mut s = setup(&db).await?;
    scan(&mut s, 2, vec![note(10, INPUT, true)], vec![]).await?;
    scan(&mut s, 3, vec![], vec![]).await?;
    scan(&mut s, 4, vec![], vec![]).await?;
    evm(&mut s, 3, vec![]).await?;
    let mut a = accounting(2, 2, 0);
    a.supply_handle = B256::ZERO;
    assert!(s.bridge_accounting(&a).await.is_err());
    // Controlled deployment metadata only; this store test does not verify RPC evidence.
    let origin = DeploymentEvidence {
        block: reference(2, 0),
        transaction: B256::repeat_byte(99),
        runtime_hash: B256::repeat_byte(98),
        service: Address::repeat_byte(9),
        auditor: Address::repeat_byte(8),
    };
    s.record_bridge_origin(&origin).await?;
    assert!(s.bridge_accounting(&a).await?.coverage_complete);
    a.supply = 1;
    assert!(s.bridge_accounting(&a).await.is_err());
    let rejected = burn(1, 4);
    let zero = burn(2, 4);
    evm(&mut s, 5, vec![rejected.clone(), zero.clone()]).await?;
    let pair = s.bridge_checkpoint_pair().await?;
    let r = s
        .record_bridge_redeem(&rejected, pair, ACTUAL, None)
        .await?;
    let z = s.record_bridge_redeem(&zero, pair, 0, None).await?;
    assert_eq!(r.state, RedeemState::Rejected);
    assert_eq!(z.state, RedeemState::Zero);
    a.checkpoint = reference(4, 0);
    a.supply = 0;
    assert!(s.bridge_accounting(&a).await.is_err());
    let report = s.bridge_accounting(&accounting(2, 4, 0)).await?;
    assert!(report.coverage_complete);
    assert_eq!(report.unpaid_redemptions, u128::from(ACTUAL));
    assert!(
        s.reserve_bridge_redeem(r.id, r.version, report.id, input())
            .await
            .is_err()
    );
    assert!(
        s.reserve_bridge_redeem(z.id, z.version, report.id, input())
            .await
            .is_err()
    );
    assert!(s.get_attempt(&r.operation_id).await?.is_none());
    db.cleanup().await
}

#[tokio::test]
async fn roundtrip_unacted_orphan_burn_id_reuse_rebinds_only_before_reservation() -> Result<()> {
    let db = TestDb::new().await?;
    let (mut s, old) = ready(&db, INPUT).await?;
    s.rewind_zama(reference(3, 0), reference(1, 0)).await?;
    let orphan = s.bridge_redeems().await?.remove(0);
    assert!(!orphan.canonical);
    assert!(orphan.version > old.version);
    let mut event = burn(1, 2);
    event.block = reference(2, 1);
    event.transaction_hash = B256::repeat_byte(90);
    event.event = Event::RedeemRequested {
        redeem_id: old.id,
        user: Address::repeat_byte(8),
        amount: B256::repeat_byte(45),
        dest_ct: Bytes::from_static(&[3, 4]),
    };
    let mut b = batch(reference(1, 0), 3, 1);
    b.chain_id = 11155111;
    b.events = vec![event.clone()];
    b.receipts = vec![ReceiptObservation {
        transaction_hash: event.transaction_hash,
        block: event.block,
        from: Address::repeat_byte(9),
        to: contract(),
        success: true,
    }];
    s.apply_zama_scan(&b).await?;
    let pair = s.bridge_checkpoint_pair().await?;
    let dest = destination()?;
    let fresh = s
        .record_bridge_redeem(&event, pair, 70_000, Some(&dest))
        .await?;
    assert_eq!(fresh.id, old.id);
    assert_eq!(fresh.operation_id, old.operation_id);
    assert_eq!(fresh.burner, Address::repeat_byte(8));
    assert_eq!(fresh.handle, B256::repeat_byte(45));
    assert_eq!(fresh.actual, 70_000);
    assert_eq!(fresh.source, reference(2, 1));
    assert!(fresh.canonical && fresh.version > orphan.version);
    let mut evidence = accounting(2, 2, 30_000);
    evidence.checkpoint = reference(2, 1);
    let report = s.bridge_accounting(&evidence).await?;
    assert!(report.coverage_complete);
    s.reserve_bridge_redeem(fresh.id, fresh.version, report.id, input())
        .await?;
    assert!(
        s.record_bridge_redeem(&event, pair, 71_000, Some(&dest))
            .await
            .is_err()
    );
    assert!(
        s.rewind_zama(reference(3, 1), reference(1, 0))
            .await
            .is_err()
    );
    let retained = s.bridge_redeems().await?.remove(0);
    assert_eq!(retained.state, RedeemState::Reserved);
    assert_eq!(retained.actual, 70_000);
    assert_eq!(s.reserved_inputs().await?.len(), 1);
    assert!(
        s.client
            .query_one("SELECT halted FROM bridge_domain", &[])
            .await?
            .get::<_, bool>(0)
    );
    db.cleanup().await
}

#[tokio::test]
async fn roundtrip_expiry_without_absence_proof_retains_liability_and_bytes() -> Result<()> {
    let db = TestDb::new().await?;
    let (mut s, r) = ready(&db, INPUT).await?;
    reserve(&mut s, &r).await?;
    s.save_prepared(&r.operation_id, &prepared()).await?;
    for height in 5..=23 {
        scan(&mut s, height, vec![], vec![]).await?;
    }
    let pair = s.bridge_checkpoint_pair().await?;
    assert_eq!(s.settle_bridge_payment(r.id, pair).await?, None);
    let report = s.bridge_accounting(&accounting(21, 2, 40_000)).await?;
    assert_eq!(report.unpaid_redemptions, ACTUAL as u128);
    assert!(report.coverage_complete);
    drop(s);
    let mut s = db.connect().await?;
    assert_eq!(s.bridge_redeems().await?[0].state, RedeemState::Reserved);
    assert_eq!(s.reserved_inputs().await?.len(), 1);
    assert_eq!(
        s.get_attempt(&r.operation_id)
            .await?
            .unwrap()
            .prepared
            .unwrap()
            .bytes,
        prepared().bytes
    );
    assert!(
        s.reserve_bridge_redeem(r.id, r.version, report.id, input())
            .await
            .is_err()
    );
    db.cleanup().await
}

#[tokio::test]
async fn roundtrip_clean_origin_mint_waits_for_separate_operator_reserve() -> Result<()> {
    let db = TestDb::new().await?;
    let (mut s, d) = super::bridge_tests::funded(&db).await?;
    s.record_bridge_origin(&DeploymentEvidence {
        block: reference(2, 0),
        transaction: B256::repeat_byte(99),
        runtime_hash: B256::repeat_byte(98),
        service: Address::repeat_byte(9),
        auditor: Address::repeat_byte(8),
    })
    .await?;
    let initial = s.bridge_accounting(&accounting(2, 2, 0)).await?;
    assert_eq!(initial.fee_reserve, Some(0));
    assert!(
        s.claim_bridge_mint(d.deposit_id, d.version, initial.id)
            .await
            .is_err()
    );
    let mut funding = note(12, 10_000, true);
    funding.mined_height = 5;
    scan(&mut s, 5, vec![funding], vec![]).await?;
    scan(&mut s, 6, vec![], vec![]).await?;
    scan(&mut s, 7, vec![], vec![]).await?;
    assert_eq!(s.ingest_bridge_deposits().await?.len(), 1);
    let backed = s.bridge_accounting(&accounting(5, 2, 0)).await?;
    assert_eq!(backed.fee_reserve, Some(10_000));
    assert!(
        s.claim_bridge_mint(d.deposit_id, d.version, backed.id)
            .await?
            .claimed
    );
    assert!(
        s.reserve("legacy-bypass", "unused", 1, &[d.note_id], 3)
            .await
            .is_err()
    );
    db.cleanup().await
}

async fn fragmented_ready(db: &TestDb, burns: u8) -> Result<(Store, Vec<BridgeRedeem>)> {
    let mut s = setup(db).await?;
    let mut notes: Vec<_> = (20..20 + 4 * burns)
        .map(|i| note(i, 30_000, true))
        .collect();
    notes.push(note(11, 5_000, false));
    scan(&mut s, 2, notes, vec![]).await?;
    scan(&mut s, 3, vec![], vec![]).await?;
    scan(&mut s, 4, vec![], vec![]).await?;
    s.ingest_bridge_deposits().await?;
    let events: Vec<_> = (1..=burns).map(|i| burn(i.into(), 2)).collect();
    evm(&mut s, 3, events.clone()).await?;
    let pair = s.bridge_checkpoint_pair().await?;
    let mut redeems = vec![];
    for event in events {
        redeems.push(
            s.record_bridge_redeem(&event, pair, 100_000, Some(&destination()?))
                .await?,
        );
    }
    Ok((s, redeems))
}
fn fragmented_inputs(start: u8) -> Vec<NoteId> {
    (start..start + 4)
        .map(|i| NoteId {
            txid: [i; 32],
            action_index: 0,
        })
        .collect()
}

#[tokio::test]
async fn roundtrip_four_inputs_reserve_restart_settle_and_charge_operator_extra_fee() -> Result<()>
{
    let db = TestDb::new().await?;
    let (mut s, redeems) = fragmented_ready(&db, 1).await?;
    let r = &redeems[0];
    let inputs = fragmented_inputs(20);
    let a = s.bridge_accounting(&accounting(2, 2, 5_000)).await?;
    assert_eq!(a.fee_reserve, Some(15_000));
    assert!(
        s.reserve_bridge_redeem_inputs(r.id, r.version, a.id, &inputs)
            .await
            .is_err()
    );
    assert!(s.get_attempt(&r.operation_id).await?.is_none());
    assert!(s.reserved_inputs().await?.is_empty());
    let a = s.bridge_accounting(&accounting(2, 2, 0)).await?;
    assert_eq!(a.fee_reserve, Some(20_000));
    let mut duplicate = inputs.clone();
    duplicate[3] = duplicate[0];
    assert!(
        s.reserve_bridge_redeem_inputs(r.id, r.version, a.id, &duplicate)
            .await
            .is_err()
    );
    s.reserve_bridge_redeem_inputs(r.id, r.version, a.id, &inputs)
        .await?;
    drop(s);
    let mut s = db.connect().await?;
    let unsigned = s.get_attempt(&r.operation_id).await?.unwrap();
    assert!(unsigned.prepared.is_none());
    assert!(unsigned.inputs == inputs);
    assert_eq!(unsigned.amount, 90_000); // User's original 10,000-zat quote is unchanged.
    assert_eq!(s.reserved_inputs().await?.len(), 4);
    s.check_payment_accounting().await?;
    let p = PreparedTransaction {
        inputs: inputs.clone(),
        fee: 20_000,
        ..prepared()
    };
    let wrong = PreparedTransaction {
        inputs: inputs.clone(),
        ..prepared()
    };
    assert!(s.save_prepared(&r.operation_id, &wrong).await.is_err());
    s.save_prepared(&r.operation_id, &p).await?;
    drop(s);
    let mut s = db.connect().await?;
    let restored = s
        .get_attempt(&r.operation_id)
        .await?
        .unwrap()
        .prepared
        .unwrap();
    assert!(restored.inputs == inputs);
    assert_eq!(restored.bytes, p.bytes);
    assert_eq!(restored.fee, 20_000);
    s.save_prepared(&r.operation_id, &p).await?;
    let mut change = note(80, 10_000, true);
    change.mined_height = 5;
    s.commit_scan(&ScanUpdate {
        checkpoint: cp(5),
        received: vec![change],
        witnesses: vec![(note(11, 5_000, false).id, vec![1])],
        spent_nullifiers: (20..24).map(|i| [i; 32]).collect(),
        txids: vec![[80; 32]],
    })
    .await?;
    scan(&mut s, 6, vec![], vec![]).await?;
    scan(&mut s, 7, vec![], vec![]).await?;
    let pair = s.bridge_checkpoint_pair().await?;
    // Every reserved input must have been spent in the payment's inclusion block.
    s.client
        .execute(
            "UPDATE notes SET spent_height=NULL WHERE txid=$1",
            &[&&[23u8; 32][..]],
        )
        .await?;
    assert!(s.settle_bridge_payment(r.id, pair).await.is_err());
    s.client
        .execute(
            "UPDATE notes SET spent_height=5 WHERE txid=$1",
            &[&&[23u8; 32][..]],
        )
        .await?;
    assert_eq!(
        s.settle_bridge_payment(r.id, pair).await?,
        Some(B256::repeat_byte(80))
    );
    assert_eq!(
        s.settle_bridge_payment(r.id, pair).await?,
        Some(B256::repeat_byte(80))
    );
    let after = s.bridge_accounting(&accounting(5, 2, 0)).await?;
    assert!(after.coverage_complete);
    assert_eq!(after.unpaid_redemptions, 0);
    assert_eq!(after.deposit_liability, 5_000);
    assert_eq!(after.assets, 15_000);
    assert_eq!(after.fee_reserve, Some(10_000));
    db.cleanup().await
}

#[tokio::test]
async fn roundtrip_pending_unsigned_inputs_reserve_extra_fee_capacity() -> Result<()> {
    let db = TestDb::new().await?;
    let (mut s, redeems) = fragmented_ready(&db, 2).await?;
    let a = s.bridge_accounting(&accounting(2, 2, 20_000)).await?;
    assert_eq!(a.fee_reserve, Some(20_000));
    s.reserve_bridge_redeem_inputs(
        redeems[0].id,
        redeems[0].version,
        a.id,
        &fragmented_inputs(20),
    )
    .await?;
    drop(s);
    let mut s = db.connect().await?;
    assert!(
        s.reserve_bridge_redeem_inputs(
            redeems[1].id,
            redeems[1].version,
            a.id,
            &fragmented_inputs(24)
        )
        .await
        .is_err()
    );
    assert!(s.get_attempt(&redeems[1].operation_id).await?.is_none());
    assert_eq!(s.reserved_inputs().await?.len(), 4);
    s.check_payment_accounting().await?;
    let lower = s.bridge_accounting(&accounting(2, 2, 25_000)).await?;
    assert_eq!(lower.fee_reserve, Some(15_000));
    assert!(s.check_payment_accounting().await.is_err());
    db.cleanup().await
}
