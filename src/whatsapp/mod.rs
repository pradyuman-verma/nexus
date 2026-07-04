//! WhatsApp ingress channel (Business Cloud API).
//!
//! Webhook → identity resolution → the same intake/ingestion pipeline as
//! every other channel. Replies (query answers, capture acks) go back out
//! through the Cloud API client.

pub mod client;
pub mod format;
pub mod handler;
pub mod signature;
pub mod types;

pub use client::WhatsApp;
