/* File sanitizer admin page */

(function () {
    'use strict';

    const dropZone   = document.getElementById('san-drop-zone');
    const fileInput  = document.getElementById('san-file-input');
    const progress   = document.getElementById('san-progress');
    const progressBar = document.getElementById('san-progress-bar');
    const result     = document.getElementById('san-result');
    const tbody      = document.getElementById('san-tbody');

    // ── Utilities ─────────────────────────────────────────────────────────────

    function escHtml(s) {
        return String(s ?? '').replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;');
    }

    function fmtSize(bytes) {
        if (bytes < 1024) return bytes + ' B';
        if (bytes < 1048576) return (bytes / 1024).toFixed(1) + ' KB';
        return (bytes / 1048576).toFixed(1) + ' MB';
    }

    function fmtDate(iso) {
        try { return new Date(iso).toLocaleString(); } catch (_) { return iso; }
    }

    function shortHash(h) {
        return h ? h.substring(0, 16) + '…' : '—';
    }

    // ── Result badges ─────────────────────────────────────────────────────────

    function clamBadge(clean, virus) {
        if (clean === null || clean === undefined) return '<span class="pill na">—</span>';
        if (clean === 1) return '<span class="pill clean">Clean</span>';
        return `<span class="pill infected" title="${escHtml(virus || '')}">Infected</span>`;
    }

    function vtBadge(status, positives, total, url) {
        if (!status) return '<span class="pill na">—</span>';
        if (status === 'clean') {
            const label = total ? `${positives}/${total}` : 'Clean';
            const tag = url
                ? `<a href="${escHtml(url)}" target="_blank" rel="noopener" class="pill clean">${label}</a>`
                : `<span class="pill clean">${label}</span>`;
            return tag;
        }
        if (status === 'detected') {
            const label = total ? `${positives}/${total}` : 'Detected';
            const tag = url
                ? `<a href="${escHtml(url)}" target="_blank" rel="noopener" class="pill infected">${label}</a>`
                : `<span class="pill infected">${label}</span>`;
            return tag;
        }
        if (status === 'unknown') return '<span class="pill unknown">Not in VT</span>';
        return '<span class="pill error">Error</span>';
    }

    // ── Drop zone ─────────────────────────────────────────────────────────────

    dropZone.addEventListener('dragover', e => {
        e.preventDefault();
        dropZone.classList.add('drag-over');
    });
    dropZone.addEventListener('dragleave', () => dropZone.classList.remove('drag-over'));
    dropZone.addEventListener('drop', e => {
        e.preventDefault();
        dropZone.classList.remove('drag-over');
        const file = e.dataTransfer.files[0];
        if (file) uploadFile(file);
    });
    fileInput.addEventListener('change', () => {
        if (fileInput.files[0]) uploadFile(fileInput.files[0]);
        fileInput.value = '';
    });

    // ── Upload & scan ─────────────────────────────────────────────────────────

    async function uploadFile(file) {
        result.hidden = true;
        result.innerHTML = '';
        progress.hidden = false;
        progressBar.style.width = '0%';

        // Animate to 80% while uploading
        let pct = 0;
        const ticker = setInterval(() => {
            if (pct < 80) { pct += 2; progressBar.style.width = pct + '%'; }
        }, 50);

        const form = new FormData();
        form.append('file', file);

        let json;
        try {
            const r = await fetch('/admin/sanitizer/scan', { method: 'POST', body: form });
            json = await r.json();
            if (!r.ok) {
                showResultError(json.error || `HTTP ${r.status}`);
                return;
            }
        } catch (e) {
            showResultError(e.message);
            return;
        } finally {
            clearInterval(ticker);
            progressBar.style.width = '100%';
            setTimeout(() => { progress.hidden = true; progressBar.style.width = '0%'; }, 400);
        }

        renderResult(json);
        loadHistory();
    }

    function showResultError(msg) {
        result.innerHTML = `<div class="san-result-error"><strong>Scan failed:</strong> ${escHtml(msg)}</div>`;
        result.hidden = false;
    }

    function renderResult(s) {
        const overall = overallStatus(s);
        result.innerHTML = `
            <div class="san-result-card ${overall}">
                <div class="san-result-header">
                    <span class="san-result-icon">${overall === 'infected' ? '⚠' : '✓'}</span>
                    <span class="san-result-title">${escHtml(s.filename)}</span>
                    <span class="san-result-size">${fmtSize(s.file_size)}</span>
                </div>
                <div class="san-result-hash" title="${escHtml(s.sha256)}">SHA-256 ${escHtml(s.sha256)}</div>
                <div class="san-result-backends">
                    <div class="san-result-backend">
                        <span class="san-result-label">ClamAV</span>
                        ${clamBadge(s.clamav_clean, s.clamav_virus)}
                        ${s.clamav_virus && s.clamav_clean === 0 ? `<span class="san-virus-name">${escHtml(s.clamav_virus)}</span>` : ''}
                    </div>
                    <div class="san-result-backend">
                        <span class="san-result-label">VirusTotal</span>
                        ${vtBadge(s.vt_status, s.vt_positives, s.vt_total, s.vt_url)}
                    </div>
                </div>
            </div>
        `;
        result.hidden = false;
    }

    function overallStatus(s) {
        if (s.clamav_clean === 0) return 'infected';
        if (s.vt_status === 'detected') return 'infected';
        if (s.clamav_clean === 1 || s.vt_status === 'clean') return 'clean';
        return 'unknown';
    }

    // ── History ───────────────────────────────────────────────────────────────

    async function loadHistory() {
        try {
            const r = await fetch('/admin/sanitizer/history');
            if (!r.ok) throw new Error(`HTTP ${r.status}`);
            const { scans } = await r.json();

            if (!scans.length) {
                tbody.innerHTML = '<tr><td colspan="7" class="table-empty">No scans yet.</td></tr>';
                return;
            }

            tbody.innerHTML = scans.map(s => `
                <tr>
                    <td class="san-fname">${escHtml(s.filename)}</td>
                    <td class="san-size">${fmtSize(s.file_size)}</td>
                    <td class="san-hash" title="${escHtml(s.sha256)}">${shortHash(s.sha256)}</td>
                    <td>${clamBadge(s.clamav_clean, s.clamav_virus)}</td>
                    <td>${vtBadge(s.vt_status, s.vt_positives, s.vt_total, s.vt_url)}</td>
                    <td class="san-date">${fmtDate(s.scanned_at)}</td>
                    <td class="col-actions">
                        <button class="button danger small" data-id="${s.id}">Delete</button>
                    </td>
                </tr>
            `).join('');
        } catch (e) {
            tbody.innerHTML = `<tr><td colspan="7" class="table-empty">Error: ${escHtml(e.message)}</td></tr>`;
        }
    }

    tbody.addEventListener('click', async e => {
        const btn = e.target.closest('[data-id]');
        if (!btn) return;
        const id = btn.dataset.id;
        btn.disabled = true;
        try {
            const r = await fetch(`/admin/sanitizer/${id}`, { method: 'DELETE' });
            if (r.ok) loadHistory();
            else btn.disabled = false;
        } catch (_) { btn.disabled = false; }
    });

    document.getElementById('btn-refresh-history').addEventListener('click', loadHistory);

    // ── Init ──────────────────────────────────────────────────────────────────

    loadHistory();
}());
