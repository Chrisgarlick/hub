-- Phase 5 (spec section 10): runs. A job across the apps, step by step,
-- pausing at checkpoints until you approve them.
CREATE TABLE runs (
  id          TEXT PRIMARY KEY,                 -- run_<ulid>
  space_id    TEXT NOT NULL REFERENCES spaces (id) ON DELETE CASCADE,
  title       TEXT NOT NULL,
  goal        TEXT,
  status      TEXT NOT NULL CHECK (status IN ('running', 'waiting', 'done', 'failed', 'cancelled')),
  notes       TEXT,                             -- how it ended
  created_by  TEXT NOT NULL,
  created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
  finished_at TIMESTAMPTZ
);
CREATE INDEX runs_space ON runs (space_id, updated_at DESC);

CREATE TABLE run_steps (
  run_id      TEXT NOT NULL REFERENCES runs (id) ON DELETE CASCADE,
  ord         INTEGER NOT NULL,                 -- 1, 2, 3…
  app         TEXT NOT NULL CHECK (app IN ('hub', 'board', 'design', 'motion', 'typeset', 'other')),
  action      TEXT NOT NULL,                    -- "Plan the site in Board"
  checkpoint  BOOLEAN NOT NULL DEFAULT false,   -- wait for you when it's done
  status      TEXT NOT NULL CHECK (status IN ('pending', 'running', 'done', 'waiting', 'failed', 'skipped')),
  notes       TEXT,
  links       JSONB NOT NULL DEFAULT '[]',      -- [{title, url}]
  review      TEXT,                             -- at a checkpoint: what to look at
  feedback    TEXT,                             -- your note when you sent it back
  approved_by TEXT,
  approved_at TIMESTAMPTZ,
  started_at  TIMESTAMPTZ,
  finished_at TIMESTAMPTZ,
  PRIMARY KEY (run_id, ord)
);
