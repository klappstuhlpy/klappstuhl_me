/* Spotlight / Ctrl+K command palette */

(function () {
    'use strict';

    // ── DOM refs ──────────────────────────────────────────────────────────────

    const overlay  = document.getElementById('spotlight-overlay');
    const input    = document.getElementById('spotlight-input');
    const results  = document.getElementById('spotlight-results');
    const triggerBtn = document.getElementById('spotlight-btn');

    // ── State ─────────────────────────────────────────────────────────────────

    let items       = [];   // current flat item list
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
        activeIdx = Math.max(0, Math.min(items.length - 1, activeIdx + delta));
        renderActive();
        scrollActive();
    }

    function renderActive() {
        const els = results.querySelectorAll('.spotlight-item');
        els.forEach((el, i) => {
            el.classList.toggle('active', i === activeIdx);
            el.setAttribute('aria-selected', i === activeIdx ? 'true' : 'false');
        });
    }

    function scrollActive() {
        const el = results.querySelectorAll('.spotlight-item')[activeIdx];
        if (el) el.scrollIntoView({ block: 'nearest' });
    }

    // ── Fetch search results ──────────────────────────────────────────────────

    async function fetchResults(q) {
        results.innerHTML = '<div class="spotlight-spinner"></div>';
        items = [];
        activeIdx = -1;
        runOutput = null;

        try {
            const url = '/admin/spotlight/search?q=' + encodeURIComponent(q);
            const r = await fetch(url);
            if (!r.ok) throw new Error('HTTP ' + r.status);
            const data = await r.json();
            renderItems(data.items || []);
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
    };

    function renderItems(raw) {
        if (!raw.length) {
            results.innerHTML = '';   // triggers ::after "No results"
            items = [];
            return;
        }

        items = raw;
        activeIdx = 0;

        // Group by kind
        const groups = {};
        for (const item of raw) {
            const k = item.kind;
            if (!groups[k]) groups[k] = [];
            groups[k].push(item);
        }

        let html = '';
        let flatIdx = 0;

        const kindOrder = ['navigate', 'page', 'api', 'script', 'image', 'audit', 'scan', 'ssh', 'container'];
        for (const kind of kindOrder) {
            if (!groups[kind]) continue;
            const sectionTitle = SECTION_TITLES[kind] || kind;
            html += `<div class="spotlight-section">${escHtml(sectionTitle)}</div>`;
            for (const item of groups[kind]) {
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

        // Re-attach click listeners
        results.querySelectorAll('.spotlight-item').forEach(el => {
            el.addEventListener('mouseenter', () => {
                activeIdx = parseInt(el.dataset.idx, 10);
                renderActive();
            });
            el.addEventListener('click', () => {
                const idx = parseInt(el.dataset.idx, 10);
                activateItem(items[idx]);
            });
        });
    }

    // ── Activate an item (navigate or run script) ─────────────────────────────

    function activateItem(item) {
        if (!item) return;

        if (item.kind === 'script') {
            runScript(item);
        } else if (item.url) {
            close();
            window.location.href = item.url;
        }
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
