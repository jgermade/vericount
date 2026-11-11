-- Users, sessions, the event log and AEAT submissions.

CREATE TABLE users (
    id            bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    username      text NOT NULL UNIQUE CHECK (username ~ '^[a-z0-9._-]{1,40}$'),
    -- Argon2id hash in PHC string format.
    password_hash text NOT NULL,
    created_at    timestamptz NOT NULL DEFAULT now(),
    disabled_at   timestamptz
);

CREATE TABLE sessions (
    id         bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id    bigint NOT NULL REFERENCES users (id),
    -- SHA-256 of the session token; the token itself is never stored.
    token_hash text NOT NULL UNIQUE CHECK (token_hash ~ '^[0-9a-f]{64}$'),
    user_agent text NOT NULL DEFAULT '',
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    revoked_at timestamptz
);

-- Insert-only log of what happened and who did it.
CREATE TABLE events (
    id         bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    created_at timestamptz NOT NULL DEFAULT now(),
    user_id    bigint REFERENCES users (id),
    kind       text NOT NULL,
    detail     text NOT NULL DEFAULT ''
);

-- A request sent to the AEAT. The row is inserted before sending, so a
-- request that never got an answer is still on record.
CREATE TABLE aeat_submissions (
    id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    issuer_nif  text NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    request_xml text NOT NULL
);

CREATE TABLE aeat_submission_records (
    submission_id bigint NOT NULL REFERENCES aeat_submissions (id),
    record_id     bigint NOT NULL REFERENCES billing_records (id),
    PRIMARY KEY (submission_id, record_id)
);

-- What came back for a submission, inserted once when the attempt ends.
CREATE TABLE aeat_responses (
    submission_id bigint PRIMARY KEY REFERENCES aeat_submissions (id),
    received_at   timestamptz NOT NULL DEFAULT now(),
    outcome       text NOT NULL CHECK (outcome IN (
        'accepted', 'partially_accepted', 'rejected',
        'fault', 'http_error', 'transport_error', 'malformed_response')),
    http_status   integer,
    response_xml  text,
    -- Secure verification code ("CSV") returned by the AEAT.
    csv           text,
    wait_seconds  integer,
    error         text
);

CREATE TABLE aeat_record_results (
    submission_id     bigint NOT NULL,
    record_id         bigint NOT NULL,
    status            text NOT NULL CHECK (status IN ('accepted', 'accepted_with_errors', 'rejected')),
    error_code        text,
    error_description text,
    PRIMARY KEY (submission_id, record_id),
    FOREIGN KEY (submission_id, record_id)
        REFERENCES aeat_submission_records (submission_id, record_id)
);

DO $$
DECLARE
    name text;
BEGIN
    FOREACH name IN ARRAY ARRAY['events', 'aeat_submissions', 'aeat_submission_records',
                                'aeat_responses', 'aeat_record_results']
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

GRANT SELECT, INSERT ON events, aeat_submissions, aeat_submission_records,
    aeat_responses, aeat_record_results TO vericounts_app;
GRANT SELECT, INSERT, UPDATE ON users, sessions TO vericounts_app;
