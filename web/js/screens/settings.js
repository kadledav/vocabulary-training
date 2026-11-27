// @ts-check
/**
 * Settings (design §7.1 screen 10, M4 part): new words per day, block size,
 * theme, rating legend; audio: voice (US/UK), speed, auto-play, and the
 * state of the recorded clips (ready / queued / failed / approximate) with
 * "Retry failed" (M6; without a TTS service the app uses the browser's
 * speech and says so). Every
 * change is saved at once; a number out of range is kept in the field with
 * a message saying the allowed range. Account: change the password (logs
 * out every other device), create the connector token (shown once), "Log
 * out" (this device) and "Log out everywhere" (every device). Export: the
 * feedback for Claude (R-57) and everything as one file (R-86).
 */

import { changePassword, connectorTokenStatus, createConnectorToken, logout, logoutEverywhere, newPasswordProblem } from '../auth.js';
import { getJson, postJson } from '../api.js';
import { forgetClips } from '../clips.js';
import { typedCardsName } from '../day-plan.js';
import { h, keepFocus, mount } from '../dom.js';
import { fetchToday } from '../session.js';
import { loadSettings, saveSettings } from '../settings.js';

/**
 * @typedef {import('../settings.js').Settings} Settings
 * @typedef {import('../settings.js').SettingsPatch} SettingsPatch
 */

/**
 * @typedef {{ enabled: false } | { enabled: true, connected: boolean, voice: 'us' | 'uk', ready: number,
 *             pending: number, failed: number, approximate: number,
 *             storage?: 'ok' | 'cache_full' | 'disk_low', cache_bytes?: number,
 *             cache_limit_bytes?: number }} AudioStatus
 */

/** @param {number} bytes */
const gib = (bytes) => `${(bytes / 2 ** 30).toFixed(1)} GiB`;

/**
 * The line about recorded audio in Settings.
 *
 * @param {AudioStatus | null} status
 * @returns {string}
 */
export function audioStatusText(status) {
  if (status === null) return 'The state of the recorded audio could not be loaded.';
  if (!status.enabled) return 'Recorded audio is not set up on this server: ▶ uses the browser’s own speech.';
  if (!status.connected) return 'The audio service has not answered yet: ▶ uses the browser’s own speech for now.';
  const parts = [`${status.ready} clips ready`];
  if (status.pending > 0) parts.push(`${status.pending} being made`);
  if (status.failed > 0) parts.push(`${status.failed} failed`);
  let text = `Recorded audio: ${parts.join(', ')}.`;
  if (status.approximate > 0) {
    text += ` ${status.approximate} ${status.approximate === 1 ? 'has' : 'have'} an approximate pronunciation (a word not in the dictionaries).`;
  }
  if (status.storage === 'cache_full') {
    text += ` No new clips are made: the audio cache is full (${gib(status.cache_bytes ?? 0)} of ${gib(status.cache_limit_bytes ?? 0)}).`;
  } else if (status.storage === 'disk_low') {
    text += ' No new clips are made: the server’s disk is nearly full.';
  }
  if (status.pending > 0) text += ' Until a clip is ready, ▶ uses the browser’s own speech.';
  return text;
}

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
 * A whole-number field saved on change. A value outside `min`–`max` is not
 * saved: the field keeps it and says what is allowed (bug 11).
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
  const problem = h('p', { class: 'field-error', id: `${id}-problem`, role: 'alert', 'data-testid': `${id}-problem` });
  const input = h('input', {
    type: 'number',
    id,
    name: id,
    min: String(min),
    max: String(max),
    step: '1',
    inputmode: 'numeric',
    value: String(value),
    'aria-describedby': `${id}-help ${id}-problem`,
  });
  input.addEventListener('change', () => {
    const n = Number(input.value);
    if (input.value.trim() !== '' && Number.isInteger(n) && n >= min && n <= max) {
      problem.textContent = '';
      input.removeAttribute('aria-invalid');
      onChange(n);
    } else {
      problem.textContent = `Enter a whole number from ${min} to ${max}. Not saved.`;
      input.setAttribute('aria-invalid', 'true');
    }
  });
  return h(
    'div',
    { class: 'setting' },
    h('label', { for: id }, `${label} (${min}–${max})`),
    input,
    problem,
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
 * The two exports (design §8.1): the feedback for Claude (R-57), shown to
 * copy or downloaded, and everything as one file (R-86).
 *
 * @param {import('../router.js').IsCurrent} isCurrent
 */
function exportsSection(isCurrent) {
  const shown = h('div', { class: 'feedback-export' });
  const status = h('p', { class: 'status', role: 'status', 'data-testid': 'feedback-status' });
  const show = h('button', { type: 'button', 'data-testid': 'show-feedback' }, 'Show the feedback to copy');
  show.addEventListener('click', () => {
    show.disabled = true;
    status.textContent = 'Loading…';
    void (async () => {
      /** @type {import('../api.js').ApiResult<unknown>} */
      const answer = await getJson('/api/export/feedback');
      if (!isCurrent()) return;
      show.disabled = false;
      if (!answer.ok) {
        status.textContent = answer.message;
        return;
      }
      status.textContent = '';
      const text = h('textarea', {
        readonly: true,
        rows: 10,
        spellcheck: 'false',
        'aria-label': 'Feedback for Claude',
        'data-testid': 'feedback-text',
      });
      text.value = `${JSON.stringify(answer.data, null, 2)}\n`;
      const copy = h('button', { type: 'button', class: 'primary', 'data-testid': 'copy-feedback' }, 'Copy');
      copy.addEventListener('click', () => {
        text.select();
        void navigator.clipboard?.writeText(text.value).then(
          () => {
            copy.textContent = 'Copied';
          },
          () => {
            copy.textContent = 'Select and copy it by hand';
          },
        );
      });
      mount(shown, text, h('div', { class: 'actions' }, copy));
    })();
  });
  return h(
    'section',
    { class: 'settings exports', 'aria-labelledby': 'exports-title' },
    h('h2', { id: 'exports-title' }, 'Export'),
    h('h3', {}, 'Feedback for Claude'),
    h(
      'p',
      { class: 'muted small' },
      'Your problem words, the sentences you hid or corrected, the answers you added and the values you changed — vocabulary only. Paste it into your Claude project and ask for the next export: Claude then writes hints and better sentences for these words.',
    ),
    h(
      'div',
      { class: 'actions' },
      show,
      h('a', { class: 'button', href: '/api/export/feedback', download: '', 'data-testid': 'download-feedback' }, 'Download as a file'),
    ),
    status,
    shown,
    h('h3', {}, 'Everything'),
    h(
      'p',
      { class: 'muted small' },
      'All words and sources, your changes, every card and your whole practice history, as one JSON file (format “vocab-full-export”, version 1). Keep it as a copy or to move your data; the server’s regular backup is its own snapshot.',
    ),
    h('a', { class: 'button', href: '/api/export/full', download: '', 'data-testid': 'download-full' }, 'Download everything'),
  );
}

/**
 * @param {HTMLElement} root
 * @param {import('../router.js').IsCurrent} isCurrent
 */
export function renderSettings(root, isCurrent) {
  const status = h('p', { class: 'status', role: 'status', 'data-testid': 'settings-status' });
  const form = h('div', { class: 'settings' }, h('p', { class: 'muted' }, 'Loading…'));
  /**
   * @param {string} testId
   * @param {string} label
   * @param {() => Promise<import('../api.js').ApiResult<unknown>>} call
   */
  const logoutButton = (testId, label, call) => {
    const button = h('button', { type: 'button', 'data-testid': testId }, label);
    button.addEventListener('click', () => {
      button.disabled = true;
      void (async () => {
        const answer = await call();
        button.disabled = false;
        if (!isCurrent()) return;
        if (answer.ok) window.location.replace('#/login');
        else status.textContent = answer.message;
      })();
    });
    return button;
  };
  const account = h(
    'section',
    { class: 'settings account' },
    h('h2', {}, 'Account'),
    h(
      'div',
      { class: 'stack' },
      logoutButton('logout', 'Log out', logout),
      h('p', { class: 'muted small' }, 'Ends the session on this device only.'),
      logoutButton('logout-all', 'Log out everywhere', logoutEverywhere),
      h('p', { class: 'muted small' }, 'Ends the sessions on every device, this one included — for a lost phone.'),
    ),
    passwordForm(isCurrent),
    connectorTokenSection(isCurrent),
  );
  mount(root, h('h1', {}, 'Settings'), form, exportsSection(isCurrent), account, status);

  /** @param {SettingsPatch} patch */
  const save = async (patch) => {
    status.textContent = 'Saving…';
    const result = await saveSettings(patch);
    if (!isCurrent()) return;
    status.textContent = result.ok ? 'Saved.' : result.message;
    if (result.ok && patch.voice) forgetClips();
    if (result.ok) keepFocus(form, () => draw(result.settings));
    if (result.ok && patch.voice) void loadAudio();
  };

  const audioLine = h('p', { class: 'muted small', 'data-testid': 'audio-status', role: 'status' }, 'Loading…');
  const retry = h('button', { type: 'button', 'data-testid': 'audio-retry', hidden: true }, 'Retry failed clips');
  retry.addEventListener('click', () => {
    retry.disabled = true;
    void postJson('/api/audio/retry', {}).then(() => {
      retry.disabled = false;
      if (isCurrent()) void loadAudio();
    });
  });
  const loadAudio = async () => {
    /** @type {import('../api.js').ApiResult<AudioStatus>} */
    const answer = await getJson('/api/audio/status');
    if (!isCurrent()) return;
    const audio = answer.ok ? answer.data : null;
    audioLine.textContent = audioStatusText(audio);
    retry.hidden = !(audio && audio.enabled && audio.failed > 0);
  };

  /** @param {Settings} s */
  const draw = (s) => {
    mount(
      form,
      h('h2', {}, 'Practice'),
      numberField(
        'new-per-day',
        'New words per day',
        'Many new words today mean many reviews over the next days. New words pause by themselves when reviews pile up.',
        s.new_per_day,
        0,
        100,
        (n) => void save({ new_per_day: n }),
      ),
      numberField(
        'new-typed-per-day',
        `New ${typedName} cards per day`,
        typedName === 'Produce'
          ? 'A word you understand well gets a Produce card (you type the English word for a sentence). This many of them come for the first time each day; the others wait for a later day.'
          : 'A word you understand well gets a Produce card (you type the English word for a sentence), and a word with a tricky spelling a Spelling card (you type the word you hear). This many of them come for the first time each day; the others wait for a later day.',
        s.new_typed_per_day,
        0,
        100,
        (n) => void save({ new_typed_per_day: n }),
      ),
      numberField('block-size', 'Cards per block', '"Continue" adds another block of this size.', s.block_size, 5, 50, (n) =>
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
      audioLine,
      retry,
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

  /** What the typed cards are called (Today knows whether Spelling exists). */
  let typedName = typedCardsName(null);

  void (async () => {
    const [settings, today] = await Promise.all([loadSettings(), fetchToday(null)]);
    if (today.ok) typedName = typedCardsName(today.data);
    if (isCurrent()) draw(settings);
    await loadAudio();
  })();
}
