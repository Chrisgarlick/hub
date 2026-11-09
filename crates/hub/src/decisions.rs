//! Decisions (spec section 5): the work departs from (or settles) something
//! in the docs, and why. An approved decision overrides the document
//! sections it contradicts; a later one can supersede it.

use chrono::{DateTime, Utc};
use hub_model::Standing;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgConnection;
use sqlx::types::Json;

use crate::documents;
use crate::error::{AppError, AppResult};
use crate::rules::{by_you, check_targets};

const MAX_STATEMENT_CHARS: usize = 500;
const MAX_REASON_CHARS: usize = 4000;
const MAX_LINKS: usize = 20;

/// A document section a decision contradicts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Section {
    /// The document's id.
    pub doc: String,
    /// Its path when the decision was made (for reading; `doc` is what counts).
    pub path: String,
    /// The heading; none means the whole document.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
}

/// Where the decision came from or shows up: a board, a design, a scene.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    pub url: String,
    /// An item in it, e.g. a Board card id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Decision {
    pub id: String,
    pub space_id: String,
    pub statement: String,
    pub reason: String,
    pub contradicts: Json<Vec<Section>>,
    pub applies_to: Vec<String>,
    pub evidence: Json<Vec<Evidence>>,
    pub status: String,
    pub supersedes: Option<String>,
    pub superseded_by: Option<String>,
    pub proposed_by: String,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Decision {
    pub fn standing(&self) -> Standing {
        match self.status.as_str() {
            "approved" => Standing::Authoritative,
            "proposed" => Standing::Tentative,
            _ => Standing::Outdated,
        }
    }

    pub fn applies(&self, target: Option<&str>) -> bool {
        target.is_none_or(|t| self.applies_to.iter().any(|a| a == "all" || a == t))
    }

    pub fn json(&self) -> Value {
        let mut v = json!(self);
        v["contradicts"] = json!(self.contradicts.0);
        v["evidence"] = json!(self.evidence.0);
        v["standing"] = json!(self.standing());
        v
    }
}

const COLS: &str = "id, space_id, statement, reason, contradicts, applies_to, evidence, status, supersedes, \
                    superseded_by, proposed_by, decided_by, decided_at, created_at, updated_at";

pub async fn get(conn: &mut PgConnection, id: &str) -> AppResult<Decision> {
    sqlx::query_as::<_, Decision>(sqlx::AssertSqlSafe(format!(
        "SELECT {COLS} FROM decisions WHERE id = $1"
    )))
    .bind(id)
    .fetch_optional(conn)
    .await?
    .ok_or(AppError::NotFound("decision"))
}

/// A space's decisions: approved, then proposed, then the rest; newest first.
pub async fn list(conn: &mut PgConnection, space_id: &str) -> AppResult<Vec<Decision>> {
    Ok(sqlx::query_as::<_, Decision>(sqlx::AssertSqlSafe(format!(
        "SELECT {COLS} FROM decisions WHERE space_id = $1
         ORDER BY CASE status WHEN 'approved' THEN 0 WHEN 'proposed' THEN 1 ELSE 2 END, created_at DESC"
    )))
    .bind(space_id)
    .fetch_all(conn)
    .await?)
}

/// Approved decisions that override part of a document.
pub async fn overriding(conn: &mut PgConnection, doc_id: &str) -> AppResult<Vec<Decision>> {
    Ok(sqlx::query_as::<_, Decision>(sqlx::AssertSqlSafe(format!(
        "SELECT {COLS} FROM decisions WHERE status = 'approved'
           AND contradicts @> jsonb_build_array(jsonb_build_object('doc', $1::text))
         ORDER BY decided_at"
    )))
    .bind(doc_id)
    .fetch_all(conn)
    .await?)
}

/// What a caller sends to point at a section: a document by path or id.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SectionRef {
    pub doc: String,
    #[serde(default)]
    pub anchor: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewDecision {
    pub statement: String,
    pub reason: String,
    #[serde(default)]
    pub contradicts: Vec<SectionRef>,
    #[serde(default)]
    pub applies_to: Option<Vec<String>>,
    #[serde(default)]
    pub evidence: Vec<Evidence>,
    /// An approved decision this one replaces (on approval).
    #[serde(default)]
    pub supersedes: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionPatch {
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
}

fn text(field: &str, value: &str, max: usize) -> AppResult<String> {
    let v = value.trim();
    if v.is_empty() || v.chars().count() > max {
        return Err(AppError::invalid(
            field,
            format!("write it in up to {max} characters"),
        ));
    }
    Ok(v.to_string())
}

/// Resolve section references to documents in this space, checking that
/// the headings exist.
async fn sections(
    conn: &mut PgConnection,
    space_id: &str,
    refs: &[SectionRef],
) -> AppResult<Vec<Section>> {
    if refs.len() > MAX_LINKS {
        return Err(AppError::invalid(
            "contradicts",
            format!("at most {MAX_LINKS} sections"),
        ));
    }
    let mut out = Vec::new();
    for (i, r) in refs.iter().enumerate() {
        let field = format!("contradicts[{i}]");
        let row: Option<(String, String, String)> = sqlx::query_as(
            "SELECT id, path, content FROM documents
             WHERE space_id = $1 AND deleted_at IS NULL AND (id = $2 OR path = $3)",
        )
        .bind(space_id)
        .bind(&r.doc)
        .bind(r.doc.trim_start_matches('/').to_lowercase())
        .fetch_optional(&mut *conn)
        .await?;
        let (id, path, content) = row.ok_or_else(|| {
            AppError::invalid(
                field.clone(),
                format!("there's no document `{}` here", r.doc),
            )
        })?;
        let anchor = r
            .anchor
            .as_deref()
            .map(|a| a.trim_start_matches('#').to_string())
            .filter(|a| !a.is_empty());
        if let Some(a) = &anchor
            && !documents::headings(&content).iter().any(|h| &h.anchor == a)
        {
            return Err(AppError::invalid(
                field,
                format!("`{path}` has no heading `#{a}`"),
            ));
        }
        out.push(Section {
            doc: id,
            path,
            anchor,
        });
    }
    Ok(out)
}

fn evidence(list: Vec<Evidence>) -> AppResult<Vec<Evidence>> {
    if list.len() > MAX_LINKS {
        return Err(AppError::invalid(
            "evidence",
            format!("at most {MAX_LINKS} links"),
        ));
    }
    for (i, e) in list.iter().enumerate() {
        if !(e.url.starts_with("https://") || e.url.starts_with("http://")) || e.url.len() > 2000 {
            return Err(AppError::invalid(
                format!("evidence[{i}].url"),
                "give a link starting https://",
            ));
        }
    }
    Ok(list)
}

/// Record a decision: approved if you make it, proposed if a key does.
pub async fn create(
    conn: &mut PgConnection,
    space_id: &str,
    b: NewDecision,
    by: &str,
) -> AppResult<Decision> {
    let statement = text("statement", &b.statement, MAX_STATEMENT_CHARS)?;
    let reason = text("reason", &b.reason, MAX_REASON_CHARS)?;
    let contradicts = sections(conn, space_id, &b.contradicts).await?;
    let applies_to = check_targets(b.applies_to)?;
    let evidence = evidence(b.evidence)?;
    if let Some(old) = &b.supersedes {
        let o = get(conn, old)
            .await
            .map_err(|_| AppError::invalid("supersedes", "no such decision"))?;
        if o.space_id != space_id || o.status != "approved" {
            return Err(AppError::invalid(
                "supersedes",
                "only an approved decision in this space can be superseded",
            ));
        }
    }
    let id = format!("dec_{}", ulid::Ulid::generate());
    sqlx::query(
        "INSERT INTO decisions (id, space_id, statement, reason, contradicts, applies_to, evidence, status,
                                supersedes, proposed_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'proposed', $8, $9)",
    )
    .bind(&id)
    .bind(space_id)
    .bind(&statement)
    .bind(&reason)
    .bind(Json(&contradicts))
    .bind(&applies_to)
    .bind(Json(&evidence))
    .bind(&b.supersedes)
    .bind(by)
    .execute(&mut *conn)
    .await?;
    if by_you(by) {
        return approve(conn, &id, by).await;
    }
    crate::index::queue_space(conn, space_id).await?;
    get(conn, &id).await
}

/// Change a decision. You can change one that's proposed or approved; a key
/// only the proposals it made.
pub async fn update(
    conn: &mut PgConnection,
    id: &str,
    p: DecisionPatch,
    by: &str,
) -> AppResult<Decision> {
    let d = get(conn, id).await?;
    if !matches!(d.status.as_str(), "proposed" | "approved") {
        return Err(AppError::State {
            code: "closed",
            message: format!("this decision is {}; record a new one instead", d.status),
        });
    }
    if !by_you(by) && !(d.status == "proposed" && d.proposed_by == by) {
        return Err(AppError::Forbidden(
            "only your own proposed decisions can be changed with a key",
        ));
    }
    let statement = match &p.statement {
        Some(s) => text("statement", s, MAX_STATEMENT_CHARS)?,
        None => d.statement.clone(),
    };
    let reason = match &p.reason {
        Some(s) => text("reason", s, MAX_REASON_CHARS)?,
        None => d.reason.clone(),
    };
    let contradicts = match &p.contradicts {
        Some(r) => sections(conn, &d.space_id, r).await?,
        None => d.contradicts.0.clone(),
    };
    let applies_to = match p.applies_to {
        Some(a) => check_targets(Some(a))?,
        None => d.applies_to.clone(),
    };
    let evidence = match p.evidence {
        Some(e) => evidence(e)?,
        None => d.evidence.0.clone(),
    };
    sqlx::query(
        "UPDATE decisions SET statement = $1, reason = $2, contradicts = $3, applies_to = $4, evidence = $5,
                updated_at = now()
         WHERE id = $6",
    )
    .bind(&statement)
    .bind(&reason)
    .bind(Json(&contradicts))
    .bind(&applies_to)
    .bind(Json(&evidence))
    .bind(id)
    .execute(&mut *conn)
    .await?;
    let d = get(conn, id).await?;
    crate::index::queue_space(conn, &d.space_id).await?;
    Ok(d)
}

fn waiting(d: &Decision) -> AppResult<()> {
    if d.status != "proposed" {
        return Err(AppError::State {
            code: "not_proposed",
            message: format!("this decision is {}, not waiting for approval", d.status),
        });
    }
    Ok(())
}

/// Approve a proposed decision; the one it supersedes is then outdated.
pub async fn approve(conn: &mut PgConnection, id: &str, by: &str) -> AppResult<Decision> {
    let d = get(conn, id).await?;
    waiting(&d)?;
    sqlx::query(
        "UPDATE decisions SET status = 'approved', decided_by = $1, decided_at = now(), updated_at = now() WHERE id = $2",
    )
    .bind(by)
    .bind(id)
    .execute(&mut *conn)
    .await?;
    if let Some(old) = &d.supersedes {
        sqlx::query(
            "UPDATE decisions SET status = 'superseded', superseded_by = $1, updated_at = now()
             WHERE id = $2 AND status = 'approved'",
        )
        .bind(id)
        .bind(old)
        .execute(&mut *conn)
        .await?;
    }
    let d = get(conn, id).await?;
    crate::index::queue_space(conn, &d.space_id).await?;
    Ok(d)
}

pub async fn reject(conn: &mut PgConnection, id: &str, by: &str) -> AppResult<Decision> {
    let d = get(conn, id).await?;
    waiting(&d)?;
    sqlx::query(
        "UPDATE decisions SET status = 'rejected', decided_by = $1, decided_at = now(), updated_at = now() WHERE id = $2",
    )
    .bind(by)
    .bind(id)
    .execute(&mut *conn)
    .await?;
    let d = get(conn, id).await?;
    crate::index::queue_space(conn, &d.space_id).await?;
    Ok(d)
}

/// The Decisions document: a space's decisions as markdown, generated.
pub fn markdown(space_name: &str, list: &[Decision]) -> String {
    let mut out = format!("# Decisions: {space_name}\n\n");
    if list.is_empty() {
        out.push_str("No decisions yet.\n");
        return out;
    }
    for (title, statuses) in [
        ("In force", &["approved"][..]),
        ("Waiting for approval", &["proposed"][..]),
        ("No longer in force", &["superseded", "rejected"][..]),
    ] {
        let group: Vec<&Decision> = list
            .iter()
            .filter(|d| statuses.contains(&d.status.as_str()))
            .collect();
        if group.is_empty() {
            continue;
        }
        out.push_str(&format!("## {title}\n\n"));
        for d in group {
            out.push_str(&format!("### {}\n\n{}\n\n", d.statement, d.reason));
            let when = d.decided_at.unwrap_or(d.created_at).format("%-d %B %Y");
            let mut meta = vec![format!("{} {when}", capital(&d.status))];
            if !d.applies_to.iter().any(|a| a == "all") {
                meta.push(format!("for {}", d.applies_to.join(", ")));
            }
            out.push_str(&format!("- {}\n", meta.join(", ")));
            for s in &d.contradicts.0 {
                let at = s
                    .anchor
                    .as_deref()
                    .map(|a| format!("#{a}"))
                    .unwrap_or_default();
                out.push_str(&format!("- Overrides `{}{at}`\n", s.path));
            }
            for e in &d.evidence.0 {
                let label = e
                    .note
                    .clone()
                    .or_else(|| e.app.clone())
                    .unwrap_or_else(|| "Link".into());
                out.push_str(&format!("- [{label}]({})\n", e.url));
            }
            out.push('\n');
        }
    }
    out
}

fn capital(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}
