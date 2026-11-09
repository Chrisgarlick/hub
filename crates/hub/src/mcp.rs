//! The MCP server at `/mcp` (spec section 8): where Claude starts every job.
//! Reading is free; every change is a proposal that waits for you (or for
//! the hands-off switch). Errors come back as tool errors carrying the REST
//! API's JSON (`error`, `message`, `path`), so Claude can correct itself.
//!
//! Auth is a Clients caller key (`Authorization: Bearer ck_…`), as for the
//! API; what Claude does is recorded as the key's name.

use std::sync::Arc;

use axum::Router;
use axum::http::request::Parts;
use axum::middleware;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{Implementation, ServerCapabilities, ServerConfig};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::app::AppState;
use crate::auth::{self, Caller};
use crate::decisions::{Evidence, NewDecision, SectionRef};
use crate::error::AppError;
use crate::proposals::{self, Edit};
use crate::rules::{self, NewRule};
use crate::{decisions, documents, library, routes, search, spaces};
use hub_model::Check;

const INSTRUCTIONS: &str = "Hub holds what you know about each client: their brief, audience, \
voice, constraints and writing rules (as documents), the rules that must hold, the decisions \
that depart from the docs, the library (Design's styles), and links to the work made for them \
in Board, Design, Motion and Typeset. Start every job for a client with client_context. Use \
search to find what the docs say (each result says how far to rely on it: authoritative, \
tentative or outdated; an outdated section comes with the decision that overrides it). Run \
check on anything before it leaves (a PDF, a design handoff, copy): hard-rule violations must \
be fixed, guidance weighed. When the work departs from the docs, record_decision with the \
reason. Creating and changing things (clients, documents, rules, decisions, links, library \
items) takes effect at once and is recorded for the user, who can revert it. Removing \
anything (delete_document, retire_rule, retire_decision, unlink_artefact, library_delete, \
delete_client) waits for the user's approval: tell them the proposal id. Only call \
approve_proposal when the user has told you, in this conversation, to approve that proposal; \
quote their words in user_said.";

type ToolResult = Result<String, String>;

fn ok(value: Value) -> ToolResult {
    serde_json::to_string_pretty(&value).map_err(|e| e.to_string())
}

fn fail(err: AppError) -> String {
    err.body().to_string()
}

fn caller(parts: &Parts) -> Result<Caller, String> {
    parts
        .extensions
        .get::<Caller>()
        .cloned()
        .ok_or_else(|| fail(AppError::Unauthorized))
}

// ---------- Parameters ----------

#[derive(Deserialize, JsonSchema)]
pub struct Client {
    /// The client's slug (from list_clients), or `_studio` for what applies to every client.
    pub client: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct Context {
    /// The client's slug.
    pub client: String,
    /// The app the work is for (board, design, motion, typeset): rules and decisions for it only.
    #[serde(default, rename = "for")]
    pub target: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct Search {
    pub query: String,
    /// A client's slug (searches it and the studio); none searches every client.
    #[serde(default)]
    pub client: Option<String>,
    /// A document type (brief, audience, voice, constraints, writing-rules, …).
    #[serde(default, rename = "type")]
    pub doc_type: Option<String>,
    /// document, rule or decision.
    #[serde(default)]
    pub source: Option<String>,
    /// An app: only what applies to it.
    #[serde(default, rename = "for")]
    pub target: Option<String>,
    /// How many results (default 10, at most 25).
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
pub struct GetDocument {
    pub client: String,
    /// The document's id or path (e.g. constraints.md).
    pub document: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct CheckText {
    pub client: String,
    pub text: String,
    /// The app it's for: board, design, motion or typeset.
    #[serde(default, rename = "for")]
    pub target: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ProposeEdit {
    pub client: String,
    /// The document's id or path.
    pub document: String,
    /// The version you read (from get_document); refused if it has moved on.
    pub version: i32,
    /// Exact replacements, each `find` appearing once in the document. Prefer these for small changes.
    #[serde(default)]
    pub edits: Vec<Edit>,
    /// Or the whole new document (frontmatter included).
    #[serde(default)]
    pub content: Option<String>,
    /// Rename or move it (a new path).
    #[serde(default)]
    pub path: Option<String>,
    /// Why: what changed, and where it came from (shown to the user).
    #[serde(default)]
    pub rationale: Option<String>,
    /// What it's based on: [{"doc": "brief.md", "anchor": "goals"}, {"decision": "dec_…"}, {"url": "https://…"}].
    #[serde(default)]
    pub sources: Vec<Value>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ProposeNew {
    pub client: String,
    /// e.g. research/interviews.md (lowercase, ending .md).
    pub path: String,
    /// Markdown, optionally with frontmatter (title, type, visibility).
    pub content: String,
    #[serde(default)]
    pub rationale: Option<String>,
    #[serde(default)]
    pub sources: Vec<Value>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ProposeRule {
    pub client: String,
    /// must, must_not, prefer or avoid.
    pub kind: String,
    /// The rule in a sentence, e.g. "Don't mention the 2024 data breach".
    pub text: String,
    /// Where it applies: all (default), board, design, motion, typeset.
    #[serde(default)]
    pub applies_to: Option<Vec<String>>,
    /// Makes it checkable: terms (must not appear), pattern (a regex that mustn't match),
    /// required_terms (for a must), case_sensitive.
    #[serde(default)]
    pub check: Option<Check>,
    #[serde(default)]
    pub rationale: Option<String>,
    #[serde(default)]
    pub sources: Vec<Value>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ProposeDecision {
    pub client: String,
    /// What's decided, in a sentence.
    pub statement: String,
    /// Why, with where the evidence is.
    pub reason: String,
    /// The document sections it overrides: [{"doc": "constraints.md", "anchor": "accounts"}].
    #[serde(default)]
    pub contradicts: Vec<SectionRef>,
    /// Where it applies: all (default), board, design, motion, typeset.
    #[serde(default)]
    pub applies_to: Option<Vec<String>>,
    /// Links to the work it shows up in: [{"app": "board", "url": "https://…", "item": "…"}].
    #[serde(default)]
    pub evidence: Vec<Evidence>,
    /// An approved decision this replaces.
    #[serde(default)]
    pub supersedes: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct LinkArtefact {
    pub client: String,
    /// board, design, motion, typeset or other.
    pub app: String,
    /// What it is: board, design, scene, pdf, …
    pub kind: String,
    pub title: String,
    pub url: String,
    /// The app's own id (e.g. a board id).
    #[serde(default)]
    pub external_id: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ListProposals {
    /// A client's slug; none means every client.
    #[serde(default)]
    pub client: Option<String>,
    /// pending (default), applied, rejected, withdrawn, stale, reverted or all.
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct Approve {
    pub proposal_id: String,
    /// The user's own words approving it, quoted from this conversation.
    pub user_said: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct Reject {
    pub proposal_id: String,
    /// Why (the user's words, if they gave a reason).
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ProposalId {
    pub proposal_id: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct DeleteDocument {
    pub client: String,
    /// The document's id or path.
    pub document: String,
    /// Why (shown to the user, who approves it).
    pub reason: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct UpdateRule {
    pub client: String,
    /// The rule's id (from get_rules). Only rules added directly; a Writing rules line is changed with edit_document.
    pub rule: String,
    /// What changes; the rest stays.
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub applies_to: Option<Vec<String>>,
    #[serde(default)]
    pub check: Option<Check>,
    #[serde(default)]
    pub rationale: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct Removal {
    pub client: String,
    /// The id of what's removed: a rule (rul_…), a decision (dec_…) or linked work (art_…).
    pub id: String,
    /// Why (shown to the user, who approves it).
    pub reason: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct UpdateDecision {
    pub client: String,
    /// The decision's id.
    pub decision: String,
    /// What changes; the rest stays.
    #[serde(default)]
    pub statement: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub contradicts: Option<Vec<SectionRef>>,
    #[serde(default)]
    pub applies_to: Option<Vec<String>>,
    #[serde(default)]
    pub evidence: Option<Vec<Evidence>>,
    #[serde(default)]
    pub rationale: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct LibraryDelete {
    pub kind: String,
    pub name: String,
    /// `_studio` (default) or a client's slug.
    #[serde(default)]
    pub scope: Option<String>,
    pub reason: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct CreateClient {
    /// The client's name, e.g. "Acme Hotels".
    pub name: String,
    /// Lowercase letters, digits and -, starting with a letter: acme-hotels. Other apps use it to find the client.
    pub slug: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct UpdateClient {
    pub client: String,
    /// The new name (the slug stays).
    pub name: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct DeleteClient {
    pub client: String,
    /// Why (shown to the user, who approves it). Deleting removes the client from Clients (brand, library) and archives its space.
    pub reason: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct LibraryList {
    /// e.g. design-style.
    #[serde(default)]
    pub kind: Option<String>,
    /// A client's slug: what that client uses (its own items and pinned studio ones). None: the whole library.
    #[serde(default)]
    pub client: Option<String>,
    /// Find items by meaning and words, e.g. "calm editorial, big type".
    #[serde(default)]
    pub query: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct LibraryGet {
    /// e.g. design-style.
    pub kind: String,
    pub name: String,
    /// A client's slug, to prefer that client's own item of this name (then the studio's).
    #[serde(default)]
    pub client: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct LibraryPutParams {
    /// The app's kind, e.g. design-style.
    pub kind: String,
    /// Lowercase letters, digits and -, e.g. elevenlabs.
    pub name: String,
    /// The whole file (markdown; a style's DESIGN.md). Its first `# ` line is the title, its first `> ` line the summary.
    pub body: String,
    /// `_studio` (default: for every client) or a client's slug (that client's own).
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    /// Where it came from, e.g. "Refero Styles".
    #[serde(default)]
    pub source: Option<String>,
    /// For a change to an existing item: the version you read.
    #[serde(default)]
    pub version: Option<i32>,
    pub rationale: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct LibraryPinParams {
    pub client: String,
    pub kind: String,
    /// A studio item's name.
    pub name: String,
    /// false to unpin.
    #[serde(default)]
    pub pin: Option<bool>,
}

// ---------- The server ----------

#[derive(Clone)]
pub struct HubMcp {
    state: AppState,
    tool_router: ToolRouter<Self>,
}

/// A document's body, cut to about this many characters for client_context.
const BRIEF_CHARS: usize = 2500;

#[tool_router]
impl HubMcp {
    pub fn new(state: AppState) -> Self {
        Self {
            state,
            tool_router: Self::tool_router(),
        }
    }

    async fn conn(&self) -> Result<sqlx::pool::PoolConnection<sqlx::Postgres>, String> {
        self.state.pool.acquire().await.map_err(|e| fail(e.into()))
    }

    /// Make a proposal and say what happened to it.
    async fn propose(
        &self,
        c: &Caller,
        client: &str,
        kind: &str,
        change: Value,
        rationale: &str,
        sources: Vec<Value>,
    ) -> ToolResult {
        let mut tx = self.state.pool.begin().await.map_err(|e| fail(e.into()))?;
        let s = routes::space(&mut tx, c, client).await.map_err(fail)?;
        let p = proposals::create(
            &mut tx,
            self.state.clients.as_ref(),
            &s.id,
            kind,
            change,
            rationale,
            sources,
            &c.actor,
        )
        .await
        .map_err(fail)?;
        tx.commit().await.map_err(|e| fail(e.into()))?;
        self.state.index_wake.notify_one();
        let mut v = p.json();
        v["what_happened"] = json!(match p.status.as_str() {
            "applied" => "Done. It's recorded in Hub, where the user can see and revert it.",
            _ if proposals::is_delete(kind) =>
                "Waiting for the user's approval in Hub (removals always do). Tell them the proposal id.",
            _ =>
                "Waiting for the user's approval in Hub (they've set this kind to wait). Tell them the proposal id.",
        });
        ok(v)
    }

    #[tool(
        description = "Every client (a space each) and the studio, with how many documents and open proposals each has."
    )]
    async fn list_clients(&self, Extension(parts): Extension<Parts>) -> ToolResult {
        let c = caller(&parts)?;
        let mut conn = self.conn().await?;
        let list = spaces::list(&mut conn).await.map_err(fail)?;
        let pending: Vec<(String, i64)> = sqlx::query_as(
            "SELECT space_id, count(*) FROM proposals WHERE status = 'pending' GROUP BY space_id",
        )
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| fail(e.into()))?;
        let out: Vec<Value> = list
            .into_iter()
            .filter(|l| c.may(&l.space.slug))
            .map(|l| {
                let open = pending
                    .iter()
                    .find(|p| p.0 == l.space.id)
                    .map_or(0, |p| p.1);
                json!({ "client": l.space.slug, "name": l.space.name, "kind": l.space.kind,
                        "documents": l.documents, "open_proposals": open })
            })
            .collect();
        ok(json!({ "clients": out }))
    }

    #[tool(
        description = "START HERE for any job for a client: the brief, the documents (with versions), the rules in force (hard ones first), recent decisions, open proposals, the work already linked to them, and where their brand lives. Pass `for` (board, design, motion, typeset) to get only what applies to that app."
    )]
    async fn client_context(
        &self,
        Parameters(p): Parameters<Context>,
        Extension(parts): Extension<Parts>,
    ) -> ToolResult {
        let c = caller(&parts)?;
        let t = routes::target(p.target).map_err(fail)?;
        let mut conn = self.conn().await?;
        let s = routes::space(&mut conn, &c, &p.client)
            .await
            .map_err(fail)?;
        let docs = documents::list(&mut conn, &s.id, false)
            .await
            .map_err(fail)?;
        let brief = match docs.iter().find(|d| d.doc_type == "brief") {
            Some(d) => {
                let full = documents::get(&mut conn, &d.id).await.map_err(fail)?;
                let body = documents::body(&full.content).trim().to_string();
                let cut: String = body.chars().take(BRIEF_CHARS).collect();
                json!({ "document": d.path, "text": cut, "truncated": cut.len() < body.len() })
            }
            None => Value::Null,
        };
        let rules: Vec<Value> = rules::for_space(&mut conn, &s.id)
            .await
            .map_err(fail)?
            .iter()
            .filter(|r| r.rule.status == "approved" && r.excepted.is_none() && r.rule.applies(t.as_deref()))
            .map(|r| {
                json!({ "id": r.rule.id, "kind": r.rule.kind, "text": r.rule.text, "hard": r.rule.is_hard(),
                        "applies_to": r.rule.applies_to,
                        "scope": if r.rule.space_id == spaces::STUDIO { "studio" } else { "client" } })
            })
            .collect();
        let decisions: Vec<Value> = decisions::list(&mut conn, &s.id)
            .await
            .map_err(fail)?
            .iter()
            .filter(|d| d.status == "approved" && d.applies(t.as_deref()))
            .take(15)
            .map(|d| json!({ "id": d.id, "statement": d.statement, "reason": d.reason, "overrides": d.contradicts.0 }))
            .collect();
        let open: Vec<Value> = proposals::list(&mut conn, std::slice::from_ref(&s.id), Some("pending"), 20)
            .await
            .map_err(fail)?
            .iter()
            .map(|p| json!({ "id": p.id, "kind": p.kind, "rationale": p.rationale, "proposed_by": p.proposed_by }))
            .collect();
        let artefacts = proposals::artefacts(&mut conn, &s.id).await.map_err(fail)?;
        let uses: Vec<Value> = library::for_client(&mut conn, &s.id)
            .await
            .map_err(fail)?
            .iter()
            .map(|i| json!({ "kind": i.kind, "name": i.name, "scope": i.scope, "title": i.title, "summary": i.summary }))
            .collect();
        let documents: Vec<Value> = docs
            .iter()
            .map(|d| json!({ "id": d.id, "path": d.path, "title": d.title, "type": d.doc_type, "version": d.version,
                             "visibility": d.visibility }))
            .collect();
        ok(json!({
            "client": { "slug": s.slug, "name": s.name, "kind": s.kind },
            "brief": brief,
            "documents": documents,
            "rules": rules,
            "decisions": decisions,
            "open_proposals": open,
            "artefacts": artefacts,
            "library": uses,
            "brand": if s.kind == "client" {
                json!({ "slug": s.slug, "note": "The brand (colours, fonts, logos) lives in Clients. Board, Design, Motion and Typeset read it by this slug: pass it as the client there." })
            } else { Value::Null },
        }))
    }

    #[tool(
        description = "Search the docs, rules and decisions by meaning and words. Each result has a source (document path and heading, rule or decision), a score (relevance) and a standing (authoritative, tentative, outdated); an outdated section lists the decision that overrides it. Pass client to search that client and the studio."
    )]
    async fn search(
        &self,
        Parameters(p): Parameters<Search>,
        Extension(parts): Extension<Parts>,
    ) -> ToolResult {
        let c = caller(&parts)?;
        let mut conn = self.conn().await?;
        let spaces: Vec<String> = match p.client.as_deref().filter(|s| !s.is_empty()) {
            Some(slug) => {
                let s = routes::space(&mut conn, &c, slug).await.map_err(fail)?;
                if s.id == spaces::STUDIO {
                    vec![s.id]
                } else {
                    vec![s.id, spaces::STUDIO.to_string()]
                }
            }
            None => routes::spaces_for(&mut conn, &c)
                .await
                .map_err(fail)?
                .into_iter()
                .map(|s| s.0)
                .collect(),
        };
        let q = search::Query {
            text: p.query,
            spaces,
            doc_type: p.doc_type,
            source: p.source,
            target: routes::target(p.target).map_err(fail)?,
            standing: None,
            limit: p.limit.unwrap_or(10),
        };
        ok(search::run(&mut conn, self.state.embedder.as_ref(), &q)
            .await
            .map_err(fail)?)
    }

    #[tool(description = "A client's documents: id, path, title, type, version, visibility.")]
    async fn list_documents(
        &self,
        Parameters(p): Parameters<Client>,
        Extension(parts): Extension<Parts>,
    ) -> ToolResult {
        let c = caller(&parts)?;
        let mut conn = self.conn().await?;
        let s = routes::space(&mut conn, &c, &p.client)
            .await
            .map_err(fail)?;
        ok(json!({ "documents": documents::list(&mut conn, &s.id, false).await.map_err(fail)? }))
    }

    #[tool(
        description = "One document in full: content, version (needed to propose an edit), headings with their anchors, and any approved decisions that override its sections."
    )]
    async fn get_document(
        &self,
        Parameters(p): Parameters<GetDocument>,
        Extension(parts): Extension<Parts>,
    ) -> ToolResult {
        let c = caller(&parts)?;
        let mut conn = self.conn().await?;
        let s = routes::space(&mut conn, &c, &p.client)
            .await
            .map_err(fail)?;
        let id: Option<String> = sqlx::query_scalar(
            "SELECT id FROM documents WHERE space_id = $1 AND deleted_at IS NULL AND (id = $2 OR path = $3)",
        )
        .bind(&s.id)
        .bind(&p.document)
        .bind(p.document.trim_start_matches('/').to_lowercase())
        .fetch_optional(&mut *conn)
        .await
        .map_err(|e| fail(e.into()))?;
        let id = id.ok_or_else(|| fail(AppError::NotFound("document")))?;
        let mut d = documents::get(&mut conn, &id).await.map_err(fail)?.json();
        d["overridden_by"] = routes::overrides(&mut conn, &id).await.map_err(fail)?;
        ok(d)
    }

    #[tool(
        description = "Check text against the client's rules (and the studio's) before it leaves: a PDF, copy, a design handoff, a caption. Returns violations of hard rules (with line, column and the matching text, or what's missing), advice from prefer/avoid rules, guidance rules for you to judge, and the decisions in force. ok is false while there are violations: fix them."
    )]
    async fn check(
        &self,
        Parameters(p): Parameters<CheckText>,
        Extension(parts): Extension<Parts>,
    ) -> ToolResult {
        let c = caller(&parts)?;
        let t = routes::target(p.target).map_err(fail)?;
        let mut conn = self.conn().await?;
        let s = routes::space(&mut conn, &c, &p.client)
            .await
            .map_err(fail)?;
        ok(routes::run_check(&mut conn, &s.id, &p.text, t.as_deref())
            .await
            .map_err(fail)?)
    }

    #[tool(
        description = "The rules for a client (with the studio's), for one app or all: hard rules (checked by code) and guidance, with where each came from."
    )]
    async fn get_rules(
        &self,
        Parameters(p): Parameters<Context>,
        Extension(parts): Extension<Parts>,
    ) -> ToolResult {
        let c = caller(&parts)?;
        let t = routes::target(p.target).map_err(fail)?;
        let mut conn = self.conn().await?;
        let s = routes::space(&mut conn, &c, &p.client)
            .await
            .map_err(fail)?;
        let list: Vec<Value> = rules::for_space(&mut conn, &s.id)
            .await
            .map_err(fail)?
            .iter()
            .filter(|r| r.excepted.is_none() && r.rule.applies(t.as_deref()))
            .map(rules::InForce::json)
            .collect();
        ok(json!({ "rules": list }))
    }

    #[tool(
        description = "Change a document: read it with get_document first and pass its version (refused if it has moved on). Prefer edits (exact find/replace, each find unique) for small changes, content for a rewrite, and path to rename or move it. Takes effect at once (recorded for the user, who can revert it); edits to client-visible documents wait for their approval."
    )]
    async fn edit_document(
        &self,
        Parameters(p): Parameters<ProposeEdit>,
        Extension(parts): Extension<Parts>,
    ) -> ToolResult {
        let c = caller(&parts)?;
        let mut change = json!({ "document": p.document, "version": p.version });
        if !p.edits.is_empty() {
            change["edits"] = json!(p.edits);
        }
        if let Some(content) = p.content {
            change["content"] = json!(content);
        }
        if let Some(path) = p.path {
            change["path"] = json!(path);
        }
        let why = p.rationale.unwrap_or_else(|| "Edited by Claude.".into());
        self.propose(&c, &p.client, "doc_edit", change, &why, p.sources)
            .await
    }

    #[tool(
        description = "Create a document for a client (research notes, a sitemap rationale, meeting notes). Takes effect at once."
    )]
    async fn create_document(
        &self,
        Parameters(p): Parameters<ProposeNew>,
        Extension(parts): Extension<Parts>,
    ) -> ToolResult {
        let c = caller(&parts)?;
        let why = p.rationale.unwrap_or_else(|| "Written by Claude.".into());
        self.propose(
            &c,
            &p.client,
            "doc_create",
            json!({ "path": p.path, "content": p.content }),
            &why,
            p.sources,
        )
        .await
    }

    #[tool(description = "Delete a document (to the trash). Waits for the user's approval.")]
    async fn delete_document(
        &self,
        Parameters(p): Parameters<DeleteDocument>,
        Extension(parts): Extension<Parts>,
    ) -> ToolResult {
        let c = caller(&parts)?;
        self.propose(
            &c,
            &p.client,
            "doc_delete",
            json!({ "document": p.document }),
            &p.reason,
            vec![],
        )
        .await
    }

    #[tool(
        description = "Add a rule for a client (or `_studio` for every client): must, must_not, prefer or avoid, with an optional check (terms, pattern, required_terms) that makes it a hard rule. Usually better as a line in the client's Writing rules document (edit_document), unless it's a one-off. Takes effect at once."
    )]
    async fn create_rule(
        &self,
        Parameters(p): Parameters<ProposeRule>,
        Extension(parts): Extension<Parts>,
    ) -> ToolResult {
        let c = caller(&parts)?;
        let rule = NewRule {
            kind: p.kind,
            text: p.text,
            applies_to: p.applies_to,
            check: p.check,
        };
        let why = p.rationale.unwrap_or_else(|| "Added by Claude.".into());
        self.propose(&c, &p.client, "rule", json!(rule), &why, p.sources)
            .await
    }

    #[tool(
        description = "Change a rule added directly (a Writing rules line is changed with edit_document). Pass only what changes. Takes effect at once."
    )]
    async fn update_rule(
        &self,
        Parameters(p): Parameters<UpdateRule>,
        Extension(parts): Extension<Parts>,
    ) -> ToolResult {
        let c = caller(&parts)?;
        let mut change = json!({ "rule": p.rule });
        if let Some(k) = p.kind {
            change["kind"] = json!(k);
        }
        if let Some(t) = p.text {
            change["text"] = json!(t);
        }
        if let Some(a) = p.applies_to {
            change["applies_to"] = json!(a);
        }
        if let Some(ch) = p.check {
            change["check"] = json!(ch);
        }
        let why = p.rationale.unwrap_or_else(|| "Changed by Claude.".into());
        self.propose(&c, &p.client, "rule_edit", change, &why, vec![])
            .await
    }

    #[tool(
        description = "Retire a rule added directly (a Writing rules line is removed with edit_document). Waits for the user's approval."
    )]
    async fn retire_rule(
        &self,
        Parameters(p): Parameters<Removal>,
        Extension(parts): Extension<Parts>,
    ) -> ToolResult {
        let c = caller(&parts)?;
        self.propose(
            &c,
            &p.client,
            "rule_retire",
            json!({ "rule": p.id }),
            &p.reason,
            vec![],
        )
        .await
    }

    #[tool(
        description = "Record a decision: whenever the work departs from (or settles) something in the docs. Say what's decided, why, and which document sections it overrides (doc and heading anchor from get_document); those sections then show as overridden in search and check. Takes effect at once."
    )]
    async fn record_decision(
        &self,
        Parameters(p): Parameters<ProposeDecision>,
        Extension(parts): Extension<Parts>,
    ) -> ToolResult {
        let c = caller(&parts)?;
        let rationale = p.reason.clone();
        let d = NewDecision {
            statement: p.statement,
            reason: p.reason,
            contradicts: p.contradicts,
            applies_to: p.applies_to,
            evidence: p.evidence,
            supersedes: p.supersedes,
        };
        self.propose(&c, &p.client, "decision", json!(d), &rationale, vec![])
            .await
    }

    #[tool(
        description = "Change a decision (its statement, reason, the sections it overrides, where it applies, evidence). Pass only what changes. Takes effect at once."
    )]
    async fn update_decision(
        &self,
        Parameters(p): Parameters<UpdateDecision>,
        Extension(parts): Extension<Parts>,
    ) -> ToolResult {
        let c = caller(&parts)?;
        let mut change = json!({ "decision": p.decision });
        if let Some(v) = p.statement {
            change["statement"] = json!(v);
        }
        if let Some(v) = p.reason {
            change["reason"] = json!(v);
        }
        if let Some(v) = p.contradicts {
            change["contradicts"] = json!(v);
        }
        if let Some(v) = p.applies_to {
            change["applies_to"] = json!(v);
        }
        if let Some(v) = p.evidence {
            change["evidence"] = json!(v);
        }
        let why = p.rationale.unwrap_or_else(|| "Changed by Claude.".into());
        self.propose(&c, &p.client, "decision_edit", change, &why, vec![])
            .await
    }

    #[tool(
        description = "Retire a decision (it stops overriding the docs; one it superseded comes back). Waits for the user's approval."
    )]
    async fn retire_decision(
        &self,
        Parameters(p): Parameters<Removal>,
        Extension(parts): Extension<Parts>,
    ) -> ToolResult {
        let c = caller(&parts)?;
        self.propose(
            &c,
            &p.client,
            "decision_retire",
            json!({ "decision": p.id }),
            &p.reason,
            vec![],
        )
        .await
    }

    #[tool(
        description = "Unlink work from a client (it stays in its app). Waits for the user's approval."
    )]
    async fn unlink_artefact(
        &self,
        Parameters(p): Parameters<Removal>,
        Extension(parts): Extension<Parts>,
    ) -> ToolResult {
        let c = caller(&parts)?;
        self.propose(
            &c,
            &p.client,
            "artefact_unlink",
            json!({ "artefact": p.id }),
            &p.reason,
            vec![],
        )
        .await
    }

    #[tool(
        description = "Add a client: Hub creates it in Clients (where its brand will live) and makes its space with starter documents. Takes effect at once."
    )]
    async fn create_client(
        &self,
        Parameters(p): Parameters<CreateClient>,
        Extension(parts): Extension<Parts>,
    ) -> ToolResult {
        let c = caller(&parts)?;
        let why = format!("Added {} as a client.", p.name.trim());
        self.propose(
            &c,
            spaces::STUDIO,
            "client_create",
            json!({ "slug": p.slug, "name": p.name }),
            &why,
            vec![],
        )
        .await
    }

    #[tool(
        description = "Rename a client (in Clients and Hub; the slug stays). Takes effect at once."
    )]
    async fn update_client(
        &self,
        Parameters(p): Parameters<UpdateClient>,
        Extension(parts): Extension<Parts>,
    ) -> ToolResult {
        let c = caller(&parts)?;
        let why = format!("Renamed to {}.", p.name.trim());
        self.propose(
            &c,
            &p.client,
            "client_update",
            json!({ "name": p.name }),
            &why,
            vec![],
        )
        .await
    }

    #[tool(
        description = "Delete a client: from Clients (its brand and library go too) and Hub (its space is archived, documents kept). Waits for the user's approval."
    )]
    async fn delete_client(
        &self,
        Parameters(p): Parameters<DeleteClient>,
        Extension(parts): Extension<Parts>,
    ) -> ToolResult {
        let c = caller(&parts)?;
        self.propose(&c, &p.client, "client_delete", json!({}), &p.reason, vec![])
            .await
    }

    #[tool(
        description = "Record that something made in another app (a board, a Design file, a Motion scene, a Typeset PDF) belongs to this client. Takes effect at once."
    )]
    async fn link_artefact(
        &self,
        Parameters(p): Parameters<LinkArtefact>,
        Extension(parts): Extension<Parts>,
    ) -> ToolResult {
        let c = caller(&parts)?;
        let change = json!({ "app": p.app, "kind": p.kind, "title": p.title, "url": p.url,
                             "external_id": p.external_id, "note": p.note });
        let rationale = format!("Made for this client in {}.", p.app);
        self.propose(&c, &p.client, "artefact_link", change, &rationale, vec![])
            .await
    }

    #[tool(
        description = "The library: reusable files the apps work from, by kind (design-style: Design's style references, DESIGN.md files). Lists names, titles and summaries; with client, what that client uses; with query, finds items by meaning (\"a calm editorial style with big type\")."
    )]
    async fn library_list(
        &self,
        Parameters(p): Parameters<LibraryList>,
        Extension(parts): Extension<Parts>,
    ) -> ToolResult {
        let c = caller(&parts)?;
        let mut conn = self.conn().await?;
        let kind = p.kind.filter(|k| !k.is_empty());
        if let Some(q) = p.query.filter(|q| !q.trim().is_empty()) {
            let spaces: Vec<String> = routes::spaces_for(&mut conn, &c)
                .await
                .map_err(fail)?
                .into_iter()
                .map(|s| s.0)
                .collect();
            let query = search::Query {
                text: q,
                spaces,
                doc_type: kind,
                source: Some("library".into()),
                target: None,
                standing: None,
                limit: 10,
            };
            return ok(search::run(&mut conn, self.state.embedder.as_ref(), &query)
                .await
                .map_err(fail)?);
        }
        let items = match p.client.as_deref().filter(|s| !s.is_empty()) {
            Some(slug) => {
                let s = routes::space(&mut conn, &c, slug).await.map_err(fail)?;
                library::for_client(&mut conn, &s.id).await.map_err(fail)?
            }
            None => {
                let spaces: Vec<String> = routes::spaces_for(&mut conn, &c)
                    .await
                    .map_err(fail)?
                    .into_iter()
                    .map(|s| s.0)
                    .collect();
                library::list(&mut conn, &spaces, kind.as_deref())
                    .await
                    .map_err(fail)?
            }
        };
        ok(
            json!({ "items": items.iter().filter(|i| kind.as_ref().is_none_or(|k| &i.kind == k)).map(library::Item::summary_json).collect::<Vec<_>>() }),
        )
    }

    #[tool(
        description = "One library item in full (its body), with its version. With client, that client's own item of the name if it has one, else the studio's."
    )]
    async fn library_get(
        &self,
        Parameters(p): Parameters<LibraryGet>,
        Extension(parts): Extension<Parts>,
    ) -> ToolResult {
        let c = caller(&parts)?;
        let mut conn = self.conn().await?;
        if let Some(slug) = p.client.as_deref().filter(|s| !s.is_empty()) {
            let s = routes::space(&mut conn, &c, slug).await.map_err(fail)?;
            if let Some(i) = library::find(&mut conn, &p.kind, &s.id, &p.name)
                .await
                .map_err(fail)?
            {
                return ok(json!(i));
            }
        }
        match library::find(&mut conn, &p.kind, spaces::STUDIO, &p.name)
            .await
            .map_err(fail)?
        {
            Some(i) => ok(json!(i)),
            None => Err(fail(AppError::NotFound("library item"))),
        }
    }

    #[tool(
        description = "Save a library item (a new one, or a new version: pass the version you read). Takes effect at once."
    )]
    async fn library_put(
        &self,
        Parameters(p): Parameters<LibraryPutParams>,
        Extension(parts): Extension<Parts>,
    ) -> ToolResult {
        let c = caller(&parts)?;
        let scope = p.scope.unwrap_or_else(|| spaces::STUDIO.to_string());
        let change = json!({ "kind": p.kind, "name": p.name, "body": p.body, "tags": p.tags, "source": p.source, "version": p.version });
        self.propose(&c, &scope, "library_put", change, &p.rationale, vec![])
            .await
    }

    #[tool(
        description = "Pin a studio library item for a client (it then shows in client_context), or unpin it with pin: false. Takes effect at once."
    )]
    async fn library_pin(
        &self,
        Parameters(p): Parameters<LibraryPinParams>,
        Extension(parts): Extension<Parts>,
    ) -> ToolResult {
        let c = caller(&parts)?;
        let item = {
            let mut conn = self.conn().await?;
            library::find(&mut conn, &p.kind, spaces::STUDIO, &p.name)
                .await
                .map_err(fail)?
                .ok_or_else(|| fail(AppError::NotFound("studio library item")))?
        };
        let pin = p.pin.unwrap_or(true);
        let rationale = format!("{} uses {}.", p.client, item.title);
        self.propose(
            &c,
            &p.client,
            "library_pin",
            json!({ "item": item.id, "pin": pin }),
            &rationale,
            vec![],
        )
        .await
    }

    #[tool(description = "Delete a library item. Waits for the user's approval.")]
    async fn library_delete(
        &self,
        Parameters(p): Parameters<LibraryDelete>,
        Extension(parts): Extension<Parts>,
    ) -> ToolResult {
        let c = caller(&parts)?;
        let scope = p.scope.unwrap_or_else(|| spaces::STUDIO.to_string());
        self.propose(
            &c,
            &scope,
            "library_delete",
            json!({ "kind": p.kind, "name": p.name }),
            &p.reason,
            vec![],
        )
        .await
    }

    #[tool(
        description = "Changes in Hub (pending ones by default: those waiting for the user), for a client or every client, with diffs and status."
    )]
    async fn list_proposals(
        &self,
        Parameters(p): Parameters<ListProposals>,
        Extension(parts): Extension<Parts>,
    ) -> ToolResult {
        let c = caller(&parts)?;
        let mut conn = self.conn().await?;
        let all = routes::spaces_for(&mut conn, &c).await.map_err(fail)?;
        let ids: Vec<String> = match p.client.as_deref().filter(|s| !s.is_empty()) {
            Some(slug) => vec![routes::space(&mut conn, &c, slug).await.map_err(fail)?.id],
            None => all.iter().map(|s| s.0.clone()).collect(),
        };
        let status = p.status.unwrap_or_else(|| "pending".into());
        let status = (status != "all").then_some(status);
        let list = proposals::list(&mut conn, &ids, status.as_deref(), 50)
            .await
            .map_err(fail)?;
        ok(
            json!({ "proposals": list.iter().map(|x| routes::with_space(x, &all)).collect::<Vec<_>>() }),
        )
    }

    #[tool(
        description = "Approve a waiting proposal (a removal, say), ONLY when the user has told you to in this conversation. Quote their words in user_said. Changes to a client-visible document can only be approved by the user in Hub."
    )]
    async fn approve_proposal(
        &self,
        Parameters(p): Parameters<Approve>,
        Extension(parts): Extension<Parts>,
    ) -> ToolResult {
        let c = caller(&parts)?;
        if p.user_said.trim().len() < 2 {
            return Err(fail(AppError::invalid(
                "user_said",
                "quote the user's words approving it",
            )));
        }
        let mut tx = self.state.pool.begin().await.map_err(|e| fail(e.into()))?;
        let x = proposals::get(&mut tx, &p.proposal_id)
            .await
            .map_err(fail)?;
        routes::space_by_id(&mut tx, &c, &x.space_id, "proposal")
            .await
            .map_err(fail)?;
        if x.change.0["manual"] == true
            && let Some(why) = proposals::stays_manual(&x)
        {
            return Err(fail(AppError::State {
                code: "approve_in_hub",
                message: format!("this one has to be approved in Hub itself: {why}"),
            }));
        }
        let by = format!("user via {}", c.actor);
        let done = proposals::approve(
            &mut tx,
            self.state.clients.as_ref(),
            &p.proposal_id,
            &by,
            Some(p.user_said.trim()),
        )
        .await
        .map_err(fail)?;
        tx.commit().await.map_err(|e| fail(e.into()))?;
        self.state.index_wake.notify_one();
        ok(done.json())
    }

    #[tool(description = "Reject a proposal, when the user has said to.")]
    async fn reject_proposal(
        &self,
        Parameters(p): Parameters<Reject>,
        Extension(parts): Extension<Parts>,
    ) -> ToolResult {
        let c = caller(&parts)?;
        let mut tx = self.state.pool.begin().await.map_err(|e| fail(e.into()))?;
        let x = proposals::get(&mut tx, &p.proposal_id)
            .await
            .map_err(fail)?;
        routes::space_by_id(&mut tx, &c, &x.space_id, "proposal")
            .await
            .map_err(fail)?;
        let by = format!("user via {}", c.actor);
        let done = proposals::reject(&mut tx, &p.proposal_id, &by, p.note.as_deref())
            .await
            .map_err(fail)?;
        tx.commit().await.map_err(|e| fail(e.into()))?;
        ok(done.json())
    }

    #[tool(
        description = "Withdraw one of your own pending proposals (e.g. to propose a better one)."
    )]
    async fn withdraw_proposal(
        &self,
        Parameters(p): Parameters<ProposalId>,
        Extension(parts): Extension<Parts>,
    ) -> ToolResult {
        let c = caller(&parts)?;
        let mut tx = self.state.pool.begin().await.map_err(|e| fail(e.into()))?;
        let x = proposals::get(&mut tx, &p.proposal_id)
            .await
            .map_err(fail)?;
        routes::space_by_id(&mut tx, &c, &x.space_id, "proposal")
            .await
            .map_err(fail)?;
        let done = proposals::withdraw(&mut tx, &p.proposal_id, &c.actor)
            .await
            .map_err(fail)?;
        tx.commit().await.map_err(|e| fail(e.into()))?;
        ok(done.json())
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for HubMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("hub", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }
}

/// `/mcp`: the MCP service, behind the same auth as the API.
pub fn service(state: AppState) -> Router {
    let mcp_state = state.clone();
    let mut hosts = vec!["localhost".to_string(), "127.0.0.1".to_string()];
    // Behind nginx, requests arrive for the public host; allow it (with and
    // without its port), or rmcp's DNS-rebinding guard refuses them.
    if let Some(authority) = state.config.public_origin.split("://").nth(1) {
        hosts.push(authority.to_string());
        if let Some((host, _port)) = authority.split_once(':') {
            hosts.push(host.to_string());
        }
    }
    let config = StreamableHttpServerConfig::default().with_allowed_hosts(hosts);
    let mcp = StreamableHttpService::new(
        move || Ok(HubMcp::new(mcp_state.clone())),
        Arc::new(LocalSessionManager::default()),
        config,
    );
    Router::new()
        .fallback_service(mcp)
        .layer(middleware::from_fn_with_state(state, auth::require_caller))
}
