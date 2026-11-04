//! Actual collector + disposable PostgreSQL + controlled chain facts. Fixture
//! authorization is dummy and never submitted to Zcash or claimed consensus valid.
use super::{
    Store,
    bridge_tests::{accounting, evm, note, scan, setup},
    payout_death_test_support::{data, hash, serve},
    zama_test_support::{TestDb, batch, contract, reference},
};
use crate::core::{payout_death, redeem_types::*};
use alloy::{
    primitives::{Address, B256, Bytes, U256},
    signers::local::PrivateKeySigner,
};
use anyhow::Result;
use zama::{
    contract::{TransactionDomain, TransactionFees},
    watcher::{Event, EventObservation, ReceiptObservation},
};
use zcash::{
    core::{LocalSigner, NoteId, PreparedTransaction},
    rpc::history::{self, LOWER_HEIGHT},
};
const END: u32 = LOWER_HEIGHT + 5;
fn prepared() -> PreparedTransaction {
    PreparedTransaction {
        bytes: include_bytes!("../../tests/fixtures/death-attempt.bin").to_vec(),
        txid: hex::decode("86cd8abeb7c12309063631c0e1a2a8dd352061ad135668d8e4dce94667e49fd2")
            .unwrap()
            .try_into()
            .unwrap(),
        expiry_height: LOWER_HEIGHT + 2,
        inputs: vec![NoteId {
            txid: [10; 32],
            action_index: 0,
        }],
        fee: REDEEM_FEE_ZAT,
    }
}
async fn issued(db: &TestDb) -> Result<(Store, BridgeRedeem)> {
    let mut s = setup(db).await?;
    scan(&mut s, 2, vec![note(10, 200_000, true)], vec![]).await?;
    scan(&mut s, 3, vec![], vec![]).await?;
    scan(&mut s, 4, vec![], vec![]).await?;
    let event = EventObservation {
        block: reference(2, 0),
        transaction_hash: B256::repeat_byte(71),
        log_index: 0,
        event: Event::RedeemRequested {
            redeem_id: U256::from(71),
            user: Address::repeat_byte(7),
            amount: B256::repeat_byte(44),
            dest_ct: Bytes::from_static(&[1]),
        },
    };
    evm(&mut s, 3, vec![event.clone()]).await?;
    let path = std::env::temp_dir().join(format!(
        "death-test-address-{}-{}",
        std::process::id(),
        rand_core::OsRng.next_u64()
    ));
    let signer = LocalSigner::create(&path)?;
    let destination = signer.addresses()?.0;
    std::fs::remove_file(path)?;
    let pair = s.bridge_checkpoint_pair().await?;
    let r = s
        .record_bridge_redeem(&event, pair, 60_000, Some(&destination))
        .await?;
    let report = s.bridge_accounting(&accounting(2, 2, 40_000)).await?;
    let r = s
        .reserve_bridge_redeem(r.id, r.version, report.id, prepared().inputs[0])
        .await?;
    s.save_prepared(&r.operation_id, &prepared()).await?;
    // Model a later scanner snapshot directly, preserving the earlier controlled
    // burn/input facts. No millions-of-block scanner replay is implied here.
    for height in END..=END + 2 {
        s.client
            .execute(
                "INSERT INTO checkpoints(height,hash,parent,frontier) VALUES($1,$2,$3,$4)",
                &[
                    &i64::from(height),
                    &&hash(height)[..],
                    &&hash(height - 1)[..],
                    &&[0_u8, 0, 0][..],
                ],
            )
            .await?;
    }
    s.client
        .execute(
            "UPDATE account SET current_height=$1",
            &[&i64::from(END + 2)],
        )
        .await?;
    let pair = s.bridge_checkpoint_pair().await?;
    s.request_payout_history(r.id, r.version, pair).await?;
    Ok((s, r))
}
use rand_core::RngCore;

#[tokio::test]
async fn interrupted_batch_restart_authorization_and_acted_reorg() -> Result<()> {
    let db = TestDb::new().await?;
    let (mut s, r) = issued(&db).await?;
    let server = serve(data(END)).await;
    let d = s.payout_history(r.id).await?.unwrap();
    let initial = history::start(&server.client, &d.attempts).await?;
    let prefix = history::advance(&server.client, &d.attempts, &initial, 2).await?;
    s.save_payout_history(r.id, d.revision, &prefix).await?;
    let saved = s.payout_history(r.id).await?.unwrap();
    server.state.lock().unwrap().fail = Some((false, LOWER_HEIGHT + 2, tonic::Code::NotFound));
    assert!(
        payout_death::collect_once(&mut s, &server.client)
            .await
            .is_err()
    );
    let unchanged = s.payout_history(r.id).await?.unwrap();
    assert_eq!(unchanged.history, saved.history);
    assert_eq!(unchanged.revision, saved.revision);
    assert_eq!(unchanged.status, "collecting");
    assert_eq!(s.reserved_inputs().await?.len(), 1);
    drop(s);
    let mut s = db.connect().await?;
    server.state.lock().unwrap().fail = None;
    assert!(payout_death::collect_once(&mut s, &server.client).await?);
    assert_eq!(s.bridge_redeems().await?[0].state, RedeemState::Reserved);
    let d = s.payout_history(r.id).await?.unwrap();
    assert!(d.history.as_ref().unwrap().complete_absence());
    assert!(!d.acted_on);
    assert!(!s.get_attempt(&r.operation_id).await?.unwrap().broadcast);
    let pair = s.bridge_checkpoint_pair().await?;
    let pending =
        payout_death::authorize_recredit(&mut s, &server.client, r.id, r.version, pair).await?;
    assert_eq!(pending.state, RedeemState::RecreditPending);
    assert_eq!(s.reserved_inputs().await?.len(), 1);
    assert_eq!(
        payout_death::authorize_recredit(&mut s, &server.client, r.id, r.version, pair)
            .await?
            .version,
        pending.version
    );
    server.state.lock().unwrap().fail = Some((false, END, tonic::Code::Unavailable));
    assert!(
        payout_death::collect_once(&mut s, &server.client)
            .await
            .is_err()
    );
    assert_eq!(s.payout_history(r.id).await?.unwrap().status, "complete");
    assert_eq!(s.http_health().await?["halted"], false);
    server.state.lock().unwrap().fail = None;
    server
        .state
        .lock()
        .unwrap()
        .blocks
        .get_mut(&END)
        .unwrap()
        .hash[31] ^= 1;
    assert!(
        payout_death::collect_once(&mut s, &server.client)
            .await
            .is_err()
    );
    assert_eq!(s.payout_history(r.id).await?.unwrap().status, "invalid");
    assert_eq!(s.http_health().await?["halted"], true);
    assert_eq!(s.reserved_inputs().await?.len(), 1);
    db.cleanup().await
}

#[tokio::test]
async fn canonical_drift_before_admission_requires_explicit_restart() -> Result<()> {
    let db = TestDb::new().await?;
    let (mut s, r) = issued(&db).await?;
    let server = serve(data(END)).await;
    payout_death::collect_once(&mut s, &server.client).await?;
    let original = s.payout_history(r.id).await?.unwrap();
    server
        .state
        .lock()
        .unwrap()
        .blocks
        .get_mut(&END)
        .unwrap()
        .hash[31] ^= 1;
    let pair = s.bridge_checkpoint_pair().await?;
    assert!(
        payout_death::authorize_recredit(&mut s, &server.client, r.id, r.version, pair)
            .await
            .is_err()
    );
    let invalid = s.payout_history(r.id).await?.unwrap();
    assert_eq!(invalid.status, "invalid");
    assert!(!invalid.acted_on);
    assert!(!payout_death::collect_once(&mut s, &server.client).await?);
    assert_eq!(
        s.payout_history(r.id).await?.unwrap().revision,
        invalid.revision
    );
    assert_eq!(s.http_health().await?["halted"], false);
    server
        .state
        .lock()
        .unwrap()
        .blocks
        .get_mut(&END)
        .unwrap()
        .hash[31] ^= 1;
    s.request_payout_history(r.id, r.version, pair).await?;
    assert!(s.payout_history(r.id).await?.unwrap().history.is_none());
    assert!(
        s.save_payout_history(r.id, original.revision, original.history.as_ref().unwrap())
            .await
            .is_err()
    );
    payout_death::collect_once(&mut s, &server.client).await?;
    assert_eq!(s.payout_history(r.id).await?.unwrap().status, "complete");
    db.cleanup().await
}

#[tokio::test]
async fn reservations_release_only_after_exact_confirmed_original_restoration() -> Result<()> {
    let db = TestDb::new().await?;
    let (mut s, r) = issued(&db).await?;
    let server = serve(data(END)).await;
    payout_death::collect_once(&mut s, &server.client).await?;
    let pair = s.bridge_checkpoint_pair().await?;
    let mut evidence = accounting(2, 2, 40_000);
    evidence.zcash_height = END;
    evidence.zcash_hash = hash(END);
    let before = s.bridge_accounting(&evidence).await?;
    assert_eq!(before.unpaid_redemptions, 60_000);
    assert!(before.coverage_complete);
    payout_death::authorize_recredit(&mut s, &server.client, r.id, r.version, pair).await?;
    let pending = s.bridge_accounting(&evidence).await?;
    assert_eq!(pending.unpaid_redemptions, before.unpaid_redemptions);
    assert_eq!(pending.fee_reserve, before.fee_reserve);
    assert_eq!(pending.assets, before.assets);
    assert!(pending.coverage_complete);
    let operator: PrivateKeySigner =
        "0000000000000000000000000000000000000000000000000000000000000001".parse()?;
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
    s.save_zama_signed(&r.recredit_id(), &signed).await?;
    drop(s);
    let mut s = db.connect().await?;
    s.save_zama_signed(&r.recredit_id(), &signed).await?;
    assert_eq!(
        s.zama_transaction(&r.recredit_id())
            .await?
            .unwrap()
            .signed
            .raw(),
        signed.raw()
    );
    let restoration = EventObservation {
        block: reference(4, 0),
        transaction_hash: signed.hash(),
        log_index: 0,
        event: Event::RedeemRecredited {
            redeem_id: r.id,
            user: r.burner,
            amount: B256::repeat_byte(81),
        },
    };
    let mut b = batch(s.zama_cursor().await?.unwrap().tip, 4, 0);
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
        s.reconcile_bridge_recredit(r.id, &restoration, pair, 60_000)
            .await
            .is_err()
    );
    evm(&mut s, 5, vec![]).await?;
    let pair = s.bridge_checkpoint_pair().await?;
    let mut wrong = restoration.clone();
    if let Event::RedeemRecredited { user, .. } = &mut wrong.event {
        *user = operator.address();
    }
    assert!(
        s.reconcile_bridge_recredit(r.id, &wrong, pair, 60_000)
            .await
            .is_err()
    );
    assert!(
        s.reconcile_bridge_recredit(r.id, &restoration, pair, 59_999)
            .await
            .is_err()
    );
    assert_eq!(s.reserved_inputs().await?.len(), 1);
    payout_death::revalidate_acted(&mut s, &server.client).await?;
    s.reconcile_bridge_recredit(r.id, &restoration, pair, 60_000)
        .await?;
    assert!(s.reserved_inputs().await?.is_empty());
    assert_eq!(
        s.get_attempt(&r.operation_id)
            .await?
            .unwrap()
            .prepared
            .unwrap()
            .bytes,
        prepared().bytes
    );
    assert_eq!(s.bridge_redeems().await?[0].state, RedeemState::Recredited);
    evidence.checkpoint = reference(4, 0);
    evidence.supply = 100_000;
    let settled = s.bridge_accounting(&evidence).await?;
    assert_eq!(settled.unpaid_redemptions, 0);
    assert_eq!(settled.assets, before.assets);
    assert_eq!(settled.fee_reserve, before.fee_reserve);
    assert!(settled.coverage_complete);
    // Released liquidity can be reserved again without erasing the old attempt's
    // inputs. Reapplying migrations preserves both active and released ownership.
    s.client.execute("INSERT INTO operations(id,destination,amount) VALUES('later-operation','controlled',1)", &[]).await?;
    s.client.execute("INSERT INTO reservations(txid,action_index,operation_id,nullifier) SELECT txid,action_index,'later-operation',nullifier FROM reservations WHERE operation_id=$1", &[&r.operation_id]).await?;
    s.migrate().await?;
    assert_eq!(s.reserved_inputs().await?.len(), 1);
    assert!(s.get_attempt(&r.operation_id).await?.unwrap().inputs == prepared().inputs);
    let retained: i64 = s
        .client
        .query_one(
            "SELECT count(*) FROM reservations WHERE released AND operation_id=$1",
            &[&r.operation_id],
        )
        .await?
        .get(0);
    assert_eq!(retained, 1);

    server
        .state
        .lock()
        .unwrap()
        .blocks
        .get_mut(&END)
        .unwrap()
        .hash[31] ^= 1;
    assert!(
        payout_death::collect_once(&mut s, &server.client)
            .await
            .is_err()
    );
    assert_eq!(s.http_health().await?["halted"], true);
    db.cleanup().await
}

#[tokio::test]
async fn http_authorization_waits_for_shared_payment_lane_and_get_is_read_only() -> Result<()> {
    use super::payout_death_test_support::{HttpServer, evm_rpc};
    use crate::api::{BridgeApi, bridge_router};
    use crate::core::destination::DestinationKey;
    use alloy::transports::http::reqwest;
    use serde_json::{Value, json};
    use std::sync::{Arc, atomic::AtomicBool};
    let db = TestDb::new().await?;
    let (mut s, r) = issued(&db).await?;
    let grpc = serve(data(END + 2)).await;
    s.client
        .execute(
            "DELETE FROM bridge_payout_death WHERE redeem_id=$1",
            &[&r.id.to_string()],
        )
        .await?;
    let (rpc, _) = evm_rpc().await?;
    let contract_client =
        Arc::new(zama::contract::CwzecClient::connect(&rpc.url, 11155111, contract()).await?);
    let path = std::env::temp_dir().join(format!(
        "death-http-key-{}-{}",
        std::process::id(),
        rand_core::OsRng.next_u64()
    ));
    let key = Arc::new(DestinationKey::create(&path)?);
    std::fs::remove_file(path)?;
    let lane = Arc::new(tokio::sync::Mutex::new(()));
    let api = BridgeApi::new(
        db.connect().await?,
        contract_client,
        Arc::new(zama::fhe::Client::new(&rpc.url)?),
        key,
        grpc.endpoint.clone(),
        "public-deposit".into(),
        "controlled-admin".into(),
        Arc::new(AtomicBool::new(true)),
    )
    .with_payment_lock(lane.clone());
    let api = HttpServer::start(bridge_router(Arc::new(api))).await?;
    let http = reqwest::Client::builder().no_proxy().build()?;
    for _ in 0..2 {
        let response = http
            .post(format!("{}/v1/admin/recovery", api.url))
            .header("x-api-key", "controlled-admin")
            .json(&json!({"redeem_id":r.id.to_string(),"version":r.version.to_string()}))
            .send()
            .await?;
        assert_eq!(response.status().as_u16(), 202);
        let requested = s.payout_history(r.id).await?.unwrap();
        assert_eq!(requested.status, "collecting");
        assert!(requested.history.is_none());
        assert!(!requested.acted_on);
        assert_eq!(requested.revision, 0);
    }
    payout_death::collect_once(&mut s, &grpc.client).await?;

    let d = s.payout_history(r.id).await?.unwrap();
    let reads = grpc.state.lock().unwrap().reads.clone();
    let response: Value = http
        .get(format!("{}/v1/admin/recovery", api.url))
        .header("x-api-key", "controlled-admin")
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(response["requests"][0]["complete_absence"], true);
    for private in ["signed_bytes", "destination", "amount", "frontier"] {
        assert!(!response.to_string().contains(private));
    }
    assert_eq!(grpc.state.lock().unwrap().reads, reads);
    assert_eq!(s.payout_history(r.id).await?.unwrap().revision, d.revision);
    let held = lane.lock().await;
    let request = http
        .post(format!("{}/v1/admin/recredit", api.url))
        .header("x-api-key", "controlled-admin")
        .json(&json!({"redeem_id":r.id.to_string(),"version":r.version.to_string()}));
    let mut pending = tokio::spawn(async move { request.send().await });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut pending)
            .await
            .is_err()
    );
    assert!(!s.payout_history(r.id).await?.unwrap().acted_on);
    drop(held);
    let response = pending.await??;
    assert_eq!(response.status().as_u16(), 202);
    assert!(s.payout_history(r.id).await?.unwrap().acted_on);
    assert!(
        s.save_prepared(&r.operation_id, &prepared()).await.is_err(),
        "journal cannot prepare after acted recovery"
    );
    assert!(
        s.mark_broadcast(&r.operation_id).await.is_err(),
        "rebroadcast acknowledgement cannot cross acted recovery"
    );
    // Unknown raw-complete/force flags never reach the scan request path.
    let response = http
        .post(format!("{}/v1/admin/recovery", api.url))
        .header("x-api-key", "controlled-admin")
        .json(
            &json!({"redeem_id":r.id.to_string(),"version":r.version.to_string(),"complete":true}),
        )
        .send()
        .await?;
    assert_eq!(response.status().as_u16(), 400);
    db.cleanup().await
}

#[tokio::test]
async fn newly_authorized_issued_recredit_recovers_uncertain_evm_submission_exact_bytes()
-> Result<()> {
    use super::payout_death_test_support::evm_rpc;
    use crate::{core::zama::ZamaService, store::TransactionState};
    let db = TestDb::new().await?;
    let (mut s, r) = issued(&db).await?;
    let server = serve(data(END)).await;
    payout_death::collect_once(&mut s, &server.client).await?;
    let pair = s.bridge_checkpoint_pair().await?;
    payout_death::authorize_recredit(&mut s, &server.client, r.id, r.version, pair).await?;
    let (rpc, facts) = evm_rpc().await?;
    let mut evm = ZamaService::open(&db.url(), &rpc.url, 11155111, contract(), 2).await?;
    let operator: PrivateKeySigner =
        "0000000000000000000000000000000000000000000000000000000000000001".parse()?;
    let call = evm.client().domain().recredit(r.id);
    payout_death::revalidate_acted(&mut s, &server.client).await?;
    let saved = evm
        .prepare_operator_after_death(
            &r.recredit_id(),
            &operator,
            &call,
            100_000,
            2,
            1,
            &server.client,
        )
        .await?;
    assert!(evm.recover_operator(&r.recredit_id()).await.is_err());
    assert!(facts.lock().unwrap().broadcasts.is_empty());
    facts.lock().unwrap().lose_ack = true;
    payout_death::revalidate_acted(&mut s, &server.client).await?;
    assert_eq!(
        evm.recover_operator_after_death(&r.recredit_id(), &server.client)
            .await?
            .state,
        TransactionState::Unresolved
    );
    drop(evm);
    let mut evm = ZamaService::open(&db.url(), &rpc.url, 11155111, contract(), 2).await?;
    facts.lock().unwrap().lose_ack = false;
    payout_death::revalidate_acted(&mut s, &server.client).await?;
    assert_eq!(
        evm.recover_operator_after_death(&r.recredit_id(), &server.client)
            .await?
            .state,
        TransactionState::Pending
    );
    let facts = facts.lock().unwrap();
    assert_eq!(facts.broadcasts.len(), 2);
    for raw in &facts.broadcasts {
        assert_eq!(raw, &format!("0x{}", hex::encode(saved.signed.raw())));
    }
    drop(facts);
    assert_eq!(s.reserved_inputs().await?.len(), 1);
    db.cleanup().await
}

#[tokio::test]
async fn paid_or_observed_journal_blocks_even_completed_absence() -> Result<()> {
    for paid in [false, true] {
        let db = TestDb::new().await?;
        let (mut s, r) = issued(&db).await?;
        let server = serve(data(END)).await;
        payout_death::collect_once(&mut s, &server.client).await?;
        // Controlled persisted scanner/payment facts conflict with absence. Both
        // the operator restart and authorization must refuse without erasure.
        if paid {
            s.client
                .execute(
                    "UPDATE bridge_redemptions SET state='paid',paid_height=$2 WHERE redeem_id=$1",
                    &[&r.id.to_string(), &i64::from(END)],
                )
                .await?;
        } else {
            s.client
                .execute(
                    "INSERT INTO observed_transactions(txid,height) VALUES($1,$2)",
                    &[&&prepared().txid[..], &i64::from(END)],
                )
                .await?;
        }
        let pair = s.bridge_checkpoint_pair().await?;
        assert!(
            s.request_payout_history(r.id, r.version, pair)
                .await
                .is_err()
        );
        assert!(
            payout_death::authorize_recredit(&mut s, &server.client, r.id, r.version, pair)
                .await
                .is_err()
        );
        assert!(!s.payout_history(r.id).await?.unwrap().acted_on);
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
        db.cleanup().await?;
    }
    Ok(())
}

#[tokio::test]
async fn canonical_drift_during_evm_nonce_lookup_blocks_signing_or_broadcast() -> Result<()> {
    use super::payout_death_test_support::evm_rpc;
    use crate::core::zama::ZamaService;
    for before_signing in [true, false] {
        let db = TestDb::new().await?;
        let (mut s, r) = issued(&db).await?;
        let server = serve(data(END)).await;
        payout_death::collect_once(&mut s, &server.client).await?;
        let pair = s.bridge_checkpoint_pair().await?;
        payout_death::authorize_recredit(&mut s, &server.client, r.id, r.version, pair).await?;
        let (rpc, facts) = evm_rpc().await?;
        let mut evm = ZamaService::open(&db.url(), &rpc.url, 11155111, contract(), 2).await?;
        let operator: PrivateKeySigner =
            "0000000000000000000000000000000000000000000000000000000000000001".parse()?;
        let call = evm.client().domain().recredit(r.id);
        if !before_signing {
            evm.prepare_operator_after_death(
                &r.recredit_id(),
                &operator,
                &call,
                100_000,
                2,
                1,
                &server.client,
            )
            .await?;
        }
        facts.lock().unwrap().drift_on_nonce = Some((server.state.clone(), END));
        if before_signing {
            assert!(
                evm.prepare_operator_after_death(
                    &r.recredit_id(),
                    &operator,
                    &call,
                    100_000,
                    2,
                    1,
                    &server.client
                )
                .await
                .is_err()
            );
            assert!(s.zama_transaction(&r.recredit_id()).await?.is_none());
        } else {
            assert!(
                evm.recover_operator_after_death(&r.recredit_id(), &server.client)
                    .await
                    .is_err()
            );
            assert!(s.zama_transaction(&r.recredit_id()).await?.is_some());
        }
        assert!(facts.lock().unwrap().broadcasts.is_empty());
        assert_eq!(s.payout_history(r.id).await?.unwrap().status, "invalid");
        assert_eq!(s.http_health().await?["halted"], true);
        assert_eq!(s.reserved_inputs().await?.len(), 1);
        db.cleanup().await?;
    }
    Ok(())
}

#[tokio::test]
async fn canonical_positive_at_inclusive_expiry_blocks_operator_recovery() -> Result<()> {
    use prost::Message;
    use zcash::rpc::CompactTx;
    let db = TestDb::new().await?;
    let (mut s, r) = issued(&db).await?;
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/death-attempt-compact.json"
    ))?;
    let compact =
        CompactTx::decode(hex::decode(fixture["compact_prost_hex"].as_str().unwrap())?.as_slice())?;
    assert_eq!(compact.txid, prepared().txid);
    let mut facts = data(END);
    let inclusion = prepared().expiry_height;
    facts.blocks.get_mut(&inclusion).unwrap().vtx.push(compact);
    for height in inclusion..=END {
        facts
            .blocks
            .get_mut(&height)
            .unwrap()
            .chain_metadata
            .as_mut()
            .unwrap()
            .ironwood_commitment_tree_size = fixture["tree_size"].as_u64().unwrap() as u32;
        facts.trees.get_mut(&height).unwrap().ironwood_tree =
            fixture["ironwood_tree"].as_str().unwrap().into();
    }
    let server = serve(facts).await;
    payout_death::collect_once(&mut s, &server.client).await?;
    let d = s.payout_history(r.id).await?.unwrap();
    assert_eq!(d.status, "included");
    let h = d.history.unwrap();
    assert!(!h.complete_absence());
    assert_eq!(h.inclusions()[0].height, inclusion);
    let pair = s.bridge_checkpoint_pair().await?;
    assert!(
        payout_death::authorize_recredit(&mut s, &server.client, r.id, r.version, pair)
            .await
            .is_err()
    );
    assert_eq!(s.bridge_redeems().await?[0].state, RedeemState::Reserved);
    assert_eq!(s.reserved_inputs().await?.len(), 1);
    assert!(!s.payout_history(r.id).await?.unwrap().acted_on);
    db.cleanup().await
}

#[tokio::test]
async fn scanner_rewind_across_acted_death_checkpoint_halts_without_erasing_journal() -> Result<()>
{
    let db = TestDb::new().await?;
    let (mut s, r) = issued(&db).await?;
    let server = serve(data(END)).await;
    payout_death::collect_once(&mut s, &server.client).await?;
    let pair = s.bridge_checkpoint_pair().await?;
    payout_death::authorize_recredit(&mut s, &server.client, r.id, r.version, pair).await?;
    let ancestor = END - 1;
    s.client
        .execute(
            "INSERT INTO checkpoints(height,hash,parent,frontier) VALUES($1,$2,$3,$4)",
            &[
                &i64::from(ancestor),
                &&hash(ancestor)[..],
                &&hash(ancestor - 1)[..],
                &&[0_u8, 0, 0][..],
            ],
        )
        .await?;
    assert!(s.rewind(ancestor, hash(ancestor)).await.is_err());
    assert_eq!(s.http_health().await?["halted"], true);
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
    db.cleanup().await
}
