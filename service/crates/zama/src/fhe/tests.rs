use super::*;
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
};

fn fixture(signer: &PrivateKeySigner) -> (Scope, Value) {
    let scope = Scope {
        contract: Address::repeat_byte(0x11),
        user: signer.address(),
        handle: B256::repeat_byte(0x22),
    };
    let start = (now_ms().unwrap() / 1000).to_string();
    let mut extra = vec![1];
    extra.extend_from_slice(&U256::from(7).to_be_bytes::<32>());
    let typed = json!({
        "domain":{"name":"Decryption","version":"1","chainId":"11155111","verifyingContract":"0x5D8BD78e2ea6bbE41f26dFe9fdaEAa349e077478"},
        "primaryType":"UserDecryptRequestVerification",
        "types":{
            "EIP712Domain":[{"name":"name","type":"string"},{"name":"version","type":"string"},{"name":"chainId","type":"uint256"},{"name":"verifyingContract","type":"address"}],
            "UserDecryptRequestVerification":[{"name":"publicKey","type":"bytes"},{"name":"contractAddresses","type":"address[]"},{"name":"startTimestamp","type":"uint256"},{"name":"durationDays","type":"uint256"},{"name":"extraData","type":"bytes"}]
        },
        "message":{"publicKey":"0xabcd","contractAddresses":[scope.contract],"startTimestamp":start,"durationDays":"1","extraData":Bytes::from(extra)}
    });
    let digest = serde_json::from_value::<TypedData>(typed.clone())
        .unwrap()
        .eip712_signing_hash()
        .unwrap();
    (
        scope,
        json!({"sessionId":"ab".repeat(32),"contract":scope.contract,"user":scope.user,"handle":scope.handle,
        "expiresAt":(now_ms().unwrap()+290_000).to_string(),"publicKey":"0xabcd","startTimestamp":start,"typedData":typed,"digest":digest}),
    )
}

fn prepared(scope: Scope, value: Value) -> Result<PreparedDecryption> {
    PreparedDecryption::validated(
        scope,
        serde_json::from_value(value).map_err(|_| anyhow!("invalid_sidecar_response"))?,
        now_ms()?,
    )
}

#[test]
fn strict_loopback_url() {
    for url in ["http://127.0.0.1:1234", "http://127.0.0.1:1234/"] {
        assert!(Client::new(url).is_ok());
    }
    for url in [
        "http://localhost:1234",
        "https://127.0.0.1:1234",
        "http://127.1:1234",
        "http://2130706433:1234",
        "http://127.0.0.1:0",
        "http://127.0.0.1",
        "http://user@127.0.0.1:1234",
        "http://127.0.0.1:1234/path",
        "http://127.0.0.1:1234/?x=1",
        "http://127.0.0.1:1234/#x",
    ] {
        assert!(Client::new(url).is_err(), "{url}");
    }
}

#[test]
fn uint64_is_lossless_and_canonical() {
    for value in [0, 1, (1u64 << 53) + 1, u64::MAX] {
        assert_eq!(decimal(&value.to_string()).unwrap(), value);
    }
    for value in [
        "",
        "01",
        "+1",
        "-1",
        " 1",
        "1.0",
        "1e2",
        "18446744073709551616",
        "１２",
    ] {
        assert!(decimal(value).is_err());
    }
    for value in [json!({"value":1}), json!({"value":"1","extra":0})] {
        assert!(serde_json::from_value::<DecryptResponse>(value).is_err());
    }
}

#[test]
fn expiry_uses_milliseconds_and_content_type_is_json_only() {
    let now = 1_792_990_000_000;
    assert!(validate_expiry(now + 1, now).is_ok());
    assert!(validate_expiry(now + 300_000, now).is_ok());
    for expires in [0, now - 1, now, now + 300_001, now / 1000, u64::MAX] {
        assert!(validate_expiry(expires, now).is_err());
    }
    for value in ["application/json", "Application/JSON; charset=UTF-8"] {
        assert!(valid_content_type(value));
    }
    for value in [
        "",
        "text/json",
        "text/plain",
        "application/json; charset=latin1",
        "application/json; charset=utf-8; extra=x",
    ] {
        assert!(!valid_content_type(value));
    }
}

#[test]
fn sanitizer_only_accepts_known_codes_at_their_exact_status() {
    for (status, code) in [
        (403, "sdk_acl_denied"),
        (410, "session_unavailable"),
        (504, "operation_timeout"),
    ] {
        let body = serde_json::to_vec(&json!({"error":{"code":code}})).unwrap();
        assert_eq!(response_error(status, &body).to_string(), code);
        assert_eq!(
            response_error(200, &body).to_string(),
            "invalid_sidecar_response"
        );
    }
    for body in [
        json!({"error":{"code":"https://secret.invalid?key=secret"}}),
        json!({"error":{"code":"sdk_acl_denied","message":"secret"}}),
        json!({"error":{"code":"sdk_acl_denied"},"stack":"secret"}),
    ] {
        let error = response_error(403, &serde_json::to_vec(&body).unwrap());
        assert_eq!(format!("{error:#}"), "invalid_sidecar_response");
    }
    assert_eq!(
        response_error(502, b"raw secret").to_string(),
        "invalid_sidecar_response"
    );
}

#[test]
fn prepare_rejects_malformed_or_rebound_response() {
    let signer = PrivateKeySigner::random();
    let (scope, value) = fixture(&signer);
    assert!(prepared(scope, value.clone()).is_ok());
    for (field, replacement) in [
        ("contract", json!(Address::repeat_byte(0x33))),
        ("user", json!(Address::repeat_byte(0x44))),
        ("handle", json!(B256::repeat_byte(0x55))),
        ("sessionId", json!("AB".repeat(32))),
        ("sessionId", json!("00".repeat(31))),
        ("expiresAt", json!("0")),
        (
            "expiresAt",
            json!((now_ms().unwrap() + 301_000).to_string()),
        ),
        ("expiresAt", json!((now_ms().unwrap() + 10_000) / 1000)),
        ("publicKey", json!("0x")),
        ("publicKey", json!("0x1")),
        ("publicKey", json!("gg")),
        ("publicKey", json!("ab".repeat(4097))),
        ("startTimestamp", json!("01")),
        ("digest", json!(B256::ZERO)),
        ("unknown", json!(true)),
    ] {
        let mut bad = value.clone();
        bad[field] = replacement;
        assert!(prepared(scope, bad).is_err(), "{field}");
    }
    for bad in [
        Scope {
            user: scope.contract,
            ..scope
        },
        Scope {
            handle: B256::ZERO,
            ..scope
        },
        Scope {
            contract: Address::ZERO,
            ..scope
        },
    ] {
        assert!(prepared(bad, value.clone()).is_err());
    }
}

#[tokio::test]
async fn authorization_binds_signer_context_and_permit_terms() {
    let signer = PrivateKeySigner::random();
    let (scope, value) = fixture(&signer);
    let authorized = prepared(scope, value.clone())
        .unwrap()
        .authorize(&signer, U256::from(7))
        .await
        .unwrap();
    assert_eq!(authorized.session_id, "ab".repeat(32));
    assert_eq!(authorized.scope.handle, scope.handle);
    let signature: Signature = authorized.signature.parse().unwrap();
    let digest = serde_json::from_value(value["digest"].clone()).unwrap();
    assert_eq!(
        signature.recover_address_from_prehash(&digest).unwrap(),
        scope.user
    );
    assert!(
        prepared(scope, value.clone())
            .unwrap()
            .authorize(&PrivateKeySigner::random(), U256::from(7))
            .await
            .is_err()
    );
    for context in [U256::ZERO, U256::from(8)] {
        assert!(
            prepared(scope, value.clone())
                .unwrap()
                .authorize(&signer, context)
                .await
                .is_err()
        );
    }
    for pointer in [
        "/typedData/domain/chainId",
        "/typedData/message/durationDays",
        "/typedData/message/startTimestamp",
    ] {
        let mut bad = value.clone();
        *bad.pointer_mut(pointer).unwrap() = json!("2");
        assert!(
            prepared(scope, bad)
                .unwrap()
                .authorize(&signer, U256::from(7))
                .await
                .is_err()
        );
    }
    let mut stale = prepared(scope, value).unwrap();
    stale.response.expires_at = "1".into();
    assert!(stale.authorize(&signer, U256::from(7)).await.is_err());
}

// Real loopback tests intentionally fail if the execution sandbox forbids bind;
// run the pure validation tests separately in that environment.
fn mock(status: u16, content_type: &str, body: String) -> (String, thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("loopback bind required for HTTP tests");
    let url = format!("http://{}", listener.local_addr().unwrap());
    let content_type = content_type.to_owned();
    let worker = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            let n = socket.read(&mut buffer).unwrap();
            if n == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..n]);
            if let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..end]);
                let len = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|s| s.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                if bytes.len() >= end + 4 + len {
                    break;
                }
            }
        }
        // Oversized/invalid responses may be rejected before the body is read.
        let _ = write!(
            socket,
            "HTTP/1.1 {status} Response\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nLocation: http://127.0.0.1:1/secret\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        String::from_utf8(bytes).unwrap()
    });
    (url, worker)
}

#[tokio::test]
async fn http_encrypt_preserves_uint64_and_rejects_bad_responses() {
    let (url, worker) = mock(
        200,
        "application/json",
        json!({"handle":B256::repeat_byte(1),"inputProof":"0xab"}).to_string(),
    );
    let result = Client::new(&url)
        .unwrap()
        .encrypt(Address::repeat_byte(1), Address::repeat_byte(2), u64::MAX)
        .await
        .unwrap();
    assert_eq!(result.input_proof, Bytes::from_static(&[0xab]));
    let request = worker.join().unwrap();
    assert!(request.starts_with("POST /encrypt "));
    assert!(request.contains("\"value\":\"18446744073709551615\""));
    for (status, ctype, body) in [
        (302, "application/json", "{}".into()),
        (200, "text/plain", "secret".into()),
        (200, "application/json", "secret".into()),
        (200, "application/json", " ".repeat(MAX_RESPONSE + 1)),
    ] {
        let (url, worker) = mock(status, ctype, body);
        let error = Client::new(&url)
            .unwrap()
            .encrypt(Address::repeat_byte(1), Address::repeat_byte(2), 0)
            .await
            .err()
            .unwrap();
        assert_eq!(error.to_string(), "invalid_sidecar_response");
        worker.join().unwrap();
    }
}

#[tokio::test]
async fn http_decrypt_consumes_original_authorization_scope() {
    let signer = PrivateKeySigner::random();
    let (scope, value) = fixture(&signer);
    let (url, worker) = mock(200, "application/json", value.to_string());
    let p = Client::new(&url)
        .unwrap()
        .prepare_decryption(scope)
        .await
        .unwrap();
    assert!(worker.join().unwrap().starts_with("POST /decrypt-request "));
    let authorization = p.authorize(&signer, U256::from(7)).await.unwrap();
    let (url, worker) = mock(
        200,
        "application/json",
        json!({"value":u64::MAX.to_string()}).to_string(),
    );
    assert_eq!(
        Client::new(&url)
            .unwrap()
            .decrypt(authorization)
            .await
            .unwrap(),
        u64::MAX
    );
    let request = worker.join().unwrap();
    let body: Value = serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body["sessionId"], value["sessionId"]);
    assert_eq!(body["contract"], value["contract"]);
    assert_eq!(body["user"], value["user"]);
    assert_eq!(body["handle"], value["handle"]);
}

#[tokio::test]
async fn wallet_permit_export_restore_requires_actual_user_and_independent_context() -> Result<()> {
    use alloy::signers::Signer;
    let user = PrivateKeySigner::random();
    let operator = PrivateKeySigner::random();
    let (scope, response) = fixture(&user);
    let request = prepared(scope, response.clone())?.wallet_request(U256::from(7))?;
    let digest: B256 = serde_json::from_value(request["digest"].clone())?;
    let signature = user.sign_hash(&digest).await?.to_string();
    let restored = PreparedDecryption::restore_wallet(scope, request.clone())?;
    let authorized = restored.authorize_wallet(signature.clone(), U256::from(7))?;
    assert_eq!(authorized.scope.user, user.address());
    assert_eq!(authorized.signature, signature);
    assert!(
        PreparedDecryption::restore_wallet(scope, request.clone())?
            .authorize_wallet(
                operator.sign_hash(&digest).await?.to_string(),
                U256::from(7)
            )
            .is_err()
    );
    assert!(
        PreparedDecryption::restore_wallet(scope, request.clone())?
            .authorize_wallet(signature, U256::from(8))
            .is_err()
    );
    let mut modified = request;
    modified["typedData"]["message"]["contractAddresses"] = json!([Address::repeat_byte(99)]);
    assert!(
        PreparedDecryption::restore_wallet(scope, modified)?
            .wallet_request(U256::from(7))
            .is_err()
    );
    Ok(())
}
