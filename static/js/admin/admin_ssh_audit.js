/* ── SSH Audit log ──────────────────────────────────────────────
   Loads /admin/ssh/audit/data with optional action / key_id filters.
   ─────────────────────────────────────────────────────────────── */

function escapeHtml(s) {
    if (s == null) return "";
    return String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

function fmtRelative(iso) {
    if (!iso) return "—";
    const t = new Date(iso).getTime();
    if (!isFinite(t)) return "—";
    const diff = Math.max(0, Math.floor((Date.now() - t) / 1000));
    if (diff < 60)    return diff + "s ago";
    if (diff < 3600)  return Math.floor(diff / 60) + "m ago";
    if (diff < 86400) return Math.floor(diff / 3600) + "h ago";
    return Math.floor(diff / 86400) + "d ago";
}

function fmtAbsolute(iso) {
    if (!iso) return "";
    const d = new Date(iso);
    if (!isFinite(d.getTime())) return iso;
    return d.toLocaleString(undefined, {
        year: "numeric", month: "short", day: "2-digit",
        hour: "2-digit", minute: "2-digit", second: "2-digit",
        timeZoneName: "short",
    });
}

/** CSS modifier class based on the ssh.* action namespace. */
function actionClass(action) {
    if (action.startsWith("ssh.key"))    return "ssh-key";
    if (action.startsWith("ssh.token"))  return "ssh-token";
    if (action.startsWith("ssh.export")) return "ssh-export";
    return "";
}

// ── Data loading ─────────────────────────────────────────────

let currentAction = "";
let currentKeyId  = "";
let currentLimit  = 200;

async function loadData() {
    const params = new URLSearchParams();
    if (currentAction) params.set("action", currentAction);
    if (currentKeyId)  params.set("key_id", currentKeyId);
    params.set("limit", currentLimit);

    const res = await fetch(`/admin/ssh/audit/data?${params}`);
    if (!res.ok) return;
    const data = await res.json();

    document.getElementById("stat-shown").textContent = data.total;
    document.getElementById("stat-limit").textContent = currentLimit;
    renderTable(data.entries);
}

function renderTable(rows) {
    const tbody = document.querySelector("#audit-table tbody");
    if (!rows || rows.length === 0) {
        tbody.innerHTML = '<tr><td colspan="6" class="muted">No events match the filter.</td></tr>';
        return;
    }

    tbody.innerHTML = rows.map(r => {
        const cls = actionClass(r.action);
        const keyCell = r.key_id != null
            ? `<code><a href="/admin/ssh?highlight=${r.key_id}">#${r.key_id}</a></code>`
            : `<span class="muted">—</span>`;
        const accountCell = r.account_id != null
            ? `<code>#${r.account_id}</code>`
            : `<span class="muted">—</span>`;
        const uaCell = r.user_agent
            ? `<span class="ua-cell" title="${escapeHtml(r.user_agent)}">${escapeHtml(r.user_agent)}</span>`
            : `<span class="muted">—</span>`;

        return `<tr>
            <td><span class="audit-when" title="${escapeHtml(fmtAbsolute(r.created_at))}">${fmtRelative(r.created_at)}</span></td>
            <td>${accountCell}</td>
            <td>${keyCell}</td>
            <td><span class="action-pill ${cls}">${escapeHtml(r.action)}</span></td>
            <td><code>${escapeHtml(r.ip || "")}</code></td>
            <td>${uaCell}</td>
        </tr>`;
    }).join("");
}

// ── Filters ──────────────────────────────────────────────────

document.getElementById("audit-filters").addEventListener("submit", e => {
    e.preventDefault();
    currentAction = document.getElementById("filter-action").value.trim();
    currentKeyId  = document.getElementById("filter-key").value.trim();
    const lv = parseInt(document.getElementById("filter-limit").value, 10);
    currentLimit  = Number.isInteger(lv) ? Math.max(10, Math.min(500, lv)) : 200;
    loadData();
});

document.getElementById("clear-filters").addEventListener("click", () => {
    document.getElementById("filter-action").value = "";
    document.getElementById("filter-key").value    = "";
    document.getElementById("filter-limit").value  = "200";
    currentAction = "";
    currentKeyId  = "";
    currentLimit  = 200;
    loadData();
});

// Pre-fill key_id from URL param (?key_id=42) so clicking the key
// link in the key table lands here pre-filtered.
const urlParams = new URLSearchParams(window.location.search);
if (urlParams.has("key_id")) {
    const kid = urlParams.get("key_id");
    document.getElementById("filter-key").value = kid;
    currentKeyId = kid;
}

// ── Boot ─────────────────────────────────────────────────────

loadData();
setInterval(loadData, 30_000);
