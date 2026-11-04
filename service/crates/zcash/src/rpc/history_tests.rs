use super::super::{
    BlockId, ChainMetadata, CompactBlock, CompactIronwoodAction, CompactTx, Empty, LightdInfo,
    TreeState,
};
use super::*;
use incrementalmerkletree::frontier::CommitmentTree;
use orchard::{
    Note, NoteVersion,
    bundle::{Authorized, BundleVersion, EffectsOnly, Flags},
    keys::Scope,
    note::{ExtractedNoteCommitment, Nullifier},
    note::{RandomSeed, Rho, TransmittedNoteCiphertext},
    note_encryption::IronwoodDomain,
    note_encryption::IronwoodNoteEncryption,
    primitives::redpallas::{SpendAuth, VerificationKey},
    value::{NoteValue, ValueCommitment},
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use zcash_note_encryption::Domain;
use zcash_primitives::transaction::TransactionData;
use zcash_protocol::{consensus::TEST_NETWORK, value::ZatBalance};
#[path = "../../examples/feasibility/fixture.rs"]
mod fixture;
fn action(scope: Scope, rho: [u8; 32], value: u64) -> orchard::Action<()> {
    let fvk = fixture::viewing_key();
    let rho = Rho::from_bytes(&rho).unwrap();
    let rseed = RandomSeed::from_bytes(
        *fixture::note(&fvk, NoteVersion::V3).rseed().as_bytes(),
        &rho,
    )
    .unwrap();
    let note = Note::from_parts(
        fvk.address_at(0_u32, scope),
        NoteValue::from_raw(value),
        rho,
        rseed,
        NoteVersion::V3,
    )
    .unwrap();
    let mut memo = [0_u8; 512];
    memo[..12].copy_from_slice(b"public memo!");
    let enc = IronwoodNoteEncryption::new(None, note, memo);
    orchard::Action::from_parts(
        Nullifier::from_bytes(&rho.to_bytes()).unwrap(),
        VerificationKey::<SpendAuth>::try_from(fixed::<32>(&fvk.to_bytes()[..32], "rk").unwrap())
            .unwrap(),
        ExtractedNoteCommitment::from(note.commitment()),
        TransmittedNoteCiphertext {
            epk_bytes: IronwoodDomain::epk_bytes(enc.epk()).0,
            enc_ciphertext: enc.encrypt_note_plaintext(),
            out_ciphertext: [0; 80],
        },
        ValueCommitment::from_bytes(&fixed::<32>(&fvk.to_bytes()[..32], "cv").unwrap()).unwrap(),
        (),
    )
    .unwrap()
}

fn full(actions: Vec<orchard::Action<()>>, expiry: u32) -> Transaction {
    // Dummy authorization is deliberate: these fixtures test scanning and parsing,
    // never consensus validity, proving, signing, submission, or a funded payment.
    let count = actions.len();
    let effects = orchard::Bundle::from_parts(
        nonempty::NonEmpty::from_vec(actions).unwrap(),
        Flags::ENABLED,
        ZatBalance::zero(),
        orchard::Anchor::empty_tree(),
        EffectsOnly,
        BundleVersion::ironwood_v3(),
    )
    .unwrap();
    let bundle = effects.map_authorization(
        &mut (),
        |_, _, _| [0_u8; 64].into(),
        |_, _| {
            Authorized::from_parts(
                orchard::Proof::new(vec![0; orchard::Proof::expected_proof_size(count)]),
                [0_u8; 64].into(),
            )
        },
    );
    TransactionData::from_parts_v6(
        BranchId::for_height(&TEST_NETWORK, LOWER_HEIGHT.into()),
        0,
        expiry.into(),
        None,
        None,
        None,
        Some(bundle),
    )
    .freeze()
    .unwrap()
}

fn prepared(expiry: u32, salt: u8) -> PreparedTransaction {
    let tx = full(vec![action(Scope::External, [salt; 32], 20)], expiry);
    let mut bytes = vec![];
    tx.write(&mut bytes).unwrap();
    PreparedTransaction {
        bytes,
        txid: *tx.txid().as_ref(),
        expiry_height: expiry,
        inputs: vec![],
        fee: 0,
    }
}
fn compact(attempt: &PreparedTransaction) -> CompactTx {
    let tx = parse(attempt).unwrap();
    CompactTx {
        index: 2,
        txid: attempt.txid.to_vec(),
        ironwood_actions: tx
            .ironwood_bundle()
            .unwrap()
            .actions()
            .iter()
            .map(|a| CompactIronwoodAction {
                nullifier: a.nullifier().to_bytes().to_vec(),
                cmx: a.cmx().to_bytes().to_vec(),
                ephemeral_key: a.encrypted_note().epk_bytes.to_vec(),
                ciphertext: a.encrypted_note().enc_ciphertext.0[..52].to_vec(),
            })
            .collect(),
    }
}
#[derive(Default)]
struct Data {
    tip: u32,
    blocks: BTreeMap<u32, CompactBlock>,
    trees: BTreeMap<u32, TreeState>,
    hash_trees: BTreeMap<Vec<u8>, TreeState>,
    hash_requests: Vec<Vec<u8>>,
    fail: Option<(bool, u32, tonic::Code)>,
    reads: BTreeMap<u32, usize>,
    reorg: Option<(u32, usize)>,
    delay_ms: u64,
    active: Arc<std::sync::atomic::AtomicUsize>,
    peak: Arc<std::sync::atomic::AtomicUsize>,
}
fn data(end: u32, mined: Option<(u32, CompactTx)>) -> Data {
    data_with_baseline(end, mined, None)
}
fn data_with_baseline(
    end: u32,
    mined: Option<(u32, CompactTx)>,
    baseline: Option<CompactTx>,
) -> Data {
    let mut data = Data {
        tip: end,
        ..Default::default()
    };
    let mut tree = CommitmentTree::<MerkleHashOrchard, 32>::empty();
    // The trusted pre-NU7 frontier can already contain earlier Ironwood leaves.
    if let Some(tx) = baseline {
        for wire in &tx.ironwood_actions {
            tree.append(MerkleHashOrchard::from_cmx(
                &compact_action(wire).unwrap().cmx(),
            ))
            .unwrap();
        }
    }
    for height in LOWER_HEIGHT - 1..=end {
        let vtx = mined
            .as_ref()
            .filter(|(h, _)| *h == height)
            .map(|(_, tx)| vec![tx.clone()])
            .unwrap_or_default();
        for tx in &vtx {
            for wire in &tx.ironwood_actions {
                tree.append(MerkleHashOrchard::from_cmx(
                    &compact_action(wire).unwrap().cmx(),
                ))
                .unwrap();
            }
        }
        let hash = hash(height);
        let block = CompactBlock {
            height: height.into(),
            hash: hash.to_vec(),
            prev_hash: self::hash(height - 1).to_vec(),
            vtx,
            chain_metadata: Some(ChainMetadata {
                ironwood_commitment_tree_size: tree.size() as u32,
            }),
        };
        let mut bytes = vec![];
        write_commitment_tree(&tree, &mut bytes).unwrap();
        let mut display = hash;
        display.reverse();
        data.trees.insert(
            height,
            TreeState {
                network: "test".into(),
                height: height.into(),
                hash: hex::encode(display),
                ironwood_tree: hex::encode(bytes),
            },
        );
        data.blocks.insert(height, block);
    }
    data
}
fn hash(height: u32) -> [u8; 32] {
    let mut hash = [0; 32];
    hash[..4].copy_from_slice(&height.to_le_bytes());
    hash
}
#[derive(Clone)]
struct Server(Arc<Mutex<Data>>);
struct InFlight(Arc<std::sync::atomic::AtomicUsize>);
impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}
impl tonic::server::NamedService for Server {
    const NAME: &'static str = "cash.z.wallet.sdk.rpc.CompactTxStreamer";
}
macro_rules! unary {
    ($req:expr, $state:expr, $q:ty, $r:ty, $body:expr) => {{
        struct Method(Arc<Mutex<Data>>);
        impl tonic::server::UnaryService<$q> for Method {
            type Response = $r;
            type Future = tonic::codegen::BoxFuture<tonic::Response<$r>, tonic::Status>;
            fn call(&mut self, req: tonic::Request<$q>) -> Self::Future {
                let state = self.0.clone();
                Box::pin(async move {
                    let (delay, active, peak) = {
                        let data = state.lock().unwrap();
                        (data.delay_ms, data.active.clone(), data.peak.clone())
                    };
                    let count = active.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    let _in_flight = InFlight(active);
                    peak.fetch_max(count, std::sync::atomic::Ordering::SeqCst);
                    if delay != 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                    }
                    let mut data = state.lock().unwrap();
                    ($body)(&mut *data, req.into_inner()).map(tonic::Response::new)
                })
            }
        }
        Ok(tonic::server::Grpc::new(tonic_prost::ProstCodec::default())
            .unary(Method($state), $req)
            .await)
    }};
}

#[path = "../watcher/batch_tests.rs"]
mod watcher_batch_tests;
impl tonic::codegen::Service<tonic::codegen::http::Request<tonic::body::Body>> for Server {
    type Response = tonic::codegen::http::Response<tonic::body::Body>;
    type Error = std::convert::Infallible;
    type Future = tonic::codegen::BoxFuture<Self::Response, Self::Error>;
    fn poll_ready(
        &mut self,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::result::Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }
    fn call(&mut self, req: tonic::codegen::http::Request<tonic::body::Body>) -> Self::Future {
        let state = self.0.clone();
        Box::pin(async move {
            match req.uri().path().rsplit('/').next().unwrap() {
                "GetLightdInfo" => unary!(
                    req,
                    state,
                    Empty,
                    LightdInfo,
                    |d: &mut Data, _: Empty| Ok::<_, tonic::Status>(LightdInfo {
                        chain_name: "test".into(),
                        consensus_branch_id: "77190ad9".into(),
                        block_height: d.tip.into()
                    })
                ),
                "GetBlock" => unary!(
                    req,
                    state,
                    BlockId,
                    CompactBlock,
                    |d: &mut Data, q: BlockId| {
                        let h = q.height as u32;
                        if let Some((false, height, code)) = d.fail {
                            if height == h {
                                return Err(tonic::Status::new(code, "fixture failure"));
                            }
                        }
                        let reads = d.reads.entry(h).or_default();
                        *reads += 1;
                        if d.reorg == Some((h, *reads)) {
                            d.blocks.get_mut(&h).unwrap().hash[31] ^= 1;
                        }
                        d.blocks
                            .get(&h)
                            .cloned()
                            .ok_or_else(|| tonic::Status::not_found("gap"))
                    }
                ),
                "GetTreeState" => unary!(
                    req,
                    state,
                    BlockId,
                    TreeState,
                    |d: &mut Data, q: BlockId| {
                        if !q.hash.is_empty() {
                            if q.height != 0 || q.hash.len() != 32 {
                                return Err(tonic::Status::invalid_argument("hash selector"));
                            }
                            d.hash_requests.push(q.hash.clone());
                            return d
                                .hash_trees
                                .get(&q.hash)
                                .cloned()
                                .ok_or_else(|| tonic::Status::not_found("hash unavailable"));
                        }
                        if let Some((true, h, code)) = d.fail {
                            if h == q.height as u32 {
                                return Err(tonic::Status::new(code, "fixture failure"));
                            }
                        }
                        d.trees
                            .get(&(q.height as u32))
                            .cloned()
                            .ok_or_else(|| tonic::Status::not_found("gap"))
                    }
                ),
                path => panic!("unexpected RPC (including forbidden GetTransaction): {path}"),
            }
        })
    }
}
struct Running {
    client: LightwalletClient,
    state: Arc<Mutex<Data>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Running {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn serve(data: Data) -> Running {
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", socket.local_addr().unwrap());
    let state = Arc::new(Mutex::new(data));
    let service = Server(state.clone());
    let task = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(service)
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(socket))
            .await
            .unwrap();
    });
    let client = LightwalletClient::connect(&endpoint).await.unwrap();
    Running {
        client,
        state,
        task,
    }
}
#[tokio::test]
async fn complete_negative_is_bounded_and_restartable() {
    let attempts = [prepared(LOWER_HEIGHT + 1, 1)];
    let end = LOWER_HEIGHT + 4;
    let server = serve(data(end, None)).await;
    let initial = start(&server.client, &attempts).await.unwrap();
    assert!(!initial.complete_absence());
    let progress = advance(&server.client, &attempts, &initial, 2)
        .await
        .unwrap();
    assert_eq!(progress.cursor_height(), LOWER_HEIGHT + 1);
    assert!(!progress.complete_absence());
    let restored: History =
        serde_json::from_slice(&serde_json::to_vec(&progress).unwrap()).unwrap();
    assert_eq!(restored, progress);
    let complete = advance(&server.client, &attempts, &restored, 16)
        .await
        .unwrap();
    assert!(complete.complete_absence());
    revalidate(&server.client, &attempts, &complete)
        .await
        .unwrap();
    assert_eq!(initial.cursor_height(), LOWER_HEIGHT - 1);
    assert!(
        advance(&server.client, &attempts, &complete, 0)
            .await
            .is_err()
    );
    assert!(
        advance(&server.client, &attempts, &complete, 17)
            .await
            .is_err()
    );
}
#[tokio::test]
async fn inclusion_at_inclusive_expiry_blocks_absence_for_all_attempts() {
    let attempts = [prepared(LOWER_HEIGHT + 1, 1), prepared(LOWER_HEIGHT + 2, 2)];
    let server = serve(data(
        LOWER_HEIGHT + 5,
        Some((LOWER_HEIGHT + 1, compact(&attempts[0]))),
    ))
    .await;
    let initial = start(&server.client, &attempts).await.unwrap();
    let complete = advance(&server.client, &attempts, &initial, 16)
        .await
        .unwrap();
    assert_eq!(complete.cursor_height(), complete.checkpoint_height());
    assert!(!complete.complete_absence());
    assert_eq!(complete.inclusions()[0].txid, attempts[0].txid);
}
#[tokio::test]
async fn burial_and_signed_identity_validation_fail_closed() {
    let expiry = LOWER_HEIGHT + 1;
    let attempts = [prepared(expiry, 1)];
    let server = serve(data(expiry + 3, None)).await;
    for tip in expiry..expiry + 3 {
        server.state.lock().unwrap().tip = tip;
        assert!(start(&server.client, &attempts).await.is_err());
    }
    server.state.lock().unwrap().tip = expiry + 3;
    assert!(start(&server.client, &attempts).await.is_ok());
    for expiry in [0, LOWER_HEIGHT - 1, u32::MAX - 2, u32::MAX] {
        assert!(validate_attempts(&[prepared(expiry, 1)]).is_err());
    }
    let mut attempt = prepared(expiry, 1);
    attempt.bytes.push(0);
    assert!(validate_attempts(&[attempt]).is_err());
    let mut attempt = prepared(expiry, 1);
    attempt.txid[0] ^= 1;
    assert!(validate_attempts(&[attempt]).is_err());
}
#[tokio::test]
async fn gaps_rpc_errors_and_malformed_data_preserve_original_progress() {
    for case in 0..13 {
        let attempts = [prepared(LOWER_HEIGHT + 1, 1)];
        let end = LOWER_HEIGHT + 4;
        let server = serve(data(end, Some((LOWER_HEIGHT, compact(&attempts[0]))))).await;
        let initial = start(&server.client, &attempts).await.unwrap();
        {
            let mut d = server.state.lock().unwrap();
            let h = LOWER_HEIGHT;
            match case {
                0 => {
                    d.blocks.remove(&h);
                }
                1 => d.blocks.get_mut(&h).unwrap().chain_metadata = None,
                2 => d.blocks.get_mut(&h).unwrap().prev_hash[0] ^= 1,
                3 => d
                    .blocks
                    .get_mut(&h)
                    .unwrap()
                    .hash
                    .pop()
                    .map(|_| ())
                    .unwrap(),
                4 => d.blocks.get_mut(&h).unwrap().vtx[0]
                    .ironwood_actions
                    .clear(),
                5 => d.blocks.get_mut(&h).unwrap().vtx[0].ironwood_actions[0]
                    .cmx
                    .pop()
                    .map(|_| ())
                    .unwrap(),
                6 => {
                    d.trees.remove(&h);
                }
                7 => d.trees.get_mut(&h).unwrap().ironwood_tree.clear(),
                8 => d.fail = Some((false, h, tonic::Code::NotFound)),
                9 => d.fail = Some((false, h, tonic::Code::Unavailable)),
                10 => d.fail = Some((true, h, tonic::Code::DeadlineExceeded)),
                11 => d.blocks.get_mut(&h).unwrap().height += 1,
                12 => d.trees.get_mut(&h).unwrap().hash = hex::encode([9; 32]),
                _ => unreachable!(),
            }
        }
        assert!(
            advance(&server.client, &attempts, &initial, 16)
                .await
                .is_err(),
            "case {case}"
        );
        assert_eq!(initial.cursor_height(), LOWER_HEIGHT - 1);
        assert!(!initial.complete_absence());
    }
}
#[tokio::test]
async fn canonical_changes_before_and_during_batch_invalidate_evidence() {
    for during in [false, true] {
        let attempts = [prepared(LOWER_HEIGHT, 1)];
        let end = LOWER_HEIGHT + 3;
        let server = serve(data(end, None)).await;
        let initial = start(&server.client, &attempts).await.unwrap();
        {
            let mut d = server.state.lock().unwrap();
            if during {
                let count = *d.reads.get(&end).unwrap();
                d.reorg = Some((end, count + 3));
            } else {
                d.blocks.get_mut(&end).unwrap().hash[31] ^= 1;
            }
        }
        let error = advance(&server.client, &attempts, &initial, 16)
            .await
            .unwrap_err();
        assert!(error.downcast_ref::<HistoryReorg>().is_some(), "{error:?}");
    }
    let attempts = [prepared(LOWER_HEIGHT, 1)];
    let end = LOWER_HEIGHT + 3;
    let server = serve(data(end, None)).await;
    let h = start(&server.client, &attempts).await.unwrap();
    let h = advance(&server.client, &attempts, &h, 16).await.unwrap();
    server
        .state
        .lock()
        .unwrap()
        .blocks
        .get_mut(&end)
        .unwrap()
        .hash[0] ^= 1;
    assert!(
        revalidate(&server.client, &attempts, &h)
            .await
            .unwrap_err()
            .downcast_ref::<HistoryReorg>()
            .is_some()
    );
    server.state.lock().unwrap().tip = end - 1;
    assert!(
        revalidate(&server.client, &attempts, &h)
            .await
            .unwrap_err()
            .downcast_ref::<HistoryReorg>()
            .is_some()
    );
}
#[tokio::test]
async fn after_expiry_inclusion_is_inconsistent_at_every_suffix_height() {
    for offset in 1..=3 {
        let attempts = [prepared(LOWER_HEIGHT, 1)];
        let server = serve(data(
            LOWER_HEIGHT + 3,
            Some((LOWER_HEIGHT + offset, compact(&attempts[0]))),
        ))
        .await;
        let h = start(&server.client, &attempts).await.unwrap();
        assert!(advance(&server.client, &attempts, &h, 16).await.is_err());
    }
}
#[tokio::test]
async fn omissions_unrelated_to_issued_identity_and_late_failures_are_inconclusive() {
    let attempts = [prepared(LOWER_HEIGHT + 1, 1)];
    let other = prepared(LOWER_HEIGHT + 1, 2);
    let server = serve(data(
        LOWER_HEIGHT + 4,
        Some((LOWER_HEIGHT, compact(&other))),
    ))
    .await;
    let initial = start(&server.client, &attempts).await.unwrap();
    // This is an unrelated transaction, so only complete commitment replay detects omission.
    server
        .state
        .lock()
        .unwrap()
        .blocks
        .get_mut(&LOWER_HEIGHT)
        .unwrap()
        .vtx
        .clear();
    assert!(
        advance(&server.client, &attempts, &initial, 16)
            .await
            .is_err()
    );
    let server = serve(data(LOWER_HEIGHT + 4, None)).await;
    let initial = start(&server.client, &attempts).await.unwrap();
    server.state.lock().unwrap().fail = Some((false, LOWER_HEIGHT + 2, tonic::Code::Internal));
    assert!(
        advance(&server.client, &attempts, &initial, 16)
            .await
            .is_err()
    );
    assert_eq!(initial.cursor_height(), LOWER_HEIGHT - 1);
    server.state.lock().unwrap().fail = None;
    assert!(
        advance(&server.client, &attempts, &initial, 16)
            .await
            .unwrap()
            .complete_absence()
    );
}
#[tokio::test]
async fn activation_baseline_and_attempt_set_are_mandatory() {
    let attempts = [prepared(LOWER_HEIGHT, 1)];
    let baseline = LOWER_HEIGHT - 1;
    for malformed in [false, true] {
        let mut d = data(LOWER_HEIGHT + 3, None);
        if malformed {
            d.blocks.get_mut(&baseline).unwrap().chain_metadata = None;
        } else {
            d.trees.remove(&baseline);
        }
        let server = serve(d).await;
        assert!(start(&server.client, &attempts).await.is_err());
    }
    let server = serve(data(LOWER_HEIGHT + 3, None)).await;
    let h = start(&server.client, &attempts).await.unwrap();
    let changed = [prepared(LOWER_HEIGHT, 2)];
    assert!(revalidate(&server.client, &changed, &h).await.is_err());
    assert!(h.matches_attempts(&changed).is_err());
    assert!(validate_attempts(&[]).is_err());
    assert!(validate_attempts(&[prepared(LOWER_HEIGHT, 1), prepared(LOWER_HEIGHT, 1)]).is_err());
    // A persisted frontier is checked again against the provider, not blindly trusted.
    let mut h = h;
    h.frontier.clear();
    assert!(revalidate(&server.client, &attempts, &h).await.is_err());
}
#[tokio::test]
async fn transaction_order_duplicates_and_field_encodings_are_checked() {
    for case in 0..8 {
        let attempts = [prepared(LOWER_HEIGHT + 1, 1)];
        let other = prepared(LOWER_HEIGHT + 1, 2);
        let server = serve(data(
            LOWER_HEIGHT + 4,
            Some((LOWER_HEIGHT, compact(&other))),
        ))
        .await;
        let h = start(&server.client, &attempts).await.unwrap();
        {
            let mut d = server.state.lock().unwrap();
            let b = d.blocks.get_mut(&LOWER_HEIGHT).unwrap();
            match case {
                0 => {
                    let mut tx = b.vtx[0].clone();
                    tx.index = 1;
                    b.vtx.push(tx);
                }
                1 => {
                    let mut tx = b.vtx[0].clone();
                    tx.index += 1;
                    b.vtx.push(tx);
                }
                2 => b.vtx[0].txid.clear(),
                3 => b.vtx[0].ironwood_actions[0].nullifier = vec![255; 32],
                4 => b.vtx[0].ironwood_actions[0].cmx = vec![255; 32],
                5 => b.vtx[0].ironwood_actions[0].ciphertext.clear(),
                6 => b.vtx[0].ironwood_actions[0].ephemeral_key.clear(),
                7 => {
                    let a = b.vtx[0].ironwood_actions[0].clone();
                    b.vtx[0].ironwood_actions.push(a);
                }
                _ => unreachable!(),
            }
        }
        assert!(
            advance(&server.client, &attempts, &h, 16).await.is_err(),
            "case {case}"
        );
    }
}

#[tokio::test]
async fn nonempty_baseline_is_restored_and_extended_across_restart() {
    let attempts = [prepared(LOWER_HEIGHT + 1, 1)];
    let prior = prepared(LOWER_HEIGHT + 1, 3);
    let later = prepared(LOWER_HEIGHT + 1, 2);
    let server = serve(data_with_baseline(
        LOWER_HEIGHT + 4,
        Some((LOWER_HEIGHT, compact(&later))),
        Some(compact(&prior)),
    ))
    .await;
    let initial = start(&server.client, &attempts).await.unwrap();
    assert_eq!(decode_tree(&initial.frontier).unwrap().size(), 1);
    assert!(!initial.complete_absence());
    let partial = advance(&server.client, &attempts, &initial, 1)
        .await
        .unwrap();
    assert_eq!(decode_tree(&partial.frontier).unwrap().size(), 2);
    let restored: History = serde_json::from_slice(&serde_json::to_vec(&partial).unwrap()).unwrap();
    revalidate(&server.client, &attempts, &restored)
        .await
        .unwrap();
    let complete = advance(&server.client, &attempts, &restored, 16)
        .await
        .unwrap();
    assert_eq!(decode_tree(&complete.frontier).unwrap().size(), 2);
    assert!(complete.complete_absence());
}
