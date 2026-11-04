//! Reuse the strict gRPC history fixture; unexpected full-transaction calls fail.
use super::*;
use crate::{
    core::{Account, WalletState},
    watcher::{Reorg, SCAN_BATCH_BLOCKS, Watcher},
};
use std::{sync::atomic::Ordering, time::Instant};

async fn wallet(watcher: &Watcher) -> WalletState {
    WalletState {
        account: Account {
            birthday: LOWER_HEIGHT,
            fvk: fixture::viewing_key().to_bytes(),
            use_qsk: false,
        },
        checkpoint: watcher.initial_checkpoint(LOWER_HEIGHT - 1).await.unwrap(),
        notes: vec![],
    }
}

#[tokio::test]
async fn bounded_prefetch_overlaps_rpc_and_matches_sequential_scanning() {
    let end = LOWER_HEIGHT + SCAN_BATCH_BLOCKS - 1;
    let server = serve(data(end, None)).await;
    let watcher = Watcher::new(server.client.clone());
    let mut sequential = wallet(&watcher).await;
    let mut batched = wallet(&watcher).await;
    {
        let mut d = server.state.lock().unwrap();
        d.delay_ms = 20;
        d.peak.store(0, Ordering::SeqCst);
    }
    let started = Instant::now();
    let blocks = watcher.fetch_batch(LOWER_HEIGHT, end).await.unwrap();
    let batch_time = started.elapsed();
    let peak = server.state.lock().unwrap().peak.load(Ordering::SeqCst);
    assert!(
        peak > 2 && peak <= 2 * SCAN_BATCH_BLOCKS as usize,
        "peak={peak}"
    );
    assert_eq!(blocks.len(), SCAN_BATCH_BLOCKS as usize);
    assert!(watcher.fetch_batch(LOWER_HEIGHT, end + 1).await.is_err());
    assert!(watcher.fetch_batch(end, LOWER_HEIGHT).await.is_err());
    for (i, (block, tree)) in blocks.iter().enumerate() {
        assert_eq!(block.height, u64::from(LOWER_HEIGHT) + i as u64);
        let update = watcher
            .scan_prefetched(&batched, block, tree)
            .await
            .unwrap();
        assert!(update.received.is_empty() && update.spent_nullifiers.is_empty());
        batched.checkpoint = update.checkpoint;
    }
    let started = Instant::now();
    while sequential.checkpoint.height < end {
        sequential.checkpoint = watcher.scan_next(&sequential).await.unwrap().checkpoint;
    }
    println!(
        "8-block RPC fixture: batched={batch_time:?}, sequential={:?}, peak={peak}",
        started.elapsed()
    );
    assert!(batched.checkpoint == sequential.checkpoint);
}

#[tokio::test]
async fn failed_prefetch_returns_no_partial_batch_and_retry_is_complete() {
    let end = LOWER_HEIGHT + SCAN_BATCH_BLOCKS - 1;
    let mut d = data(end, None);
    d.fail = Some((true, LOWER_HEIGHT + 3, tonic::Code::Unavailable));
    let server = serve(d).await;
    let watcher = Watcher::new(server.client.clone());
    assert!(watcher.fetch_batch(LOWER_HEIGHT, end).await.is_err());
    server.state.lock().unwrap().fail = None;
    let blocks = watcher.fetch_batch(LOWER_HEIGHT, end).await.unwrap();
    assert_eq!(blocks.len(), SCAN_BATCH_BLOCKS as usize);
    assert_eq!(blocks.last().unwrap().0.height, u64::from(end));
}

#[tokio::test]
async fn prefetched_data_still_rejects_gaps_reorgs_and_bad_tree_evidence() {
    let server = serve(data(LOWER_HEIGHT + 1, None)).await;
    let watcher = Watcher::new(server.client.clone());
    let state = wallet(&watcher).await;
    let mut blocks = watcher
        .fetch_batch(LOWER_HEIGHT, LOWER_HEIGHT + 1)
        .await
        .unwrap();
    assert!(
        watcher
            .scan_prefetched(&state, &blocks[1].0, &blocks[1].1)
            .await
            .is_err()
    );
    let (block, tree) = &mut blocks[0];
    block.prev_hash[0] ^= 1;
    assert!(
        watcher
            .scan_prefetched(&state, block, tree)
            .await
            .err()
            .unwrap()
            .downcast_ref::<Reorg>()
            .is_some()
    );
    block.prev_hash[0] ^= 1;
    tree.hash = "00".repeat(32);
    assert!(watcher.scan_prefetched(&state, block, tree).await.is_err());
    tree.hash = {
        let mut h = block.hash.clone();
        h.reverse();
        hex::encode(h)
    };
    let mut wrong_tree = CommitmentTree::<MerkleHashOrchard, 32>::empty();
    wrong_tree
        .append(MerkleHashOrchard::from_cmx(
            &compact_action(&compact(&prepared(LOWER_HEIGHT + 3, 5)).ironwood_actions[0])
                .unwrap()
                .cmx(),
        ))
        .unwrap();
    let mut bytes = vec![];
    write_commitment_tree(&wrong_tree, &mut bytes).unwrap();
    tree.ironwood_tree = hex::encode(bytes);
    block
        .chain_metadata
        .as_mut()
        .unwrap()
        .ironwood_commitment_tree_size = 1;
    assert!(watcher.scan_prefetched(&state, block, tree).await.is_err());
}

fn stale_height_tree(data: &mut Data, height: u32) -> Vec<u8> {
    let good = data.trees[&height].clone();
    let selector = hex::decode(&good.hash).unwrap();
    data.hash_trees.insert(selector.clone(), good);
    data.trees.get_mut(&height).unwrap().hash = "ff".repeat(32);
    selector
}

#[tokio::test]
async fn tree_hash_retry_preserves_sequential_and_prefetched_scans() {
    let server = serve(data(LOWER_HEIGHT, None)).await;
    let watcher = Watcher::new(server.client.clone());
    let state = wallet(&watcher).await;
    let expected = watcher.scan_next(&state).await.unwrap();
    assert!(server.state.lock().unwrap().hash_requests.is_empty());
    let selector = stale_height_tree(&mut server.state.lock().unwrap(), LOWER_HEIGHT);

    let sequential = watcher.scan_next(&state).await.unwrap();
    let batch = watcher
        .fetch_batch(LOWER_HEIGHT, LOWER_HEIGHT)
        .await
        .unwrap();
    let prefetched = watcher
        .scan_prefetched(&state, &batch[0].0, &batch[0].1)
        .await
        .unwrap();
    assert!(sequential.checkpoint == expected.checkpoint);
    assert!(prefetched.checkpoint == expected.checkpoint);
    assert_eq!(
        server.state.lock().unwrap().hash_requests,
        vec![selector.clone(), selector]
    );

    // Startup checkpoints use the same strict recovery path.
    let selector = stale_height_tree(&mut server.state.lock().unwrap(), LOWER_HEIGHT - 1);
    assert!(watcher.initial_checkpoint(LOWER_HEIGHT - 1).await.unwrap() == state.checkpoint);
    assert_eq!(
        server.state.lock().unwrap().hash_requests.last(),
        Some(&selector)
    );
}

#[tokio::test]
async fn tree_hash_retry_rejects_invalid_identity_frontier_and_unavailable_hash() {
    for case in 0..7 {
        let baseline = compact(&prepared(LOWER_HEIGHT + 3, 5));
        let server = serve(data_with_baseline(LOWER_HEIGHT, None, Some(baseline))).await;
        let watcher = Watcher::new(server.client.clone());
        let state = wallet(&watcher).await;
        {
            let mut data = server.state.lock().unwrap();
            let selector = stale_height_tree(&mut data, LOWER_HEIGHT);
            let retry = data.hash_trees.get_mut(&selector).unwrap();
            match case {
                0 => retry.hash = "ff".repeat(32),
                1 => retry.height += 1,
                2 => retry.network = "main".into(),
                3 => retry.ironwood_tree = "not hex".into(),
                4 => {
                    // Valid encoding and size, but a different root: replay must reject it.
                    let mut tree = CommitmentTree::<MerkleHashOrchard, 32>::empty();
                    let tx = compact(&prepared(LOWER_HEIGHT + 3, 6));
                    tree.append(MerkleHashOrchard::from_cmx(
                        &compact_action(&tx.ironwood_actions[0]).unwrap().cmx(),
                    ))
                    .unwrap();
                    let mut bytes = vec![];
                    write_commitment_tree(&tree, &mut bytes).unwrap();
                    retry.ironwood_tree = hex::encode(bytes);
                }
                5 => {
                    data.hash_trees.clear();
                }
                6 => {
                    data.trees.get_mut(&LOWER_HEIGHT).unwrap().hash = "not hex".into();
                }
                _ => unreachable!(),
            }
        }
        assert!(watcher.scan_next(&state).await.is_err(), "case {case}");
        assert_eq!(
            server.state.lock().unwrap().hash_requests.len(),
            usize::from(case != 6)
        );
    }
}

#[tokio::test]
async fn tree_hash_retry_preserves_history_replay_and_reorg_checks() {
    let attempts = [prepared(LOWER_HEIGHT, 5)];
    let server = serve(data(LOWER_HEIGHT + 3, None)).await;
    let initial = start(&server.client, &attempts).await.unwrap();
    let selector = stale_height_tree(&mut server.state.lock().unwrap(), LOWER_HEIGHT + 1);
    let next = advance(&server.client, &attempts, &initial, 2)
        .await
        .unwrap();
    assert_eq!(next.cursor_height(), LOWER_HEIGHT + 1);
    assert!(
        server
            .state
            .lock()
            .unwrap()
            .hash_requests
            .iter()
            .all(|s| s == &selector)
    );
    assert!(!server.state.lock().unwrap().hash_requests.is_empty());

    server
        .state
        .lock()
        .unwrap()
        .blocks
        .get_mut(&(LOWER_HEIGHT + 1))
        .unwrap()
        .hash[0] ^= 1;
    assert!(revalidate(&server.client, &attempts, &next).await.is_err());
}
