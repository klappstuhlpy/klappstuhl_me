// The paste editor (/paste, /p/:id/edit).
//
// No CodeMirror, no vendor bundle — a plain <textarea> with a line-number gutter
// beside it. Everything here is progressive enhancement: with JS off the form is
// still a normal POST that works, it just loses the counter, the gutter, and the
// secret-scan dialog (the server refuses a leaked credential either way).

(function () {
    "use strict";

    const form = document.getElementById("paste-form");
    const input = document.getElementById("editor-input");
    const gutter = document.getElementById("editor-gutter");
    const counter = document.getElementById("editor-counter");
    const frame = document.getElementById("editor-frame");
    const language = document.getElementById("editor-language");
    const confirmSecrets = document.getElementById("confirm-secrets");
    const modal = document.getElementById("secret-modal");
    const highlightLayer = document.getElementById("editor-highlight");
    const codeArea = document.getElementById("editor-code");

    if (!form || !input) return;

    const maxBytes = Number(counter?.dataset.max || 0);
    const encoder = new TextEncoder();

    // ── Gutter + counter ────────────────────────────────────────────────────

    function refresh() {
        const value = input.value;
        const lines = value.length === 0 ? 1 : value.split("\n").length;

        if (gutter) {
            let text = "";
            for (let i = 1; i <= lines; i++) text += i + "\n";
            gutter.textContent = text;
        }

        if (counter) {
            const bytes = encoder.encode(value).length;
            counter.textContent = `${lines} line${lines === 1 ? "" : "s"} · ${formatBytes(bytes)}`;
            counter.classList.toggle("is-over", maxBytes > 0 && bytes > maxBytes);
        }
    }

    function formatBytes(bytes) {
        if (bytes >= 1024 * 1024) return (bytes / (1024 * 1024)).toFixed(1) + " MB";
        if (bytes >= 1024) return (bytes / 1024).toFixed(1) + " KB";
        return bytes + " B";
    }

    // Keep the gutter and the highlight layer aligned as the textarea scrolls.
    function syncScroll() {
        if (gutter) gutter.scrollTop = input.scrollTop;
        if (highlightLayer) {
            highlightLayer.style.transform = `translate(${-input.scrollLeft}px, ${-input.scrollTop}px)`;
        }
    }

    // The single "the body changed" entry point: gutter + counter now, coloured
    // syntax shortly after.
    function onEdit() {
        refresh();
        scheduleHighlight();
    }

    input.addEventListener("scroll", syncScroll);
    input.addEventListener("input", onEdit);
    refresh();

    // ── Live syntax highlighting ────────────────────────────────────────────
    //
    // The textarea's own text is transparent; a <pre> behind it carries the
    // colour. On every edit we paint an instant plain (escaped) layer so text is
    // never invisible, then, debounced, ask the server for real highlighting —
    // the same syntect the viewer uses, so the preview can't lie about the
    // result. Auto mode is resolved server-side and reflected on the picker.

    let highlightTimer = null;
    let highlightSeq = 0;

    if (highlightLayer && codeArea) {
        codeArea.classList.add("is-live");
        renderPlain();
        requestHighlight(true);
        if (language) language.addEventListener("change", () => requestHighlight(true));
    }

    function renderPlain() {
        // textContent escapes for us; the trailing newline matches the textarea's
        // own trailing line so the two layers stay the same height.
        highlightLayer.textContent = input.value + "\n";
        syncScroll();
    }

    function scheduleHighlight() {
        if (!highlightLayer) return;
        renderPlain();
        clearTimeout(highlightTimer);
        highlightTimer = setTimeout(() => requestHighlight(false), 180);
    }

    async function requestHighlight(immediate) {
        if (!highlightLayer) return;
        if (immediate) {
            clearTimeout(highlightTimer);
            renderPlain();
        }
        const seq = ++highlightSeq;
        try {
            const response = await fetch("/paste/preview", {
                method: "POST",
                headers: { "Content-Type": "application/json" },
                body: JSON.stringify({ content: input.value, language: language ? language.value : "" }),
            });
            if (!response.ok) return;
            const data = await response.json();
            // A newer keystroke already superseded this response.
            if (seq !== highlightSeq) return;

            highlightLayer.innerHTML = data.html + "\n";
            if (data.background) codeArea.style.background = data.background;
            if (data.foreground) {
                highlightLayer.style.color = data.foreground;
                input.style.caretColor = data.foreground;
            }
            // Show what Auto resolved to on the picker, without touching the
            // stored (empty) value.
            if (language && language.value === "" && langPicker.reflectAuto) {
                langPicker.reflectAuto(data.language, data.label);
            }
            syncScroll();
        } catch (_) {
            /* a failed preview leaves the plain layer in place — not a broken editor */
        }
    }

    // ── Language picker (searchable, with brand logos) ──────────────────────
    //
    // Enhances the native <select> in place: it stays the form's value and the
    // no-JS fallback, while this draws a searchable dropdown that writes back to
    // it. Popular tokens carry a vendored logo (data-icon="1"); everything else,
    // and any logo that fails to load, falls back to a tinted badge.

    const langPicker = buildLangPicker(document.getElementById("lang-picker"), language);

    // ── Editing affordances ─────────────────────────────────────────────────

    const INDENT = "    ";

    input.addEventListener("keydown", (event) => {
        // Ctrl/Cmd+Enter saves. Checked first so it never auto-indents instead.
        if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
            event.preventDefault();
            form.requestSubmit();
            return;
        }
        // Tab inserts spaces instead of leaving the field. Shift+Tab still
        // escapes, so the editor never becomes a keyboard trap.
        if (event.key === "Tab" && !event.shiftKey) {
            event.preventDefault();
            const start = input.selectionStart;
            input.setRangeText(INDENT, start, input.selectionEnd, "end");
            onEdit();
            return;
        }
        // Enter carries the current line's indentation down to the new line, and
        // adds one level after an opener (`:` `{` `[` `(`) so blocks stay lined
        // up — "save tabulation on new line".
        if (event.key === "Enter") {
            event.preventDefault();
            const start = input.selectionStart;
            const value = input.value;
            const lineStart = value.lastIndexOf("\n", start - 1) + 1;
            const prefix = value.slice(lineStart, start);
            let indent = (prefix.match(/^[ \t]*/) || [""])[0];
            if (/[:{[(]\s*$/.test(prefix)) indent += INDENT;
            input.setRangeText("\n" + indent, start, input.selectionEnd, "end");
            onEdit();
            return;
        }
        // Backspace eats a whole indent step when the cursor sits inside a line's
        // leading spaces — "break it on backspace" — instead of one space at a
        // time. A non-multiple falls back to the previous tab stop.
        if (event.key === "Backspace" && input.selectionStart === input.selectionEnd) {
            const pos = input.selectionStart;
            const value = input.value;
            const lineStart = value.lastIndexOf("\n", pos - 1) + 1;
            const prefix = value.slice(lineStart, pos);
            if (prefix.length > 0 && /^ +$/.test(prefix)) {
                event.preventDefault();
                const remove = prefix.length % 4 === 0 ? 4 : prefix.length % 4;
                input.setRangeText("", pos - remove, pos, "end");
                onEdit();
                return;
            }
        }
    });

    // Drop a file onto the editor to load it; the extension picks the language.
    if (frame) {
        ["dragenter", "dragover"].forEach((name) =>
            frame.addEventListener(name, (event) => {
                event.preventDefault();
                frame.classList.add("is-dropping");
            })
        );
        ["dragleave", "drop"].forEach((name) =>
            frame.addEventListener(name, () => frame.classList.remove("is-dropping"))
        );

        frame.addEventListener("drop", (event) => {
            event.preventDefault();
            const file = event.dataTransfer?.files?.[0];
            if (!file) return;

            file.text().then((text) => {
                input.value = text;
                onEdit();

                const ext = file.name.includes(".") ? file.name.split(".").pop().toLowerCase() : "";
                if (ext && language && [...language.options].some((o) => o.value === ext)) {
                    langPicker.setValue(ext);
                }
                const title = form.querySelector('input[name="title"]');
                if (title && !title.value) title.value = file.name;
            });
        });
    }

    // ── Submit ──────────────────────────────────────────────────────────────
    //
    // Submitted async so the server's secret-scan refusal (422 with the rule
    // names) can be turned into a dialog rather than a lost page of typing.

    form.addEventListener("submit", async (event) => {
        event.preventDefault();

        const data = new FormData(form);
        const response = await fetch(form.action, {
            method: "POST",
            headers: { Accept: "application/json" },
            body: new URLSearchParams(data),
        }).catch(() => null);

        if (!response) {
            alert("Could not reach the server. Please try again.");
            return;
        }

        const payload = await response.json().catch(() => ({}));

        if (response.ok) {
            // An anonymous paste hands back an edit token exactly once — carry it
            // in the URL so the landing page can show it and offer the edit link.
            const url = payload.edit_token ? `${payload.url}?token=${encodeURIComponent(payload.edit_token)}` : payload.url;
            window.location.assign(url || "/pastes");
            return;
        }

        if (response.status === 422 && Array.isArray(payload.secrets)) {
            showSecretWarning(payload.secrets, payload.overridable === true, payload.error);
            return;
        }

        alert(payload.error || "Could not save the paste.");
    });

    // ── The secret-scan warning ─────────────────────────────────────────────

    function showSecretWarning(rules, overridable, message) {
        if (!modal) {
            alert(message || "This paste looks like it contains a secret.");
            return;
        }

        const list = document.getElementById("secret-rules");
        if (list) {
            list.innerHTML = "";
            rules.forEach((rule) => {
                const chip = document.createElement("span");
                chip.className = "secret-rule";
                chip.textContent = rule;
                list.appendChild(chip);
            });
        }

        const publish = document.getElementById("secret-publish");
        const text = document.getElementById("secret-message");
        if (text) {
            text.textContent = overridable
                ? "Publishing it here makes it readable by anyone with the link. If it is a live credential, rotate it instead."
                : "Anonymous pastes can't carry credentials. Sign in to publish it anyway, or remove the secret.";
        }
        // An anonymous author has no "publish anyway": the refusal is the point.
        if (publish) publish.hidden = !overridable;

        modal.showModal();
    }

    document.getElementById("secret-cancel")?.addEventListener("click", () => modal?.close());
    document.getElementById("secret-publish")?.addEventListener("click", () => {
        if (confirmSecrets) confirmSecrets.value = "on";
        modal?.close();
        form.requestSubmit();
    });

    // ── Format ──────────────────────────────────────────────────────────────
    //
    // Deliberately narrow: JSON is the one thing we can reformat correctly with
    // no dependency (the browser owns the parser). It's the common pastebin case
    // — a wall of minified JSON someone wants to read. Anything else is left
    // untouched rather than mangled by a half-formatter.

    document.getElementById("editor-format")?.addEventListener("click", () => {
        const text = input.value;
        if (!text.trim()) return;

        const lang = language ? language.value : "";
        const isJson = lang === "json" || looksLikeJson(text);
        if (!isJson) {
            showFormatNote("Only JSON can be reformatted for now.");
            return;
        }
        try {
            input.value = JSON.stringify(JSON.parse(text), null, 2);
            onEdit();
        } catch (_) {
            showFormatNote("That doesn't parse as valid JSON.");
        }
    });

    function looksLikeJson(text) {
        const s = text.trim();
        return (s.startsWith("{") && s.endsWith("}")) || (s.startsWith("[") && s.endsWith("]"));
    }

    let formatNoteTimer = null;
    function showFormatNote(message) {
        const btn = document.getElementById("editor-format");
        if (!btn) return;
        const original = btn.dataset.label || (btn.dataset.label = btn.textContent);
        btn.textContent = message;
        btn.classList.add("chip-warning");
        clearTimeout(formatNoteTimer);
        formatNoteTimer = setTimeout(() => {
            btn.textContent = original;
            btn.classList.remove("chip-warning");
        }, 2200);
    }

    // ── Language-picker implementation ──────────────────────────────────────

    function buildLangPicker(root, select) {
        if (!root || !select) return { setValue() {} };
        root.classList.add("is-enhanced");

        const options = [...select.options];

        const toggle = document.createElement("button");
        toggle.type = "button";
        toggle.className = "lang-toggle";
        toggle.setAttribute("aria-haspopup", "listbox");
        toggle.setAttribute("aria-expanded", "false");
        const iconSlot = document.createElement("span");
        iconSlot.className = "lang-icon-slot";
        const nameSlot = document.createElement("span");
        nameSlot.className = "lang-name";
        const caret = document.createElement("span");
        caret.className = "lang-caret";
        caret.setAttribute("aria-hidden", "true");
        caret.textContent = "▾";
        toggle.append(iconSlot, nameSlot, caret);

        const panel = document.createElement("div");
        panel.className = "glass lang-panel";
        const search = document.createElement("input");
        search.type = "search";
        search.className = "lang-search";
        search.placeholder = "Search languages…";
        search.setAttribute("aria-label", "Search languages");
        const list = document.createElement("div");
        list.className = "lang-list";
        list.setAttribute("role", "listbox");
        panel.append(search, list);
        root.append(toggle, panel);

        const rows = options.map((opt) => {
            const row = document.createElement("button");
            row.type = "button";
            row.className = "lang-option";
            row.setAttribute("role", "option");
            row.dataset.token = opt.value;
            row.dataset.search = `${opt.textContent} ${opt.value}`.toLowerCase();

            const name = document.createElement("span");
            name.className = "lang-name";
            name.textContent = opt.textContent;
            const token = document.createElement("span");
            token.className = "lang-token";
            token.textContent = opt.value;

            row.append(makeIcon(opt.value, opt.dataset.icon === "1"), name, token);
            row.addEventListener("click", () => {
                setValue(opt.value);
                close();
                toggle.focus();
            });
            list.append(row);
            return row;
        });

        function setValue(token) {
            select.value = token;
            const opt = options.find((o) => o.value === select.value) || options[0];
            iconSlot.replaceChildren(makeIcon(opt.value, opt.dataset.icon === "1"));
            nameSlot.textContent = opt.textContent;
            rows.forEach((r) => r.classList.toggle("is-active", r.dataset.token === select.value));
            select.dispatchEvent(new Event("change", { bubbles: true }));
        }

        // Show what Auto detected on the toggle without changing the stored value
        // (which stays "" so the paste keeps re-detecting as it's edited).
        function reflectAuto(token, labelText) {
            if (select.value !== "") return;
            const opt = options.find((o) => o.value === token);
            iconSlot.replaceChildren(makeIcon(token, opt ? opt.dataset.icon === "1" : false));
            nameSlot.textContent = labelText || "Auto-detect";
        }

        function open() {
            root.classList.add("is-open");
            toggle.setAttribute("aria-expanded", "true");
            search.value = "";
            filter("");
            search.focus();
        }
        function close() {
            root.classList.remove("is-open");
            toggle.setAttribute("aria-expanded", "false");
        }
        function filter(q) {
            rows.forEach((r) => {
                r.hidden = q !== "" && !r.dataset.search.includes(q);
            });
        }

        toggle.addEventListener("click", () => (root.classList.contains("is-open") ? close() : open()));
        search.addEventListener("input", () => filter(search.value.toLowerCase().trim()));
        search.addEventListener("keydown", (event) => {
            if (event.key === "Escape") {
                close();
                toggle.focus();
            } else if (event.key === "Enter") {
                event.preventDefault();
                const first = rows.find((r) => !r.hidden);
                if (first) {
                    setValue(first.dataset.token);
                    close();
                    toggle.focus();
                }
            }
        });
        document.addEventListener("click", (event) => {
            if (root.classList.contains("is-open") && !root.contains(event.target)) close();
        });

        setValue(select.value);
        return { setValue, reflectAuto };
    }

    // An icon element for a token: the vendored logo when there is one (with a
    // badge fallback if it 404s), the badge otherwise, and a ~ glyph for Auto.
    function makeIcon(token, hasLogo) {
        if (!token) {
            const auto = document.createElement("span");
            auto.className = "lang-auto";
            auto.textContent = "~";
            return auto;
        }
        if (hasLogo) {
            const img = document.createElement("img");
            img.className = "lang-logo";
            img.loading = "lazy";
            img.alt = "";
            img.src = `/static/img/lang/${encodeURIComponent(token)}.svg`;
            img.addEventListener("error", () => img.replaceWith(makeBadge(token)));
            return img;
        }
        return makeBadge(token);
    }

    function makeBadge(token) {
        const badge = document.createElement("span");
        badge.className = "lang-badge";
        const letters = token.replace(/[^a-z0-9]/gi, "");
        badge.textContent = (letters || token).slice(0, 2);
        return badge;
    }
})();
