/* /terminal — a faux shell that streams answers from the AI assistant.

   Local commands (help, clear, whoami, …) are handled here; anything else is
   sent to the server-side /api/ask proxy, which streams the model's reply back
   over SSE. Conversation history is kept client-side for multi-turn context.
   All rendering uses textContent, so model/user output is never HTML. */
(function () {
    "use strict";

    const root   = document.getElementById("terminal");
    const screen = document.getElementById("term-screen");
    const form   = document.getElementById("term-form");
    const input  = document.getElementById("term-input");
    if (!root || !screen || !form || !input) return;

    // can_use: this viewer may actually spend tokens (public on, or admin).
    let canUse = root.dataset.enabled === "true";
    // configured: an API key exists server-side (feature exists at all).
    const configured = root.dataset.configured === "true";
    /** @type {{role: string, content: string}[]} */
    const history = [];
    let busy = false;

    const GERUNDS = [
        "Cogitating", "Pondering", "Reticulating", "Musing", "Computing",
        "Ruminating", "Percolating", "Deliberating", "Synthesizing",
    ];

    /* ── DOM helpers ──────────────────────────────────────────────────── */

    function scrollDown() {
        screen.scrollTop = screen.scrollHeight;
    }

    function echo(text) {
        const entry = document.createElement("div");
        entry.className = "term-entry";
        const cmd = document.createElement("div");
        cmd.className = "term-echo";
        cmd.textContent = text;
        entry.appendChild(cmd);
        screen.appendChild(entry);
        scrollDown();
        return entry;
    }

    function respond(entry, text, kind) {
        const el = document.createElement("div");
        el.className = "term-response " + (kind || "local");
        el.textContent = text;
        entry.appendChild(el);
        scrollDown();
        return el;
    }

    // A standalone output line not tied to a typed command (e.g. toggle feedback).
    function systemLine(text, kind) {
        const entry = document.createElement("div");
        entry.className = "term-entry";
        respond(entry, text, kind);
        screen.appendChild(entry);
        scrollDown();
    }

    /* ── Local commands ───────────────────────────────────────────────── */

    const HELP =
        "Commands:\n" +
        "  help        show this message\n" +
        "  whoami      who runs this site\n" +
        "  projects    what lives here\n" +
        "  github      open the source\n" +
        "  status      open the live status page\n" +
        "  clear       wipe the screen\n" +
        "\nAnything else is sent to the AI. Try: \"is the site up?\"";

    const WHOAMI =
        "Benedikt — handle Klappstuhl / klappstuhlpy. Builds things for the web, mostly in Rust.\n" +
        "klappstuhl.me is one Rust/Axum binary: a personal site, image host, and homelab admin dashboard.";

    const PROJECTS =
        "- Image host with expiring uploads, ShareX config, OpenGraph embeds\n" +
        "- Homelab admin: live metrics, Docker control + dependency graph, snapshots\n" +
        "- Security: firewall manager, GeoIP login analytics, secrets scanner, file sanitizer\n" +
        "- Reverse-proxy / domain manager, uptime monitoring, off-site S3 backups\n" +
        "- A documented REST API at /api/docs\n" +
        "Type a question to ask the AI for more.";

    function runLocal(cmd, entry) {
        switch (cmd) {
            case "help":     respond(entry, HELP, "local"); return true;
            case "whoami":   respond(entry, WHOAMI, "local"); return true;
            case "projects": respond(entry, PROJECTS, "local"); return true;
            case "clear":
                screen.innerHTML = "";
                return true;
            case "github":
                respond(entry, "Opening github.com/klappstuhlpy …", "local");
                window.open("https://github.com/klappstuhlpy", "_blank", "noopener");
                return true;
            case "status":
                respond(entry, "Opening /status …", "local");
                window.location.href = "/status";
                return true;
            case "":
                return true;
            default:
                return false;
        }
    }

    /* ── AI streaming ─────────────────────────────────────────────────── */

    function makeThinking(entry) {
        const el = document.createElement("div");
        el.className = "term-tool";
        let i = 0;
        el.textContent = GERUNDS[0] + "…";
        const timer = setInterval(() => {
            i = (i + 1) % GERUNDS.length;
            el.textContent = GERUNDS[i] + "…";
        }, 900);
        entry.appendChild(el);
        scrollDown();
        return { el, stop: () => clearInterval(timer) };
    }

    const TOOL_LABELS = {
        get_site_status: "checked live status",
        list_projects: "looked up the projects",
    };

    async function ask(question, entry) {
        history.push({ role: "user", content: question });

        const thinking = makeThinking(entry);
        let answerEl = null;
        let answer = "";
        let pendingNav = null;

        function ensureAnswer() {
            if (!answerEl) {
                thinking.stop();
                thinking.el.remove();
                answerEl = document.createElement("div");
                answerEl.className = "term-response assistant";
                entry.appendChild(answerEl);
            }
            return answerEl;
        }

        try {
            const resp = await fetch("/api/ask", {
                method: "POST",
                headers: { "Content-Type": "application/json" },
                body: JSON.stringify({ messages: history }),
            });

            if (!resp.ok) {
                thinking.stop();
                thinking.el.remove();
                const msg = resp.status === 503
                    ? "The assistant isn't configured on this server yet."
                    : resp.status === 429
                        ? "Slow down a moment — too many questions. Try again shortly."
                        : resp.status === 403
                            ? "The assistant is currently limited to the site owner. Local commands still work — type help."
                            : "Request failed (" + resp.status + ").";
                if (resp.status === 403) canUse = false;
                respond(entry, msg, "error");
                history.pop();
                return;
            }

            const reader = resp.body.getReader();
            const decoder = new TextDecoder();
            let buf = "";

            for (;;) {
                const { value, done } = await reader.read();
                if (done) break;
                buf += decoder.decode(value, { stream: true });

                let nl;
                while ((nl = buf.indexOf("\n")) >= 0) {
                    const line = buf.slice(0, nl).replace(/\r$/, "");
                    buf = buf.slice(nl + 1);
                    if (!line.startsWith("data:")) continue;
                    const payload = line.slice(5).trim();
                    if (!payload) continue;

                    let ev;
                    try { ev = JSON.parse(payload); } catch (_) { continue; }

                    if (ev.type === "text") {
                        answer += ev.text;
                        ensureAnswer().textContent = answer;
                        scrollDown();
                    } else if (ev.type === "tool") {
                        if (ev.name !== "navigate") {
                            const t = document.createElement("div");
                            t.className = "term-tool";
                            t.textContent = TOOL_LABELS[ev.name] || ("used " + ev.name);
                            entry.insertBefore(t, answerEl);
                            scrollDown();
                        }
                    } else if (ev.type === "navigate") {
                        // Only same-origin relative paths; redirect after the
                        // turn finishes so the answer text isn't cut off.
                        if (typeof ev.path === "string" && ev.path[0] === "/" && ev.path[1] !== "/") {
                            pendingNav = ev.path;
                        }
                    } else if (ev.type === "error") {
                        thinking.stop();
                        thinking.el.remove();
                        respond(entry, ev.message || "Something went wrong.", "error");
                    }
                    // "done" needs no action — the stream ends after it.
                }
            }

            thinking.stop();
            if (thinking.el.isConnected) thinking.el.remove();

            if (answer.trim()) {
                history.push({ role: "assistant", content: answer });
            } else if (!entry.querySelector(".term-response.error") && !pendingNav) {
                respond(entry, "(no answer)", "local");
                history.pop();
            }

            // The model asked to change page — show it, then redirect.
            if (pendingNav) {
                const nav = document.createElement("div");
                nav.className = "term-tool";
                nav.textContent = "→ opening " + pendingNav + " …";
                entry.appendChild(nav);
                scrollDown();
                setTimeout(() => { window.location.href = pendingNav; }, 900);
            }
        } catch (err) {
            thinking.stop();
            if (thinking.el.isConnected) thinking.el.remove();
            respond(entry, "Network error reaching the AI.", "error");
            history.pop();
        }
    }

    /* ── Submit handler ───────────────────────────────────────────────── */

    form.addEventListener("submit", async (e) => {
        e.preventDefault();
        if (busy) return;
        const raw = input.value.trim();
        input.value = "";
        if (!raw) return;

        const entry = echo(raw);
        const lower = raw.toLowerCase();

        if (runLocal(lower, entry)) return;

        if (!canUse) {
            const msg = configured
                ? "The assistant is currently limited to the site owner. Local commands still work — type help."
                : "The assistant isn't configured on this server yet — try the local commands (type help).";
            respond(entry, msg, "error");
            return;
        }

        busy = true;
        input.disabled = true;
        input.placeholder = "thinking…";
        await ask(raw, entry);
        busy = false;
        input.disabled = false;
        input.placeholder = "type a question and press Enter…";
        input.focus();
    });

    // ── Admin: public-access toggle ──────────────────────────────────
    const toggleBtn = document.getElementById("ai-public-toggle");
    if (toggleBtn) {
        toggleBtn.addEventListener("click", async (e) => {
            e.stopPropagation(); // don't refocus the input
            const next = toggleBtn.dataset.public !== "true";
            toggleBtn.disabled = true;
            try {
                const r = await fetch("/admin/ai/public", {
                    method: "POST",
                    headers: { "Content-Type": "application/json" },
                    body: JSON.stringify({ enabled: next }),
                });
                if (!r.ok) throw new Error("toggle failed");
                const data = await r.json();
                const on = !!data.public;
                toggleBtn.dataset.public = on ? "true" : "false";
                toggleBtn.classList.toggle("on", on);
                const b = toggleBtn.querySelector("b");
                if (b) b.textContent = on ? "on" : "off";
                systemLine(
                    on
                        ? "Public AI access enabled — any visitor can now spend tokens (rate-limited)."
                        : "Public AI access disabled — the assistant is now limited to admins.",
                    "local"
                );
            } catch (_) {
                systemLine("Could not change public access.", "error");
            } finally {
                toggleBtn.disabled = false;
            }
        });
    }

    // Keep focus on the prompt when clicking anywhere in the window.
    root.addEventListener("click", (e) => {
        if (window.getSelection().toString()) return; // don't steal text selection
        if (e.target.closest("#ai-public-toggle")) return;
        if (e.target.tagName !== "A") input.focus();
    });

    input.focus();
})();
