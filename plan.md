# Hub: build plan

How to get Hub built, in order. The spec is `spec.md`; section numbers (§) refer to it. Each phase ends in something you can use.

Rust throughout (Axum, Tokio, sqlx, rmcp), deployed like Board and Motion: a static binary built by GitHub Actions, systemd, nginx and certbot on the Kritano droplet, at `hub.kritano.com`.

---

## Decisions to make, and when

| Decide | Before | Spec | Notes |
|---|---|---|---|
| ~~Where docs live~~ | — | §1 | **Decided:** a new app; Clients stays the home of clients, brands and keys |
| ~~Who uses it~~ | — | §1, §11 | **Decided:** just you; a client portal later reads the same documents |
| ~~Human sign-off~~ | — | §7 | **Decided:** always, by default, with a switch that lets chosen kinds through; hands-off can be built later without a rewrite |
| ~~Language~~ | — | §2 | **Decided:** Rust |
| Embedding provider | Phase 3 | §13 Q1 | Recommendation: Voyage (`voyage-3.5`) |
| pgvector on the droplet | Phase 0 | §13 Q2 | Install `postgresql-16-pgvector`; check memory headroom |
| Name | Phase 0 | §13 Q6 | "Hub" until something better |

---

## Phase 0: Setup

**Goal:** an empty app that builds, tests, runs and deploys.

- [ ] Cargo workspace: `crates/hub` (server, CLI), `crates/hub-model` (the types other apps may use: rules, decisions, search results).
- [ ] Postgres: a `hub` database and role on the droplet; `CREATE EXTENSION vector`; sqlx migrations; locally, Postgres with pgvector in Docker.
- [ ] Config file (`hub.toml`): bind address, database URL, public origin, Clients base URL and service key, embedding provider and key.
- [ ] CLI skeleton: `hub serve`, `hub admin`, `hub reindex`, `hub backup`.
- [ ] Frontend toolchain: Preact with signals, esbuild, embedded in release builds (as Board).
- [ ] `/healthz` and `/readyz` (database, Clients reachable).
- [ ] Local CI (`scripts/ci.sh`, pre-push hook), Release workflow (static musl binary), `deploy/` (service, nginx site, install and deploy scripts), as Board's.

**Done when:** `hub.kritano.com/healthz` answers from the droplet.

**Status:** done (2026-11-09). Live at https://hub.kritano.com; `/readyz` reports the database, pgvector and Clients all reachable.

---

## Phase 1: Spaces and documents

**Goal:** every client has a space of markdown documents you can edit.

- [x] **Clients change:** emit a `client.created` event (§3.1), and register Hub as an app for Sign in with Clients.
- [x] Sign in with Clients for the UI (§9); Clients keys for the API (introspection, cached 60s).
- [x] Passkeys for the UI (as Board), so Hub works on your phone; Sign in with Clients gets you in the first time.
- [x] Spaces from Clients' events and a reconcile against `GET /v1/clients` on start; archive on `client.deleted`; the `_studio` space.
- [x] Starter documents from templates: Brief, Audience, Voice and tone, Constraints, Writing rules (§3.2).
- [x] Documents: frontmatter (`type`, `title`, `visibility`), paths, stable heading anchors, revisions, trash.
- [x] REST API for spaces and documents.
- [x] UI: space list, document list, editor (markdown with preview, no raw HTML), history, trash.
- [x] Tests: event handling is idempotent, reconcile finds missed clients, anchors survive edits that keep the heading, revisions and restore.

**Done when:** you create a client in Clients and its space, with starter documents, appears in Hub within a minute; you can edit, see history and restore.

**Status:** built (2026-11-09). Hub's side is deployed; it follows Clients once Clients' `client.created` build is installed and Hub is registered and keyed (`deploy/README.md`, "Connecting to Clients"). Spaces also reconcile against Clients' list, which archives a deleted client's space if its event was missed, so a slug can be reused.

---

## Phase 2: Rules and decisions

**Goal:** what must hold for a client is written once and usable by code and by Claude.

- [x] Rules (§4): parsed from Writing rules on save, or added directly; kinds, `applies_to`, optional deterministic `check` (terms, pattern, required terms); `_studio` rules and client overrides.
- [x] Decisions (§5): statement, reason, what it contradicts, evidence links, status, supersedes.
- [x] Standing (§5): authoritative, tentative, outdated, derived from status and who wrote or approved what.
- [x] `check` (§8.1): hard-rule violations with positions; guidance and decisions for the target.
- [x] UI: rules and decisions per space; the Decisions document as a generated view.
- [x] Tests: rule parsing from prose, term and pattern matching (whole words, case), overrides, standing.

**Done when:** a Writing rules document with "don't mention X" makes `check` flag X in a draft, and a decision shows as overriding the section it contradicts.

**Status:** built (2026-11-09). How rules are written is in `crates/hub/src/rules.rs` (and the Writing rules template): a bullet per rule, its first word for the kind (Don't, Always, Avoid, Prefer), its heading for where it applies, exact words in "double quotes" for the check. Rules and decisions from a key are proposed and need your approval in the UI; `check` enforces approved rules only. A studio rule can be switched off for one client, with a reason.

---

## Phase 3: Search

**Goal:** find what the docs say by meaning, with where it came from and how far to trust it.

- [ ] Chunking at headings (§6.1), text hashes so unchanged chunks keep their embeddings; a background job queue.
- [ ] `Embedder` trait, Voyage implementation, provider and model recorded per chunk; `hub reindex`.
- [ ] Hybrid search (§6.3): full-text and HNSW in parallel, reciprocal rank fusion, outdated below current, overriding decisions attached.
- [ ] `secret` documents indexed for words only (§6.2).
- [ ] UI: search across a space or everything.
- [ ] Tests: fusion ordering, filters, a decision overriding a section, secrets never embedded, a fake embedder for repeatable tests.

**Done when:** "can guests check out?" finds the Constraints section and the approved decision that overrides it, marked so.

---

## Phase 4: MCP and proposals

**Goal:** Claude starts every job in Hub and can't change anything without you.

- [ ] MCP at `/mcp` (§8): `client_context`, `search`, documents, `check`, `get_rules`, proposals, `approve_proposal`/`reject_proposal`, `link_artefact`.
- [ ] Proposals (§7): document edits as diffs against a revision (refused if the document moved on), rules, decisions, artefact links.
- [ ] Approval policy per space and kind, the hands-off switch (off), and the limits that always stay manual.
- [ ] Artefact links: boards, Design files, Motion scenes, PDFs per client.
- [ ] UI: the approvals queue (diff, rationale, sources, approve or reject), the switch and policies in settings.
- [ ] Tests: nothing applies without approval under `review`; `auto` applies and logs; the hard limits ignore the switch; stale diffs are refused.

**Done when:** in a Claude Code session, Claude reads a client's context, plans a board in Board, proposes a docs change to match it, and the change waits in Hub until you approve it.

---

## Phase 4A: Library

**Goal:** Design's style references (and later other reusable files) live in Hub, available to any app or session.

- [ ] Library items (§6A.1): kind, name, scope, title and summary from the markdown, tags, source, body, versions; pins per client.
- [ ] REST API for items; MCP `library_list`, `library_get`, `library_put` (a proposal), `library_pin`; pinned items in `client_context`.
- [ ] Search over the library (§6), filtered by kind.
- [ ] UI: the library, by kind; paste a file, edit, history, pin to clients.
- [ ] `hub admin import-styles`: bring Design's existing styles across (§6A.2).
- [ ] **Design change** (its repo): its styles tools and panel read and write through Hub, with a read cache; drop its `styles` table after the import.
- [ ] Tests: names unique per kind and scope, pins, versions, a Claude change waiting for approval, import is repeatable.

**Done when:** "design Kritano's homepage in the elevenlabs style" works in Design exactly as now, with the style coming from Hub, and the same style can be pulled from any other session.

---

## Phase 5: Runs

**Goal:** a job across the apps is one thing you can follow, pause and pick up.

- [ ] Runs and steps (§10), checkpoints, links to each step's output.
- [ ] MCP `start_run`, `update_run`, `checkpoint`; tool descriptions for the pipeline order (context, plan, docs, design, animate, handoff, check, PDF).
- [ ] UI: runs per client, a run's timeline, waiting checkpoints alongside proposals.
- [ ] A Claude Code skill (or prompt) for the full pipeline that uses Hub, Board, Design, Motion and Typeset together.
- [ ] Tests: checkpoint gating, resuming a run in a new session from its record.

**Done when:** one session takes a client from brief to proposal PDF through every app, stopping for you at each checkpoint, and the run in Hub shows every step and link.

---

## Phase 6: Later

- [ ] Typeset calls `check` for documents with a client, so hard rules hold even if Claude forgets (§13 Q3).
- [ ] Board and Design show a client's rules alongside the work (§13 Q4).
- [ ] The runner: Hub runs steps itself with the Claude Agent SDK, on a schedule or trigger, using the same policies (§10).
- [ ] A client portal: client users in Clients, read-only client-visible pages in Hub (§11).

---

## Outside this repo

- **Clients:** a `client.created` event; Hub registered for Sign in with Clients; a key for Hub (service) and one for Claude Code's Hub MCP (caller).
- **Droplet:** `postgresql-16-pgvector`, a `hub` database and role.
- **Design:** its styles move to Hub (phase 4A): its tools and panel read and write through Hub's API.
- **Board, Motion, Typeset:** nothing needed for v1. Each already has its own MCP and knows its client by slug; library kinds for them can come later.
