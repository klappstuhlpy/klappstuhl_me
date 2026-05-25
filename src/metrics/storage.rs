//! Persistence layer for metrics: inserts into `metric_sample` / `docker_stat`,
//! and time-series fetches for the dashboard.

use crate::AppState;
use serde::Serialize;

use super::{DockerStat, Sample};

/// Insert one row into `metric_sample`.
///
/// Uses `Database::call` instead of `Database::execute` because the latter
/// requires `P: Send + 'static`, and `params!` produces `&[&dyn ToSql]` — the
/// `dyn ToSql` trait object is neither `Send` nor `Sync`.  A tuple wouldn't
/// help either (rusqlite only impls `Params` for tuples up to length 16; this
/// insert has 19 columns).  Running `params!` synchronously inside the worker
/// thread sidesteps the whole problem.
pub async fn insert_sample(state: &AppState, ts: i64, s: &Sample) -> rusqlite::Result<()> {
    let s = s.clone();
    state
        .database()
        .call(move |conn| -> rusqlite::Result<()> {
            let mut stmt = conn.prepare_cached(
                "INSERT OR REPLACE INTO metric_sample
                    (ts, cpu_user, cpu_system, cpu_iowait, cpu_idle,
                     load_1, load_5, load_15,
                     mem_total, mem_used, mem_cached, swap_total, swap_used,
                     net_rx_bytes, net_tx_bytes,
                     temp_max, temp_avg,
                     disk_total, disk_used)
                 VALUES (?,?,?,?,?, ?,?,?, ?,?,?,?,?, ?,?, ?,?, ?,?)",
            )?;
            stmt.execute(rusqlite::params![
                ts,
                s.cpu_user, s.cpu_system, s.cpu_iowait, s.cpu_idle,
                s.load_1, s.load_5, s.load_15,
                s.mem_total as i64, s.mem_used as i64, s.mem_cached as i64,
                s.swap_total as i64, s.swap_used as i64,
                s.net_rx_bytes as i64, s.net_tx_bytes as i64,
                s.temp_max, s.temp_avg,
                s.disk_total as i64, s.disk_used as i64,
            ])?;
            Ok(())
        })
        .await
}

/// Insert one row per container into `docker_stat` for the given timestamp.
pub async fn insert_docker_stats(
    state: &AppState,
    ts: i64,
    stats: &[DockerStat],
) -> rusqlite::Result<()> {
    // Move the data into a Vec we can ship to the worker thread
    let owned: Vec<DockerStat> = stats.to_vec();
    state
        .database()
        .call(move |conn| -> rusqlite::Result<()> {
            let tx = conn.transaction()?;
            {
                let mut stmt = tx.prepare_cached(
                    "INSERT OR REPLACE INTO docker_stat
                        (ts, container_name, cpu_pct, mem_used, mem_limit, net_rx_bytes, net_tx_bytes)
                     VALUES (?,?,?,?,?,?,?)",
                )?;
                for s in owned {
                    stmt.execute(rusqlite::params![
                        ts,
                        s.name,
                        s.cpu_pct,
                        s.mem_used as i64,
                        s.mem_limit as i64,
                        s.net_rx_bytes as i64,
                        s.net_tx_bytes as i64,
                    ])?;
                }
            }
            tx.commit()?;
            Ok(())
        })
        .await
}

/// Drops every sample older than `cutoff` from both tables.
pub async fn prune_older_than(state: &AppState, age: time::Duration) -> rusqlite::Result<()> {
    let cutoff = (time::OffsetDateTime::now_utc() - age).unix_timestamp();
    state
        .database()
        .execute("DELETE FROM metric_sample WHERE ts < ?", [cutoff])
        .await?;
    state
        .database()
        .execute("DELETE FROM docker_stat WHERE ts < ?", [cutoff])
        .await?;
    Ok(())
}

// ─── Dashboard reads ─────────────────────────────────────────────────────

/// Returns the most recent host sample, or None if nothing has been scraped yet.
pub async fn fetch_current(state: &AppState) -> Option<CurrentView> {
    state
        .database()
        .get_row(
            "SELECT * FROM metric_sample ORDER BY ts DESC LIMIT 1",
            [],
            CurrentView::from_row,
        )
        .await
        .ok()
}

/// One row returned by the /history endpoint (timestamp + every numeric field).
#[derive(Debug, Serialize, Clone)]
pub struct HistoryPoint {
    pub ts: i64,
    pub cpu_total: f64,
    pub mem_used_pct: f64,
    pub net_rx_bytes: i64,
    pub net_tx_bytes: i64,
    pub temp_max: Option<f64>,
    pub disk_used_pct: f64,
    pub load_1: f64,
}

/// Returns history points within the last `seconds` seconds, oldest first.
pub async fn fetch_history(state: &AppState, seconds: i64) -> rusqlite::Result<Vec<HistoryPoint>> {
    let since = time::OffsetDateTime::now_utc().unix_timestamp() - seconds;
    state
        .database()
        .call(move |conn| -> rusqlite::Result<Vec<HistoryPoint>> {
            let mut stmt = conn.prepare_cached(
                "SELECT ts, cpu_idle, mem_used, mem_total, net_rx_bytes, net_tx_bytes,
                        temp_max, disk_total, disk_used, load_1
                 FROM metric_sample
                 WHERE ts >= ?
                 ORDER BY ts ASC",
            )?;
            let rows = stmt.query_map([since], |row| {
                let cpu_idle: f64 = row.get(1)?;
                let mem_used: i64 = row.get(2)?;
                let mem_total: i64 = row.get(3)?;
                let disk_total: i64 = row.get(7)?;
                let disk_used: i64 = row.get(8)?;
                Ok(HistoryPoint {
                    ts: row.get(0)?,
                    cpu_total: (100.0 - cpu_idle).max(0.0).min(100.0),
                    mem_used_pct: if mem_total > 0 { mem_used as f64 / mem_total as f64 * 100.0 } else { 0.0 },
                    net_rx_bytes: row.get(4)?,
                    net_tx_bytes: row.get(5)?,
                    temp_max: row.get(6)?,
                    disk_used_pct: if disk_total > 0 { disk_used as f64 / disk_total as f64 * 100.0 } else { 0.0 },
                    load_1: row.get(9)?,
                })
            })?;
            rows.collect()
        })
        .await
}

/// One point in a container's history.
#[derive(Debug, Serialize, Clone)]
pub struct DockerHistoryPoint {
    pub ts: i64,
    pub cpu_pct: f64,
    pub mem_pct: f64,
}

/// Returns per-container history grouped by container name.
pub async fn fetch_docker_history(
    state: &AppState,
    seconds: i64,
) -> rusqlite::Result<std::collections::BTreeMap<String, Vec<DockerHistoryPoint>>> {
    let since = time::OffsetDateTime::now_utc().unix_timestamp() - seconds;
    state
        .database()
        .call(move |conn| -> rusqlite::Result<_> {
            let mut stmt = conn.prepare_cached(
                "SELECT ts, container_name, cpu_pct, mem_used, mem_limit
                 FROM docker_stat
                 WHERE ts >= ?
                 ORDER BY container_name ASC, ts ASC",
            )?;
            let mut out: std::collections::BTreeMap<String, Vec<DockerHistoryPoint>> = Default::default();
            let mut rows = stmt.query([since])?;
            while let Some(row) = rows.next()? {
                let ts: i64 = row.get(0)?;
                let name: String = row.get(1)?;
                let cpu_pct: f64 = row.get(2)?;
                let mem_used: i64 = row.get(3)?;
                let mem_limit: i64 = row.get(4)?;
                let mem_pct = if mem_limit > 0 { mem_used as f64 / mem_limit as f64 * 100.0 } else { 0.0 };
                out.entry(name).or_default().push(DockerHistoryPoint { ts, cpu_pct, mem_pct });
            }
            Ok(out)
        })
        .await
}

/// The shape returned by /admin/metrics/current — includes the live container
/// list (so the dashboard tile + table don't need a separate query).
#[derive(Debug, Serialize)]
pub struct CurrentView {
    pub ts: i64,
    pub cpu_user: f64,
    pub cpu_system: f64,
    pub cpu_iowait: f64,
    pub cpu_idle: f64,
    pub cpu_total: f64,
    pub load_1: f64,
    pub load_5: f64,
    pub load_15: f64,
    pub mem_total: i64,
    pub mem_used: i64,
    pub mem_cached: i64,
    pub mem_used_pct: f64,
    pub swap_total: i64,
    pub swap_used: i64,
    pub net_rx_bytes: i64,
    pub net_tx_bytes: i64,
    pub temp_max: Option<f64>,
    pub temp_avg: Option<f64>,
    pub disk_total: i64,
    pub disk_used: i64,
    pub disk_used_pct: f64,
}

impl CurrentView {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        let cpu_idle: f64 = row.get("cpu_idle")?;
        let mem_total: i64 = row.get("mem_total")?;
        let mem_used: i64 = row.get("mem_used")?;
        let disk_total: i64 = row.get("disk_total")?;
        let disk_used: i64 = row.get("disk_used")?;
        Ok(Self {
            ts: row.get("ts")?,
            cpu_user: row.get("cpu_user")?,
            cpu_system: row.get("cpu_system")?,
            cpu_iowait: row.get("cpu_iowait")?,
            cpu_idle,
            cpu_total: (100.0 - cpu_idle).max(0.0).min(100.0),
            load_1: row.get("load_1")?,
            load_5: row.get("load_5")?,
            load_15: row.get("load_15")?,
            mem_total,
            mem_used,
            mem_cached: row.get("mem_cached")?,
            mem_used_pct: if mem_total > 0 { mem_used as f64 / mem_total as f64 * 100.0 } else { 0.0 },
            swap_total: row.get("swap_total")?,
            swap_used: row.get("swap_used")?,
            net_rx_bytes: row.get("net_rx_bytes")?,
            net_tx_bytes: row.get("net_tx_bytes")?,
            temp_max: row.get("temp_max")?,
            temp_avg: row.get("temp_avg")?,
            disk_total,
            disk_used,
            disk_used_pct: if disk_total > 0 { disk_used as f64 / disk_total as f64 * 100.0 } else { 0.0 },
        })
    }
}
