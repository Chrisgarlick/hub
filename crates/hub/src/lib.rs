//! Hub: what you know about each client, and where every job starts.
//!
//! See `spec.md` for the design and `plan.md` for the build order. The
//! binary (`main.rs`) is a thin wrapper around [`cli`].

pub mod app;
pub mod assets;
pub mod cli;
pub mod config;
pub mod db;
pub mod health;
