//! Explicit local CPU measurement; no RPC, database, secrets, or proof generation.
use super::*;

#[test]
#[ignore = "explicit scanner CPU timing; run with --release --nocapture"]
fn scanner_cpu_profile() {
    let mut state = base_state();
    let tx = full(
        (1..=8)
            .map(|i| action(Scope::External, [i; 32], 50_000))
            .collect(),
    );
    let mut block = make_block(&state, &tx);
    let update = scan(&state, &block, tx).unwrap();
    apply(&mut state, update);
    block.height += 1;
    block.prev_hash = state.checkpoint.hash.to_vec();
    block.hash = vec![9; 32];
    block.vtx.clear();
    let started = std::time::Instant::now();
    for _ in 0..32 {
        let update = Scan::start(&state, &block)
            .unwrap()
            .finish(&block, &HashMap::new())
            .unwrap();
        assert_eq!(update.witnesses.len(), 8);
        assert_eq!(update.checkpoint.frontier, state.checkpoint.frontier);
        std::hint::black_box(update);
    }
    eprintln!(
        "scanner CPU: 32 empty blocks, 8 live notes, elapsed={:?}",
        started.elapsed()
    );
}
