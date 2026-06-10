/* Percy Dashboard — Members management */

(function() {
    const tbody = document.getElementById('members-tbody');
    const searchInput = document.getElementById('member-search');
    const loadMoreBtn = document.getElementById('load-more');
    const modal = document.getElementById('action-modal');
    const modalTitle = document.getElementById('modal-title');
    const modalDesc = document.getElementById('modal-description');
    const modalReason = document.getElementById('modal-reason');
    const modalCancel = document.getElementById('modal-cancel');
    const modalConfirm = document.getElementById('modal-confirm');

    let pendingAction = null;

    // Resolve role names from the ROLES global
    const roleMap = {};
    if (typeof ROLES !== 'undefined') {
        ROLES.forEach(r => { roleMap[r.id] = r; });
    }
    document.querySelectorAll('.role-chip').forEach(chip => {
        const role = roleMap[chip.dataset.roleId];
        if (role) {
            chip.textContent = '@' + role.name;
            if (role.color) {
                chip.style.borderColor = '#' + role.color.toString(16).padStart(6, '0');
            }
        }
    });

    // Search filtering
    let searchTimeout;
    searchInput.addEventListener('input', () => {
        clearTimeout(searchTimeout);
        searchTimeout = setTimeout(() => {
            const query = searchInput.value.toLowerCase();
            tbody.querySelectorAll('tr').forEach(row => {
                row.hidden = query && !row.dataset.name.includes(query);
            });
        }, 200);
    });

    // Load more pagination
    loadMoreBtn.addEventListener('click', async () => {
        const rows = tbody.querySelectorAll('tr');
        const lastId = rows.length ? rows[rows.length - 1].dataset.userId : '0';
        loadMoreBtn.disabled = true;

        try {
            const resp = await fetch(`/percy/dashboard/guild/${GUILD_ID}/members.json?after=${lastId}`);
            const data = await resp.json();
            if (data.members && data.members.length > 0) {
                data.members.forEach(m => {
                    const row = document.createElement('tr');
                    row.dataset.userId = m.id;
                    row.dataset.name = m.display_name.toLowerCase();

                    const rolesHtml = m.roles.map(rid => {
                        const role = roleMap[rid];
                        const name = role ? '@' + role.name : rid;
                        const style = role && role.color ? `border-color:#${role.color.toString(16).padStart(6,'0')}` : '';
                        return `<span class="role-chip" style="${style}">${name}</span>`;
                    }).join('');

                    const actionsHtml = m.bot ? '' :
                        `<button class="button small" data-action="kick" data-user-id="${m.id}" data-name="${m.display_name}">Kick</button>` +
                        `<button class="button small danger" data-action="ban" data-user-id="${m.id}" data-name="${m.display_name}">Ban</button>`;

                    row.innerHTML = `
                        <td class="member-cell">
                            <img class="member-avatar" src="${m.avatar_url}?size=32" alt="" width="32" height="32">
                            <div class="member-info">
                                <span class="member-name">${m.display_name}</span>
                                <span class="member-username">${m.name}${m.bot ? ' <span class="badge">BOT</span>' : ''}</span>
                            </div>
                        </td>
                        <td class="member-joined"><time class="js-ts">${m.joined_at || '—'}</time></td>
                        <td class="member-roles">${rolesHtml}</td>
                        <td class="member-actions">${actionsHtml}</td>
                    `;
                    tbody.appendChild(row);
                });
                if (window.formatTimestamps) window.formatTimestamps(tbody);
                if (data.members.length < 100) loadMoreBtn.hidden = true;
            } else {
                loadMoreBtn.hidden = true;
            }
        } catch {
            showToast('error', 'Failed to load members.');
        } finally {
            loadMoreBtn.disabled = false;
        }
    });

    // Action buttons (delegated)
    tbody.addEventListener('click', (e) => {
        const btn = e.target.closest('[data-action]');
        if (!btn) return;

        const action = btn.dataset.action;
        const userId = btn.dataset.userId;
        const name = btn.dataset.name;

        pendingAction = { action, userId };
        modalTitle.textContent = action === 'ban' ? 'Ban Member' : 'Kick Member';
        modalDesc.textContent = `Are you sure you want to ${action} ${name}?`;
        modalReason.value = '';
        modal.hidden = false;
    });

    // Modal handlers
    modalCancel.addEventListener('click', () => {
        modal.hidden = true;
        pendingAction = null;
    });

    modal.addEventListener('click', (e) => {
        if (e.target === modal) {
            modal.hidden = true;
            pendingAction = null;
        }
    });

    modalConfirm.addEventListener('click', async () => {
        if (!pendingAction) return;
        modalConfirm.disabled = true;

        const { action, userId } = pendingAction;
        const reason = modalReason.value.trim() || undefined;

        try {
            const resp = await fetch(`/percy/dashboard/guild/${GUILD_ID}/members/${userId}/action`, {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ action, reason }),
            });
            const data = await resp.json();
            if (data.ok) {
                showToast('success', `Successfully ${action === 'ban' ? 'banned' : 'kicked'} the member.`);
                const row = tbody.querySelector(`tr[data-user-id="${userId}"]`);
                if (row) row.remove();
            } else {
                showToast('error', data.error || `Failed to ${action} member.`);
            }
        } catch {
            showToast('error', 'Network error.');
        } finally {
            modal.hidden = true;
            pendingAction = null;
            modalConfirm.disabled = false;
        }
    });

    // Toast (reuse if loaded from percy-dashboard, otherwise define)
    if (typeof window.showToast === 'undefined') {
        window.showToast = function(level, message) {
            const toast = document.createElement('div');
            toast.className = 'toast toast-' + level;
            toast.textContent = message;
            document.body.appendChild(toast);
            requestAnimationFrame(() => toast.classList.add('visible'));
            setTimeout(() => {
                toast.classList.remove('visible');
                setTimeout(() => toast.remove(), 300);
            }, 3000);
        };
    }
})();
