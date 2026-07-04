//! Re-export of the shared async SQLite kernel plus the app-specific data-path
//! helper.
//!
//! The generic database machinery — [`Database`], [`DatabaseBuilder`],
//! [`Transaction`], the [`Table`] row-mapping trait, [`is_unique_constraint_violation`],
//! and the [`boxed_params`] macro — lives in the shared `kls-web-core` crate so
//! the Percy dashboard binary can link the exact same wrapper (see
//! DASHBOARD_DECOUPLING_PLAN.md, Phase 3). Everything is re-exported here so
//! existing `crate::database::…` call sites are unchanged. The `boxed_params`
//! macro is re-exported at the crate root from `lib.rs`.
//!
//! Only [`directory`] stays app-side: it hard-codes this application's data
//! directory layout (`<data>/<PROGRAM_NAME>/main.db`), which is not shared.

use std::path::PathBuf;

pub use kls_web_core::database::*;

/// Returns (and creates) the directory for the main.db file
pub fn directory() -> anyhow::Result<PathBuf> {
    use anyhow::Context;

    let mut path = dirs::data_dir().context("could not find a data directory for the current user")?;
    path.push(crate::PROGRAM_NAME);
    // create_dir_all is idempotent (no error if it already exists) and tolerates a
    // missing intermediate data dir on first run.
    std::fs::create_dir_all(&path).context("could not create application local data directory")?;
    path.push("main.db");
    Ok(path)
}
