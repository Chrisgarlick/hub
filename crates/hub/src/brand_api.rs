//! `/v1`: a client's published brand, for site builds (wp-base, and later
//! shopify-base and laravel-base) with a Hub API token scoped `brand:read`
//! (`crate::tokens`).
//!
//! Clients stays the source of truth: every call goes through to Clients
//! with Hub's service key, and nothing is kept.
//!
//! - `GET /v1/clients/{slug}/brand[?version=N]`: the published brand JSON,
//!   exactly as Clients publishes it (with its `version`), each entry under
//!   `assets` (the `lib:` names it uses) also saying its `name`, `use` (font,
//!   logo or image), `file` and the Hub `url` to fetch it.
//! - `GET /v1/clients/{slug}/brand/assets/{name}[?version=N]`: one of those
//!   files, streamed from Clients, with its content type and its SHA-256 as
//!   the ETag.
//! - `GET /v1/clients/{slug}/brand/bundle[?version=N]`: a zip of
//!   `brand.json` (as above) and `assets/<file>` for each, checked against
//!   their SHA-256s, up to [`BUNDLE_MAX`].

use std::io::Write as _;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router, middleware};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::app::AppState;
use crate::error::{AppError, AppResult};
use crate::tokens;

/// The most a bundle may hold (Clients allows 25 MB a file).
pub const BUNDLE_MAX: u64 = 64 << 20;

pub fn v1(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/clients/{slug}/brand", get(brand))
        .route("/clients/{slug}/brand/assets/{name}", get(asset))
        .route("/clients/{slug}/brand/bundle", get(bundle))
        .fallback(|| async { AppError::NotFound("route") })
        .layer(middleware::from_fn_with_state(
            state,
            tokens::require_brand_read,
        ))
}

#[derive(Deserialize)]
struct Pin {
    version: Option<i64>,
}

fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 64
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The published brand from Clients, and its version.
async fn published(st: &AppState, slug: &str, version: Option<i64>) -> AppResult<(Value, i64)> {
    if !valid_slug(slug) {
        return Err(AppError::Missing(format!("no client `{slug}`")));
    }
    if version.is_some_and(|v| v < 1) {
        return Err(AppError::BadRequest(
            "version is a published version number, from 1".into(),
        ));
    }
    let clients = st.clients.as_ref().ok_or(AppError::Unavailable(
        "Hub isn't connected to Clients".into(),
    ))?;
    let doc = clients.published_brand(slug, version).await?;
    let v = doc["version"]
        .as_i64()
        .or(version)
        .ok_or_else(|| AppError::Unavailable("Clients' brand has no version".into()))?;
    Ok((doc, v))
}

fn extension(mime: &str) -> &'static str {
    match mime {
        "font/ttf" => "ttf",
        "font/otf" => "otf",
        "font/woff" => "woff",
        "font/woff2" => "woff2",
        "image/svg+xml" => "svg",
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => "bin",
    }
}

/// The document with each `assets` entry described for a build: `name`,
/// `use`, `file` and `url`. What Clients published is left as it was.
fn describe(st: &AppState, slug: &str, version: i64, mut doc: Value) -> Value {
    doc["version"] = json!(version);
    let logos: Vec<String> = doc["logos"]
        .as_object()
        .map(|m| {
            m.values()
                .filter_map(|v| v.as_str()?.strip_prefix("lib:").map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if let Some(assets) = doc.get_mut("assets").and_then(Value::as_object_mut) {
        for (name, a) in assets.iter_mut() {
            let kind = a["kind"].as_str().unwrap_or("image").to_string();
            let mime = a["mime"].as_str().unwrap_or_default().to_string();
            let used = if logos.contains(name) {
                "logo"
            } else if kind == "font" {
                "font"
            } else {
                "image"
            };
            a["name"] = json!(name);
            a["use"] = json!(used);
            a["file"] = json!(format!("{name}.{}", extension(&mime)));
            a["url"] = json!(format!(
                "{}/v1/clients/{slug}/brand/assets/{name}?version={version}",
                st.config.public_origin
            ));
        }
    }
    doc
}

/// `GET /v1/clients/{slug}/brand`.
async fn brand(
    State(st): State<AppState>,
    Path(slug): Path<String>,
    Query(pin): Query<Pin>,
) -> AppResult<Response> {
    let (doc, version) = published(&st, &slug, pin.version).await?;
    let doc = describe(&st, &slug, version, doc);
    Ok((
        [
            (header::ETAG, format!("\"v{version}\"")),
            (header::CACHE_CONTROL, "no-cache".into()),
        ],
        Json(doc),
    )
        .into_response())
}

/// An asset the brand uses: (sha256, mime, bytes, file).
fn entry(
    doc: &Value,
    slug: &str,
    version: i64,
    name: &str,
) -> AppResult<(String, String, u64, String)> {
    let a = &doc["assets"][name];
    let sha = a["sha256"].as_str().ok_or_else(|| {
        AppError::Missing(format!(
            "`{name}` isn't a file {slug}'s brand v{version} uses"
        ))
    })?;
    let mime = a["mime"]
        .as_str()
        .unwrap_or("application/octet-stream")
        .to_string();
    let file = format!("{name}.{}", extension(&mime));
    Ok((
        sha.to_string(),
        mime,
        a["bytes"].as_u64().unwrap_or(0),
        file,
    ))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `GET /v1/clients/{slug}/brand/assets/{name}`.
async fn asset(
    State(st): State<AppState>,
    Path((slug, name)): Path<(String, String)>,
    Query(pin): Query<Pin>,
    headers: HeaderMap,
) -> AppResult<Response> {
    if !crate::brand::valid_name(&name) {
        return Err(AppError::Missing(format!("no file `{name}`")));
    }
    let (doc, version) = published(&st, &slug, pin.version).await?;
    let (sha, mime, _, file) = entry(&doc, &slug, version, &name)?;
    let etag = format!("\"{sha}\"");
    let cache = if pin.version.is_some() {
        "private, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        == Some(etag.as_str())
    {
        return Ok((
            StatusCode::NOT_MODIFIED,
            [(header::ETAG, etag), (header::CACHE_CONTROL, cache.into())],
        )
            .into_response());
    }
    let clients = st.clients.as_ref().ok_or(AppError::Unavailable(
        "Hub isn't connected to Clients".into(),
    ))?;
    let upstream = clients.blob(&sha).await?;
    let length = upstream.content_length();
    let mut res = Body::from_stream(upstream.bytes_stream()).into_response();
    let h = res.headers_mut();
    let value = |s: &str| HeaderValue::from_str(s).map_err(|e| AppError::Internal(e.into()));
    h.insert(header::CONTENT_TYPE, value(&mime)?);
    h.insert(header::ETAG, value(&etag)?);
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    h.insert("x-content-sha256", value(&sha)?);
    h.insert(
        header::CONTENT_DISPOSITION,
        value(&format!("attachment; filename=\"{file}\""))?,
    );
    if let Some(n) = length {
        h.insert(header::CONTENT_LENGTH, n.into());
    }
    Ok(res)
}

/// `GET /v1/clients/{slug}/brand/bundle`.
async fn bundle(
    State(st): State<AppState>,
    Path(slug): Path<String>,
    Query(pin): Query<Pin>,
) -> AppResult<Response> {
    let (doc, version) = published(&st, &slug, pin.version).await?;
    let names: Vec<String> = doc["assets"]
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    let mut files = Vec::with_capacity(names.len());
    let mut total = 0u64;
    for name in &names {
        let e = entry(&doc, &slug, version, name)?;
        total += e.2;
        files.push(e);
    }
    if total > BUNDLE_MAX {
        return Err(AppError::TooLarge(format!(
            "{slug}'s brand files come to {} MB, over the bundle's {} MB: fetch them one by one from brand/assets",
            total >> 20,
            BUNDLE_MAX >> 20
        )));
    }
    let clients = st.clients.as_ref().ok_or(AppError::Unavailable(
        "Hub isn't connected to Clients".into(),
    ))?;
    let doc = describe(&st, &slug, version, doc);
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let stored = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .unix_permissions(0o644);
    let zerr = |e: zip::result::ZipError| AppError::Internal(e.into());
    zip.start_file("brand.json", stored).map_err(zerr)?;
    zip.write_all(
        serde_json::to_string_pretty(&doc)
            .unwrap_or_default()
            .as_bytes(),
    )
    .map_err(|e| AppError::Internal(e.into()))?;
    let mut sent = 0u64;
    for (sha, _, _, file) in &files {
        let bytes = clients
            .blob(sha)
            .await?
            .bytes()
            .await
            .map_err(|e| AppError::Unavailable(format!("Clients' file {file}: {e}")))?;
        sent += bytes.len() as u64;
        if sent > BUNDLE_MAX {
            return Err(AppError::TooLarge(format!(
                "{slug}'s brand files are over the bundle's {} MB: fetch them one by one from brand/assets",
                BUNDLE_MAX >> 20
            )));
        }
        if hex(&Sha256::digest(&bytes)) != *sha {
            return Err(AppError::Unavailable(format!(
                "{file} arrived from Clients corrupted (its SHA-256 doesn't match)"
            )));
        }
        zip.start_file(format!("assets/{file}"), stored)
            .map_err(zerr)?;
        zip.write_all(&bytes)
            .map_err(|e| AppError::Internal(e.into()))?;
    }
    let body = zip.finish().map_err(zerr)?.into_inner();
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{slug}-brand-v{version}.zip\""),
            ),
            (header::ETAG, format!("\"v{version}\"")),
            (header::CACHE_CONTROL, "no-cache".into()),
        ],
        body,
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_and_extensions() {
        assert!(valid_slug("chris-garlick"));
        assert!(!valid_slug("../x") && !valid_slug("") && !valid_slug("Acme"));
        assert_eq!(extension("font/otf"), "otf");
        assert_eq!(extension("image/svg+xml"), "svg");
        assert_eq!(extension("text/x"), "bin");
        assert_eq!(hex(&[0, 255, 16]), "00ff10");
    }
}
