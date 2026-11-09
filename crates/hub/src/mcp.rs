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
use crate::{decisions, documents, routes, search, spaces};
use hub_model::Check;

const INSTRUCTIONS: &str = "Hub holds what you know about each client: their brief, audience, \
voice, constraints and writing rules (as documents), the rules that must hold, the decisions \
that depart from the docs, and links to the work made for them in Board, Design, Motion and \
Typeset. Start every job for a client with client_context. Use search to find what the docs \
say (each result says how far to rely on it: authoritative, tentative or outdated; an outdated \
section comes with the decision that overrides it). Run check on anything before it leaves \
(a PDF, a design handoff, copy): hard-rule violations must be fixed, guidance weighed. When \
the work departs from the docs, propose_decision with the reason. You can't change anything \
directly: propose_document_edit, propose_new_document, propose_rule, propose_decision and \
link_artefact make proposals that wait for the user (tell them, with the proposal id). Only \
call approve_proposal when the user has told you, in this conversation, to approve that \
proposal; quote their words in user_said.";

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
    /// Why: what changed, and where it came from.
    pub rationale: String,
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
    pub rationale: String,
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
    pub rationale: String,
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
        let p = proposals::create(&mut tx, &s.id, kind, change, rationale, sources, &c.actor)
            .await
            .map_err(fail)?;
        tx.commit().await.map_err(|e| fail(e.into()))?;
        self.state.index_wake.notify_one();
        let mut v = p.json();
        v["what_happened"] = json!(match p.status.as_str() {
            "applied" =>
                "Applied at once (the hands-off switch allows this kind for this client); it can be reverted in Hub.",
            _ =>
                "Waiting for the user's approval in Hub's Approvals page. Tell them the proposal id.",
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
        description = "Propose a change to a document. Read it with get_document first and pass its version; prefer edits (exact find/replace, each find unique) for small changes, or content for a rewrite. It waits for the user's approval (tell them the proposal id) unless the hands-off switch allows it; edits to client-visible documents always wait."
    )]
    async fn propose_document_edit(
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
        self.propose(&c, &p.client, "doc_edit", change, &p.rationale, p.sources)
            .await
    }

    #[tool(
        description = "Propose a new document for a client (e.g. research notes, a sitemap rationale). Waits for approval like any change."
    )]
    async fn propose_new_document(
        &self,
        Parameters(p): Parameters<ProposeNew>,
        Extension(parts): Extension<Parts>,
    ) -> ToolResult {
        let c = caller(&parts)?;
        self.propose(
            &c,
            &p.client,
            "doc_create",
            json!({ "path": p.path, "content": p.content }),
            &p.rationale,
            p.sources,
        )
        .await
    }

    #[tool(
        description = "Propose a rule for a client (or `_studio` for every client): must, must_not, prefer or avoid, with an optional check (terms, pattern, required_terms) that makes it a hard rule. Usually better as a line in the client's Writing rules document (propose_document_edit), unless it's a one-off."
    )]
    async fn propose_rule(
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
        self.propose(&c, &p.client, "rule", json!(rule), &p.rationale, p.sources)
            .await
    }

    #[tool(
        description = "Propose a decision: whenever the work departs from (or settles) something in the docs. Say what's decided, why, and which document sections it overrides (doc and heading anchor from get_document). Once approved, those sections show as overridden in search and check."
    )]
    async fn propose_decision(
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
        description = "Record that something made in another app (a board, a Design file, a Motion scene, a Typeset PDF) belongs to this client. A proposal, like any change."
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
        description = "Proposals (pending by default) for a client or every client, with their diffs and status."
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
        description = "Approve a proposal, ONLY when the user has told you to in this conversation. Quote their words in user_said. Proposals that change a client-visible document can only be approved by the user in Hub."
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
        if let Some(why) = proposals::stays_manual(&x) {
            return Err(fail(AppError::State {
                code: "approve_in_hub",
                message: format!("this one has to be approved in Hub itself: {why}"),
            }));
        }
        let by = format!("user via {}", c.actor);
        let done = proposals::approve(&mut tx, &p.proposal_id, &by, Some(p.user_said.trim()))
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
