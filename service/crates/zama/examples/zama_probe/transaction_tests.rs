use super::*;
use alloy::{consensus::Transaction, network::TransactionBuilder, primitives::TxKind};

#[test]
fn deployment_is_buildable_as_a_contract_creation() {
    let request = deployment_request(
        Address::repeat_byte(0x11),
        Bytes::from_static(&[0x60, 0x00]),
    )
    .with_chain_id(11155111)
    .with_nonce(0)
    .with_gas_limit(1_000_000)
    .with_max_fee_per_gas(10)
    .with_max_priority_fee_per_gas(1);
    let tx = request
        .build_unsigned()
        .expect("deployment must have a create destination");
    assert_eq!(tx.kind(), TxKind::Create);
}
