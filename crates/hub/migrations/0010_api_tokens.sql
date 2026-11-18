-- Hub's own API tokens (`hub admin token create`), for build scripts that
-- download a client's published brand from `/v1`. Only the hash is kept.
CREATE TABLE api_tokens (
  name         TEXT PRIMARY KEY,
  token_hash   TEXT NOT NULL UNIQUE,
  scope        TEXT NOT NULL,                  -- brand:read
  created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
  last_used_at TIMESTAMPTZ
);
