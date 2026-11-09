//! Passkey routes, as in Board: adding and removing passkeys (signed in, in
//! a browser) and signing in with one (public). The checks are in
//! [`crate::passkey`].

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgConnection;

use crate::app::AppState;
use crate::auth::{self, Caller};
use crate::error::{AppError, AppResult};
use crate::passkey::{self, RelyingParty, b64, unb64};

const CHALLENGE_MINUTES: i64 = 5;
const MAX_NAME_CHARS: usize = 60;
const MAX_PASSKEYS: i64 = 20;
/// Outstanding challenges at once. Sign-in options are public, so this
/// stops anyone filling the table; real use needs one or two.
const MAX_CHALLENGES: i64 = 200;

fn rp(state: &AppState) -> RelyingParty {
    RelyingParty::from_origin(&state.config.public_origin)
}

async fn issue_challenge(state: &AppState, kind: &str) -> AppResult<String> {
    let challenge = auth::random_secret();
    sqlx::query("DELETE FROM passkey_challenges WHERE expires_at <= now()")
        .execute(&state.pool)
        .await?;
    let open: i64 = sqlx::query_scalar("SELECT count(*) FROM passkey_challenges")
        .fetch_one(&state.pool)
        .await?;
    if open >= MAX_CHALLENGES {
        return Err(AppError::State {
            code: "busy",
            message: "Too many sign-in attempts right now; try again in a few minutes.".into(),
        });
    }
    sqlx::query("INSERT INTO passkey_challenges (challenge, kind, expires_at) VALUES ($1, $2, $3)")
        .bind(&challenge)
        .bind(kind)
        .bind(Utc::now() + TimeDelta::minutes(CHALLENGE_MINUTES))
        .execute(&state.pool)
        .await?;
    Ok(challenge)
}

/// Use up the challenge the browser signed. Returns it if it was ours, of
/// this kind, and unexpired.
async fn take_challenge(
    conn: &mut PgConnection,
    kind: &str,
    client_data_json: &[u8],
) -> AppResult<Option<String>> {
    let Some(challenge) = serde_json::from_slice::<Value>(client_data_json)
        .ok()
        .and_then(|v| {
            v.get("challenge")?
                .as_str()
                .map(|c| c.trim_end_matches('=').to_string())
        })
    else {
        return Ok(None);
    };
    let used = sqlx::query(
        "DELETE FROM passkey_challenges WHERE challenge = $1 AND kind = $2 AND expires_at > now()",
    )
    .bind(&challenge)
    .bind(kind)
    .execute(conn)
    .await?;
    Ok((used.rows_affected() == 1).then_some(challenge))
}

fn only_the_user(caller: &Caller) -> AppResult<()> {
    // Passkeys are for you in a browser, not keys: a leaked key mustn't be
    // able to turn itself into a login.
    if !caller.session {
        return Err(AppError::Forbidden(
            "passkeys can only be managed from a signed-in browser",
        ));
    }
    Ok(())
}

// ---------- Managing passkeys (signed in) ----------

#[derive(Serialize, sqlx::FromRow)]
pub struct PasskeyInfo {
    id: String,
    name: String,
    created_at: DateTime<Utc>,
    last_used_at: Option<DateTime<Utc>>,
}

/// `GET /api/passkeys`
pub async fn list(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
) -> AppResult<Json<Value>> {
    only_the_user(&caller)?;
    let list: Vec<PasskeyInfo> = sqlx::query_as(
        "SELECT id, name, created_at, last_used_at FROM passkeys ORDER BY created_at",
    )
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(json!({ "passkeys": list })))
}

/// `POST /api/passkeys/options`: what the browser needs to make a passkey.
pub async fn creation_options(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
) -> AppResult<Json<Value>> {
    only_the_user(&caller)?;
    let existing: Vec<String> = sqlx::query_scalar("SELECT id FROM passkeys")
        .fetch_all(&state.pool)
        .await?;
    let challenge = issue_challenge(&state, "register").await?;
    Ok(Json(json!(passkey::creation_options(
        &rp(&state),
        challenge,
        existing
    ))))
}

#[derive(Deserialize)]
pub struct Registration {
    name: String,
    #[serde(rename = "clientDataJSON")]
    client_data_json: String,
    #[serde(rename = "attestationObject")]
    attestation_object: String,
}

/// `POST /api/passkeys`: check and keep a new passkey.
pub async fn register(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    Json(body): Json<Registration>,
) -> AppResult<(StatusCode, Json<Value>)> {
    only_the_user(&caller)?;
    let name = body.name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME_CHARS {
        return Err(AppError::invalid(
            "name",
            format!("give the passkey a name of up to {MAX_NAME_CHARS} characters"),
        ));
    }
    let bad = |e: passkey::Refused| {
        tracing::warn!(reason = %e, "passkey registration refused");
        AppError::BadRequest(format!("Couldn't add that passkey: {e}."))
    };
    let client = unb64(&body.client_data_json, "clientDataJSON").map_err(bad)?;
    let attestation = unb64(&body.attestation_object, "attestationObject").map_err(bad)?;

    let mut tx = state.pool.begin().await?;
    // One registration at a time, so the limit holds.
    sqlx::query("LOCK TABLE passkeys IN SHARE ROW EXCLUSIVE MODE")
        .execute(&mut *tx)
        .await?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM passkeys")
        .fetch_one(&mut *tx)
        .await?;
    if count >= MAX_PASSKEYS {
        return Err(AppError::State {
            code: "too_many_passkeys",
            message: format!("Hub keeps at most {MAX_PASSKEYS} passkeys; remove one first"),
        });
    }
    let challenge = take_challenge(&mut tx, "register", &client)
        .await?
        .ok_or_else(|| {
            AppError::BadRequest("That took too long; try adding the passkey again.".into())
        })?;
    let cred = passkey::verify_registration(&rp(&state), &challenge, &client, &attestation)
        .map_err(bad)?;
    let id = b64(&cred.id);
    let created: Option<DateTime<Utc>> = sqlx::query_scalar(
        "INSERT INTO passkeys (id, name, public_key, sign_count) VALUES ($1, $2, $3, $4)
         ON CONFLICT (id) DO NOTHING RETURNING created_at",
    )
    .bind(&id)
    .bind(name)
    .bind(&cred.public_key)
    .bind(i64::from(cred.sign_count))
    .fetch_optional(&mut *tx)
    .await?;
    let Some(created) = created else {
        return Err(AppError::State {
            code: "passkey_exists",
            message: "that passkey is already registered".into(),
        });
    };
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "id": id, "name": name, "created_at": created, "last_used_at": null })),
    ))
}

/// `DELETE /api/passkeys/{id}`
pub async fn remove(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> AppResult<StatusCode> {
    only_the_user(&caller)?;
    let gone = sqlx::query("DELETE FROM passkeys WHERE id = $1")
        .bind(&id)
        .execute(&state.pool)
        .await?;
    if gone.rows_affected() == 0 {
        return Err(AppError::NotFound("passkey"));
    }
    Ok(StatusCode::NO_CONTENT)
}

// ---------- Signing in (public) ----------

/// `POST /auth/passkey/options`: a challenge to sign.
pub async fn request_options(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<Json<Value>> {
    same_origin(&state, &headers)?;
    let challenge = issue_challenge(&state, "login").await?;
    Ok(Json(json!(passkey::request_options(
        &rp(&state),
        challenge
    ))))
}

#[derive(Deserialize)]
pub struct Assertion {
    /// The credential id (rawId), base64url.
    id: String,
    #[serde(rename = "clientDataJSON")]
    client_data_json: String,
    #[serde(rename = "authenticatorData")]
    authenticator_data: String,
    signature: String,
}

/// `POST /auth/passkey/login`: check the signature and start a session.
pub async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Assertion>,
) -> AppResult<Response> {
    same_origin(&state, &headers)?;
    let refused = |why: &str| {
        tracing::warn!(reason = why, "passkey sign-in refused");
        AppError::Unauthorized
    };
    let client =
        unb64(&body.client_data_json, "clientDataJSON").map_err(|e| refused(&e.to_string()))?;
    let auth_data = unb64(&body.authenticator_data, "authenticatorData")
        .map_err(|e| refused(&e.to_string()))?;
    let signature = unb64(&body.signature, "signature").map_err(|e| refused(&e.to_string()))?;
    let id = body.id.trim_end_matches('=');

    let mut tx = state.pool.begin().await?;
    let challenge = take_challenge(&mut tx, "login", &client)
        .await?
        .ok_or_else(|| refused("unknown or expired challenge"))?;
    let stored: Option<(Vec<u8>, i64)> =
        sqlx::query_as("SELECT public_key, sign_count FROM passkeys WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some((public_key, stored_count)) = stored else {
        tx.commit().await?; // the challenge stays used
        return Err(AppError::State {
            code: "unknown_passkey",
            message: "That passkey isn't registered with Hub (it may have been removed).".into(),
        });
    };
    let verified = passkey::verify_assertion(
        &rp(&state),
        &challenge,
        &client,
        &auth_data,
        &signature,
        &public_key,
        u32::try_from(stored_count).unwrap_or(0),
    );
    let count = match verified {
        Ok(c) => c,
        Err(e) => {
            tx.commit().await?; // the challenge stays used
            return Err(refused(&e.to_string()));
        }
    };
    sqlx::query("UPDATE passkeys SET sign_count = $1, last_used_at = now() WHERE id = $2")
        .bind(i64::from(count))
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    let session = auth::start_session(&state).await?;
    Ok((
        [
            (header::SET_COOKIE, auth::session_cookie(&state, &session)),
            (header::CACHE_CONTROL, "no-store".to_string()),
        ],
        Json(json!({ "ok": true })),
    )
        .into_response())
}

/// The sign-in routes are public, so at least insist the request comes from
/// Hub's own pages (the signed client data is checked too).
fn same_origin(state: &AppState, headers: &HeaderMap) -> AppResult<()> {
    match headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        Some(o) if o == state.config.public_origin => Ok(()),
        _ => Err(AppError::Forbidden("sign in from Hub's own page")),
    }
}
