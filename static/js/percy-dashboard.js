/* Percy Dashboard — tab switching, async form submission, dirty tracking, cancel/save */

(function() {
    const tabs = document.querySelectorAll('[role="tab"]');
    const panels = document.querySelectorAll('[role="tabpanel"]');

    function switchTab(tabId) {
        tabs.forEach(t => t.setAttribute('aria-selected', t.dataset.tab === tabId ? 'true' : 'false'));
        panels.forEach(p => {
            p.hidden = p.id !== 'tab-' + tabId;
        });
        history.replaceState(null, '', '#' + tabId);
    }

    tabs.forEach(tab => {
        tab.addEventListener('click', () => switchTab(tab.dataset.tab));
    });

    const hash = location.hash.slice(1);
    if (hash && document.getElementById('tab-' + hash)) {
        switchTab(hash);
    }

    document.querySelectorAll('.gk-setup-link').forEach(link => {
        link.addEventListener('click', (e) => {
            e.preventDefault();
            switchTab('gatekeeper');
        });
    });

    // -- Form state tracking ---------------------------------------------------

    function captureFormState(form) {
        const state = {};
        for (const el of form.elements) {
            if (!el.name || el.name.startsWith('_')) continue;
            if (el.type === 'checkbox') {
                state[el.name] = el.checked;
            } else {
                state[el.name] = el.value;
            }
        }
        return state;
    }

    function restoreFormState(form, state) {
        for (const el of form.elements) {
            if (!el.name || el.name.startsWith('_')) continue;
            if (el.type === 'checkbox') {
                el.checked = !!state[el.name];
            } else {
                el.value = state[el.name] !== undefined ? state[el.name] : '';
            }
        }
    }

    function isFormDirty(form, initial) {
        for (const el of form.elements) {
            if (!el.name || el.name.startsWith('_')) continue;
            if (el.type === 'checkbox') {
                if (el.checked !== !!initial[el.name]) return true;
            } else {
                const initVal = initial[el.name] !== undefined ? initial[el.name] : '';
                if (el.value !== initVal) return true;
            }
        }
        return false;
    }

    // Store initial state for each form
    const formStates = new Map();
    const forms = document.querySelectorAll('.config-tab-panel form');
    forms.forEach(form => {
        formStates.set(form, captureFormState(form));
    });

    // -- Unsaved changes banner ------------------------------------------------

    let banner = null;

    function createBanner() {
        if (banner) return banner;
        banner = document.createElement('div');
        banner.className = 'unsaved-banner';
        banner.innerHTML = `
            <span class="unsaved-banner-text">You have unsaved changes</span>
            <div class="unsaved-banner-actions">
                <button type="button" class="button small" id="banner-cancel">Cancel</button>
                <button type="button" class="button primary small" id="banner-save">Save Changes</button>
            </div>
        `;
        document.body.appendChild(banner);

        banner.querySelector('#banner-cancel').addEventListener('click', () => {
            if (activeDirtyForm) {
                const initial = formStates.get(activeDirtyForm);
                if (initial) restoreFormState(activeDirtyForm, initial);
                hideBanner();
                showToast('success', 'Changes discarded.');
            }
        });

        banner.querySelector('#banner-save').addEventListener('click', () => {
            if (activeDirtyForm) {
                activeDirtyForm.requestSubmit();
            }
        });

        return banner;
    }

    let activeDirtyForm = null;

    function showBanner(form) {
        activeDirtyForm = form;
        const b = createBanner();
        requestAnimationFrame(() => b.classList.add('visible'));
    }

    function hideBanner() {
        activeDirtyForm = null;
        if (banner) banner.classList.remove('visible');
    }

    function checkDirty(form) {
        const initial = formStates.get(form);
        if (!initial) return;
        if (isFormDirty(form, initial)) {
            showBanner(form);
        } else {
            if (activeDirtyForm === form) hideBanner();
        }
    }

    // Listen for changes on all form elements
    forms.forEach(form => {
        form.addEventListener('input', () => checkDirty(form));
        form.addEventListener('change', () => checkDirty(form));
    });

    // -- Cancel button (inline, per-section) -----------------------------------

    document.querySelectorAll('.cancel-btn').forEach(btn => {
        btn.addEventListener('click', () => {
            const form = btn.closest('form');
            if (!form) return;
            const initial = formStates.get(form);
            if (!initial) { form.reset(); return; }
            restoreFormState(form, initial);
            hideBanner();
            showToast('success', 'Changes discarded.');
        });
    });

    // -- Async form submission -------------------------------------------------

    forms.forEach(form => {
        form.addEventListener('submit', async (e) => {
            e.preventDefault();
            const btn = form.querySelector('button[type="submit"]');
            if (btn) btn.disabled = true;

            try {
                const resp = await fetch(form.action, {
                    method: 'POST',
                    headers: { 'Accept': 'application/json' },
                    body: new URLSearchParams(new FormData(form)),
                });
                const data = await resp.json();
                if (data.ok) {
                    showToast('success', data.message || 'Settings saved.');
                    formStates.set(form, captureFormState(form));
                    hideBanner();
                    const section = form.querySelector('input[name="_section"]');
                    if (section && section.value === 'flags') {
                        setTimeout(() => location.reload(), 400);
                    }
                } else {
                    showToast('error', data.error || 'Failed to save settings.');
                }
            } catch {
                showToast('error', 'Network error. Please try again.');
            } finally {
                if (btn) btn.disabled = false;
            }
        });
    });

    // -- Toast notifications ---------------------------------------------------

    function showToast(level, message) {
        const toast = document.createElement('div');
        toast.className = 'toast toast-' + level;
        toast.textContent = message;
        document.body.appendChild(toast);
        requestAnimationFrame(() => toast.classList.add('visible'));
        setTimeout(() => {
            toast.classList.remove('visible');
            setTimeout(() => toast.remove(), 300);
        }, 3000);
    }

    window.showToast = showToast;
})();
