//! Percy internal-API client — re-exported from the extracted `percy-client`
//! crate.
//!
//! The client itself now lives in `crates/percy-client` (part of the dashboard
//! decoupling split — see DASHBOARD_DECOUPLING_PLAN.md). This module preserves
//! the historical `crate::percy::…` import paths so call sites are unchanged.
//! Construction from the app's config block is done via
//! [`crate::config::PercyConfig::build_client`].

pub use percy_client::*;
