//! Who's calling (spec section 9).
//!
//! - In a browser: a passkey (`crate::passkeys`), or "Sign in with Clients"
//!   to get in the first time (and as a fallback): `/auth/clients` sends you
//!   to Clients, which sends you back to `/auth/callback` with a one-time
//!   code; Hub swaps it (with its service key). Either way Hub keeps its own
//!   session cookie.
//! - From Claude or a script: a Clients key (`Authorization: Bearer ck_…`),
//!   checked with Clients and cached for 60 seconds.
//!
//! Cookie sessions must send an `Origin` matching `public_origin` on writes.

use axum::extract::{Query, Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{TimeDelta, Utc};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::app::AppState;
use crate::error::{AppError, AppResult};

const SESSION_DAYS: i64 = 30;

/// The authenticated caller, attached to every `/api` request.
#[derive(Debug, Clone)]
pub struct Caller {
    /// `user` for you in a browser, otherwise the key's name.
    pub actor: String,
    /// Client slugs a key is limited to; `None` means all.
    pub clients: Option<Vec<String>>,
    /// Signed in in a browser (not a key): only this may manage passkeys.
    pub session: bool,
}

impl Caller {
    pub fn may(&self, slug: &str) -> bool {
        slug == crate::spaces::STUDIO
            || self
                .clients
                .as_ref()
                .is_none_or(|c| c.iter().any(|s| s == slug))
    }
}

fn hash(secret: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(secret.as_bytes()))
}

fn cookie_name(state: &AppState) -> &'static str {
    // `__Host-` cookies must be Secure, so only over https.
    if state.config.public_origin.starts_with("https://") {
        "__Host-hub"
    } else {
        "hub_session"
    }
}

pub fn session_cookie(state: &AppState, value: &str) -> String {
    let secure = if state.config.public_origin.starts_with("https://") {
        "; Secure"
    } else {
        ""
    };
    format!(
        "{}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{secure}",
        cookie_name(state),
        SESSION_DAYS * 86_400
    )
}

fn session_from(state: &AppState, headers: &HeaderMap) -> Option<String> {
    let name = cookie_name(state);
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_string())
        .filter(|v| !v.is_empty())
}

/// 32 random bytes, base64url: sessions and passkey challenges.
pub fn random_secret() -> String {
    let mut bytes = [0u8; 32];
    rand::fill(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

pub async fn start_session(state: &AppState) -> AppResult<String> {
    let session = random_secret();
    sqlx::query("INSERT INTO sessions (id_hash, expires_at) VALUES ($1, $2)")
        .bind(hash(&session))
        .bind(Utc::now() + TimeDelta::days(SESSION_DAYS))
        .execute(&state.pool)
        .await?;
    Ok(session)
}

async fn session_valid(state: &AppState, session: &str) -> AppResult<bool> {
    let r = sqlx::query(
        "UPDATE sessions SET last_seen_at = now() WHERE id_hash = $1 AND expires_at > now()",
    )
    .bind(hash(session))
    .execute(&state.pool)
    .await?;
    Ok(r.rows_affected() == 1)
}

/// Authenticate an `/api` request and attach a [`Caller`].
pub async fn require_caller(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Response {
    match caller(&state, req.headers(), req.method()).await {
        Ok(c) => {
            req.extensions_mut().insert(c);
            next.run(req).await
        }
        Err(e) => e.into_response(),
    }
}

async fn caller(state: &AppState, headers: &HeaderMap, method: &Method) -> AppResult<Caller> {
    if let Some(token) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
    {
        let clients = state.clients.as_ref().ok_or(AppError::Unavailable(
            "keys can't be checked: Hub isn't connected to Clients".into(),
        ))?;
        return match clients.introspect(token).await? {
            Some(c) if c.kind == "caller" => Ok(Caller {
                actor: c.key,
                clients: c.clients,
                session: false,
            }),
            Some(_) => Err(AppError::Forbidden("use a caller key")),
            None => Err(AppError::Unauthorized),
        };
    }
    let session = session_from(state, headers).ok_or(AppError::Unauthorized)?;
    if !session_valid(state, &session).await? {
        return Err(AppError::Unauthorized);
    }
    if !matches!(*method, Method::GET | Method::HEAD) {
        let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
        if origin != Some(state.config.public_origin.as_str()) {
            return Err(AppError::Forbidden("cross-site request refused"));
        }
    }
    Ok(Caller {
        actor: "user".into(),
        clients: None,
        session: true,
    })
}

/// Only a path on this site: `/…` but not `//…`.
fn safe_return(r: Option<&str>) -> String {
    match r {
        Some(p) if p.starts_with('/') && !p.starts_with("//") && !p.contains('\\') => p.to_string(),
        _ => "/".to_string(),
    }
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[derive(Deserialize)]
pub struct LoginQuery {
    #[serde(default, rename = "return")]
    ret: Option<String>,
}

/// `GET /auth/clients`: off to Clients to sign in (or straight in, in
/// development).
pub async fn via_clients(
    State(state): State<AppState>,
    Query(q): Query<LoginQuery>,
) -> AppResult<Response> {
    let ret = safe_return(q.ret.as_deref());
    if state.config.dev_login {
        let session = start_session(&state).await?;
        return Ok((
            [(header::SET_COOKIE, session_cookie(&state, &session))],
            Redirect::to(&ret),
        )
            .into_response());
    }
    let url = format!(
        "{}/handoff?app=hub&return={}",
        state.config.clients_public_url.trim_end_matches('/'),
        enc(&ret)
    );
    Ok(Redirect::to(&url).into_response())
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    code: String,
    #[serde(default, rename = "return")]
    ret: Option<String>,
}

/// `GET /auth/callback?code=…&return=…`: back from Clients.
pub async fn callback(
    State(state): State<AppState>,
    Query(q): Query<CallbackQuery>,
) -> AppResult<Response> {
    let clients = state.clients.as_ref().ok_or(AppError::Unavailable(
        "Hub isn't connected to Clients".into(),
    ))?;
    if !clients.handoff(&q.code).await? {
        return Ok((
            StatusCode::UNAUTHORIZED,
            "That sign-in link has expired. Go back and sign in again.",
        )
            .into_response());
    }
    let session = start_session(&state).await?;
    let ret = safe_return(q.ret.as_deref());
    Ok((
        [(header::SET_COOKIE, session_cookie(&state, &session))],
        Redirect::to(&ret),
    )
        .into_response())
}

/// `POST /logout`.
pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> AppResult<Response> {
    if let Some(s) = session_from(&state, &headers) {
        sqlx::query("DELETE FROM sessions WHERE id_hash = $1")
            .bind(hash(&s))
            .execute(&state.pool)
            .await?;
    }
    let clear = format!(
        "{}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0",
        cookie_name(&state)
    );
    Ok(([(header::SET_COOKIE, clear)], StatusCode::NO_CONTENT).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn return_paths_stay_on_site() {
        assert_eq!(safe_return(Some("/s/acme")), "/s/acme");
        assert_eq!(safe_return(Some("//evil.example")), "/");
        assert_eq!(safe_return(Some("https://evil.example")), "/");
        assert_eq!(safe_return(Some("/\\evil")), "/");
        assert_eq!(safe_return(None), "/");
        assert_eq!(enc("/s/acme?x=1&y"), "/s/acme%3Fx%3D1%26y");
    }
}
