//! A client's brand in Clients, from Claude: the library (fonts and logos)
//! and the brand draft. Clients lets Hub's key do both (its `CLIENTS_MANAGERS`);
//! publishing stays in Clients' admin pages, behind the password and a code.
//!
//! Files are too big to pass through a tool call, so `brand_upload_link`
//! hands out a one-time address instead, as Design's `upload_link` does:
//! `POST /u/{token}` with the file as the body, within 15 minutes, once. Hub
//! passes the file to Clients under the name the link was made for. Links
//! live in memory: a restart only means asking for a new one.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::app::AppState;

/// How long an upload link works.
pub const UPLOAD_TTL: Duration = Duration::from_secs(15 * 60);
/// Clients' own limit on a library file.
pub const MAX_BYTES: usize = 25 << 20;

/// A library name, as Clients takes it and brands refer to it (`lib:<name>`).
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

struct Pending {
    slug: String,
    name: String,
    by: String,
    expires: Instant,
}

/// Upload links waiting to be used, by the hash of their token.
#[derive(Clone, Default)]
pub struct Uploads(Arc<Mutex<HashMap<Vec<u8>, Pending>>>);

impl Uploads {
    /// A new link's token, for a file to go into `slug`'s library as `name`.
    pub fn issue(&self, slug: &str, name: &str, by: &str) -> String {
        let token = crate::auth::random_secret();
        let mut map = self.0.lock().unwrap();
        map.retain(|_, p| p.expires > Instant::now());
        map.insert(
            Sha256::digest(token.as_bytes()).to_vec(),
            Pending {
                slug: slug.into(),
                name: name.into(),
                by: by.into(),
                expires: Instant::now() + UPLOAD_TTL,
            },
        );
        token
    }

    /// The link's target, used up: `None` if unknown, used or expired.
    fn take(&self, token: &str) -> Option<Pending> {
        let p = self
            .0
            .lock()
            .unwrap()
            .remove(&Sha256::digest(token.as_bytes()).to_vec())?;
        (p.expires > Instant::now()).then_some(p)
    }
}

/// `POST /u/{token}`: the file, as the body. Answers with what Clients
/// stored (`name`, `sha256`, `kind`…), or a problem.
pub async fn receive(
    State(st): State<AppState>,
    Path(token): Path<String>,
    body: Bytes,
) -> Response {
    let problem = |status: StatusCode, detail: String| {
        (
            status,
            Json(json!({ "error": status.canonical_reason(), "message": detail })),
        )
            .into_response()
    };
    // Before the link is used up, so a mistake doesn't cost it.
    if body.is_empty() {
        return problem(
            StatusCode::UNPROCESSABLE_ENTITY,
            "send the file as the request body (curl --data-binary @file)".into(),
        );
    }
    let Some(p) = st.uploads.take(&token) else {
        return problem(
            StatusCode::NOT_FOUND,
            "this upload link is unknown, used or expired: ask for a new one with brand_upload_link".into(),
        );
    };
    let Some(clients) = st.clients.as_ref() else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "Hub isn't connected to Clients".into(),
        );
    };
    match clients.put_library(&p.slug, &p.name, body.to_vec()).await {
        Ok(v) => {
            tracing::info!(slug = %p.slug, name = %p.name, by = %p.by, bytes = body.len(), "library file uploaded to Clients");
            Json(json!({ "client": p.slug, "name": p.name, "clients": v, "use_as": format!("lib:{}", p.name) })).into_response()
        }
        Err(e) => e.into_response(),
    }
}
