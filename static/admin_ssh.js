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

// ── Safe row construction ────────────────────────────────────
//
// All table rows go through buildRow() so the cell count is enforced by
// the JS structure, not by careful template-string composition. Earlier
// code stitched <tr>/<td> as strings — one stray character in any field
// could shift columns left or right and put e.g. the Status pill into
// the "Last used" column. Building DOM nodes here means each cell is a
// real element, isolated from its neighbours, and the column count is
// always exactly cells.length.

/**
 * Build a <tr> from an array of cell contents.
 * Each entry may be a Node (appended as-is), or a string of HTML
 * (assigned via .innerHTML — caller is responsible for escaping any
 * untrusted substrings before calling).
 * Optional second arg sets data-* attrs on the row.
 */
function buildRow(cells, attrs) {
    const tr = document.createElement("tr");
    if (attrs) {
        for (const [k, v] of Object.entries(attrs)) {
            tr.setAttribute(k, String(v));
        }
    }
    for (const cell of cells) {
        const td = document.createElement("td");
        if (cell instanceof Node) {
            td.appendChild(cell);
        } else {
            td.innerHTML = cell == null ? "" : String(cell);
        }
        tr.appendChild(td);
    }
    return tr;
}

/** Replace a tbody's children with the given rows (or a colspan'd message). */
function replaceTbody(tbody, rows, fallbackHtml, colCount) {
    while (tbody.firstChild) tbody.removeChild(tbody.firstChild);
    if (rows.length === 0) {
        const tr = document.createElement("tr");
        const td = document.createElement("td");
        td.colSpan = colCount;
        td.className = "muted";
        td.innerHTML = fallbackHtml;
        tr.appendChild(td);
        tbody.appendChild(tr);
        return;
    }
    for (const row of rows) tbody.appendChild(row);
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

    // Server derives the target host user from the key's comment.
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

/// Keys table column count — keep in sync with the <thead> in
/// admin_ssh.html. Used by the loading / empty / error fallback rows so
/// they always span the right number of columns.
const KEYS_COLS = 8;

// In-flight guard so concurrent loadData() calls (boot + every action
// handler kicks one off) don't race and overwrite each other with stale
// data. Newer call wins: when one is in flight we skip starting another
// and instead set a "re-run when current finishes" flag.
let loadDataInFlight = false;
let loadDataDirty = false;

async function loadData() {
    if (loadDataInFlight) { loadDataDirty = true; return; }
    loadDataInFlight = true;
    try {
        await loadDataOnce();
        if (loadDataDirty) {
            loadDataDirty = false;
            await loadDataOnce();
        }
    } finally {
        loadDataInFlight = false;
    }
}

async function loadDataOnce() {
    const tbody = document.querySelector("#keys-table tbody");
    let data;
    try {
        const res = await fetch("/admin/ssh/data");
        if (!res.ok) {
            // Try to pull `{error: "..."}` out of the body so the operator
            // sees the real reason (e.g. "no such column: target_user")
            // instead of a bare 'HTTP 500'.
            let detail = "";
            try {
                const body = await res.json();
                if (body && body.error) detail = ` — ${body.error}`;
            } catch { /* body wasn't JSON */ }
            replaceTbody(tbody, [],
                `Failed to load keys — HTTP ${res.status} ${escapeHtml(res.statusText || "")}${escapeHtml(detail)}.`,
                KEYS_COLS);
            return;
        }
        data = await res.json();
    } catch (err) {
        replaceTbody(tbody, [],
            `Failed to load keys — ${escapeHtml(err.message || String(err))}.`,
            KEYS_COLS);
        return;
    }

    document.getElementById("stat-total").textContent   = data.total;
    document.getElementById("stat-active").textContent  = data.active;
    document.getElementById("stat-revoked").textContent = data.revoked;
    document.getElementById("key-count").textContent    = data.total;

    renderKeys(data.keys || []);
}

function renderKeys(keys) {
    const tbody = document.querySelector("#keys-table tbody");

    const rows = keys.map(k => {
        const isActive = !k.revoked_at;

        // ── Cell 1: label + (optional) comment ──
        const labelCell = document.createElement("span");
        const nameEl = document.createElement("strong");
        nameEl.textContent = k.name ?? "";
        labelCell.appendChild(nameEl);
        if (k.comment) {
            const cmt = document.createElement("span");
            cmt.className = "key-comment muted";
            cmt.textContent = " — " + k.comment;
            labelCell.appendChild(cmt);
        }

        // ── Cell 2: fingerprint ──
        const fpCell = document.createElement("code");
        fpCell.className = "key-fp";
        fpCell.textContent = k.fingerprint ?? "";

        // ── Cell 3: algorithm ──
        const algoCell = document.createElement("span");
        algoCell.className = "key-algo";
        algoCell.textContent = k.algo ?? "";

        // ── Cell 4: target user ──
        let targetCell;
        if (k.target_user) {
            targetCell = document.createElement("code");
            targetCell.textContent = k.target_user;
        } else {
            targetCell = document.createElement("span");
            targetCell.className = "pill revoked";
            targetCell.title = "Legacy key with no target_user — not synced to any authorized_keys file. Re-add to fix.";
            targetCell.textContent = "not synced";
        }

        // ── Cell 5: added_at ──
        const addedCell = document.createElement("span");
        addedCell.title = k.added_at ?? "";
        addedCell.textContent = fmtRelative(k.added_at);

        // ── Cell 6: last_used_at ──
        const lastCell = document.createElement("span");
        lastCell.title = k.last_used_at ?? "";
        lastCell.textContent = fmtRelative(k.last_used_at);

        // ── Cell 7: status pill ──
        const statusCell = document.createElement("span");
        statusCell.className = "pill " + (isActive ? "active" : "revoked");
        statusCell.textContent = isActive ? "Active" : "Revoked";

        // ── Cell 8: action buttons ──
        const actionsCell = document.createElement("div");
        actionsCell.className = "row-actions";
        const auditBtn = document.createElement("a");
        auditBtn.className = "button outline";
        auditBtn.href = `/admin/ssh/audit?key_id=${encodeURIComponent(k.id)}`;
        auditBtn.textContent = "Audit";
        actionsCell.appendChild(auditBtn);
        if (isActive) {
            const revokeBtn = document.createElement("button");
            revokeBtn.className = "button outline";
            revokeBtn.dataset.action = "revoke";
            revokeBtn.dataset.id = String(k.id);
            revokeBtn.type = "button";
            revokeBtn.textContent = "Revoke";
            actionsCell.appendChild(revokeBtn);
        }
        const deleteBtn = document.createElement("button");
        deleteBtn.className = "button danger";
        deleteBtn.dataset.action = "delete";
        deleteBtn.dataset.id = String(k.id);
        deleteBtn.type = "button";
        deleteBtn.textContent = "Delete";
        actionsCell.appendChild(deleteBtn);

        return buildRow(
            [labelCell, fpCell, algoCell, targetCell, addedCell, lastCell, statusCell, actionsCell],
            { "data-id": k.id },
        );
    });

    replaceTbody(tbody, rows, `No keys — click "Add key" to add one.`, KEYS_COLS);

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

/// Tokens table column count — keep in sync with the <thead>.
const TOKENS_COLS = 7;

let loadTokensInFlight = false;
let loadTokensDirty = false;

async function loadTokens() {
    if (loadTokensInFlight) { loadTokensDirty = true; return; }
    loadTokensInFlight = true;
    try {
        await loadTokensOnce();
        if (loadTokensDirty) {
            loadTokensDirty = false;
            await loadTokensOnce();
        }
    } finally {
        loadTokensInFlight = false;
    }
}

async function loadTokensOnce() {
    const tbody = document.querySelector("#tokens-table tbody");
    let data;
    try {
        const res = await fetch("/admin/ssh/tokens");
        if (!res.ok) {
            let detail = "";
            try {
                const body = await res.json();
                if (body && body.error) detail = ` — ${body.error}`;
            } catch { /* body wasn't JSON */ }
            replaceTbody(tbody, [],
                `Failed to load tokens — HTTP ${res.status} ${escapeHtml(res.statusText || "")}${escapeHtml(detail)}.`,
                TOKENS_COLS);
            return;
        }
        data = await res.json();
    } catch (err) {
        replaceTbody(tbody, [],
            `Failed to load tokens — ${escapeHtml(err.message || String(err))}.`,
            TOKENS_COLS);
        return;
    }
    document.getElementById("token-count").textContent = data.total;
    renderTokens(data.tokens || []);
}

/** Build the expiry cell. Returns a real DOM Node so it slots into a td as-is. */
function buildExpiryCell(iso) {
    if (!iso) {
        const span = document.createElement("span");
        span.className = "muted";
        span.textContent = "Never";
        return span;
    }
    const t = new Date(iso).getTime();
    const span = document.createElement("span");
    span.title = iso;
    if (!isFinite(t)) {
        span.textContent = "—";
        return span;
    }
    const now = Date.now();
    if (t < now) {
        span.className = "token-expired";
        span.textContent = "Expired";
        return span;
    }
    const diff = Math.floor((t - now) / 1000);
    if (diff < 3600) {
        span.className = "token-expires-soon";
        span.textContent = `${Math.floor(diff / 60)}m left`;
    } else if (diff < 86400) {
        span.textContent = `${Math.floor(diff / 3600)}h left`;
    } else {
        span.textContent = `${Math.floor(diff / 86400)}d left`;
    }
    return span;
}

function renderTokens(tokens) {
    const tbody = document.querySelector("#tokens-table tbody");

    const rows = tokens.map(t => {
        const isActive = !t.revoked_at;

        // ── Cell 1: label ──
        const labelCell = document.createElement("strong");
        labelCell.textContent = t.label ?? "";

        // ── Cell 2: scopes ──
        const scopesCell = document.createElement("div");
        scopesCell.className = "scope-wrap";
        if (t.scopes) {
            for (const s of t.scopes.split(",").filter(Boolean)) {
                const pill = document.createElement("span");
                pill.className = "scope-pill";
                pill.textContent = s;
                scopesCell.appendChild(pill);
            }
        } else {
            const muted = document.createElement("span");
            muted.className = "muted";
            muted.textContent = "full";
            scopesCell.appendChild(muted);
        }

        // ── Cell 3: created_at ──
        const createdCell = document.createElement("span");
        createdCell.title = t.created_at ?? "";
        createdCell.textContent = fmtRelative(t.created_at);

        // ── Cell 4: expires_at ──
        const expiresCell = buildExpiryCell(t.expires_at);

        // ── Cell 5: used_at ──
        const usedCell = document.createElement("span");
        usedCell.title = t.used_at ?? "";
        usedCell.textContent = fmtRelative(t.used_at);

        // ── Cell 6: status ──
        const statusCell = document.createElement("span");
        statusCell.className = "pill " + (isActive ? "active" : "revoked");
        statusCell.textContent = isActive ? "Active" : "Revoked";

        // ── Cell 7: actions ──
        const actionsCell = document.createElement("div");
        actionsCell.className = "row-actions";
        if (isActive) {
            const revokeBtn = document.createElement("button");
            revokeBtn.className = "button outline";
            revokeBtn.dataset.taction = "revoke";
            revokeBtn.dataset.id = String(t.id);
            revokeBtn.type = "button";
            revokeBtn.textContent = "Revoke";
            actionsCell.appendChild(revokeBtn);
        }
        const deleteBtn = document.createElement("button");
        deleteBtn.className = "button danger";
        deleteBtn.dataset.taction = "delete";
        deleteBtn.dataset.id = String(t.id);
        deleteBtn.type = "button";
        deleteBtn.textContent = "Delete";
        actionsCell.appendChild(deleteBtn);

        return buildRow(
            [labelCell, scopesCell, createdCell, expiresCell, usedCell, statusCell, actionsCell],
            { "data-id": t.id },
        );
    });

    replaceTbody(tbody, rows, `No tokens — click "Issue token" to create one.`, TOKENS_COLS);

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
