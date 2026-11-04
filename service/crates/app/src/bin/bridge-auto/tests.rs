use super::safe_message;

#[test]
fn diagnostics_accept_only_exact_local_messages() {
    assert_eq!(safe_message("scan: commit"), Some("scan: commit"));
    for message in [
        "scan: retry tip",
        "scan: retry checkpoint hash",
        "near-tip GetBlock availability retries exhausted",
        "near-tip GetBlock retry tip changed",
        "near-tip GetBlock retry checkpoint changed",
        "RPC block lookup failed",
        "RPC log lookup failed",
        "RPC receipt lookup failed",
        "RPC chain lookup failed",
        "nonconsecutive canonical headers",
        "previous checkpoint changed during scan",
        "Zama watcher halted; manual investigation required",
        "redemption: select payout input",
        "redemption: sign payout",
        "insufficient confirmed unreserved single-note liquidity; fragmented liquidity is unsupported",
    ] {
        assert_eq!(safe_message(message), Some(message));
    }
    for private in [
        "https://private-rpc.invalid/token",
        "scan: commit: private SQL row",
        "lightwalletd GetBlock\nprivate remote details",
        "private SDK response",
        "near-tip GetBlock availability retries exhausted: private server text",
        "scan: retry checkpoint hash\nprivate remote details",
        "RPC block lookup failed: https://private-rpc.invalid/token",
        "RPC log lookup failed\nprivate remote details",
        "redemption: sign payout: private destination",
    ] {
        assert!(safe_message(private).is_none());
    }
}
