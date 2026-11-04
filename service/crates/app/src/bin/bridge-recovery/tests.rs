use super::*;

#[test]
fn operator_credential_and_strict_immutable_request_are_required() {
    let valid = r#"{"admin_key":"test-only-credential","request":{"action":"refund","deposit_id":"0x1111111111111111111111111111111111111111111111111111111111111111","version":4,"participant":"known-alice","destination":"explicit-destination"}}"#;
    assert!(matches!(
        authenticate(valid, "test-only-credential").unwrap().request,
        Action::Refund { version: 4, .. }
    ));
    assert!(authenticate(valid, "another-credential").is_err());
    assert!(
        authenticate(
            &valid.replace("test-only-credential", ""),
            "test-only-credential"
        )
        .is_err()
    );
    assert!(
        authenticate(
            &valid.replace("\"version\":4", "\"version\":4,\"amount\":100"),
            "test-only-credential"
        )
        .is_err()
    );
    assert!(authenticate(r#"{"request":{"action":"status"}}"#, "test-only-credential").is_err());
}
