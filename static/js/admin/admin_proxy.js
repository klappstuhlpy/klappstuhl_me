/* ── Reverse proxy dashboard ───────────────────────────────────
   Route CRUD + config preview, auto-refresh every 30s.
   ─────────────────────────────────────────────────────────── */

function escapeHtml(s) {
    if (s == null) return "";
    return String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

let currentRoutes = [];
let currentContainers = [];

async function loadData() {
    const res = await fetch("/admin/proxy/data");
    if (!res.ok) return;
    const data = await res.json();
    currentRoutes = data.routes || [];
    currentContainers = data.containers || [];
    renderTiles(data);
    renderRoutes(currentRoutes);
    populateContainers();
}

function renderTiles(data) {
    document.getElementById("tile-kind").textContent = data.proxy_kind;
    document.getElementById("tile-routes").textContent =
        `${data.enabled_count} / ${data.total}`;
    const dirEl = document.getElementById("tile-dir");
    dirEl.textContent = data.cloudflared_api ? "Cloudflare API" : (data.config_dir || "off");
    dirEl.title = data.cloudflared_api
        ? "managed over the Cloudflare tunnel API"
        : (data.config_dir || "disk output disabled");
    // In API mode there's no local config dir to warn about.
    document.getElementById("nodir-banner").style.display =
        (data.config_dir || data.cloudflared_api) ? "none" : "";
    // The "Import from Cloudflare" button only applies in tunnel API mode.
    const importBtn = document.getElementById("cf-import-btn");
    if (importBtn) importBtn.hidden = !data.cloudflared_api;
}

function renderRoutes(rows) {
    const tbody = document.querySelector("#routes-table tbody");
    if (!rows || rows.length === 0) {
        tbody.innerHTML = `<tr><td colspan="8" class="muted">No routes. Add one with <strong>+ New route</strong>.</td></tr>`;
        return;
    }
    tbody.innerHTML = rows.map(r => {
        const container = r.container
            ? `<span class="chip container-tag">${escapeHtml(r.container)}</span>` : "";
        const target = `${escapeHtml(r.target_scheme)}://${escapeHtml(r.target_host)}:${r.target_port}${container}`;
        const yn = (v) => v ? `<span class="flag-yes">yes</span>` : `<span class="flag-no">no</span>`;
        return `<tr data-id="${r.id}">
            <td><strong>${escapeHtml(r.subdomain)}</strong></td>
            <td class="target-cell">${target}</td>
            <td>${yn(r.ssl_managed)}</td>
            <td>${yn(r.cloudflare_proxied)}</td>
            <td>${yn(r.has_auth)}</td>
            <td>${r.rate_limit_rps ? r.rate_limit_rps + "/s" : "—"}</td>
            <td>${r.enabled ? '<span class="pill dot up">enabled</span>' : '<span class="pill dot pending">disabled</span>'}</td>
            <td><div class="row-actions">
                <button class="button outline" data-action="edit">Edit</button>
                <button class="button outline" data-action="toggle">${r.enabled ? "Disable" : "Enable"}</button>
                <button class="button danger small" data-action="delete">Delete</button>
            </div></td>
        </tr>`;
    }).join("");

    tbody.querySelectorAll("button[data-action]").forEach(btn => {
        btn.addEventListener("click", (ev) => {
            const id = ev.target.closest("tr").dataset.id;
            const action = ev.target.dataset.action;
            const route = rows.find(x => String(x.id) === id);
            handleRouteAction(id, action, route);
        });
    });
}

async function handleRouteAction(id, action, route) {
    if (action === "edit") {
        openRouteModal(route);
    } else if (action === "delete") {
        if (!confirm(`Delete route ${route.subdomain}?`)) return;
        const res = await fetch(`/admin/proxy/${id}`, { method: "DELETE" });
        if (res.ok) loadData();
    } else if (action === "toggle") {
        const body = new URLSearchParams({ enabled: route.enabled ? "false" : "true" });
        const res = await fetch(`/admin/proxy/${id}/toggle`, {
            method: "POST",
            headers: { "content-type": "application/x-www-form-urlencoded" },
            body,
        });
        if (res.ok) loadData();
    }
}

/* ── Route modal ────────────────────────────────────────────── */

const routeModal = document.getElementById("route-modal");

function populateContainers() {
    const sel = document.getElementById("r-container");
    const cur = sel.value;
    sel.innerHTML = `<option value="">— raw host:port —</option>` +
        currentContainers.map(c =>
            `<option value="${escapeHtml(c.identifier)}">${escapeHtml(c.name)} (${escapeHtml(c.identifier)})</option>`
        ).join("");
    sel.value = cur;
}

function openRouteModal(route) {
    document.getElementById("route-modal-title").textContent = route ? "Edit route" : "New route";
    document.getElementById("r-id").value = route ? route.id : "";
    document.getElementById("r-subdomain").value = route ? route.subdomain : "";
    document.getElementById("r-host").value = route ? route.target_host : "";
    document.getElementById("r-port").value = route ? route.target_port : "";
    document.getElementById("r-scheme").value = route ? route.target_scheme : "http";
    document.getElementById("r-container").value = route && route.container ? route.container : "";
    document.getElementById("r-ssl").checked = route ? route.ssl_managed : true;
    document.getElementById("r-cf").checked = route ? route.cloudflare_proxied : false;
    document.getElementById("r-auth-user").value = route && route.http_auth_user ? route.http_auth_user : "";
    document.getElementById("r-auth-pass").value = "";
    document.getElementById("r-rate").value = route && route.rate_limit_rps ? route.rate_limit_rps : "";
    document.getElementById("r-access").value = route && route.access_rules_json ? route.access_rules_json : "";
    document.getElementById("r-extra").value = route && route.extra_config ? route.extra_config : "";
    document.getElementById("r-enabled").checked = route ? route.enabled : true;
    const pre = document.getElementById("route-preview");
    pre.hidden = true;
    pre.textContent = "";
    populateContainers();
    document.getElementById("r-container").value = route && route.container ? route.container : "";
    routeModal.hidden = false;
}

function closeRouteModal() { routeModal.hidden = true; }

// When a container is picked, default the target host to its identifier.
document.getElementById("r-container").addEventListener("change", (ev) => {
    const v = ev.target.value;
    if (v) document.getElementById("r-host").value = v;
});

document.getElementById("new-route-btn").addEventListener("click", () => openRouteModal(null));
document.getElementById("route-modal-close").addEventListener("click", closeRouteModal);
document.getElementById("route-modal-cancel").addEventListener("click", closeRouteModal);
routeModal.addEventListener("click", (ev) => { if (ev.target === routeModal) closeRouteModal(); });

function formBody() {
    const body = new URLSearchParams();
    body.set("subdomain", document.getElementById("r-subdomain").value.trim());
    body.set("target_host", document.getElementById("r-host").value.trim());
    body.set("target_port", document.getElementById("r-port").value);
    body.set("target_scheme", document.getElementById("r-scheme").value);
    const container = document.getElementById("r-container").value;
    if (container) body.set("container", container);
    if (document.getElementById("r-ssl").checked) body.set("ssl_managed", "on");
    if (document.getElementById("r-cf").checked) body.set("cloudflare_proxied", "on");
    const user = document.getElementById("r-auth-user").value.trim();
    if (user) body.set("http_auth_user", user);
    const pass = document.getElementById("r-auth-pass").value;
    if (pass) body.set("http_auth_password", pass);
    const rate = document.getElementById("r-rate").value;
    if (rate) body.set("rate_limit_rps", rate);
    const access = document.getElementById("r-access").value.trim();
    if (access) body.set("access_rules_json", access);
    const extra = document.getElementById("r-extra").value.trim();
    if (extra) body.set("extra_config", extra);
    body.set("enabled", document.getElementById("r-enabled").checked ? "on" : "false");
    return body;
}

document.getElementById("route-form").addEventListener("submit", async (ev) => {
    ev.preventDefault();
    const id = document.getElementById("r-id").value;
    const url = id ? `/admin/proxy/${id}` : "/admin/proxy";
    const res = await fetch(url, {
        method: "POST",
        headers: { "content-type": "application/x-www-form-urlencoded" },
        body: formBody(),
    });
    if (res.ok) {
        closeRouteModal();
        loadData();
    } else {
        alert(`Save failed (HTTP ${res.status}). Subdomain may already exist or input is invalid.`);
    }
});

// Preview only works for saved routes (server renders from the DB row).
document.getElementById("route-preview-btn").addEventListener("click", async () => {
    const id = document.getElementById("r-id").value;
    const pre = document.getElementById("route-preview");
    if (!id) {
        pre.hidden = false;
        pre.textContent = "Save the route first to preview its generated config.";
        return;
    }
    const res = await fetch(`/admin/proxy/${id}/preview`);
    if (!res.ok) { pre.hidden = false; pre.textContent = `Preview failed (HTTP ${res.status}).`; return; }
    const data = await res.json();
    pre.hidden = false;
    pre.textContent = `# ${data.file}\n\n${data.config}`;
});

/* ── Regenerate & reload ────────────────────────────────────── */

document.getElementById("reapply-btn").addEventListener("click", async () => {
    const btn = document.getElementById("reapply-btn");
    btn.disabled = true;
    try {
        const res = await fetch("/admin/proxy/apply", { method: "POST" });
        const data = await res.json();
        let msg = data.dir
            ? `Wrote ${data.written} file(s) to ${data.dir}.`
            : "Disk output disabled (no proxy_config_dir).";
        if (data.reload) msg += `\n\nReload: ${data.reload}`;
        if (data.errors && data.errors.length) msg += "\n\nErrors:\n" + data.errors.join("\n");
        alert(msg);
        loadData();
    } finally {
        btn.disabled = false;
    }
});

/* ── Import from Cloudflare (tunnel API mode) ──────────────────── */

document.getElementById("cf-import-btn")?.addEventListener("click", async () => {
    const btn = document.getElementById("cf-import-btn");
    btn.disabled = true;
    const original = btn.textContent;
    btn.textContent = "Importing…";
    try {
        const res = await fetch("/admin/proxy/import", { method: "POST" });
        const data = await res.json();
        if (res.ok) {
            alert(`Imported from Cloudflare tunnel:\n  ${data.imported} new, ${data.updated} updated, ${data.skipped} skipped.`);
            loadData();
        } else {
            alert("Import failed:\n" + (data.error || res.statusText));
        }
    } catch (e) {
        alert("Import failed: " + e);
    } finally {
        btn.disabled = false;
        btn.textContent = original;
    }
});

loadData();
setInterval(loadData, 30_000);
