//! The ingestion queue consumer — a continuous background task draining the
//! mpsc channel fed by the message handler.

pub mod fetcher;
pub mod pipeline;
pub mod youtube;

use crate::models::IngestionJob;
use crate::state::AppState;
use tokio::sync::mpsc;

/// Run forever, processing jobs one at a time. Each job is fully self-contained;
/// a failure in one never affects the next.
pub async fn run_consumer(state: AppState, mut rx: mpsc::Receiver<IngestionJob>) {
    tracing::info!("ingestion consumer started");
    while let Some(job) = rx.recv().await {
        pipeline::process(&state, job).await;
    }
    tracing::warn!("ingestion consumer channel closed; exiting");
}
