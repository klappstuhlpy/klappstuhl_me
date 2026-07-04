//! Generic, gapless-from-0 SQLite schema migration runner.
//!
//! This is the reusable engine both databases in the app drive: `main.db` (whose
//! migration set is embedded at build time) and the request log (an inline set in
//! `logging.rs`). It is deliberately free of any app-specific migration list — the
//! caller supplies the `&[EmbeddedMigration]` and a tracking-table name — so it
//! moves cleanly into the shared kls-web-core crate (see
//! DASHBOARD_DECOUPLING_PLAN.md, Phase 3). The build-time discovery of `main.db`'s
//! migrations (`build.rs` → `EMBEDDED_MIGRATIONS`) and the `migrate` entry point
//! stay app-side.
//!
//! # How versioning works
//!
//! The runner is the single source of truth for a database's schema version. It
//! tracks progress two ways, kept in lock-step:
//!
//! * `PRAGMA user_version` — the fast gate. It equals the number of applied
//!   migrations, i.e. the version of the *next* migration to apply. Because the
//!   set is contiguous from `0`, "the next migration" is always the one whose
//!   [`EmbeddedMigration::version`] equals the current `user_version`.
//! * a tracking table (name supplied by the caller) — one row per applied
//!   migration recording its SHA-256 checksum and timestamp. This powers the
//!   integrity check.
//!
//! The runner owns `user_version` entirely; the `.sql` files no longer set it.
//!
//! # Integrity checking
//!
//! Every already-applied migration is re-hashed on startup and compared against
//! the checksum recorded when it was applied. If a migration is edited after it
//! has been applied to a database, the checksums diverge and the runner refuses
//! to start — surfacing schema drift loudly instead of silently running a database
//! whose history no longer matches its files.
//!
//! Databases that predate the tracking table (tracked only by `user_version`) are
//! *adopted* on first run: their already-applied migrations are recorded at the
//! current on-disk checksum, establishing a trusted baseline.
//!
//! # Concurrency
//!
//! The connection pool opens N connections in parallel and may run [`run`] on
//! every one of them. All writes happen inside `IMMEDIATE` transactions, so the
//! connections serialise: the first to win the write lock applies the pending
//! migrations; the rest observe the bumped `user_version` and no-op. Idempotency of
//! the individual migrations is therefore not required for correctness, but is
//! still good practice.

use rusqlite::OptionalExtension;
use sha2::{Digest, Sha256};
use tracing::{debug, info};

/// A migration embedded into the binary at build time.
///
/// For `main.db` these are constructed by the array `build.rs` generates; other
/// databases (e.g. the request log) construct them inline.
pub struct EmbeddedMigration {
    /// The migration's version, parsed from its file name (`<version>.sql`).
    pub version: u32,
    /// The raw SQL of the migration.
    pub sql: &'static str,
}

/// SHA-256 of a migration's SQL, rendered as lowercase hex.
fn checksum(sql: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(sql.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Reads `PRAGMA user_version`, clamping the (theoretically signed) value to a
/// sane unsigned version number.
pub fn user_version(conn: &rusqlite::Connection) -> rusqlite::Result<u32> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    Ok(version.max(0) as u32)
}

/// Runs an arbitrary, gapless-from-0 migration set against a connection, recording
/// progress in `tracking_table`, and verifies the integrity of already-applied
/// migrations.
///
/// This is safe to hand to a connection pool's `with_init`: it assumes the pool
/// has already configured connection-level pragmas (`busy_timeout`,
/// `journal_mode=WAL`, …) so concurrent invocations serialise on the write lock
/// rather than failing with `SQLITE_BUSY`.
pub fn run(
    conn: &mut rusqlite::Connection,
    migrations: &[EmbeddedMigration],
    tracking_table: &str,
) -> anyhow::Result<()> {
    // Guard the contiguity invariant the runner relies on. build.rs enforces this for
    // the embedded set at compile time; this also covers any inline set.
    for (index, migration) in migrations.iter().enumerate() {
        anyhow::ensure!(
            migration.version as usize == index,
            "migration set is not contiguous from 0: position {index} holds version {}",
            migration.version
        );
    }

    // `IF NOT EXISTS` on an existing table is a cheap read-only no-op, so this does
    // not take a write lock on the steady-state startup path.
    conn.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS {tracking_table} (
            version    INTEGER PRIMARY KEY,
            checksum   TEXT    NOT NULL,
            applied_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
        );"
    ))?;

    verify_and_adopt(conn, migrations, tracking_table)?;
    apply_pending(conn, migrations, tracking_table)?;
    Ok(())
}

/// Verifies the checksums of already-applied migrations and adopts legacy databases
/// that have no tracking rows yet.
///
/// Integrity verification is read-only, so a healthy, fully-migrated database takes
/// **no** write lock here — important because every pooled connection runs this on
/// startup. A write transaction is opened only when there are tracking rows to
/// backfill (a one-time event when adopting a pre-tracking database).
fn verify_and_adopt(
    conn: &mut rusqlite::Connection,
    migrations: &[EmbeddedMigration],
    tracking_table: &str,
) -> anyhow::Result<()> {
    let current = user_version(conn)?;

    // Read-only pass. Already-applied migrations are exactly those with
    // version < current; contiguity means `take_while` stops at the first pending one.
    let mut to_adopt: Vec<(u32, String)> = Vec::new();
    for migration in migrations.iter().take_while(|m| m.version < current) {
        let digest = checksum(migration.sql);
        let recorded: Option<String> = conn
            .query_row(
                &format!("SELECT checksum FROM {tracking_table} WHERE version = ?"),
                [migration.version],
                |row| row.get(0),
            )
            .optional()?;

        match recorded {
            // This database predates the tracking table — record it for adoption.
            None => to_adopt.push((migration.version, digest)),
            Some(stored) if stored != digest => {
                anyhow::bail!(
                    "migration integrity check failed: migration {} was modified after being applied \
                     (recorded checksum {}…, current {}…). Refusing to start to avoid schema drift — \
                     restore the original file or create a new migration instead of editing this one.",
                    migration.version,
                    short(&stored),
                    short(&digest),
                );
            }
            Some(_) => {}
        }
    }

    if to_adopt.is_empty() {
        return Ok(());
    }

    // Backfill the missing tracking rows. `INSERT OR IGNORE` inside a single
    // `IMMEDIATE` transaction makes this safe when several pooled connections race to
    // adopt the same legacy database — the loser's inserts are simply ignored.
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    for (version, digest) in &to_adopt {
        tx.execute(
            &format!("INSERT OR IGNORE INTO {tracking_table}(version, checksum) VALUES (?, ?)"),
            rusqlite::params![version, digest],
        )?;
    }
    tx.commit()?;
    debug!(count = to_adopt.len(), "adopted previously-applied migrations");
    Ok(())
}

/// Applies every migration whose version is `>= user_version`, one transaction per
/// migration so a failure leaves earlier migrations committed and resumable.
fn apply_pending(
    conn: &mut rusqlite::Connection,
    migrations: &[EmbeddedMigration],
    tracking_table: &str,
) -> anyhow::Result<()> {
    let target = migrations.len() as u32;
    let start = user_version(conn)?;
    if target.saturating_sub(start) == 0 {
        debug!(version = start, "database schema is up to date");
        return Ok(());
    }
    info!(
        from = start,
        to = target,
        count = target - start,
        "applying database migrations"
    );

    loop {
        // Re-read inside the transaction so pooled connections racing here observe
        // each other's committed progress and never apply the same migration twice.
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current = user_version(&tx)?;

        // Contiguity guarantees the next migration to apply has version == current.
        let Some(migration) = migrations.iter().find(|m| m.version == current) else {
            tx.commit()?;
            break;
        };

        tx.execute_batch(migration.sql)?;
        tx.execute(
            &format!("INSERT OR REPLACE INTO {tracking_table}(version, checksum) VALUES (?, ?)"),
            rusqlite::params![migration.version, checksum(migration.sql)],
        )?;
        // The runner owns user_version; PRAGMA values can't be bound, but `current`
        // is a trusted integer read from SQLite itself, so the format is safe.
        tx.execute_batch(&format!("PRAGMA user_version = {};", current + 1))?;
        tx.commit()?;
        debug!(version = migration.version, "applied migration");
    }

    info!(version = target, "database migrations complete");
    Ok(())
}

/// First 12 chars of a checksum for compact log/error display.
fn short(checksum: &str) -> &str {
    &checksum[..checksum.len().min(12)]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small, self-contained migration set so the generic runner can be tested
    /// without any app-supplied `EMBEDDED_MIGRATIONS`.
    const TEST_MIGRATIONS: [EmbeddedMigration; 2] = [
        EmbeddedMigration {
            version: 0,
            sql: "CREATE TABLE a(id INTEGER PRIMARY KEY);",
        },
        EmbeddedMigration {
            version: 1,
            sql: "CREATE TABLE b(id INTEGER PRIMARY KEY);",
        },
    ];

    const TRACKING_TABLE: &str = "schema_migrations";

    fn open_memory() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().expect("open in-memory db");
        conn.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
        conn
    }

    fn migrate(conn: &mut rusqlite::Connection) -> anyhow::Result<()> {
        run(conn, &TEST_MIGRATIONS, TRACKING_TABLE)
    }

    #[test]
    fn run_rejects_non_contiguous_set() {
        let mut conn = open_memory();
        let broken = [EmbeddedMigration {
            version: 1,
            sql: "SELECT 1;",
        }];
        let err = run(&mut conn, &broken, TRACKING_TABLE).expect_err("non-contiguous set must be rejected");
        assert!(err.to_string().contains("not contiguous"), "got: {err}");
    }

    #[test]
    fn migrate_fresh_database_reaches_target() {
        let mut conn = open_memory();
        migrate(&mut conn).expect("migrate fresh db");
        assert_eq!(user_version(&conn).unwrap(), TEST_MIGRATIONS.len() as u32);

        // Every migration recorded with a checksum.
        let count: u32 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {TRACKING_TABLE}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, TEST_MIGRATIONS.len() as u32);
    }

    #[test]
    fn migrate_is_idempotent() {
        let mut conn = open_memory();
        migrate(&mut conn).expect("first migrate");
        // A second run must be a no-op and must not error.
        migrate(&mut conn).expect("second migrate");
        assert_eq!(user_version(&conn).unwrap(), TEST_MIGRATIONS.len() as u32);
    }

    #[test]
    fn integrity_check_detects_tampering() {
        let mut conn = open_memory();
        migrate(&mut conn).expect("migrate");

        // Simulate a migration file having been edited after it was applied by
        // corrupting its recorded checksum.
        conn.execute(
            &format!("UPDATE {TRACKING_TABLE} SET checksum = 'deadbeef' WHERE version = 0"),
            [],
        )
        .unwrap();

        let err = migrate(&mut conn).expect_err("tampering should be rejected");
        assert!(err.to_string().contains("integrity check failed"), "got: {err}");
    }

    #[test]
    fn adopts_legacy_database_without_tracking_table() {
        let mut conn = open_memory();
        // Simulate a pre-tracking database: schema applied, user_version set, but no
        // tracking table.
        migrate(&mut conn).expect("seed schema");
        conn.execute_batch(&format!("DROP TABLE {TRACKING_TABLE};")).unwrap();

        // Re-running must adopt cleanly and backfill the tracking rows.
        migrate(&mut conn).expect("adopt legacy db");
        let count: u32 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {TRACKING_TABLE}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, TEST_MIGRATIONS.len() as u32);
    }
}
