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

use crate::clients::Clients;
use crate::decisions::{self, NewDecision};
use crate::documents::{self, Patch};
use crate::error::{AppError, AppResult};
use crate::library;
use crate::rules::{self, APPROVED_BY_YOU, AUTO, NewRule};
use crate::spaces::{self, STUDIO};

pub const KINDS: [&str; 17] = [
    "doc_edit",
    "doc_create",
    "doc_delete",
    "rule",
    "rule_edit",
    "rule_retire",
    "decision",
    "decision_edit",
    "decision_retire",
    "artefact_link",
    "artefact_unlink",
    "library_put",
    "library_pin",
    "library_delete",
    "client_create",
    "client_update",
    "client_delete",
];

/// Removals: these always wait for your approval, whatever the policies say.
pub const DELETES: [&str; 6] = [
    "doc_delete",
    "rule_retire",
    "decision_retire",
    "artefact_unlink",
    "library_delete",
    "client_delete",
];

pub fn is_delete(kind: &str) -> bool {
    DELETES.contains(&kind)
}
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
    /// Rename (move) it.
    #[serde(default)]
    path: Option<String>,
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

/// A library item to save, in the proposal's space (its scope).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LibraryPut {
    /// The app's kind, e.g. design-style.
    pub kind: String,
    pub name: String,
    /// The whole file (markdown).
    pub body: String,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub source: Option<String>,
    /// For a change to an existing item: the version it was made against.
    #[serde(default)]
    pub version: Option<i32>,
}

/// Pin (or unpin) a studio item for the proposal's client.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LibraryPin {
    /// The item's id.
    pub item: String,
    #[serde(default = "yes")]
    pub pin: bool,
}

fn yes() -> bool {
    true
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
                (None, true) if e.path.is_some() => content.clone(),
                _ => {
                    return Err(AppError::invalid(
                        "change",
                        "give content (the whole new document), edits, or a new path",
                    ));
                }
            };
            let new_path = match &e.path {
                Some(p) => Some(documents::check_path(p)?).filter(|p| *p != path),
                None => None,
            };
            if new == content && new_path.is_none() {
                return Err(AppError::invalid("change", "that changes nothing"));
            }
            let m = documents::meta(new_path.as_deref().unwrap_or(&path), &new)?;
            let manual = visibility == "client" || m.visibility == "client";
            let shown = match &new_path {
                Some(np) => format!("{path} → {np}"),
                None => path.clone(),
            };
            Ok(Ready {
                target: Some(id),
                base_version: Some(version),
                diff: Some(unified(&shown, version, &content, &new)),
                change: json!({ "content": new, "base": content, "path": path, "new_path": new_path }),
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
        "library_put" => {
            let l: LibraryPut = parse(change)?;
            library::check_kind(&l.kind)?;
            library::check_name(&l.name)?;
            let body = library::check_body(&l.body)?.to_string();
            let current = library::find(conn, &l.kind, space_id, &l.name).await?;
            let (base, base_version) = match &current {
                Some(cur) => {
                    if l.version.is_some_and(|v| v != cur.version) {
                        return Err(AppError::Conflict {
                            current: cur.summary_json(),
                        });
                    }
                    if cur.body == body && l.tags.as_ref().is_none_or(|t| *t == cur.tags) {
                        return Err(AppError::invalid("change", "that changes nothing"));
                    }
                    (Some(cur.body.clone()), Some(cur.version))
                }
                None => (None, None),
            };
            let label = format!("{}/{}", l.kind, l.name);
            Ok(Ready {
                target: current.as_ref().map(|c| c.id.clone()),
                base_version,
                diff: Some(unified(
                    &label,
                    base_version.unwrap_or(0),
                    base.as_deref().unwrap_or(""),
                    &body,
                )),
                change: json!({ "kind": l.kind, "name": l.name, "body": body, "tags": l.tags, "source": l.source,
                                "base": base, "label": label }),
                manual: false,
            })
        }
        "library_pin" => {
            let pin: LibraryPin = parse(change)?;
            let item = library::get(conn, &pin.item).await?;
            if item.space_id != STUDIO || space_id == STUDIO {
                return Err(AppError::invalid(
                    "change.item",
                    "pin a studio item to a client",
                ));
            }
            Ok(Ready {
                target: Some(item.id.clone()),
                base_version: None,
                diff: None,
                change: json!({ "item": item.id, "pin": pin.pin, "kind": item.kind, "name": item.name, "title": item.title }),
                manual: false,
            })
        }
        "doc_delete" => {
            let d: Target = parse(change)?;
            let (id, path, _, _, visibility) = find_doc(conn, space_id, &d.id("document")?).await?;
            Ok(Ready {
                target: Some(id.clone()),
                base_version: None,
                diff: None,
                change: json!({ "document": id, "path": path }),
                manual: visibility == "client",
            })
        }
        "rule_edit" | "rule_retire" => {
            let t: Target = parse(change.clone())?;
            let rule = rules::get(conn, &t.id("rule")?).await?;
            if rule.space_id != space_id || rule.status == "retired" {
                return Err(AppError::invalid(
                    "change.rule",
                    "no such rule in this space",
                ));
            }
            if rule.source_doc.is_some() {
                return Err(AppError::invalid(
                    "change.rule",
                    "this rule comes from the Writing rules document: edit that instead",
                ));
            }
            let base = json!({ "kind": rule.kind, "text": rule.text, "applies_to": rule.applies_to, "check": rule.check() });
            if kind == "rule_retire" {
                return Ok(Ready {
                    target: Some(rule.id.clone()),
                    base_version: None,
                    diff: None,
                    change: json!({ "rule": rule.id, "text": rule.text }),
                    manual: false,
                });
            }
            // A partial change, on top of the rule as it is.
            let mut new = base.clone();
            for k in ["kind", "text", "applies_to", "check"] {
                if let Some(v) = change.get(k) {
                    new[k] = v.clone();
                }
            }
            let r: NewRule = parse(new.clone())?;
            rules::valid_rule(&r)?;
            Ok(Ready {
                target: Some(rule.id.clone()),
                base_version: None,
                diff: None,
                change: json!({ "rule": rule.id, "new": r, "base": base }),
                manual: false,
            })
        }
        "decision_edit" | "decision_retire" => {
            let t: Target = parse(change.clone())?;
            let d = decisions::get(conn, &t.id("decision")?).await?;
            if d.space_id != space_id || !matches!(d.status.as_str(), "proposed" | "approved") {
                return Err(AppError::invalid(
                    "change.decision",
                    "no such decision in force here",
                ));
            }
            if kind == "decision_retire" {
                return Ok(Ready {
                    target: Some(d.id.clone()),
                    base_version: None,
                    diff: None,
                    change: json!({ "decision": d.id, "statement": d.statement, "was": d.status }),
                    manual: false,
                });
            }
            let mut patch = change.clone();
            if let Some(o) = patch.as_object_mut() {
                o.remove("decision");
            }
            let _: decisions::DecisionPatch = parse(patch.clone())?;
            let base = json!({
                "statement": d.statement, "reason": d.reason, "applies_to": d.applies_to, "evidence": d.evidence.0,
                "contradicts": d.contradicts.0.iter().map(|s| json!({ "doc": s.doc, "anchor": s.anchor })).collect::<Vec<_>>(),
            });
            Ok(Ready {
                target: Some(d.id.clone()),
                base_version: None,
                diff: None,
                change: json!({ "decision": d.id, "statement": d.statement, "patch": patch, "base": base }),
                manual: false,
            })
        }
        "library_delete" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Which {
                kind: String,
                name: String,
            }
            let w: Which = parse(change)?;
            let item = library::find(conn, &w.kind, space_id, &w.name)
                .await?
                .ok_or_else(|| AppError::invalid("change.name", "no such library item here"))?;
            Ok(Ready {
                target: Some(item.id.clone()),
                base_version: None,
                diff: None,
                change: json!({ "item": item.id, "kind": item.kind, "name": item.name, "title": item.title, "label": format!("{}/{}", item.kind, item.name) }),
                manual: false,
            })
        }
        "artefact_unlink" => {
            let t: Target = parse(change)?;
            let id = t.id("artefact")?;
            let a: Option<(String, String)> = sqlx::query_as(
                "SELECT title, url FROM artefacts WHERE id = $1 AND space_id = $2 AND removed_at IS NULL",
            )
            .bind(&id)
            .bind(space_id)
            .fetch_optional(&mut *conn)
            .await?;
            let (title, url) =
                a.ok_or_else(|| AppError::invalid("change.artefact", "no such linked work here"))?;
            Ok(Ready {
                target: Some(id.clone()),
                base_version: None,
                diff: None,
                change: json!({ "artefact": id, "title": title, "url": url }),
                manual: false,
            })
        }
        "client_create" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct NewClient {
                slug: String,
                name: String,
            }
            let c: NewClient = parse(change)?;
            let slug = c.slug.trim().to_string();
            let name = c.name.trim().to_string();
            let mut ch = slug.chars();
            if !(slug.len() <= 64
                && matches!(ch.next(), Some('a'..='z'))
                && ch.all(|c| matches!(c, 'a'..='z' | '0'..='9' | '-')))
            {
                return Err(AppError::invalid(
                    "change.slug",
                    "lowercase letters, digits and -, starting with a letter: acme-hotels",
                ));
            }
            if name.is_empty() || name.chars().count() > 200 {
                return Err(AppError::invalid("change.name", "give the client a name"));
            }
            if spaces::by_slug(conn, &slug).await.is_ok() {
                return Err(AppError::invalid(
                    "change.slug",
                    format!("`{slug}` is already a client"),
                ));
            }
            Ok(Ready {
                target: None,
                base_version: None,
                diff: None,
                change: json!({ "slug": slug, "name": name }),
                manual: false,
            })
        }
        "client_update" | "client_delete" => {
            let s: (String, String, String) = sqlx::query_as(
                "SELECT slug, name, kind FROM spaces WHERE id = $1 AND archived_at IS NULL",
            )
            .bind(space_id)
            .fetch_one(&mut *conn)
            .await?;
            if s.2 != "client" {
                return Err(AppError::invalid("client", "the studio isn't a client"));
            }
            if kind == "client_delete" {
                return Ok(Ready {
                    target: Some(space_id.to_string()),
                    base_version: None,
                    diff: None,
                    change: json!({ "slug": s.0, "name": s.1 }),
                    manual: false,
                });
            }
            let name = change["name"]
                .as_str()
                .map(str::trim)
                .unwrap_or_default()
                .to_string();
            if name.is_empty() || name.chars().count() > 200 || name == s.1 {
                return Err(AppError::invalid(
                    "change.name",
                    "give the client a new name",
                ));
            }
            Ok(Ready {
                target: Some(space_id.to_string()),
                base_version: None,
                diff: None,
                change: json!({ "slug": s.0, "name": name, "old_name": s.1 }),
                manual: false,
            })
        }
        other => Err(AppError::invalid(
            "kind",
            format!("`{other}` isn't one of {}", KINDS.join(", ")),
        )),
    }
}

/// `{document: …}`, `{rule: …}` and the like: the one thing a change is about.
#[derive(Deserialize)]
struct Target(serde_json::Map<String, Value>);

impl Target {
    fn id(&self, key: &str) -> AppResult<String> {
        self.0
            .get(key)
            .and_then(Value::as_str)
            .map(String::from)
            .ok_or_else(|| AppError::invalid(format!("change.{key}"), format!("say which {key}")))
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

// ---------- Policies ----------

/// What applies for a space and kind: its own row, the '*' row, or `auto`
/// (Claude's creates and updates apply at once). Deletes always wait.
pub async fn mode(conn: &mut PgConnection, space_id: &str, kind: &str) -> AppResult<String> {
    let m: Option<String> = sqlx::query_scalar(
        "SELECT mode FROM approval_policies WHERE kind = $2 AND space_id IN ($1, '*')
         ORDER BY space_id = '*' LIMIT 1",
    )
    .bind(space_id)
    .bind(kind)
    .fetch_optional(conn)
    .await?;
    if is_delete(kind) {
        return Ok("review".into());
    }
    Ok(m.unwrap_or_else(|| "auto".into()))
}

pub async fn settings(conn: &mut PgConnection) -> AppResult<Value> {
    let policies: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT space_id, kind, mode FROM approval_policies ORDER BY space_id, kind",
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(json!({
        "kinds": KINDS.iter().filter(|k| !is_delete(k)).collect::<Vec<_>>(),
        "deletes": DELETES,
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
        if is_delete(&pol.kind) {
            return Err(AppError::invalid(
                format!("policies[{i}].kind"),
                "removals always wait for you",
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
    settings(conn).await
}

// ---------- Proposing, approving, rejecting, reverting ----------

/// Make a change as a proposal. It applies at once unless it's a removal,
/// its kind is set to wait for this space, or it stays manual; either way
/// it's recorded, and an applied one can be reverted.
#[allow(clippy::too_many_arguments)]
pub async fn create(
    conn: &mut PgConnection,
    clients: Option<&Clients>,
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
    if !r.manual && mode(conn, space_id, kind).await? == "auto" {
        return apply(conn, clients, p, "auto", None).await;
    }
    Ok(p)
}

/// Why a proposal must wait for you, in the UI, whatever the switch says
/// (and even if Claude says you approved it).
pub fn stays_manual(p: &Proposal) -> Option<&'static str> {
    if p.change.0["manual"] == true {
        return Some("it changes a document clients can see");
    }
    is_delete(&p.kind).then_some("it removes something")
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
    clients: Option<&Clients>,
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
                path: c["new_path"].as_str().map(String::from),
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
        "library_put" => {
            let tags: Option<Vec<String>> =
                serde_json::from_value(c["tags"].clone()).unwrap_or(None);
            let put = library::Put {
                kind: c["kind"].as_str().unwrap_or_default(),
                space_id: &p.space_id,
                name: c["name"].as_str().unwrap_or_default(),
                body: c["body"].as_str().unwrap_or_default(),
                tags: tags.as_deref(),
                source: c["source"].as_str(),
                version: p.base_version,
            };
            let exists = library::find(conn, put.kind, put.space_id, put.name).await?;
            // Changed (or made) since the proposal: stale, as for a doc edit.
            if exists.as_ref().map(|i| i.version) != p.base_version {
                sqlx::query("UPDATE proposals SET status = 'stale', decided_by = $1, decided_at = now() WHERE id = $2")
                    .bind(decided_by)
                    .bind(&p.id)
                    .execute(&mut *conn)
                    .await?;
                return get(conn, &p.id).await;
            }
            let item = library::put(conn, put, &by).await?;
            json!({ "library": item.id, "version": item.version, "created": p.base_version.is_none() })
        }
        "library_pin" => {
            let item = c["item"].as_str().unwrap_or_default();
            if c["pin"] == false {
                library::unpin(conn, &p.space_id, item).await?;
            } else {
                library::pin(conn, &p.space_id, item, &by).await?;
            }
            json!({ "library": item, "pin": c["pin"] != false })
        }
        "artefact_link" => {
            let a: ArtefactLink = parse(c.clone())?;
            let id = link(conn, &p.space_id, &a, &by).await?;
            json!({ "artefact": id })
        }
        "doc_delete" => {
            let d =
                documents::delete(conn, c["document"].as_str().unwrap_or_default(), &by).await?;
            json!({ "document": d.id })
        }
        "rule_edit" => {
            let r = rules::update(
                conn,
                c["rule"].as_str().unwrap_or_default(),
                parse(c["new"].clone())?,
                &by,
            )
            .await?;
            json!({ "rule": r.id })
        }
        "rule_retire" => {
            let r = rules::retire(conn, c["rule"].as_str().unwrap_or_default(), &by).await?;
            json!({ "rule": r.id })
        }
        "decision_edit" => {
            let d = decisions::update(
                conn,
                c["decision"].as_str().unwrap_or_default(),
                parse(c["patch"].clone())?,
                &by,
            )
            .await?;
            json!({ "decision": d.id })
        }
        "decision_retire" => {
            let d =
                decisions::revert(conn, c["decision"].as_str().unwrap_or_default(), &by).await?;
            json!({ "decision": d.id, "was": c["was"] })
        }
        "library_delete" => {
            let item = c["item"].as_str().unwrap_or_default();
            library::delete(conn, item).await?;
            json!({ "library": item })
        }
        "artefact_unlink" => {
            let id = c["artefact"].as_str().unwrap_or_default();
            remove_artefact(conn, id).await?;
            json!({ "artefact": id })
        }
        "client_create" | "client_update" | "client_delete" => {
            let clients = clients.ok_or(AppError::Unavailable(
                "Hub isn't connected to Clients".into(),
            ))?;
            let slug = c["slug"].as_str().unwrap_or_default();
            match p.kind.as_str() {
                "client_create" => {
                    let info = clients
                        .create_client(slug, c["name"].as_str().unwrap_or_default())
                        .await?;
                    // The space now, rather than at the next event poll.
                    spaces::ensure(conn, &info.id, &info.slug, &info.name, "client").await?;
                    json!({ "client": info.id, "slug": info.slug })
                }
                "client_update" => {
                    let info = clients
                        .rename_client(slug, c["name"].as_str().unwrap_or_default())
                        .await?;
                    spaces::ensure(conn, &info.id, &info.slug, &info.name, "client").await?;
                    json!({ "client": info.id, "slug": info.slug })
                }
                _ => {
                    clients.delete_client(slug).await?;
                    spaces::archive(conn, &p.space_id).await?;
                    json!({ "client": p.space_id, "slug": slug })
                }
            }
        }
        other => {
            return Err(AppError::invalid(
                "kind",
                format!("`{other}` can't be applied"),
            ));
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
    clients: Option<&Clients>,
    id: &str,
    decided_by: &str,
    note: Option<&str>,
) -> AppResult<Proposal> {
    let p = get(conn, id).await?;
    apply(conn, clients, p, decided_by, note).await
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
pub async fn revert(
    conn: &mut PgConnection,
    clients: Option<&Clients>,
    id: &str,
    by: &str,
) -> AppResult<Proposal> {
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
            let renamed = p.change.0["new_path"].is_string();
            let patch = Patch {
                version: applied,
                content: p.change.0["base"].as_str().map(String::from),
                path: renamed.then(|| p.change.0["path"].as_str().unwrap_or_default().to_string()),
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
        "library_put" => {
            let id = s("library");
            if result["created"] == true {
                library::delete(conn, &id).await?;
            } else {
                let item = library::get(conn, &id).await?;
                if item.version != result["version"].as_i64().unwrap_or_default() as i32 {
                    return Err(AppError::State {
                        code: "changed_since",
                        message: "the item has changed since; restore the version you want from its history".into(),
                    });
                }
                let put = library::Put {
                    kind: &item.kind,
                    space_id: &item.space_id,
                    name: &item.name,
                    body: p.change.0["base"].as_str().unwrap_or_default(),
                    tags: None,
                    source: None,
                    version: Some(item.version),
                };
                library::put(conn, put, by).await?;
            }
        }
        "library_pin" => {
            let item = s("library");
            if result["pin"] == true {
                library::unpin(conn, &p.space_id, &item).await?;
            } else {
                library::pin(conn, &p.space_id, &item, by).await?;
            }
        }
        "artefact_link" => {
            sqlx::query("UPDATE artefacts SET removed_at = now() WHERE id = $1")
                .bind(s("artefact"))
                .execute(&mut *conn)
                .await?;
        }
        "doc_delete" => {
            documents::undelete(conn, &s("document")).await?;
        }
        "rule_edit" => {
            rules::update(conn, &s("rule"), parse(p.change.0["base"].clone())?, by).await?;
        }
        "rule_retire" => {
            rules::unretire(conn, &s("rule")).await?;
        }
        "decision_edit" => {
            decisions::update(conn, &s("decision"), parse(p.change.0["base"].clone())?, by).await?;
        }
        "decision_retire" => {
            decisions::reinstate(
                conn,
                &s("decision"),
                result["was"].as_str().unwrap_or("approved"),
            )
            .await?;
        }
        "library_delete" => {
            library::undelete(conn, &s("library")).await?;
        }
        "artefact_unlink" => {
            sqlx::query("UPDATE artefacts SET removed_at = NULL WHERE id = $1")
                .bind(s("artefact"))
                .execute(&mut *conn)
                .await?;
        }
        "client_update" => {
            let clients = clients.ok_or(AppError::Unavailable(
                "Hub isn't connected to Clients".into(),
            ))?;
            let old = p.change.0["old_name"].as_str().unwrap_or_default();
            let info = clients.rename_client(&s("slug"), old).await?;
            spaces::ensure(conn, &info.id, &info.slug, &info.name, "client").await?;
        }
        _ => {
            return Err(AppError::State {
                code: "not_revertible",
                message: "adding or deleting a client can't be undone here; add or delete it again"
                    .into(),
            });
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
