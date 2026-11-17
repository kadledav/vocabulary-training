// @ts-check
/**
 * Settings (design §7.1 screen 10, M4 part): new words per day, block size,
 * theme, rating legend; voice, speed and auto-play are stored for the
 * neural audio of M6 (browser speech already uses voice and speed). Every
 * change is saved at once.
 */

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
 * @param {HTMLElement} root
 * @param {import('../router.js').IsCurrent} isCurrent
 */
export function renderSettings(root, isCurrent) {
  const status = h('p', { class: 'status', role: 'status', 'data-testid': 'settings-status' });
  const form = h('div', { class: 'settings' }, h('p', { class: 'muted' }, 'Loading…'));
  mount(root, h('h1', {}, 'Settings'), form, status);

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
