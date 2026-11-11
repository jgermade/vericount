-- Issued invoices and their Veri*factu billing records.
--
-- Every table here is insert-only: rows are never updated or deleted.
-- Corrections and cancellations are new rows.

CREATE TABLE invoices (
    id             bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    issuer_nif     text NOT NULL,
    issuer_name    text NOT NULL,
    series         text NOT NULL,
    number         integer NOT NULL CHECK (number > 0),
    -- "NumSerieFactura" in Veri*factu records.
    series_number  text NOT NULL,
    issue_date     date NOT NULL,
    -- "TipoFactura" in Veri*factu records.
    invoice_type   text NOT NULL CHECK (invoice_type IN ('F1')),
    recipient_name text NOT NULL,
    recipient_nif  text NOT NULL,
    description    text NOT NULL,
    total_base     numeric(14, 2) NOT NULL,
    total_tax      numeric(14, 2) NOT NULL,
    total_amount   numeric(14, 2) NOT NULL,
    created_at     timestamptz NOT NULL DEFAULT now(),
    UNIQUE (issuer_nif, series, number),
    UNIQUE (issuer_nif, series_number)
);

CREATE TABLE invoice_lines (
    invoice_id  bigint NOT NULL REFERENCES invoices (id),
    position    integer NOT NULL CHECK (position > 0),
    description text NOT NULL,
    quantity    numeric(16, 4) NOT NULL,
    unit_price  numeric(16, 4) NOT NULL,
    vat_rate    numeric(5, 2) NOT NULL,
    base_amount numeric(14, 2) NOT NULL,
    PRIMARY KEY (invoice_id, position)
);

CREATE TABLE invoice_taxes (
    invoice_id  bigint NOT NULL REFERENCES invoices (id),
    vat_rate    numeric(5, 2) NOT NULL,
    base_amount numeric(14, 2) NOT NULL,
    tax_amount  numeric(14, 2) NOT NULL,
    PRIMARY KEY (invoice_id, vat_rate)
);

-- One chain of records per issuer. Each record stores the hash of the
-- previous one; the first record of a chain has an empty previous hash.
CREATE TABLE billing_records (
    id            bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    invoice_id    bigint NOT NULL REFERENCES invoices (id),
    record_type   text NOT NULL CHECK (record_type IN ('registration', 'cancellation')),
    issuer_nif    text NOT NULL,
    series_number text NOT NULL,
    issue_date    date NOT NULL,
    invoice_type  text,
    total_tax     numeric(14, 2),
    total_amount  numeric(14, 2),
    previous_hash text NOT NULL CHECK (previous_hash ~ '^([0-9A-F]{64})?$'),
    hash          text NOT NULL CHECK (hash ~ '^[0-9A-F]{64}$'),
    -- "FechaHoraHusoGenRegistro", kept as the exact text that was hashed.
    generated_at  text NOT NULL,
    created_at    timestamptz NOT NULL DEFAULT now(),
    CHECK (
        (record_type = 'registration'
            AND invoice_type IS NOT NULL AND total_tax IS NOT NULL AND total_amount IS NOT NULL)
        OR
        (record_type = 'cancellation'
            AND invoice_type IS NULL AND total_tax IS NULL AND total_amount IS NULL)
    ),
    -- A hash can be the previous hash of only one record: the chain cannot fork.
    UNIQUE (issuer_nif, previous_hash),
    UNIQUE (issuer_nif, hash),
    UNIQUE (invoice_id, record_type)
);

CREATE FUNCTION reject_modification() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION '% is not allowed on %: the table is insert-only', TG_OP, TG_TABLE_NAME
        USING ERRCODE = 'integrity_constraint_violation';
END;
$$;

DO $$
DECLARE
    name text;
BEGIN
    FOREACH name IN ARRAY ARRAY['invoices', 'invoice_lines', 'invoice_taxes', 'billing_records']
    LOOP
        EXECUTE format(
            'CREATE TRIGGER %I BEFORE UPDATE OR DELETE ON %I
             FOR EACH ROW EXECUTE FUNCTION reject_modification()',
            name || '_reject_row_changes', name);
        EXECUTE format(
            'CREATE TRIGGER %I BEFORE TRUNCATE ON %I
             FOR EACH STATEMENT EXECUTE FUNCTION reject_modification()',
            name || '_reject_truncate', name);
    END LOOP;
END;
$$;

-- The application connects with a role that is a member of vericounts_app,
-- which can only read and insert. Migrations run with the owner role.
DO $$
BEGIN
    CREATE ROLE vericounts_app NOLOGIN;
EXCEPTION
    WHEN duplicate_object OR unique_violation THEN NULL;
END;
$$;

GRANT SELECT, INSERT ON invoices, invoice_lines, invoice_taxes, billing_records TO vericounts_app;
