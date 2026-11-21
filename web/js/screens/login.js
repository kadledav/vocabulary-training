// @ts-check
/**
 * Login screen (design §9.1, R-82): one password field. While the app has
 * no password yet it shows the first-run setup instead: the setup code from
 * the server log plus the new password, twice. After a successful login or
 * setup the app returns to the screen that asked for it.
 */

import { leaveLogin, login, newPasswordProblem, setUp, setupStatus } from '../auth.js';
import { h, mount } from '../dom.js';
import { reloadSettings } from '../settings.js';

/**
 * @param {Record<string, string | boolean>} attrs
 */
function passwordInput(attrs) {
  return h('input', { type: 'password', required: true, ...attrs });
}

/** Back to the screen that asked for the login. */
function done() {
  void reloadSettings();
  leaveLogin();
}

/**
 * @param {HTMLElement} root
 * @param {import('../router.js').IsCurrent} isCurrent
 */
export function renderLogin(root, isCurrent) {
  // Ask first, so a first run never flashes the login form (bug 12).
  mount(root, h('p', { class: 'muted' }, 'Loading…'));
  void (async () => {
    const answer = await setupStatus();
    if (!isCurrent()) return;
    if (answer.ok && answer.data.required) drawSetup(root, isCurrent);
    else drawLogin(root, isCurrent);
  })();
}

/**
 * @param {HTMLElement} root
 * @param {import('../router.js').IsCurrent} isCurrent
 */
function drawLogin(root, isCurrent) {
  const password = passwordInput({
    id: 'password',
    name: 'password',
    autocomplete: 'current-password',
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
      done();
      return;
    }
    if (answer.code === 'setup_required') {
      drawSetup(root, isCurrent);
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

/**
 * First run: setup code (from the app's log) and the new password.
 *
 * @param {HTMLElement} root
 * @param {import('../router.js').IsCurrent} isCurrent
 */
function drawSetup(root, isCurrent) {
  const code = h('input', {
    type: 'text',
    id: 'setup-code',
    name: 'setup-code',
    autocomplete: 'one-time-code',
    autocapitalize: 'characters',
    spellcheck: 'false',
    required: true,
    placeholder: 'XXXX-XXXX-XX',
  });
  const password = passwordInput({ id: 'new-password', name: 'new-password', autocomplete: 'new-password' });
  const repeated = passwordInput({ id: 'repeat-password', name: 'repeat-password', autocomplete: 'new-password' });
  const submit = h('button', { type: 'submit', class: 'primary big' }, 'Set up');
  const status = h('p', { class: 'status', role: 'status', 'data-testid': 'setup-status' });

  /** @param {string} message */
  const fail = (message) => {
    status.textContent = message;
    status.classList.add('error');
  };

  /** @param {Event} event */
  const onSubmit = async (event) => {
    event.preventDefault();
    status.classList.remove('error');
    if (code.value.trim() === '') {
      fail('Enter the setup code from the server log.');
      code.focus();
      return;
    }
    const problem = newPasswordProblem(password.value, repeated.value);
    if (problem) {
      fail(problem);
      password.focus();
      return;
    }
    submit.disabled = true;
    status.textContent = 'Setting up…';
    const answer = await setUp(code.value.trim(), password.value);
    if (!isCurrent()) return;
    submit.disabled = false;
    if (answer.ok) {
      status.textContent = '';
      done();
      return;
    }
    if (answer.code === 'already_set_up') {
      drawLogin(root, isCurrent);
      return;
    }
    fail(answer.code === 'invalid_setup_code' ? 'Wrong setup code. Check the server log and try again.' : answer.message);
    if (answer.code === 'invalid_setup_code') code.select();
  };

  const form = h(
    'form',
    { class: 'login', onsubmit: (/** @type {Event} */ e) => void onSubmit(e), novalidate: true },
    h('label', { for: 'setup-code' }, 'Setup code'),
    code,
    h('label', { for: 'new-password' }, 'New password'),
    password,
    h('label', { for: 'repeat-password' }, 'Repeat the new password'),
    repeated,
    submit,
    status,
  );
  mount(
    root,
    h('h1', {}, 'Set up'),
    h('p', {}, 'The setup code is in the app’s log. Enter it and choose a password of at least 12 characters.'),
    h('p', { class: 'muted small' }, 'To see the log on the server: ', h('code', {}, 'docker logs vocabulary')),
    form,
  );
  code.focus();
}
