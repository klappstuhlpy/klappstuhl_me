/* This file is licensed under AGPL-3.0 */
/* Behaviour for the account shell (/account/*), the 2FA setup page, and the
   recovery-code page. Every hook is optional — the same file is loaded on all
   of them and each page only wires up what it actually renders.

   `callApi`, `showAlert` and `deviceDescription` come from base.js. */

/* ── Sidebar (mobile collapse) ───────────────────────────────── */
(() => {
  const sidebar = document.getElementById('account-sidebar');
  const toggle = sidebar?.querySelector('.account-sidebar-toggle');
  toggle?.addEventListener('click', () => {
    const open = sidebar.classList.toggle('open');
    toggle.setAttribute('aria-expanded', String(open));
  });
})();

/* ── Password reveal toggles (shared by every dialog) ────────── */
document.querySelectorAll('.password-icon').forEach((el) => {
  el.addEventListener('click', () => {
    const input = el.previousElementSibling;
    const img = el.firstElementChild;
    if (input.type === 'password') {
      input.type = 'text';
      img.src = '/static/img/visibility_off.svg';
    } else {
      input.type = 'password';
      img.src = '/static/img/visibility.svg';
    }
  });
});

/* Opens the dialog `dialogId` from the button `buttonId`, and wires its Cancel. */
function wireModal(buttonId, dialogId) {
  const dialog = document.getElementById(dialogId);
  if (!dialog) return null;
  document.getElementById(buttonId)?.addEventListener('click', () => dialog.showModal());
  dialog.querySelector('.button[formmethod="dialog"]')?.addEventListener('click', () => dialog.close());
  return dialog;
}

/* ── Security page ───────────────────────────────────────────── */

wireModal('change-password', 'change-password-modal');
wireModal('disable-2fa', 'disable-2fa-modal');
wireModal('regen-codes', 'regen-codes-modal');

/* Label the session this password change creates, so it isn't "No description". */
document.getElementById('session-description')?.setAttribute('value', deviceDescription());

/* Keep the confirm field in lock-step with the new password so the browser
   blocks submission (with a native message) on a mismatch before it hits the
   server, which validates the match again. */
(() => {
  const newPw = document.getElementById('new-password');
  const confirmPw = document.getElementById('confirm-password');
  if (!newPw || !confirmPw) return;
  const sync = () => {
    confirmPw.setCustomValidity(confirmPw.value !== newPw.value ? 'Passwords do not match.' : '');
  };
  newPw.addEventListener('input', sync);
  confirmPw.addEventListener('input', sync);
})();

/* ── Recovery codes: download as a text file ─────────────────── */
document.getElementById('dl-codes')?.addEventListener('click', () => {
  const codes = Array.from(document.querySelectorAll('.twofa-codes li'))
    .map((li) => li.textContent.trim())
    .filter(Boolean);
  const text = 'Klappstuhl.me 2FA recovery codes\n'
    + 'Keep these safe. Each code can be used once.\n\n'
    + codes.join('\n') + '\n';
  const blob = new Blob([text], { type: 'text/plain' });
  const a = document.createElement('a');
  a.href = URL.createObjectURL(blob);
  a.download = 'klappstuhl-recovery-codes.txt';
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(a.href);
});

/* ── Sessions page ───────────────────────────────────────────── */

async function invalidateSession(button, token) {
  await fetch('/account/invalidate', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ session_id: token }),
  });
  const row = button.closest('.session');
  const list = row?.parentElement;
  row?.remove();
  // Emptying the list leaves a heading pointing at nothing — reload instead.
  if (list && list.childElementCount === 0) window.location.reload();
}

document.querySelectorAll('.invalidate[data-token]').forEach((el) => {
  const token = el.dataset.token;
  el.removeAttribute('data-token');
  el.addEventListener('click', () => invalidateSession(el, token));
});

(() => {
  const dialog = document.getElementById('rename-session-modal');
  if (!dialog) return;
  const form = document.getElementById('rename-session-form');
  const input = document.getElementById('session-name');
  let target = null; // the .rename button whose session we're editing

  document.querySelectorAll('.rename[data-token]').forEach((el) => {
    el.addEventListener('click', () => {
      target = el;
      input.value = el.dataset.label === 'No description' ? '' : el.dataset.label;
      dialog.showModal();
      input.focus();
    });
  });

  document.getElementById('rename-session-cancel')?.addEventListener('click', () => dialog.close());

  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    const description = input.value.trim();
    if (!description || !target) return;
    const response = await fetch('/account/sessions/rename', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ session_id: target.dataset.token, description }),
    });
    if (!response.ok) {
      showAlert({ level: 'error', content: 'Could not rename that session.' });
      return;
    }
    // Patch the row in place rather than reloading the page under the user.
    const label = target.closest('.session')?.querySelector('.session-label');
    if (label) label.textContent = description;
    target.dataset.label = description;
    dialog.close();
    showAlert({ level: 'success', content: 'Session renamed.' });
  });
})();

/* ── API page ────────────────────────────────────────────────── */

document.getElementById('api-key-reveal')?.addEventListener('click', (e) => {
  e.preventDefault();
  const input = document.getElementById('api-key');
  const img = e.currentTarget.querySelector('img');
  if (input.type === 'password') {
    input.type = 'text';
    if (img) img.src = '/static/img/visibility_off.svg';
  } else {
    input.type = 'password';
    if (img) img.src = '/static/img/visibility.svg';
  }
});

/* Copy the token and flash the blur/fade "Copied" overlay on the field. */
let apiKeyCopiedTimer = null;
async function copyApiKey() {
  const input = document.getElementById('api-key');
  const key = input ? input.value : '';
  if (!key) return;
  await navigator.clipboard.writeText(key);
  const field = input.closest('.api-key-field');
  if (field) {
    field.classList.add('copied');
    clearTimeout(apiKeyCopiedTimer);
    apiKeyCopiedTimer = setTimeout(() => field.classList.remove('copied'), 1200);
  }
}

// The input itself acts as a copy button (readonly, cursor: pointer).
document.getElementById('api-key')?.addEventListener('click', copyApiKey);
document.getElementById('copy-api-key')?.addEventListener('click', copyApiKey);

document.querySelector('#api-section button[type=submit][name="new"]')?.addEventListener('click', async (e) => {
  e.preventDefault();
  // Collect ticked scope checkboxes so the server stores them on the
  // new session row. Empty array = legacy / unscoped (full access).
  const scopes = Array.from(
    document.querySelectorAll('#api-section input[name="scope"]:checked')
  ).map((c) => c.value);
  const response = await callApi('/account/api_key', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({
      new: e.target.value === 'true',
      scopes,
    }),
  });
  const apiKey = document.getElementById('api-key');
  if (apiKey === null) {
    window.location.reload();
  } else {
    apiKey.value = response.token;
    showAlert({ level: 'success', content: 'Successfully regenerated API key.' });
  }
});

/* ── Danger zone ─────────────────────────────────────────────── */

(() => {
  const dialog = wireModal('delete-account', 'delete-account-modal');
  if (!dialog) return;
  const username = document.getElementById('delete-username');
  const confirm = document.getElementById('delete-confirm');
  const expected = username.dataset.expect;

  // The submit button stays disabled until the username matches exactly. The
  // server checks this again — this is here so the button can't be hit by
  // reflex, not as the guard.
  const sync = () => {
    confirm.disabled = username.value.trim() !== expected;
  };
  username.addEventListener('input', sync);
  sync();
})();
