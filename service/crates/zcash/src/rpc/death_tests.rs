use super::*;

#[test]
fn contextual_rpc_status_is_preserved_but_never_proves_absence() {
    for code in [
        tonic::Code::NotFound,
        tonic::Code::Unavailable,
        tonic::Code::DeadlineExceeded,
        tonic::Code::Internal,
    ] {
        let result = Err(anyhow::Error::new(tonic::Status::new(
            code,
            "private backend details",
        )))
        .context("lightwalletd GetTransaction");
        let expected = if code == tonic::Code::NotFound {
            TransactionLookup::NotFoundUnverified
        } else {
            TransactionLookup::RpcFailure
        };
        assert_eq!(classify_transaction(result), expected);
    }
    assert_eq!(
        classify_transaction(Err(anyhow::anyhow!("transport timeout"))),
        TransactionLookup::RpcFailure
    );
}

#[test]
fn diagnostic_status_excludes_remote_message_and_preserves_context() {
    let error = anyhow::Error::new(tonic::Status::unavailable("private backend data"))
        .context("lightwalletd GetBlock");
    assert_eq!(failure_code(&error), Some(14));
    assert_eq!(failure_code(&anyhow::anyhow!("grpc_code=5")), None);
}

#[test]
fn empty_success_is_incomplete_and_returned_transactions_are_diagnostic() {
    assert_eq!(
        classify_transaction(Ok(RawTransaction {
            data: vec![],
            height: 100
        })),
        TransactionLookup::EmptyResponse
    );
    for height in [0, 100, u64::MAX] {
        assert_eq!(
            classify_transaction(Ok(RawTransaction {
                data: vec![1],
                height
            })),
            TransactionLookup::Returned { height }
        );
    }
}
