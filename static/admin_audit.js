/* ── Audit log dashboard ─────────────────────────────────────────
   Loads /admin/audit/data with optional action / actor filters,
   renders tile counts + a paginated table.
   ───────────────────────────────────────────────────────────────── */

function escapeHtml(s) {
  if (s == null) return "";
  return String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

function fmtRelative(iso) {
  if (!iso) return "—";
  const t = new Date(iso).getTime();
  if (!isFinite(t)) return "—";
  const diff = Math.max(0, Math.floor((Date.now() - t) / 1000));
  if (diff < 60)   return diff + "s ago";
  if (diff < 3600) return Math.floor(diff / 60) + "m ago";
  if (diff < 86400) return Math.floor(diff / 3600) + "h ago";
  return Math.floor(diff / 86400) + "d ago";
}

function fmtNumber(n) { return (n ?? 0).toLocaleString(); }

/** Returns a CSS class for the action pill based on namespace prefix. */
function actionClass(action) {
  if (action === "auth.login.fail") return "auth-fail";
  if (action.startsWith("auth.")) return "auth";
  if (action.startsWith("service.")) return "service";
  if (action.startsWith("invite.")) return "invite";
  if (action.startsWith("secret.")) return "secret";
  return "";
}

let currentAction = "";
let currentActor  = "";

async function loadData() {
  const params = new URLSearchParams();
  if (currentAction) params.set("action", currentAction);
  if (currentActor)  params.set("actor", currentActor);
  const url = `/admin/audit/data${params.toString() ? "?" + params : ""}`;
  const res = await fetch(url);
  if (!res.ok) return;
  const data = await res.json();
  renderTiles(data.counts);
  renderTable(data.entries);
}

function renderTiles(c) {
  document.getElementById("tile-today").textContent  = fmtNumber(c.today);
  document.getElementById("tile-failed").textContent = fmtNumber(c.failed_logins_24h);
  document.getElementById("tile-admin").textContent  = fmtNumber(c.admin_actions_24h);
  document.getElementById("tile-total").textContent  = fmtNumber(c.total);
}

function renderTable(rows) {
  const tbody = document.querySelector("#audit-table tbody");
  if (!rows || rows.length === 0) {
    tbody.innerHTML = '<tr><td colspan="6" class="muted">No events match the filter</td></tr>';
    return;
  }
  tbody.innerHTML = rows.map((r) => {
    const cls = actionClass(r.action);
    const target = r.target ? `<code>${escapeHtml(r.target)}</code>` : "";
    let detail = "";
    if (r.meta) {
      try {
        detail = `<code class="detail-snippet" title="${escapeHtml(JSON.stringify(r.meta, null, 2))}">${escapeHtml(JSON.stringify(r.meta))}</code>`;
      } catch (_) {}
    }
    return `<tr>
      <td><span title="${escapeHtml(r.ts)}">${fmtRelative(r.ts)}</span></td>
      <td>${escapeHtml(r.actor_label)}</td>
      <td><span class="action-pill ${cls}">${escapeHtml(r.action)}</span></td>
      <td>${target}</td>
      <td><code>${escapeHtml(r.ip || "")}</code></td>
      <td>${detail}</td>
    </tr>`;
  }).join("");
}

/* ── filters ───────────────────────────────────────────────── */

document.getElementById("audit-filters").addEventListener("submit", (e) => {
  e.preventDefault();
  currentAction = document.getElementById("filter-action").value.trim();
  currentActor  = document.getElementById("filter-actor").value.trim();
  loadData();
});

document.getElementById("clear-filters").addEventListener("click", () => {
  document.getElementById("filter-action").value = "";
  document.getElementById("filter-actor").value  = "";
  currentAction = "";
  currentActor  = "";
  loadData();
});

/* ── boot + auto-refresh every 15s ─────────────────────────── */

loadData();
setInterval(loadData, 15_000);
