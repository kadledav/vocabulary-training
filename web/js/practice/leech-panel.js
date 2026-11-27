// @ts-check
/**
 * The leech screen in practice (design §6.5, §7.1 screen 4; R-55): when a
 * flagged card is revealed, the answer side opens with this panel — the
 * word keeps slipping, its memory hint (or that none exists yet), the
 * words it is easily confused with, and the choices of `leech.js`. The
 * answer and the rating buttons stay below, so "Keep practising" is just
 * rating as usual. The word's usefulness and goal (which choices exist)
 * come from the word itself, fetched when the panel opens.
 */

import { getJson } from '../api.js';
import { h, mount } from '../dom.js';
import { leechActions } from '../leech.js';

/**
 * @typedef {import('../session.js').Item} Item
 * @typedef {{ action: import('../progress-model.js').LeechActionName, text: string, undo: string | null }} LeechDone
 */

/**
 * The word's usefulness and goal, asked once per word while the panel is
 * redrawn (More, help); forgotten when a choice changes them.
 *
 * @type {Map<number, Promise<import('../api.js').ApiResult<{ usefulness: number, effective_goal: 'active' | 'recognise' }>>>}
 */
const facts = new Map();

/** @param {number} id */
function wordFacts(id) {
  let answer = facts.get(id);
  if (!answer) {
    answer = getJson(`/api/words/${id}`);
    facts.set(id, answer);
    void answer.then((a) => {
      if (!a.ok) facts.delete(id);
    });
  }
  return answer;
}

/**
 * The panel for `item`. `done` is the confirmation of a choice made
 * earlier in this visit (the panel then only shows it).
 *
 * @param {Item} item
 * @param {LeechDone | null} done
 * @param {(done: LeechDone) => void} onDone
 * @param {import('../router.js').IsCurrent} isCurrent
 * @returns {HTMLElement}
 */
export function leechPanel(item, done, onDone, isCurrent) {
  const word = item.word;
  if (done) {
    return h('section', { class: 'leech-panel', 'data-testid': 'leech-panel' }, h('p', { class: 'leech-done', role: 'status', 'data-testid': 'leech-done' }, done.text));
  }
  const choices = h('div', { class: 'leech-choices' }, h('p', { class: 'muted small' }, 'Loading the choices…'));
  void (async () => {
    const answer = await wordFacts(word.id);
    if (!isCurrent() || !choices.isConnected) return;
    if (!answer.ok) {
      mount(choices, h('p', { class: 'muted small' }, `The choices could not be loaded: ${answer.message} Rate the card as usual.`));
      return;
    }
    mount(
      choices,
      leechActions(
        { id: word.id, headword: word.headword, usefulness: answer.data.usefulness, goal: answer.data.effective_goal },
        ({ action, text, answer: done }) => {
          facts.delete(word.id);
          onDone({ action, text, undo: typeof done.undo === 'string' ? done.undo : null });
        },
      ),
    );
  })();
  return h(
    'section',
    { class: 'leech-panel', 'aria-labelledby': 'leech-title', 'data-testid': 'leech-panel' },
    h('h2', { id: 'leech-title' }, 'This word keeps slipping'),
    h('p', {}, 'You have forgotten it on several days. What would help?'),
    word.hint
      ? h('p', { class: 'leech-hint' }, 'Memory hint: ', h('span', { lang: 'en' }, word.hint))
      : h('p', { class: 'muted' }, 'No memory hint yet: the feedback export for Claude (Settings) asks for one.'),
    word.confusables.length ? h('p', { class: 'muted' }, `Easily confused with: ${word.confusables.join(', ')}`) : null,
    choices,
    h(
      'p',
      { class: 'muted small' },
      'Or ',
      h('a', { href: `#/words/${word.id}` }, 'open the word'),
      ' to write a hint or fix a sentence. Then rate the card as usual.',
    ),
  );
}
