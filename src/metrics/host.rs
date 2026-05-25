//! Host metrics: parses /proc and /sys directly.
//!
//! Paths are configurable via the `HOST_PROC` / `HOST_SYS` env vars, so the
//! app can read the *host's* /proc when running inside a Docker container
//! (mount /proc:/host/proc:ro and set HOST_PROC=/host/proc).
//!
//! Everything here is async and uses tokio::fs. /proc reads are kernel-served
//! from memory so they are effectively instant; tokio::fs is mostly for
//! consistency with the rest of the codebase.

use anyhow::Context;
use serde::Serialize;
use std::path::PathBuf;
use tokio::process::Command;

/// One snapshot of host-wide metrics.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Sample {
    // CPU percentages (0..100, summed across all cores then averaged)
    pub cpu_user: f64,
    pub cpu_system: f64,
    pub cpu_iowait: f64,
    pub cpu_idle: f64,

    // Load averages (1-minute, 5-minute, 15-minute)
    pub load_1: f64,
    pub load_5: f64,
    pub load_15: f64,

    // Memory (bytes)
    pub mem_total: u64,
    pub mem_used: u64,
    pub mem_cached: u64,
    pub swap_total: u64,
    pub swap_used: u64,

    // Cumulative network counters (summed across non-loopback interfaces)
    pub net_rx_bytes: u64,
    pub net_tx_bytes: u64,

    // Temperatures (°C), if any sensors were found
    pub temp_max: Option<f64>,
    pub temp_avg: Option<f64>,

    // Root filesystem
    pub disk_total: u64,
    pub disk_used: u64,
}

impl Sample {
    /// Total CPU usage = 100 - idle.
    pub fn cpu_total_pct(&self) -> f64 {
        (100.0 - self.cpu_idle).max(0.0).min(100.0)
    }

    pub fn mem_used_pct(&self) -> f64 {
        if self.mem_total == 0 {
            0.0
        } else {
            self.mem_used as f64 / self.mem_total as f64 * 100.0
        }
    }

    pub fn disk_used_pct(&self) -> f64 {
        if self.disk_total == 0 {
            0.0
        } else {
            self.disk_used as f64 / self.disk_total as f64 * 100.0
        }
    }
}

fn proc_path() -> PathBuf {
    PathBuf::from(std::env::var("HOST_PROC").unwrap_or_else(|_| "/proc".into()))
}

fn sys_path() -> PathBuf {
    PathBuf::from(std::env::var("HOST_SYS").unwrap_or_else(|_| "/sys".into()))
}

/// Collects one full snapshot. CPU percentages are sampled across a brief
/// (~250ms) window because /proc/stat reports cumulative jiffies.
pub async fn collect() -> anyhow::Result<Sample> {
    let mut s = Sample::default();

    // CPU: two reads of /proc/stat with a small delay between
    let cpu1 = read_proc_stat().await?;
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    let cpu2 = read_proc_stat().await?;
    let cpu = cpu2.diff_percent(&cpu1);
    s.cpu_user = cpu.user;
    s.cpu_system = cpu.system;
    s.cpu_iowait = cpu.iowait;
    s.cpu_idle = cpu.idle;

    // Load average
    if let Ok((l1, l5, l15)) = read_loadavg().await {
        s.load_1 = l1;
        s.load_5 = l5;
        s.load_15 = l15;
    }

    // Memory
    if let Ok(mem) = read_meminfo().await {
        s.mem_total = mem.total;
        s.mem_used = mem.used;
        s.mem_cached = mem.cached;
        s.swap_total = mem.swap_total;
        s.swap_used = mem.swap_used;
    }

    // Network (sum of all non-loopback interfaces)
    if let Ok((rx, tx)) = read_net_dev().await {
        s.net_rx_bytes = rx;
        s.net_tx_bytes = tx;
    }

    // Temperatures (scan /sys/class/thermal/thermal_zone*)
    let (temp_max, temp_avg) = read_temperatures().await;
    s.temp_max = temp_max;
    s.temp_avg = temp_avg;

    // Disk usage of "/"
    if let Ok((total, used)) = root_disk_usage().await {
        s.disk_total = total;
        s.disk_used = used;
    }

    Ok(s)
}

// ─── /proc/stat: CPU jiffies ─────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
struct CpuJiffies {
    user: u64,
    nice: u64,
    system: u64,
    idle: u64,
    iowait: u64,
    irq: u64,
    softirq: u64,
    steal: u64,
}

#[derive(Debug, Clone, Copy, Default)]
struct CpuPercent {
    user: f64,
    system: f64,
    iowait: f64,
    idle: f64,
}

impl CpuJiffies {
    fn total(&self) -> u64 {
        self.user + self.nice + self.system + self.idle + self.iowait
            + self.irq + self.softirq + self.steal
    }

    fn diff_percent(&self, prev: &Self) -> CpuPercent {
        let total = self.total().saturating_sub(prev.total()) as f64;
        if total == 0.0 {
            return CpuPercent::default();
        }
        let pct = |a: u64, b: u64| (a.saturating_sub(b)) as f64 / total * 100.0;
        CpuPercent {
            user:   pct(self.user + self.nice, prev.user + prev.nice),
            system: pct(self.system + self.irq + self.softirq, prev.system + prev.irq + prev.softirq),
            iowait: pct(self.iowait, prev.iowait),
            idle:   pct(self.idle, prev.idle),
        }
    }
}

async fn read_proc_stat() -> anyhow::Result<CpuJiffies> {
    let text = tokio::fs::read_to_string(proc_path().join("stat"))
        .await
        .context("read /proc/stat")?;
    let first = text.lines().next().context("empty /proc/stat")?;
    // Format: "cpu  user nice system idle iowait irq softirq steal guest guest_nice"
    let mut iter = first.split_ascii_whitespace();
    let label = iter.next().unwrap_or("");
    if label != "cpu" {
        anyhow::bail!("unexpected /proc/stat first line: {first}");
    }
    let mut v = [0u64; 8];
    for slot in &mut v {
        if let Some(tok) = iter.next() {
            *slot = tok.parse().unwrap_or(0);
        }
    }
    Ok(CpuJiffies {
        user: v[0], nice: v[1], system: v[2], idle: v[3],
        iowait: v[4], irq: v[5], softirq: v[6], steal: v[7],
    })
}

// ─── /proc/loadavg ───────────────────────────────────────────────────────

async fn read_loadavg() -> anyhow::Result<(f64, f64, f64)> {
    let text = tokio::fs::read_to_string(proc_path().join("loadavg")).await?;
    let mut parts = text.split_ascii_whitespace();
    let l1: f64 = parts.next().context("loadavg short")?.parse()?;
    let l5: f64 = parts.next().context("loadavg short")?.parse()?;
    let l15: f64 = parts.next().context("loadavg short")?.parse()?;
    Ok((l1, l5, l15))
}

// ─── /proc/meminfo ───────────────────────────────────────────────────────

#[derive(Debug, Default)]
struct MemInfo {
    total: u64,
    used: u64,
    cached: u64,
    swap_total: u64,
    swap_used: u64,
}

async fn read_meminfo() -> anyhow::Result<MemInfo> {
    let text = tokio::fs::read_to_string(proc_path().join("meminfo")).await?;
    let mut total = 0u64;
    let mut available = 0u64;
    let mut buffers = 0u64;
    let mut cached = 0u64;
    let mut swap_total = 0u64;
    let mut swap_free = 0u64;
    for line in text.lines() {
        // Lines look like: "MemTotal:       16302944 kB"
        let mut parts = line.split_ascii_whitespace();
        let Some(key) = parts.next() else { continue };
        let Some(val_str) = parts.next() else { continue };
        let val: u64 = val_str.parse().unwrap_or(0) * 1024; // kB → bytes
        match key {
            "MemTotal:" => total = val,
            "MemAvailable:" => available = val,
            "Buffers:" => buffers = val,
            "Cached:" => cached = val,
            "SwapTotal:" => swap_total = val,
            "SwapFree:" => swap_free = val,
            _ => {}
        }
    }
    let used = total.saturating_sub(available);
    Ok(MemInfo {
        total,
        used,
        cached: cached + buffers,
        swap_total,
        swap_used: swap_total.saturating_sub(swap_free),
    })
}

// ─── /proc/net/dev ───────────────────────────────────────────────────────

async fn read_net_dev() -> anyhow::Result<(u64, u64)> {
    let text = tokio::fs::read_to_string(proc_path().join("net/dev")).await?;
    let mut rx_total = 0u64;
    let mut tx_total = 0u64;
    // First two lines are headers; each data line: "  eth0: 12345 ..."
    for line in text.lines().skip(2) {
        let Some((iface, rest)) = line.split_once(':') else { continue };
        let name = iface.trim();
        // Skip loopback and virtual docker bridges
        if name == "lo" || name.starts_with("docker") || name.starts_with("br-") || name.starts_with("veth") {
            continue;
        }
        let mut nums = rest.split_ascii_whitespace();
        let rx: u64 = nums.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        // Columns: rx_bytes rx_packets rx_errs rx_drop rx_fifo rx_frame
        //          rx_compressed rx_multicast tx_bytes ...
        let tx: u64 = nums.nth(7).and_then(|s| s.parse().ok()).unwrap_or(0);
        rx_total = rx_total.saturating_add(rx);
        tx_total = tx_total.saturating_add(tx);
    }
    Ok((rx_total, tx_total))
}

// ─── /sys/class/thermal/thermal_zone*/temp ───────────────────────────────

async fn read_temperatures() -> (Option<f64>, Option<f64>) {
    let dir = sys_path().join("class/thermal");
    let mut entries = match tokio::fs::read_dir(&dir).await {
        Ok(d) => d,
        Err(_) => return (None, None),
    };

    let mut temps_c: Vec<f64> = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with("thermal_zone") {
            continue;
        }
        let temp_path = entry.path().join("temp");
        if let Ok(s) = tokio::fs::read_to_string(&temp_path).await {
            if let Ok(millideg) = s.trim().parse::<i64>() {
                // Values are millidegrees Celsius
                let c = millideg as f64 / 1000.0;
                if (0.0..=150.0).contains(&c) {
                    temps_c.push(c);
                }
            }
        }
    }

    if temps_c.is_empty() {
        return (None, None);
    }
    let max = temps_c.iter().copied().fold(f64::MIN, f64::max);
    let avg = temps_c.iter().sum::<f64>() / temps_c.len() as f64;
    (Some(max), Some(avg))
}

// ─── df / for root filesystem usage ─────────────────────────────────────

async fn root_disk_usage() -> anyhow::Result<(u64, u64)> {
    // Use `df -B1 -P /` for POSIX-portable single-line output in bytes.
    let out = Command::new("df")
        .args(["-B1", "-P", "/"])
        .output()
        .await
        .context("running df")?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Expected:
    //   Filesystem    1B-blocks      Used    Available  Capacity  Mounted on
    //   /dev/sda1     50000000000   12345    49000000   25%       /
    let line = stdout.lines().nth(1).context("df output too short")?;
    let mut parts = line.split_ascii_whitespace();
    let _fs = parts.next();
    let total: u64 = parts.next().context("df: missing total")?.parse()?;
    let used: u64 = parts.next().context("df: missing used")?.parse()?;
    Ok((total, used))
}
