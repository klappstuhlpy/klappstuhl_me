// The paste viewer (/p/:id) and the one-shot burned page.
//
// Every lookup here is guarded, because the burned page shares this file but has
// no theme picker, no share panel and no wrap toggle — it only wants `copy`.

(function () {
    "use strict";

    const lines = document.getElementById("paste-lines");

    /** The paste's plaintext, reconstructed from the rendered lines. This is why
     *  the body never needs to be repeated into a <script> tag: it is already in
     *  the DOM, and textContent gives it back exactly. */
    function plaintext() {
        if (!lines) return "";
        return [...lines.querySelectorAll(".paste-line > code")].map((el) => el.textContent).join("\n");
    }

    async function copy(text, button) {
        try {
            await navigator.clipboard.writeText(text);
        } catch {
            return;
        }
        if (!button) return;
        const original = button.textContent;
        button.textContent = "copied ✓";
        setTimeout(() => (button.textContent = original), 1200);
    }

    // ── Copy ────────────────────────────────────────────────────────────────

    document.getElementById("copy-all")?.addEventListener("click", (event) => copy(plaintext(), event.currentTarget));

    // ── Word wrap ───────────────────────────────────────────────────────────

    const wrapToggle = document.getElementById("wrap-toggle");
    if (wrapToggle && lines) {
        const stored = localStorage.getItem("paste-wrap") === "1";
        setWrap(stored);
        wrapToggle.addEventListener("click", () => setWrap(!lines.classList.contains("is-wrapped")));
    }

    function setWrap(on) {
        lines.classList.toggle("is-wrapped", on);
        wrapToggle.setAttribute("aria-pressed", String(on));
        localStorage.setItem("paste-wrap", on ? "1" : "0");
    }

    // ── Line anchors ────────────────────────────────────────────────────────
    //
    // Click a line number for #L12; shift-click a second one for a range
    // (#L12-L20). The selection is highlighted and the URL updated, so "copy link
    // to selection" is just "copy the address bar".

    let anchorStart = null;

    lines?.addEventListener("click", (event) => {
        const link = event.target.closest("a[data-line]");
        if (!link) return;
        event.preventDefault();

        const line = Number(link.dataset.line);
        if (event.shiftKey && anchorStart !== null) {
            const from = Math.min(anchorStart, line);
            const to = Math.max(anchorStart, line);
            select(from, to);
            history.replaceState(null, "", `#L${from}-L${to}`);
        } else {
            anchorStart = line;
            select(line, line);
            history.replaceState(null, "", `#L${line}`);
        }
    });

    function select(from, to) {
        lines.querySelectorAll(".paste-line").forEach((row, index) => {
            const number = index + 1;
            row.classList.toggle("is-selected", number >= from && number <= to);
        });
    }

    // Honour a #L12-L20 range on load — a shared link has to land on the lines it
    // promised, not just the first of them.
    function applyHash() {
        const match = /^#L(\d+)(?:-L(\d+))?$/.exec(window.location.hash);
        if (!match || !lines) return;
        const from = Number(match[1]);
        const to = match[2] ? Number(match[2]) : from;
        select(from, to);
        anchorStart = from;
        document.getElementById(`L${from}`)?.scrollIntoView({ block: "center" });
    }
    applyHash();
    window.addEventListener("hashchange", applyHash);

    // ── Markdown: rendered ⇄ source ─────────────────────────────────────────

    const renderToggle = document.getElementById("render-toggle");
    const markdownPane = document.getElementById("markdown-pane");
    const codePane = document.getElementById("code-pane");
    renderToggle?.addEventListener("click", () => {
        const rendered = !markdownPane.hidden;
        markdownPane.hidden = rendered;
        codePane.hidden = !rendered;
        renderToggle.textContent = rendered ? "source" : "rendered";
        renderToggle.setAttribute("aria-pressed", String(!rendered));
    });

    // ── Share ───────────────────────────────────────────────────────────────

    const shareToggle = document.getElementById("share-toggle");
    const sharePanel = document.getElementById("share-panel");
    shareToggle?.addEventListener("click", () => {
        const open = sharePanel.classList.toggle("is-open");
        shareToggle.setAttribute("aria-expanded", String(open));
    });

    document.addEventListener("click", (event) => {
        if (!sharePanel?.classList.contains("is-open")) return;
        if (event.target.closest(".paste-share")) return;
        sharePanel.classList.remove("is-open");
        shareToggle?.setAttribute("aria-expanded", "false");
    });

    document.getElementById("share-copy")?.addEventListener("click", (event) => {
        const field = document.getElementById("share-url");
        copy(field ? field.value : window.location.href, event.currentTarget);
    });

    // The QR comes from the site's existing /api/render/qr endpoint — the same one
    // the spotlight palette uses. Fetched on first open, then cached in the DOM.
    const qrToggle = document.getElementById("share-qr-toggle");
    const qrBox = document.getElementById("share-qr");
    qrToggle?.addEventListener("click", async (event) => {
        event.preventDefault();
        const open = qrBox.classList.toggle("is-open");
        qrToggle.textContent = open ? "Hide QR code" : "Show QR code";
        if (!open || qrBox.dataset.loaded === "1") return;

        const response = await fetch("/api/render/qr", {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ content: PASTE_URL || window.location.href }),
        }).catch(() => null);

        if (!response || !response.ok) {
            qrBox.textContent = "Could not render a QR code.";
            return;
        }
        qrBox.innerHTML = await response.text();
        qrBox.dataset.loaded = "1";
    });

    // ── Destructive forms ───────────────────────────────────────────────────

    document.querySelectorAll("form[data-confirm]").forEach((form) => {
        form.addEventListener("submit", (event) => {
            if (!window.confirm(form.dataset.confirm)) event.preventDefault();
        });
    });
})();
