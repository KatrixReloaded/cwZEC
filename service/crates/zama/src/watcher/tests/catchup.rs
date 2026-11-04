use super::*;

fn sparse_batch() -> ScanBatch {
    ScanBatch {
        chain_id: 1,
        contract: contract(),
        previous: reference(0),
        end: reference(SCAN_BLOCKS),
        blocks: (SCAN_BLOCKS - RETAIN_BLOCKS..=SCAN_BLOCKS)
            .map(|n| Block {
                reference: reference(n),
                parent_hash: hash(n - 1),
            })
            .collect(),
        events: vec![],
        receipts: vec![],
    }
}

#[test]
fn catchup_header_validation_requires_complete_rewind_window_and_event_headers() {
    let mut batch = sparse_batch();
    batch.validate_headers().unwrap();
    let event = decode_log(&minted(), contract(), 0, SCAN_BLOCKS).unwrap();
    batch.events.push(event);
    assert!(batch.validate_headers().is_err());
    batch.blocks.insert(
        0,
        Block {
            reference: reference(2),
            parent_hash: hash(1),
        },
    );
    batch.validate_headers().unwrap();
    for index in [1, 64, batch.blocks.len() - 1] {
        let removed = batch.blocks.remove(index);
        assert!(batch.validate_headers().is_err());
        batch.blocks.insert(index, removed);
    }
    batch.blocks[64].parent_hash = hash(99);
    assert!(batch.validate_headers().is_err());
    batch.blocks[64].parent_hash = hash(batch.blocks[64].reference.number - 1);
    batch.blocks.push(batch.blocks[0]);
    assert!(batch.validate_headers().is_err());
}

#[test]
fn catchup_header_validation_keeps_short_scans_contiguous_and_bounds_ranges() {
    let mut batch = sparse_batch();
    batch.previous = reference(SCAN_BLOCKS - RETAIN_BLOCKS - 1);
    batch.validate_headers().unwrap();
    batch.blocks[0].parent_hash = hash(99);
    assert!(batch.validate_headers().is_err());
    let mut batch = sparse_batch();
    batch.end = reference(SCAN_BLOCKS + 1);
    assert!(batch.validate_headers().is_err());
    batch.end = batch.previous;
    assert!(batch.validate_headers().is_err());
    batch.blocks.clear();
    batch.validate_headers().unwrap();
}

#[tokio::test]
async fn catchup_queries_every_log_range_with_far_fewer_header_calls() {
    let fixture = Fixture::new(normal);
    let batch = fixture
        .watcher
        .scan(reference(0), SCAN_BLOCKS, &[])
        .await
        .unwrap();
    assert_eq!(batch.blocks.len() as u64, RETAIN_BLOCKS + 1);
    batch.validate_headers().unwrap();
    let requests = fixture.requests.lock().unwrap();
    let mut next = 1;
    for r in requests.iter().filter(|r| r["method"] == "eth_getLogs") {
        let height = |name: &str| {
            u64::from_str_radix(
                r["params"][0][name]
                    .as_str()
                    .unwrap()
                    .trim_start_matches("0x"),
                16,
            )
            .unwrap()
        };
        assert_eq!(height("fromBlock"), next);
        assert!(height("toBlock") - next < 10);
        next = height("toBlock") + 1;
    }
    assert_eq!(next, SCAN_BLOCKS + 1);
    assert_eq!(requests.len(), 338); // 205 log queries, 132 headers, one chain ID.
}

#[tokio::test]
async fn catchup_checks_old_event_header_and_receipt_outside_retained_window() {
    for fault in 0..3 {
        let fixture = Fixture::new(move |r| {
            if r["method"] == "eth_getLogs" && r["params"][0]["fromBlock"] == "0x1" {
                let mut event = minted();
                if fault == 1 {
                    event.block_hash = Some(hash(99));
                }
                json!([event])
            } else if r["method"] == "eth_getTransactionReceipt" {
                receipt(fault != 2)
            } else {
                normal(r)
            }
        });
        let result = fixture.watcher.scan(reference(0), SCAN_BLOCKS, &[]).await;
        if fault == 0 {
            let batch = result.unwrap();
            assert_eq!(batch.events.len(), 1);
            assert_eq!(batch.receipts.len(), 1);
            assert_eq!(batch.blocks[0].reference, reference(2));
            assert_eq!(batch.blocks.len() as u64, RETAIN_BLOCKS + 2);
        } else {
            assert!(result.is_err());
        }
    }
}

#[tokio::test]
async fn catchup_rejects_late_log_failure_and_anchor_changes_without_partial_batch() {
    for fault in 0..3 {
        let reads = std::sync::atomic::AtomicUsize::new(0);
        let fixture = Fixture::new(move |r| {
            if fault == 0 && r["method"] == "eth_getLogs" && r["params"][0]["fromBlock"] == "0x7f9"
            {
                return Value::Null;
            }
            let mut value = normal(r);
            let anchor = if fault == 1 { "0x0" } else { "0x800" };
            if fault > 0
                && r["method"] == "eth_getBlockByNumber"
                && r["params"][0] == anchor
                && reads.fetch_add(1, Ordering::SeqCst) > 0
            {
                value["hash"] = json!(hash(99));
            }
            value
        });
        assert!(
            fixture
                .watcher
                .scan(reference(0), SCAN_BLOCKS, &[])
                .await
                .is_err()
        );
    }
}
