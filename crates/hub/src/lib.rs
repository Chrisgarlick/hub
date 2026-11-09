//! Hub: what you know about each client, and where every job starts.
//!
//! See `spec.md` for the design and `plan.md` for the build order. The
//! binary (`main.rs`) is a thin wrapper around [`cli`].

pub mod app;
pub mod assets;
pub mod auth;
pub mod cli;
pub mod clients;
pub mod config;
pub mod db;
pub mod documents;
pub mod error;
pub mod health;
pub mod passkey;
pub mod passkeys;
pub mod routes;
pub mod spaces;
