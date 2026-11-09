//! Documents (spec section 3.2): markdown files with a small frontmatter,
//! full history and a trash.

use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgConnection;

use crate::error::{AppError, AppResult};

pub const MAX_CONTENT_BYTES: usize = 512 * 1024;
/// Saves by the same author within this window merge into one revision.
const COALESCE: TimeDelta = TimeDelta::minutes(5);
const KEEP_REVISIONS: i64 = 50;

// ---------- Pure rules ----------

/// What the frontmatter says (and the defaults).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meta {
    pub title: String,
    pub doc_type: String,
    pub visibility: String,
}

/// Read `title`, `type` and `visibility` from a `---` block at the top.
/// Only single-line `key: value` pairs; anything else is left alone. Without
/// a title, the first `# ` heading, then the file name.
pub fn meta(path: &str, content: &str) -> AppResult<Meta> {
    let mut title = None;
    let mut doc_type = None;
    let mut visibility = None;
    let mut body = content;
    if let Some(rest) = content
        .strip_prefix("---\n")
        .or_else(|| content.strip_prefix("---\r\n"))
        && let Some(end) = rest.find("\n---")
    {
        for line in rest[..end].lines() {
            let Some((k, v)) = line.split_once(':') else {
                continue;
            };
            let v = v.trim().trim_matches(|c| c == '"' || c == '\'').trim();
            if v.is_empty() {
                continue;
            }
            match k.trim() {
                "title" => title = Some(v.to_string()),
                "type" => doc_type = Some(v.to_string()),
                "visibility" => visibility = Some(v.to_string()),
                _ => {}
            }
        }
        body = &rest[end + 4..];
    }
    let visibility = visibility.unwrap_or_else(|| "internal".into());
    if !matches!(visibility.as_str(), "internal" | "client" | "secret") {
        return Err(AppError::invalid(
            "content",
            format!("visibility is `{visibility}`; use internal, client or secret"),
        ));
    }
    let title = title
        .or_else(|| {
            body.lines()
                .find_map(|l| l.strip_prefix("# "))
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
        })
        .unwrap_or_else(|| stem_title(path));
    Ok(Meta {
        title: title.chars().take(200).collect(),
        doc_type: doc_type
            .unwrap_or_else(|| "note".into())
            .chars()
            .take(50)
            .collect(),
        visibility,
    })
}

fn stem_title(path: &str) -> String {
    let stem = path
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .trim_end_matches(".md");
    let words = stem.replace(['-', '_'], " ");
    let mut c = words.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => "Untitled".into(),
    }
}

/// A document path: lowercase segments of letters, digits, `-` and `_`,
/// joined by `/`, ending `.md`. At most 200 characters.
pub fn check_path(path: &str) -> AppResult<String> {
    let path = path.trim().trim_start_matches('/').to_ascii_lowercase();
    let bad = || {
        AppError::invalid(
            "path",
            "use lowercase letters, digits, `-` and `_`, folders with `/`, ending `.md`, e.g. `research/interviews.md`",
        )
    };
    if path.len() > 200 || !path.ends_with(".md") {
        return Err(bad());
    }
    let ok = path.trim_end_matches(".md").split('/').all(|seg| {
        !seg.is_empty()
            && seg
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
    });
    if !ok {
        return Err(bad());
    }
    Ok(path)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Heading {
    pub level: u8,
    pub text: String,
    /// Stable while the heading's text is: rules and decisions point here.
    pub anchor: String,
}

/// The document's headings with their anchors, ignoring fenced code.
pub fn headings(content: &str) -> Vec<Heading> {
    let mut out: Vec<Heading> = Vec::new();
    let mut fenced = false;
    for line in content.lines() {
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let level = t.chars().take_while(|c| *c == '#').count();
        if !(1..=6).contains(&level) || !t[level..].starts_with(' ') {
            continue;
        }
        let text = t[level..].trim().trim_end_matches('#').trim().to_string();
        let base = slug(&text);
        if base.is_empty() {
            continue;
        }
        let mut anchor = base.clone();
        let mut n = 2;
        while out.iter().any(|h| h.anchor == anchor) {
            anchor = format!("{base}-{n}");
            n += 1;
        }
        out.push(Heading {
            level: level as u8,
            text,
            anchor,
        });
    }
    out
}

fn slug(text: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for ch in text.chars().flat_map(char::to_lowercase) {
        if ch.is_alphanumeric() {
            out.push(ch);
            dash = false;
        } else if (ch.is_whitespace() || ch == '-' || ch == '_') && !out.is_empty() && !dash {
            out.push('-');
            dash = true;
        }
    }
    out.trim_end_matches('-').to_string()
}

// ---------- Storage ----------

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Document {
    pub id: String,
    pub space_id: String,
    pub path: String,
    pub title: String,
    #[serde(rename = "type")]
    pub doc_type: String,
    pub visibility: String,
    pub content: String,
    pub version: i32,
    pub created_by: String,
    pub updated_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deleted_by: Option<String>,
}

impl Document {
    /// The document as the API returns it: with its headings.
    pub fn json(&self) -> Value {
        let mut v = json!(self);
        v["headings"] = json!(headings(&self.content));
        v
    }
}

/// A document in a list: everything but the content.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Summary {
    pub id: String,
    pub path: String,
    pub title: String,
    #[serde(rename = "type")]
    pub doc_type: String,
    pub visibility: String,
    pub version: i32,
    pub updated_by: String,
    pub updated_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<DateTime<Utc>>,
}

const COLS: &str = "id, space_id, path, title, doc_type, visibility, content, version, created_by, \
                    updated_by, created_at, updated_at, deleted_at, deleted_by";

fn new_id() -> String {
    format!("doc_{}", ulid::Ulid::generate())
}

fn check_content(content: &str) -> AppResult<()> {
    if content.len() > MAX_CONTENT_BYTES {
        return Err(AppError::invalid(
            "content",
            format!("a document is at most {} KB", MAX_CONTENT_BYTES / 1024),
        ));
    }
    Ok(())
}

async fn row(conn: &mut PgConnection, id: &str) -> AppResult<Document> {
    sqlx::query_as::<_, Document>(sqlx::AssertSqlSafe(format!(
        "SELECT {COLS} FROM documents WHERE id = $1"
    )))
    .bind(id)
    .fetch_optional(conn)
    .await?
    .ok_or(AppError::NotFound("document"))
}

/// A live document.
pub async fn get(conn: &mut PgConnection, id: &str) -> AppResult<Document> {
    let d = row(conn, id).await?;
    if d.deleted_at.is_some() {
        return Err(AppError::NotFound("document"));
    }
    Ok(d)
}

/// A space's live documents (or its trash), by path.
pub async fn list(conn: &mut PgConnection, space_id: &str, trash: bool) -> AppResult<Vec<Summary>> {
    Ok(sqlx::query_as::<_, Summary>(
        "SELECT id, path, title, doc_type, visibility, version, updated_by, updated_at, deleted_at
         FROM documents WHERE space_id = $1 AND (deleted_at IS NOT NULL) = $2
         ORDER BY CASE WHEN $2 THEN deleted_at END DESC, path",
    )
    .bind(space_id)
    .bind(trash)
    .fetch_all(conn)
    .await?)
}

/// Create a document. `by` is `user` or a key's name.
pub async fn create(
    conn: &mut PgConnection,
    space_id: &str,
    path: &str,
    content: &str,
    by: &str,
) -> AppResult<Document> {
    let path = check_path(path)?;
    check_content(content)?;
    let m = meta(&path, content)?;
    let id = new_id();
    let inserted = sqlx::query(
        "INSERT INTO documents (id, space_id, path, title, doc_type, visibility, content, created_by, updated_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8) ON CONFLICT DO NOTHING",
    )
    .bind(&id)
    .bind(space_id)
    .bind(&path)
    .bind(&m.title)
    .bind(&m.doc_type)
    .bind(&m.visibility)
    .bind(content)
    .bind(by)
    .execute(&mut *conn)
    .await?;
    if inserted.rows_affected() == 0 {
        return Err(AppError::invalid(
            "path",
            format!("there's already a document at `{path}` here"),
        ));
    }
    record_revision(conn, &id, 1, &m.title, content, by).await?;
    get(conn, &id).await
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Patch {
    pub version: i32,
    #[serde(default)]
    pub content: Option<String>,
    /// Rename (move) the document.
    #[serde(default)]
    pub path: Option<String>,
}

/// Update a document's content or path. Needs the current version.
pub async fn update(
    conn: &mut PgConnection,
    id: &str,
    patch: Patch,
    by: &str,
) -> AppResult<Document> {
    let current = get(conn, id).await?;
    if patch.version != current.version {
        return Err(AppError::Conflict {
            current: current.json(),
        });
    }
    let path = match &patch.path {
        Some(p) => check_path(p)?,
        None => current.path.clone(),
    };
    let content = patch.content.unwrap_or_else(|| current.content.clone());
    check_content(&content)?;
    if path == current.path && content == current.content {
        return Ok(current);
    }
    let m = meta(&path, &content)?;
    let version = current.version + 1;
    let updated = sqlx::query(
        "UPDATE documents SET path = $1, title = $2, doc_type = $3, visibility = $4, content = $5,
                version = $6, updated_by = $7, updated_at = now()
         WHERE id = $8 AND version = $9 AND deleted_at IS NULL",
    )
    .bind(&path)
    .bind(&m.title)
    .bind(&m.doc_type)
    .bind(&m.visibility)
    .bind(&content)
    .bind(version)
    .bind(by)
    .bind(id)
    .bind(current.version)
    .execute(&mut *conn)
    .await;
    match updated {
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            return Err(AppError::invalid(
                "path",
                format!("there's already a document at `{path}` here"),
            ));
        }
        Err(e) => return Err(e.into()),
        Ok(r) if r.rows_affected() == 0 => {
            return Err(AppError::Conflict {
                current: get(conn, id).await?.json(),
            });
        }
        Ok(_) => {}
    }
    if content != current.content || m.title != current.title {
        record_revision(conn, id, version, &m.title, &content, by).await?;
    }
    get(conn, id).await
}

pub async fn delete(conn: &mut PgConnection, id: &str, by: &str) -> AppResult<Document> {
    get(conn, id).await?;
    sqlx::query("UPDATE documents SET deleted_at = now(), deleted_by = $1, updated_at = now() WHERE id = $2")
        .bind(by)
        .bind(id)
        .execute(&mut *conn)
        .await?;
    row(conn, id).await
}

pub async fn undelete(conn: &mut PgConnection, id: &str) -> AppResult<Document> {
    let d = row(conn, id).await?;
    if d.deleted_at.is_none() {
        return Err(AppError::State {
            code: "not_deleted",
            message: "this document isn't in the trash".into(),
        });
    }
    let restored = sqlx::query(
        "UPDATE documents SET deleted_at = NULL, deleted_by = NULL, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .execute(&mut *conn)
    .await;
    if let Err(sqlx::Error::Database(e)) = &restored
        && e.is_unique_violation()
    {
        return Err(AppError::State {
            code: "path_taken",
            message: format!("there's a document at `{}` now; rename it first", d.path),
        });
    }
    restored?;
    get(conn, id).await
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Revision {
    pub id: i64,
    pub version: i32,
    pub title: String,
    pub content: String,
    pub changed_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

async fn record_revision(
    conn: &mut PgConnection,
    id: &str,
    version: i32,
    title: &str,
    content: &str,
    by: &str,
) -> AppResult<()> {
    let newest: Option<(i64, String, DateTime<Utc>)> = sqlx::query_as(
        "SELECT id, changed_by, updated_at FROM document_revisions WHERE document_id = $1 ORDER BY id DESC LIMIT 1",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some((rev, changed_by, at)) = newest
        && changed_by == by
        && at > Utc::now() - COALESCE
    {
        sqlx::query("UPDATE document_revisions SET version = $1, title = $2, content = $3, updated_at = now() WHERE id = $4")
            .bind(version)
            .bind(title)
            .bind(content)
            .bind(rev)
            .execute(&mut *conn)
            .await?;
        return Ok(());
    }
    sqlx::query("INSERT INTO document_revisions (document_id, version, title, content, changed_by) VALUES ($1, $2, $3, $4, $5)")
        .bind(id)
        .bind(version)
        .bind(title)
        .bind(content)
        .bind(by)
        .execute(&mut *conn)
        .await?;
    sqlx::query(
        "DELETE FROM document_revisions WHERE document_id = $1 AND id NOT IN
           (SELECT id FROM document_revisions WHERE document_id = $1 ORDER BY id DESC LIMIT $2)",
    )
    .bind(id)
    .bind(KEEP_REVISIONS)
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn revisions(conn: &mut PgConnection, id: &str) -> AppResult<Vec<Revision>> {
    row(conn, id).await?;
    Ok(sqlx::query_as::<_, Revision>(
        "SELECT id, version, title, content, changed_by, created_at, updated_at
         FROM document_revisions WHERE document_id = $1 ORDER BY id DESC",
    )
    .bind(id)
    .fetch_all(conn)
    .await?)
}

/// Put a document back to a revision: an ordinary versioned update.
pub async fn restore(
    conn: &mut PgConnection,
    id: &str,
    revision: i64,
    version: i32,
    by: &str,
) -> AppResult<Document> {
    let content: Option<(String,)> =
        sqlx::query_as("SELECT content FROM document_revisions WHERE id = $1 AND document_id = $2")
            .bind(revision)
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?;
    let (content,) = content.ok_or(AppError::NotFound("revision"))?;
    update(
        conn,
        id,
        Patch {
            version,
            content: Some(content),
            path: None,
        },
        by,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_and_defaults() {
        let m = meta(
            "brief.md",
            "---\ntitle: The brief\ntype: brief\nvisibility: client\n---\n\n# Ignored\n",
        )
        .unwrap();
        assert_eq!(
            m,
            Meta {
                title: "The brief".into(),
                doc_type: "brief".into(),
                visibility: "client".into()
            }
        );
        let m = meta("notes/kick-off_call.md", "Some text\n\n# Kick-off\n").unwrap();
        assert_eq!(
            (m.title.as_str(), m.doc_type.as_str(), m.visibility.as_str()),
            ("Kick-off", "note", "internal")
        );
        let m = meta("notes/kick-off_call.md", "No headings at all").unwrap();
        assert_eq!(m.title, "Kick off call");
        let m = meta("a.md", "---\ntitle: \"Quoted\"\nunknown: x\n---\nbody").unwrap();
        assert_eq!(m.title, "Quoted");
        assert!(meta("a.md", "---\nvisibility: public\n---\n").is_err());
    }

    #[test]
    fn paths() {
        assert_eq!(
            check_path("/Research/Interviews.md").unwrap(),
            "research/interviews.md"
        );
        assert_eq!(check_path("writing-rules.md").unwrap(), "writing-rules.md");
        for bad in [
            "brief", "a b.md", "../x.md", "a//b.md", ".md", "x.MD.txt", "über.md",
        ] {
            assert!(check_path(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn anchors_are_stable_and_unique() {
        let h = headings(
            "# Brief\n## Delivery areas\ntext\n```\n# not a heading\n```\n## Delivery areas\n### Q&A: what's next?\n#nope\n",
        );
        let anchors: Vec<&str> = h.iter().map(|h| h.anchor.as_str()).collect();
        assert_eq!(
            anchors,
            [
                "brief",
                "delivery-areas",
                "delivery-areas-2",
                "qa-whats-next"
            ]
        );
        assert_eq!(h[3].level, 3);
        // Same heading text, same anchor, whatever else changes.
        let again = headings("Intro paragraph\n\n## Delivery areas\nNew text");
        assert_eq!(again[0].anchor, "delivery-areas");
    }
}
