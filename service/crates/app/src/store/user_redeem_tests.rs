//! User-origin persistence checks with disposable PostgreSQL schemas and ephemeral signers.
use super::{
    bridge_tests::setup,
    zama_test_support::{TestDb, contract},
};
use alloy::{
    primitives::{Address, B256, Bytes},
    signers::local::PrivateKeySigner,
};
use anyhow::Result;
use zama::contract::{TransactionDomain, TransactionFees};

fn fees(nonce: u64) -> TransactionFees {
    TransactionFees {
        nonce,
        gas_limit: 100_000,
        max_fee_per_gas: 20,
        max_priority_fee_per_gas: 2,
    }
}

#[tokio::test]
async fn user_request_identity_and_exact_signed_bytes_survive_restart() -> Result<()> {
    let db = TestDb::new().await?;
    let mut store = setup(&db).await?;
    let user = PrivateKeySigner::random();
    let operator = PrivateKeySigner::random();
    let domain = TransactionDomain::new(11155111, contract())?;
    let destination = B256::repeat_byte(3);
    let call = domain.request_redeem(
        user.address(),
        B256::repeat_byte(4),
        Bytes::new(),
        Bytes::from_static(b"opaque destination"),
    );
    let first = store
        .save_user_redeem_request("request", user.address(), 25, destination, call.calldata())
        .await?;
    assert!(first.signed.is_none());
    // A new encryption result on retry must return the original persisted calldata.
    let retry = store
        .save_user_redeem_request(
            "request",
            user.address(),
            25,
            destination,
            &Bytes::from_static(b"different encryption"),
        )
        .await?;
    assert_eq!(retry.calldata, first.calldata);
    for (address, amount, dest) in [
        (operator.address(), 25, destination),
        (user.address(), 26, destination),
        (user.address(), 25, B256::repeat_byte(8)),
    ] {
        assert!(
            store
                .save_user_redeem_request("request", address, amount, dest, call.calldata())
                .await
                .is_err()
        );
    }
    assert!(domain.sign_operator(&operator, &call, fees(7)).is_err());
    assert!(domain.sign_user_redeem(&operator, &call, fees(7)).is_err());
    let changed_call = domain.request_redeem(
        user.address(),
        B256::repeat_byte(9),
        Bytes::new(),
        Bytes::from_static(b"changed destination"),
    );
    let changed = domain.sign_user_redeem(&user, &changed_call, fees(7))?;
    assert!(
        store
            .save_user_redeem_signed("request", &changed)
            .await
            .is_err()
    );
    // A valid user-shaped transaction signed by the operator still fails journal binding.
    let wrong_call = domain.request_redeem(
        operator.address(),
        B256::repeat_byte(4),
        Bytes::new(),
        Bytes::from_static(b"opaque destination"),
    );
    let wrong = domain.sign_user_redeem(&operator, &wrong_call, fees(7))?;
    assert!(
        store
            .save_user_redeem_signed("request", &wrong)
            .await
            .is_err()
    );
    for mismatched in [
        TransactionDomain::new(1, contract())?,
        TransactionDomain::new(11155111, Address::repeat_byte(99))?,
    ] {
        let call = mismatched.request_redeem(
            user.address(),
            B256::repeat_byte(4),
            Bytes::new(),
            Bytes::from_static(b"opaque destination"),
        );
        let signed = mismatched.sign_user_redeem(&user, &call, fees(7))?;
        assert!(
            store
                .save_user_redeem_signed("request", &signed)
                .await
                .is_err()
        );
    }
    assert!(
        store
            .user_redeem_request("request")
            .await?
            .unwrap()
            .signed
            .is_none()
    );
    let signed = domain.sign_user_redeem(&user, &call, fees(7))?;
    store.save_user_redeem_signed("request", &signed).await?;
    store.save_user_redeem_signed("request", &signed).await?;
    let replacement = domain.sign_user_redeem(&user, &call, fees(8))?;
    assert!(
        store
            .save_user_redeem_signed("request", &replacement)
            .await
            .is_err()
    );
    let mut replacement_fees = fees(7);
    replacement_fees.max_fee_per_gas += 1;
    let replacement = domain.sign_user_redeem(&user, &call, replacement_fees)?;
    assert!(
        store
            .save_user_redeem_signed("request", &replacement)
            .await
            .is_err()
    );
    drop(store);
    let store = db.connect().await?;
    let restored = store.user_redeem_request("request").await?.unwrap();
    assert_eq!(restored.calldata, first.calldata);
    let restored = restored.signed.unwrap();
    assert_eq!(restored.raw(), signed.raw());
    assert_eq!(restored.hash(), signed.hash());
    assert_eq!(restored.nonce(), 7);
    assert_eq!(restored.sender(), user.address());
    // No RPC/broadcast is performed by these store operations.
    db.cleanup().await
}
