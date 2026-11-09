//! `/healthz` (the process is up) and `/readyz` (it can do its job: the
//! database answers with pgvector enabled, and Clients is reachable).

use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::app::AppState;

pub async fn healthz() -> Json<Value> {
    Json(json!({ "status": "ok", "version": env!("CARGO_PKG_VERSION") }))
}

pub async fn readyz(State(state): State<AppState>) -> (StatusCode, Json<Value>) {
    let database = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'vector')",
    )
    .fetch_one(&state.pool)
    .await;
    let (database, vector) = match database {
        Ok(v) => (true, v),
        Err(e) => {
            tracing::warn!(error = %e, "database not ready");
            (false, false)
        }
    };
    let clients = match state.config.clients_url.clone() {
        None => Value::Null,
        Some(url) => json!(clients_up(url).await),
    };
    let ready = database && vector && clients != json!(false);
    (
        if ready {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(json!({ "database": database, "vector": vector, "clients": clients })),
    )
}

/// Clients' own `/healthz` answers.
async fn clients_up(url: String) -> bool {
    reqwest::Client::new()
        .get(format!("{}/healthz", url.trim_end_matches('/')))
        .timeout(Duration::from_secs(3))
        .send()
        .await
        .is_ok_and(|r| r.status().is_success())
}
