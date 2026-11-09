//! The `hub` command line.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use crate::app::{self, AppState};
use crate::config::Config;
use crate::db;

#[derive(Parser)]
#[command(
    name = "hub",
    version,
    about = "What you know about each client, and where every job starts"
)]
pub struct Cli {
    /// Path to the config file.
    #[arg(long, env = "HUB_CONFIG", default_value = "hub.toml", global = true)]
    config: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the server.
    Serve,
    /// Run the database migrations and exit.
    Migrate,
    /// Rebuild every search chunk and embedding (phase 3).
    Reindex,
}

pub async fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Serve => serve(&cli.config).await,
        Command::Migrate => {
            let config = Config::load(&cli.config)?;
            db::connect(&config.database_url).await?;
            println!("Migrations are up to date.");
            Ok(())
        }
        Command::Reindex => {
            let config = Config::load(&cli.config)?;
            let pool = db::connect(&config.database_url).await?;
            let n = crate::index::reindex(&pool).await?;
            println!(
                "Queued {n} documents and spaces; the running server re-embeds them over the next few minutes."
            );
            Ok(())
        }
    }
}

async fn serve(path: &Path) -> Result<()> {
    let config = Config::load(path)?;
    // Migrate before serving; if the database is down, start anyway and let
    // /readyz say so (systemd restarts would only hammer it).
    let (pool, connected) = match db::connect(&config.database_url).await {
        Ok(pool) => (pool, true),
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "database unavailable; starting without it");
            (db::lazy(&config.database_url)?, false)
        }
    };
    let bind = config.bind;
    let state = AppState::new(config, pool);
    // Housekeeping that needs the database, without holding up serving.
    if connected {
        let pool = state.pool.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::spaces::ensure_studio(&pool).await {
                tracing::warn!(error = %e, "studio space not made yet");
            }
            if let Err(e) = crate::rules::sync_all(&pool).await {
                tracing::warn!(error = %e, "rules not re-read from documents");
            }
        });
        if state.embedder.is_none() {
            tracing::warn!("no embedding_provider: search works by words only");
        }
        crate::index::spawn_worker(
            state.pool.clone(),
            state.embedder.clone(),
            state.index_wake.clone(),
        );
    }
    match &state.clients {
        Some(c) => crate::spaces::spawn_sync(state.pool.clone(), c.clone()),
        None => tracing::warn!("no Clients service key: spaces won't follow Clients"),
    }
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    tracing::info!(%bind, public_origin = %state.config.public_origin, "hub listening");
    axum::serve(listener, app::router(state))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .context("server error")
}
