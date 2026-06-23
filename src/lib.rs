//! Nexus — a group ambient intelligence layer for Telegram.
//!
//! Library crate exposing all subsystems; the `nexus` binary (`src/main.rs`)
//! is a thin wiring layer over these modules, and integration tests in
//! `tests/` drive them directly.

pub mod bot;
pub mod config;
pub mod cron;
pub mod db;
pub mod graph;
pub mod ingestion;
pub mod llm;
pub mod models;
pub mod query;
pub mod scorer;
pub mod state;
