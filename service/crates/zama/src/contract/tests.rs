use super::*;
use serde_json::{Value, json};

fn permit_fixture(contract: Address) -> Value {
    let start = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .to_string();
    let key = format!("0x{}", "ab".repeat(32));
    let typed = json!({
        "domain":{"name":"Decryption","version":"1","chainId":"11155111","verifyingContract":"0x5D8BD78e2ea6bbE41f26dFe9fdaEAa349e077478"},
        "primaryType":"UserDecryptRequestVerification",
        "types":{
            "EIP712Domain":[{"name":"name","type":"string"},{"name":"version","type":"string"},{"name":"chainId","type":"uint256"},{"name":"verifyingContract","type":"address"}],
            "UserDecryptRequestVerification":[{"name":"publicKey","type":"bytes"},{"name":"contractAddresses","type":"address[]"},{"name":"startTimestamp","type":"uint256"},{"name":"durationDays","type":"uint256"},{"name":"extraData","type":"bytes"}]
        },
        "message":{"publicKey":key,"contractAddresses":[contract],"startTimestamp":start,"durationDays":"1","extraData":"0x00"}
    });
    let digest = serde_json::from_value::<TypedData>(typed.clone())
        .unwrap()
        .eip712_signing_hash()
        .unwrap();
    json!({"publicKey":key,"startTimestamp":start,"typedData":typed,"digest":digest})
}

#[tokio::test]
async fn permit_signing_rejects_changed_scope_and_terms() {
    let signer = PrivateKeySigner::random();
    let contract = Address::repeat_byte(0x11);
    let value = permit_fixture(contract);
    let signature = sign_permit(
        &signer,
        contract,
        serde_json::from_value(value.clone()).unwrap(),
    )
    .await
    .unwrap();
    let signature: alloy::primitives::Signature = signature.parse().unwrap();
    let digest: B256 = serde_json::from_value(value["digest"].clone()).unwrap();
    assert_eq!(
        signature.recover_address_from_prehash(&digest).unwrap(),
        signer.address()
    );
    for pointer in [
        "/typedData/domain/chainId",
        "/typedData/message/durationDays",
        "/typedData/message/startTimestamp",
    ] {
        let mut changed = value.clone();
        *changed.pointer_mut(pointer).unwrap() = json!("2");
        assert!(
            sign_permit(&signer, contract, serde_json::from_value(changed).unwrap())
                .await
                .is_err()
        );
    }
    assert!(
        sign_permit(
            &signer,
            Address::repeat_byte(0x22),
            serde_json::from_value(value.clone()).unwrap()
        )
        .await
        .is_err()
    );
    let mut changed = value;
    changed["digest"] = json!(B256::ZERO);
    assert!(
        sign_permit(&signer, contract, serde_json::from_value(changed).unwrap())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn maintained_permit_requires_independently_expected_kms_context() {
    let signer = PrivateKeySigner::random();
    let contract = Address::repeat_byte(0x11);
    let context = U256::from(7);
    let mut value = permit_fixture(contract);
    let mut extra = vec![1];
    extra.extend_from_slice(&context.to_be_bytes::<32>());
    value["typedData"]["message"]["extraData"] = json!(Bytes::from(extra));
    value["digest"] = json!(
        serde_json::from_value::<TypedData>(value["typedData"].clone())
            .unwrap()
            .eip712_signing_hash()
            .unwrap()
    );
    let permit = || serde_json::from_value(value.clone()).unwrap();
    let signature: alloy::primitives::Signature =
        sign_permit_for_context(&signer, contract, permit(), context)
            .await
            .unwrap()
            .parse()
            .unwrap();
    let digest = serde_json::from_value(value["digest"].clone()).unwrap();
    assert_eq!(
        signature.recover_address_from_prehash(&digest).unwrap(),
        signer.address()
    );
    assert!(sign_permit(&signer, contract, permit()).await.is_err());
    for wrong in [U256::ZERO, U256::from(8)] {
        assert!(
            sign_permit_for_context(&signer, contract, permit(), wrong)
                .await
                .is_err()
        );
    }
    for extra in [
        "0x00".to_string(),
        format!("0x02{}", "01".repeat(64)),
        format!("0x01{}", "00".repeat(32)),
    ] {
        let mut bad = value.clone();
        bad["typedData"]["message"]["extraData"] = json!(extra);
        bad["digest"] = json!(
            serde_json::from_value::<TypedData>(bad["typedData"].clone())
                .unwrap()
                .eip712_signing_hash()
                .unwrap()
        );
        assert!(
            sign_permit_for_context(
                &signer,
                contract,
                serde_json::from_value(bad).unwrap(),
                context
            )
            .await
            .is_err()
        );
    }
    assert!(
        sign_permit_for_context(&signer, Address::repeat_byte(0x22), permit(), context)
            .await
            .is_err()
    );
}

#[test]
fn maintained_sdk_generated_permit_matches_rust_hash() {
    // Generated by the installed SDK's actual permit builder, not our mock.
    let value: Value = serde_json::from_str(include_str!(
        "../../../../../sidecar/test/fixtures/permit-v1.json"
    ))
    .unwrap();
    let context: U256 = value["context"].as_str().unwrap().parse().unwrap();
    let mut extra = vec![1];
    extra.extend_from_slice(&context.to_be_bytes::<32>());
    let request = UserDecryptRequestVerification {
        publicKey: value["publicKey"].as_str().unwrap().parse().unwrap(),
        contractAddresses: vec![Address::repeat_byte(0x11)],
        startTimestamp: U256::from(1792990000u64),
        durationDays: U256::from(1),
        extraData: extra.into(),
    };
    let domain = eip712_domain! {
        name: "Decryption", version: "1", chain_id: 11155111,
        verifying_contract: alloy::primitives::address!("5D8BD78e2ea6bbE41f26dFe9fdaEAa349e077478"),
    };
    let digest: B256 = value["digest"].as_str().unwrap().parse().unwrap();
    assert_eq!(request.eip712_signing_hash(&domain), digest);
    let sdk: TypedData = serde_json::from_value(value["typedData"].clone()).unwrap();
    assert_eq!(sdk.eip712_signing_hash().unwrap(), digest);
}
