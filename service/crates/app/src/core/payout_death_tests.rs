use super::*;
use zcash::core::PreparedTransaction;

fn attempt(expiry_height: u32) -> Attempt {
    Attempt {
        operation_id: "redeem:1".into(),
        destination: "must not appear in report".into(),
        amount: 42,
        inputs: vec![],
        prepared: Some(PreparedTransaction {
            bytes: vec![99],
            txid: [1; 32],
            inputs: vec![],
            expiry_height,
            fee: 1,
        }),
        broadcast: false,
        observed_height: None,
    }
}

#[test]
fn inclusive_expiry_and_three_block_burial_never_suffice_for_death() {
    for tip in [99, 100, 101, 102, 103, 104] {
        let report = assess_attempt(
            &attempt(100),
            Some(tip),
            Some(TransactionLookup::NotFoundUnverified),
        );
        assert_eq!(
            report.reasons.contains(&DeathBlocker::ExpiryNotBuried),
            tip < 103
        );
        assert!(
            report
                .reasons
                .contains(&DeathBlocker::CompleteCanonicalHistoryUnavailable)
        );
        assert!(
            report
                .reasons
                .contains(&DeathBlocker::LookupNotFoundUnverified)
        );
        assert!(!report.recovery_enabled);
    }
    let report = assess_attempt(
        &attempt(0),
        Some(u32::MAX),
        Some(TransactionLookup::NotFoundUnverified),
    );
    assert!(report.reasons.contains(&DeathBlocker::ExpiryDisabled));
    let report = assess_attempt(&attempt(u32::MAX), Some(u32::MAX), None);
    assert!(report.reasons.contains(&DeathBlocker::ExpiryNotBuried));
}

#[test]
fn rpc_failures_missing_tip_and_incomplete_history_remain_reserved() {
    for lookup in [
        None,
        Some(TransactionLookup::RpcFailure),
        Some(TransactionLookup::EmptyResponse),
        Some(TransactionLookup::NotFoundUnverified),
        Some(TransactionLookup::Returned { height: 100 }),
        Some(TransactionLookup::Returned { height: 0 }),
    ] {
        let report = assess_attempt(&attempt(100), None, lookup);
        assert!(!report.recovery_enabled);
        assert!(report.reasons.contains(&DeathBlocker::TipUnavailable));
        assert!(
            report
                .reasons
                .contains(&DeathBlocker::CompleteCanonicalHistoryUnavailable)
        );
    }
}

#[test]
fn no_broadcast_ack_and_observed_attempts_are_both_inspected_without_secrets() {
    let mut unacknowledged = attempt(100);
    let mut observed = attempt(100);
    observed.broadcast = true;
    observed.observed_height = Some(100);
    observed.operation_id = "refund:2".into();
    unacknowledged.operation_id = "maintenance:3".into();
    for attempt in [unacknowledged, observed] {
        let report = assess_attempt(&attempt, Some(103), Some(TransactionLookup::RpcFailure));
        assert_eq!(report.operation_id, attempt.operation_id);
        assert!(!report.recovery_enabled);
        assert_eq!(
            report
                .reasons
                .contains(&DeathBlocker::JournalRecordsInclusion),
            attempt.observed_height.is_some()
        );
        let json = serde_json::to_string(&report).unwrap();
        assert!(!json.contains("must not appear"));
        assert!(!json.contains("bytes"));
    }
    let mut missing = attempt(100);
    missing.prepared = None;
    let report = assess_attempt(&missing, Some(103), None);
    assert!(
        report
            .reasons
            .contains(&DeathBlocker::SignedTransactionMissing)
    );
    assert!(!report.recovery_enabled);
}
