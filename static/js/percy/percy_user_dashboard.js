/* Percy personal dashboard — settings form, consent-tracked history
   (names, avatars, presence-over-time uPlot chart) and the data
   download/delete controls. GUILD_ID is injected by the template. */

(function () {
    'use strict';

    // -- Settings form ---------------------------------------------------------
    const form = document.getElementById('user-settings-form');
    const statusEl = document.getElementById('settings-status');
    const saveBtn = document.getElementById('settings-save-btn');

    if (form) {
        form.addEventListener('submit', async function (e) {
            e.preventDefault();
            saveBtn.disabled = true;
            statusEl.textContent = 'Saving…';
            statusEl.className = 'setting-status';

            const payload = {
                timezone: form.querySelector('[name="timezone"]').value.trim() || null,
                track_presence: form.querySelector('[name="track_presence"]').checked,
                track_history: form.querySelector('[name="track_history"]').checked,
            };

            try {
                const res = await fetch(`/dashboard/guild/${GUILD_ID}/me/settings`, {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json', 'Accept': 'application/json' },
                    body: JSON.stringify(payload),
                });
                const data = await res.json();
                if (res.ok && data.ok) {
                    statusEl.textContent = 'Saved';
                    statusEl.className = 'setting-status is-success';
                    if (window.showToast) showToast('success', 'Settings saved.');
                } else {
                    statusEl.textContent = data.error || 'Failed to save';
                    statusEl.className = 'setting-status is-error';
                    if (window.showToast) showToast('error', data.error || 'Failed to save settings.');
                }
            } catch (err) {
                statusEl.textContent = 'Network error';
                statusEl.className = 'setting-status is-error';
                if (window.showToast) showToast('error', 'Network error — check your connection.');
            } finally {
                saveBtn.disabled = false;
                setTimeout(() => { statusEl.textContent = ''; }, 4000);
            }
        });
    }

    // -- History (names, avatars, presence) ------------------------------------
    const esc = (s) => String(s == null ? '' : s)
        .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');

    // `tsHtml` (from timestamps.js) humanises an ISO string; fall back to raw.
    const ts = (iso) => (window.tsHtml ? window.tsHtml(iso) : esc(iso || '—'));

    function renderNameList(el, entries) {
        if (!el) return;
        if (!entries.length) {
            el.innerHTML = '<li class="history-empty">No changes recorded.</li>';
            return;
        }
        el.innerHTML = entries.map((e) =>
            `<li><span class="history-value">${esc(e.name)}</span>` +
            `<span class="history-when">${ts(e.changed_at)}</span></li>`
        ).join('');
    }

    function renderAvatars(el, avatars) {
        if (!el) return;
        el.innerHTML = avatars.slice().reverse().map((a) =>
            `<figure class="avatar-snap">` +
            `<img loading="lazy" alt="Previous avatar" src="data:image/png;base64,${a.image}">` +
            `<figcaption>${ts(a.changed_at)}</figcaption></figure>`
        ).join('');
    }

    // Presence statuses mapped to a numeric lane for the stepped timeline.
    const PRESENCE_LANES = { offline: 0, invisible: 0, dnd: 1, idle: 2, online: 3 };
    const PRESENCE_LABELS = ['Offline', 'DND', 'Idle', 'Online'];

    function renderPresence(el, presence) {
        if (!el) return;
        const message = (msg) => { el.innerHTML = `<div class="chart-message">${esc(msg)}</div>`; };

        if (!window.uPlot) { message('Chart library failed to load (CDN blocked?).'); return; }
        // Percy returns newest-first; the chart needs oldest-first.
        const rows = presence
            .filter((p) => p.changed_at)
            .map((p) => ({ t: Math.floor(Date.parse(p.changed_at) / 1000), lane: PRESENCE_LANES[p.status] ?? 0 }))
            .filter((p) => Number.isFinite(p.t))
            .sort((a, b) => a.t - b.t);

        if (rows.length < 2) { message('Not enough presence history yet — it builds up over time while tracking is on.'); return; }

        const xs = rows.map((r) => r.t);
        const ys = rows.map((r) => r.lane);
        // Extend the last known status to "now" so the final segment is visible.
        const now = Math.floor(Date.now() / 1000);
        if (now > xs[xs.length - 1]) { xs.push(now); ys.push(ys[ys.length - 1]); }

        const accent = getComputedStyle(document.documentElement).getPropertyValue('--branding').trim() || '#d97757';
        const size = () => ({ width: Math.max(220, el.clientWidth - 4), height: 220 });

        el.innerHTML = '';
        const chart = new uPlot({
            ...size(),
            cursor: { drag: { setScale: false } },
            legend: { show: false },
            scales: { x: { time: true }, y: { range: [-0.4, 3.4] } },
            series: [
                {},
                {
                    label: 'Status',
                    stroke: accent,
                    width: 2,
                    paths: uPlot.paths.stepped({ align: 1 }),
                    points: { show: false },
                    value: (_, v) => (v == null ? '—' : PRESENCE_LABELS[v] || v),
                },
            ],
            axes: [
                { stroke: '#71717a' },
                {
                    stroke: '#71717a',
                    grid: { stroke: 'rgba(127,127,127,0.15)' },
                    splits: () => [0, 1, 2, 3],
                    values: () => PRESENCE_LABELS,
                },
            ],
        }, [xs, ys], el);

        let resizeTimer;
        window.addEventListener('resize', () => {
            clearTimeout(resizeTimer);
            resizeTimer = setTimeout(() => chart.setSize(size()), 150);
        });
    }

    async function loadHistory() {
        let data;
        try {
            const res = await fetch(`/dashboard/guild/${GUILD_ID}/me/history`, {
                headers: { 'Accept': 'application/json' },
            });
            if (!res.ok) return;
            data = await res.json();
        } catch (err) {
            return;
        }

        const usernames = data.usernames || [];
        const nicknames = data.nicknames || [];
        const avatars = data.avatars || [];
        const presence = data.presence || [];

        if (usernames.length || nicknames.length) {
            renderNameList(document.getElementById('username-history'), usernames);
            renderNameList(document.getElementById('nickname-history'), nicknames);
            document.getElementById('names-section').hidden = false;
        }
        if (avatars.length) {
            renderAvatars(document.getElementById('avatar-history'), avatars);
            document.getElementById('avatars-section').hidden = false;
        }
        if (presence.length) {
            const section = document.getElementById('presence-section');
            section.hidden = false;
            renderPresence(document.getElementById('presence-chart'), presence);
        }
    }

    loadHistory();

    // -- Data deletion ---------------------------------------------------------
    const deleteBtn = document.getElementById('delete-data-btn');
    if (deleteBtn) {
        deleteBtn.addEventListener('click', async function () {
            const ok = window.confirm(
                'Permanently delete your stored presence, avatar and name/nickname history?\n\n' +
                'This cannot be undone.'
            );
            if (!ok) return;

            deleteBtn.disabled = true;
            const original = deleteBtn.textContent;
            deleteBtn.textContent = 'Deleting…';
            try {
                const res = await fetch(`/dashboard/guild/${GUILD_ID}/me/delete-data`, {
                    method: 'POST',
                    headers: { 'Accept': 'application/json' },
                });
                const data = await res.json().catch(() => ({}));
                if (res.ok && data.ok) {
                    if (window.showToast) showToast('success', 'Your history has been deleted.');
                    setTimeout(() => window.location.reload(), 800);
                } else {
                    if (window.showToast) showToast('error', data.error || 'Failed to delete data.');
                    deleteBtn.disabled = false;
                    deleteBtn.textContent = original;
                }
            } catch (err) {
                if (window.showToast) showToast('error', 'Network error — check your connection.');
                deleteBtn.disabled = false;
                deleteBtn.textContent = original;
            }
        });
    }
})();
