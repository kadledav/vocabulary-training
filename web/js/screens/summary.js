// @ts-check
/**
 * Block summary (design §7.1 screen 5; research wireframe 6): rating
 * distribution in the rating colours, words to look at again, what is due
 * next, and Done / Continue +N.
 */

import { h, mount } from '../dom.js';
import { RATINGS } from '../rating.js';
import { fetchToday, startBlock } from '../session.js';
import { getSummary } from '../summary-store.js';

/**
 * @param {number} n
 * @param {string} one
 * @param {string} [many]
 */
function plural(n, one, many) {
  return `${n} ${n === 1 ? one : many ?? `${one}s`}`;
}

/** @param {import('../session.js').BlockSummary} summary */
function distribution(summary) {
  const max = Math.max(1, ...summary.ratings);
  return h(
    'div',
    { class: 'distribution', 'data-testid': 'distribution' },
    RATINGS.map((r) => {
      const count = summary.ratings[r.grade - 1] ?? 0;
      return h(
        'div',
        { class: `dist-row rate-${r.grade}${r.grade === 4 ? ' first-fail' : ''}`, 'data-grade': r.grade },
        h('span', { class: 'dist-label' }, `${r.grade} ${r.label}`),
        h('progress', { class: 'dist-bar', max: String(max), value: String(count), 'aria-hidden': 'true' }),
        h('span', { class: 'dist-count', 'data-testid': `count-${r.grade}` }, String(count)),
      );
    }),
  );
}

/**
 * @param {HTMLElement} root
 * @param {import('../router.js').IsCurrent} isCurrent
 */
export function renderSummary(root, isCurrent) {
  const entry = getSummary();
  if (!entry) {
    mount(root, h('h1', {}, 'Summary'), h('p', {}, 'No finished block to show. ', h('a', { href: '#/' }, 'Back to Today'), '.'));
    return;
  }
  const { summary, scope, blockSize } = entry;
  const dueLine = h('p', { class: 'muted', 'data-testid': 'due-line' });
  const message = h('p', { class: 'status', role: 'status' });
  const continueButton = h(
    'button',
    {
      type: 'button',
      'data-testid': 'continue',
      onclick: async () => {
        continueButton.disabled = true;
        const answer = await startBlock({ kind: 'continue', scope });
        if (!isCurrent()) return;
        continueButton.disabled = false;
        if (!answer.ok) {
          message.textContent = answer.message;
          return;
        }
        if (!answer.data.session) {
          message.textContent = 'Nothing more to practise right now.';
          return;
        }
        window.location.hash = '#/practice';
      },
    },
    `Continue +${blockSize}`,
  );

  mount(
    root,
    h(
      'div',
      { class: 'summary' },
      h('h1', {}, summary.unanswered > 0 ? 'Block ended' : 'Done'),
      h(
        'p',
        { 'data-testid': 'summary-line' },
        [
          plural(summary.answered, 'card'),
          summary.new_words > 0 ? plural(summary.new_words, 'new word') : null,
          summary.repeats > 0 ? plural(summary.repeats, 'repeat') : null,
        ]
          .filter(Boolean)
          .join(' · '),
      ),
      summary.unanswered > 0
        ? h('p', { class: 'muted' }, `${plural(summary.unanswered, 'card')} not rated — they stay due.`)
        : null,
      distribution(summary),
      summary.look_again.length > 0
        ? h(
            'section',
            { class: 'look-again' },
            h('h2', {}, 'Look at these again'),
            h(
              'ul',
              { class: 'rows' },
              summary.look_again.map((w) => h('li', {}, h('a', { href: `#/words/${w.word_id}`, lang: 'en' }, w.headword))),
            ),
          )
        : null,
      dueLine,
      message,
      h(
        'div',
        { class: 'actions summary-actions' },
        h('a', { class: 'button', href: '#/', 'data-testid': 'done' }, 'Done'),
        continueButton,
      ),
    ),
  );

  void (async () => {
    const answer = await fetchToday(scope);
    if (!isCurrent() || !answer.ok) return;
    const tomorrow = answer.data.forecast[0]?.due ?? 0;
    dueLine.textContent = `Due now ${answer.data.due_now} · tomorrow ${tomorrow}`;
  })();
}
