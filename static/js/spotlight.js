/* Spotlight / Ctrl+K command palette */

(function () {
    'use strict';

    // ── DOM refs ──────────────────────────────────────────────────────────────

    const overlay  = document.getElementById('spotlight-overlay');
    const input    = document.getElementById('spotlight-input');
    const results  = document.getElementById('spotlight-results');
    const triggerBtn = document.getElementById('spotlight-btn');

    // ── State ─────────────────────────────────────────────────────────────────

    let items       = [];   // current flat item list (grouped render order)
    let itemEls     = [];   // cached .spotlight-item elements (parallel to items)
    let activeIdx   = -1;   // keyboard-focused index
    let searchTimer = null; // debounce handle
    let runOutput   = null; // pending run-output div

    // ── Open / close ──────────────────────────────────────────────────────────

    function open() {
        overlay.hidden = false;
        input.value = '';
        activeIdx = -1;
        runOutput = null;
        fetchResults('');
        requestAnimationFrame(() => input.focus());
    }

    function close() {
        overlay.hidden = true;
        input.value = '';
        results.innerHTML = '';
        items = [];
        itemEls = [];
        activeIdx = -1;
        runOutput = null;
    }

    // ── Triggers ──────────────────────────────────────────────────────────────

    document.addEventListener('keydown', e => {
        if ((e.ctrlKey || e.metaKey) && e.key === 'k') {
            e.preventDefault();
            overlay.hidden ? open() : close();
            return;
        }
        if (!overlay.hidden) {
            handleKey(e);
        }
    });

    overlay.addEventListener('mousedown', e => {
        // Close when clicking the backdrop (not the modal itself)
        if (e.target === overlay) close();
    });

    if (triggerBtn) {
        triggerBtn.addEventListener('click', () => overlay.hidden ? open() : close());
    }

    // ── Input handler ─────────────────────────────────────────────────────────

    input.addEventListener('input', () => {
        clearTimeout(searchTimer);
        searchTimer = setTimeout(() => fetchResults(input.value.trim()), 180);
    });

    // ── Keyboard nav ──────────────────────────────────────────────────────────

    function handleKey(e) {
        switch (e.key) {
            case 'Escape':
                e.preventDefault();
                close();
                break;
            case 'ArrowDown':
                e.preventDefault();
                moveFocus(1);
                break;
            case 'ArrowUp':
                e.preventDefault();
                moveFocus(-1);
                break;
            case 'Enter':
                e.preventDefault();
                if (activeIdx >= 0 && activeIdx < items.length) {
                    activateItem(items[activeIdx]);
                }
                break;
        }
    }

    function moveFocus(delta) {
        if (!items.length) return;
        const next = Math.max(0, Math.min(items.length - 1, activeIdx + delta));
        setActive(next);
        const el = itemEls[activeIdx];
        if (el) el.scrollIntoView({ block: 'nearest' });
    }

    // O(1) active-row update: only the previously- and newly-active rows are
    // touched, so moving the mouse over a long list while scrolling stays cheap
    // (the old code re-toggled every row on every mouseenter — the scroll lag).
    function setActive(idx) {
        if (idx === activeIdx) return;
        const prev = itemEls[activeIdx];
        if (prev) {
            prev.classList.remove('active');
            prev.setAttribute('aria-selected', 'false');
        }
        activeIdx = idx;
        const next = itemEls[activeIdx];
        if (next) {
            next.classList.add('active');
            next.setAttribute('aria-selected', 'true');
        }
    }

    // ── Fetch search results ──────────────────────────────────────────────────

    async function fetchResults(q) {
        results.innerHTML = '<div class="spotlight-spinner"></div>';
        items = [];
        itemEls = [];
        activeIdx = -1;
        runOutput = null;

        try {
            const url = '/admin/spotlight/search?q=' + encodeURIComponent(q);
            const r = await fetch(url);
            if (!r.ok) throw new Error('HTTP ' + r.status);
            const data = await r.json();
            const list = data.items || [];
            // Admin-only AI: offer to ask the assistant about the typed query.
            // Rendered last (see kindOrder) so it never steals the default Enter.
            if (q) {
                list.push({ kind: 'ai', title: 'Ask the AI', subtitle: '“' + q + '”', query: q });
            }
            renderItems(list);
        } catch (err) {
            results.innerHTML = '<div style="padding:1rem;font-size:0.82rem;color:var(--error-text)">Error: ' + escHtml(err.message) + '</div>';
        }
    }

    // ── Render items ──────────────────────────────────────────────────────────

    const KIND_ICONS = {
        navigate:  '🔗',
        page:      '🌐',
        api:       '🧩',
        script:    '⚡',
        image:     '🖼',
        audit:     '📋',
        scan:      '🛡',
        ssh:       '🔑',
        container: '🐳',
        ai:        '✻',
    };

    const KIND_LABELS = {
        navigate:  '',          // no badge for admin nav items
        page:      '',          // no badge for site pages
        api:       'API',
        script:    'Script',
        image:     'Image',
        audit:     'Audit',
        scan:      'Scan',
        ssh:       'SSH',
        container: 'Container',
        ai:        'AI',
    };

    // Group items by kind for section headers
    const SECTION_TITLES = {
        navigate:  'Admin',
        page:      'Site',
        api:       'API',
        script:    'Scripts',
        image:     'Images',
        audit:     'Audit log',
        scan:      'File scans',
        ssh:       'SSH keys',
        container: 'Containers',
        ai:        'Assistant',
    };

    function renderItems(raw) {
        if (!raw.length) {
            results.innerHTML = '';   // triggers ::after "No results"
            items = [];
            itemEls = [];
            return;
        }

        activeIdx = 0;

        // Group by kind
        const groups = {};
        for (const item of raw) {
            const k = item.kind;
            if (!groups[k]) groups[k] = [];
            groups[k].push(item);
        }

        // Rebuild the flat list in render (grouped) order so item indices line
        // up with the rendered data-idx values.
        const flat = [];
        let html = '';
        let flatIdx = 0;

        const kindOrder = ['navigate', 'page', 'api', 'script', 'image', 'audit', 'scan', 'ssh', 'container', 'ai'];
        for (const kind of kindOrder) {
            if (!groups[kind]) continue;
            const sectionTitle = SECTION_TITLES[kind] || kind;
            html += `<div class="spotlight-section">${escHtml(sectionTitle)}</div>`;
            for (const item of groups[kind]) {
                flat.push(item);
                const icon   = KIND_ICONS[kind]  || '•';
                const label  = KIND_LABELS[kind] || kind;
                const badge  = label
                    ? `<span class="spotlight-item-badge kind-${escHtml(kind)}">${escHtml(label)}</span>`
                    : '';
                const isActive = flatIdx === 0 ? ' active' : '';
                const ariaSelected = flatIdx === 0 ? 'true' : 'false';

                html += `
                    <button
                        class="spotlight-item${isActive}"
                        role="option"
                        aria-selected="${ariaSelected}"
                        data-idx="${flatIdx}"
                    >
                        <span class="spotlight-item-icon">${icon}</span>
                        <span class="spotlight-item-body">
                            <span class="spotlight-item-title">${escHtml(item.title)}</span>
                            <span class="spotlight-item-subtitle">${escHtml(item.subtitle)}</span>
                        </span>
                        ${badge}
                    </button>
                `;
                flatIdx++;
            }
        }

        results.innerHTML = html;
        items = flat;
        itemEls = Array.from(results.querySelectorAll('.spotlight-item'));

        // Re-attach listeners. Hover uses the O(1) setActive so moving the mouse
        // while scrolling doesn't re-style the whole list each frame.
        itemEls.forEach((el, i) => {
            el.addEventListener('mouseenter', () => setActive(i));
            el.addEventListener('click', () => activateItem(items[i]));
        });
    }

    // ── Activate an item (navigate or run script) ─────────────────────────────

    function activateItem(item) {
        if (!item) return;

        if (item.kind === 'ai') {
            askAi(item.query);
        } else if (item.kind === 'script') {
            runScript(item);
        } else if (item.url) {
            close();
            window.location.href = item.url;
        }
    }

    // ── AI assistant (admin-only) ─────────────────────────────────────────────
    // Streams an answer from /api/ask into the palette's output panel. Admins
    // are always allowed through the token-spend gate, so no extra check needed.

    async function askAi(query) {
        if (!query) return;
        const pre = showAiPanel();
        let answer = '';
        let pendingNav = null;

        try {
            const resp = await fetch('/api/ask', {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ messages: [{ role: 'user', content: query }] }),
            });

            if (!resp.ok) {
                setAiStatus('err');
                pre.textContent = resp.status === 503
                    ? "The assistant isn't configured on this server yet."
                    : resp.status === 429
                        ? 'Rate-limited — try again in a moment.'
                        : 'Request failed (' + resp.status + ').';
                return;
            }

            const reader = resp.body.getReader();
            const decoder = new TextDecoder();
            let buf = '';
            for (;;) {
                const { value, done } = await reader.read();
                if (done) break;
                buf += decoder.decode(value, { stream: true });
                let nl;
                while ((nl = buf.indexOf('\n')) >= 0) {
                    const line = buf.slice(0, nl).replace(/\r$/, '');
                    buf = buf.slice(nl + 1);
                    if (!line.startsWith('data:')) continue;
                    const payload = line.slice(5).trim();
                    if (!payload) continue;
                    let ev;
                    try { ev = JSON.parse(payload); } catch (_) { continue; }

                    if (ev.type === 'text') {
                        answer += ev.text;
                        pre.textContent = answer;
                        pre.scrollTop = pre.scrollHeight;
                    } else if (ev.type === 'navigate') {
                        if (typeof ev.path === 'string' && ev.path[0] === '/' && ev.path[1] !== '/') {
                            pendingNav = ev.path;
                        }
                    } else if (ev.type === 'error') {
                        setAiStatus('err');
                        answer += (answer ? '\n' : '') + (ev.message || 'Something went wrong.');
                        pre.textContent = answer;
                    }
                }
            }

            setAiStatus('ok');
            if (!answer.trim() && !pendingNav) pre.textContent = '(no answer)';
            if (pendingNav) {
                pre.textContent = (answer ? answer + '\n' : '') + '→ opening ' + pendingNav + ' …';
                setTimeout(() => { window.location.href = pendingNav; }, 900);
            }
        } catch (err) {
            setAiStatus('err');
            pre.textContent = 'Network error reaching the AI.';
        }
    }

    function showAiPanel() {
        const existing = document.getElementById('spotlight-run-panel');
        if (existing) existing.remove();

        const panel = document.createElement('div');
        panel.id = 'spotlight-run-panel';
        panel.className = 'spotlight-run-wrap';
        panel.innerHTML = `
            <div class="spotlight-run-header">
                <em>✻ Assistant</em>
                <span class="spotlight-run-status" id="spotlight-ai-status">thinking…</span>
            </div>
            <pre class="spotlight-run-pre" id="spotlight-ai-pre"></pre>
        `;

        const modal = overlay.querySelector('.spotlight-modal');
        const footer = modal.querySelector('.spotlight-footer');
        modal.insertBefore(panel, footer);
        runOutput = panel;
        return panel.querySelector('#spotlight-ai-pre');
    }

    function setAiStatus(kind) {
        const s = document.getElementById('spotlight-ai-status');
        if (!s) return;
        s.classList.add(kind);
        s.textContent = kind === 'ok' ? 'OK' : 'Error';
    }

    // ── Script execution ──────────────────────────────────────────────────────

    async function runScript(item) {
        // Show a spinner in the output area
        showRunOutput(item.title, null, null);

        try {
            const r = await fetch('/admin/spotlight/run', {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ script_id: item.script_id }),
            });
            const data = await r.json();

            if (!r.ok) {
                showRunOutput(item.title, null, data.error || 'HTTP ' + r.status);
                return;
            }

            const output = [data.stdout, data.stderr].filter(Boolean).join('\n').trim() || '(no output)';
            showRunOutput(item.title, data.success, output);
        } catch (err) {
            showRunOutput(item.title, null, err.message);
        }
    }

    function showRunOutput(scriptName, success, output) {
        // Remove any existing run output panel
        const existing = document.getElementById('spotlight-run-panel');
        if (existing) existing.remove();

        const panel = document.createElement('div');
        panel.id = 'spotlight-run-panel';
        panel.className = 'spotlight-run-wrap';

        if (output === null) {
            // Loading state
            panel.innerHTML = `
                <div class="spotlight-run-header">
                    Running <em>${escHtml(scriptName)}</em>…
                </div>
                <div class="spotlight-spinner"></div>
            `;
        } else {
            const statusClass  = success === true ? 'ok' : (success === false ? 'err' : 'err');
            const statusLabel  = success === true ? 'OK' : (success === false ? 'Failed' : 'Error');
            panel.innerHTML = `
                <div class="spotlight-run-header">
                    <em>${escHtml(scriptName)}</em>
                    <span class="spotlight-run-status ${statusClass}">${statusLabel}</span>
                </div>
                <pre class="spotlight-run-pre">${escHtml(String(output))}</pre>
            `;
        }

        // Insert before footer
        const modal = overlay.querySelector('.spotlight-modal');
        const footer = modal.querySelector('.spotlight-footer');
        modal.insertBefore(panel, footer);
        runOutput = panel;
    }

    // ── Utilities ─────────────────────────────────────────────────────────────

    function escHtml(s) {
        return String(s ?? '').replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
    }
}());
