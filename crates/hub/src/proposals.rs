//! Proposals (spec section 7): every change that isn't yours, in the UI.
//! A proposal waits for you, or applies at once when the hands-off switch
//! is on and its kind is allowed for the space. Applied ones can be
//! reverted. Some never apply by themselves, whatever the switch says.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use similar::TextDiff;
use sqlx::PgConnection;
use sqlx::types::Json;

use crate::decisions::{self, NewDecision};
use crate::documents::{self, Patch};
use crate::error::{AppError, AppResult};
use crate::rules::{self, APPROVED_BY_YOU, AUTO, NewRule};

pub const KINDS: [&str; 5] = [
    "doc_edit",
    "doc_create",
    "rule",
    "decision",
    "artefact_link",
];
pub const APPS: [&str; 5] = ["board", "design", "motion", "typeset", "other"];
const MAX_RATIONALE_CHARS: usize = 2000;
const MAX_SOURCES: usize = 20;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Proposal {
    pub id: String,
    pub space_id: String,
    pub kind: String,
    pub target: Option<String>,
    pub base_version: Option<i32>,
    pub change: Json<Value>,
    pub diff: Option<String>,
    pub rationale: String,
    pub sources: Json<Value>,
    pub proposed_by: String,
    pub status: String,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub note: Option<String>,
    pub result: Option<Json<Value>>,
    pub created_at: DateTime<Utc>,
}

impl Proposal {
    /// As the API returns it (without a doc edit's full old text).
    pub fn json(&self) -> Value {
        let mut v = json!(self);
        let mut change = self.change.0.clone();
        if let Some(o) = change.as_object_mut() {
            o.remove("base");
        }
        v["change"] = change;
        v["sources"] = self.sources.0.clone();
        v["result"] = self.result.as_ref().map_or(Value::Null, |r| r.0.clone());
        v
    }
}

const COLS: &str = "id, space_id, kind, target, base_version, change, diff, rationale, sources, proposed_by, \
                    status, decided_by, decided_at, note, result, created_at";

pub async fn get(conn: &mut PgConnection, id: &str) -> AppResult<Proposal> {
    sqlx::query_as::<_, Proposal>(sqlx::AssertSqlSafe(format!(
        "SELECT {COLS} FROM proposals WHERE id = $1"
    )))
    .bind(id)
    .fetch_optional(conn)
    .await?
    .ok_or(AppError::NotFound("proposal"))
}

/// Proposals in these spaces, newest first, optionally of one status.
pub async fn list(
    conn: &mut PgConnection,
    spaces: &[String],
    status: Option<&str>,
    limit: i64,
) -> AppResult<Vec<Proposal>> {
    Ok(sqlx::query_as::<_, Proposal>(sqlx::AssertSqlSafe(format!(
        "SELECT {COLS} FROM proposals WHERE space_id = ANY($1) AND ($2::text IS NULL OR status = $2)
         ORDER BY created_at DESC LIMIT $3"
    )))
    .bind(spaces)
    .bind(status)
    .bind(limit)
    .fetch_all(conn)
    .await?)
}

// ---------- What can be proposed ----------

/// One change in a document edit: `find` must appear exactly once.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Edit {
    pub find: String,
    pub replace: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DocEdit {
    /// The document's id or path.
    document: String,
    /// The version the change was made against.
    version: i32,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    edits: Vec<Edit>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DocCreate {
    path: String,
    content: String,
}

/// Something made in another app that belongs to this client.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArtefactLink {
    /// board, design, motion, typeset or other.
    pub app: String,
    /// What it is: board, design, scene, pdf, …
    pub kind: String,
    pub title: String,
    pub url: String,
    /// The app's own id, e.g. a board id.
    #[serde(default)]
    pub external_id: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

fn parse<T: for<'de> Deserialize<'de>>(change: Value) -> AppResult<T> {
    serde_json::from_value(change).map_err(|e| AppError::invalid("change", e.to_string()))
}

/// A document's id, path, version, content and visibility, by id or path.
async fn find_doc(
    conn: &mut PgConnection,
    space_id: &str,
    doc: &str,
) -> AppResult<(String, String, i32, String, String)> {
    sqlx::query_as(
        "SELECT id, path, version, content, visibility FROM documents
         WHERE space_id = $1 AND deleted_at IS NULL AND (id = $2 OR path = $3)",
    )
    .bind(space_id)
    .bind(doc)
    .bind(doc.trim_start_matches('/').to_lowercase())
    .fetch_optional(conn)
    .await?
    .ok_or_else(|| {
        AppError::invalid(
            "change.document",
            format!("there's no document `{doc}` here"),
        )
    })
}

fn unified(path: &str, from: i32, old: &str, new: &str) -> String {
    TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header(
            &format!("{path} (version {from})"),
            &format!("{path} (proposed)"),
        )
        .to_string()
}

/// A checked proposal, before it's stored.
struct Ready {
    target: Option<String>,
    base_version: Option<i32>,
    change: Value,
    diff: Option<String>,
    /// Never applies by itself: a client-visible document.
    manual: bool,
}

async fn ready(
    conn: &mut PgConnection,
    space_id: &str,
    kind: &str,
    change: Value,
) -> AppResult<Ready> {
    match kind {
        "doc_edit" => {
            let e: DocEdit = parse(change)?;
            let (id, path, version, content, visibility) =
                find_doc(conn, space_id, &e.document).await?;
            if e.version != version {
                return Err(AppError::Conflict {
                    current: documents::get(conn, &id).await?.json(),
                });
            }
            let new = match (e.content, e.edits.is_empty()) {
                (Some(c), true) => c,
                (None, false) => {
                    let mut text = content.clone();
                    for (i, ed) in e.edits.iter().enumerate() {
                        let n = text.matches(&ed.find).count();
                        if ed.find.is_empty() || n != 1 {
                            return Err(AppError::invalid(
                                format!("change.edits[{i}].find"),
                                if n == 0 {
                                    "not found in the document".to_string()
                                } else {
                                    format!("found {n} times; include more text so it's unique")
                                },
                            ));
                        }
                        text = text.replacen(&ed.find, &ed.replace, 1);
                    }
                    text
                }
                _ => {
                    return Err(AppError::invalid(
                        "change",
                        "give either content (the whole new document) or edits",
                    ));
                }
            };
            if new == content {
                return Err(AppError::invalid("change", "that changes nothing"));
            }
            let m = documents::meta(&path, &new)?;
            let manual = visibility == "client" || m.visibility == "client";
            Ok(Ready {
                target: Some(id),
                base_version: Some(version),
                diff: Some(unified(&path, version, &content, &new)),
                change: json!({ "content": new, "base": content, "path": path }),
                manual,
            })
        }
        "doc_create" => {
            let c: DocCreate = parse(change)?;
            let path = documents::check_path(&c.path)?;
            let m = documents::meta(&path, &c.content)?;
            let taken: Option<String> = sqlx::query_scalar(
                "SELECT id FROM documents WHERE space_id = $1 AND path = $2 AND deleted_at IS NULL",
            )
            .bind(space_id)
            .bind(&path)
            .fetch_optional(&mut *conn)
            .await?;
            if taken.is_some() {
                return Err(AppError::invalid(
                    "change.path",
                    format!("there's already a document at `{path}`; propose an edit"),
                ));
            }
            Ok(Ready {
                target: None,
                base_version: None,
                diff: Some(unified(&path, 0, "", &c.content)),
                change: json!({ "path": path, "content": c.content }),
                manual: m.visibility == "client",
            })
        }
        "rule" => {
            let r: NewRule = parse(change)?;
            rules::valid_rule(&r)?;
            Ok(Ready {
                target: None,
                base_version: None,
                diff: None,
                change: json!(r),
                manual: false,
            })
        }
        "decision" => {
            let d: NewDecision = parse(change)?;
            decisions::prepare(conn, space_id, d.clone()).await?;
            Ok(Ready {
                target: None,
                base_version: None,
                diff: None,
                change: json!(d),
                manual: false,
            })
        }
        "artefact_link" => {
            let a: ArtefactLink = parse(change)?;
            check_artefact(&a)?;
            Ok(Ready {
                target: None,
                base_version: None,
                diff: None,
                change: json!(a),
                manual: false,
            })
        }
        other => Err(AppError::invalid(
            "kind",
            format!("`{other}` isn't one of {}", KINDS.join(", ")),
        )),
    }
}

fn check_artefact(a: &ArtefactLink) -> AppResult<()> {
    if !APPS.contains(&a.app.as_str()) {
        return Err(AppError::invalid(
            "change.app",
            format!("use one of {}", APPS.join(", ")),
        ));
    }
    if !(a.url.starts_with("https://") || a.url.starts_with("http://")) || a.url.len() > 2000 {
        return Err(AppError::invalid(
            "change.url",
            "give a link starting https://",
        ));
    }
    if a.title.trim().is_empty()
        || a.title.chars().count() > 200
        || a.kind.trim().is_empty()
        || a.kind.len() > 40
    {
        return Err(AppError::invalid(
            "change.title",
            "give a title (up to 200 characters) and a kind",
        ));
    }
    Ok(())
}

fn check_sources(sources: &[Value]) -> AppResult<()> {
    if sources.len() > MAX_SOURCES {
        return Err(AppError::invalid(
            "sources",
            format!("at most {MAX_SOURCES}"),
        ));
    }
    for (i, s) in sources.iter().enumerate() {
        if !s.is_object()
            || (s.get("doc").is_none() && s.get("url").is_none() && s.get("decision").is_none())
        {
            return Err(AppError::invalid(
                format!("sources[{i}]"),
                "each source is {doc, anchor?}, {decision} or {url, note?}",
            ));
        }
    }
    Ok(())
}

// ---------- Policies and the hands-off switch ----------

pub async fn hands_off(conn: &mut PgConnection) -> AppResult<bool> {
    let v: Option<Json<Value>> =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = 'hands_off'")
            .fetch_optional(conn)
            .await?;
    Ok(v.is_some_and(|v| v.0 == json!(true)))
}

/// What applies for a space and kind: its own row, the '*' row, or review.
pub async fn mode(conn: &mut PgConnection, space_id: &str, kind: &str) -> AppResult<String> {
    let m: Option<String> = sqlx::query_scalar(
        "SELECT mode FROM approval_policies WHERE kind = $2 AND space_id IN ($1, '*')
         ORDER BY space_id = '*' LIMIT 1",
    )
    .bind(space_id)
    .bind(kind)
    .fetch_optional(conn)
    .await?;
    Ok(m.unwrap_or_else(|| "review".into()))
}

pub async fn settings(conn: &mut PgConnection) -> AppResult<Value> {
    let policies: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT space_id, kind, mode FROM approval_policies ORDER BY space_id, kind",
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(json!({
        "hands_off": hands_off(conn).await?,
        "kinds": KINDS,
        "policies": policies.into_iter().map(|(s, k, m)| json!({ "space": s, "kind": k, "mode": m })).collect::<Vec<_>>(),
    }))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    /// A space id, or `*` for every space.
    pub space: String,
    pub kind: String,
    pub mode: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsPatch {
    #[serde(default)]
    pub hands_off: Option<bool>,
    #[serde(default)]
    pub policies: Vec<Policy>,
}

pub async fn set_settings(conn: &mut PgConnection, p: SettingsPatch) -> AppResult<Value> {
    for (i, pol) in p.policies.iter().enumerate() {
        if !KINDS.contains(&pol.kind.as_str()) || !matches!(pol.mode.as_str(), "review" | "auto") {
            return Err(AppError::invalid(
                format!("policies[{i}]"),
                "a kind, and a mode of review or auto",
            ));
        }
        if pol.space != "*" {
            let exists: Option<String> = sqlx::query_scalar("SELECT id FROM spaces WHERE id = $1")
                .bind(&pol.space)
                .fetch_optional(&mut *conn)
                .await?;
            if exists.is_none() {
                return Err(AppError::invalid(
                    format!("policies[{i}].space"),
                    "no such space",
                ));
            }
        }
        sqlx::query(
            "INSERT INTO approval_policies (space_id, kind, mode) VALUES ($1, $2, $3)
             ON CONFLICT (space_id, kind) DO UPDATE SET mode = EXCLUDED.mode",
        )
        .bind(&pol.space)
        .bind(&pol.kind)
        .bind(&pol.mode)
        .execute(&mut *conn)
        .await?;
    }
    if let Some(on) = p.hands_off {
        sqlx::query("UPDATE settings SET value = $1, updated_at = now() WHERE key = 'hands_off'")
            .bind(Json(json!(on)))
            .execute(&mut *conn)
            .await?;
    }
    settings(conn).await
}

// ---------- Proposing, approving, rejecting, reverting ----------

/// Make a proposal. It applies at once if the hands-off switch is on, its
/// kind is `auto` for the space, and it isn't one that stays manual.
pub async fn create(
    conn: &mut PgConnection,
    space_id: &str,
    kind: &str,
    change: Value,
    rationale: &str,
    sources: Vec<Value>,
    by: &str,
) -> AppResult<Proposal> {
    let rationale = rationale.trim();
    if rationale.is_empty() || rationale.chars().count() > MAX_RATIONALE_CHARS {
        return Err(AppError::invalid(
            "rationale",
            format!("say why, in up to {MAX_RATIONALE_CHARS} characters"),
        ));
    }
    check_sources(&sources)?;
    let mut r = ready(conn, space_id, kind, change).await?;
    if r.manual {
        r.change["manual"] = json!(true);
    }
    let id = format!("prp_{}", ulid::Ulid::generate());
    sqlx::query(
        "INSERT INTO proposals (id, space_id, kind, target, base_version, change, diff, rationale, sources, proposed_by, status)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'pending')",
    )
    .bind(&id)
    .bind(space_id)
    .bind(kind)
    .bind(&r.target)
    .bind(r.base_version)
    .bind(Json(&r.change))
    .bind(&r.diff)
    .bind(rationale)
    .bind(Json(json!(sources)))
    .bind(by)
    .execute(&mut *conn)
    .await?;
    let p = get(conn, &id).await?;
    if !r.manual && hands_off(conn).await? && mode(conn, space_id, kind).await? == "auto" {
        return apply(conn, p, "auto", None).await;
    }
    Ok(p)
}

/// Why a proposal must wait for you, in the UI, whatever the switch says
/// (and even if Claude says you approved it).
pub fn stays_manual(p: &Proposal) -> Option<&'static str> {
    (p.change.0["manual"] == true).then_some("it changes a document clients can see")
}

fn pending(p: &Proposal) -> AppResult<()> {
    if p.status != "pending" {
        return Err(AppError::State {
            code: "not_pending",
            message: format!("this proposal is {}, not waiting", p.status),
        });
    }
    Ok(())
}

/// Apply a pending proposal: `decided_by` is "user", "user via <key>" or
/// "auto". A doc edit whose document moved on becomes `stale` instead.
pub async fn apply(
    conn: &mut PgConnection,
    p: Proposal,
    decided_by: &str,
    note: Option<&str>,
) -> AppResult<Proposal> {
    pending(&p)?;
    let by = if decided_by == "auto" {
        format!("{}{AUTO}", p.proposed_by)
    } else {
        format!("{}{APPROVED_BY_YOU}", p.proposed_by)
    };
    let c = &p.change.0;
    let result = match p.kind.as_str() {
        "doc_edit" => {
            let doc = p.target.clone().unwrap_or_default();
            let current = sqlx::query_scalar::<_, i32>(
                "SELECT version FROM documents WHERE id = $1 AND deleted_at IS NULL",
            )
            .bind(&doc)
            .fetch_optional(&mut *conn)
            .await?;
            if current != p.base_version {
                sqlx::query("UPDATE proposals SET status = 'stale', decided_by = $1, decided_at = now() WHERE id = $2")
                    .bind(decided_by)
                    .bind(&p.id)
                    .execute(&mut *conn)
                    .await?;
                return get(conn, &p.id).await;
            }
            let patch = Patch {
                version: p.base_version.unwrap_or_default(),
                content: c["content"].as_str().map(String::from),
                path: None,
            };
            let d = documents::update(conn, &doc, patch, &by).await?;
            json!({ "document": d.id, "version": d.version })
        }
        "doc_create" => {
            let d = documents::create(
                conn,
                &p.space_id,
                c["path"].as_str().unwrap_or_default(),
                c["content"].as_str().unwrap_or_default(),
                &by,
            )
            .await?;
            json!({ "document": d.id, "version": d.version })
        }
        "rule" => {
            let r = rules::create(conn, &p.space_id, parse(c.clone())?, &by).await?;
            json!({ "rule": r.id })
        }
        "decision" => {
            let d = decisions::create(conn, &p.space_id, parse(c.clone())?, &by).await?;
            json!({ "decision": d.id })
        }
        _ => {
            let a: ArtefactLink = parse(c.clone())?;
            let id = link(conn, &p.space_id, &a, &by).await?;
            json!({ "artefact": id })
        }
    };
    sqlx::query(
        "UPDATE proposals SET status = 'applied', decided_by = $1, decided_at = now(), note = $2, result = $3 WHERE id = $4",
    )
    .bind(decided_by)
    .bind(note)
    .bind(Json(result))
    .bind(&p.id)
    .execute(&mut *conn)
    .await?;
    get(conn, &p.id).await
}

/// Approve: yours alone (in the UI, or by Claude passing on your words).
pub async fn approve(
    conn: &mut PgConnection,
    id: &str,
    decided_by: &str,
    note: Option<&str>,
) -> AppResult<Proposal> {
    let p = get(conn, id).await?;
    apply(conn, p, decided_by, note).await
}

pub async fn reject(
    conn: &mut PgConnection,
    id: &str,
    decided_by: &str,
    note: Option<&str>,
) -> AppResult<Proposal> {
    let p = get(conn, id).await?;
    pending(&p)?;
    sqlx::query("UPDATE proposals SET status = 'rejected', decided_by = $1, decided_at = now(), note = $2 WHERE id = $3")
        .bind(decided_by)
        .bind(note)
        .bind(id)
        .execute(&mut *conn)
        .await?;
    get(conn, id).await
}

/// The proposer takes it back.
pub async fn withdraw(conn: &mut PgConnection, id: &str, by: &str) -> AppResult<Proposal> {
    let p = get(conn, id).await?;
    pending(&p)?;
    if p.proposed_by != by {
        return Err(AppError::Forbidden(
            "only whoever proposed it can withdraw it",
        ));
    }
    sqlx::query("UPDATE proposals SET status = 'withdrawn', decided_by = $1, decided_at = now() WHERE id = $2")
        .bind(by)
        .bind(id)
        .execute(&mut *conn)
        .await?;
    get(conn, id).await
}

/// Undo an applied proposal. A doc edit is undone only if nothing changed
/// the document since; otherwise restore an older version from History.
pub async fn revert(conn: &mut PgConnection, id: &str, by: &str) -> AppResult<Proposal> {
    let p = get(conn, id).await?;
    if p.status != "applied" {
        return Err(AppError::State {
            code: "not_applied",
            message: format!(
                "this proposal is {}; only an applied one can be reverted",
                p.status
            ),
        });
    }
    let result = p.result.as_ref().map(|r| r.0.clone()).unwrap_or_default();
    let s = |k: &str| result[k].as_str().unwrap_or_default().to_string();
    match p.kind.as_str() {
        "doc_edit" => {
            let doc = s("document");
            let applied = result["version"].as_i64().unwrap_or_default() as i32;
            let current = documents::get(conn, &doc).await?;
            if current.version != applied {
                return Err(AppError::State {
                    code: "changed_since",
                    message: "the document has changed since; restore the version you want from its History".into(),
                });
            }
            let patch = Patch {
                version: applied,
                content: p.change.0["base"].as_str().map(String::from),
                path: None,
            };
            documents::update(conn, &doc, patch, by).await?;
        }
        "doc_create" => {
            documents::delete(conn, &s("document"), by).await?;
        }
        "rule" => {
            rules::retire(conn, &s("rule"), by).await?;
        }
        "decision" => {
            decisions::revert(conn, &s("decision"), by).await?;
        }
        _ => {
            sqlx::query("UPDATE artefacts SET removed_at = now() WHERE id = $1")
                .bind(s("artefact"))
                .execute(&mut *conn)
                .await?;
        }
    }
    sqlx::query("UPDATE proposals SET status = 'reverted', decided_by = $1, decided_at = now() WHERE id = $2")
        .bind(by)
        .bind(id)
        .execute(&mut *conn)
        .await?;
    get(conn, id).await
}

// ---------- Artefacts ----------

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Artefact {
    pub id: String,
    pub space_id: String,
    pub app: String,
    pub kind: String,
    pub title: String,
    pub url: String,
    pub external_id: Option<String>,
    pub note: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
}

/// Record a link (or refresh one with the same URL).
pub async fn link(
    conn: &mut PgConnection,
    space_id: &str,
    a: &ArtefactLink,
    by: &str,
) -> AppResult<String> {
    check_artefact(a)?;
    let id: String = sqlx::query_scalar(
        "INSERT INTO artefacts (id, space_id, app, kind, title, url, external_id, note, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         ON CONFLICT (space_id, url) WHERE removed_at IS NULL
           DO UPDATE SET title = EXCLUDED.title, kind = EXCLUDED.kind, note = EXCLUDED.note
         RETURNING id",
    )
    .bind(format!("art_{}", ulid::Ulid::generate()))
    .bind(space_id)
    .bind(&a.app)
    .bind(a.kind.trim())
    .bind(a.title.trim())
    .bind(&a.url)
    .bind(&a.external_id)
    .bind(&a.note)
    .bind(by)
    .fetch_one(conn)
    .await?;
    Ok(id)
}

pub async fn artefacts(conn: &mut PgConnection, space_id: &str) -> AppResult<Vec<Artefact>> {
    Ok(sqlx::query_as(
        "SELECT id, space_id, app, kind, title, url, external_id, note, created_by, created_at
         FROM artefacts WHERE space_id = $1 AND removed_at IS NULL ORDER BY created_at DESC",
    )
    .bind(space_id)
    .fetch_all(conn)
    .await?)
}

pub async fn remove_artefact(conn: &mut PgConnection, id: &str) -> AppResult<()> {
    let r =
        sqlx::query("UPDATE artefacts SET removed_at = now() WHERE id = $1 AND removed_at IS NULL")
            .bind(id)
            .execute(conn)
            .await?;
    if r.rows_affected() == 0 {
        return Err(AppError::NotFound("artefact"));
    }
    Ok(())
}
