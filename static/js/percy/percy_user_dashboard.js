(function () {
    'use strict';

    const form = document.getElementById('user-settings-form');
    const statusEl = document.getElementById('settings-status');
    const saveBtn = document.getElementById('settings-save-btn');

    if (!form) return;

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
})();
