-- Company data, numbering series and contacts.

-- The company that issues the invoices. There is exactly one.
CREATE TABLE company (
    singleton   boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    nif         text NOT NULL,
    name        text NOT NULL,
    address     text NOT NULL,
    postal_code text NOT NULL,
    city        text NOT NULL,
    updated_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE series (
    code        text PRIMARY KEY CHECK (code ~ '^[A-Z0-9]{1,20}$'),
    description text NOT NULL DEFAULT '',
    active      boolean NOT NULL DEFAULT true,
    created_at  timestamptz NOT NULL DEFAULT now()
);

INSERT INTO series (code) SELECT DISTINCT series FROM invoices;
ALTER TABLE invoices ADD FOREIGN KEY (series) REFERENCES series (code);

-- Customers and suppliers.
CREATE TABLE contacts (
    id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name        text NOT NULL,
    nif         text NOT NULL UNIQUE,
    address     text NOT NULL DEFAULT '',
    postal_code text NOT NULL DEFAULT '',
    city        text NOT NULL DEFAULT '',
    email       text NOT NULL DEFAULT '',
    is_customer boolean NOT NULL DEFAULT true,
    is_supplier boolean NOT NULL DEFAULT false,
    archived    boolean NOT NULL DEFAULT false,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now()
);

-- Invoices keep a copy of the addresses as they were when issued, and a
-- link to the contact they were issued to.
ALTER TABLE invoices
    ADD COLUMN issuer_address    text NOT NULL DEFAULT '',
    ADD COLUMN recipient_address text NOT NULL DEFAULT '',
    ADD COLUMN contact_id        bigint REFERENCES contacts (id);

GRANT SELECT, INSERT, UPDATE ON company, series, contacts TO vericounts_app;
