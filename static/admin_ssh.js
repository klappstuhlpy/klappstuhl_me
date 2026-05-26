/* ── SSH Keys admin dashboard ───────────────────────────────────
   Handles:
     - key list load / render
     - fingerprint preview via SubtleCrypto (matches ssh-keygen SHA256)
     - .pub file upload → textarea populate
     - add / revoke / delete actions
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

// ── Fingerprint preview ──────────────────────────────────────

// Compute SHA256 fingerprint from an OpenSSH public key line,
// matching the output of `ssh-keygen -lf` (SHA256:<base64>).
async function computeFingerprint(keyLine) {
    const parts = keyLine.trim().split(/\s+/);
    if (parts.length < 2) return null;
    const [algo, b64] = parts;

    let raw;
    try {
        raw = Uint8Array.from(atob(b64), c => c.charCodeAt(0));
    } catch {
        return null;
    }

    const hashBuf = await crypto.subtle.digest("SHA-256", raw);
    const hashB64 = btoa(String.fromCharCode(...new Uint8Array(hashBuf)));
    return { fingerprint: "SHA256:" + hashB64, algo };
}

const keyContent  = document.getElementById("key-content");
const keyPreview  = document.getElementById("key-preview");
const keyFp       = document.getElementById("key-fingerprint");
const keyAlgo     = document.getElementById("key-algo");
const submitBtn   = document.getElementById("submit-key-btn");
const addError    = document.getElementById("add-error");

async function updatePreview() {
    const val = keyContent.value.trim();
    if (!val) {
        keyPreview.hidden = true;
        submitBtn.disabled = true;
        return;
    }
    const result = await computeFingerprint(val);
    if (!result) {
        keyPreview.hidden = true;
        submitBtn.disabled = true;
        return;
    }
    keyFp.textContent   = result.fingerprint;
    keyAlgo.textContent = result.algo;
    keyPreview.hidden   = false;
    submitBtn.disabled  = false;
}

keyContent.addEventListener("input", updatePreview);

// ── File upload ──────────────────────────────────────────────

document.getElementById("key-file").addEventListener("change", async (e) => {
    const file = e.target.files[0];
    if (!file) return;
    const text = await file.text();
    keyContent.value = text.trim();
    await updatePreview();
    e.target.value = "";
});

// ── Drawer open/close ────────────────────────────────────────

const drawer = document.getElementById("add-drawer");

document.getElementById("add-key-btn").addEventListener("click", () => {
    drawer.hidden = false;
    document.getElementById("key-name").focus();
});

document.getElementById("close-drawer-btn").addEventListener("click", () => {
    drawer.hidden = true;
    resetDrawer();
});

function resetDrawer() {
    document.getElementById("key-name").value = "";
    keyContent.value = "";
    keyPreview.hidden = true;
    addError.hidden = true;
    submitBtn.disabled = true;
}

// ── Submit new key ───────────────────────────────────────────

document.getElementById("submit-key-btn").addEventListener("click", async () => {
    const name = document.getElementById("key-name").value.trim();
    const publicKey = keyContent.value.trim();
    addError.hidden = true;

    if (!name) {
        showError("Please enter a label for the key.");
        return;
    }

    submitBtn.disabled = true;
    submitBtn.textContent = "Adding…";

    try {
        const res = await fetch("/admin/ssh/keys", {
            method: "POST",
            headers: { "content-type": "application/json" },
            body: JSON.stringify({ name, public_key: publicKey }),
        });

        if (res.ok) {
            drawer.hidden = true;
            resetDrawer();
            loadData();
        } else {
            const body = await res.json().catch(() => ({}));
            showError(body.error || `Server error (HTTP ${res.status}).`);
        }
    } finally {
        submitBtn.disabled = false;
        submitBtn.textContent = "Add key";
    }
});

function showError(msg) {
    addError.textContent = msg;
    addError.hidden = false;
}

// ── Key list ─────────────────────────────────────────────────

async function loadData() {
    const res = await fetch("/admin/ssh/data");
    if (!res.ok) return;
    const data = await res.json();

    document.getElementById("stat-total").textContent   = data.total;
    document.getElementById("stat-active").textContent  = data.active;
    document.getElementById("stat-revoked").textContent = data.revoked;
    document.getElementById("key-count").textContent    = data.total;

    renderKeys(data.keys);
}

function renderKeys(keys) {
    const tbody = document.querySelector("#keys-table tbody");
    if (!keys || keys.length === 0) {
        tbody.innerHTML = `<tr><td colspan="7" class="muted">No keys — click "Add key" to add one.</td></tr>`;
        return;
    }

    tbody.innerHTML = keys.map(k => {
        const isActive = !k.revoked_at;
        const statusPill = isActive
            ? `<span class="status-pill active">Active</span>`
            : `<span class="status-pill revoked">Revoked</span>`;

        const actions = isActive
            ? `<a class="button outline" href="/admin/ssh/audit?key_id=${k.id}">Audit</a>
               <button class="button outline" data-action="revoke" data-id="${k.id}">Revoke</button>
               <button class="button danger"  data-action="delete" data-id="${k.id}">Delete</button>`
            : `<a class="button outline" href="/admin/ssh/audit?key_id=${k.id}">Audit</a>
               <button class="button danger"  data-action="delete" data-id="${k.id}">Delete</button>`;

        const comment = k.comment
            ? `<span class="key-comment muted"> — ${escapeHtml(k.comment)}</span>`
            : "";

        return `<tr data-id="${k.id}">
            <td><strong>${escapeHtml(k.name)}</strong>${comment}</td>
            <td><code class="key-fp">${escapeHtml(k.fingerprint)}</code></td>
            <td><span class="key-algo">${escapeHtml(k.algo)}</span></td>
            <td><span title="${escapeHtml(k.added_at)}">${fmtRelative(k.added_at)}</span></td>
            <td><span title="${k.last_used_at || ''}">${fmtRelative(k.last_used_at)}</span></td>
            <td>${statusPill}</td>
            <td><div class="row-actions">${actions}</div></td>
        </tr>`;
    }).join("");

    tbody.querySelectorAll("[data-action]").forEach(btn => {
        btn.addEventListener("click", () => handleAction(btn.dataset.action, btn.dataset.id));
    });
}

async function handleAction(action, id) {
    if (action === "revoke") {
        if (!confirm("Revoke this key? It will no longer be authorized.")) return;
        const res = await fetch(`/admin/ssh/keys/${id}/revoke`, { method: "POST" });
        if (!res.ok) alert(`Failed to revoke key (HTTP ${res.status}).`);
    } else if (action === "delete") {
        if (!confirm("Permanently delete this key? This cannot be undone.")) return;
        const res = await fetch(`/admin/ssh/keys/${id}`, { method: "DELETE" });
        if (!res.ok) alert(`Failed to delete key (HTTP ${res.status}).`);
    }
    loadData();
}

// ── Token list ───────────────────────────────────────────────

async function loadTokens() {
    const res = await fetch("/admin/ssh/tokens");
    if (!res.ok) return;
    const data = await res.json();
    document.getElementById("token-count").textContent = data.total;
    renderTokens(data.tokens);
}

function fmtExpiry(iso) {
    if (!iso) return '<span class="muted">Never</span>';
    const t = new Date(iso).getTime();
    if (!isFinite(t)) return "—";
    const now = Date.now();
    if (t < now) return `<span class="token-expired" title="${escapeHtml(iso)}">Expired</span>`;
    const diff = Math.floor((t - now) / 1000);
    if (diff < 3600) return `<span class="token-expires-soon">${Math.floor(diff / 60)}m left</span>`;
    if (diff < 86400) return `<span title="${escapeHtml(iso)}">${Math.floor(diff / 3600)}h left</span>`;
    return `<span title="${escapeHtml(iso)}">${Math.floor(diff / 86400)}d left</span>`;
}

function renderTokens(tokens) {
    const tbody = document.querySelector("#tokens-table tbody");
    if (!tokens || tokens.length === 0) {
        tbody.innerHTML = `<tr><td colspan="7" class="muted">No tokens — click "Issue token" to create one.</td></tr>`;
        return;
    }

    tbody.innerHTML = tokens.map(t => {
        const isActive = !t.revoked_at;
        const statusPill = isActive
            ? `<span class="status-pill active">Active</span>`
            : `<span class="status-pill revoked">Revoked</span>`;

        const scopes = t.scopes
            ? t.scopes.split(",").filter(Boolean).map(s => `<span class="scope-pill">${escapeHtml(s)}</span>`).join("")
            : `<span class="muted">full</span>`;

        const actions = isActive
            ? `<button class="button outline" data-taction="revoke" data-id="${t.id}">Revoke</button>
               <button class="button danger"  data-taction="delete" data-id="${t.id}">Delete</button>`
            : `<button class="button danger"  data-taction="delete" data-id="${t.id}">Delete</button>`;

        return `<tr data-id="${t.id}">
            <td><strong>${escapeHtml(t.label)}</strong></td>
            <td><div class="scope-wrap">${scopes}</div></td>
            <td><span title="${escapeHtml(t.created_at)}">${fmtRelative(t.created_at)}</span></td>
            <td>${fmtExpiry(t.expires_at)}</td>
            <td><span title="${t.used_at || ''}">${fmtRelative(t.used_at)}</span></td>
            <td>${statusPill}</td>
            <td><div class="row-actions">${actions}</div></td>
        </tr>`;
    }).join("");

    tbody.querySelectorAll("[data-taction]").forEach(btn => {
        btn.addEventListener("click", () => handleTokenAction(btn.dataset.taction, btn.dataset.id));
    });
}

async function handleTokenAction(action, id) {
    if (action === "revoke") {
        if (!confirm("Revoke this token? It will stop working immediately.")) return;
        const res = await fetch(`/admin/ssh/tokens/${id}/revoke`, { method: "POST" });
        if (!res.ok) alert(`Failed to revoke token (HTTP ${res.status}).`);
    } else if (action === "delete") {
        if (!confirm("Permanently delete this token record?")) return;
        const res = await fetch(`/admin/ssh/tokens/${id}`, { method: "DELETE" });
        if (!res.ok) alert(`Failed to delete token (HTTP ${res.status}).`);
    }
    loadTokens();
}

// ── Issue token drawer ───────────────────────────────────────

const issueDrawer = document.getElementById("issue-drawer");

document.getElementById("issue-token-btn").addEventListener("click", () => {
    issueDrawer.hidden = false;
    document.getElementById("token-label").focus();
});

document.getElementById("close-issue-btn").addEventListener("click", () => {
    issueDrawer.hidden = true;
    resetIssueDrawer();
});

function resetIssueDrawer() {
    document.getElementById("token-label").value = "";
    document.getElementById("token-expires").value = "";
    document.getElementById("issue-error").hidden = true;
    issueDrawer.querySelectorAll(".scope-check input").forEach(cb => { cb.checked = true; });
}

document.getElementById("submit-token-btn").addEventListener("click", async () => {
    const label = document.getElementById("token-label").value.trim();
    const expiresRaw = document.getElementById("token-expires").value.trim();
    const expiresInHours = expiresRaw ? parseInt(expiresRaw, 10) : null;
    const issueError = document.getElementById("issue-error");
    issueError.hidden = true;

    if (!label) { showIssueError("Please enter a label."); return; }
    if (expiresRaw && (!Number.isInteger(expiresInHours) || expiresInHours < 1)) {
        showIssueError("Expiry must be a positive number of hours.");
        return;
    }

    const checkedScopes = [...issueDrawer.querySelectorAll(".scope-check input:checked")]
        .map(cb => cb.value);
    const scopes = checkedScopes.join(",");

    const btn = document.getElementById("submit-token-btn");
    btn.disabled = true;
    btn.textContent = "Issuing…";

    try {
        const res = await fetch("/admin/ssh/tokens", {
            method: "POST",
            headers: { "content-type": "application/json" },
            body: JSON.stringify({ label, scopes, expires_in_hours: expiresInHours }),
        });
        if (res.ok) {
            const data = await res.json();
            issueDrawer.hidden = true;
            resetIssueDrawer();
            showTokenReveal(data.token);
            loadTokens();
        } else {
            const body = await res.json().catch(() => ({}));
            showIssueError(body.error || `Server error (HTTP ${res.status}).`);
        }
    } finally {
        btn.disabled = false;
        btn.textContent = "Issue token";
    }
});

function showIssueError(msg) {
    const el = document.getElementById("issue-error");
    el.textContent = msg;
    el.hidden = false;
}

// ── Copy-once reveal banner ──────────────────────────────────

const tokenReveal = document.getElementById("token-reveal");
const tokenRevealValue = document.getElementById("token-reveal-value");

function showTokenReveal(token) {
    tokenRevealValue.textContent = token;
    tokenReveal.hidden = false;
    tokenReveal.scrollIntoView({ behavior: "smooth", block: "nearest" });
}

document.getElementById("token-copy-btn").addEventListener("click", async () => {
    const val = tokenRevealValue.textContent;
    try {
        await navigator.clipboard.writeText(val);
        const btn = document.getElementById("token-copy-btn");
        btn.textContent = "Copied!";
        setTimeout(() => { btn.textContent = "Copy"; }, 2000);
    } catch {
        // Fallback: select the text
        const range = document.createRange();
        range.selectNode(tokenRevealValue);
        window.getSelection().removeAllRanges();
        window.getSelection().addRange(range);
    }
});

document.getElementById("token-reveal-dismiss").textContent = "×";
document.getElementById("token-reveal-dismiss").addEventListener("click", () => {
    tokenReveal.hidden = true;
    tokenRevealValue.textContent = "";
});

// ── Boot ─────────────────────────────────────────────────────

loadData();
loadTokens();
