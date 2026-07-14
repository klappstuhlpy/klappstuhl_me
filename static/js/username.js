/* This file is licensed under AGPL-3.0 */
/* Live "is this username free?" hint, shared by the signup form and the account
   rename dialog.

   Both ask GET /account/username/check, which runs the *same* rules the POST
   enforces (format, reserved names, taken, and names another account released
   recently) — so the hint can never promise a name the submit would refuse. It
   is advisory only: the server re-checks under a transaction, and a name can be
   taken between the hint and the submit.

   Opt in from HTML: put `data-username-status="<id of the status element>"` on
   the input, and optionally `data-username-submit="<id of the submit button>"`
   to have it disabled while the name is known to be unavailable. */

(() => {
  const PATTERN = /^[a-z0-9._-]{3,32}$/;
  const DEBOUNCE_MS = 350;

  function wire(input) {
    const status = document.getElementById(input.dataset.usernameStatus);
    if (!status) return;
    const submit = input.dataset.usernameSubmit
      ? document.getElementById(input.dataset.usernameSubmit)
      : null;

    let timer = null;
    let inFlight = null;

    const render = (state, message) => {
      status.textContent = message;
      status.className = `username-status${state ? ` ${state}` : ''}`;
      // Only a *known* refusal blocks the button. On a network error we say
      // nothing and let the server be the one to refuse.
      if (submit) submit.disabled = state === 'bad';
    };

    const check = async (name) => {
      inFlight?.abort();
      const controller = new AbortController();
      inFlight = controller;
      try {
        const response = await fetch(`/account/username/check?name=${encodeURIComponent(name)}`, {
          headers: { accept: 'application/json' },
          signal: controller.signal,
        });
        if (!response.ok) throw new Error(response.statusText);
        const result = await response.json();
        // The field moved on while we were waiting — this answer is about a
        // name the user is no longer typing.
        if (input.value.trim() !== name) return;
        render(result.available ? 'ok' : 'bad', result.message);
      } catch (e) {
        if (e.name === 'AbortError') return;
        render('', '');
      }
    };

    input.addEventListener('input', () => {
      clearTimeout(timer);
      inFlight?.abort();
      const name = input.value.trim();

      if (!name) return render('', '');
      // Say the obvious thing without a round trip. The endpoint would answer
      // this identically; there is just no reason to ask it on every keystroke.
      if (!PATTERN.test(name)) {
        return render(
          'bad',
          '3-32 characters, lowercase letters, digits, dot, dash or underscore.'
        );
      }

      render('checking', 'Checking availability…');
      timer = setTimeout(() => check(name), DEBOUNCE_MS);
    });
  }

  document.querySelectorAll('input[data-username-status]').forEach(wire);
})();
