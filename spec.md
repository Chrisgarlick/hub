# Hub: specification

Working name: **Hub** (rename freely). Status: draft v0.2 (2026-11-09). The build order lives in `plan.md`.

Hub is where a client job starts and what every step checks against. It holds **what you know about each client** (briefs, voice, constraints, decisions, writing rules) as markdown, makes it searchable by meaning, and gives Claude one MCP to start from. The work itself stays in the apps that do it: Board plans, Design designs, Motion animates, Typeset writes the PDF.

---

## 1. What it does, and doesn't

**Does**
- Keeps a **space per client** from Clients, created when the client is, with starter documents.
- Stores **markdown documents** with full history.
- Records **rules** (what must, mustn't or should be done for a client, and where) and **decisions** (where the work departs from the docs, and why), each with its source and whether you've approved it.
- **Searches by meaning and by words** (pgvector plus full-text), returning snippets with where they came from.
- **Checks** a draft against a client's rules before it ships: hard rules by code, the rest as guidance for Claude.
- Treats every change Claude makes as a **proposal** you approve, with a switch to let chosen kinds of change through automatically.
- Tracks **runs**: a job across apps (plan, design, animate, document), its steps, links to what each step made, and where it's waiting for you.
- Knows which **artefacts** belong to a client: boards, Design files, Motion scenes, PDFs.
- Keeps a **library** of reusable files the apps work from (Design's style references first), so any app or Claude session can pull what it needs (section 6A).

**Doesn't**
- Hold brands, assets, keys or the client list: that's **Clients**.
- Plan, design, animate or render: that's Board, Design, Motion and Typeset. Hub links to their work; it doesn't copy it.
- Proxy other apps' MCP tools (section 8.2).
- Have users or roles yet: it's for you. A client portal later reads the same documents (section 11).

### The apps and their jobs

```
            Clients  ── clients, brands, keys, sign-in ──┐
               │ events (client.created, …)              │
               ▼                                         │
   Claude ──▶ Hub ── context, rules, decisions, runs     │
     │         ▲                                         │
     │         │ links, checks                           │
     ├──▶ Board    (the plan: flows, schema, sitemap)  ◀─┤
     ├──▶ Design   (screens, components)               ◀─┤
     ├──▶ Motion   (animation)                         ◀─┤
     └──▶ Typeset  (PDFs)                              ◀─┘
```

Claude Code connects to every app's MCP (they're all at user scope). Hub is the first call of every job and the place each step reports back to.

## 2. Shape

| Concern | Choice |
|---|---|
| Language | Rust (stable): Axum, Tokio, sqlx, rmcp (same as Board and Design) |
| Storage | **Postgres 16 with pgvector**, on the Kritano droplet's existing Postgres (a `hub` database and role) |
| Search | pgvector (HNSW, cosine) plus Postgres full-text (`tsvector`), fused by reciprocal rank |
| Embeddings | An embedding API behind a small trait (section 6.2); Voyage to start |
| UI | Preact with signals and a markdown editor, like Board |
| Binary | One static binary: `hub serve`, `hub admin …`, `hub reindex`, `hub backup` |
| Host | The Kritano droplet, systemd, nginx and certbot, `hub.kritano.com`, built by GitHub Actions like Board and Motion |
| Auth | **Passkeys** for the UI (Sign in with Clients to get in the first time); **Clients keys** for the API and MCP (section 9) |

## 3. Spaces and documents

### 3.1 Spaces

- One space per client, keyed by its Clients slug. Hub polls Clients' events (`GET /v1/events`, every 30s, cursor stored) and:
  - `client.created` → create the space and its starter documents. **Needs a small change in Clients:** it emits `client.deleted` but not `client.created` today.
  - `client.deleted` → archive the space (kept 30 days, then purged), never a silent delete.
  - Hub also reconciles against `GET /v1/clients` on start, so a missed event can't lose a space.
- One **studio space** (`_studio`) for what applies to every client: your house style, how you write proposals, things you never say.

### 3.2 Documents

- Markdown with a small YAML frontmatter: `type`, `title`, `visibility` (`internal` | `client`, for the portal later; default `internal`).
- Starter documents per client (templates, editable): **Brief**, **Audience**, **Voice and tone**, **Constraints** (budget, deadlines, tech, legal), **Writing rules** (section 4), **Decisions** (a generated view of section 5).
- Paths like `acme/brief.md`, unique per space. Folders are just path prefixes.
- Every save is a revision (who: you or Claude; when; a summary). Soft delete with a trash, as in Board.
- Headings get stable anchors (`#delivery-areas`), so rules, decisions and search results can point into a document and survive edits that keep the heading.

## 4. Rules

Rules are the things that must hold for a client, written so both Claude and code can use them.

```json
{
  "id": "rul_…",
  "space": "acme",
  "kind": "must_not",                 // must | must_not | prefer | avoid
  "applies_to": ["typeset", "design"],// any of: board, design, motion, typeset, all
  "text": "Don't mention the 2024 data breach in client-facing documents.",
  "check": { "terms": ["breach", "data leak"], "case_sensitive": false },
  "source": { "doc": "acme/writing-rules.md", "anchor": "sensitive-topics" },
  "status": "approved",               // proposed | approved | retired
  "created_by": "user",
  "approved_at": "2026-11-09T10:00:00Z"
}
```

- Most rules live in the client's **Writing rules** document, as list items under headings; Hub parses them into rule rows on save, so you write prose and still get structure. Rules can also be added directly (by you, or proposed by Claude).
- `check` is optional and deterministic: `terms` (whole words or phrases), `pattern` (a regular expression), or `required_terms` for a `must`. A rule with a check is a **hard rule**: code decides. One without is **guidance**: Claude decides, with the rule in front of it.
- `_studio` rules apply to every client unless a client rule overrides them by id.

## 5. Decisions

A decision records that the work departs from (or settles) something in the docs, and why. This is the "against the docs, but for a good reason" case.

```json
{
  "id": "dec_…",
  "space": "acme",
  "statement": "Checkout lets guests buy without an account.",
  "reason": "Two thirds of orders are gifts from one-off buyers (brief, Audience).",
  "contradicts": [{ "doc": "acme/constraints.md", "anchor": "accounts" }],
  "applies_to": ["board", "design"],
  "evidence": [{ "app": "board", "url": "https://board.kritano.com/boards/…", "item": "01M6…" }],
  "status": "approved",               // proposed | approved | rejected | superseded
  "superseded_by": null,
  "proposed_by": "claude",
  "decided_at": "2026-11-09T10:05:00Z"
}
```

### Trust, not a confidence number

A similarity score says how **relevant** a snippet is, not whether it's **true**. So results carry two separate things:

- **`score`**: how well the snippet matches the query (from search, 0–1).
- **`standing`**: how much to rely on it, from facts Hub knows rather than a number anyone guesses:
  - `authoritative`: an approved rule or decision, or a document section you wrote or approved.
  - `tentative`: a proposal not yet approved, or a section Claude wrote that you haven't reviewed.
  - `outdated`: superseded, retired, or contradicted by a later approved decision.

When a document says one thing and an approved decision says another, search returns both, with the decision marked as overriding that section. Claude is told which wins and why.

## 6. Search

### 6.1 Chunks

- Each document is split at headings into chunks of roughly 200–800 tokens (long sections split on paragraphs, with the heading path kept as context: `Constraints › Accounts`).
- Rules and decisions are indexed too, one chunk each.
- Each chunk row holds: space, source (document and anchor, or rule or decision id), revision, text, heading path, `tsvector`, embedding, standing.
- Chunks are rebuilt from a document on save, by a background job; unchanged chunks (same text hash) keep their embedding, so small edits are cheap.

### 6.2 Embeddings

- An `Embedder` trait with one method (`embed(texts) -> vectors`), so the provider can change. Start with Voyage (`voyage-3.5`, 1024 dimensions; documents and queries embedded with their matching input types). The provider and model are recorded per chunk; changing them means `hub reindex`.
- Text leaves the droplet to be embedded. Anything marked `visibility: secret` (frontmatter) is indexed for words only, never sent out.

### 6.3 Queries

`search(space, query, filters)`:

1. Full-text and vector search in parallel (top 50 each), within the space (plus `_studio`), filtered by document type, `applies_to` and standing.
2. Reciprocal rank fusion, then outdated chunks drop below anything current.
3. Up to 10 snippets, each with: text, source (document path, heading, anchor, revision; or rule or decision), `score`, `standing`, and any approved decision that overrides it.

## 6A. Library

Reusable files the apps work from, kept in one place so anything can use them: Design's style references today, other kinds later. **Hub stores and serves the files; each app keeps what it does with them.**

### 6A.1 Items

```json
{
  "kind": "design-style",              // the app-defined kind
  "name": "elevenlabs",                // lowercase, unique per kind and scope
  "scope": "_studio",                  // _studio (everyone) or a client slug
  "title": "ElevenLabs",               // from the file's first `# ` line
  "summary": "Calm, editorial, generous spacing…",  // from its first `> ` line
  "tags": ["editorial", "dark"],
  "source": "Refero Styles",           // where it came from, free text
  "body": "# ElevenLabs\n> …",         // markdown (DESIGN.md), up to 200 KB
  "version": 3
}
```

- **Kinds are named by the app that uses them** (`design-style`, later perhaps `typeset-outline`, `motion-template`, `board-template`, `prompt`). Hub doesn't interpret the body; it only validates the envelope (name, size, markdown).
- **Scope:** most items are `_studio` (yours, for any client). A client can have its own (a style they asked for) or **pin** studio items (`acme` uses `elevenlabs` and `linear`), which `client_context` then lists.
- Versioned like documents; a change by Claude is a proposal (section 7).
- Indexed for search (section 6), so "find a calm editorial style with big type" works across the library. A style's own text is embedded; its images, if any, aren't.
- Library items aren't client documents: they're not part of a client's rules or decisions, and a portal never shows them.

### 6A.2 Design's styles move here

Design keeps styles in its own table today (Design D39, phase 13). With Hub:

- Hub is the home of `design-style` items. Design's `styles`, `style` and `style_put` tools (and its Styles panel) read and write through Hub's API with Design's Clients key, so they work exactly as now for Claude in Design.
- Design keeps what's Design's: the guide that maps a style's terms onto its roles, spacing scale, radii and components, and the rule to borrow layout and type but keep the client's colours, fonts and logos.
- Moving is one import: Design's existing styles become `_studio` items with the same names, then Design's table is dropped.
- If Hub is unreachable, Design can keep a read cache of the last copy of each style, so a session isn't blocked.

## 7. Changes, approval and what waits

Every change that doesn't come from you in the UI is recorded as a **proposal**: a document edit (a diff against the revision Claude read), a new document, a rule, a decision, a library item or pin, a link to work in another app, or a client added or renamed (in Clients too).

```json
{ "id": "prp_…", "space": "acme", "kind": "doc_edit", "target": "doc_…", "base_version": 12,
  "change": { … }, "diff": "…", "rationale": "Board's checkout now allows guests (decision dec_…).",
  "sources": [ … ], "proposed_by": "claude-code", "status": "applied", "decided_by": "auto" }
```

- **Creates and updates apply at once** (decided by you, 2026-11-09): recorded as "claude-code, auto", shown as not reviewed (`tentative`) until you edit or approve them, and revertible from Approvals.
- **Removals always wait for you**: deleting a document, retiring a rule or decision, unlinking work, deleting a library item, deleting a client. So does any change to a `visibility: client` document.
- **Per space, per kind, you can make a create or update wait** (`review`), for every client or one. The defaults are `auto`.
- Approving is one click in the UI's queue (with the diff and sources). **Removals and client-visible documents are approved in Hub only**: asking Claude to delete something isn't approving the delete (2026-11-10, after a test where "…and then delete the test one" was taken as approval). For a create or update you've set to wait, Claude may pass on your approval with `approve_proposal`, quoting your words.
- Clients are managed through Clients' API with Hub's key (`CLIENTS_MANAGERS`); Hub deletes a client there only once you've approved it.

## 8. MCP

Mounted at `/mcp`, Streamable HTTP, bearer auth with a Clients key (section 9).

### 8.1 Tools

| Tool | Purpose |
|---|---|
| `client_context` | **First call of a job.** For a client: the brief in short, key rules (hard first), recent approved decisions, open proposals, the brand (link to Clients), and the artefacts (boards, Design files, scenes, PDFs) with links |
| `search` | Section 6.3 |
| `get_document` / `list_documents` | Read documents (with revision) |
| `check` | `check(space, text, for: "typeset" \| "design" \| "board" \| "motion")`: hard-rule violations (with the rule and where in the text), plus relevant guidance and decisions for Claude to weigh. Run before publishing anything |
| `get_rules` | The rules for a space and target, hard and guidance |
| `edit_document`, `create_document`, `delete_document`; `create_rule`, `update_rule`, `retire_rule`; `record_decision`, `update_decision`, `retire_decision` | Changes (section 7): creates and updates apply at once; removals wait for you |
| `create_client`, `update_client`, `delete_client` | Clients, in Clients and Hub; deleting waits for you |
| `approve_proposal` / `reject_proposal` | Only when you've said so in the conversation |
| `link_artefact`, `unlink_artefact` | Record that a board, Design file, scene or PDF belongs to a client and a run (unlinking waits for you) |
| `library_list`, `library_get` | Library items by kind and scope, with a client's pins (section 6A) |
| `library_put`, `library_delete` | Save or change an item (at once), or delete one (waits for you) |
| `library_pin` | Pin a studio item for a client |
| `start_run`, `update_run`, `checkpoint` | Section 10 |

Tool descriptions tell Claude the order: `client_context` first; `check` before anything leaves (a PDF, a design handoff); a decision whenever the work departs from the docs; never `approve_proposal` without your say-so.

### 8.2 Why Hub doesn't wrap the other apps

Re-exposing Board's, Design's, Motion's and Typeset's tools through Hub would put a hundred-plus tools in front of Claude and make it choose worse. Each app keeps its own MCP. Hub adds what cuts across them: context, rules, decisions, approvals, runs and links.

## 9. Auth

- **UI:** a passkey (WebAuthn, ES256, as in Board), so Hub works from your phone. Sign in with Clients (handoff, as Motion does) gets you in the first time to add a passkey, and stays as a fallback. Hub keeps its own session cookie either way. Only a browser session can add or remove passkeys, never a key. Clients needs Hub registered as an app origin.
- **API and MCP:** Clients keys, checked with `POST /v1/auth/introspect` (cached 60s). Claude Code gets its own key; a revoked key stops working within a minute. (Board still uses its own tokens; moving it onto Clients keys too is Design's open question 5.)
- Every write records who: `user` (UI), or the key's name (`claude-code`, …).

## 10. Runs

A run is one job across the apps: "plan and design Acme's booking site", "monthly report for Zaltek".

```
run: Acme booking site                                    waiting for you
  1 context     Hub        ✓  client_context
  2 plan        Board      ✓  board 01M6… (flows, schema, sitemap)
  3 docs        Hub        ⏸  proposal prp_… (constraints: guest checkout)   ← checkpoint
  4 design      Design     ·
  5 animate     Motion     ·
  6 handoff     Design     ·
  7 proposal    Typeset    ·  check → render
```

- Steps have an app, an action, a status (`pending`, `running`, `done`, `waiting`, `failed`), links to what they made, and notes.
- A **checkpoint** pauses the run until you approve it in Hub (or send it back with a note, which Claude reads when it picks the run up). Claude can't approve one: a later step can't start while a checkpoint waits (2026-11-10).
- **v1: Claude Code drives.** You start a job in a session; Claude calls `start_run`, does each step in the right app, records it with `update_run`, and stops at checkpoints. Hub records and gates; it doesn't run anything itself.
- **Later: a runner.** Hub (or a small worker beside it) runs the same steps with the Claude Agent SDK, connected to the same MCPs, on a schedule or a trigger. Nothing in the data model changes.

## 11. A client portal, later

Not built now, but designed for: `visibility` on documents (and on rules and decisions), the space-per-client model, and artefact links are what a portal would show. Clients' sign-in would need client users then; Hub would serve read-only, client-visible pages.

## 12. Security

- Hub only calls out to Clients (events, introspection, handoff) and the embedding provider. It never fetches a URL from a document.
- Markdown is rendered with a safe renderer (no raw HTML), as in Board.
- `secret` documents never leave the droplet (section 6.2).
- Backups: nightly `pg_dump` of the `hub` database (with its embeddings, so a restore needs no re-embedding), 14 days on the droplet, copied off it.

## 13. Open questions

1. **Embedding provider.** Voyage is the starting point (Anthropic recommends it, and it has code and document models). A local model on the droplet would keep text in-house but needs memory the 2 GB droplet doesn't have.
2. **pgvector on the droplet.** The Kritano droplet runs Postgres 16; it needs the `postgresql-16-pgvector` package. Check memory: the HNSW index for a few thousand chunks is small, but the droplet is shared.
3. **Typeset checking for itself.** v1 has Claude call `check` before rendering. Typeset could also call Hub's `check` for documents with a `client`, so a hard rule holds even when Claude forgets. Worth it once Hub exists.
4. **Board and Design reading Hub.** Board could show a client's rules beside a board, or flag a card that breaks one. Nice, not needed for v1.
5. **More library kinds.** Candidates: Typeset report outlines, Motion scene templates, Board starter boards, prompts and skills. Each is the owning app's to define; add them when an app wants one.
6. **Name.**
