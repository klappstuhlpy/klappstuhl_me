/* ── Live metrics dashboard ──────────────────────────────────────
   - polls /admin/metrics/current every 5s for the tile row + container table
   - refetches /admin/metrics/history when the range picker changes (and on load)
   - renders four uPlot charts (CPU+Load, Memory, Network, Temperature)
   ──────────────────────────────────────────────────────────────── */

const LIVE_POLL_MS = 5_000;
let currentRange = "1h";

const charts = {};          // chart-id → uPlot instance
let lastNetRx = null;       // for live throughput calculation in tiles
let lastNetTs = null;

/* ── helpers ─────────────────────────────────────────────────── */

function fmtBytes(n) {
  if (n == null || !isFinite(n)) return "—";
  if (n === 0) return "0 B";
  const k = 1024;
  const sizes = ["B", "KiB", "MiB", "GiB", "TiB"];
  const i = Math.floor(Math.log(Math.abs(n)) / Math.log(k));
  return (n / Math.pow(k, i)).toFixed(i ? 1 : 0) + " " + sizes[i];
}

function fmtRate(bytesPerSec) {
  if (bytesPerSec == null || !isFinite(bytesPerSec)) return "—";
  return fmtBytes(bytesPerSec) + "/s";
}

function statusClassFor(pct, warn = 70, alert = 90) {
  if (pct == null) return "";
  if (pct >= alert) return "alert";
  if (pct >= warn)  return "warn";
  return "";
}

/* ── live tiles ──────────────────────────────────────────────── */

async function pollCurrent() {
  try {
    const res = await fetch("/admin/metrics/current");
    if (!res.ok) return;
    const data = await res.json();

    const host = data.host;
    if (host) {
      // CPU
      document.getElementById("tile-cpu-pct").textContent = host.cpu_total.toFixed(1);
      document.getElementById("tile-cpu-load").textContent =
        `load ${host.load_1.toFixed(2)} · ${host.load_5.toFixed(2)} · ${host.load_15.toFixed(2)}`;
      document.getElementById("tile-cpu-status").className = "tile-status " + statusClassFor(host.cpu_total);

      // Memory
      document.getElementById("tile-mem-pct").textContent = host.mem_used_pct.toFixed(1);
      document.getElementById("tile-mem-detail").textContent =
        `${fmtBytes(host.mem_used)} / ${fmtBytes(host.mem_total)}`;
      document.getElementById("tile-mem-status").className = "tile-status " + statusClassFor(host.mem_used_pct);

      // Disk
      document.getElementById("tile-disk-pct").textContent = host.disk_used_pct.toFixed(1);
      document.getElementById("tile-disk-detail").textContent =
        `${fmtBytes(host.disk_used)} / ${fmtBytes(host.disk_total)}`;
      document.getElementById("tile-disk-status").className = "tile-status " + statusClassFor(host.disk_used_pct, 80, 90);

      // Temperature
      const tMax = host.temp_max;
      const tAvg = host.temp_avg;
      document.getElementById("tile-temp-c").textContent = tMax == null ? "—" : tMax.toFixed(1);
      document.getElementById("tile-temp-detail").textContent = tAvg == null ? "no sensors" : `avg ${tAvg.toFixed(1)} °C`;
      document.getElementById("tile-temp-status").className = "tile-status " + statusClassFor(tMax, 70, 80);

      // Network throughput: delta vs previous poll
      if (lastNetRx != null && lastNetTs != null && host.ts > lastNetTs) {
        const dt = host.ts - lastNetTs;
        const rxRate = (host.net_rx_bytes - lastNetRx.rx) / dt;
        const txRate = (host.net_tx_bytes - lastNetRx.tx) / dt;
        document.getElementById("tile-net-rx").textContent = fmtRate(Math.max(0, rxRate));
        document.getElementById("tile-net-tx").textContent = fmtRate(Math.max(0, txRate));
      }
      lastNetRx = { rx: host.net_rx_bytes, tx: host.net_tx_bytes };
      lastNetTs = host.ts;
    }

    // Container table
    const tbody = document.querySelector("#container-table tbody");
    document.getElementById("container-count").textContent = data.containers.length;
    if (data.containers.length === 0) {
      tbody.innerHTML = '<tr><td colspan="4" class="muted">No running containers</td></tr>';
    } else {
      tbody.innerHTML = data.containers.map(c => {
        const memPct = c.mem_limit > 0 ? (c.mem_used / c.mem_limit * 100) : 0;
        return `<tr>
          <td>${escapeHtml(c.name)}</td>
          <td><div class="bar-cell"><span>${c.cpu_pct.toFixed(1)}%</span>
              <div class="bar ${statusClassFor(c.cpu_pct)}"><span style="width:${Math.min(c.cpu_pct, 100)}%"></span></div></div></td>
          <td><div class="bar-cell"><span>${fmtBytes(c.mem_used)} / ${fmtBytes(c.mem_limit)}</span>
              <div class="bar ${statusClassFor(memPct)}"><span style="width:${Math.min(memPct, 100)}%"></span></div></div></td>
          <td class="numeric">↓ ${fmtBytes(c.net_rx_bytes)} · ↑ ${fmtBytes(c.net_tx_bytes)}</td>
        </tr>`;
      }).join("");
    }
  } catch (e) {
    console.error("current poll failed:", e);
  }
}

function escapeHtml(s) {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

/* ── chart setup ─────────────────────────────────────────────── */

function chartSize(el) {
  const w = el.parentElement.clientWidth - 32;   // subtract panel padding
  return { width: Math.max(200, w), height: 220 };
}

function commonOpts(title, scales, series, el) {
  return {
    ...chartSize(el),
    cursor: { drag: { setScale: false } },
    legend: { live: true },
    scales,
    series,
    axes: [
      { stroke: "#71717a" },
      { stroke: "#71717a", grid: { stroke: "rgba(127,127,127,0.15)" } },
    ],
  };
}

function buildChart(id, opts) {
  const el = document.getElementById(id);
  el.innerHTML = "";
  const size = chartSize(el);
  charts[id] = new uPlot({ ...opts, ...size }, opts.data, el);
}

/* ── load history + render charts ─────────────────────────────── */

async function loadHistory() {
  try {
    const res = await fetch(`/admin/metrics/history?range=${encodeURIComponent(currentRange)}`);
    if (!res.ok) return;
    const data = await res.json();
    renderCharts(data.points || []);
  } catch (e) {
    console.error("history load failed:", e);
  }
}

function renderCharts(points) {
  if (!window.uPlot) return;

  const xs = points.map(p => p.ts);
  const cpu = points.map(p => p.cpu_total);
  const load = points.map(p => p.load_1);
  const mem = points.map(p => p.mem_used_pct);
  const disk = points.map(p => p.disk_used_pct);
  const temp = points.map(p => p.temp_max);

  // Network: convert cumulative byte counters into rate (bytes/s) using deltas
  const netRx = [];
  const netTx = [];
  for (let i = 0; i < points.length; i++) {
    if (i === 0) { netRx.push(null); netTx.push(null); continue; }
    const dt = points[i].ts - points[i-1].ts;
    if (dt <= 0) { netRx.push(null); netTx.push(null); continue; }
    const drx = Math.max(0, points[i].net_rx_bytes - points[i-1].net_rx_bytes) / dt;
    const dtx = Math.max(0, points[i].net_tx_bytes - points[i-1].net_tx_bytes) / dt;
    netRx.push(drx);
    netTx.push(dtx);
  }

  rebuildChart("chart-cpu", {
    data: [xs, cpu, load],
    series: [
      {},
      { label: "CPU %",   stroke: "#7c3aed", width: 1.5 },
      { label: "Load 1m", stroke: "#fbbf24", width: 1.5, scale: "load" },
    ],
    scales: { x: { time: true }, y: { range: [0, 100] }, load: { auto: true } },
    axes: [
      { stroke: "#71717a" },
      { stroke: "#71717a", grid: { stroke: "rgba(127,127,127,0.15)" }, values: (u, v) => v.map(x => x + "%") },
      { stroke: "#fbbf24", side: 1, scale: "load", grid: { show: false } },
    ],
  });

  rebuildChart("chart-mem", {
    data: [xs, mem, disk],
    series: [
      {},
      { label: "RAM %",  stroke: "#60a5fa", width: 1.5 },
      { label: "Disk %", stroke: "#a78bfa", width: 1.5 },
    ],
    scales: { x: { time: true }, y: { range: [0, 100] } },
    axes: [
      { stroke: "#71717a" },
      { stroke: "#71717a", grid: { stroke: "rgba(127,127,127,0.15)" }, values: (u, v) => v.map(x => x + "%") },
    ],
  });

  rebuildChart("chart-net", {
    data: [xs, netRx, netTx],
    series: [
      {},
      { label: "↓ Recv", stroke: "#86efac", width: 1.5, value: (u, v) => fmtRate(v) },
      { label: "↑ Send", stroke: "#f87171", width: 1.5, value: (u, v) => fmtRate(v) },
    ],
    scales: { x: { time: true } },
    axes: [
      { stroke: "#71717a" },
      { stroke: "#71717a", grid: { stroke: "rgba(127,127,127,0.15)" }, values: (u, v) => v.map(fmtRate) },
    ],
  });

  rebuildChart("chart-temp", {
    data: [xs, temp],
    series: [
      {},
      { label: "Max °C", stroke: "#fb923c", width: 1.5 },
    ],
    scales: { x: { time: true }, y: { auto: true } },
    axes: [
      { stroke: "#71717a" },
      { stroke: "#71717a", grid: { stroke: "rgba(127,127,127,0.15)" }, values: (u, v) => v.map(x => x + " °C") },
    ],
  });
}

function rebuildChart(id, cfg) {
  const el = document.getElementById(id);
  el.innerHTML = "";
  const { width, height } = chartSize(el);
  charts[id] = new uPlot({ width, height, ...cfg }, cfg.data, el);
}

/* ── range picker ────────────────────────────────────────────── */

document.querySelectorAll("#range-picker .button").forEach(btn => {
  btn.addEventListener("click", () => {
    document.querySelectorAll("#range-picker .button").forEach(b => b.classList.remove("active"));
    btn.classList.add("active");
    currentRange = btn.dataset.range;
    loadHistory();
  });
});

/* ── resize handler ──────────────────────────────────────────── */

let resizeTimer;
window.addEventListener("resize", () => {
  clearTimeout(resizeTimer);
  resizeTimer = setTimeout(() => loadHistory(), 200);
});

/* ── boot ────────────────────────────────────────────────────── */

pollCurrent();
loadHistory();
setInterval(pollCurrent, LIVE_POLL_MS);
