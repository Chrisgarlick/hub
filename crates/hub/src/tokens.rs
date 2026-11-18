//! Hub's own API tokens, for scripts that aren't Claude: a site base's build
//! downloading a client's published brand (`crate::brand_api`).
//!
//! Made on the server with `hub admin token create --name wp-base --scope
//! brand:read`, which prints the token once; only its hash is kept. Sent as
//! `Authorization: Bearer hub_…`. A token has one scope and opens only the
//! routes for it: `brand:read` is the `/v1/clients/{slug}/brand…` routes, and
//! nothing else (the API and the MCP refuse it, `crate::auth`). Every call is
//! logged, and each token is held to [`PER_MINUTE`] requests a minute.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Request, State};
use axum::http::header;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;

use crate::app::AppState;
use crate::auth::{hash, random_secret};
use crate::error::{AppError, AppResult};

/// What every Hub token starts with, so it's told apart from a Clients key.
pub const PREFIX: &str = "hub_";
/// The scopes a token can have.
pub const SCOPES: &[&str] = &[BRAND_READ];
/// Download a client's published brand and its files (`/v1`).
pub const BRAND_READ: &str = "brand:read";
/// Requests a minute, per token. A build makes a handful.
pub const PER_MINUTE: u32 = 120;

/// The token behind a `/v1` request.
#[derive(Debug, Clone)]
pub struct ApiToken {
    pub name: String,
    pub scope: String,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct TokenInfo {
    pub name: String,
    pub scope: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

/// A new token, returned once (only its hash is kept).
pub async fn create(pool: &PgPool, name: &str, scope: &str) -> AppResult<String> {
    if !SCOPES.contains(&scope) {
        return Err(AppError::invalid(
            "scope",
            format!("a token's scope is one of: {}", SCOPES.join(", ")),
        ));
    }
    let name = name.trim();
    if name.is_empty()
        || name.len() > 60
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err(AppError::invalid(
            "name",
            "give the token a name of up to 60 letters, digits, `-`, `_` or `.`, e.g. `wp-base`",
        ));
    }
    let token = format!("{PREFIX}{}", random_secret());
    let r = sqlx::query(
        "INSERT INTO api_tokens (name, token_hash, scope) VALUES ($1, $2, $3) ON CONFLICT (name) DO NOTHING",
    )
    .bind(name)
    .bind(hash(&token))
    .bind(scope)
    .execute(pool)
    .await?;
    if r.rows_affected() == 0 {
        return Err(AppError::invalid(
            "name",
            format!(
                "there's already a token called `{name}`: revoke it first, or pick another name"
            ),
        ));
    }
    Ok(token)
}

pub async fn list(pool: &PgPool) -> AppResult<Vec<TokenInfo>> {
    Ok(sqlx::query_as(
        "SELECT name, scope, created_at, last_used_at FROM api_tokens ORDER BY created_at",
    )
    .fetch_all(pool)
    .await?)
}

/// Revoke a token by name: it stops working at once.
pub async fn revoke(pool: &PgPool, name: &str) -> AppResult<()> {
    let r = sqlx::query("DELETE FROM api_tokens WHERE name = $1")
        .bind(name)
        .execute(pool)
        .await?;
    if r.rows_affected() == 0 {
        return Err(AppError::Missing(format!("no token called `{name}`")));
    }
    Ok(())
}

/// A live token, noting when it was used.
async fn lookup(pool: &PgPool, token: &str) -> AppResult<Option<ApiToken>> {
    if !token.starts_with(PREFIX) {
        return Ok(None);
    }
    let row: Option<(String, String)> = sqlx::query_as(
        "UPDATE api_tokens SET last_used_at = now() WHERE token_hash = $1 RETURNING name, scope",
    )
    .bind(hash(token))
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(name, scope)| ApiToken { name, scope }))
}

/// Requests per token in the current minute.
#[derive(Clone, Default)]
pub struct Limiter(Arc<Mutex<HashMap<String, (Instant, u32)>>>);

impl Limiter {
    /// Count a request; `false` once the token has had its minute's worth.
    pub fn allow(&self, name: &str) -> bool {
        let mut map = self.0.lock().unwrap();
        let now = Instant::now();
        let window = Duration::from_secs(60);
        map.retain(|_, (start, _)| now.duration_since(*start) < window);
        let (_, n) = map.entry(name.to_string()).or_insert((now, 0));
        *n += 1;
        *n <= PER_MINUTE
    }
}

/// `/v1`: a Hub token with `brand:read`, within its rate; every call logged.
pub async fn require_brand_read(
    State(st): State<AppState>,
    mut req: Request,
    next: Next,
) -> Response {
    let bearer = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim);
    let token = match bearer {
        None => return AppError::Unauthorized.into_response(),
        Some(t) if !t.starts_with(PREFIX) => {
            return AppError::Forbidden(
                "these routes take a Hub API token (hub admin token create --scope brand:read)",
            )
            .into_response();
        }
        Some(t) => match lookup(&st.pool, t).await {
            Ok(Some(t)) => t,
            Ok(None) => return AppError::Unauthorized.into_response(),
            Err(e) => return e.into_response(),
        },
    };
    if token.scope != BRAND_READ {
        return AppError::Forbidden("this token's scope doesn't include brand:read")
            .into_response();
    }
    let (method, path) = (req.method().clone(), req.uri().path().to_string());
    if !st.token_limits.allow(&token.name) {
        tracing::warn!(token = %token.name, %method, %path, "api token over its rate limit");
        return AppError::TooMany.into_response();
    }
    let name = token.name.clone();
    req.extensions_mut().insert(token);
    let res = next.run(req).await;
    tracing::info!(token = %name, %method, path = %format!("/v1{path}"), status = res.status().as_u16(), "api token call");
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_limit_is_per_token() {
        let l = Limiter::default();
        for _ in 0..PER_MINUTE {
            assert!(l.allow("a"));
        }
        assert!(!l.allow("a"));
        assert!(l.allow("b"));
    }
}
