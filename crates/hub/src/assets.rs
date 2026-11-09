//! The embedded frontend (`web/dist`).
//!
//! In debug builds `rust-embed` reads files from disk on each request, so a
//! frontend rebuild shows up without recompiling Rust. Release builds embed
//! the files in the binary.

use axum::body::Body;
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../../web/dist"]
#[allow_missing = true]
struct Dist;

/// Serve a built frontend file, falling back to `index.html` for app routes.
pub async fn serve(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');

    // Unknown API routes are real 404s, never the app shell.
    if path == "api"
        || path.starts_with("api/")
        || path == "mcp"
        || path.starts_with("mcp/")
        || path.starts_with("v1/")
    {
        return StatusCode::NOT_FOUND.into_response();
    }

    if !path.is_empty()
        && let Some(response) = file(path)
    {
        return response;
    }

    // Paths that look like files (have an extension) and weren't found are 404s;
    // everything else is a client-side route and gets the app shell.
    let looks_like_file = path
        .rsplit('/')
        .next()
        .is_some_and(|last| last.contains('.'));
    if looks_like_file {
        return StatusCode::NOT_FOUND.into_response();
    }

    file("index.html").unwrap_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Frontend not built. Run `npm run build` in web/.",
        )
            .into_response()
    })
}

fn file(path: &str) -> Option<Response> {
    let asset = Dist::get(path)?;
    let mime = asset.metadata.mimetype().to_string();
    // index.html must always be revalidated so a new deploy is picked up;
    // other files are small and served fresh too until we add hashed names.
    Some(
        (
            [
                (header::CONTENT_TYPE, mime),
                (header::CACHE_CONTROL, "no-cache".to_string()),
            ],
            Body::from(asset.data.into_owned()),
        )
            .into_response(),
    )
}
