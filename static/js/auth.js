/* This file is licensed under AGPL-3.0 */
/* Login + signup behaviour. The account shell's behaviour lives in account.js. */

const username = document.getElementById('username');

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

document.getElementById('session-description')?.setAttribute('value', deviceDescription());
