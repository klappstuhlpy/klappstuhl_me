//! Shared web kernel for klappstuhl.me and the Percy dashboard.
//!
//! Holds the state-agnostic foundation both binaries build on. Everything here
//! is free of app-specific types (no `AppState`, `Config`, or domain models),
//! so it moves cleanly into the standalone `klappstuhl-shared` repo at Phase 5.
//! See DASHBOARD_DECOUPLING_PLAN.md (Phase 3).
//!
//! Modules:
//! - [`key`] — HMAC-SHA256 signing/verification over a 32-byte [`key::SecretKey`],
//!   plus hex helpers. Backs signed cookies, tokens, and TOTP secrets.

pub mod key;
