use super::{bindings::*, client::*};
use alloy::{
    primitives::{Address, B256, Bytes, U256, keccak256},
    signers::local::PrivateKeySigner,
    sol_types::{SolCall, SolValue},
};
fn domain() -> TransactionDomain {
    TransactionDomain::new(11155111, Address::repeat_byte(42)).unwrap()
}
fn fees() -> TransactionFees {
    TransactionFees {
        nonce: 7,
        gas_limit: 100000,
        max_fee_per_gas: 20,
        max_priority_fee_per_gas: 2,
    }
}
#[test]
fn selectors_and_return_decoding() {
    assert_eq!(
        confidentialMintCall::SELECTOR,
        keccak256("confidentialMint(address,bytes32,bytes,bytes32)")[..4]
    );
    assert_eq!(
        requestRedeemCall::SELECTOR,
        keccak256("requestRedeem(bytes32,bytes,bytes)")[..4]
    );
    assert_eq!(
        reCreditRedeemCall::SELECTOR,
        keccak256("reCreditRedeem(uint256)")[..4]
    );
    assert_eq!(
        markRedeemFulfilledCall::SELECTOR,
        keccak256("markRedeemFulfilled(uint256,bytes32)")[..4]
    );
    let r = redemptionsCall::abi_decode_returns(
        &(
            Address::repeat_byte(1),
            B256::repeat_byte(2),
            U256::from(1),
            B256::ZERO,
        )
            .abi_encode(),
    )
    .unwrap();
    assert_eq!(r.status, 1);
    assert_eq!(r.amount, B256::repeat_byte(2));
    assert!(DepositStatus::try_from(3).is_err());
    assert!(RedemptionStatus::try_from(4).is_err());
}
#[test]
fn signing_recovery_and_domain_checks() {
    let signer = PrivateKeySigner::random();
    let d = domain();
    let signed = d
        .sign_operator(&signer, &d.void_deposit(B256::repeat_byte(5)), fees())
        .unwrap();
    assert_eq!(signed.hash(), keccak256(signed.raw()));
    assert_eq!(signed.sender(), signer.address());
    assert_eq!(signed.nonce(), 7);
    let restored = SignedTransaction::restore(
        signed.chain_id(),
        signed.contract(),
        signed.sender(),
        signed.raw().clone(),
    )
    .unwrap();
    assert_eq!(restored.hash(), signed.hash());
    assert!(
        SignedTransaction::restore(1, signed.contract(), signed.sender(), signed.raw().clone())
            .is_err()
    );
    assert!(
        SignedTransaction::restore(
            signed.chain_id(),
            Address::repeat_byte(43),
            signed.sender(),
            signed.raw().clone()
        )
        .is_err()
    );
    assert!(
        SignedTransaction::restore(
            signed.chain_id(),
            signed.contract(),
            Address::repeat_byte(43),
            signed.raw().clone()
        )
        .is_err()
    );
    let other = TransactionDomain::new(1, signed.contract()).unwrap();
    assert!(
        other
            .sign_operator(&signer, &d.recredit(U256::from(1)), fees())
            .is_err()
    );
    let mut extra = signed.raw().to_vec();
    extra.push(0);
    assert!(
        SignedTransaction::restore(
            signed.chain_id(),
            signed.contract(),
            signed.sender(),
            extra.into()
        )
        .is_err()
    );
}
#[test]
fn redeem_binds_actual_user_and_operator_cannot_sign() {
    let signer = PrivateKeySigner::random();
    let d = domain();
    let call = d.request_redeem(
        signer.address(),
        B256::repeat_byte(1),
        Bytes::new(),
        Bytes::from_static(b"opaque"),
    );
    assert!(d.sign_operator(&signer, &call, fees()).is_err());
    assert!(d.sign_user_redeem(&signer, &call, fees()).is_ok());
    assert!(
        d.sign_user_redeem(&PrivateKeySigner::random(), &call, fees())
            .is_err()
    );
    assert!(
        d.sign_user_redeem(&signer, &d.void_deposit(B256::ZERO), fees())
            .is_err()
    );
}

#[test]
fn lifecycle_events_use_real_handle_abi() {
    use alloy::sol_types::SolEvent;
    assert_eq!(
        DepositMinted::SIGNATURE_HASH,
        keccak256("DepositMinted(bytes32,address,bytes32)")
    );
    assert_eq!(
        DepositVoided::SIGNATURE_HASH,
        keccak256("DepositVoided(bytes32)")
    );
    assert_eq!(
        RedeemRequested::SIGNATURE_HASH,
        keccak256("RedeemRequested(uint256,address,bytes32,bytes)")
    );
    assert_eq!(
        RedeemFulfilled::SIGNATURE_HASH,
        keccak256("RedeemFulfilled(uint256,bytes32)")
    );
    assert_eq!(
        RedeemRecredited::SIGNATURE_HASH,
        keccak256("RedeemRecredited(uint256,address,bytes32)")
    );
    let event = RedeemRequested {
        redeemId: U256::from(42),
        user: Address::repeat_byte(2),
        amount: B256::repeat_byte(3),
        destCt: Bytes::from_static(b"opaque"),
    };
    let encoded = event.encode_log_data();
    let decoded = RedeemRequested::decode_log_data(&encoded).unwrap();
    assert_eq!(decoded.redeemId, event.redeemId);
    assert_eq!(decoded.amount, event.amount);
    assert_eq!(decoded.destCt, event.destCt);
}

/// Uses only a controlled local JSON-RPC server; no live endpoint or signer.
#[tokio::test]
async fn concrete_http_client_reads_and_rejects_wrong_chain() {
    use serde_json::{Value, json};
    use std::{
        io::{Read, Write},
        net::TcpListener,
    };
    let listener = TcpListener::bind("127.0.0.1:0").expect("controlled HTTP listener unavailable");
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let worker = std::thread::spawn(move || {
        let expected = [
            ("eth_chainId", json!("0xaa36a7")),
            ("eth_getCode", json!("0x6000")),
            (
                "eth_call",
                json!(format!(
                    "0x{}",
                    alloy::hex::encode(U256::from(8).abi_encode())
                )),
            ),
            (
                "eth_call",
                json!(format!(
                    "0x{}",
                    alloy::hex::encode(U256::from(1).abi_encode())
                )),
            ),
            (
                "eth_call",
                json!(format!(
                    "0x{}",
                    alloy::hex::encode(B256::repeat_byte(7).abi_encode())
                )),
            ),
            ("eth_chainId", json!("0x1")),
        ];
        for (method, result) in expected {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            let value: Value = loop {
                let n = stream.read(&mut buffer).unwrap();
                assert_ne!(n, 0);
                request.extend_from_slice(&buffer[..n]);
                if let Some(split) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..split]);
                    let size: usize = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse().unwrap())
                        })
                        .unwrap();
                    if request.len() >= split + 4 + size {
                        break serde_json::from_slice(&request[split + 4..split + 4 + size])
                            .unwrap();
                    }
                }
            };
            assert_eq!(value["method"], method);
            let body = json!({"jsonrpc":"2.0","id":value["id"],"result":result}).to_string();
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
        }
    });
    let client = CwzecClient::connect(&endpoint, 11155111, Address::repeat_byte(42))
        .await
        .unwrap();
    assert_eq!(
        client.consumed(B256::ZERO).await.unwrap(),
        DepositStatus::Minted
    );
    assert_eq!(
        client.balance(Address::repeat_byte(1)).await.unwrap(),
        B256::repeat_byte(7)
    );
    assert!(
        CwzecClient::connect(&endpoint, 11155111, Address::repeat_byte(42))
            .await
            .is_err()
    );
    worker.join().unwrap();
}

#[test]
fn recovery_intents_cannot_be_confused_after_signed_restore() {
    let signer = PrivateKeySigner::random();
    let d = domain();
    let id = U256::from(42);
    let tag = B256::repeat_byte(3);
    for (call, recredit, void) in [
        (d.recredit(id), Some(id), None),
        (d.void_deposit(tag), None, Some(tag)),
        (d.fulfill(id, tag), None, None),
    ] {
        let signed = d.sign_operator(&signer, &call, fees()).unwrap();
        let restored = SignedTransaction::restore(
            signed.chain_id(),
            signed.contract(),
            signed.sender(),
            signed.raw().clone(),
        )
        .unwrap();
        assert_eq!(restored.recredit_identity(), recredit);
        assert_eq!(restored.void_identity(), void);
    }
}
