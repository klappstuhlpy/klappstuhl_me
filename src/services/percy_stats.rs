//! Background poller that fetches Percy bot stats at a fixed interval and
//! publishes them on the "percy" WebSocket topic so the dashboard stats page
//! can update tiles in real time without full-page refreshes.

use std::time::Duration;

use tracing::{debug, warn};

use crate::AppState;

const POLL_INTERVAL: Duration = Duration::from_secs(60);

pub fn spawn_poller(state: AppState) {
    let Some(percy) = state.config().percy.build_client(state.percy_client.clone()) else {
        return;
    };

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(POLL_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            interval.tick().await;

            match percy.get_bot_stats().await {
                Ok(stats) => {
                    let data = serde_json::to_value(&stats).unwrap_or_default();
                    state.live_publish("percy", data);
                    debug!("percy stats broadcast");
                }
                Err(e) => {
                    warn!("percy stats poll failed: {e}");
                }
            }
        }
    });
}
