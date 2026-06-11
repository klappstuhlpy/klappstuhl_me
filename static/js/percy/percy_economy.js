/* Percy Dashboard — Economy (shop items + lottery management).
   Expects GUILD_ID and window.showToast (percy_common.js). */

(function() {
    const guildId = GUILD_ID;
    const base = `/percy/dashboard/guild/${guildId}/economy`;

    // Add item
    const addItemBtn = document.getElementById('add-item-btn');
    const addItemModal = document.getElementById('add-item-modal');
    const effectSelect = document.getElementById('item_effect');
    const effectValueGroup = document.getElementById('effect-value-group');
    const effectValueLabel = document.getElementById('effect-value-label');
    const effectRoleGroup = document.getElementById('effect-role-group');
    const effectDurationGroup = document.getElementById('effect-duration-group');

    function updateEffectFields() {
        const effect = effectSelect.value;
        effectValueGroup.hidden = true;
        effectRoleGroup.hidden = true;
        effectDurationGroup.hidden = true;
        if (effect === 'cash' || effect === 'lootbox') {
            effectValueGroup.hidden = false;
            effectValueLabel.textContent = 'Cash Amount';
        } else if (effect === 'xp_boost' || effect === 'loot_boost') {
            effectValueGroup.hidden = false;
            effectValueLabel.textContent = 'Bonus Percent (1-500)';
            effectDurationGroup.hidden = false;
        } else if (effect === 'role') {
            effectRoleGroup.hidden = false;
        }
    }

    if (addItemBtn && addItemModal) {
        effectSelect.addEventListener('change', updateEffectFields);
        addItemBtn.addEventListener('click', () => { addItemModal.hidden = false; });
        document.getElementById('item-modal-cancel').addEventListener('click', () => { addItemModal.hidden = true; });
        addItemModal.addEventListener('click', (e) => { if (e.target === addItemModal) addItemModal.hidden = true; });
        document.getElementById('item-modal-confirm').addEventListener('click', async () => {
            const name = document.getElementById('item_name').value.trim();
            const price = parseInt(document.getElementById('item_price').value);
            const description = document.getElementById('item_desc').value.trim() || null;
            if (!name || !price) { showToast('error', 'Name and price required.'); return; }
            const effect = effectSelect.value;
            const body = { name, price, description };
            if (effect !== 'none') {
                body.effect = effect;
                if (effect === 'cash' || effect === 'lootbox' || effect === 'xp_boost' || effect === 'loot_boost') {
                    const val = parseInt(document.getElementById('item_effect_value').value);
                    if (!val || val < 1) { showToast('error', 'Effect value is required.'); return; }
                    body.effect_value = val;
                }
                if (effect === 'role') {
                    const roleId = document.getElementById('item_effect_role').value;
                    if (!roleId) { showToast('error', 'Please select a role.'); return; }
                    body.effect_value = parseInt(roleId);
                }
                if (effect === 'xp_boost' || effect === 'loot_boost') {
                    const dur = parseInt(document.getElementById('item_duration').value);
                    if (!dur || dur < 1) { showToast('error', 'Duration is required for boost effects.'); return; }
                    body.duration_minutes = dur;
                }
            }
            try {
                const r = await fetch(`${base}/items`, { method: 'POST', headers: { 'Content-Type': 'application/json', 'Accept': 'application/json' }, body: JSON.stringify(body) });
                const d = await r.json();
                if (d.ok) { showToast('success', 'Item created.'); setTimeout(() => location.reload(), 400); }
                else { showToast('error', d.error || 'Failed.'); }
            } catch { showToast('error', 'Network error.'); }
        });
    }

    // Delete item
    document.querySelectorAll('.delete-item-btn').forEach(btn => {
        btn.addEventListener('click', async () => {
            const name = btn.dataset.name;
            if (!confirm(`Delete item "${name}"?`)) return;
            try {
                const r = await fetch(`${base}/items/${encodeURIComponent(name)}`, { method: 'DELETE', headers: { 'Accept': 'application/json' } });
                const d = await r.json();
                if (d.ok) { showToast('success', 'Deleted.'); setTimeout(() => location.reload(), 400); }
                else { showToast('error', d.error || 'Failed.'); }
            } catch { showToast('error', 'Network error.'); }
        });
    });

    // Lottery start
    const startBtn = document.getElementById('start-lottery-btn');
    const lotteryModal = document.getElementById('lottery-modal');
    if (startBtn && lotteryModal) {
        startBtn.addEventListener('click', () => { lotteryModal.hidden = false; });
        document.getElementById('lottery-modal-cancel').addEventListener('click', () => { lotteryModal.hidden = true; });
        lotteryModal.addEventListener('click', (e) => { if (e.target === lotteryModal) lotteryModal.hidden = true; });
        document.getElementById('lottery-modal-confirm').addEventListener('click', async () => {
            const ticket_price = parseInt(document.getElementById('lottery_price').value);
            const duration_minutes = parseInt(document.getElementById('lottery_duration').value);
            const channel_id = document.getElementById('lottery_channel').value.trim();
            if (!ticket_price || !duration_minutes || !channel_id) { showToast('error', 'All fields required.'); return; }
            try {
                const r = await fetch(`${base}/lottery`, { method: 'POST', headers: { 'Content-Type': 'application/json', 'Accept': 'application/json' }, body: JSON.stringify({ ticket_price, duration_minutes, channel_id }) });
                const d = await r.json();
                if (d.ok) { showToast('success', 'Lottery started.'); setTimeout(() => location.reload(), 400); }
                else { showToast('error', d.error || 'Failed.'); }
            } catch { showToast('error', 'Network error.'); }
        });
    }

    // Lottery cancel
    const cancelBtn = document.getElementById('cancel-lottery-btn');
    if (cancelBtn) {
        cancelBtn.addEventListener('click', async () => {
            if (!confirm('Cancel the active lottery?')) return;
            try {
                const r = await fetch(`${base}/lottery`, { method: 'DELETE', headers: { 'Accept': 'application/json' } });
                const d = await r.json();
                if (d.ok) { showToast('success', 'Lottery cancelled.'); setTimeout(() => location.reload(), 400); }
                else { showToast('error', d.error || 'Failed.'); }
            } catch { showToast('error', 'Network error.'); }
        });
    }
})();
