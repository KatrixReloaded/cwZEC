//! Real database acceptance tests for the deposit-only bridge barrier.
use super::{
    Store,
    zama_test_support::{TestDb, batch, contract, reference},
};
use crate::core::{bridge_types::*, deposit_id, make_memo};
use alloy::{
    primitives::{Address, B256, Bytes},
    signers::local::PrivateKeySigner,
};
use anyhow::Result;
use zama::{
    contract::{DepositStatus, TransactionDomain, TransactionFees},
    watcher::{Event, EventObservation, ReceiptObservation},
};
use zcash::core::{Account, Checkpoint, NoteId, ScanUpdate, WalletNote};

pub(crate) fn note(n: u8, value: u64, internal: bool) -> WalletNote {
    let text = make_memo(&format!("{:#x}", Address::repeat_byte(7))).unwrap();
    let mut memo = [0; 512];
    memo[..text.len()].copy_from_slice(text.as_bytes());
    WalletNote {
        id: NoteId {
            txid: [n; 32],
            action_index: 0,
        },
        recipient: [1; 43],
        value,
        rho: [n; 32],
        rseed: [n + 1; 32],
        nullifier: [n; 32],
        position: n.into(),
        internal,
        memo,
        mined_height: 2,
        spent_height: None,
        witness: vec![1],
    }
}
pub(crate) fn cp(height: u32) -> Checkpoint {
    Checkpoint {
        height,
        hash: [height as u8; 32],
        parent: [height.saturating_sub(1) as u8; 32],
        frontier: vec![1],
    }
}
pub(crate) async fn setup(db: &TestDb) -> Result<Store> {
    let mut s = db.connect().await?;
    s.initialize(
        &Account {
            birthday: 2,
            fvk: [1; 96],
            use_qsk: false,
        },
        &cp(1),
    )
    .await?;
    s.initialize_zama(11155111, contract(), 2, reference(1, 0))
        .await?;
    s.initialize_bridge(BridgeDomain {
        chain_id: 11155111,
        contract: contract(),
    })
    .await?;
    Ok(s)
}
pub(crate) async fn scan(
    s: &mut Store,
    height: u32,
    received: Vec<WalletNote>,
    spent: Vec<[u8; 32]>,
) -> Result<()> {
    let witnesses = s
        .deposits()
        .await?
        .into_iter()
        .filter(|n| n.spent_height.is_none() && !spent.contains(&n.id.txid))
        .map(|n| (n.id, vec![1]))
        .collect();
    s.commit_scan(&ScanUpdate {
        checkpoint: cp(height),
        received,
        witnesses,
        spent_nullifiers: spent,
        txids: vec![],
    })
    .await
}
pub(crate) async fn evm(s: &mut Store, end: u64, events: Vec<EventObservation>) -> Result<()> {
    let mut b = batch(s.zama_cursor().await?.unwrap().tip, end, 0);
    b.chain_id = 11155111;
    b.receipts = events
        .iter()
        .map(|e| ReceiptObservation {
            transaction_hash: e.transaction_hash,
            block: e.block,
            from: Address::repeat_byte(9),
            to: contract(),
            success: true,
        })
        .collect();
    b.events = events;
    s.apply_zama_scan(&b).await
}
pub(crate) async fn funded(db: &TestDb) -> Result<(Store, BridgeDeposit)> {
    let mut s = setup(db).await?;
    scan(&mut s, 2, vec![note(10, 100, false)], vec![]).await?;
    scan(&mut s, 3, vec![], vec![]).await?;
    scan(&mut s, 4, vec![], vec![]).await?;
    evm(&mut s, 3, vec![]).await?;
    let d = s.ingest_bridge_deposits().await?.remove(0);
    Ok((s, d))
}
pub(crate) fn accounting(z: u32, e: u64, supply: u64) -> AccountingEvidence {
    AccountingEvidence {
        zcash_height: z,
        zcash_hash: cp(z).hash,
        checkpoint: reference(e, 0),
        supply_handle: B256::repeat_byte(8),
        supply,
    }
}
pub(crate) fn minted(d: &BridgeDeposit) -> EventObservation {
    EventObservation {
        block: reference(3, 0),
        transaction_hash: B256::repeat_byte(30),
        log_index: 0,
        event: Event::DepositMinted {
            deposit_id: d.deposit_id,
            to: d.recipient.unwrap(),
            amount: B256::repeat_byte(44),
        },
    }
}
pub(crate) fn evidence(e: EventObservation, actual: Option<u64>) -> ConsumptionEvidence {
    ConsumptionEvidence {
        checkpoint: e.block,
        status: if actual.is_some() {
            DepositStatus::Minted
        } else {
            DepositStatus::Refunded
        },
        actual_handle: actual.map(|_| B256::repeat_byte(44)),
        actual_amount: actual,
        event: Some(e),
    }
}
async fn halted(s: &Store) -> Result<bool> {
    Ok(s.client
        .query_one("SELECT halted FROM bridge_domain", &[])
        .await?
        .get(0))
}

#[tokio::test]
async fn bridge_ingestion_confirmation_identity_restart_and_refund_liabilities() -> Result<()> {
    let db = TestDb::new().await?;
    let mut s = setup(&db).await?;
    let mut bad = note(11, 20, false);
    bad.memo = [0; 512];
    let mut zero = note(12, 30, false);
    let text = make_memo(&format!("{:#x}", Address::ZERO))?;
    zero.memo = [0; 512];
    zero.memo[..text.len()].copy_from_slice(text.as_bytes());
    scan(
        &mut s,
        2,
        vec![
            note(10, 100, false),
            bad,
            zero,
            note(13, 40, true),
            note(14, 0, false),
        ],
        vec![],
    )
    .await?;
    assert!(s.ingest_bridge_deposits().await?.is_empty());
    scan(&mut s, 3, vec![], vec![]).await?;
    assert!(s.ingest_bridge_deposits().await?.is_empty());
    scan(&mut s, 4, vec![], vec![]).await?;
    evm(&mut s, 3, vec![]).await?;
    let ds = s.ingest_bridge_deposits().await?;
    assert_eq!(ds.len(), 4);
    let d = ds.iter().find(|d| d.amount == 100).unwrap();
    let id = d.deposit_id;
    let op = d.operation_id.clone();
    assert_eq!(id, B256::from(deposit_id(&[10; 32], &[11; 32])));
    assert_eq!(d.recipient, Some(Address::repeat_byte(7)));
    assert_eq!(d.state, DepositState::Eligible);
    assert_eq!(
        ds.iter()
            .filter(|d| d.state == DepositState::RefundPending)
            .count(),
        3
    );
    drop(s);
    let mut s = db.connect().await?;
    let ds = s.ingest_bridge_deposits().await?;
    assert_eq!(ds.len(), 4);
    let d = s.bridge_deposit(id).await?.unwrap();
    assert_eq!(d.version, 0);
    assert_eq!(d.operation_id, op);
    assert!(
        s.initialize_bridge(BridgeDomain {
            chain_id: 11155111,
            contract: Address::repeat_byte(99)
        })
        .await
        .is_err()
    );
    let r = s.bridge_accounting(&accounting(2, 2, 0)).await?;
    assert_eq!(
        (r.assets, r.deposit_liability, r.fee_reserve),
        (190, 150, Some(40))
    );
    db.cleanup().await
}

#[tokio::test]
async fn bridge_claim_cas_uncertain_signed_none_and_restart_preserve_identity() -> Result<()> {
    let db = TestDb::new().await?;
    let (mut s, d) = funded(&db).await?;
    assert!(
        s.claim_bridge_mint(d.deposit_id, d.version, -1)
            .await
            .is_err()
    );
    let r = s.bridge_accounting(&accounting(2, 2, 0)).await?;
    let claimed = s.claim_bridge_mint(d.deposit_id, d.version, r.id).await?;
    assert!(
        s.claim_bridge_mint(d.deposit_id, d.version, r.id)
            .await
            .is_err()
    );
    let signer: PrivateKeySigner =
        "0000000000000000000000000000000000000000000000000000000000000001".parse()?;
    let domain = TransactionDomain::new(11155111, contract())?;
    let tx = domain.sign_operator(
        &signer,
        &domain.mint(
            d.recipient.unwrap(),
            B256::repeat_byte(44),
            Bytes::from_static(&[1]),
            d.deposit_id,
        ),
        TransactionFees {
            nonce: 7,
            gas_limit: 100000,
            max_fee_per_gas: 2,
            max_priority_fee_per_gas: 1,
        },
    )?;
    s.initialize_zama_lane(tx.sender(), 7, 7).await?;
    assert!(s.save_zama_signed("wrong-owner", &tx).await.is_err());
    s.save_zama_signed(&d.operation_id, &tx).await?;
    let none = ConsumptionEvidence {
        checkpoint: reference(2, 0),
        status: DepositStatus::None,
        event: None,
        actual_handle: None,
        actual_amount: None,
    };
    assert!(
        s.reconcile_bridge_deposit(d.deposit_id, d.version, &none)
            .await
            .is_err()
    );
    s.reconcile_bridge_deposit(d.deposit_id, claimed.version, &none)
        .await?;
    drop(s);
    let mut s = db.connect().await?;
    let recovered = s.zama_transaction(&d.operation_id).await?.unwrap();
    assert_eq!(recovered.signed.raw(), tx.raw());
    assert_eq!(recovered.signed.hash(), tx.hash());
    assert_eq!(recovered.signed.nonce(), 7);
    assert_eq!(s.zama_next_nonce(tx.sender()).await?, 8);
    let d = s.bridge_deposit(d.deposit_id).await?.unwrap();
    assert!(d.claimed);
    assert_eq!(d.state, DepositState::MintPending);
    s.reconcile_bridge_deposit(d.deposit_id, d.version, &none)
        .await?;
    assert!(s.rewind(1, cp(1).hash).await.is_err());
    assert!(halted(&s).await?);
    assert!(s.bridge_deposit(d.deposit_id).await?.unwrap().canonical);
    assert_eq!(
        s.zama_transaction(&d.operation_id)
            .await?
            .unwrap()
            .signed
            .raw(),
        tx.raw()
    );
    db.cleanup().await
}

#[tokio::test]
async fn bridge_exact_mint_two_confirmations_settles_once_and_old_checkpoint_keeps_liability()
-> Result<()> {
    let db = TestDb::new().await?;
    let (mut s, d) = funded(&db).await?;
    let e = minted(&d);
    // Rewind the unacted scanner fixture, then ingest one-confirmation lifecycle evidence.
    s.rewind_zama(reference(3, 0), reference(2, 0)).await?;
    evm(&mut s, 3, vec![e.clone()]).await?;
    let ev = evidence(e.clone(), Some(100));
    assert!(
        s.reconcile_bridge_deposit(d.deposit_id, d.version, &ev)
            .await
            .is_err()
    );
    evm(&mut s, 4, vec![]).await?;
    let settled = s
        .reconcile_bridge_deposit(d.deposit_id, d.version, &ev)
        .await?;
    assert_eq!(settled.state, DepositState::Minted);
    assert!(settled.acted_on);
    let same = s
        .reconcile_bridge_deposit(d.deposit_id, settled.version, &ev)
        .await?;
    assert_eq!(same.version, settled.version);
    assert!(
        s.client
            .query_one(
                "SELECT acted_on FROM zama_receipts WHERE tx_hash=$1",
                &[&e.transaction_hash.as_slice()]
            )
            .await?
            .get::<_, bool>(0)
    );
    let future = s.bridge_accounting(&accounting(1, 3, 0)).await?;
    assert!(!future.coverage_complete);
    assert_eq!(future.assets, 0);
    let old = s.bridge_accounting(&accounting(2, 2, 0)).await?;
    assert_eq!(old.deposit_liability, 100);
    let new = s.bridge_accounting(&accounting(2, 3, 100)).await?;
    assert_eq!(
        (new.assets, new.deposit_liability, new.fee_reserve),
        (100, 0, Some(0))
    );
    assert!(
        s.rewind_zama(reference(4, 0), reference(2, 0))
            .await
            .is_err()
    );
    assert!(halted(&s).await?);
    assert_eq!(
        s.bridge_deposit(d.deposit_id).await?.unwrap().state,
        DepositState::Minted
    );
    assert_eq!(s.zama_events().await?.len(), 1);
    db.cleanup().await
}

#[tokio::test]
async fn bridge_zero_mismatch_and_uint64_actual_are_durable_halts_with_liability() -> Result<()> {
    for actual in [0, 99, u64::MAX] {
        let db = TestDb::new().await?;
        let (mut s, d) = funded(&db).await?;
        s.rewind_zama(reference(3, 0), reference(2, 0)).await?;
        let e = minted(&d);
        evm(&mut s, 4, vec![e.clone()]).await?;
        let d = s
            .reconcile_bridge_deposit(d.deposit_id, d.version, &evidence(e, Some(actual)))
            .await?;
        assert_eq!(d.state, DepositState::Mismatch);
        assert!(halted(&s).await?);
        let r = s
            .client
            .query_one(
                "SELECT actual_amount::text,settled_number FROM bridge_deposits",
                &[],
            )
            .await?;
        assert_eq!(r.get::<_, String>(0), actual.to_string());
        assert_eq!(r.get::<_, Option<i64>>(1), None);
        drop(s);
        let mut s = db.connect().await?;
        s.migrate().await?;
        assert!(s.ingest_bridge_deposits().await.is_err());
        s.initialize_bridge(BridgeDomain {
            chain_id: 11155111,
            contract: contract(),
        })
        .await?;
        assert!(s.bridge_checkpoint_pair().await.is_err());
        assert_eq!(
            s.bridge_deposit(d.deposit_id).await?.unwrap().state,
            DepositState::Mismatch
        );
        db.cleanup().await?;
    }
    Ok(())
}

#[tokio::test]
async fn bridge_void_is_unpaid_liability_and_reversible_until_acted() -> Result<()> {
    let db = TestDb::new().await?;
    let (mut s, d) = funded(&db).await?;
    s.rewind_zama(reference(3, 0), reference(2, 0)).await?;
    let e = EventObservation {
        event: Event::DepositVoided {
            deposit_id: d.deposit_id,
        },
        ..minted(&d)
    };
    evm(&mut s, 4, vec![e.clone()]).await?;
    let d = s
        .reconcile_bridge_deposit(d.deposit_id, d.version, &evidence(e, None))
        .await?;
    assert_eq!(d.state, DepositState::Voided);
    assert!(!d.acted_on);
    let r = s.bridge_accounting(&accounting(2, 3, 0)).await?;
    assert_eq!(r.deposit_liability, 100);
    assert!(
        s.claim_bridge_mint(d.deposit_id, d.version, r.id)
            .await
            .is_err()
    );
    s.rewind_zama(reference(4, 0), reference(2, 0)).await?;
    let d = s.bridge_deposit(d.deposit_id).await?.unwrap();
    assert_eq!(d.state, DepositState::Eligible);
    s.rewind(1, cp(1).hash).await?;
    assert!(!s.bridge_deposit(d.deposit_id).await?.unwrap().canonical);
    assert!(!halted(&s).await?);
    db.cleanup().await
}

#[tokio::test]
async fn bridge_accounting_as_of_spends_change_and_checkpoint_validation() -> Result<()> {
    let db = TestDb::new().await?;
    let (mut s, d) = funded(&db).await?;
    let mut change = note(11, 90, true);
    change.mined_height = 5;
    scan(&mut s, 5, vec![change], vec![[10; 32]]).await?;
    scan(&mut s, 6, vec![], vec![]).await?;
    scan(&mut s, 7, vec![], vec![]).await?;
    let old = s.bridge_accounting(&accounting(2, 2, 0)).await?;
    assert_eq!(old.assets, 100);
    assert_eq!(old.deposit_liability, 100);
    let mut wrong = accounting(2, 2, 0);
    wrong.zcash_hash = [99; 32];
    assert!(s.bridge_accounting(&wrong).await.is_err());
    let mut wrong_evm_hash = accounting(2, 2, 0);
    wrong_evm_hash.checkpoint.hash = B256::repeat_byte(99);
    assert!(s.bridge_accounting(&wrong_evm_hash).await.is_err());
    let wrong = accounting(2, 3, 0);
    assert!(s.bridge_accounting(&wrong).await.is_err());
    let mut missing = accounting(0, 2, 0);
    assert!(s.bridge_accounting(&missing).await.is_err());
    missing = accounting(4, 2, 0);
    let r = s.bridge_accounting(&missing).await?;
    assert_eq!(r.assets, 100);
    assert!(
        s.claim_bridge_mint(d.deposit_id, d.version, r.id)
            .await
            .is_err()
    );
    let r = s.bridge_accounting(&accounting(5, 2, 0)).await?;
    assert_eq!(r.assets, 90);
    assert!(r.fee_reserve.is_none());
    assert!(halted(&s).await?);
    db.cleanup().await
}

#[tokio::test]
async fn bridge_unsupported_outgoing_redeem_and_synthetic_mint_block_authorization() -> Result<()> {
    for kind in 0..3 {
        let db = TestDb::new().await?;
        let (mut s, d) = funded(&db).await?;
        if kind == 0 {
            s.client.execute("INSERT INTO operations(id,destination,amount) VALUES('refund','test-destination',1)",&[]).await?;
        } else {
            s.rewind_zama(reference(3, 0), reference(2, 0)).await?;
            let event = if kind == 1 {
                Event::RedeemFulfilled {
                    redeem_id: alloy::primitives::U256::from(1),
                    zcash_txid: B256::repeat_byte(9),
                }
            } else {
                Event::DepositMinted {
                    deposit_id: B256::repeat_byte(99),
                    to: Address::repeat_byte(7),
                    amount: B256::repeat_byte(44),
                }
            };
            evm(
                &mut s,
                4,
                vec![EventObservation {
                    event,
                    ..minted(&d)
                }],
            )
            .await?;
        }
        let n = if kind == 0 { 2 } else { 3 };
        let r = s.bridge_accounting(&accounting(2, n, 0)).await?;
        assert!(!r.coverage_complete);
        assert!(!r.unsupported.is_empty());
        assert!(
            s.claim_bridge_mint(d.deposit_id, d.version, r.id)
                .await
                .is_err()
        );
        db.cleanup().await?;
    }
    Ok(())
}

#[tokio::test]
async fn bridge_uint64_supply_is_lossless_and_deficit_checkpoint_is_durable() -> Result<()> {
    let db = TestDb::new().await?;
    let (mut s, _) = funded(&db).await?;
    let r = s.bridge_accounting(&accounting(2, 2, u64::MAX)).await?;
    assert_eq!(r.supply, u64::MAX);
    assert!(r.fee_reserve.is_none());
    assert!(halted(&s).await?);
    assert_eq!(
        s.client
            .query_one(
                "SELECT supply::text FROM bridge_checkpoints WHERE id=$1",
                &[&r.id]
            )
            .await?
            .get::<_, String>(0),
        u64::MAX.to_string()
    );
    db.cleanup().await
}

#[tokio::test]
async fn bridge_duplicate_note_tag_cannot_rebind_amount_or_recipient() -> Result<()> {
    let db = TestDb::new().await?;
    let (mut s, d) = funded(&db).await?;
    let mut collision = note(11, 200, false);
    collision.rho = [10; 32];
    collision.rseed = [11; 32];
    collision.mined_height = 5;
    scan(&mut s, 5, vec![collision], vec![]).await?;
    scan(&mut s, 6, vec![], vec![]).await?;
    scan(&mut s, 7, vec![], vec![]).await?;
    assert!(s.ingest_bridge_deposits().await.is_err());
    let unchanged = s.bridge_deposit(d.deposit_id).await?.unwrap();
    assert_eq!(unchanged.amount, 100);
    assert_eq!(unchanged.recipient, d.recipient);
    assert_eq!(unchanged.note_id.txid, d.note_id.txid);
    assert_eq!(s.bridge_deposits().await?.len(), 1);
    db.cleanup().await
}

#[tokio::test]
async fn bridge_acted_accounting_checkpoint_reorg_halts_without_observed_mint() -> Result<()> {
    let db = TestDb::new().await?;
    let (mut s, d) = funded(&db).await?;
    evm(&mut s, 4, vec![]).await?;
    let r = s.bridge_accounting(&accounting(2, 3, 0)).await?;
    let d = s.claim_bridge_mint(d.deposit_id, d.version, r.id).await?;
    assert!(
        s.rewind_zama(reference(4, 0), reference(2, 0))
            .await
            .is_err()
    );
    assert!(halted(&s).await?);
    assert_eq!(s.zama_cursor().await?.unwrap().tip, reference(4, 0));
    assert_eq!(
        s.bridge_deposit(d.deposit_id).await?.unwrap().state,
        DepositState::MintPending
    );
    let row = s
        .client
        .query_one(
            "SELECT acted_on,valid FROM bridge_checkpoints WHERE id=$1",
            &[&r.id],
        )
        .await?;
    assert!(row.get::<_, bool>(0));
    assert!(row.get::<_, bool>(1));
    db.cleanup().await
}

#[tokio::test]
async fn bridge_missing_decryption_receipt_and_zero_supply_fail_closed() -> Result<()> {
    for mode in 0..4 {
        let db = TestDb::new().await?;
        let (mut s, d) = funded(&db).await?;
        let mut empty_supply = accounting(2, 2, 0);
        empty_supply.supply_handle = B256::ZERO;
        assert!(s.bridge_accounting(&empty_supply).await.is_err());
        s.rewind_zama(reference(3, 0), reference(2, 0)).await?;
        let event = minted(&d);
        let mut scan = batch(reference(2, 0), 4, 0);
        scan.chain_id = 11155111;
        scan.events = vec![event.clone()];
        if mode != 0 {
            scan.receipts = vec![ReceiptObservation {
                transaction_hash: event.transaction_hash,
                block: event.block,
                from: Address::repeat_byte(9),
                to: contract(),
                success: mode != 1,
            }];
        }
        s.apply_zama_scan(&scan).await?;
        let mut ev = evidence(event, Some(100));
        if mode == 2 {
            ev.actual_amount = None;
        }
        if mode == 3 {
            ev.actual_handle = None;
        }
        assert!(
            s.reconcile_bridge_deposit(d.deposit_id, d.version, &ev)
                .await
                .is_err()
        );
        let unchanged = s.bridge_deposit(d.deposit_id).await?.unwrap();
        assert_eq!(unchanged.state, DepositState::Eligible);
        assert_eq!(unchanged.version, d.version);
        assert!(!halted(&s).await?);
        db.cleanup().await?;
    }
    Ok(())
}

#[tokio::test]
async fn bridge_reincluded_source_note_keeps_identity_and_revises_provenance() -> Result<()> {
    let db = TestDb::new().await?;
    let (mut s, d) = funded(&db).await?;
    s.rewind(1, cp(1).hash).await?;
    let orphan = s.bridge_deposit(d.deposit_id).await?.unwrap();
    assert!(!orphan.canonical);
    assert!(orphan.version > d.version);
    let mut source = note(10, 100, false);
    source.mined_height = 3;
    scan(&mut s, 2, vec![], vec![]).await?;
    scan(&mut s, 3, vec![source], vec![]).await?;
    scan(&mut s, 4, vec![], vec![]).await?;
    scan(&mut s, 5, vec![], vec![]).await?;
    let restored = s.ingest_bridge_deposits().await?.remove(0);
    assert_eq!(restored.deposit_id, d.deposit_id);
    assert_eq!(restored.operation_id, d.operation_id);
    assert_eq!(restored.amount, d.amount);
    assert_eq!(restored.recipient, d.recipient);
    assert_eq!(restored.source_height, 3);
    assert_eq!(restored.source_hash, cp(3).hash);
    assert!(restored.version > orphan.version);
    assert!(restored.canonical);
    assert!(!halted(&s).await?);
    db.cleanup().await
}
