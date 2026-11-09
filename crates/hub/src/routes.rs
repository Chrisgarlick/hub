//! The REST API: spaces and their documents (spec section 3).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Extension, Json, Router, middleware};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgConnection;

use crate::app::AppState;
use crate::auth::{self, Caller};
use crate::documents::{self, Patch};
use crate::error::{AppError, AppResult};
use crate::{passkeys, spaces};

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
    Ok(Json(documents::get(&mut conn, &id).await?.json()))
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
