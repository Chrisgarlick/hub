//! The library (spec section 6A): reusable files the apps work from, by
//! kind (named by the app: `design-style`), name and scope (the studio, or
//! one client's own). Hub keeps and serves them; it doesn't interpret the
//! body. Clients pin studio items they use.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::PgConnection;

use crate::error::{AppError, AppResult};
use crate::index;
use crate::spaces::STUDIO;

/// A Refero DESIGN.md is 10–30 KB.
pub const MAX_BODY_BYTES: usize = 200 << 10;
const MAX_TAGS: usize = 20;
const KEEP_REVISIONS: i64 = 50;

/// `design-style`, `typeset-outline`: lowercase words joined by `-`.
pub fn check_kind(kind: &str) -> AppResult<()> {
    if (1..=40).contains(&kind.len())
        && kind
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !kind.starts_with('-')
        && !kind.ends_with('-')
    {
        Ok(())
    } else {
        Err(AppError::invalid(
            "kind",
            format!("`{kind}`: lowercase words joined by -, e.g. design-style"),
        ))
    }
}

/// `elevenlabs`, `linear-dark` (as Design names its styles).
pub fn check_name(name: &str) -> AppResult<()> {
    if (1..=60).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
    {
        Ok(())
    } else {
        Err(AppError::invalid(
            "name",
            format!("`{name}`: use lowercase letters, digits and -, e.g. elevenlabs"),
        ))
    }
}

fn title_of(body: &str) -> Option<String> {
    body.lines()
        .find_map(|l| l.strip_prefix("# "))
        .map(|t| t.trim().chars().take(200).collect())
        .filter(|t: &String| !t.is_empty())
}

fn summary_of(body: &str) -> Option<String> {
    body.lines()
        .find_map(|l| l.strip_prefix("> "))
        .map(|t| t.trim().chars().take(400).collect())
        .filter(|t: &String| !t.is_empty())
}

fn clean_tags(tags: &[String]) -> AppResult<Vec<String>> {
    if tags.len() > MAX_TAGS {
        return Err(AppError::invalid("tags", format!("at most {MAX_TAGS}")));
    }
    let mut out: Vec<String> = Vec::new();
    for t in tags {
        let t = t.trim().to_lowercase();
        if t.is_empty() {
            continue;
        }
        if t.chars().count() > 40 {
            return Err(AppError::invalid(
                "tags",
                format!("`{t}` is longer than 40 characters"),
            ));
        }
        if !out.contains(&t) {
            out.push(t);
        }
    }
    Ok(out)
}

/// What an item's put is checked against.
pub fn check_body(body: &str) -> AppResult<&str> {
    let b = body.trim();
    if b.is_empty() {
        return Err(AppError::invalid("body", "paste the file's markdown"));
    }
    if b.len() > MAX_BODY_BYTES {
        return Err(AppError::invalid(
            "body",
            format!("that's {} KB; an item is at most 200 KB", b.len() >> 10),
        ));
    }
    Ok(b)
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Item {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub space_id: String,
    /// The scope's slug: `_studio`, or the client's.
    pub scope: String,
    pub title: String,
    pub summary: Option<String>,
    pub tags: Vec<String>,
    pub source: Option<String>,
    pub body: String,
    pub version: i32,
    pub created_by: String,
    pub updated_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Item {
    /// Without the body, for lists.
    pub fn summary_json(&self) -> Value {
        let mut v = json!(self);
        if let Some(o) = v.as_object_mut() {
            o.remove("body");
        }
        v
    }
}

const COLS: &str = "i.id, i.kind, i.name, i.space_id, s.slug AS scope, i.title, i.summary, i.tags, i.source, i.body, \
                    i.version, i.created_by, i.updated_by, i.created_at, i.updated_at";

pub async fn get(conn: &mut PgConnection, id: &str) -> AppResult<Item> {
    sqlx::query_as::<_, Item>(sqlx::AssertSqlSafe(format!(
        "SELECT {COLS} FROM library_items i JOIN spaces s ON s.id = i.space_id WHERE i.id = $1 AND i.deleted_at IS NULL"
    )))
    .bind(id)
    .fetch_optional(conn)
    .await?
    .ok_or(AppError::NotFound("library item"))
}

/// An item by kind, scope (space id) and name.
pub async fn find(
    conn: &mut PgConnection,
    kind: &str,
    space_id: &str,
    name: &str,
) -> AppResult<Option<Item>> {
    Ok(sqlx::query_as::<_, Item>(sqlx::AssertSqlSafe(format!(
        "SELECT {COLS} FROM library_items i JOIN spaces s ON s.id = i.space_id
         WHERE i.kind = $1 AND i.space_id = $2 AND i.name = $3 AND i.deleted_at IS NULL"
    )))
    .bind(kind)
    .bind(space_id)
    .bind(name)
    .fetch_optional(conn)
    .await?)
}

/// Items in these scopes (space ids), optionally of one kind, by kind and name.
pub async fn list(
    conn: &mut PgConnection,
    spaces: &[String],
    kind: Option<&str>,
) -> AppResult<Vec<Item>> {
    Ok(sqlx::query_as::<_, Item>(sqlx::AssertSqlSafe(format!(
        "SELECT {COLS} FROM library_items i JOIN spaces s ON s.id = i.space_id
         WHERE i.deleted_at IS NULL AND i.space_id = ANY($1) AND ($2::text IS NULL OR i.kind = $2)
         ORDER BY i.kind, i.space_id <> '{STUDIO}', i.name"
    )))
    .bind(spaces)
    .bind(kind)
    .fetch_all(conn)
    .await?)
}

/// What a put changes.
#[derive(Debug, Clone)]
pub struct Put<'a> {
    pub kind: &'a str,
    pub space_id: &'a str,
    pub name: &'a str,
    pub body: &'a str,
    /// None keeps the current tags (or none, for a new item).
    pub tags: Option<&'a [String]>,
    pub source: Option<&'a str>,
    /// For an existing item: the version this change was made against.
    pub version: Option<i32>,
}

/// Save an item, a new one or a new version (a no-op if nothing changed).
pub async fn put(conn: &mut PgConnection, p: Put<'_>, by: &str) -> AppResult<Item> {
    check_kind(p.kind)?;
    check_name(p.name)?;
    let body = check_body(p.body)?;
    let source = p
        .source
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(200).collect::<String>());
    let title = title_of(body).unwrap_or_else(|| p.name.to_string());
    let summary = summary_of(body);
    let current = find(conn, p.kind, p.space_id, p.name).await?;
    let item_id = match current {
        Some(cur) => {
            if let Some(v) = p.version
                && v != cur.version
            {
                return Err(AppError::Conflict {
                    current: cur.summary_json(),
                });
            }
            let tags = match p.tags {
                Some(t) => clean_tags(t)?,
                None => cur.tags.clone(),
            };
            let source = source.or(cur.source.clone());
            if cur.body == body && cur.tags == tags && cur.source == source {
                return Ok(cur);
            }
            sqlx::query(
                "UPDATE library_items SET body = $1, title = $2, summary = $3, tags = $4, source = $5,
                        version = version + 1, updated_by = $6, updated_at = now()
                 WHERE id = $7",
            )
            .bind(body)
            .bind(&title)
            .bind(&summary)
            .bind(&tags)
            .bind(&source)
            .bind(by)
            .bind(&cur.id)
            .execute(&mut *conn)
            .await?;
            cur.id
        }
        None => {
            let tags = clean_tags(p.tags.unwrap_or_default())?;
            let id = format!("lib_{}", ulid::Ulid::generate());
            sqlx::query(
                "INSERT INTO library_items (id, kind, name, space_id, title, summary, tags, source, body, created_by, updated_by)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10)",
            )
            .bind(&id)
            .bind(p.kind)
            .bind(p.name)
            .bind(p.space_id)
            .bind(&title)
            .bind(&summary)
            .bind(&tags)
            .bind(&source)
            .bind(body)
            .bind(by)
            .execute(&mut *conn)
            .await?;
            id
        }
    };
    let item = get(conn, &item_id).await?;
    sqlx::query(
        "INSERT INTO library_revisions (item_id, version, body, tags, source, changed_by) VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(&item.id)
    .bind(item.version)
    .bind(&item.body)
    .bind(&item.tags)
    .bind(&item.source)
    .bind(by)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "DELETE FROM library_revisions WHERE item_id = $1 AND id NOT IN
           (SELECT id FROM library_revisions WHERE item_id = $1 ORDER BY id DESC LIMIT $2)",
    )
    .bind(&item.id)
    .bind(KEEP_REVISIONS)
    .execute(&mut *conn)
    .await?;
    index::queue_library(conn, &item.id).await?;
    Ok(item)
}

pub async fn delete(conn: &mut PgConnection, id: &str) -> AppResult<()> {
    let r = sqlx::query(
        "UPDATE library_items SET deleted_at = now() WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(id)
    .execute(&mut *conn)
    .await?;
    if r.rows_affected() == 0 {
        return Err(AppError::NotFound("library item"));
    }
    sqlx::query("DELETE FROM library_pins WHERE item_id = $1")
        .bind(id)
        .execute(&mut *conn)
        .await?;
    index::queue_library(conn, id).await?;
    Ok(())
}

/// Bring a deleted item back, if its name is still free.
pub async fn undelete(conn: &mut PgConnection, id: &str) -> AppResult<()> {
    let r = sqlx::query(
        "UPDATE library_items SET deleted_at = NULL WHERE id = $1 AND deleted_at IS NOT NULL",
    )
    .bind(id)
    .execute(&mut *conn)
    .await;
    match r {
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => Err(AppError::State {
            code: "name_taken",
            message: "another item has that name now; rename one first".into(),
        }),
        Err(e) => Err(e.into()),
        Ok(_) => index::queue_library(conn, id).await,
    }
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Revision {
    pub id: i64,
    pub version: i32,
    pub body: String,
    pub tags: Vec<String>,
    pub source: Option<String>,
    pub changed_by: String,
    pub created_at: DateTime<Utc>,
}

pub async fn revisions(conn: &mut PgConnection, id: &str) -> AppResult<Vec<Revision>> {
    Ok(sqlx::query_as(
        "SELECT id, version, body, tags, source, changed_by, created_at FROM library_revisions
         WHERE item_id = $1 ORDER BY id DESC",
    )
    .bind(id)
    .fetch_all(conn)
    .await?)
}

// ---------- Pins ----------

/// A client uses a studio item. Returns whether it was newly pinned.
pub async fn pin(
    conn: &mut PgConnection,
    space_id: &str,
    item_id: &str,
    by: &str,
) -> AppResult<bool> {
    let item = get(conn, item_id).await?;
    if item.space_id != STUDIO || space_id == STUDIO {
        return Err(AppError::invalid("item", "pin a studio item to a client"));
    }
    let r = sqlx::query(
        "INSERT INTO library_pins (space_id, item_id, created_by) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
    )
    .bind(space_id)
    .bind(item_id)
    .bind(by)
    .execute(conn)
    .await?;
    Ok(r.rows_affected() == 1)
}

pub async fn unpin(conn: &mut PgConnection, space_id: &str, item_id: &str) -> AppResult<()> {
    sqlx::query("DELETE FROM library_pins WHERE space_id = $1 AND item_id = $2")
        .bind(space_id)
        .bind(item_id)
        .execute(conn)
        .await?;
    Ok(())
}

/// What a client uses: its own items and the studio items pinned to it.
pub async fn for_client(conn: &mut PgConnection, space_id: &str) -> AppResult<Vec<Item>> {
    Ok(sqlx::query_as::<_, Item>(sqlx::AssertSqlSafe(format!(
        "SELECT {COLS} FROM library_items i JOIN spaces s ON s.id = i.space_id
         WHERE i.deleted_at IS NULL
           AND (i.space_id = $1 OR i.id IN (SELECT item_id FROM library_pins WHERE space_id = $1))
         ORDER BY i.kind, i.name"
    )))
    .bind(space_id)
    .fetch_all(conn)
    .await?)
}

/// The clients a studio item is pinned to: (space id, slug, name).
pub async fn pinned_to(
    conn: &mut PgConnection,
    item_id: &str,
) -> AppResult<Vec<(String, String, String)>> {
    Ok(sqlx::query_as(
        "SELECT s.id, s.slug, s.name FROM library_pins p JOIN spaces s ON s.id = p.space_id
         WHERE p.item_id = $1 AND s.archived_at IS NULL ORDER BY s.name",
    )
    .bind(item_id)
    .fetch_all(conn)
    .await?)
}

/// Design's styles, as studio `design-style` items. Returns how many were
/// new, changed and unchanged.
pub async fn import_styles(
    pool: &sqlx::PgPool,
    json_text: &str,
) -> anyhow::Result<(usize, usize, usize)> {
    #[derive(serde::Deserialize)]
    struct Style {
        name: String,
        markdown: String,
        #[serde(default)]
        source: Option<String>,
    }
    let text = json_text.trim();
    // sqlite3 -json prints nothing at all for no rows.
    let styles: Vec<Style> = if text.is_empty() {
        vec![]
    } else {
        serde_json::from_str(text)?
    };
    crate::spaces::ensure_studio(pool).await?;
    let (mut made, mut changed, mut same) = (0, 0, 0);
    for st in &styles {
        let mut tx = pool.begin().await?;
        let before = find(&mut tx, "design-style", STUDIO, &st.name)
            .await?
            .map(|i| i.version);
        let change = Put {
            kind: "design-style",
            space_id: STUDIO,
            name: &st.name,
            body: &st.markdown,
            tags: None,
            source: st.source.as_deref(),
            version: None,
        };
        let item = put(&mut tx, change, "hub")
            .await
            .map_err(|e| anyhow::anyhow!("style `{}`: {e}", st.name))?;
        tx.commit().await?;
        match before {
            None => made += 1,
            Some(v) if v == item.version => same += 1,
            Some(_) => changed += 1,
        }
    }
    Ok((made, changed, same))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_titles_and_tags() {
        let md = "# Intercom — Style Reference\n> Warm cream editorial spread\n\n**Theme:** light";
        assert_eq!(title_of(md).as_deref(), Some("Intercom — Style Reference"));
        assert_eq!(
            summary_of(md).as_deref(),
            Some("Warm cream editorial spread")
        );
        assert!(check_name("elevenlabs").is_ok() && check_name("linear-dark").is_ok());
        assert!(
            check_name("Eleven Labs").is_err()
                && check_name("../x").is_err()
                && check_name("").is_err()
        );
        assert!(
            check_kind("design-style").is_ok()
                && check_kind("Design").is_err()
                && check_kind("x-").is_err()
        );
        assert_eq!(
            clean_tags(&[
                "Dark".into(),
                " dark ".into(),
                "".into(),
                "editorial".into()
            ])
            .unwrap(),
            ["dark", "editorial"]
        );
        assert!(check_body("  ").is_err());
        assert!(check_body(&"x".repeat(MAX_BODY_BYTES + 1)).is_err());
    }
}
