//! Placeholder for future server-push moderation notifications.
//!
//! The "moderation" WebSocket topic is registered and non-admin users can
//! subscribe to it.  Currently the audit log page does client-side polling
//! via the `/audit-log/recent` endpoint.  When Percy's internal API gains
//! WebSocket or webhook support, this module will poll/receive events and
//! publish them on the "moderation" topic via `state.live_publish`.

use crate::{percy::PercyClient, AppState};

/// No-op when Percy config is absent.  Reserves the background task slot for
/// future server-push integration.
pub fn spawn_poller(state: AppState) {
    let percy = {
        let config = state.config();
        PercyClient::new(state.percy_client.clone(), &config.percy)
    };
    if percy.is_none() {
        return;
    }
    // Future: spawn a tokio task that polls Percy for new moderation events
    // and publishes them via state.live_publish("moderation", ...).
    let _ = state;
}
