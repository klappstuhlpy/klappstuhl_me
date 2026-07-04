//! Re-export of the shared flash-message machinery.
//!
//! The implementation — [`Flasher`], [`Flashes`], [`FlashMessage`], and the
//! [`process_flash_messages`] middleware — lives in the shared `kls-web-core`
//! crate (see DASHBOARD_DECOUPLING_PLAN.md, Phase 4) so both apps get identical,
//! HMAC-signed flash messaging. Re-exported here so `crate::flash::…` call sites
//! are unchanged.

pub use kls_web_core::flash::*;
