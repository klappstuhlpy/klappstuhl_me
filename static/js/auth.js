/* This file is licensed under AGPL-3.0 */
const username = document.getElementById('username');

async function invalidateToken(button, token) {
  await fetch('/account/invalidate', {
    method: 'POST',
    headers: {
      'content-type': 'application/json',
    },
    body: JSON.stringify({
      session_id: token,
    })
  });
  let element = button.parentElement;
  let parent = element.parentElement;
  console.log(element, parent);
  parent.removeChild(element);
  if(parent.childElementCount === 0) {
    window.location.reload();
  }
}

username?.addEventListener('input', () => {
  if (username.validity.patternMismatch) {
    username.setCustomValidity("Must be all lowercase letters, numbers, or .-_ characters");
  } else {
    username.setCustomValidity("");
  }
});

document.querySelectorAll('.password-icon').forEach(el => {
  el.addEventListener('click', () => {
    let input = el.previousElementSibling;
    let img = el.firstElementChild;
    if(input.type === 'password') {
      input.type = 'text';
      img.src = '/static/img/visibility_off.svg';
    } else {
      input.type = 'password';
      img.src = '/static/img/visibility.svg';
    }
  })
})

document.getElementById('change-password')?.addEventListener('click', () => {
  document.getElementById('change-password-modal').showModal();
});
document.querySelector('#change-password-modal .button[formmethod="dialog"]')?.addEventListener('click', () => {
  document.getElementById('change-password-modal').close();
});

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

document.getElementById('disable-2fa')?.addEventListener('click', () => {
  document.getElementById('disable-2fa-modal').showModal();
});
document.querySelector('#disable-2fa-modal .button[formmethod="dialog"]')?.addEventListener('click', () => {
  document.getElementById('disable-2fa-modal').close();
});

document.getElementById('session-description')?.setAttribute('value', deviceDescription());

document.querySelectorAll('.invalidate[data-token]').forEach(el => {
  const token = el.dataset.token;
  el.removeAttribute('data-token');
  el.addEventListener('click', () => invalidateToken(el, token));
});

/* ── API key reveal toggle ───────────────────────────────────── */
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
  // .value because the API key now lives in an <input>, not a <code> node.
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
  let response = await callApi('/account/api_key', {
    method: 'POST',
    headers: {
      'content-type': 'application/json',
    },
    body: JSON.stringify({
      new: e.target.getAttribute('new') === 'true',
      scopes,
    })
  });
  let apiKey = document.getElementById('api-key');
  if(apiKey === null) {
    window.location.reload();
  } else {
    apiKey.value = response.token;
    showAlert({level: 'success', content: 'Successfully regenerated API key.'})
  }
})