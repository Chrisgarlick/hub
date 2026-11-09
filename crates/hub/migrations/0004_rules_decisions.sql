-- Phase 2 (spec sections 4 and 5): rules and decisions.

-- What must hold for a client. Most come from a space's Writing rules
-- document (source_doc set, re-read on every save); others are added
-- directly. Studio rules apply to every client unless excepted.
CREATE TABLE rules (
  id            TEXT PRIMARY KEY,              -- rul_<ulid>
  space_id      TEXT NOT NULL REFERENCES spaces (id) ON DELETE CASCADE,
  kind          TEXT NOT NULL CHECK (kind IN ('must', 'must_not', 'prefer', 'avoid')),
  applies_to    TEXT[] NOT NULL,               -- all, board, design, motion, typeset
  text          TEXT NOT NULL,
  "check"       JSONB,                         -- terms, pattern, required_terms; NULL = guidance
  warning       TEXT,                          -- why a check couldn't be made from the text
  source_doc    TEXT REFERENCES documents (id) ON DELETE SET NULL,
  source_anchor TEXT,                          -- the heading it sits under
  source_index  INTEGER,                       -- its place in that document
  status        TEXT NOT NULL CHECK (status IN ('proposed', 'approved', 'retired')),
  created_by    TEXT NOT NULL,
  approved_by   TEXT,
  approved_at   TIMESTAMPTZ,
  retired_at    TIMESTAMPTZ,
  created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX rules_space ON rules (space_id) WHERE status <> 'retired';
CREATE INDEX rules_source ON rules (source_doc) WHERE status <> 'retired';

-- A studio rule that doesn't apply to one client, and why.
CREATE TABLE rule_exceptions (
  space_id   TEXT NOT NULL REFERENCES spaces (id) ON DELETE CASCADE,
  rule_id    TEXT NOT NULL REFERENCES rules (id) ON DELETE CASCADE,
  reason     TEXT NOT NULL,
  created_by TEXT NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (space_id, rule_id)
);

-- The work departs from (or settles) something in the docs, and why.
CREATE TABLE decisions (
  id            TEXT PRIMARY KEY,              -- dec_<ulid>
  space_id      TEXT NOT NULL REFERENCES spaces (id) ON DELETE CASCADE,
  statement     TEXT NOT NULL,
  reason        TEXT NOT NULL,
  contradicts   JSONB NOT NULL DEFAULT '[]',   -- [{doc (id), path, anchor}]
  applies_to    TEXT[] NOT NULL,
  evidence      JSONB NOT NULL DEFAULT '[]',   -- [{app, url, item?, note?}]
  status        TEXT NOT NULL CHECK (status IN ('proposed', 'approved', 'rejected', 'superseded')),
  supersedes    TEXT REFERENCES decisions (id),
  superseded_by TEXT REFERENCES decisions (id),
  proposed_by   TEXT NOT NULL,
  decided_by    TEXT,
  decided_at    TIMESTAMPTZ,
  created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX decisions_space ON decisions (space_id, created_at DESC);
