//! SDK identity checks for the operator transaction recovered by the refunder.
use super::*;
use alloy::primitives::{Address, B256};
use zama::contract::{SignedTransaction, TransactionDomain, TransactionFees};

#[test]
fn refund_void_restart_preserves_bound_identity() -> Result<()> {
    let signer: PrivateKeySigner =
        "0000000000000000000000000000000000000000000000000000000000000001".parse()?;
    let contract = Address::repeat_byte(7);
    let domain = TransactionDomain::new(11155111, contract)?;
    let id = B256::repeat_byte(42);
    let call = domain.void_deposit(id);
    let signed = domain.sign_operator(
        &signer,
        &call,
        TransactionFees {
            nonce: 3,
            gas_limit: 100_000,
            max_fee_per_gas: 10,
            max_priority_fee_per_gas: 1,
        },
    )?;
    let restored =
        SignedTransaction::restore(11155111, contract, signer.address(), signed.raw().clone())?;
    assert_eq!(restored.hash(), signed.hash());
    assert_eq!(restored.void_identity(), Some(id));
    assert!(restored.matches_call(&call));
    assert!(!restored.matches_call(&domain.void_deposit(B256::repeat_byte(43))));
    assert_eq!(restored.mint_identity(), None);
    Ok(())
}
