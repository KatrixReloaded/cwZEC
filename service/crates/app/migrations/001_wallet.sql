CREATE TABLE IF NOT EXISTS account (
    singleton BOOLEAN PRIMARY KEY DEFAULT true CHECK(singleton),
    network TEXT NOT NULL CHECK(network = 'testnet'),
    birthday BIGINT NOT NULL CHECK(birthday BETWEEN 0 AND 4294967295),
    fvk BYTEA NOT NULL CHECK(octet_length(fvk)=96),
    use_qsk BOOLEAN NOT NULL,
    current_height BIGINT NOT NULL
);
CREATE TABLE IF NOT EXISTS checkpoints (
    height BIGINT PRIMARY KEY CHECK(height BETWEEN 0 AND 4294967295),
    hash BYTEA NOT NULL CHECK(octet_length(hash)=32),
    parent BYTEA NOT NULL CHECK(octet_length(parent)=32),
    frontier BYTEA NOT NULL,
    codec_version SMALLINT NOT NULL DEFAULT 1 CHECK(codec_version=1)
);
CREATE TABLE IF NOT EXISTS notes (
    txid BYTEA NOT NULL CHECK(octet_length(txid)=32),
    action_index BIGINT NOT NULL CHECK(action_index BETWEEN 0 AND 4294967295),
    recipient BYTEA NOT NULL CHECK(octet_length(recipient)=43),
    value BIGINT NOT NULL CHECK(value BETWEEN 0 AND 2100000000000000),
    rho BYTEA NOT NULL CHECK(octet_length(rho)=32),
    rseed BYTEA NOT NULL CHECK(octet_length(rseed)=32),
    nullifier BYTEA NOT NULL CHECK(octet_length(nullifier)=32),
    position BIGINT NOT NULL CHECK(position>=0),
    internal BOOLEAN NOT NULL,
    memo BYTEA NOT NULL CHECK(octet_length(memo)=512),
    note_version SMALLINT NOT NULL DEFAULT 3 CHECK(note_version=3),
    mined_height BIGINT NOT NULL,
    spent_height BIGINT,
    canonical BOOLEAN NOT NULL DEFAULT true,
    PRIMARY KEY(txid,action_index)
);
CREATE UNIQUE INDEX IF NOT EXISTS canonical_position ON notes(position) WHERE canonical;
CREATE UNIQUE INDEX IF NOT EXISTS canonical_nullifier ON notes(nullifier) WHERE canonical;
CREATE TABLE IF NOT EXISTS witnesses (
    height BIGINT NOT NULL REFERENCES checkpoints(height) ON DELETE CASCADE,
    txid BYTEA NOT NULL,
    action_index BIGINT NOT NULL,
    bytes BYTEA NOT NULL,
    codec_version SMALLINT NOT NULL DEFAULT 1 CHECK(codec_version=1),
    PRIMARY KEY(height,txid,action_index),
    FOREIGN KEY(txid,action_index) REFERENCES notes(txid,action_index)
);
CREATE TABLE IF NOT EXISTS observed_transactions (
    txid BYTEA NOT NULL CHECK(octet_length(txid)=32),
    height BIGINT NOT NULL REFERENCES checkpoints(height) ON DELETE CASCADE,
    PRIMARY KEY(txid,height)
);
CREATE TABLE IF NOT EXISTS operations (
    id TEXT PRIMARY KEY CHECK(length(id)>0),
    destination TEXT NOT NULL CHECK(length(destination)>0),
    amount BIGINT NOT NULL CHECK(amount>0 AND amount<=2100000000000000),
    signed_bytes BYTEA,
    txid BYTEA CHECK(octet_length(txid)=32),
    expiry_height BIGINT CHECK(expiry_height BETWEEN 1 AND 4294967295),
    fee BIGINT CHECK(fee BETWEEN 0 AND 2100000000000000),
    broadcast BOOLEAN NOT NULL DEFAULT false,
    CHECK((signed_bytes IS NULL AND txid IS NULL AND expiry_height IS NULL AND fee IS NULL)
       OR (signed_bytes IS NOT NULL AND octet_length(signed_bytes)>0 AND txid IS NOT NULL AND expiry_height IS NOT NULL AND fee IS NOT NULL))
);
CREATE TABLE IF NOT EXISTS reservations (
    txid BYTEA NOT NULL,
    action_index BIGINT NOT NULL,
    operation_id TEXT NOT NULL REFERENCES operations(id),
    nullifier BYTEA NOT NULL UNIQUE CHECK(octet_length(nullifier)=32),
    PRIMARY KEY(txid,action_index),
    FOREIGN KEY(txid,action_index) REFERENCES notes(txid,action_index)
);
