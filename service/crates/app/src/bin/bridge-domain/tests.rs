use super::deployment_gas_limit;

#[test]
fn observed_deployment_estimate_and_bounded_margin() {
    assert_eq!(deployment_gas_limit(18_126_605).unwrap(), 21_751_926);
    assert_eq!(deployment_gas_limit(20_000_000).unwrap(), 24_000_000);
    for estimate in [0, 20_000_001, u64::MAX] {
        assert!(deployment_gas_limit(estimate).is_err());
    }
}
