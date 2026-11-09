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
use crate::rules::{self, NewRule};
use crate::{index, passkeys, search, spaces};

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
async fn space(conn: &mut PgConnection, c: &Caller, slug: &str) -> AppResult<spaces::Space> {
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
async fn overrides(conn: &mut PgConnection, doc_id: &str) -> AppResult<Value> {
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
async fn space_by_id(
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

fn target(t: Option<String>) -> AppResult<Option<String>> {
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
    if b.text.len() > MAX_CHECK_BYTES {
        return Err(AppError::invalid("text", "check at most 1 MB at a time"));
    }
    let t = target(b.target)?;
    let mut conn = st.pool.acquire().await?;
    let s = space(&mut conn, &c, &slug).await?;
    let in_force: Vec<rules::InForce> = rules::for_space(&mut conn, &s.id)
        .await?
        .into_iter()
        .filter(|r| {
            r.rule.status == "approved" && r.excepted.is_none() && r.rule.applies(t.as_deref())
        })
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
                if let Some(found) = rules::run(check, &b.text) {
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
    let decisions: Vec<Value> = decisions::list(&mut conn, &s.id)
        .await?
        .iter()
        .filter(|d| d.status == "approved" && d.applies(t.as_deref()))
        .map(|d| json!({ "id": d.id, "statement": d.statement, "reason": d.reason, "overrides": d.contradicts.0 }))
        .collect();
    Ok(Json(json!({
        "ok": violations.is_empty(),
        "for": t.unwrap_or_else(|| "all".into()),
        "violations": violations,
        "advice": advice,
        "guidance": guidance,
        "decisions": decisions,
    })))
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
        && !matches!(src.as_str(), "document" | "rule" | "decision")
    {
        return Err(AppError::invalid(
            "source",
            "use document, rule or decision",
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
