// @ts-check
/**
 * Progress (design §7.1 screen 9; R-72, R-55): what is due in the next
 * two weeks, the words by status (all and per source, with the reading
 * position), how well the owner remembers compared with the app's aim,
 * the last 30 days, the problem words with their choices, and the pack
 * suggestions waiting for an answer. Every number says what it means in
 * words; charts are drawn as SVG from the stylesheet's colours, with the
 * same numbers as text for screen readers and without colour alone
 * carrying meaning. No streaks, scores or goals (R-74).
 */

import { getJson } from '../api.js';
import { h, mount, svg } from '../dom.js';
import { leechActions, undoLeechChoice } from '../leech.js';
import {
  STATUS_PARTS,
  activitySummary,
  barHeights,
  forecastSummary,
  reasonText,
  retentionText,
  shortDay,
  statusLine,
  statusSegments,
} from '../progress-model.js';
import { Toast } from '../toast.js';
import { plural } from '../words-model.js';

/**
 * @typedef {import('../progress-model.js').Progress} Progress
 * @typedef {import('../progress-model.js').StatusCounts} StatusCounts
 * @typedef {import('../progress-model.js').LeechEntry} LeechEntry
 */

/** How long the toast after a leech choice stays (with Undo). */
const TOAST_MS = 8000;

/**
 * The 14-day forecast as bars with the count above each and the day below.
 *
 * @param {{ day: string, due: number }[]} forecast
 */
function forecastChart(forecast) {
  const w = 24;
  const top = 16;
  const chart = 60;
  const heights = barHeights(
    forecast.map((f) => f.due),
    chart,
  );
  const width = forecast.length * w;
  return svg(
    'svg',
    { class: 'chart forecast-chart', viewBox: `0 0 ${width} ${top + chart + 28}`, 'aria-hidden': 'true', focusable: 'false' },
    forecast.map((f, i) => {
      const x = i * w;
      const height = heights[i];
      return svg(
        'g',
        {},
        svg('rect', { class: 'bar', x: x + 4, y: top + chart - height, width: w - 8, height }),
        svg('text', { class: 'chart-value', x: x + w / 2, y: top + chart - height - 4, 'text-anchor': 'middle' }, String(f.due)),
        svg(
          'text',
          { class: 'chart-label', x: x + w / 2, y: top + chart + 12, 'text-anchor': 'middle' },
          shortDay(f.day).slice(0, 2),
        ),
        svg(
          'text',
          { class: 'chart-label', x: x + w / 2, y: top + chart + 24, 'text-anchor': 'middle' },
          shortDay(f.day).split(' ')[1] ?? '',
        ),
      );
    }),
  );
}

/**
 * A horizontal bar of the words by status, with a legend that carries the
 * numbers (the colours only repeat it).
 *
 * @param {StatusCounts} counts
 * @param {string} testId
 */
function statusBar(counts, testId) {
  const width = 300;
  const segments = statusSegments(counts, width);
  return h(
    'div',
    { class: 'status-bar', 'data-testid': testId },
    segments.length
      ? svg(
          'svg',
          { class: 'chart status-chart', viewBox: `0 0 ${width} 14`, preserveAspectRatio: 'none', 'aria-hidden': 'true', focusable: 'false' },
          segments.map(([key, , x, w]) => svg('rect', { class: `seg seg-${key}`, x, y: 0, width: w, height: 14 })),
        )
      : null,
    h(
      'ul',
      { class: 'legend', 'aria-label': statusLine(counts) },
      STATUS_PARTS.filter(([key]) => counts[key] > 0).map(([key, label]) =>
        h('li', {}, h('span', { class: `swatch seg-${key}`, 'aria-hidden': 'true' }), `${counts[key]} ${label.toLowerCase()}`),
      ),
      counts.can_produce > 0 ? h('li', { class: 'muted' }, `${counts.can_produce} you can produce`) : null,
    ),
  );
}

/**
 * The measured share as a bar with a tick at the aim.
 *
 * @param {number} measured
 * @param {number} target
 */
function retentionMeter(measured, target) {
  const width = 300;
  return svg(
    'svg',
    { class: 'chart meter', viewBox: `0 0 ${width} 16`, preserveAspectRatio: 'none', 'aria-hidden': 'true', focusable: 'false' },
    svg('rect', { class: 'meter-track', x: 0, y: 3, width, height: 10 }),
    svg('rect', { class: 'meter-fill', x: 0, y: 3, width: Math.round(measured * width), height: 10 }),
    svg('rect', { class: 'meter-target', x: Math.min(width - 3, Math.round(target * width)), y: 0, width: 3, height: 16 }),
  );
}

/**
 * The last 30 days as small bars of ratings per day (days without
 * practice are simply empty: no streaks).
 *
 * @param {import('../progress-model.js').DayActivity[]} days
 */
function activityChart(days) {
  const w = 10;
  const chart = 40;
  const heights = barHeights(
    days.map((d) => d.reviews),
    chart,
  );
  return svg(
    'svg',
    { class: 'chart activity-chart', viewBox: `0 0 ${days.length * w} ${chart + 2}`, 'aria-hidden': 'true', focusable: 'false' },
    svg('rect', { class: 'baseline', x: 0, y: chart, width: days.length * w, height: 2 }),
    heights.map((height, i) => svg('rect', { class: 'bar', x: i * w + 1, y: chart - height, width: w - 2, height })),
  );
}

/**
 * @param {HTMLElement} root
 * @param {import('../router.js').IsCurrent} isCurrent
 */
export function renderProgress(root, isCurrent) {
  const view = h('div', { class: 'progress' }, h('p', { class: 'muted' }, 'Loading…'));
  const toastSlot = h('div', { class: 'toast-slot word-toast', 'data-testid': 'progress-toast' });
  const toast = new Toast(toastSlot);
  mount(root, h('h1', {}, 'Progress'), view, toastSlot);
  let generation = 0;

  /**
   * @param {LeechEntry} entry
   */
  const leechItem = (entry) => {
    const actions = h('div', { class: 'leech-choices' });
    mount(
      actions,
      leechActions({ id: entry.id, headword: entry.headword, usefulness: entry.usefulness, goal: entry.goal, spelling: entry.spelling }, (done) => {
        const token = done.answer.undo;
        toast.show({
          text: `${entry.headword}: ${done.text}`,
          ms: TOAST_MS,
          onUndo:
            typeof token === 'string'
              ? () =>
                  void (async () => {
                    const answer = await undoLeechChoice(entry.id, token);
                    if (!isCurrent()) return;
                    if (!answer.ok) toast.show({ text: `Could not undo: ${answer.message}`, ms: TOAST_MS });
                    void load();
                  })()
              : undefined,
        });
        void load();
      }),
    );
    return h(
      'li',
      { class: 'card leech', 'data-testid': 'leech' },
      h(
        'h3',
        {},
        h('a', { href: `#/words/${entry.id}`, lang: 'en' }, entry.headword),
        entry.flagged ? h('span', { class: 'badge' }, 'New problem') : null,
      ),
      h('p', { class: 'muted', lang: 'cs' }, entry.czech.join(', ')),
      h('ul', { class: 'reasons' }, entry.cards.map((c) => h('li', {}, reasonText(c)))),
      entry.hint
        ? h('p', {}, 'Memory hint: ', h('span', { lang: 'en' }, entry.hint))
        : h('p', { class: 'muted' }, 'No memory hint yet: the feedback export (Settings) asks Claude for one, or add your own in the word.'),
      entry.confusables.length ? h('p', { class: 'muted' }, `Easily confused with: ${entry.confusables.join(', ')}`) : null,
      h('p', { class: 'muted small' }, 'Open the word to write a hint, fix or hide a sentence, or reset the card.'),
      actions,
    );
  };

  /** @param {Progress} p */
  const draw = (p) => {
    const sources = p.sources.filter((s) => s.words > 0);
    mount(
      view,
      h(
        'section',
        { class: 'card', 'aria-labelledby': 'progress-due' },
        h('h2', { id: 'progress-due' }, 'Coming up'),
        h(
          'p',
          { 'data-testid': 'progress-due-now' },
          h('span', { class: 'big-number' }, String(p.due_now)),
          ' ',
          p.due_now === 1 ? 'card is' : 'cards are',
          ' due today (what Today’s blocks still hold).',
        ),
        h(
          'p',
          { 'data-testid': 'progress-week' },
          `${plural(p.due_next_7_days, 'review')} come due in the next 7 days. Each bar is one day of the next two weeks; the number is how many cards fall due that day.`,
        ),
        forecastChart(p.forecast),
        h('p', { class: 'sr-only' }, forecastSummary(p.forecast)),
      ),
      h(
        'section',
        { class: 'card', 'aria-labelledby': 'progress-words' },
        h('h2', { id: 'progress-words' }, 'Your words'),
        h('p', { 'data-testid': 'progress-totals' }, statusLine(p.totals)),
        statusBar(p.totals, 'status-all'),
        h(
          'dl',
          { class: 'status-terms small' },
          STATUS_PARTS.map(([, label, meaning]) => h('div', {}, h('dt', {}, label), h('dd', {}, meaning))),
          h('div', {}, h('dt', {}, 'Can produce'), h('dd', {}, 'its Produce card (typing the word) is remembered for a week or more')),
        ),
      ),
      sources.length > 0
        ? h(
            'section',
            { class: 'card', 'aria-labelledby': 'progress-sources' },
            h('h2', { id: 'progress-sources' }, 'By source'),
            h(
              'ul',
              { class: 'source-progress' },
              sources.map((s) =>
                h(
                  'li',
                  { 'data-testid': 'source-progress' },
                  h('h3', {}, s.title),
                  s.reading_position
                    ? h(
                        'p',
                        { class: 'muted small' },
                        `Reading position: ${s.reading_position}`,
                        s.reading_position_exported_at ? ` (from the pack of ${s.reading_position_exported_at})` : '',
                      )
                    : null,
                  h('p', { class: 'small' }, statusLine(s)),
                  statusBar(s, `status-${s.id}`),
                ),
              ),
            ),
          )
        : null,
      h(
        'section',
        { class: 'card', 'aria-labelledby': 'progress-retention' },
        h('h2', { id: 'progress-retention' }, 'How well you remember'),
        h(
          'p',
          { class: 'muted small' },
          `Counted over the last 30 days. A test is one review of a card you had last seen on an earlier day; a card that came up often counts once for each time. Remembered = rated 1–3. The aim is what the schedule plans for (higher for more useful words), worked out from each word's usefulness and goal as they are now, not as they were at each test; the tick on each bar marks it.`,
        ),
        p.retention.kinds.length === 0
          ? h('p', { class: 'muted' }, 'Nothing to measure yet.')
          : h(
              'ul',
              { class: 'retention' },
              p.retention.kinds.map((r) =>
                h(
                  'li',
                  { 'data-testid': `retention-${r.kind}` },
                  h('p', {}, retentionText(r)),
                  r.measured !== null && r.target !== null ? retentionMeter(r.measured, r.target) : null,
                ),
              ),
            ),
      ),
      h(
        'section',
        { class: 'card', 'aria-labelledby': 'progress-activity' },
        h('h2', { id: 'progress-activity' }, 'Last 30 days'),
        h(
          'p',
          { 'data-testid': 'progress-activity' },
          `${plural(p.activity.reviews, 'rating')} and ${plural(p.activity.new_words, 'new word')}. One bar per day, from ${shortDay(p.activity.days[0]?.day ?? p.study_day)} to today; its height is the day’s ratings.`,
        ),
        activityChart(p.activity.days),
        h('p', { class: 'sr-only' }, activitySummary(p.activity)),
      ),
      h(
        'section',
        { class: 'card', id: 'problem-words', 'aria-labelledby': 'progress-leeches' },
        h('h2', { id: 'progress-leeches' }, 'Problem words'),
        p.leeches.length === 0
          ? h('p', { class: 'muted', 'data-testid': 'no-leeches' }, 'None. A word becomes one when you forget it on four different days, or twice in a row after you knew it well.')
          : [
              h(
                'p',
                { class: 'muted small' },
                'Words that keep slipping. Choose what helps; any choice also clears the alert (four more slips bring it back). They leave this list once you remember them for three weeks, or when suspended.',
              ),
              h('ul', { class: 'leeches' }, p.leeches.map(leechItem)),
            ],
      ),
      p.conflicts.length
        ? h(
            'section',
            { class: 'card', 'aria-labelledby': 'progress-conflicts' },
            h('h2', { id: 'progress-conflicts' }, 'Pack suggestions to answer'),
            h('p', { class: 'muted small' }, 'A later pack brought other values for these words; open a word to use or dismiss them.'),
            h(
              'ul',
              { class: 'conflicts' },
              p.conflicts.map((c) =>
                h('li', {}, h('a', { href: `#/words/${c.word_id}`, lang: 'en' }, c.headword), ` — ${plural(c.open, 'suggestion')}`),
              ),
            ),
          )
        : null,
    );
  };

  /** @param {string} problem */
  const failed = (problem) => {
    const retry = h('button', { type: 'button', 'data-testid': 'try-again' }, 'Try again');
    retry.addEventListener('click', () => void load());
    mount(view, h('div', { class: 'card' }, h('p', { class: 'error' }, problem), retry));
  };

  const load = async () => {
    const mine = ++generation;
    /** @type {import('../api.js').ApiResult<Progress>} */
    const answer = await getJson('/api/progress');
    if (mine !== generation || !isCurrent()) return;
    if (!answer.ok) {
      failed(answer.message);
      return;
    }
    const y = window.scrollY;
    draw(answer.data);
    window.scrollTo(window.scrollX, y);
  };
  void load();
}
