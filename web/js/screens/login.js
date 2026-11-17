// @ts-check
/**
 * Login screen (design §9.1, R-82): one password field. After a successful
 * login the app returns to the screen that asked for it.
 */

import { login, takeReturn } from '../auth.js';
import { h, mount } from '../dom.js';
import { reloadSettings } from '../settings.js';

/**
 * @param {HTMLElement} root
 * @param {import('../router.js').IsCurrent} isCurrent
 */
export function renderLogin(root, isCurrent) {
  const password = h('input', {
    type: 'password',
    id: 'password',
    name: 'password',
    autocomplete: 'current-password',
    required: true,
    autofocus: true,
    'aria-describedby': 'login-status',
  });
  const submit = h('button', { type: 'submit', class: 'primary big' }, 'Log in');
  const status = h('p', { class: 'status', id: 'login-status', role: 'status', 'data-testid': 'login-status' });

  /** @param {Event} event */
  const onSubmit = async (event) => {
    event.preventDefault();
    if (password.value === '') {
      status.textContent = 'Enter the password.';
      password.focus();
      return;
    }
    submit.disabled = true;
    status.classList.remove('error');
    status.textContent = 'Logging in…';
    const answer = await login(password.value);
    if (!isCurrent()) return;
    submit.disabled = false;
    if (answer.ok) {
      password.value = '';
      status.textContent = '';
      void reloadSettings();
      window.location.hash = takeReturn();
      return;
    }
    status.textContent = answer.code === 'invalid_password' ? 'Wrong password. Try again.' : answer.message;
    status.classList.add('error');
    password.select();
  };

  const form = h(
    'form',
    { class: 'login', onsubmit: (/** @type {Event} */ e) => void onSubmit(e), novalidate: true },
    h('label', { for: 'password' }, 'Password'),
    password,
    submit,
    status,
  );
  mount(root, h('h1', {}, 'Vocabulary'), h('p', { class: 'muted' }, 'Log in to practise.'), form);
  password.focus();
}
