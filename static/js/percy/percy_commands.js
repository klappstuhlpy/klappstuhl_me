/* Percy Dashboard — Commands (enable/disable per channel + plonks).
   Expects GUILD_ID, COMMANDS_DATA and window.showToast (percy_common.js). */

(function() {
    let editingIdx = null;
    let currentMode = 'enabled';

    function openCommandEdit(idx) {
        editingIdx = idx;
        const cmd = COMMANDS_DATA[idx];
        if (!cmd) return;
        document.getElementById('cmd-edit-title').textContent = cmd.name;
        document.getElementById('cmd-edit-desc').textContent = cmd.category;

        if (cmd.globally_disabled) {
            setMode('disabled');
        } else if (cmd.disabled_in.length > 0) {
            setMode('partial');
        } else {
            setMode('enabled');
        }

        document.querySelectorAll('#cmd-channels-list input[type="checkbox"]').forEach(function(cb) {
            cb.checked = cmd.disabled_in.indexOf(cb.value) !== -1;
        });

        document.getElementById('cmd-edit-modal').hidden = false;
    }

    function closeCommandEdit() {
        document.getElementById('cmd-edit-modal').hidden = true;
        editingIdx = null;
    }

    function setMode(mode) {
        currentMode = mode;
        var btns = document.querySelectorAll('.cmd-mode-btn');
        for (var i = 0; i < btns.length; i++) {
            btns[i].className = 'cmd-mode-btn';
            if (btns[i].getAttribute('data-mode') === mode) {
                btns[i].classList.add('active-' + mode);
            }
        }
        document.getElementById('channel-select-wrap').hidden = (mode !== 'partial');
    }

    // Event delegation for command cards
    var grid = document.getElementById('command-grid');
    if (grid) {
        grid.addEventListener('click', function(e) {
            var card = e.target.closest('.command-card');
            if (!card) return;
            var idx = parseInt(card.getAttribute('data-idx'), 10);
            if (!isNaN(idx)) openCommandEdit(idx);
        });
    }

    // Mode buttons
    document.querySelectorAll('.cmd-mode-btn').forEach(function(btn) {
        btn.addEventListener('click', function() {
            setMode(btn.getAttribute('data-mode'));
        });
    });

    // Save button
    document.getElementById('cmd-save-btn').addEventListener('click', async function() {
        if (editingIdx === null) return;
        var cmd = COMMANDS_DATA[editingIdx];
        var btn = document.getElementById('cmd-save-btn');
        btn.disabled = true;

        try {
            if (currentMode === 'enabled') {
                var resp = await fetch('/dashboard/guild/' + GUILD_ID + '/commands/toggle', {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json', 'Accept': 'application/json' },
                    body: JSON.stringify({ name: cmd.name, enabled: true, channel_id: null }),
                });
                var data = await resp.json();
                if (!data.ok) { showToast('error', data.error || 'Failed'); btn.disabled = false; return; }
            } else if (currentMode === 'disabled') {
                var resp = await fetch('/dashboard/guild/' + GUILD_ID + '/commands/toggle', {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json', 'Accept': 'application/json' },
                    body: JSON.stringify({ name: cmd.name, enabled: false, channel_id: null }),
                });
                var data = await resp.json();
                if (!data.ok) { showToast('error', data.error || 'Failed'); btn.disabled = false; return; }
            } else {
                // Partial: clear all, then disable in selected channels
                await fetch('/dashboard/guild/' + GUILD_ID + '/commands/toggle', {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json', 'Accept': 'application/json' },
                    body: JSON.stringify({ name: cmd.name, enabled: true, channel_id: null }),
                });

                var checked = document.querySelectorAll('#cmd-channels-list input:checked');
                for (var i = 0; i < checked.length; i++) {
                    await fetch('/dashboard/guild/' + GUILD_ID + '/commands/toggle', {
                        method: 'POST',
                        headers: { 'Content-Type': 'application/json', 'Accept': 'application/json' },
                        body: JSON.stringify({ name: cmd.name, enabled: false, channel_id: checked[i].value }),
                    });
                }
            }

            showToast('success', 'Command "' + cmd.name + '" updated.');
            closeCommandEdit();
            setTimeout(function() { location.reload(); }, 400);
        } catch(err) {
            showToast('error', 'Network error');
        } finally {
            btn.disabled = false;
        }
    });

    // Cancel button + overlay click close the edit modal.
    document.getElementById('cmd-cancel-btn').addEventListener('click', closeCommandEdit);
    document.getElementById('cmd-edit-modal').addEventListener('click', function(e) {
        if (e.target === e.currentTarget) closeCommandEdit();
    });

    // -- Add Plonk modal --
    var plonkModal = document.getElementById('plonk-modal');
    var plonkInput = document.getElementById('plonk-id-input');
    function openPlonkModal() { plonkInput.value = ''; plonkModal.hidden = false; plonkInput.focus(); }
    function closePlonkModal() { plonkModal.hidden = true; }

    document.getElementById('add-plonk-btn').addEventListener('click', openPlonkModal);
    document.getElementById('plonk-cancel-btn').addEventListener('click', closePlonkModal);
    plonkModal.addEventListener('click', function(e) { if (e.target === e.currentTarget) closePlonkModal(); });

    document.getElementById('plonk-add-btn').addEventListener('click', async function() {
        var id = plonkInput.value.trim();
        if (!/^\d{15,20}$/.test(id)) { showToast('error', 'Enter a valid Discord ID (15-20 digits).'); return; }
        var btn = this;
        btn.disabled = true;
        try {
            var resp = await fetch('/dashboard/guild/' + GUILD_ID + '/plonks', {
                method: 'POST',
                headers: { 'Content-Type': 'application/json', 'Accept': 'application/json' },
                body: JSON.stringify({ action: 'add', entity_id: id }),
            });
            var data = await resp.json();
            if (data.ok) {
                showToast('success', 'Entity plonked.');
                closePlonkModal();
                setTimeout(function() { location.reload(); }, 400);
            } else {
                showToast('error', data.error || 'Failed to plonk.');
            }
        } catch(err) {
            showToast('error', 'Network error');
        } finally {
            btn.disabled = false;
        }
    });

    // Search filter
    document.getElementById('cmd-search').addEventListener('input', function() {
        var q = this.value.toLowerCase();
        var cards = document.querySelectorAll('.command-card');
        for (var i = 0; i < cards.length; i++) {
            var name = cards[i].getAttribute('data-name') || '';
            var cat = cards[i].getAttribute('data-category') || '';
            cards[i].style.display = (name.indexOf(q) !== -1 || cat.indexOf(q) !== -1) ? '' : 'none';
        }
    });

    // Plonk removal (expose globally for inline onclick)
    window.removePlonk = async function(entityId) {
        try {
            var resp = await fetch('/dashboard/guild/' + GUILD_ID + '/plonks', {
                method: 'POST',
                headers: { 'Content-Type': 'application/json', 'Accept': 'application/json' },
                body: JSON.stringify({ action: 'remove', entity_id: entityId }),
            });
            var data = await resp.json();
            if (data.ok) {
                var row = document.querySelector('[data-entity-id="' + entityId + '"]');
                if (row) row.remove();
                showToast('success', 'Entity unplonked.');
            } else {
                showToast('error', data.error || 'Failed to unplonk');
            }
        } catch(err) {
            showToast('error', 'Network error');
        }
    };
})();
