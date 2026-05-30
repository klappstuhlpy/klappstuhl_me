/* ── Postgres admin dashboard ──────────────────────────────────
   Three panels:
     - Database picker in the header (drives the page state).
     - Left: Tables / Roles tabs.
     - Right: SQL editor + Run button + results table.
   Safe-mode is on by default; toggling it off shows a confirmation
   banner the first time and switches the Run button to danger style.
   ──────────────────────────────────────────────────────────── */

const dbPicker     = document.getElementById("db-picker");
const tabsEl       = document.getElementById("pg-tabs");
const tablesTbody  = document.querySelector("#tables-table tbody");
const rolesTbody   = document.querySelector("#roles-table tbody");
const sqlInput     = document.getElementById("sql-input");
const runBtn       = document.getElementById("run-btn");
const safeToggle   = document.getElementById("safe-mode");
const statusEl     = document.getElementById("pg-status");
const resultMeta   = document.getElementById("pg-result-meta");
const resultTable  = document.getElementById("pg-result-table");

function escapeHtml(s) {
    if (s == null) return "";
    return String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

function fmtNumber(n) { return (n ?? 0).toLocaleString(); }

function showStatus(text, cls) {
    statusEl.className = "pg-status" + (cls ? " " + cls : "");
    statusEl.textContent = text || "";
}

/* ── Databases ─────────────────────────────────────────────── */

async function loadDatabases() {
    showStatus("Loading databases…");
    const res = await fetch("/admin/postgres/databases");
    if (!res.ok) {
        const err = await res.json().catch(() => ({}));
        showStatus("Failed to list databases: " + (err.error || res.status), "error");
        return;
    }
    const dbs = await res.json();
    dbPicker.innerHTML = "";
    for (const db of dbs) {
        const opt = document.createElement("option");
        opt.value = db.name;
        opt.textContent = `${db.name}  ·  ${db.size_pretty}`;
        dbPicker.appendChild(opt);
    }
    showStatus("");
    if (dbs.length > 0) {
        await loadTables(dbs[0].name);
    }
}

/* ── Tables list ───────────────────────────────────────────── */

async function loadTables(db) {
    tablesTbody.innerHTML = '<tr><td colspan="6" class="muted">Loading…</td></tr>';
    const res = await fetch(`/admin/postgres/tables?db=${encodeURIComponent(db)}`);
    if (!res.ok) {
        const err = await res.json().catch(() => ({}));
        tablesTbody.innerHTML = `<tr><td colspan="6" class="muted">Error: ${escapeHtml(err.error || res.statusText)}</td></tr>`;
        return;
    }
    const rows = await res.json();
    if (rows.length === 0) {
        tablesTbody.innerHTML = '<tr><td colspan="6" class="muted">No tables in this database</td></tr>';
        return;
    }
    tablesTbody.innerHTML = rows.map(r => {
        const fq = `${escapeHtml(r.schema)}.${escapeHtml(r.name)}`;
        return `<tr>
            <td>${escapeHtml(r.schema)}</td>
            <td><code>${escapeHtml(r.name)}</code></td>
            <td>${escapeHtml(r.owner)}</td>
            <td class="numeric">${fmtNumber(r.row_estimate)}</td>
            <td class="numeric">${escapeHtml(r.size_pretty)}</td>
            <td><button class="button outline use-table-btn"
                    data-table="${fq}" title="Insert a SELECT * stub into the query box">Use</button></td>
        </tr>`;
    }).join("");

    // Wire "Use" buttons
    tablesTbody.querySelectorAll(".use-table-btn").forEach(btn => {
        btn.addEventListener("click", () => {
            const fq = btn.dataset.table;
            sqlInput.value = `SELECT * FROM ${fq} LIMIT 100;`;
            sqlInput.focus();
        });
    });
}

/* ── Roles list ─────────────────────────────────────────────── */

async function loadRoles() {
    rolesTbody.innerHTML = '<tr><td colspan="5" class="muted">Loading…</td></tr>';
    const res = await fetch("/admin/postgres/roles");
    if (!res.ok) {
        const err = await res.json().catch(() => ({}));
        rolesTbody.innerHTML = `<tr><td colspan="5" class="muted">Error: ${escapeHtml(err.error || res.statusText)}</td></tr>`;
        return;
    }
    const rows = await res.json();
    if (rows.length === 0) {
        rolesTbody.innerHTML = '<tr><td colspan="5" class="muted">No roles found</td></tr>';
        return;
    }
    const yn = (b) => b ? "✓" : "·";
    rolesTbody.innerHTML = rows.map(r => `<tr>
        <td><code>${escapeHtml(r.name)}</code></td>
        <td>${yn(r.can_login)}</td>
        <td>${yn(r.superuser)}</td>
        <td>${yn(r.can_create_db)}</td>
        <td>${yn(r.can_create_role)}</td>
    </tr>`).join("");
}

/* ── Tab switching ──────────────────────────────────────────── */

tabsEl.addEventListener("click", (e) => {
    const btn = e.target.closest("[data-tab]");
    if (!btn) return;
    tabsEl.querySelectorAll(".pg-tab").forEach(b => b.classList.toggle("active", b === btn));
    document.querySelectorAll(".pg-tab-panel").forEach(p => p.classList.add("hidden"));
    const panel = document.getElementById("pg-panel-" + btn.dataset.tab);
    if (panel) panel.classList.remove("hidden");
    if (btn.dataset.tab === "roles") loadRoles();
});

dbPicker.addEventListener("change", () => loadTables(dbPicker.value));

/* ── Safe-mode toggle ───────────────────────────────────────── */

safeToggle.addEventListener("change", () => {
    const safe = safeToggle.checked;
    runBtn.classList.toggle("primary", safe);
    runBtn.classList.toggle("danger", !safe);
    // Danger banner appears when leaving safe-mode for the first time
    let banner = document.querySelector(".pg-danger-banner");
    if (!safe && !banner) {
        banner = document.createElement("div");
        banner.className = "pg-danger-banner";
        banner.textContent =
            "⚠ Safe mode off. The query will run in a normal transaction — INSERT, UPDATE, DELETE, DROP and other writes are permitted.";
        runBtn.parentElement.insertAdjacentElement("afterend", banner);
    } else if (safe && banner) {
        banner.remove();
    }
});

/* ── Run query ──────────────────────────────────────────────── */

runBtn.addEventListener("click", async () => {
    const sql = sqlInput.value.trim();
    if (!sql) return;
    const db = dbPicker.value;
    if (!db) {
        showStatus("Pick a database first.", "error");
        return;
    }
    runBtn.disabled = true;
    showStatus("Running…");

    // serde_urlencoded can't parse `danger_mode=` (empty) into a bool, so
    // always send an explicit "true"/"false" instead of an empty string.
    const body = new URLSearchParams({
        db,
        sql,
        danger_mode: safeToggle.checked ? "false" : "true",
    });

    try {
        const res = await fetch("/admin/postgres/query", {
            method: "POST",
            headers: { "content-type": "application/x-www-form-urlencoded" },
            body,
        });
        if (!res.ok) {
            const err = await res.json().catch(() => ({}));
            showStatus("Error: " + (err.error || res.statusText), "error");
            return;
        }
        const data = await res.json();
        renderResult(data);
        showStatus(`OK · ${data.row_count} rows in ${data.elapsed_ms} ms`, "ok");
    } catch (e) {
        showStatus("Network error: " + (e.message || e), "error");
    } finally {
        runBtn.disabled = false;
    }
});

function renderResult(data) {
    const cols = data.columns || [];
    const rows = data.rows || [];

    resultMeta.textContent = data.truncated
        ? `Showing first ${rows.length} of ${fmtNumber(data.row_count)} rows (capped at 1000) · ${data.elapsed_ms} ms`
        : `${fmtNumber(rows.length)} row${rows.length === 1 ? "" : "s"} · ${data.elapsed_ms} ms`;

    if (cols.length === 0 && rows.length === 0) {
        resultTable.querySelector("thead").innerHTML = '<tr><th class="muted">Query executed (no rows returned)</th></tr>';
        resultTable.querySelector("tbody").innerHTML = "";
        return;
    }

    const thead = resultTable.querySelector("thead");
    thead.innerHTML = "<tr>" + cols.map(c => `<th>${escapeHtml(c)}</th>`).join("") + "</tr>";
    const tbody = resultTable.querySelector("tbody");
    tbody.innerHTML = rows.map(row =>
        "<tr>" + row.map(cell => `<td>${escapeHtml(cell)}</td>`).join("") + "</tr>"
    ).join("");
}

/* ── Boot ──────────────────────────────────────────────────── */

loadDatabases();

// Keyboard shortcut: Ctrl/Cmd+Enter runs the query.
sqlInput.addEventListener("keydown", (e) => {
    if ((e.ctrlKey || e.metaKey) && e.key === "Enter") {
        e.preventDefault();
        runBtn.click();
    }
});
