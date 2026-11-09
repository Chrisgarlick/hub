-- Phase 3 (spec section 6): search. Documents are split into chunks at
-- headings; rules and decisions are a chunk each. Each chunk is found by
-- its words (tsv) and, unless it's secret, by meaning (embedding).
CREATE TABLE chunks (
  id           BIGSERIAL PRIMARY KEY,
  space_id     TEXT NOT NULL REFERENCES spaces (id) ON DELETE CASCADE,
  source       TEXT NOT NULL CHECK (source IN ('document', 'rule', 'decision')),
  doc_id       TEXT REFERENCES documents (id) ON DELETE CASCADE,
  rule_id      TEXT REFERENCES rules (id) ON DELETE CASCADE,
  decision_id  TEXT REFERENCES decisions (id) ON DELETE CASCADE,
  ord          INTEGER NOT NULL DEFAULT 0,      -- order within its document
  revision     INTEGER,                         -- the document's version
  heading_path TEXT NOT NULL,                   -- "Constraints › Accounts"
  anchors      TEXT[] NOT NULL DEFAULT '{}',    -- the headings it sits under, outermost first
  text         TEXT NOT NULL,
  text_hash    TEXT NOT NULL,                   -- of what's embedded: unchanged chunks keep it
  doc_type     TEXT NOT NULL,                   -- the document's type, or 'rule' / 'decision'
  applies_to   TEXT[] NOT NULL DEFAULT '{all}',
  status       TEXT,                            -- a rule's or decision's status
  written_by   TEXT NOT NULL,                   -- who last changed this chunk's text
  secret       BOOLEAN NOT NULL DEFAULT false,  -- never sent out to be embedded
  tsv          TSVECTOR GENERATED ALWAYS AS
                 (to_tsvector('english', heading_path || ' ' || text)) STORED,
  embedding    vector(1024),
  embed_model  TEXT,                            -- provider/model that made the embedding
  created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX chunks_space ON chunks (space_id);
CREATE INDEX chunks_doc ON chunks (doc_id);
CREATE INDEX chunks_tsv ON chunks USING gin (tsv);
CREATE INDEX chunks_embedding ON chunks USING hnsw (embedding vector_cosine_ops);
CREATE INDEX chunks_unembedded ON chunks (id) WHERE embedding IS NULL AND NOT secret;

-- Work for the indexer: rebuild a document's chunks, or a space's rules and
-- decisions. One row per thing, however often it changes before it runs.
CREATE TABLE index_queue (
  kind       TEXT NOT NULL CHECK (kind IN ('document', 'space')),
  target     TEXT NOT NULL,
  queued_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
  attempts   INTEGER NOT NULL DEFAULT 0,
  next_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
  last_error TEXT,
  PRIMARY KEY (kind, target)
);

-- Everything that exists now gets indexed.
INSERT INTO index_queue (kind, target)
  SELECT 'document', id FROM documents WHERE deleted_at IS NULL
  UNION ALL SELECT 'space', id FROM spaces;
