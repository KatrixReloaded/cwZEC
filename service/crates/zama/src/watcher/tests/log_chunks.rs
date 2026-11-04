use super::*;
use std::sync::atomic::AtomicUsize;

fn height(value: &Value) -> u64 {
    u64::from_str_radix(value.as_str().unwrap().trim_start_matches("0x"), 16).unwrap()
}

#[tokio::test]
async fn log_chunks_cover_the_full_range_with_filters_and_tail() {
    let fixture = Fixture::new(|request| {
        if request["method"] == "eth_getLogs" {
            let filter = &request["params"][0];
            let first = height(&filter["fromBlock"]);
            let last = height(&filter["toBlock"]);
            assert!(last - first < 10);
            let mut event = minted();
            event.block_number = Some(first);
            json!([event])
        } else {
            normal(request)
        }
    });
    let logs = fixture.watcher.logs(1, 128).await.unwrap();
    assert_eq!(logs.len(), 13);
    let requests = fixture.requests.lock().unwrap();
    let mut next = 1;
    for request in requests.iter().filter(|r| r["method"] == "eth_getLogs") {
        let filter = &request["params"][0];
        assert_eq!(height(&filter["fromBlock"]), next);
        next = height(&filter["toBlock"]) + 1;
        assert_eq!(filter["topics"][0].as_array().unwrap().len(), 5);
        assert!(filter["address"] == json!(contract()) || filter["address"] == json!([contract()]));
    }
    assert_eq!(next, 129);
    assert_eq!(logs.last().unwrap().block_number, Some(121));
}

#[tokio::test]
async fn failed_log_chunk_rejects_batch_and_retry_recovers_all_events() {
    let fail = Arc::new(AtomicBool::new(true));
    let failing = fail.clone();
    let fixture = Fixture::new(move |request| {
        if request["method"] == "eth_getLogs" {
            let first = height(&request["params"][0]["fromBlock"]);
            if first == 11 && failing.load(Ordering::SeqCst) {
                return Value::Null; // Malformed response, not evidence of no events.
            }
            if first == 1 {
                json!([minted()])
            } else {
                json!([])
            }
        } else if request["method"] == "eth_getTransactionReceipt" {
            receipt(true)
        } else {
            normal(request)
        }
    });
    assert!(fixture.watcher.scan(reference(0), 21, &[]).await.is_err());
    fail.store(false, Ordering::SeqCst);
    let batch = fixture.watcher.scan(reference(0), 21, &[]).await.unwrap();
    assert_eq!(batch.end, reference(21));
    assert_eq!(batch.events.len(), 1);
    assert_eq!(batch.receipts.len(), 1);
}

#[tokio::test]
async fn log_limit_applies_across_chunks() {
    let fixture = Fixture::new(|request| {
        if request["method"] == "eth_getLogs" {
            json!(vec![minted(); MAX_LOGS])
        } else {
            normal(request)
        }
    });
    assert_eq!(
        fixture.watcher.logs(1, 11).await.unwrap_err().to_string(),
        "RPC log bound exceeded"
    );
}

#[tokio::test]
async fn throttled_log_query_retries_identical_filter_then_completes() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let fixture = Fixture::with_response(move |request| {
        if count.fetch_add(1, Ordering::SeqCst) == 0 {
            json!({"jsonrpc":"2.0", "id":request["id"], "error":{"code":429,"message":"rate limited"}})
        } else {
            json!({"jsonrpc":"2.0", "id":request["id"], "result":[]})
        }
    });
    assert!(fixture.watcher.logs(1, 10).await.unwrap().is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let requests = fixture.requests.lock().unwrap();
    assert_eq!(requests[0]["params"], requests[1]["params"]);
}

#[tokio::test]
async fn persistent_throttling_is_bounded_and_other_errors_are_not_retried() {
    for (code, expected_calls) in [(429, 4), (4444, 1)] {
        let fixture = Fixture::with_response(move |request| {
            json!({"jsonrpc":"2.0", "id":request["id"], "error":{
                "code":code, "message":"private remote details https://private.invalid/key"}})
        });
        let error = fixture.watcher.logs(1, 10).await.unwrap_err();
        assert_eq!(error.to_string(), "RPC log lookup failed");
        assert_eq!(fixture.requests.lock().unwrap().len(), expected_calls);
    }
}

#[test]
fn rpc_failure_discards_http_body_and_credentials() {
    let error = alloy::transports::TransportErrorKind::http_error(
        429,
        "private body https://private.invalid/key".into(),
    );
    let safe = super::super::rpc::rpc_failure("RPC log lookup failed", error);
    assert_eq!(format!("{safe:?}"), "RPC log lookup failed");
}
