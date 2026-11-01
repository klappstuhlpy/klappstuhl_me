//! Cross-cutting fundamentals: configuration, app state, the database layer,
//! data models, error types, logging, CLI entry points, and shared utilities.

pub mod audit;
pub mod audit_store;
pub mod cli;
pub mod config;
pub mod database;
pub mod error;
pub mod filters;
pub mod logging;
pub mod migrations;
pub mod models;
pub mod state;
pub mod utils;
