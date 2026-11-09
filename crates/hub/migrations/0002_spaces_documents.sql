-- Phase 1 (spec section 3): a space per client, and its markdown documents.

-- A space per client from Clients, keyed by the client's id (slugs can be
-- reused after a client is deleted), plus the studio space `_studio`.
CREATE TABLE spaces (
  id          TEXT PRIMARY KEY,               -- Clients' client id, or '_studio'
  slug        TEXT NOT NULL,
  name        TEXT NOT NULL,
  kind        TEXT NOT NULL CHECK (kind IN ('client', 'studio')),
  archived_at TIMESTAMPTZ,                    -- the client was deleted in Clients
  created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX spaces_live_slug ON spaces (slug) WHERE archived_at IS NULL;

CREATE TABLE documents (
  id          TEXT PRIMARY KEY,               -- doc_<ulid>
  space_id    TEXT NOT NULL REFERENCES spaces (id) ON DELETE CASCADE,
  path        TEXT NOT NULL,                  -- e.g. brief.md, research/interviews.md
  title       TEXT NOT NULL,                  -- from the frontmatter, else the first heading
  doc_type    TEXT NOT NULL,                  -- from the frontmatter (brief, audience, …)
  visibility  TEXT NOT NULL CHECK (visibility IN ('internal', 'client', 'secret')),
  content     TEXT NOT NULL,                  -- the whole markdown file, frontmatter included
  version     INTEGER NOT NULL DEFAULT 1,
  created_by  TEXT NOT NULL,                  -- 'user', or a Clients key's name
  updated_by  TEXT NOT NULL,
  created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
  deleted_at  TIMESTAMPTZ,
  deleted_by  TEXT
);
CREATE UNIQUE INDEX documents_live_path ON documents (space_id, path) WHERE deleted_at IS NULL;
CREATE INDEX documents_space ON documents (space_id) WHERE deleted_at IS NULL;

-- Every saved version; saves by the same author within 5 minutes merge.
CREATE TABLE document_revisions (
  id          BIGSERIAL PRIMARY KEY,
  document_id TEXT NOT NULL REFERENCES documents (id) ON DELETE CASCADE,
  version     INTEGER NOT NULL,
  title       TEXT NOT NULL,
  content     TEXT NOT NULL,
  changed_by  TEXT NOT NULL,
  created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX document_revisions_doc ON document_revisions (document_id, id);

-- UI sessions (Sign in with Clients). Only a hash of the cookie is kept.
CREATE TABLE sessions (
  id_hash      TEXT PRIMARY KEY,
  created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
  last_seen_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  expires_at   TIMESTAMPTZ NOT NULL
);

-- Small bits of state, e.g. the Clients event cursor.
CREATE TABLE sync_state (
  key        TEXT PRIMARY KEY,
  value      TEXT NOT NULL,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
