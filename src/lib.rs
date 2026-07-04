//! Nexus — a context capture protocol. Forward anything — links, notes,
//! voice notes, photos — from any connected channel (Telegram, WhatsApp,
//! later IG/X) and it builds a personal knowledge brain over it.
//!
//! v0: Layer 1 (captures + context envelope) + Layer 2 (events + taste profile)
//! per user, queryable across all channels via `/ask`.
//!
//! Library crate exposing all subsystems; the `nexus` binary (`src/main.rs`)
//! is a thin wiring layer over these modules, and integration tests in
//! `tests/` drive them directly.

pub mod bot;
pub mod config;
pub mod cron;
pub mod db;
pub mod graph;
pub mod http;
pub mod ingestion;
pub mod intake;
pub mod llm;
pub mod models;
pub mod query;
pub mod scorer;
pub mod search;
pub mod state;
pub mod whatsapp;
