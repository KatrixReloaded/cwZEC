-- One configured chain/contract and one operator lane. No payment authorization.
CREATE TABLE IF NOT EXISTS zama_chain (
    singleton BOOLEAN PRIMARY KEY DEFAULT true CHECK(singleton),
    chain_id BIGINT NOT NULL CHECK(chain_id > 0),
    contract BYTEA NOT NULL CHECK(octet_length(contract)=20),
    deployment_block BIGINT NOT NULL CHECK(deployment_block > 0),
    tip_number BIGINT NOT NULL,
    tip_hash BYTEA NOT NULL CHECK(octet_length(tip_hash)=32),
    finalized_number BIGINT NOT NULL,
    finalized_hash BYTEA NOT NULL CHECK(octet_length(finalized_hash)=32),
    halted BOOLEAN NOT NULL DEFAULT false,
    halt_reason TEXT
);
CREATE TABLE IF NOT EXISTS zama_blocks (
    number BIGINT PRIMARY KEY,
    hash BYTEA NOT NULL CHECK(octet_length(hash)=32),
    parent_hash BYTEA NOT NULL CHECK(octet_length(parent_hash)=32)
);
CREATE TABLE IF NOT EXISTS zama_events (
    block_number BIGINT NOT NULL,
    block_hash BYTEA NOT NULL CHECK(octet_length(block_hash)=32),
    tx_hash BYTEA NOT NULL CHECK(octet_length(tx_hash)=32),
    log_index BIGINT NOT NULL CHECK(log_index >= 0),
    event TEXT NOT NULL,
    canonical BOOLEAN NOT NULL,
    finalized BOOLEAN NOT NULL,
    PRIMARY KEY(block_hash,tx_hash,log_index),
    CHECK(NOT finalized OR canonical)
);
CREATE UNIQUE INDEX IF NOT EXISTS zama_event_canonical_identity ON zama_events(tx_hash,log_index) WHERE canonical;
CREATE TABLE IF NOT EXISTS zama_receipts (
    tx_hash BYTEA NOT NULL CHECK(octet_length(tx_hash)=32),
    block_number BIGINT NOT NULL,
    block_hash BYTEA NOT NULL CHECK(octet_length(block_hash)=32),
    sender BYTEA NOT NULL CHECK(octet_length(sender)=20),
    recipient BYTEA NOT NULL CHECK(octet_length(recipient)=20),
    success BOOLEAN NOT NULL,
    canonical BOOLEAN NOT NULL,
    finalized BOOLEAN NOT NULL,
    PRIMARY KEY(tx_hash,block_hash),
    CHECK(NOT finalized OR canonical)
);
CREATE UNIQUE INDEX IF NOT EXISTS zama_receipt_canonical_identity ON zama_receipts(tx_hash) WHERE canonical;
CREATE TABLE IF NOT EXISTS zama_lane (
    singleton BOOLEAN PRIMARY KEY DEFAULT true CHECK(singleton),
    sender BYTEA NOT NULL CHECK(octet_length(sender)=20),
    next_nonce BIGINT NOT NULL CHECK(next_nonce >= 0)
);
CREATE TABLE IF NOT EXISTS zama_transactions (
    operation_id TEXT PRIMARY KEY,
    sender BYTEA NOT NULL CHECK(octet_length(sender)=20),
    nonce BIGINT NOT NULL CHECK(nonce >= 0),
    tx_hash BYTEA NOT NULL UNIQUE CHECK(octet_length(tx_hash)=32),
    raw BYTEA NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('pending','successful','reverted','unresolved')),
    finalized BOOLEAN NOT NULL DEFAULT false,
    UNIQUE(sender,nonce),
    CHECK(NOT finalized OR state IN ('successful','reverted'))
);
