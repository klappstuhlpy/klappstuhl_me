/* Percy Dashboard — guild Moderation tab (ignore list, lockdowns, audit-log
   event flags). Loaded alongside percy_dashboard.js on the guild config page.
   Expects GUILD_ID and window.showToast (percy_common.js). */

(function() {
    function toast(level, msg) { (window.showToast || function(){})(level, msg); }

    async function postJSON(url, body) {
        var resp = await fetch(url, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', 'Accept': 'application/json' },
            body: JSON.stringify(body),
        });
        return resp.json();
    }

    // -- Ignored entities --
    var ignoreAdd = document.getElementById('ignore-add-btn');
    if (ignoreAdd) {
        ignoreAdd.addEventListener('click', async function() {
            var input = document.getElementById('ignore-id-input');
            var sel = document.getElementById('ignore-select');
            var id = (input.value.trim() || sel.value || '').trim();
            if (!/^\d{15,20}$/.test(id)) { toast('error', 'Select an entity or paste a valid Discord ID.'); return; }
            ignoreAdd.disabled = true;
            try {
                var data = await postJSON('/percy/dashboard/guild/' + GUILD_ID + '/moderation/ignore', { action: 'add', entity_id: id });
                if (data.ok) { toast('success', 'Added to ignore list.'); setTimeout(function() { location.reload(); }, 400); }
                else { toast('error', data.error || 'Failed.'); ignoreAdd.disabled = false; }
            } catch (e) { toast('error', 'Network error'); ignoreAdd.disabled = false; }
        });
    }

    function bindRemove(selector, url, rowSel, tbodyId, emptyId, wrapId, key) {
        document.querySelectorAll(selector).forEach(function(btn) {
            btn.addEventListener('click', async function() {
                var id = btn.getAttribute('data-' + key);
                btn.disabled = true;
                var payload = {};
                if (url.indexOf('/ignore') !== -1) { payload = { action: 'remove', entity_id: id }; }
                else { payload = { channel_ids: [id] }; }
                try {
                    var data = await postJSON(url, payload);
                    if (data.ok) {
                        var row = document.querySelector(rowSel.replace('{id}', id));
                        if (row) row.remove();
                        var tbody = document.getElementById(tbodyId);
                        if (tbody && tbody.children.length === 0) {
                            var wrap = document.getElementById(wrapId); if (wrap) wrap.hidden = true;
                            var empty = document.getElementById(emptyId); if (empty) empty.hidden = false;
                        }
                        toast('success', 'Done.');
                    } else { toast('error', data.error || 'Failed.'); btn.disabled = false; }
                } catch (e) { toast('error', 'Network error'); btn.disabled = false; }
            });
        });
    }

    bindRemove('.ignore-remove-btn', '/percy/dashboard/guild/' + GUILD_ID + '/moderation/ignore',
        '#ignore-tbody tr[data-entity-id="{id}"]', 'ignore-tbody', 'ignore-empty', 'ignore-table-wrap', 'entity-id');
    bindRemove('.lockdown-unlock-btn', '/percy/dashboard/guild/' + GUILD_ID + '/lockdowns/unlock',
        '#lockdown-tbody tr[data-channel-id="{id}"]', 'lockdown-tbody', 'lockdown-empty', 'lockdown-table-wrap', 'channel-id');

    // -- Lockdown (lock selected) --
    var lockBtn = document.getElementById('lockdown-lock-btn');
    if (lockBtn) {
        lockBtn.addEventListener('click', async function() {
            var sel = document.getElementById('lockdown-select');
            var ids = Array.prototype.map.call(sel.selectedOptions, function(o) { return o.value; });
            if (!ids.length) { toast('error', 'Select at least one channel.'); return; }
            if (!confirm('Lock ' + ids.length + ' channel(s)? @everyone will not be able to send messages.')) return;
            lockBtn.disabled = true;
            try {
                var data = await postJSON('/percy/dashboard/guild/' + GUILD_ID + '/lockdowns/lock', { channel_ids: ids });
                if (data.ok) { toast('success', 'Channels locked.'); setTimeout(function() { location.reload(); }, 400); }
                else { toast('error', data.error || 'Failed.'); lockBtn.disabled = false; }
            } catch (e) { toast('error', 'Network error'); lockBtn.disabled = false; }
        });
    }

    // -- Audit Log Flags --
    var auditGrid = document.getElementById('audit-flags-grid');
    if (auditGrid) {
        async function saveAuditFlags(flags) {
            try {
                var resp = await fetch('/percy/dashboard/guild/' + GUILD_ID + '/audit-log-flags', {
                    method: 'PATCH',
                    headers: { 'Content-Type': 'application/json', 'Accept': 'application/json' },
                    body: JSON.stringify(flags),
                });
                var data = await resp.json();
                if (data.ok) toast('success', 'Audit log events updated.');
                else toast('error', data.error || 'Failed to update.');
            } catch (e) { toast('error', 'Network error'); }
        }

        auditGrid.addEventListener('change', function(e) {
            if (e.target.tagName !== 'INPUT') return;
            var flags = {};
            flags[e.target.dataset.flag] = e.target.checked;
            saveAuditFlags(flags);
        });

        var allBtn = document.getElementById('audit-flags-all');
        var noneBtn = document.getElementById('audit-flags-none');
        if (allBtn) {
            allBtn.addEventListener('click', function() {
                var flags = {};
                auditGrid.querySelectorAll('input[data-flag]').forEach(function(cb) {
                    cb.checked = true;
                    flags[cb.dataset.flag] = true;
                });
                saveAuditFlags(flags);
            });
        }
        if (noneBtn) {
            noneBtn.addEventListener('click', function() {
                var flags = {};
                auditGrid.querySelectorAll('input[data-flag]').forEach(function(cb) {
                    cb.checked = false;
                    flags[cb.dataset.flag] = false;
                });
                saveAuditFlags(flags);
            });
        }
    }
})();
