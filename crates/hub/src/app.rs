//! The router and shared state.

use std::sync::Arc;

use axum::Router;
use axum::routing::{get, post};
use sqlx::PgPool;

use tokio::sync::Notify;

use crate::clients::Clients;
use crate::config::Config;
use crate::embed::Embedder;
use crate::{assets, auth, brand, health, passkeys, routes};

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub pool: PgPool,
    /// `None` until Hub has a Clients service key.
    pub clients: Option<Clients>,
    /// `None`: search by words only.
    pub embedder: Option<Embedder>,
    /// Wakes the indexer when something's queued.
    pub index_wake: Arc<Notify>,
    /// Upload links for clients' libraries (`brand_upload_link`).
    pub uploads: crate::brand::Uploads,
}

impl AppState {
    pub fn new(config: Config, pool: PgPool) -> Self {
        let clients = match (&config.clients_url, &config.clients_service_key) {
            (Some(url), Some(key)) if !key.is_empty() => Some(Clients::new(url, key)),
            _ => None,
        };
        let embedder = Embedder::from_config(&config);
        Self {
            config: Arc::new(config),
            pool,
            clients,
            embedder,
            index_wake: Arc::new(Notify::new()),
            uploads: crate::brand::Uploads::default(),
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
        // A one-time upload link (brand_upload_link): the token is the auth.
        .route(
            "/u/{token}",
            post(brand::receive).layer(axum::extract::DefaultBodyLimit::max(
                brand::MAX_BYTES + 1024,
            )),
        )
        .nest("/api", routes::api(state.clone()))
        .nest_service("/mcp", crate::mcp::service(state.clone()))
        .fallback(assets::serve)
        .with_state(state)
}
