//! Phase 0: the server starts, says whether it's ready, and serves the UI.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use hub::app::{AppState, router};
use hub::config::Config;
use serde_json::Value;
use tower::ServiceExt as _;

async fn get(state: AppState, uri: &str) -> (StatusCode, Vec<u8>) {
    let res = router(state)
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    (
        status,
        res.into_body().collect().await.unwrap().to_bytes().to_vec(),
    )
}

fn state(database_url: &str, pool: sqlx::PgPool) -> AppState {
    AppState {
        config: Arc::new(Config::parse(&format!("database_url = \"{database_url}\"")).unwrap()),
        pool,
    }
}

#[tokio::test]
async fn health_and_readiness_without_a_database() {
    // Nothing listens on port 1: the database is "down".
    let url = "postgres://hub:x@127.0.0.1:1/hub";
    let s = state(url, hub::db::lazy(url).unwrap());

    let (status, body) = get(s.clone(), "/healthz").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["status"],
        "ok"
    );

    let (status, body) = get(s.clone(), "/readyz").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["database"], false);
    assert!(v["clients"].is_null(), "Clients isn't configured here");

    // Unknown API paths are 404s, never the app shell.
    assert_eq!(
        get(s.clone(), "/api/nothing").await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(get(s, "/v1/nothing").await.0, StatusCode::NOT_FOUND);
}

/// With a real Postgres (CI sets `HUB_TEST_DATABASE_URL`): migrations run,
/// pgvector is enabled, and Hub is ready.
#[tokio::test]
async fn ready_with_postgres_and_pgvector() {
    let Ok(url) = std::env::var("HUB_TEST_DATABASE_URL") else {
        eprintln!("HUB_TEST_DATABASE_URL not set; skipping the database test");
        return;
    };
    let pool = hub::db::connect(&url).await.expect("connect and migrate");
    let (status, body) = get(state(&url, pool.clone()), "/readyz").await;
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["vector"], true);
    // pgvector really works: a distance between two vectors.
    let d: f64 = sqlx::query_scalar("SELECT ('[1,0,0]'::vector <=> '[0,1,0]'::vector)::float8")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!((d - 1.0).abs() < 1e-9, "cosine distance {d}");
}
