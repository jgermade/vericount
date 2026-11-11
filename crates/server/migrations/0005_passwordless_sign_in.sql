-- Passwordless sign-in: one-time codes sent by email, and passkeys.
-- Users are identified by their email address and have no password.

ALTER TABLE users DROP COLUMN password_hash;
ALTER TABLE users ADD COLUMN email text UNIQUE
    CHECK (email = lower(email) AND email ~ '^[^@[:space:]]+@[^@[:space:]]+$' AND length(email) <= 254);
-- Random user handle for passkeys. Authenticators keep it, so it must not
-- reveal anything about the user.
ALTER TABLE users ADD COLUMN webauthn_id uuid NOT NULL UNIQUE DEFAULT gen_random_uuid();
ALTER TABLE users ADD COLUMN invited_by bigint REFERENCES users (id);
-- Set on the first sign-in, which is when an invited user signs up.
ALTER TABLE users ADD COLUMN activated_at timestamptz;

-- Users created before this migration have neither email nor password, so
-- they cannot sign in any more; they have to be invited again.
UPDATE users SET disabled_at = now() WHERE disabled_at IS NULL;
ALTER TABLE users DROP COLUMN username;
ALTER TABLE users ADD CONSTRAINT users_enabled_have_email
    CHECK (email IS NOT NULL OR disabled_at IS NOT NULL);

-- One-time sign-in codes. Only the latest code of a user is valid.
CREATE TABLE login_codes (
    id         bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id    bigint NOT NULL REFERENCES users (id),
    -- SHA-256 of the user id and the code; the code itself is never stored.
    code_hash  text NOT NULL CHECK (code_hash ~ '^[0-9a-f]{64}$'),
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    attempts   integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    used_at    timestamptz
);
CREATE INDEX login_codes_user ON login_codes (user_id, created_at);

CREATE TABLE passkeys (
    id            bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id       bigint NOT NULL REFERENCES users (id),
    credential_id bytea NOT NULL UNIQUE,
    -- The credential as webauthn-rs serialises it: public key, signature
    -- counter and flags. No secret is stored.
    credential    jsonb NOT NULL,
    name          text NOT NULL DEFAULT '' CHECK (length(name) <= 60),
    created_at    timestamptz NOT NULL DEFAULT now(),
    last_used_at  timestamptz
);
CREATE INDEX passkeys_user ON passkeys (user_id);

-- State of a passkey registration or sign-in between its two requests.
-- Each one is used once: finishing deletes it.
CREATE TABLE webauthn_ceremonies (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    kind       text NOT NULL CHECK (kind IN ('registration', 'authentication')),
    -- The user registering a passkey; empty for a sign-in.
    user_id    bigint REFERENCES users (id),
    state      jsonb NOT NULL,
    expires_at timestamptz NOT NULL
);

GRANT SELECT, INSERT, UPDATE ON login_codes TO vericounts_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON passkeys TO vericounts_app;
GRANT SELECT, INSERT, DELETE ON webauthn_ceremonies TO vericounts_app;
