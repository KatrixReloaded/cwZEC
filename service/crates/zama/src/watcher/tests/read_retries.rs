use super::super::rpc::read_with_retry;
use super::*;
use alloy::transports::TransportErrorKind;

#[tokio::test]
async fn http_429_retries_but_other_http_errors_stop_immediately() {
    for (status, expected_calls) in [(429, 2), (403, 1), (503, 1)] {
        let mut calls = 0;
        let result = read_with_retry("RPC block lookup failed", || {
            calls += 1;
            let response = if calls == 1 {
                Err(TransportErrorKind::http_error(
                    status,
                    "private response".into(),
                ))
            } else {
                Ok(42)
            };
            async move { response }
        })
        .await;
        assert_eq!(calls, expected_calls);
        if status == 429 {
            assert_eq!(result.unwrap(), 42);
        } else {
            assert_eq!(result.unwrap_err().to_string(), "RPC block lookup failed");
        }
    }
}

#[tokio::test]
async fn scan_recovers_throttled_chain_header_and_receipt_reads() {
    let seen = Arc::new(Mutex::new(BTreeSet::new()));
    let first = seen.clone();
    let fixture = Fixture::with_response(move |request| {
        let method = request["method"].as_str().unwrap();
        if matches!(
            method,
            "eth_chainId" | "eth_getBlockByNumber" | "eth_getTransactionReceipt"
        ) && first.lock().unwrap().insert(method.to_owned())
        {
            return json!({"jsonrpc":"2.0", "id":request["id"], "error":{"code":429,"message":"rate limited"}});
        }
        let result = if method == "eth_getTransactionReceipt" {
            receipt(true)
        } else {
            normal(request)
        };
        json!({"jsonrpc":"2.0", "id":request["id"], "result":result})
    });
    let batch = fixture
        .watcher
        .scan(reference(0), 17, &[hash(50)])
        .await
        .unwrap();
    assert_eq!(batch.end, reference(17));
    assert_eq!(batch.blocks.len(), 17);
    assert_eq!(batch.receipts.len(), 1);
    let requests = fixture.requests.lock().unwrap();
    for method in [
        "eth_chainId",
        "eth_getBlockByNumber",
        "eth_getTransactionReceipt",
    ] {
        let attempts: Vec<_> = requests
            .iter()
            .filter(|r| r["method"] == method)
            .take(2)
            .collect();
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0]["params"], attempts[1]["params"]);
    }
}

#[tokio::test]
async fn retry_does_not_hide_noncanonical_headers() {
    let throttled = Arc::new(AtomicBool::new(false));
    let flag = throttled.clone();
    let fixture = Fixture::with_response(move |request| {
        if request["method"] == "eth_getBlockByNumber" && request["params"][0] == "0x9" {
            if !flag.swap(true, Ordering::SeqCst) {
                return json!({"jsonrpc":"2.0", "id":request["id"], "error":{"code":429,"message":"rate limited"}});
            }
            let mut changed = normal(request);
            changed["parentHash"] = json!(hash(99));
            return json!({"jsonrpc":"2.0", "id":request["id"], "result":changed});
        }
        json!({"jsonrpc":"2.0", "id":request["id"], "result":normal(request)})
    });
    let error = fixture
        .watcher
        .scan(reference(0), 17, &[])
        .await
        .err()
        .unwrap();
    assert_eq!(error.to_string(), "nonconsecutive canonical headers");
}
