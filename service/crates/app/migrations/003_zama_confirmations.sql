-- Preserve identities/observations while replacing the former finality policy.
-- Re-evaluate eligibility on the first validated canonical scan, not during DDL.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM information_schema.columns
        WHERE table_schema=current_schema() AND table_name='zama_transactions' AND column_name='finalized') THEN
        ALTER TABLE zama_transactions RENAME COLUMN finalized TO confirmed;
        ALTER TABLE zama_events RENAME COLUMN finalized TO confirmed;
        ALTER TABLE zama_receipts RENAME COLUMN finalized TO confirmed;
        UPDATE zama_transactions SET confirmed=false;
        UPDATE zama_events SET confirmed=false;
        UPDATE zama_receipts SET confirmed=false;
    END IF;
END $$;
ALTER TABLE zama_chain DROP COLUMN IF EXISTS finalized_number;
ALTER TABLE zama_chain DROP COLUMN IF EXISTS finalized_hash;
-- Application records use of a successful observation before an external action.
-- This marker survives any rewind and never authorizes a payment by itself.
ALTER TABLE zama_receipts ADD COLUMN IF NOT EXISTS acted_on BOOLEAN NOT NULL DEFAULT false;
