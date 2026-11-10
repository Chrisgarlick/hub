//! The REST API: spaces and their documents (spec section 3), rules
//! (section 4), decisions (section 5) and `check` (section 8.1).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post, put};
use axum::{Extension, Json, Router, middleware};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgConnection;

use crate::app::AppState;
use crate::auth::{self, Caller};
use crate::decisions::{self, DecisionPatch, NewDecision};
use crate::documents::{self, Patch};
use crate::error::{AppError, AppResult};
use crate::proposals::{self, ArtefactLink, SettingsPatch};
use crate::rules::{self, NewRule};
use crate::{index, library, passkeys, runs, search, spaces};

pub fn api(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/me", get(me))
        .route("/spaces", get(list_spaces))
        .route("/spaces/{slug}", get(get_space))
        .route("/spaces/{slug}/documents", post(create_document))
        .route("/spaces/{slug}/trash", get(trash))
        .route(
            "/documents/{id}",
            get(get_document)
                .patch(update_document)
                .delete(delete_document),
        )
        .route("/documents/{id}/undelete", post(undelete_document))
        .route("/documents/{id}/revisions", get(revisions))
        .route("/documents/{id}/restore/{rev}", post(restore))
        .route("/spaces/{slug}/rules", get(list_rules).post(create_rule))
        .route(
            "/spaces/{slug}/rules/{id}/exception",
            put(except_rule).delete(unexcept_rule),
        )
        .route(
            "/rules/{id}",
            get(get_rule).patch(update_rule).delete(retire_rule),
        )
        .route("/rules/{id}/approve", post(approve_rule))
        .route("/spaces/{slug}/check", post(check))
        .route(
            "/spaces/{slug}/decisions",
            get(list_decisions).post(create_decision),
        )
        .route("/spaces/{slug}/decisions.md", get(decisions_markdown))
        .route("/decisions/{id}", get(get_decision).patch(update_decision))
        .route("/decisions/{id}/approve", post(approve_decision))
        .route("/decisions/{id}/reject", post(reject_decision))
        .route("/proposals", get(list_proposals))
        .route("/spaces/{slug}/proposals", post(create_proposal))
        .route("/proposals/{id}", get(get_proposal))
        .route("/proposals/{id}/approve", post(approve_proposal))
        .route("/proposals/{id}/reject", post(reject_proposal))
        .route("/proposals/{id}/withdraw", post(withdraw_proposal))
        .route("/proposals/{id}/revert", post(revert_proposal))
        .route(
            "/settings/approvals",
            get(get_approvals).patch(set_approvals),
        )
        .route(
            "/spaces/{slug}/artefacts",
            get(list_artefacts).post(add_artefact),
        )
        .route("/artefacts/{id}", delete(remove_artefact))
        .route("/library", get(list_library))
        .route(
            "/library/{kind}/{scope}/{name}",
            get(get_library).put(put_library).delete(delete_library),
        )
        .route(
            "/library/{kind}/{scope}/{name}/revisions",
            get(library_revisions),
        )
        .route("/spaces/{slug}/library", get(client_library))
        .route(
            "/spaces/{slug}/pins/{item}",
            put(pin_item).delete(unpin_item),
        )
        .route("/runs", get(list_runs))
        .route("/runs/{id}", get(get_run))
        .route("/runs/{id}/cancel", post(cancel_run))
        .route("/runs/{id}/steps/{ord}/approve", post(approve_step))
        .route("/runs/{id}/steps/{ord}/send-back", post(send_back_step))
        .route("/search", get(search))
        .route("/index", get(index_status))
        .route("/sync", post(sync))
        .route("/passkeys", get(passkeys::list).post(passkeys::register))
        .route("/passkeys/options", post(passkeys::creation_options))
        .route("/passkeys/{id}", delete(passkeys::remove))
        .fallback(|| async { AppError::NotFound("route") })
        .layer(middleware::from_fn_with_state(state, auth::require_caller))
}

async fn me(Extension(c): Extension<Caller>) -> Json<Value> {
    Json(json!({ "actor": c.actor, "clients": c.clients, "session": c.session }))
}

async fn list_spaces(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
) -> AppResult<Json<Value>> {
    let mut conn = st.pool.acquire().await?;
    let list: Vec<_> = spaces::list(&mut conn)
        .await?
        .into_iter()
        .filter(|s| c.may(&s.space.slug))
        .collect();
    Ok(Json(json!({ "spaces": list })))
}

/// A live space the caller may use.
pub(crate) async fn space(
    conn: &mut PgConnection,
    c: &Caller,
    slug: &str,
) -> AppResult<spaces::Space> {
    let s = spaces::by_slug(conn, slug).await?;
    if !c.may(&s.slug) {
        return Err(AppError::NotFound("space"));
    }
    Ok(s)
}

/// A document the caller may use (in a live space).
async fn document(
    conn: &mut PgConnection,
    c: &Caller,
    id: &str,
    trashed_ok: bool,
) -> AppResult<(spaces::Space, String)> {
    let row: Option<(String, Option<chrono::DateTime<chrono::Utc>>)> =
        sqlx::query_as("SELECT space_id, deleted_at FROM documents WHERE id = $1")
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?;
    let (space_id, deleted) = row.ok_or(AppError::NotFound("document"))?;
    if deleted.is_some() && !trashed_ok {
        return Err(AppError::NotFound("document"));
    }
    let s: Option<spaces::Space> = sqlx::query_as(
        "SELECT id, slug, name, kind, archived_at, created_at, updated_at FROM spaces WHERE id = $1 AND archived_at IS NULL",
    )
    .bind(&space_id)
    .fetch_optional(&mut *conn)
    .await?;
    let s = s.ok_or(AppError::NotFound("document"))?;
    if !c.may(&s.slug) {
        return Err(AppError::NotFound("document"));
    }
    Ok((s, space_id))
}

async fn get_space(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(slug): Path<String>,
) -> AppResult<Json<Value>> {
    let mut conn = st.pool.acquire().await?;
    let s = space(&mut conn, &c, &slug).await?;
    let docs = documents::list(&mut conn, &s.id, false).await?;
    Ok(Json(json!({ "space": s, "documents": docs })))
}

async fn trash(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(slug): Path<String>,
) -> AppResult<Json<Value>> {
    let mut conn = st.pool.acquire().await?;
    let s = space(&mut conn, &c, &slug).await?;
    Ok(Json(
        json!({ "documents": documents::list(&mut conn, &s.id, true).await? }),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewDocument {
    path: String,
    #[serde(default)]
    content: Option<String>,
}

async fn create_document(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(slug): Path<String>,
    Json(b): Json<NewDocument>,
) -> AppResult<(StatusCode, Json<Value>)> {
    propose_instead(&c)?;
    let mut tx = st.pool.begin().await?;
    let s = space(&mut tx, &c, &slug).await?;
    let path = documents::check_path(&b.path)?;
    let content = b.content.unwrap_or_else(|| {
        format!(
            "# {}\n\n",
            documents::meta(&path, "")
                .map(|m| m.title)
                .unwrap_or_default()
        )
    });
    let d = documents::create(&mut tx, &s.id, &path, &content, &c.actor).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(d.json())))
}

async fn get_document(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let mut conn = st.pool.acquire().await?;
    document(&mut conn, &c, &id, false).await?;
    let mut d = documents::get(&mut conn, &id).await?.json();
    d["overridden_by"] = overrides(&mut conn, &id).await?;
    Ok(Json(d))
}

/// The approved decisions overriding sections of a document.
pub(crate) async fn overrides(conn: &mut PgConnection, doc_id: &str) -> AppResult<Value> {
    let list = decisions::overriding(conn, doc_id).await?;
    let mut out = Vec::new();
    for d in &list {
        for s in d.contradicts.0.iter().filter(|s| s.doc == doc_id) {
            out.push(json!({
                "anchor": s.anchor,
                "decision": { "id": d.id, "statement": d.statement, "reason": d.reason, "decided_at": d.decided_at },
            }));
        }
    }
    Ok(json!(out))
}

async fn update_document(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(id): Path<String>,
    Json(p): Json<Patch>,
) -> AppResult<Json<Value>> {
    propose_instead(&c)?;
    let mut tx = st.pool.begin().await?;
    document(&mut tx, &c, &id, false).await?;
    let d = documents::update(&mut tx, &id, p, &c.actor).await?;
    tx.commit().await?;
    Ok(Json(d.json()))
}

async fn delete_document(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    propose_instead(&c)?;
    let mut tx = st.pool.begin().await?;
    document(&mut tx, &c, &id, false).await?;
    let d = documents::delete(&mut tx, &id, &c.actor).await?;
    tx.commit().await?;
    Ok(Json(d.json()))
}

async fn undelete_document(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    propose_instead(&c)?;
    let mut tx = st.pool.begin().await?;
    document(&mut tx, &c, &id, true).await?;
    let d = documents::undelete(&mut tx, &id).await?;
    tx.commit().await?;
    Ok(Json(d.json()))
}

async fn revisions(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let mut conn = st.pool.acquire().await?;
    document(&mut conn, &c, &id, true).await?;
    Ok(Json(
        json!({ "revisions": documents::revisions(&mut conn, &id).await? }),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RestoreBody {
    version: i32,
}

async fn restore(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path((id, rev)): Path<(String, i64)>,
    Json(b): Json<RestoreBody>,
) -> AppResult<Json<Value>> {
    propose_instead(&c)?;
    let mut tx = st.pool.begin().await?;
    document(&mut tx, &c, &id, false).await?;
    let d = documents::restore(&mut tx, &id, rev, b.version, &c.actor).await?;
    tx.commit().await?;
    Ok(Json(d.json()))
}

/// `POST /api/sync`: catch up with Clients now (spaces, then events).
async fn sync(State(st): State<AppState>) -> AppResult<Json<Value>> {
    let clients = st.clients.as_ref().ok_or(AppError::Unavailable(
        "Hub isn't connected to Clients".into(),
    ))?;
    let list = clients.clients().await?;
    let created = spaces::reconcile(&st.pool, &list).await?;
    let events = spaces::sync_events(&st.pool, clients).await?;
    Ok(Json(
        json!({ "clients": list.len(), "created": created, "events": events }),
    ))
}

// ---------- Rules ----------

/// The live space with this id, if the caller may use it.
pub(crate) async fn space_by_id(
    conn: &mut PgConnection,
    c: &Caller,
    id: &str,
    what: &'static str,
) -> AppResult<spaces::Space> {
    let s: Option<spaces::Space> = sqlx::query_as(
        "SELECT id, slug, name, kind, archived_at, created_at, updated_at FROM spaces WHERE id = $1 AND archived_at IS NULL",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    match s {
        Some(s) if c.may(&s.slug) => Ok(s),
        _ => Err(AppError::NotFound(what)),
    }
}

/// Changes from a Clients key are proposals (`POST /api/spaces/{slug}/proposals`,
/// or the MCP's propose_ tools); only you change things directly.
fn propose_instead(c: &Caller) -> AppResult<()> {
    if !c.session {
        return Err(AppError::Forbidden(
            "a key can't change this directly: propose it (POST /api/spaces/{slug}/proposals, or the MCP's propose_ tools)",
        ));
    }
    Ok(())
}

/// Approving, and switching studio rules off for a client, are yours alone.
fn only_you(c: &Caller) -> AppResult<()> {
    if !c.session {
        return Err(AppError::Forbidden(
            "only you can approve, from a signed-in browser",
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
struct ForQuery {
    #[serde(default, rename = "for")]
    target: Option<String>,
}

pub(crate) fn target(t: Option<String>) -> AppResult<Option<String>> {
    match t.filter(|t| !t.is_empty() && t != "all") {
        Some(t) if hub_model::TARGETS.contains(&t.as_str()) => Ok(Some(t)),
        Some(t) => Err(AppError::invalid(
            "for",
            format!("`{t}` isn't one of {}", hub_model::TARGETS.join(", ")),
        )),
        None => Ok(None),
    }
}

async fn list_rules(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(slug): Path<String>,
    Query(q): Query<ForQuery>,
) -> AppResult<Json<Value>> {
    let t = target(q.target)?;
    let mut conn = st.pool.acquire().await?;
    let s = space(&mut conn, &c, &slug).await?;
    let list: Vec<Value> = rules::for_space(&mut conn, &s.id)
        .await?
        .iter()
        .filter(|r| r.rule.applies(t.as_deref()))
        .map(rules::InForce::json)
        .collect();
    Ok(Json(json!({ "rules": list })))
}

async fn create_rule(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(slug): Path<String>,
    Json(b): Json<NewRule>,
) -> AppResult<(StatusCode, Json<Value>)> {
    propose_instead(&c)?;
    let mut tx = st.pool.begin().await?;
    let s = space(&mut tx, &c, &slug).await?;
    let r = rules::create(&mut tx, &s.id, b, &c.actor).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(r.json(None, None))))
}

async fn rule_for(conn: &mut PgConnection, c: &Caller, id: &str) -> AppResult<rules::Rule> {
    let r = rules::get(conn, id).await?;
    space_by_id(conn, c, &r.space_id, "rule").await?;
    Ok(r)
}

async fn get_rule(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let mut conn = st.pool.acquire().await?;
    Ok(Json(rule_for(&mut conn, &c, &id).await?.json(None, None)))
}

async fn update_rule(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(id): Path<String>,
    Json(b): Json<NewRule>,
) -> AppResult<Json<Value>> {
    propose_instead(&c)?;
    let mut tx = st.pool.begin().await?;
    rule_for(&mut tx, &c, &id).await?;
    let r = rules::update(&mut tx, &id, b, &c.actor).await?;
    tx.commit().await?;
    Ok(Json(r.json(None, None)))
}

async fn retire_rule(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    propose_instead(&c)?;
    let mut tx = st.pool.begin().await?;
    rule_for(&mut tx, &c, &id).await?;
    let r = rules::retire(&mut tx, &id, &c.actor).await?;
    tx.commit().await?;
    Ok(Json(r.json(None, None)))
}

async fn approve_rule(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    only_you(&c)?;
    let mut tx = st.pool.begin().await?;
    rule_for(&mut tx, &c, &id).await?;
    let r = rules::approve(&mut tx, &id, &c.actor).await?;
    tx.commit().await?;
    Ok(Json(r.json(None, None)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExceptionBody {
    reason: String,
}

async fn except_rule(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path((slug, id)): Path<(String, String)>,
    Json(b): Json<ExceptionBody>,
) -> AppResult<StatusCode> {
    only_you(&c)?;
    let mut conn = st.pool.acquire().await?;
    let s = space(&mut conn, &c, &slug).await?;
    rules::set_exception(&mut conn, &s.id, &id, Some(&b.reason), &c.actor).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn unexcept_rule(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path((slug, id)): Path<(String, String)>,
) -> AppResult<StatusCode> {
    only_you(&c)?;
    let mut conn = st.pool.acquire().await?;
    let s = space(&mut conn, &c, &slug).await?;
    rules::set_exception(&mut conn, &s.id, &id, None, &c.actor).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------- Check ----------

const MAX_CHECK_BYTES: usize = 1 << 20;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckBody {
    text: String,
    #[serde(default, rename = "for")]
    target: Option<String>,
}

/// `check` (spec section 8.1): hard-rule violations in a draft, with where;
/// advice from `prefer` and `avoid` checks; and the guidance and decisions
/// for Claude to weigh. Approved rules only.
async fn check(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(slug): Path<String>,
    Json(b): Json<CheckBody>,
) -> AppResult<Json<Value>> {
    let t = target(b.target)?;
    let mut conn = st.pool.acquire().await?;
    let s = space(&mut conn, &c, &slug).await?;
    Ok(Json(
        run_check(&mut conn, &s.id, &b.text, t.as_deref()).await?,
    ))
}

/// The check itself, for the API and the MCP.
pub(crate) async fn run_check(
    conn: &mut PgConnection,
    space_id: &str,
    text: &str,
    t: Option<&str>,
) -> AppResult<Value> {
    if text.len() > MAX_CHECK_BYTES {
        return Err(AppError::invalid("text", "check at most 1 MB at a time"));
    }
    let in_force: Vec<rules::InForce> = rules::for_space(conn, space_id)
        .await?
        .into_iter()
        .filter(|r| r.rule.status == "approved" && r.excepted.is_none() && r.rule.applies(t))
        .collect();
    let brief = |r: &rules::InForce| {
        json!({
            "id": r.rule.id, "kind": r.rule.kind, "text": r.rule.text,
            "scope": if r.rule.space_id == spaces::STUDIO { "studio" } else { "client" },
            "source_path": r.source_path, "source_anchor": r.rule.source_anchor,
        })
    };
    let (mut violations, mut advice, mut guidance) = (vec![], vec![], vec![]);
    for r in &in_force {
        match r.rule.check() {
            None => guidance.push(brief(r)),
            Some(check) => {
                if let Some(found) = rules::run(check, text) {
                    let v =
                        json!({ "rule": brief(r), "spans": found.spans, "missing": found.missing });
                    if r.rule.kind().is_strict() {
                        violations.push(v)
                    } else {
                        advice.push(v)
                    }
                }
            }
        }
    }
    let decisions: Vec<Value> = decisions::list(conn, space_id)
        .await?
        .iter()
        .filter(|d| d.status == "approved" && d.applies(t))
        .map(|d| json!({ "id": d.id, "statement": d.statement, "reason": d.reason, "overrides": d.contradicts.0 }))
        .collect();
    Ok(json!({
        "ok": violations.is_empty(),
        "for": t.unwrap_or("all"),
        "violations": violations,
        "advice": advice,
        "guidance": guidance,
        "decisions": decisions,
    }))
}

// ---------- Decisions ----------

async fn list_decisions(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(slug): Path<String>,
    Query(q): Query<ForQuery>,
) -> AppResult<Json<Value>> {
    let t = target(q.target)?;
    let mut conn = st.pool.acquire().await?;
    let s = space(&mut conn, &c, &slug).await?;
    let list: Vec<Value> = decisions::list(&mut conn, &s.id)
        .await?
        .iter()
        .filter(|d| d.applies(t.as_deref()))
        .map(decisions::Decision::json)
        .collect();
    Ok(Json(json!({ "decisions": list })))
}

async fn decisions_markdown(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(slug): Path<String>,
) -> AppResult<Json<Value>> {
    let mut conn = st.pool.acquire().await?;
    let s = space(&mut conn, &c, &slug).await?;
    let list = decisions::list(&mut conn, &s.id).await?;
    Ok(Json(
        json!({ "markdown": decisions::markdown(&s.name, &list) }),
    ))
}

async fn create_decision(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(slug): Path<String>,
    Json(b): Json<NewDecision>,
) -> AppResult<(StatusCode, Json<Value>)> {
    propose_instead(&c)?;
    let mut tx = st.pool.begin().await?;
    let s = space(&mut tx, &c, &slug).await?;
    let d = decisions::create(&mut tx, &s.id, b, &c.actor).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(d.json())))
}

async fn decision_for(
    conn: &mut PgConnection,
    c: &Caller,
    id: &str,
) -> AppResult<decisions::Decision> {
    let d = decisions::get(conn, id).await?;
    space_by_id(conn, c, &d.space_id, "decision").await?;
    Ok(d)
}

async fn get_decision(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let mut conn = st.pool.acquire().await?;
    Ok(Json(decision_for(&mut conn, &c, &id).await?.json()))
}

async fn update_decision(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(id): Path<String>,
    Json(p): Json<DecisionPatch>,
) -> AppResult<Json<Value>> {
    propose_instead(&c)?;
    let mut tx = st.pool.begin().await?;
    decision_for(&mut tx, &c, &id).await?;
    let d = decisions::update(&mut tx, &id, p, &c.actor).await?;
    tx.commit().await?;
    Ok(Json(d.json()))
}

async fn approve_decision(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    only_you(&c)?;
    let mut tx = st.pool.begin().await?;
    decision_for(&mut tx, &c, &id).await?;
    let d = decisions::approve(&mut tx, &id, &c.actor).await?;
    tx.commit().await?;
    Ok(Json(d.json()))
}

async fn reject_decision(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    only_you(&c)?;
    let mut tx = st.pool.begin().await?;
    decision_for(&mut tx, &c, &id).await?;
    let d = decisions::reject(&mut tx, &id, &c.actor).await?;
    tx.commit().await?;
    Ok(Json(d.json()))
}

// ---------- Search ----------

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
    /// A space's slug; none means every space you may use.
    #[serde(default)]
    space: Option<String>,
    #[serde(default, rename = "type")]
    doc_type: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default, rename = "for")]
    target: Option<String>,
    /// Comma-separated: authoritative, tentative, outdated.
    #[serde(default)]
    standing: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

/// `GET /api/search?q=…&space=…`: spec section 6.3.
async fn search(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Query(p): Query<SearchQuery>,
) -> AppResult<Json<Value>> {
    if p.q.chars().count() > 500 {
        return Err(AppError::invalid("q", "search with at most 500 characters"));
    }
    let mut conn = st.pool.acquire().await?;
    let spaces: Vec<String> = match p.space.as_deref().filter(|s| !s.is_empty()) {
        Some(slug) => {
            let s = space(&mut conn, &c, slug).await?;
            if s.id == spaces::STUDIO {
                vec![s.id]
            } else {
                vec![s.id, spaces::STUDIO.to_string()]
            }
        }
        None => spaces::list(&mut conn)
            .await?
            .into_iter()
            .filter(|l| c.may(&l.space.slug))
            .map(|l| l.space.id)
            .collect(),
    };
    if let Some(src) = &p.source
        && !matches!(src.as_str(), "document" | "rule" | "decision" | "library")
    {
        return Err(AppError::invalid(
            "source",
            "use document, rule, decision or library",
        ));
    }
    let standing = match p.standing.as_deref().filter(|s| !s.is_empty()) {
        None => None,
        Some(list) => Some(
            list.split(',')
                .map(|s| match s.trim() {
                    "authoritative" => Ok(hub_model::Standing::Authoritative),
                    "tentative" => Ok(hub_model::Standing::Tentative),
                    "outdated" => Ok(hub_model::Standing::Outdated),
                    other => Err(AppError::invalid(
                        "standing",
                        format!("`{other}` isn't a standing"),
                    )),
                })
                .collect::<AppResult<Vec<_>>>()?,
        ),
    };
    let q = search::Query {
        text: p.q,
        spaces,
        doc_type: p.doc_type.filter(|t| !t.is_empty()),
        source: p.source,
        target: target(p.target)?,
        standing,
        limit: p.limit.unwrap_or(10),
    };
    Ok(Json(
        search::run(&mut conn, st.embedder.as_ref(), &q).await?,
    ))
}

/// `GET /api/index`: how far along the index is.
async fn index_status(State(st): State<AppState>) -> AppResult<Json<Value>> {
    Ok(Json(index::status(&st.pool, st.embedder.as_ref()).await?))
}

// ---------- Proposals ----------

/// The spaces a caller may use: id → (slug, name).
pub(crate) async fn spaces_for(
    conn: &mut PgConnection,
    c: &Caller,
) -> AppResult<Vec<(String, String, String)>> {
    Ok(spaces::list(conn)
        .await?
        .into_iter()
        .filter(|l| c.may(&l.space.slug))
        .map(|l| (l.space.id, l.space.slug, l.space.name))
        .collect())
}

pub(crate) fn with_space(p: &proposals::Proposal, spaces: &[(String, String, String)]) -> Value {
    let mut v = p.json();
    if let Some((_, slug, name)) = spaces.iter().find(|s| s.0 == p.space_id) {
        v["space"] = json!({ "id": p.space_id, "slug": slug, "name": name });
    }
    v["stays_manual"] = json!(proposals::stays_manual(p));
    v
}

#[derive(Deserialize)]
struct ProposalQuery {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    space: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
}

async fn list_proposals(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Query(q): Query<ProposalQuery>,
) -> AppResult<Json<Value>> {
    let mut conn = st.pool.acquire().await?;
    // Deleted clients' changes too: deleting one is itself one of them.
    let all: Vec<(String, String, String)> = sqlx::query_as::<_, (String, String, String, bool)>(
        "SELECT id, slug, name, archived_at IS NOT NULL FROM spaces",
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .filter(|(_, slug, _, _)| c.may(slug))
    .map(|(id, slug, name, gone)| {
        (
            id,
            slug,
            if gone {
                format!("{name} (deleted)")
            } else {
                name
            },
        )
    })
    .collect();
    let ids: Vec<String> = match q.space.as_deref().filter(|s| !s.is_empty()) {
        Some(slug) => vec![space(&mut conn, &c, slug).await?.id],
        None => all.iter().map(|s| s.0.clone()).collect(),
    };
    let status = q.status.filter(|s| !s.is_empty() && s != "all");
    let list = proposals::list(
        &mut conn,
        &ids,
        status.as_deref(),
        q.limit.unwrap_or(100).clamp(1, 500),
    )
    .await?;
    let pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM proposals WHERE status = 'pending' AND space_id = ANY($1)",
    )
    .bind(&ids)
    .fetch_one(&mut *conn)
    .await?;
    let checkpoints: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM run_steps s JOIN runs r ON r.id = s.run_id
         WHERE s.status = 'waiting' AND r.status = 'waiting' AND r.space_id = ANY($1)",
    )
    .bind(&ids)
    .fetch_one(&mut *conn)
    .await?;
    Ok(Json(json!({
        "checkpoints": checkpoints,
        "pending": pending,
        "proposals": list.iter().map(|p| with_space(p, &all)).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewProposal {
    kind: String,
    change: Value,
    rationale: String,
    #[serde(default)]
    sources: Vec<Value>,
}

async fn create_proposal(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(slug): Path<String>,
    Json(b): Json<NewProposal>,
) -> AppResult<(StatusCode, Json<Value>)> {
    let mut tx = st.pool.begin().await?;
    let s = space(&mut tx, &c, &slug).await?;
    let p = proposals::create(
        &mut tx,
        st.clients.as_ref(),
        &s.id,
        &b.kind,
        b.change,
        &b.rationale,
        b.sources,
        &c.actor,
    )
    .await?;
    tx.commit().await?;
    st.index_wake.notify_one();
    let mut conn = st.pool.acquire().await?;
    let all = spaces_for(&mut conn, &c).await?;
    Ok((StatusCode::CREATED, Json(with_space(&p, &all))))
}

async fn proposal_for(
    conn: &mut PgConnection,
    c: &Caller,
    id: &str,
) -> AppResult<proposals::Proposal> {
    let p = proposals::get(conn, id).await?;
    space_by_id(conn, c, &p.space_id, "proposal").await?;
    Ok(p)
}

async fn get_proposal(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let mut conn = st.pool.acquire().await?;
    let p = proposal_for(&mut conn, &c, &id).await?;
    let all = spaces_for(&mut conn, &c).await?;
    Ok(Json(with_space(&p, &all)))
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct DecideBody {
    #[serde(default)]
    note: Option<String>,
}

async fn decide(
    st: &AppState,
    c: &Caller,
    id: &str,
    body: Option<Json<DecideBody>>,
    what: &str,
) -> AppResult<Json<Value>> {
    let note = body.and_then(|b| b.0.note).filter(|n| !n.trim().is_empty());
    let mut tx = st.pool.begin().await?;
    proposal_for(&mut tx, c, id).await?;
    let p = match what {
        "approve" => {
            only_you(c)?;
            proposals::approve(&mut tx, st.clients.as_ref(), id, "user", note.as_deref()).await?
        }
        "reject" => {
            only_you(c)?;
            proposals::reject(&mut tx, id, "user", note.as_deref()).await?
        }
        "withdraw" => proposals::withdraw(&mut tx, id, &c.actor).await?,
        _ => {
            only_you(c)?;
            proposals::revert(&mut tx, st.clients.as_ref(), id, "user").await?
        }
    };
    tx.commit().await?;
    st.index_wake.notify_one();
    let mut conn = st.pool.acquire().await?;
    let all = spaces_for(&mut conn, c).await?;
    Ok(Json(with_space(&p, &all)))
}

async fn approve_proposal(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(id): Path<String>,
    body: Option<Json<DecideBody>>,
) -> AppResult<Json<Value>> {
    decide(&st, &c, &id, body, "approve").await
}

async fn reject_proposal(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(id): Path<String>,
    body: Option<Json<DecideBody>>,
) -> AppResult<Json<Value>> {
    decide(&st, &c, &id, body, "reject").await
}

async fn withdraw_proposal(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    decide(&st, &c, &id, None, "withdraw").await
}

async fn revert_proposal(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    decide(&st, &c, &id, None, "revert").await
}

async fn get_approvals(State(st): State<AppState>) -> AppResult<Json<Value>> {
    let mut conn = st.pool.acquire().await?;
    Ok(Json(proposals::settings(&mut conn).await?))
}

async fn set_approvals(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Json(b): Json<SettingsPatch>,
) -> AppResult<Json<Value>> {
    only_you(&c)?;
    let mut tx = st.pool.begin().await?;
    let v = proposals::set_settings(&mut tx, b).await?;
    tx.commit().await?;
    Ok(Json(v))
}

// ---------- Artefacts ----------

async fn list_artefacts(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(slug): Path<String>,
) -> AppResult<Json<Value>> {
    let mut conn = st.pool.acquire().await?;
    let s = space(&mut conn, &c, &slug).await?;
    Ok(Json(
        json!({ "artefacts": proposals::artefacts(&mut conn, &s.id).await? }),
    ))
}

async fn add_artefact(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(slug): Path<String>,
    Json(a): Json<ArtefactLink>,
) -> AppResult<(StatusCode, Json<Value>)> {
    propose_instead(&c)?;
    let mut conn = st.pool.acquire().await?;
    let s = space(&mut conn, &c, &slug).await?;
    let id = proposals::link(&mut conn, &s.id, &a, &c.actor).await?;
    Ok((StatusCode::CREATED, Json(json!({ "id": id }))))
}

async fn remove_artefact(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(id): Path<String>,
) -> AppResult<StatusCode> {
    only_you(&c)?;
    let mut conn = st.pool.acquire().await?;
    let space_id: Option<String> =
        sqlx::query_scalar("SELECT space_id FROM artefacts WHERE id = $1")
            .bind(&id)
            .fetch_optional(&mut *conn)
            .await?;
    space_by_id(
        &mut conn,
        &c,
        &space_id.ok_or(AppError::NotFound("artefact"))?,
        "artefact",
    )
    .await?;
    proposals::remove_artefact(&mut conn, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------- Library ----------

#[derive(Deserialize)]
struct LibraryQuery {
    #[serde(default)]
    kind: Option<String>,
    /// A scope's slug (`_studio` or a client's); none means every scope you may use.
    #[serde(default)]
    scope: Option<String>,
    /// Search the library by meaning and words instead of listing it.
    #[serde(default)]
    q: Option<String>,
}

/// `GET /api/library`: items without their bodies (or search results, with `q`).
async fn list_library(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Query(p): Query<LibraryQuery>,
) -> AppResult<Json<Value>> {
    let mut conn = st.pool.acquire().await?;
    let scopes: Vec<String> = match p.scope.as_deref().filter(|s| !s.is_empty()) {
        Some(slug) => vec![space(&mut conn, &c, slug).await?.id],
        None => spaces_for(&mut conn, &c)
            .await?
            .into_iter()
            .map(|s| s.0)
            .collect(),
    };
    let kind = p.kind.filter(|k| !k.is_empty());
    if let Some(q) = p.q.filter(|q| !q.trim().is_empty()) {
        let query = search::Query {
            text: q,
            spaces: scopes,
            doc_type: kind,
            source: Some("library".into()),
            target: None,
            standing: None,
            limit: 20,
        };
        return Ok(Json(
            search::run(&mut conn, st.embedder.as_ref(), &query).await?,
        ));
    }
    let items = library::list(&mut conn, &scopes, kind.as_deref()).await?;
    let kinds: Vec<String> = {
        let mut k: Vec<String> = items.iter().map(|i| i.kind.clone()).collect();
        k.dedup();
        k
    };
    Ok(Json(
        json!({ "kinds": kinds, "items": items.iter().map(library::Item::summary_json).collect::<Vec<_>>() }),
    ))
}

/// The item at kind/scope/name (or, with `?fallback=1` for a client scope,
/// the studio's of that name), with the clients it's pinned to.
async fn find_item(
    conn: &mut PgConnection,
    c: &Caller,
    kind: &str,
    scope: &str,
    name: &str,
    fallback: bool,
) -> AppResult<library::Item> {
    let s = space(conn, c, scope).await?;
    if let Some(i) = library::find(conn, kind, &s.id, name).await? {
        return Ok(i);
    }
    if fallback
        && s.id != spaces::STUDIO
        && let Some(i) = library::find(conn, kind, spaces::STUDIO, name).await?
    {
        return Ok(i);
    }
    Err(AppError::NotFound("library item"))
}

#[derive(Deserialize)]
struct Fallback {
    #[serde(default)]
    fallback: Option<String>,
}

async fn get_library(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path((kind, scope, name)): Path<(String, String, String)>,
    Query(f): Query<Fallback>,
) -> AppResult<Json<Value>> {
    let mut conn = st.pool.acquire().await?;
    let item = find_item(
        &mut conn,
        &c,
        &kind,
        &scope,
        &name,
        f.fallback.is_some_and(|f| f != "0"),
    )
    .await?;
    let pins: Vec<Value> = library::pinned_to(&mut conn, &item.id)
        .await?
        .into_iter()
        .filter(|(_, slug, _)| c.may(slug))
        .map(|(id, slug, name)| json!({ "id": id, "slug": slug, "name": name }))
        .collect();
    let mut v = json!(item);
    v["pinned_to"] = json!(pins);
    Ok(Json(v))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PutItem {
    body: String,
    #[serde(default)]
    tags: Option<Vec<String>>,
    #[serde(default)]
    source: Option<String>,
    /// The version you're changing (to catch a change made meanwhile).
    #[serde(default)]
    version: Option<i32>,
    /// Why, for a proposal (when a key or an agent saves it).
    #[serde(default)]
    rationale: Option<String>,
}

/// `PUT /api/library/{kind}/{scope}/{name}`: saved at once if it's you (in
/// Hub, or in Design's UI); otherwise a proposal, which says so.
async fn put_library(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path((kind, scope, name)): Path<(String, String, String)>,
    Json(b): Json<PutItem>,
) -> AppResult<(StatusCode, Json<Value>)> {
    let mut tx = st.pool.begin().await?;
    let s = space(&mut tx, &c, &scope).await?;
    if c.is_you() {
        let put = library::Put {
            kind: &kind,
            space_id: &s.id,
            name: &name,
            body: &b.body,
            tags: b.tags.as_deref(),
            source: b.source.as_deref(),
            version: b.version,
        };
        let item = library::put(&mut tx, put, "user").await?;
        tx.commit().await?;
        st.index_wake.notify_one();
        return Ok((StatusCode::OK, Json(json!({ "item": item.summary_json() }))));
    }
    let change = json!({ "kind": kind, "name": name, "body": b.body, "tags": b.tags, "source": b.source, "version": b.version });
    let rationale = b
        .rationale
        .filter(|r| !r.trim().is_empty())
        .unwrap_or_else(|| format!("Saved by {}.", c.actor));
    let p = proposals::create(
        &mut tx,
        st.clients.as_ref(),
        &s.id,
        "library_put",
        change,
        &rationale,
        vec![],
        &c.actor,
    )
    .await?;
    tx.commit().await?;
    st.index_wake.notify_one();
    if p.status == "applied" {
        // Applied at once: answer as a save does, with the item.
        let mut conn = st.pool.acquire().await?;
        let item = library::find(&mut conn, &kind, &s.id, &name)
            .await?
            .ok_or(AppError::NotFound("library item"))?;
        return Ok((
            StatusCode::OK,
            Json(json!({ "item": item.summary_json(), "proposal": p.json() })),
        ));
    }
    Ok((StatusCode::ACCEPTED, Json(json!({ "proposal": p.json() }))))
}

async fn delete_library(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path((kind, scope, name)): Path<(String, String, String)>,
) -> AppResult<StatusCode> {
    if !c.is_you() {
        return Err(AppError::Forbidden("only you can delete a library item"));
    }
    let mut tx = st.pool.begin().await?;
    let item = find_item(&mut tx, &c, &kind, &scope, &name, false).await?;
    library::delete(&mut tx, &item.id).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn library_revisions(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path((kind, scope, name)): Path<(String, String, String)>,
) -> AppResult<Json<Value>> {
    let mut conn = st.pool.acquire().await?;
    let item = find_item(&mut conn, &c, &kind, &scope, &name, false).await?;
    Ok(Json(
        json!({ "revisions": library::revisions(&mut conn, &item.id).await? }),
    ))
}

/// What a client uses: its own items and pinned studio items.
async fn client_library(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(slug): Path<String>,
) -> AppResult<Json<Value>> {
    let mut conn = st.pool.acquire().await?;
    let s = space(&mut conn, &c, &slug).await?;
    let items = library::for_client(&mut conn, &s.id).await?;
    Ok(Json(
        json!({ "items": items.iter().map(library::Item::summary_json).collect::<Vec<_>>() }),
    ))
}

async fn pin_item(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path((slug, item)): Path<(String, String)>,
) -> AppResult<StatusCode> {
    propose_instead(&c)?;
    let mut conn = st.pool.acquire().await?;
    let s = space(&mut conn, &c, &slug).await?;
    library::pin(&mut conn, &s.id, &item, &c.actor).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn unpin_item(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path((slug, item)): Path<(String, String)>,
) -> AppResult<StatusCode> {
    propose_instead(&c)?;
    let mut conn = st.pool.acquire().await?;
    let s = space(&mut conn, &c, &slug).await?;
    library::unpin(&mut conn, &s.id, &item).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------- Runs ----------

#[derive(Deserialize)]
struct RunQuery {
    #[serde(default)]
    space: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

async fn list_runs(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Query(q): Query<RunQuery>,
) -> AppResult<Json<Value>> {
    let mut conn = st.pool.acquire().await?;
    let all = spaces_for(&mut conn, &c).await?;
    let ids: Vec<String> = match q.space.as_deref().filter(|s| !s.is_empty()) {
        Some(slug) => vec![space(&mut conn, &c, slug).await?.id],
        None => all.iter().map(|s| s.0.clone()).collect(),
    };
    let status = q.status.filter(|s| !s.is_empty() && s != "all");
    let mut list = runs::list(&mut conn, &ids, status.as_deref()).await?;
    for r in &mut list {
        if let Some((_, slug, name)) = all.iter().find(|s| r["space_id"] == s.0.as_str()) {
            r["space"] = json!({ "slug": slug, "name": name });
        }
    }
    Ok(Json(json!({ "runs": list })))
}

async fn run_for(conn: &mut PgConnection, c: &Caller, id: &str) -> AppResult<spaces::Space> {
    let r = runs::get(conn, id).await?;
    space_by_id(conn, c, &r.space_id, "run").await
}

async fn get_run(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let mut conn = st.pool.acquire().await?;
    let s = run_for(&mut conn, &c, &id).await?;
    let mut v = runs::full(&mut conn, &id).await?;
    v["space"] = json!({ "slug": s.slug, "name": s.name });
    Ok(Json(v))
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct StepNote {
    #[serde(default)]
    note: Option<String>,
}

/// Approving a checkpoint is yours, in Hub: never Claude's.
async fn approve_step(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path((id, ord)): Path<(String, i32)>,
    body: Option<Json<StepNote>>,
) -> AppResult<Json<Value>> {
    only_you(&c)?;
    let mut tx = st.pool.begin().await?;
    run_for(&mut tx, &c, &id).await?;
    let note = body.and_then(|b| b.0.note);
    let v = runs::approve(&mut tx, &id, ord, "user", note.as_deref()).await?;
    tx.commit().await?;
    Ok(Json(v))
}

async fn send_back_step(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path((id, ord)): Path<(String, i32)>,
    Json(b): Json<StepNote>,
) -> AppResult<Json<Value>> {
    only_you(&c)?;
    let mut tx = st.pool.begin().await?;
    run_for(&mut tx, &c, &id).await?;
    let v = runs::send_back(&mut tx, &id, ord, b.note.as_deref().unwrap_or("")).await?;
    tx.commit().await?;
    Ok(Json(v))
}

async fn cancel_run(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    only_you(&c)?;
    let mut tx = st.pool.begin().await?;
    run_for(&mut tx, &c, &id).await?;
    let v = runs::finish(&mut tx, &id, "cancelled", Some("Cancelled in Hub.")).await?;
    tx.commit().await?;
    Ok(Json(v))
}
