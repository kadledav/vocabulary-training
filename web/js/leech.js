// @ts-check
/**
 * The choices of a problem word's leech screen (design §6.5, R-55), used
 * on the Progress screen and in practice: Keep practising, Recognise only,
 * Lower usefulness (with a picker), Suspend word. Each choice is one
 * `POST /api/words/{id}/actions`, which also acknowledges the flag; the
 * caller is told what happened (with the server's answer, whose `undo`
 * token can undo it).
 */

import { postJson } from './api.js';
import { h } from './dom.js';
import { leechChoices, leechDoneText, lowerUsefulness } from './progress-model.js';

/**
 * @typedef {import('./progress-model.js').LeechActionName} LeechActionName
 * @typedef {{ undo?: string | null }} ActionAnswer
 */

/**
 * Undo a choice (`token`: the `undo` of its answer): the word and its cards
 * exactly as before (`409 undo_unavailable` once the word changed since).
 *
 * @param {number} id the word
 * @param {string} token
 */
export function undoLeechChoice(id, token) {
  return postJson(`/api/words/${id}/undo`, { token });
}

/**
 * The choice buttons for word `word`.
 *
 * @param {{ id: number, headword: string, usefulness: number, goal: 'active' | 'recognise', spelling: boolean }} word
 * @param {(done: { action: LeechActionName, text: string, answer: ActionAnswer }) => void} onDone
 * @returns {HTMLElement}
 */
export function leechActions(word, onDone) {
  const status = h('p', { class: 'status', role: 'status' });
  /** @type {HTMLButtonElement[]} */
  const buttons = [];

  /**
   * @param {LeechActionName} action
   * @param {number} [usefulness] `leech_usefulness`: the new value
   */
  const choose = async (action, usefulness) => {
    for (const b of buttons) b.disabled = true;
    status.classList.remove('error');
    status.textContent = 'Saving…';
    /** @type {import('./api.js').ApiResult<ActionAnswer>} */
    const answer = await postJson(`/api/words/${word.id}/actions`, usefulness === undefined ? { action } : { action, usefulness });
    for (const b of buttons) b.disabled = false;
    if (!answer.ok) {
      status.textContent = answer.message;
      status.classList.add('error');
      return;
    }
    status.textContent = '';
    onDone({ action, text: leechDoneText(action, usefulness), answer: answer.data });
  };

  // "Lower usefulness" opens a row of the lower values (buttons, not a
  // native picker: one tap each, and WebKit's picker breaks the CSP).
  const values = lowerUsefulness(word.usefulness).map((v) => {
    const button = h(
      'button',
      { type: 'button', class: 'step', 'data-testid': `leech-usefulness-${v}`, 'aria-label': `Usefulness ${v}` },
      String(v),
    );
    button.addEventListener('click', () => void choose('leech_usefulness', v));
    buttons.push(button);
    return button;
  });
  const picker = h(
    'div',
    { class: 'leech-lower', role: 'group', 'aria-label': `Lower the usefulness of ${word.headword} (now ${word.usefulness}) to`, hidden: true },
    h('span', { class: 'muted small' }, `Now ${word.usefulness}. Lower to:`),
    values,
  );

  const row = leechChoices(word).map(({ action, label, primary }) => {
    const button = h(
      'button',
      { type: 'button', class: primary ? 'primary' : null, 'data-testid': action.replace('_', '-') },
      label,
    );
    if (action === 'leech_usefulness') {
      button.setAttribute('aria-expanded', 'false');
      button.addEventListener('click', () => {
        picker.hidden = !picker.hidden;
        button.setAttribute('aria-expanded', picker.hidden ? 'false' : 'true');
      });
    } else {
      button.addEventListener('click', () => void choose(action));
    }
    buttons.push(button);
    return button;
  });
  return h('div', { class: 'leech-actions' }, h('div', { class: 'actions' }, row), picker, status);
}
