-- Phase 4 (spec section 7): every change that isn't yours, in the UI, is a
-- proposal. It applies when you approve it, or at once when the hands-off
-- switch is on and its kind is allowed for the space.
CREATE TABLE proposals (
  id           TEXT PRIMARY KEY,               -- prp_<ulid>
  space_id     TEXT NOT NULL REFERENCES spaces (id) ON DELETE CASCADE,
  kind         TEXT NOT NULL CHECK (kind IN ('doc_edit', 'doc_create', 'rule', 'decision', 'artefact_link')),
  target       TEXT,                           -- the document a doc_edit changes
  base_version INTEGER,                        -- the version a doc_edit was made against
  change       JSONB NOT NULL,                 -- what to apply (content, rule, decision, link)
  diff         TEXT,                           -- for a doc_edit: unified diff against base_version
  rationale    TEXT NOT NULL,
  sources      JSONB NOT NULL DEFAULT '[]',    -- [{doc, anchor?} | {url, note?}]
  proposed_by  TEXT NOT NULL,
  status       TEXT NOT NULL CHECK (status IN ('pending', 'applied', 'rejected', 'withdrawn', 'stale', 'reverted')),
  decided_by   TEXT,                           -- 'user', 'auto', or 'user via <key>'
  decided_at   TIMESTAMPTZ,
  note         TEXT,                           -- your reason for rejecting, or the words you approved with
  result       JSONB,                          -- what applying made: {document, version} | {rule} | {decision} | {artefact}
  created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX proposals_pending ON proposals (created_at) WHERE status = 'pending';
CREATE INDEX proposals_space ON proposals (space_id, created_at DESC);

-- review (wait for you) or auto (apply now), per space and kind. A space
-- without a row uses the '*' row for that kind; without that, review.
CREATE TABLE approval_policies (
  space_id TEXT NOT NULL,                      -- a space id, or '*' for every space
  kind     TEXT NOT NULL,
  mode     TEXT NOT NULL CHECK (mode IN ('review', 'auto')),
  PRIMARY KEY (space_id, kind)
);

CREATE TABLE settings (
  key        TEXT PRIMARY KEY,
  value      JSONB NOT NULL,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
-- The hands-off switch: off. While off, every proposal waits for you.
INSERT INTO settings (key, value) VALUES ('hands_off', 'false');

-- Work that belongs to a client: boards, designs, scenes, PDFs.
CREATE TABLE artefacts (
  id          TEXT PRIMARY KEY,                -- art_<ulid>
  space_id    TEXT NOT NULL REFERENCES spaces (id) ON DELETE CASCADE,
  app         TEXT NOT NULL CHECK (app IN ('board', 'design', 'motion', 'typeset', 'other')),
  kind        TEXT NOT NULL,                   -- board, design, scene, pdf, …
  title       TEXT NOT NULL,
  url         TEXT NOT NULL,
  external_id TEXT,                            -- the app's own id, e.g. a board id
  note        TEXT,
  created_by  TEXT NOT NULL,
  created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
  removed_at  TIMESTAMPTZ
);
CREATE UNIQUE INDEX artefacts_live_url ON artefacts (space_id, url) WHERE removed_at IS NULL;
