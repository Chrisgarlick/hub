-- Phase 4A (spec section 6A): the library. Reusable files the apps work
-- from (Design's style references first), by kind, name and scope.
CREATE TABLE library_items (
  id         TEXT PRIMARY KEY,                -- lib_<ulid>
  kind       TEXT NOT NULL,                   -- named by the app: design-style, …
  name       TEXT NOT NULL,                   -- lowercase, unique per kind and scope
  space_id   TEXT NOT NULL REFERENCES spaces (id) ON DELETE CASCADE,  -- '_studio', or a client's own
  title      TEXT NOT NULL,                   -- from the first `# ` line
  summary    TEXT,                            -- from the first `> ` line
  tags       TEXT[] NOT NULL DEFAULT '{}',
  source     TEXT,                            -- where it came from, free text
  body       TEXT NOT NULL,                   -- markdown, up to 200 KB
  version    INTEGER NOT NULL DEFAULT 1,
  created_by TEXT NOT NULL,
  updated_by TEXT NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  deleted_at TIMESTAMPTZ
);
CREATE UNIQUE INDEX library_live_name ON library_items (kind, space_id, name) WHERE deleted_at IS NULL;

CREATE TABLE library_revisions (
  id         BIGSERIAL PRIMARY KEY,
  item_id    TEXT NOT NULL REFERENCES library_items (id) ON DELETE CASCADE,
  version    INTEGER NOT NULL,
  body       TEXT NOT NULL,
  tags       TEXT[] NOT NULL,
  source     TEXT,
  changed_by TEXT NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX library_revisions_item ON library_revisions (item_id, id DESC);

-- A client uses a studio item: `acme` uses `elevenlabs`.
CREATE TABLE library_pins (
  space_id   TEXT NOT NULL REFERENCES spaces (id) ON DELETE CASCADE,
  item_id    TEXT NOT NULL REFERENCES library_items (id) ON DELETE CASCADE,
  created_by TEXT NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (space_id, item_id)
);

-- Library changes are proposals too; library items are searchable.
ALTER TABLE proposals DROP CONSTRAINT proposals_kind_check;
ALTER TABLE proposals ADD CONSTRAINT proposals_kind_check
  CHECK (kind IN ('doc_edit', 'doc_create', 'rule', 'decision', 'artefact_link', 'library_put', 'library_pin'));
ALTER TABLE chunks DROP CONSTRAINT chunks_source_check;
ALTER TABLE chunks ADD CONSTRAINT chunks_source_check CHECK (source IN ('document', 'rule', 'decision', 'library'));
ALTER TABLE chunks ADD COLUMN library_id TEXT REFERENCES library_items (id) ON DELETE CASCADE;
CREATE INDEX chunks_library ON chunks (library_id);
ALTER TABLE index_queue DROP CONSTRAINT index_queue_kind_check;
ALTER TABLE index_queue ADD CONSTRAINT index_queue_kind_check CHECK (kind IN ('document', 'space', 'library'));
