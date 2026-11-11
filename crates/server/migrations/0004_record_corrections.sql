-- Corrections of billing records.
--
-- When the AEAT rejects a record, the invoice gets a new record of the
-- same type that takes its place. A new registration is a correction
-- ("subsanación"); `previous_rejection` is the `RechazoPrevio` value it
-- is sent with: N, S or X. A new cancellation is sent with S.

ALTER TABLE billing_records
    ADD COLUMN correction boolean NOT NULL DEFAULT false,
    ADD COLUMN previous_rejection text CHECK (previous_rejection IN ('N', 'S', 'X')),
    ADD CHECK (NOT correction OR (record_type = 'registration' AND previous_rejection IS NOT NULL)),
    ADD CHECK (record_type = 'registration' OR previous_rejection IS NULL OR previous_rejection = 'S'),
    DROP CONSTRAINT billing_records_invoice_id_record_type_key;

-- An invoice still has exactly one original registration.
CREATE UNIQUE INDEX billing_records_one_original_registration
    ON billing_records (invoice_id)
    WHERE record_type = 'registration' AND NOT correction;
