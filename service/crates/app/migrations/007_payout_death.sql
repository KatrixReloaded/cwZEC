-- Keep signed operations AND their original inputs permanently. Only confirmed
-- original-burn restoration releases liquidity; old reservation rows survive.
ALTER TABLE reservations ADD COLUMN IF NOT EXISTS released BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE reservations DROP CONSTRAINT IF EXISTS reservations_pkey;
ALTER TABLE reservations ADD PRIMARY KEY(txid,action_index,operation_id);
ALTER TABLE reservations DROP CONSTRAINT IF EXISTS reservations_nullifier_key;
CREATE UNIQUE INDEX IF NOT EXISTS active_reserved_note ON reservations(txid,action_index) WHERE NOT released;
CREATE UNIQUE INDEX IF NOT EXISTS active_reserved_nullifier ON reservations(nullifier) WHERE NOT released;

CREATE TABLE IF NOT EXISTS bridge_payout_death (
    redeem_id TEXT PRIMARY KEY REFERENCES bridge_redemptions(redeem_id),
    operation_id TEXT NOT NULL UNIQUE REFERENCES operations(id),
    chain_id BIGINT NOT NULL,
    contract BYTEA NOT NULL CHECK(octet_length(contract)=20),
    signed_bytes BYTEA NOT NULL CHECK(octet_length(signed_bytes)>0),
    history TEXT,
    revision BIGINT NOT NULL DEFAULT 0,
    status TEXT NOT NULL DEFAULT 'collecting' CHECK(status IN ('collecting','complete','included','invalid')),
    checkpoint_height BIGINT,
    checkpoint_hash BYTEA CHECK(octet_length(checkpoint_hash)=32),
    acted_on BOOLEAN NOT NULL DEFAULT false,
    CHECK((checkpoint_height IS NULL)=(checkpoint_hash IS NULL)),
    CHECK(NOT acted_on OR history IS NOT NULL)
);
