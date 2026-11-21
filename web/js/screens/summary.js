// @ts-check
/**
 * Block summary (design §7.1 screen 5; research wireframe 6): rating
 * distribution in the rating colours, words to look at again, what is due
 * next, Done, and what comes next: Continue with what the next block holds,
 * or "Done for today" with "Practise more" / "Learn N more new words"
 * (owner feedback F1).
 */

import { h, mount } from '../dom.js';
import { RATINGS } from '../rating.js';
import { dayDone, dueTomorrow } from '../day-plan.js';
import { nextSteps } from '../next-steps.js';
import { fetchToday } from '../session.js';
import { loadSettings } from '../settings.js';
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
    // Nothing to show after a reload: Today is the useful screen (U7).
    window.location.replace('#/');
    return;
  }
  const { summary, scope } = entry;
  const dueLine = h('p', { class: 'muted', 'data-testid': 'due-line' });
  const message = h('p', { class: 'status', role: 'status' });
  const next = h('div', { class: 'summary-next' });
  const done = h('a', { class: 'button', href: '#/', 'data-testid': 'done' }, 'Done');

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
      next,
      message,
      h('div', { class: 'actions summary-actions' }, done),
    ),
  );

  void (async () => {
    const [answer, settings] = await Promise.all([fetchToday(scope), loadSettings()]);
    if (!isCurrent() || !answer.ok) return;
    const today = answer.data;
    if (!dayDone(today, settings.block_size)) dueLine.textContent = `Due now ${today.due_now} · tomorrow ${dueTomorrow(today)}`;
    if (today.active_session) return;
    mount(
      next,
      nextSteps({
        today,
        scope,
        blockSize: settings.block_size,
        perDay: settings.new_per_day,
        isCurrent,
        message,
        startTestId: 'continue',
      }),
    );
  })();
}
