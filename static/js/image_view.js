/* This file is licensed under AGPL-3.0 */
// Copy-to-clipboard for the image viewer's share buttons. Each button carries
// the snippet to copy in its data-copy attribute (direct link, Markdown, HTML,
// BBCode). On success the button briefly confirms.

async function copyText(text) {
    if (navigator.clipboard && window.isSecureContext) {
        await navigator.clipboard.writeText(text);
        return;
    }
    // Fallback for non-secure contexts (e.g. plain-http localhost).
    const ta = document.createElement('textarea');
    ta.value = text;
    ta.style.position = 'fixed';
    ta.style.opacity = '0';
    document.body.appendChild(ta);
    ta.select();
    try {
        document.execCommand('copy');
    } finally {
        document.body.removeChild(ta);
    }
}

document.querySelectorAll('button.copy-btn[data-copy]').forEach((btn) => {
    btn.addEventListener('click', async () => {
        try {
            await copyText(btn.dataset.copy);
            const original = btn.textContent;
            btn.textContent = 'Copied';
            btn.classList.add('copied');
            setTimeout(() => {
                btn.textContent = original;
                btn.classList.remove('copied');
            }, 1200);
        } catch (_) {
            btn.textContent = 'Failed';
            setTimeout(() => { btn.textContent = btn.title.replace(/^Copy /, ''); }, 1200);
        }
    });
});
