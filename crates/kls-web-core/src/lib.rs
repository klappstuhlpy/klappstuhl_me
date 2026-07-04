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
//! - [`database`] — the async SQLite wrapper (thread-pool over blocking
//!   `rusqlite` connections), the [`database::Table`] row-mapping trait, and the
//!   [`boxed_params`] helper macro.
//! - [`migrations`] — the generic gapless-from-0 SQLite migration runner
//!   ([`migrations::run`]) plus [`migrations::EmbeddedMigration`]. The build-time
//!   discovery of a specific app's migration set stays in that app.

pub mod database;
pub mod key;
pub mod migrations;

pub use database::Database;
