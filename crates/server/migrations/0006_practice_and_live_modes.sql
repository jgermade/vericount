-- Practice and live modes, and the AEAT certificate uploaded from the
-- settings.
--
-- An installation starts in practice mode: its invoices are not real and
-- their records go to a mock of the AEAT inside the server. Once a
-- certificate is uploaded, live mode can be turned on, once and for good.
-- Live invoices start a new numbering and a new chain of records, and
-- practice data is no longer shown. Rows are still never deleted.

-- Existing rows predate live mode, so they are practice data.
ALTER TABLE invoices ADD COLUMN live boolean NOT NULL DEFAULT false;
ALTER TABLE billing_records ADD COLUMN live boolean NOT NULL DEFAULT false;

-- Each mode has its own numbering and its own chain.
ALTER TABLE invoices
    DROP CONSTRAINT invoices_issuer_nif_series_number_key,
    DROP CONSTRAINT invoices_issuer_nif_series_number_key1,
    ADD CONSTRAINT invoices_number_key UNIQUE (issuer_nif, live, series, number),
    ADD CONSTRAINT invoices_series_number_key UNIQUE (issuer_nif, live, series_number),
    ADD CONSTRAINT invoices_id_live_key UNIQUE (id, live);
ALTER TABLE billing_records
    DROP CONSTRAINT billing_records_issuer_nif_previous_hash_key,
    DROP CONSTRAINT billing_records_issuer_nif_hash_key,
    ADD CONSTRAINT billing_records_previous_hash_key UNIQUE (issuer_nif, live, previous_hash),
    ADD CONSTRAINT billing_records_hash_key UNIQUE (issuer_nif, live, hash),
    -- A record is in the mode of its invoice.
    ADD CONSTRAINT billing_records_invoice_mode_fkey
        FOREIGN KEY (invoice_id, live) REFERENCES invoices (id, live);

-- Set once, when live mode is turned on. There is no way back.
CREATE TABLE live_mode (
    singleton    boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    activated_at timestamptz NOT NULL DEFAULT now(),
    activated_by bigint NOT NULL REFERENCES users (id),
    -- The company NIF cannot change afterwards.
    issuer_nif   text NOT NULL
);

CREATE TRIGGER live_mode_reject_row_changes BEFORE UPDATE OR DELETE ON live_mode
    FOR EACH ROW EXECUTE FUNCTION reject_modification();
CREATE TRIGGER live_mode_reject_truncate BEFORE TRUNCATE ON live_mode
    FOR EACH STATEMENT EXECUTE FUNCTION reject_modification();

-- The client certificate for the AEAT, with its private key, encrypted
-- with VERICOUNT_SECRET_KEY. There is at most one; uploading another one
-- or removing it deletes the row, so the key does not stay behind.
CREATE TABLE aeat_certificate (
    singleton      boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    uploaded_at    timestamptz NOT NULL DEFAULT now(),
    uploaded_by    bigint NOT NULL REFERENCES users (id),
    subject        text NOT NULL,
    nif            text,
    of_legal_person boolean NOT NULL,
    expires_at     timestamptz NOT NULL,
    -- XChaCha20-Poly1305: 24-byte nonce, then the encrypted PEM file.
    nonce          bytea NOT NULL CHECK (length(nonce) = 24),
    identity       bytea NOT NULL
);

ALTER TABLE aeat_submissions ADD COLUMN live boolean NOT NULL DEFAULT false;

GRANT SELECT, INSERT ON live_mode TO vericounts_app;
GRANT SELECT, INSERT, DELETE ON aeat_certificate TO vericounts_app;
