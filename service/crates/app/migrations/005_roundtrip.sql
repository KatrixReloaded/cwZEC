-- Additive: existing synthetic domains, wallet journals and signed bytes survive.
ALTER TABLE bridge_deposits ADD COLUMN IF NOT EXISTS accounting_id BIGINT REFERENCES bridge_checkpoints(id);
ALTER TABLE bridge_checkpoints ADD COLUMN IF NOT EXISTS unpaid_redemptions NUMERIC(39,0) NOT NULL DEFAULT 0;
CREATE TABLE IF NOT EXISTS bridge_redemptions (
    redeem_id TEXT PRIMARY KEY,
    burner BYTEA NOT NULL CHECK(octet_length(burner)=20),
    handle BYTEA NOT NULL CHECK(octet_length(handle)=32),
    dest_ct BYTEA NOT NULL,
    source_number BIGINT NOT NULL,
    source_hash BYTEA NOT NULL CHECK(octet_length(source_hash)=32),
    source_tx BYTEA NOT NULL CHECK(octet_length(source_tx)=32),
    source_log BIGINT NOT NULL,
    canonical BOOLEAN NOT NULL DEFAULT true,
    actual NUMERIC(39,0) NOT NULL CHECK(actual>=0),
    destination TEXT,
    state TEXT NOT NULL CHECK(state IN ('ready','zero','rejected','reserved','paid','fulfilled')),
    rejection TEXT,
    version BIGINT NOT NULL DEFAULT 0,
    operation_id TEXT NOT NULL UNIQUE,
    accounting_id BIGINT REFERENCES bridge_checkpoints(id),
    paid_height BIGINT,
    paid_hash BYTEA CHECK(octet_length(paid_hash)=32),
    fulfilled_number BIGINT,
    fulfilled_hash BYTEA CHECK(octet_length(fulfilled_hash)=32)
);
CREATE TABLE IF NOT EXISTS bridge_origin (
    singleton BOOLEAN PRIMARY KEY DEFAULT true CHECK(singleton),
    deployment_number BIGINT NOT NULL,
    deployment_hash BYTEA NOT NULL CHECK(octet_length(deployment_hash)=32),
    deployment_tx BYTEA NOT NULL CHECK(octet_length(deployment_tx)=32),
    runtime_hash BYTEA NOT NULL CHECK(octet_length(runtime_hash)=32),
    service BYTEA NOT NULL CHECK(octet_length(service)=20),
    auditor BYTEA NOT NULL CHECK(octet_length(auditor)=20)
);
CREATE TABLE IF NOT EXISTS bridge_user_requests (
    id TEXT PRIMARY KEY,
    user_address BYTEA NOT NULL CHECK(octet_length(user_address)=20),
    amount NUMERIC(39,0) NOT NULL CHECK(amount>=0),
    destination_hash BYTEA NOT NULL CHECK(octet_length(destination_hash)=32),
    calldata BYTEA NOT NULL,
    signed_raw BYTEA,
    tx_hash BYTEA UNIQUE CHECK(octet_length(tx_hash)=32),
    CHECK((signed_raw IS NULL)=(tx_hash IS NULL))
);
CREATE TABLE IF NOT EXISTS bridge_acceptance (
    singleton BOOLEAN PRIMARY KEY DEFAULT true CHECK(singleton),
    user_address BYTEA NOT NULL CHECK(octet_length(user_address)=20),
    destination TEXT NOT NULL,
    redeem_amount BIGINT NOT NULL CHECK(redeem_amount>10000),
    deposit_id BYTEA REFERENCES bridge_deposits(deposit_id),
    balance_handle BYTEA CHECK(octet_length(balance_handle)=32)
);
