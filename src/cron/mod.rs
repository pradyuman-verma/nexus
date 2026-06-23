//! tokio-cron-scheduler wiring.

pub mod jobs;

use crate::models::IngestionJob;
use crate::state::AppState;
use anyhow::Result;
use tokio::sync::mpsc;
use tokio_cron_scheduler::{Job, JobScheduler};

/// Build and start the scheduler. Holds the `tx` clone so the health job can
/// observe queue depth.
pub async fn start(state: AppState, tx: mpsc::Sender<IngestionJob>) -> Result<JobScheduler> {
    let sched = JobScheduler::new().await?;

    // Graph builder — every 6 hours.
    {
        let state = state.clone();
        let schedule = state.config.graph_cron_schedule.clone();
        sched
            .add(Job::new_async(schedule.as_str(), move |_uuid, _l| {
                let state = state.clone();
                Box::pin(async move { jobs::graph_builder(&state).await })
            })?)
            .await?;
    }

    // Cleanup — daily.
    {
        let state = state.clone();
        let schedule = state.config.cleanup_cron_schedule.clone();
        sched
            .add(Job::new_async(schedule.as_str(), move |_uuid, _l| {
                let state = state.clone();
                Box::pin(async move { jobs::cleanup(&state).await })
            })?)
            .await?;
    }

    // Health + retry — every 15 minutes.
    {
        let state = state.clone();
        let tx = tx.clone();
        let schedule = state.config.health_cron_schedule.clone();
        sched
            .add(Job::new_async(schedule.as_str(), move |_uuid, _l| {
                let state = state.clone();
                let tx = tx.clone();
                Box::pin(async move { jobs::health_and_retry(&state, &tx).await })
            })?)
            .await?;
    }

    sched.start().await?;
    tracing::info!("cron scheduler started");
    Ok(sched)
}
