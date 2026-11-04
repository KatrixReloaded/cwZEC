use super::*;

#[test]
fn decimal_terms_are_canonical_and_bounded() {
    for invalid in [
        "",
        "01",
        " 1",
        "+1",
        "1.0",
        "1e4",
        "-1",
        "18446744073709551616",
    ] {
        assert!(decimal_u64(invalid).is_err());
    }
    assert_eq!(decimal_u64("18446744073709551615").ok(), Some(u64::MAX));
    assert!(redeem_id("0x2").is_err());
    assert!(redeem_id("02").is_err());
    assert_eq!(redeem_id("2").ok(), Some(U256::from(2)));
}

#[test]
fn private_action_terms_reject_numbers_and_extra_fields() {
    assert!(serde_json::from_value::<Recredit>(json!({"redeem_id":"2","version":0})).is_err());
    assert!(
        serde_json::from_value::<Recredit>(json!({"redeem_id":"2","version":"0","amount":"10"}))
            .is_err()
    );
    assert!(serde_json::from_value::<Prepare>(json!({"operation_id":"x","user":Address::ZERO,"amount":10001,"zcash_address":"secret","chain_id":"11155111","contract":Address::ZERO})).is_err());
}

#[tokio::test]
async fn errors_are_static_json() {
    let response = conflict().into_response();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let bytes = axum::body::to_bytes(response.into_body(), 8192)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes).unwrap(),
        json!({"error":"operation_blocked_or_terms_conflict"})
    );
}

#[tokio::test]
async fn malformed_memo_query_never_echoes_input() {
    for query in [
        "",
        "?recipient=private-marker",
        "?recipient=private-marker&unexpected=true",
    ] {
        let uri = format!("/v1/memo{query}").parse().unwrap();
        let extracted = Query::<Memo>::try_from_uri(&uri);
        let response = match memo(extracted).await {
            Ok(_) => panic!("invalid request accepted"),
            Err(e) => e.into_response(),
        };
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let bytes = axum::body::to_bytes(response.into_body(), 8192)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap(),
            json!({"error":"invalid_request"})
        );
    }
}

#[tokio::test]
async fn memo_query_returns_canonical_memo() {
    let recipient = "0xc71a9057fa590213c530caaafa78230c8a20ece4";
    let uri = format!("/v1/memo?recipient={recipient}").parse().unwrap();
    let response = memo(Query::<Memo>::try_from_uri(&uri))
        .await
        .ok()
        .expect("valid memo query");
    assert_eq!(response.0, json!({"memo":make_memo(recipient).unwrap()}));
}
