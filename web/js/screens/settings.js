// @ts-check
/**
 * Settings (design §7.1 screen 10, M4 part): new words per day, block size,
 * theme, rating legend; voice, speed and auto-play are stored for the
 * neural audio of M6 (browser speech already uses voice and speed). Every
 * change is saved at once. Account: change the password (logs out every
 * other device), create the connector token (shown once), and "Log out",
 * which ends this device's session.
 */

import { changePassword, connectorTokenStatus, createConnectorToken, logout, newPasswordProblem } from '../auth.js';
import { h, mount } from '../dom.js';
import { loadSettings, saveSettings } from '../settings.js';

/**
 * @typedef {import('../settings.js').Settings} Settings
 * @typedef {import('../settings.js').SettingsPatch} SettingsPatch
 */

/**
 * A group of radio buttons.
 *
 * @param {string} name
 * @param {string} label
 * @param {[string, string][]} options value, text
 * @param {string} current
 * @param {(value: string) => void} onChange
 */
function radioGroup(name, label, options, current, onChange) {
  return h(
    'fieldset',
    { class: 'setting choice' },
    h('legend', {}, label),
    options.map(([value, text]) =>
      h(
        'label',
        { class: 'choice-option' },
        h('input', { type: 'radio', name, value, checked: value === current, onchange: () => onChange(value) }),
        ' ',
        text,
      ),
    ),
  );
}

/**
 * A whole-number field saved on change.
 *
 * @param {string} id
 * @param {string} label
 * @param {string} help
 * @param {number} value
 * @param {number} min
 * @param {number} max
 * @param {(value: number) => void} onChange
 */
function numberField(id, label, help, value, min, max, onChange) {
  const input = h('input', {
    type: 'number',
    id,
    min: String(min),
    max: String(max),
    step: '1',
    inputmode: 'numeric',
    value: String(value),
    'aria-describedby': `${id}-help`,
  });
  input.addEventListener('change', () => {
    const n = Number(input.value);
    if (Number.isInteger(n) && n >= min && n <= max) onChange(n);
    else input.value = String(value);
  });
  return h(
    'div',
    { class: 'setting' },
    h('label', { for: id }, label),
    input,
    h('p', { class: 'muted small', id: `${id}-help` }, help),
  );
}

/**
 * Change password: current, new, new again.
 *
 * @param {import('../router.js').IsCurrent} isCurrent
 */
function passwordForm(isCurrent) {
  const field = (/** @type {string} */ id, /** @type {string} */ label, /** @type {string} */ autocomplete) => {
    const input = h('input', { type: 'password', id, autocomplete, required: true });
    return { input, nodes: [h('label', { for: id }, label), input] };
  };
  const current = field('current-password', 'Current password', 'current-password');
  const next = field('new-password', 'New password', 'new-password');
  const repeated = field('repeat-password', 'Repeat the new password', 'new-password');
  const submit = h('button', { type: 'submit' }, 'Change password');
  const status = h('p', { class: 'status', role: 'status', 'data-testid': 'password-status' });

  /** @param {Event} event */
  const onSubmit = async (event) => {
    event.preventDefault();
    status.classList.remove('error');
    const problem =
      current.input.value === '' ? 'Enter the current password.' : newPasswordProblem(next.input.value, repeated.input.value);
    if (problem) {
      status.textContent = problem;
      status.classList.add('error');
      return;
    }
    submit.disabled = true;
    status.textContent = 'Changing…';
    const answer = await changePassword(current.input.value, next.input.value);
    if (!isCurrent()) return;
    submit.disabled = false;
    if (answer.ok) {
      for (const f of [current, next, repeated]) f.input.value = '';
      status.textContent = 'Password changed. Every other device is logged out.';
      return;
    }
    status.textContent = answer.code === 'invalid_password' ? 'The current password is wrong.' : answer.message;
    status.classList.add('error');
  };

  return h(
    'form',
    { class: 'credentials', onsubmit: (/** @type {Event} */ e) => void onSubmit(e), novalidate: true },
    h('h3', {}, 'Change password'),
    current.nodes,
    next.nodes,
    repeated.nodes,
    h('p', { class: 'muted small' }, 'At least 12 characters. Every other device is logged out.'),
    submit,
    status,
  );
}

/**
 * Connector token for the Claude Desktop connector: shown once when made.
 *
 * @param {import('../router.js').IsCurrent} isCurrent
 */
function connectorTokenSection(isCurrent) {
  const state = h('p', { class: 'muted small', 'data-testid': 'connector-status' }, 'Loading…');
  const shown = h('div', { class: 'connector-token' });
  const password = h('input', { type: 'password', id: 'token-password', autocomplete: 'current-password' });
  const create = h('button', { type: 'submit', 'data-testid': 'create-token' }, 'Create token');
  const problem = h('p', { class: 'status', role: 'status', 'data-testid': 'token-status' });

  /** @param {{ exists: boolean, created_at: string | null }} s */
  const describe = (s) => {
    state.textContent = s.exists
      ? `A token exists (created ${s.created_at ?? 'earlier'}). Creating a new one replaces it.`
      : 'No token yet: the connector cannot import until you create one.';
  };

  /** @param {Event} event */
  const onSubmit = (event) => {
    event.preventDefault();
    problem.classList.remove('error');
    problem.textContent = '';
    if (password.value === '') {
      problem.textContent = 'Enter your password to create a token.';
      problem.classList.add('error');
      password.focus();
      return;
    }
    create.disabled = true;
    void (async () => {
      const answer = await createConnectorToken(password.value);
      if (!isCurrent()) return;
      create.disabled = false;
      password.value = '';
      if (!answer.ok) {
        problem.textContent = answer.code === 'invalid_password' ? 'The password is wrong.' : answer.message;
        problem.classList.add('error');
        return;
      }
      const value = h('input', {
        type: 'text',
        readonly: true,
        value: answer.data.token,
        'aria-label': 'Connector token',
        'data-testid': 'connector-token',
      });
      const copy = h('button', { type: 'button' }, 'Copy');
      copy.addEventListener('click', () => {
        value.select();
        void navigator.clipboard?.writeText(value.value).then(
          () => {
            copy.textContent = 'Copied';
          },
          () => {
            copy.textContent = 'Select and copy it by hand';
          },
        );
      });
      mount(shown, value, copy, h('p', { class: 'muted small' }, 'Shown only now: paste it into the connector.'));
      describe({ exists: true, created_at: 'just now' });
    })();
  };

  void (async () => {
    const answer = await connectorTokenStatus();
    if (isCurrent() && answer.ok) describe(answer.data);
  })();

  return h(
    'div',
    {},
    h('h3', {}, 'Connector token'),
    h('p', { class: 'muted small' }, 'The Claude Desktop connector uses it to import word packs.'),
    state,
    h(
      'form',
      { class: 'credentials', onsubmit: (/** @type {Event} */ e) => onSubmit(e), novalidate: true },
      h('label', { for: 'token-password' }, 'Your password (to create a token)'),
      password,
      create,
      problem,
    ),
    shown,
  );
}

/**
 * @param {HTMLElement} root
 * @param {import('../router.js').IsCurrent} isCurrent
 */
export function renderSettings(root, isCurrent) {
  const status = h('p', { class: 'status', role: 'status', 'data-testid': 'settings-status' });
  const form = h('div', { class: 'settings' }, h('p', { class: 'muted' }, 'Loading…'));
  const logoutButton = h(
    'button',
    {
      type: 'button',
      'data-testid': 'logout',
      onclick: () => {
        logoutButton.disabled = true;
        void (async () => {
          const answer = await logout();
          logoutButton.disabled = false;
          if (!isCurrent()) return;
          if (answer.ok) window.location.hash = '#/login';
          else status.textContent = answer.message;
        })();
      },
    },
    'Log out',
  );
  const account = h(
    'section',
    { class: 'settings account' },
    h('h2', {}, 'Account'),
    h('p', { class: 'muted small' }, 'Logging out ends the session on this device only.'),
    logoutButton,
    passwordForm(isCurrent),
    connectorTokenSection(isCurrent),
  );
  mount(root, h('h1', {}, 'Settings'), form, account, status);

  /** @param {SettingsPatch} patch */
  const save = async (patch) => {
    status.textContent = 'Saving…';
    const result = await saveSettings(patch);
    if (!isCurrent()) return;
    status.textContent = result.ok ? 'Saved.' : result.message;
    if (result.ok) draw(result.settings);
  };

  /** @param {Settings} s */
  const draw = (s) => {
    mount(
      form,
      h('h2', {}, 'Practice'),
      numberField('new-per-day', 'New words per day', '0–15; paused automatically when reviews pile up.', s.new_per_day, 0, 15, (n) =>
        void save({ new_per_day: n }),
      ),
      numberField('block-size', 'Cards per block', '5–50; "Continue" adds another block.', s.block_size, 5, 50, (n) =>
        void save({ block_size: n }),
      ),
      radioGroup(
        'legend',
        'Rating legend above the buttons',
        [
          ['auto', 'During the first 3 blocks'],
          ['always', 'Always'],
          ['off', 'Off (press ? to see it)'],
        ],
        s.legend,
        (v) => void save({ legend: /** @type {Settings['legend']} */ (v) }),
      ),
      h('h2', {}, 'Look'),
      radioGroup(
        'theme',
        'Theme',
        [
          ['dark', 'Dark'],
          ['light', 'Light'],
          ['system', 'Same as the system'],
        ],
        s.theme,
        (v) => void save({ theme: /** @type {Settings['theme']} */ (v) }),
      ),
      h('h2', {}, 'Audio'),
      h('p', { class: 'muted small' }, 'Until recorded audio arrives, ▶ uses the browser’s own speech.'),
      radioGroup(
        'voice',
        'Voice',
        [
          ['us', 'US English'],
          ['uk', 'UK English'],
        ],
        s.voice,
        (v) => void save({ voice: /** @type {Settings['voice']} */ (v) }),
      ),
      radioGroup(
        'speed',
        'Speed',
        [
          ['1', '1×'],
          ['0.75', '0.75×'],
        ],
        s.speed,
        (v) => void save({ speed: /** @type {Settings['speed']} */ (v) }),
      ),
      h(
        'label',
        { class: 'setting check' },
        h('input', { type: 'checkbox', checked: s.autoplay, onchange: (/** @type {Event} */ e) => void save({ autoplay: /** @type {HTMLInputElement} */ (e.target).checked }) }),
        ' Play the word when the answer is shown (recorded audio only)',
      ),
    );
  };

  void (async () => {
    const settings = await loadSettings();
    if (isCurrent()) draw(settings);
  })();
}
