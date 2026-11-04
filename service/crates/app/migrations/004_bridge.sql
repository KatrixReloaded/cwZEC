-- Deposit/mint foundation only. No refund payment or redemption settlement claims.
CREATE TABLE IF NOT EXISTS bridge_domain (
    singleton BOOLEAN PRIMARY KEY DEFAULT true CHECK(singleton),
    chain_id BIGINT NOT NULL CHECK(chain_id=11155111),
    contract BYTEA NOT NULL CHECK(octet_length(contract)=20),
    halted BOOLEAN NOT NULL DEFAULT false,
    halt_reason TEXT
);
CREATE TABLE IF NOT EXISTS bridge_deposits (
    deposit_id BYTEA PRIMARY KEY CHECK(octet_length(deposit_id)=32),
    txid BYTEA NOT NULL CHECK(octet_length(txid)=32),
    action_index BIGINT NOT NULL CHECK(action_index BETWEEN 0 AND 4294967295),
    amount BIGINT NOT NULL CHECK(amount BETWEEN 0 AND 2100000000000000),
    recipient BYTEA CHECK(octet_length(recipient)=20),
    source_height BIGINT NOT NULL,
    source_hash BYTEA NOT NULL CHECK(octet_length(source_hash)=32),
    canonical BOOLEAN NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('eligible','refund_pending','mint_pending','minted','voided','mismatch')),
    version BIGINT NOT NULL DEFAULT 0 CHECK(version>=0),
    operation_id TEXT NOT NULL UNIQUE,
    claimed BOOLEAN NOT NULL DEFAULT false,
    acted_on BOOLEAN NOT NULL DEFAULT false,
    settled_number BIGINT,
    settled_hash BYTEA CHECK(octet_length(settled_hash)=32),
    observed_number BIGINT,
    observed_hash BYTEA CHECK(octet_length(observed_hash)=32),
    actual_amount NUMERIC(39,0) CHECK(actual_amount>=0),
    actual_handle BYTEA CHECK(octet_length(actual_handle)=32),
    UNIQUE(txid,action_index),
    FOREIGN KEY(txid,action_index) REFERENCES notes(txid,action_index),
    CHECK((settled_number IS NULL) = (settled_hash IS NULL)),
    CHECK((observed_number IS NULL) = (observed_hash IS NULL))
);
CREATE TABLE IF NOT EXISTS bridge_checkpoints (
    id BIGSERIAL PRIMARY KEY,
    zcash_height BIGINT NOT NULL,
    zcash_hash BYTEA NOT NULL CHECK(octet_length(zcash_hash)=32),
    zama_number BIGINT NOT NULL,
    zama_hash BYTEA NOT NULL CHECK(octet_length(zama_hash)=32),
    supply_handle BYTEA NOT NULL CHECK(octet_length(supply_handle)=32),
    supply NUMERIC(39,0) NOT NULL CHECK(supply>=0),
    assets NUMERIC(39,0) NOT NULL CHECK(assets>=0),
    deposit_liability NUMERIC(39,0) NOT NULL CHECK(deposit_liability>=0),
    fee_reserve NUMERIC(39,0),
    coverage_complete BOOLEAN NOT NULL,
    acted_on BOOLEAN NOT NULL DEFAULT false,
    valid BOOLEAN NOT NULL DEFAULT true
);
