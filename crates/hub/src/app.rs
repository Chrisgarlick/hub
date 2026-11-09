//! The router and shared state.

use std::sync::Arc;

use axum::Router;
use axum::routing::{get, post};
use sqlx::PgPool;

use crate::clients::Clients;
use crate::config::Config;
use crate::{assets, auth, health, passkeys, routes};

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub pool: PgPool,
    /// `None` until Hub has a Clients service key.
    pub clients: Option<Clients>,
}

impl AppState {
    pub fn new(config: Config, pool: PgPool) -> Self {
        let clients = match (&config.clients_url, &config.clients_service_key) {
            (Some(url), Some(key)) if !key.is_empty() => Some(Clients::new(url, key)),
            _ => None,
        };
        Self {
            config: Arc::new(config),
            pool,
            clients,
        }
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(health::healthz))
        .route("/readyz", get(health::readyz))
        .route("/auth/clients", get(auth::via_clients))
        .route("/auth/passkey/options", post(passkeys::request_options))
        .route("/auth/passkey/login", post(passkeys::login))
        .route("/auth/callback", get(auth::callback))
        .route("/logout", post(auth::logout))
        .nest("/api", routes::api(state.clone()))
        .fallback(assets::serve)
        .with_state(state)
}
