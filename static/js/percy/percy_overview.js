// Public server overview: keeps the now-playing panel live and wires the
// (voice-gated) playback controls. All mutations are re-authorized server-side.
(function () {
    'use strict';

    const section = document.getElementById('music-section');
    if (!section) return;

    const body = document.getElementById('music-body');
    const controls = document.getElementById('music-controls');
    const hint = document.getElementById('music-hint');

    function renderInactive(text) {
        body.innerHTML =
            '<div class="listener-status"><span class="ls-indicator inactive"></span>' +
            '<span class="text-muted">' + (text || 'No active music session in this server.') + '</span></div>';
    }

    function renderPlaying(np, channel) {
        let html =
            '<div class="listener-status"><span class="ls-indicator active"></span>' +
            '<span>Playing in <strong>' + (channel ? 'voice channel' : 'voice') + '</strong></span></div>';
        if (np && np.title) {
            html +=
                '<div class="now-playing"><span class="np-dot"></span><span><strong>' +
                escapeHtml(np.title) + '</strong> <span class="text-muted">&mdash; ' +
                escapeHtml(np.author || '') + '</span></span></div>';
        } else {
            html += '<p class="text-muted">Connected but nothing playing.</p>';
        }
        body.innerHTML = html;
    }

    function escapeHtml(s) {
        const div = document.createElement('div');
        div.textContent = s;
        return div.innerHTML;
    }

    function setControlVisibility(canControl) {
        if (controls) controls.hidden = !canControl;
        if (hint) hint.hidden = canControl;
    }

    async function refresh() {
        try {
            const res = await fetch(`/percy/dashboard/guild/${GUILD_ID}/overview/music`, {
                headers: { 'Accept': 'application/json' },
            });
            const data = await res.json();
            if (!data.ok) return;
            if (data.active) {
                renderPlaying(data.now_playing, data.channel);
            } else {
                renderInactive();
            }
            setControlVisibility(!!data.can_control);
        } catch (_) {
            /* transient — keep last good render */
        }
    }

    async function control(action) {
        try {
            const res = await fetch(`/percy/dashboard/guild/${GUILD_ID}/overview/music/control`, {
                method: 'POST',
                headers: { 'Content-Type': 'application/json', 'Accept': 'application/json' },
                body: JSON.stringify({ action }),
            });
            const data = await res.json();
            if (data.ok) {
                showToast('success', 'Done.');
                setTimeout(refresh, 600);
            } else {
                showToast('error', data.error || 'Action failed.');
            }
        } catch (_) {
            showToast('error', 'Could not reach the bot.');
        }
    }

    if (controls) {
        controls.querySelectorAll('button[data-action]').forEach((btn) => {
            btn.addEventListener('click', () => control(btn.dataset.action));
        });
    }

    // Poll while the tab is visible; back off when hidden to spare the bot.
    refresh();
    let timer = setInterval(refresh, 8000);
    document.addEventListener('visibilitychange', () => {
        clearInterval(timer);
        if (!document.hidden) {
            refresh();
            timer = setInterval(refresh, 8000);
        }
    });
})();
