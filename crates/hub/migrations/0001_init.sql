-- Hub's database (spec section 2): Postgres 16 with pgvector. Spaces,
-- documents, rules, decisions, chunks and the rest arrive from phase 1.
CREATE EXTENSION IF NOT EXISTS vector;
