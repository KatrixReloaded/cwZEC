use super::*;
use std::sync::atomic::AtomicUsize;

#[tokio::test]
async fn header_fetches_overlap_with_a_bound_and_return_ordered_history() {
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let (running, maximum) = (active.clone(), peak.clone());
    let fixture = Fixture::new(move |request| {
        if request["method"] == "eth_getBlockByNumber" {
            let count = running.fetch_add(1, Ordering::SeqCst) + 1;
            maximum.fetch_max(count, Ordering::SeqCst);
            let number = u64::from_str_radix(
                request["params"][0]
                    .as_str()
                    .unwrap()
                    .trim_start_matches("0x"),
                16,
            )
            .unwrap();
            thread::sleep(std::time::Duration::from_millis(10 + (number % 3) * 10));
            running.fetch_sub(1, Ordering::SeqCst);
        }
        normal(request)
    });
    let batch = fixture.watcher.scan(reference(0), 17, &[]).await.unwrap();
    assert_eq!(batch.blocks.len(), 17);
    for (number, block) in (1..=17).zip(batch.blocks) {
        assert_eq!(block.reference, reference(number));
        assert_eq!(block.parent_hash, hash(number - 1));
    }
    assert_eq!(active.load(Ordering::SeqCst), 0);
    let peak = peak.load(Ordering::SeqCst);
    assert!(peak > 1 && peak <= HEADER_CONCURRENCY, "peak={peak}");
}

#[tokio::test]
async fn missing_concurrent_header_fails_the_scan_and_retry_is_complete() {
    let fail = Arc::new(AtomicBool::new(true));
    let failing = fail.clone();
    let fixture = Fixture::new(move |request| {
        if failing.load(Ordering::SeqCst)
            && request["method"] == "eth_getBlockByNumber"
            && request["params"][0] == "0x9"
        {
            Value::Null
        } else {
            normal(request)
        }
    });
    assert!(fixture.watcher.scan(reference(0), 17, &[]).await.is_err());
    fail.store(false, Ordering::SeqCst);
    let batch = fixture.watcher.scan(reference(0), 17, &[]).await.unwrap();
    assert_eq!(batch.blocks.len(), 17);
    assert_eq!(batch.end, reference(17));
}
