//! Spaces (spec section 3.1): one per client in Clients, kept in step with
//! its event feed, plus the studio space for what applies to every client.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgConnection, PgPool};

use crate::clients::{ClientInfo, Clients, Event};
use crate::documents;
use crate::error::{AppError, AppResult};

pub const STUDIO: &str = "_studio";
const POLL: Duration = Duration::from_secs(30);
/// A full reconcile against Clients' list, in case an event was missed.
const RECONCILE_EVERY: u32 = 20; // polls, so every 10 minutes

/// Starter documents for a new client's space: (path, content).
const CLIENT_TEMPLATES: [(&str, &str); 5] = [
    ("brief.md", include_str!("../templates/brief.md")),
    ("audience.md", include_str!("../templates/audience.md")),
    ("voice.md", include_str!("../templates/voice.md")),
    (
        "constraints.md",
        include_str!("../templates/constraints.md"),
    ),
    (
        "writing-rules.md",
        include_str!("../templates/writing-rules.md"),
    ),
];
const STUDIO_TEMPLATES: [(&str, &str); 2] = [
    (
        "house-style.md",
        include_str!("../templates/house-style.md"),
    ),
    (
        "writing-rules.md",
        include_str!("../templates/writing-rules.md"),
    ),
];

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Space {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Listed {
    #[sqlx(flatten)]
    #[serde(flatten)]
    pub space: Space,
    pub documents: i64,
}

const COLS: &str = "s.id, s.slug, s.name, s.kind, s.archived_at, s.created_at, s.updated_at";

/// Live spaces, the studio first, then clients by name.
pub async fn list(conn: &mut PgConnection) -> AppResult<Vec<Listed>> {
    Ok(sqlx::query_as::<_, Listed>(sqlx::AssertSqlSafe(format!(
        "SELECT {COLS}, (SELECT count(*) FROM documents d WHERE d.space_id = s.id AND d.deleted_at IS NULL) AS documents
         FROM spaces s WHERE s.archived_at IS NULL ORDER BY s.kind = 'client', lower(s.name)"
    )))
    .fetch_all(conn)
    .await?)
}

/// A live space by slug.
pub async fn by_slug(conn: &mut PgConnection, slug: &str) -> AppResult<Space> {
    sqlx::query_as::<_, Space>(sqlx::AssertSqlSafe(format!(
        "SELECT {COLS} FROM spaces s WHERE s.slug = $1 AND s.archived_at IS NULL"
    )))
    .bind(slug)
    .fetch_optional(conn)
    .await?
    .ok_or(AppError::NotFound("space"))
}

/// Make sure a space exists for this client (or the studio), seeding the
/// starter documents the first time. Keeps its slug and name current.
/// Returns whether it was created.
pub async fn ensure(
    conn: &mut PgConnection,
    id: &str,
    slug: &str,
    name: &str,
    kind: &str,
) -> AppResult<bool> {
    let created: Option<(bool,)> = sqlx::query_as(
        "INSERT INTO spaces (id, slug, name, kind) VALUES ($1, $2, $3, $4)
         ON CONFLICT (id) DO UPDATE SET slug = EXCLUDED.slug, name = EXCLUDED.name, updated_at = now()
           WHERE spaces.archived_at IS NULL AND (spaces.slug, spaces.name) IS DISTINCT FROM (EXCLUDED.slug, EXCLUDED.name)
         RETURNING (xmax = 0)",
    )
    .bind(id)
    .bind(slug)
    .bind(name)
    .bind(kind)
    .fetch_optional(&mut *conn)
    .await?;
    let created = created.is_some_and(|(c,)| c);
    if created {
        let templates: &[(&str, &str)] = if kind == "studio" {
            &STUDIO_TEMPLATES
        } else {
            &CLIENT_TEMPLATES
        };
        for (path, content) in templates {
            documents::create(conn, id, path, content, "hub").await?;
        }
        tracing::info!(space = slug, "space created");
    }
    Ok(created)
}

/// Archive a deleted client's space. Its documents stay (spec 3.1).
pub async fn archive(conn: &mut PgConnection, id: &str) -> AppResult<bool> {
    let r = sqlx::query("UPDATE spaces SET archived_at = now(), updated_at = now() WHERE id = $1 AND archived_at IS NULL AND kind = 'client'")
        .bind(id)
        .execute(conn)
        .await?;
    Ok(r.rows_affected() > 0)
}

pub async fn ensure_studio(pool: &PgPool) -> AppResult<()> {
    let mut conn = pool.acquire().await?;
    ensure(&mut conn, STUDIO, STUDIO, "Studio", "studio").await?;
    Ok(())
}

/// Make spaces match Clients' list of active clients: archive spaces whose
/// client is gone (a missed `client.deleted`, which also frees its slug),
/// then create or update one for every client. Returns how many were made.
pub async fn reconcile(pool: &PgPool, clients: &[ClientInfo]) -> AppResult<usize> {
    // An empty list is more likely a fault than every client deleted.
    if !clients.is_empty() {
        let ids: Vec<&str> = clients.iter().map(|c| c.id.as_str()).collect();
        let gone = sqlx::query(
            "UPDATE spaces SET archived_at = now(), updated_at = now()
             WHERE kind = 'client' AND archived_at IS NULL AND NOT (id = ANY($1))",
        )
        .bind(&ids)
        .execute(pool)
        .await?;
        if gone.rows_affected() > 0 {
            tracing::info!(
                archived = gone.rows_affected(),
                "spaces archived by reconcile"
            );
        }
    }
    let mut made = 0;
    for c in clients {
        let mut tx = pool.begin().await?;
        if ensure(&mut tx, &c.id, &c.slug, &c.name, "client").await? {
            made += 1;
        }
        tx.commit().await?;
    }
    Ok(made)
}

/// Apply one event from Clients.
pub async fn apply(pool: &PgPool, clients: &Clients, e: &Event) -> AppResult<()> {
    match e.kind.as_str() {
        "client.created" => {
            if let (Some(id), Some(slug)) = (e.client_id.as_deref(), e.slug()) {
                let name = e.payload["name"].as_str().unwrap_or(slug);
                let mut tx = pool.begin().await?;
                ensure(&mut tx, id, slug, name, "client").await?;
                tx.commit().await?;
            }
        }
        "client.updated" => {
            if let (Some(id), Some(slug)) = (e.client_id.as_deref(), e.slug()) {
                let name = e.payload["name"].as_str().unwrap_or(slug);
                let mut tx = pool.begin().await?;
                ensure(&mut tx, id, slug, name, "client").await?;
                tx.commit().await?;
            }
        }
        "client.deleted" => {
            if let Some(id) = e.client_id.as_deref() {
                let mut conn = pool.acquire().await?;
                if archive(&mut conn, id).await? {
                    tracing::info!(client = e.slug().unwrap_or(id), "space archived");
                }
            }
        }
        "key.revoked" => {
            if let Some(key) = e.payload["key"].as_str() {
                clients.forget_key(key);
            }
        }
        _ => {}
    }
    Ok(())
}

async fn cursor(pool: &PgPool) -> AppResult<i64> {
    let v: Option<(String,)> =
        sqlx::query_as("SELECT value FROM sync_state WHERE key = 'clients_cursor'")
            .fetch_optional(pool)
            .await?;
    Ok(v.and_then(|(v,)| v.parse().ok()).unwrap_or(0))
}

async fn set_cursor(pool: &PgPool, id: i64) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO sync_state (key, value) VALUES ('clients_cursor', $1)
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = now()",
    )
    .bind(id.to_string())
    .execute(pool)
    .await?;
    Ok(())
}

/// One pass: read new events and apply them, in order. Returns how many.
pub async fn sync_events(pool: &PgPool, clients: &Clients) -> AppResult<usize> {
    let mut after = cursor(pool).await?;
    let mut applied = 0;
    loop {
        let events = clients.events(after).await?;
        if events.is_empty() {
            break;
        }
        for e in &events {
            apply(pool, clients, e).await?;
            after = e.id;
            set_cursor(pool, after).await?;
            applied += 1;
        }
        if events.len() < 100 {
            break;
        }
    }
    Ok(applied)
}

/// Keep spaces in step with Clients, forever: reconcile now, then poll the
/// event feed every 30 seconds and reconcile every 10 minutes.
pub fn spawn_sync(pool: PgPool, clients: Clients) {
    tokio::spawn(async move {
        let mut n: u32 = 0;
        loop {
            if n.is_multiple_of(RECONCILE_EVERY) {
                match clients.clients().await {
                    Ok(list) => match reconcile(&pool, &list).await {
                        Ok(made) if made > 0 => tracing::info!(made, "spaces reconciled"),
                        Ok(_) => {}
                        Err(e) => tracing::warn!(error = %e, "reconcile failed"),
                    },
                    Err(e) => tracing::warn!(error = %e, "Clients list unavailable"),
                }
            }
            if let Err(e) = sync_events(&pool, &clients).await {
                tracing::warn!(error = %e, "Clients events unavailable");
            }
            n = n.wrapping_add(1);
            tokio::time::sleep(POLL).await;
        }
    });
}
