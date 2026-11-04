-- Preserve all original attempts, reservations, deployment and runtime evidence.
ALTER TABLE bridge_redemptions DROP CONSTRAINT IF EXISTS bridge_redemptions_state_check;
ALTER TABLE bridge_redemptions ADD CONSTRAINT bridge_redemptions_state_check
 CHECK(state IN ('ready','zero','rejected','reserved','paid','fulfilled','recredit_pending','recredited'));
ALTER TABLE bridge_redemptions ADD COLUMN IF NOT EXISTS recredited_number BIGINT;
ALTER TABLE bridge_redemptions ADD COLUMN IF NOT EXISTS recredited_hash BYTEA CHECK(octet_length(recredited_hash)=32);
ALTER TABLE bridge_redemptions ADD COLUMN IF NOT EXISTS restored_handle BYTEA CHECK(octet_length(restored_handle)=32);

CREATE TABLE IF NOT EXISTS bridge_refunds (
    deposit_id BYTEA PRIMARY KEY REFERENCES bridge_deposits(deposit_id),
    participant TEXT NOT NULL CHECK(length(participant) BETWEEN 1 AND 128),
    destination TEXT NOT NULL CHECK(length(destination)>0),
    gross_amount BIGINT NOT NULL CHECK(gross_amount>10000),
    fee BIGINT NOT NULL CHECK(fee=10000),
    state TEXT NOT NULL CHECK(state IN ('authorized','reserved','paid')),
    operation_id TEXT NOT NULL UNIQUE,
    void_operation_id TEXT NOT NULL UNIQUE,
    version BIGINT NOT NULL DEFAULT 0,
    accounting_id BIGINT REFERENCES bridge_checkpoints(id),
    paid_height BIGINT,
    paid_hash BYTEA CHECK(octet_length(paid_hash)=32),
    CHECK((paid_height IS NULL)=(paid_hash IS NULL))
);
