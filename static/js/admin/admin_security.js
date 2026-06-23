/* ── Security dashboard ───────────────────────────────────────────
   Renders the timeline area chart, top-IPs table, reason breakdown,
   country distribution, recent activity feed, and the optional
   Cloudflare panels.
   ──────────────────────────────────────────────────────────────── */

const FLAGS = window.SECURITY_FLAGS || { geoipEnabled: false, cloudflareEnabled: false };
let currentRange = "24h";

/* ── helpers ─────────────────────────────────────────────────── */

function escapeHtml(s) {
    if (s == null) return "";
    return String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

function fmtRelative(unixSecs) {
    const diff = Math.max(0, Math.floor(Date.now() / 1000 - unixSecs));
    if (diff < 60)   return diff + "s ago";
    if (diff < 3600) return Math.floor(diff / 60) + "m ago";
    if (diff < 86400) return Math.floor(diff / 3600) + "h ago";
    return Math.floor(diff / 86400) + "d ago";
}

function fmtBytes(n) {
    if (n == null || !isFinite(n)) return "—";
    const k = 1024, sizes = ["B","KiB","MiB","GiB","TiB","PiB"];
    const i = Math.floor(Math.log(Math.max(1, Math.abs(n))) / Math.log(k));
    return (n / Math.pow(k, i)).toFixed(i ? 1 : 0) + " " + sizes[i];
}

function fmtNumber(n) {
    if (n == null) return "—";
    return n.toLocaleString();
}

/** Two-letter ISO → UTF-8 regional-indicator flag emoji. */
function flagEmoji(cc) {
    if (!cc || cc.length !== 2) return "";
    const A = 0x1F1E6;
    const codes = [...cc.toUpperCase()].map(c => A + (c.charCodeAt(0) - 65));
    return String.fromCodePoint(...codes);
}

function reasonPillClass(reason) {
    if (reason === "Incorrect Login") return "failed-login";
    if (reason === "Rate Limited")    return "rate-limited";
    return "";
}

/* ── range picker ────────────────────────────────────────────── */

document.querySelectorAll("#range-picker .button").forEach(btn => {
    btn.addEventListener("click", () => {
        document.querySelectorAll("#range-picker .button").forEach(b => b.classList.remove("active"));
        btn.classList.add("active");
        currentRange = btn.dataset.range;
        loadAll();
    });
});

/* ── load + render security data ─────────────────────────────── */

async function loadAll() {
    await Promise.all([loadAppData(), FLAGS.cloudflareEnabled ? loadCloudflare() : null]);
}

async function loadAppData() {
    const res = await fetch(`/admin/security/data?range=${encodeURIComponent(currentRange)}`);
    if (!res.ok) return;
    const data = await res.json();

    // Tiles
    document.getElementById("tile-failed-logins").textContent = fmtNumber(data.totals.failed_logins);
    document.getElementById("tile-rate-limited").textContent  = fmtNumber(data.totals.rate_limited);
    document.getElementById("tile-bad-requests").textContent  = fmtNumber(data.totals.bad_requests);
    document.getElementById("tile-unique-ips").textContent    = fmtNumber(data.totals.unique_ips);

    renderTimeline(data.timeline);
    renderTopIps(data.top_ips);
    renderReasonBreakdown(data.reason_breakdown);
    renderCountryDistribution(data.country_distribution);
    renderRecent(data.recent);
}

/* ── timeline chart ──────────────────────────────────────────── */

let timelineChart = null;

function chartSize(el) {
    const w = el.parentElement.clientWidth - 32;
    return { width: Math.max(200, w), height: 240 };
}

function renderTimeline(buckets) {
    const el = document.getElementById("chart-timeline");
    el.innerHTML = "";
    if (!buckets || buckets.length === 0) {
        el.innerHTML = '<div class="muted" style="padding: 1.5rem 0;">No 4xx activity in this window.</div>';
        return;
    }
    const xs = buckets.map(b => b.ts);
    const failed = buckets.map(b => b.failed_logins);
    const rl     = buckets.map(b => b.rate_limited);
    const other  = buckets.map(b => Math.max(0, b.bad_requests - b.failed_logins - b.rate_limited));

    const { width, height } = chartSize(el);
    timelineChart = new uPlot({
        width, height,
        legend: { live: true },
        scales: { x: { time: true } },
        axes: [
            { stroke: "#71717a" },
            { stroke: "#71717a", grid: { stroke: "rgba(127,127,127,0.15)" } },
        ],
        series: [
            {},
            { label: "Failed logins", stroke: "#f87171", fill: "rgba(248,113,113,0.18)", width: 1.5 },
            { label: "Rate limited",  stroke: "#fbbf24", fill: "rgba(251,191,36,0.18)",  width: 1.5 },
            { label: "Other 4xx",     stroke: "#a78bfa", fill: "rgba(167,139,250,0.18)", width: 1.5 },
        ],
    }, [xs, failed, rl, other], el);
}

/* ── top IPs table ───────────────────────────────────────────── */

function renderTopIps(rows) {
    const tbody = document.querySelector("#top-ips tbody");
    if (!rows || rows.length === 0) {
        tbody.innerHTML = `<tr><td colspan="${FLAGS.geoipEnabled ? 3 : 2}" class="muted">No data</td></tr>`;
        return;
    }
    tbody.innerHTML = rows.map(r => {
        const country = r.country ? `${flagEmoji(r.country_code)} ${escapeHtml(r.country)}` : "—";
        const city = r.city ? `<div class="muted" style="font-size:0.75rem">${escapeHtml(r.city)}</div>` : "";
        return `<tr>
            <td data-label="IP"><code>${escapeHtml(r.ip)}</code></td>
            ${FLAGS.geoipEnabled ? `<td data-label="Country">${country}${city}</td>` : ""}
            <td data-label="4xx" class="numeric">${fmtNumber(r.count)}</td>
        </tr>`;
    }).join("");
}

/* ── reason breakdown ────────────────────────────────────────── */

function renderReasonBreakdown(rows) {
    const el = document.getElementById("reason-list");
    if (!rows || rows.length === 0) {
        el.innerHTML = '<div class="muted">No 4xx in window.</div>';
        return;
    }
    const max = Math.max(...rows.map(r => r.count));
    el.innerHTML = rows.map(r => {
        const cls = r.reason === "Incorrect Login" ? "error"
                  : r.reason === "Rate Limited" ? "warn"
                  : "info";
        return `<div class="reason-row ${cls}">
            <span class="label">${escapeHtml(r.reason)}</span>
            <div class="bar"><span style="width:${(r.count / max * 100).toFixed(1)}%"></span></div>
            <span class="count">${fmtNumber(r.count)}</span>
        </div>`;
    }).join("");
}

/* ── country distribution ────────────────────────────────────── */

function renderCountryDistribution(rows) {
    if (!FLAGS.geoipEnabled) return;
    const el = document.getElementById("country-list");
    if (!el) return;
    if (!rows || rows.length === 0) {
        el.innerHTML = '<div class="muted">No geolocated requests.</div>';
        return;
    }
    const max = Math.max(...rows.map(r => r.count));
    el.innerHTML = rows.map(r => `
        <div class="country-row">
            <span class="label"><span class="flag">${flagEmoji(r.country_code)}</span>${escapeHtml(r.country) || r.country_code}</span>
            <div class="bar"><span style="width:${(r.count / max * 100).toFixed(1)}%"></span></div>
            <span class="count">${fmtNumber(r.count)}</span>
        </div>`).join("");
}

/* ── recent events feed ──────────────────────────────────────── */

function renderRecent(rows) {
    const tbody = document.querySelector("#recent-events tbody");
    if (!rows || rows.length === 0) {
        tbody.innerHTML = `<tr><td colspan="6" class="muted">Nothing recent</td></tr>`;
        return;
    }
    tbody.innerHTML = rows.map(r => {
        const cc = r.country_code ? `<td data-label="Country">${flagEmoji(r.country_code)}</td>` : (FLAGS.geoipEnabled ? `<td data-label="Country"></td>` : "");
        const statusCls = r.status_code >= 500 ? "s5xx" : "s4xx";
        return `<tr>
            <td data-label="When">${window.tsHtml(new Date(r.ts * 1000).toISOString())}</td>
            <td data-label="IP"><code>${escapeHtml(r.ip || "—")}</code></td>
            ${cc}
            <td data-label="Status"><span class="status-pill ${statusCls}">${r.status_code}</span></td>
            <td data-label="Reason"><span class="reason-pill ${reasonPillClass(r.reason)}">${escapeHtml(r.reason)}</span></td>
            <td data-label="Path"><code class="muted" style="font-size:0.78rem">${escapeHtml(r.path)}</code></td>
        </tr>`;
    }).join("");
}

/* ── Cloudflare panels ───────────────────────────────────────── */

async function loadCloudflare() {
    try {
        const res = await fetch(`/admin/security/cloudflare?range=${encodeURIComponent(currentRange)}`);
        if (!res.ok) {
            document.getElementById("cf-section")?.classList.add("hidden");
            return;
        }
        const data = await res.json();
        renderCfTiles(data.summary);
        renderCfChart(data.summary.series);
        renderCfEvents(data.events);
    } catch (e) {
        console.error("cloudflare load failed:", e);
    }
}

function renderCfTiles(s) {
    document.getElementById("cf-requests").textContent = fmtNumber(s.total_requests);
    const cachedPct = s.total_requests > 0 ? (s.cached_requests / s.total_requests * 100) : 0;
    document.getElementById("cf-cached").textContent   = cachedPct.toFixed(1);
    document.getElementById("cf-threats").textContent  = fmtNumber(s.threats);
    document.getElementById("cf-bytes").textContent    = fmtBytes(s.bytes);
}

function renderCfChart(series) {
    const el = document.getElementById("chart-cf");
    if (!el) return;
    el.innerHTML = "";
    if (!series || series.length === 0) {
        el.innerHTML = '<div class="muted" style="padding:1.5rem 0;">No Cloudflare data.</div>';
        return;
    }
    const xs = series.map(p => p.ts);
    const requests = series.map(p => p.requests);
    const threats  = series.map(p => p.threats);
    const { width, height } = chartSize(el);
    new uPlot({
        width, height,
        legend: { live: true },
        scales: { x: { time: true } },
        axes: [
            { stroke: "#71717a" },
            { stroke: "#71717a", grid: { stroke: "rgba(127,127,127,0.15)" } },
        ],
        series: [
            {},
            { label: "Requests", stroke: "#60a5fa", width: 1.5 },
            { label: "Threats",  stroke: "#f87171", width: 1.5 },
        ],
    }, [xs, requests, threats], el);
}

function renderCfEvents(events) {
    const tbody = document.querySelector("#cf-events tbody");
    if (!events || events.length === 0) {
        tbody.innerHTML = `<tr><td colspan="6" class="muted">No WAF events</td></tr>`;
        return;
    }
    tbody.innerHTML = events.map(e => `
        <tr>
            <td data-label="When">${window.tsHtml(new Date(e.ts * 1000).toISOString())}</td>
            <td data-label="Action"><span class="reason-pill ${e.action === "block" ? "failed-login" : ""}">${escapeHtml(e.action)}</span></td>
            <td data-label="Source">${escapeHtml(e.source)}</td>
            <td data-label="Country">${escapeHtml(e.country)}</td>
            <td data-label="IP"><code>${escapeHtml(e.client_ip)}</code></td>
            <td data-label="Path"><code class="muted" style="font-size:0.78rem">${escapeHtml(e.uri)}</code></td>
        </tr>`).join("");
}

/* ── boot ────────────────────────────────────────────────────── */

let resizeTimer;
window.addEventListener("resize", () => {
    clearTimeout(resizeTimer);
    resizeTimer = setTimeout(loadAll, 200);
});

loadAll();
