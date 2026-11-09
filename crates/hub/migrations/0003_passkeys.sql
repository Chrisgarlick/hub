-- Passkeys (WebAuthn) for signing in to the UI, as in Board. Single user:
-- every passkey is yours. Only ES256 (P-256) keys, which every passkey
-- provider supports.
CREATE TABLE passkeys (
  id           TEXT PRIMARY KEY,               -- the credential id, base64url
  name         TEXT NOT NULL,                  -- e.g. "iPhone", for the list
  public_key   BYTEA NOT NULL,                 -- SEC1 uncompressed P-256 point (65 bytes)
  sign_count   BIGINT NOT NULL DEFAULT 0,      -- synced passkeys always report 0
  created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
  last_used_at TIMESTAMPTZ
);

-- Single-use challenges for adding a passkey and signing in, valid 5 minutes.
CREATE TABLE passkey_challenges (
  challenge  TEXT PRIMARY KEY,                 -- base64url of 32 random bytes
  kind       TEXT NOT NULL CHECK (kind IN ('register', 'login')),
  expires_at TIMESTAMPTZ NOT NULL
);
