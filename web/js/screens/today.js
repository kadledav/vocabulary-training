// @ts-check
/**
 * Today (design §7.1 screen 1; R-54, R-59; usability U1, U6; owner
 * feedback F1): scope chooser (all sources or one source with its reading
 * position, remembered on this device), due and new-word counts, the 7-day
 * forecast, Resume, Start/Continue saying what the block will hold — or,
 * when the day's work is done, "Done for today" with "Practise more" and
 * "Learn N more new words" — and the backlog notice with "Add new words
 * anyway (today)". A failed load offers "Try again" and retries by itself
 * when the connection comes back.
 */

import { newWaiting, pausedForBacklog, plural, typedLine } from '../day-plan.js';
import { h, keepFocus, mount } from '../dom.js';
import { begin, nextSteps } from '../next-steps.js';
import { takeNotice } from '../notice.js';
import { fetchToday } from '../session.js';
import { loadSettings } from '../settings.js';

/**
 * @typedef {import('../session.js').Today} Today
 * @typedef {import('../session.js').SourceSummary} SourceSummary
 */

const SCOPE_KEY = 'vocab.scope';

/** @returns {number | null} the scope remembered on this device */
function rememberedScope() {
  try {
    const value = localStorage.getItem(SCOPE_KEY);
    return value && /^[0-9]+$/.test(value) ? Number(value) : null;
  } catch {
    return null;
  }
}

/** @param {number | null} scope */
function rememberScope(scope) {
  try {
    if (scope === null) localStorage.removeItem(SCOPE_KEY);
    else localStorage.setItem(SCOPE_KEY, String(scope));
  } catch {
    // No storage (private mode): the choice simply is not remembered.
  }
}

/**
 * "Mon", "Tue", … for a study day `YYYY-MM-DD`.
 *
 * @param {string} day
 */
export function weekday(day) {
  const date = new Date(`${day}T12:00:00Z`);
  if (Number.isNaN(date.getTime())) return day;
  return date.toLocaleDateString('en-GB', { weekday: 'short', timeZone: 'UTC' });
}

/** @param {SourceSummary} source */
function sourceDetail(source) {
  return [source.reading_position ? `reading: ${source.reading_position}` : null, plural(source.word_count, 'word')]
    .filter(Boolean)
    .join(' · ');
}

/**
 * @param {HTMLElement} root
 * @param {import('../router.js').IsCurrent} isCurrent
 */
export function renderToday(root, isCurrent) {
  let scope = rememberedScope();
  let generation = 0;
  const notice = takeNotice();
  const message = h('p', { class: 'status', role: 'status' });
  const view = h('div', { class: 'today' }, h('p', { class: 'muted' }, 'Loading…'));
  mount(
    root,
    h('h1', {}, 'Today'),
    notice ? h('p', { class: 'notice-box', role: 'alert', 'data-testid': 'notice' }, notice) : null,
    view,
    message,
  );

  /**
   * @param {Today} today
   * @param {import('../settings.js').Settings} settings
   */
  const draw = (today, settings) => {
    if (today.sources.length === 0) {
      mount(
        view,
        h('section', { class: 'card' }, h('p', {}, 'No words yet.'), h('p', {}, h('a', { href: '#/import' }, 'Import a pack'), ' to start.')),
      );
      return;
    }
    const sourceName = (/** @type {number | null} */ id) =>
      id === null ? 'All sources' : today.sources.find((s) => s.id === id)?.title ?? 'one source';
    const mine = generation;
    const options = {
      today,
      scope,
      blockSize: settings.block_size,
      perDay: settings.new_per_day,
      isCurrent: () => isCurrent() && generation === mine,
      message,
      startTestId: 'start',
    };

    const active = today.active_session;
    const resume = active
      ? h(
          'section',
          { class: 'card resume' },
          h('p', {}, `Block in progress (${sourceName(active.scope_source_id)}).`),
          h(
            'a',
            { class: 'button primary big', href: '#/practice', 'data-testid': 'resume' },
            `Resume — ${active.remaining} of ${active.total} left`,
          ),
        )
      : null;

    /** @type {{ id: number | null, title: string, detail: string }[]} */
    const choices = [{ id: null, title: 'All sources', detail: plural(today.sources.reduce((n, s) => n + s.word_count, 0), 'word') }];
    for (const s of today.sources) choices.push({ id: s.id, title: s.title, detail: sourceDetail(s) });
    const radios = choices.map((choice) =>
      h(
        'label',
        { class: 'scope-option' },
        h('input', {
          type: 'radio',
          name: 'scope',
          value: choice.id === null ? 'all' : String(choice.id),
          checked: choice.id === scope,
          onchange: () => {
            scope = choice.id;
            rememberScope(scope);
            void load();
          },
        }),
        h('span', { class: 'scope-text' }, h('span', { class: 'scope-title' }, choice.title), h('span', { class: 'scope-detail muted small' }, choice.detail)),
      ),
    );

    const nw = today.new_words;
    const waiting = newWaiting(today);
    /** @type {(Node | null)[]} */
    const newLines = [h('p', { 'data-testid': 'new-waiting' }, waiting === 0 ? 'No new words waiting in this scope.' : `${plural(waiting, 'new word')} waiting.`)];
    if (pausedForBacklog(today)) {
      const anyway = h('button', { type: 'button', 'data-testid': 'add-new-anyway' }, 'Add new words anyway (today)');
      anyway.addEventListener('click', () =>
        void begin(options, { kind: today.finished_blocks.today > 0 ? 'continue' : 'start', addNewAnyway: true }, anyway),
      );
      newLines.push(
        h(
          'p',
          { class: 'notice', 'data-testid': 'backlog' },
          `New words are paused until you catch up on reviews (${plural(nw.due_now, 'review')} waiting)`,
          active ? '.' : ' — you can still add some now.',
        ),
        active ? null : anyway,
      );
    } else if (waiting > 0) {
      newLines.push(
        h(
          'p',
          { class: 'muted', 'data-testid': 'new-left' },
          `New words today: ${nw.left_today} of ${nw.per_day} left`,
          nw.anyway_today ? ' (added anyway despite the backlog)' : '',
          '.',
        ),
      );
    }

    const typed = typedLine(today);
    if (typed) newLines.push(h('p', { class: 'muted', 'data-testid': 'typed-waiting' }, typed));

    keepFocus(view, () =>
      mount(
        view,
        resume,
        h('fieldset', { class: 'scope' }, h('legend', {}, 'Practise'), radios),
        active ? null : nextSteps(options),
        h(
          'section',
          { class: 'card counts' },
          h(
            'p',
            { class: 'due', 'data-testid': 'due-now' },
            h('span', { class: 'big-number' }, String(today.due_now)),
            ' due now',
            today.relearning > 0 ? h('span', { class: 'muted' }, ` (${today.relearning} to relearn)`) : null,
          ),
          newLines,
          h('h2', {}, 'Next 7 days'),
          h(
            'ol',
            { class: 'forecast', 'aria-label': 'Cards due on each of the next 7 days' },
            today.forecast.map((f) =>
              h('li', {}, h('span', { class: 'fc-day' }, weekday(f.day)), h('span', { class: 'fc-count' }, String(f.due))),
            ),
          ),
        ),
      ),
    );
  };

  /** @param {string} problem */
  const failed = (problem) => {
    const retry = h('button', { type: 'button', 'data-testid': 'try-again' }, 'Try again');
    retry.addEventListener('click', () => void load());
    mount(view, h('div', { class: 'card' }, h('p', { class: 'error' }, problem), retry));
    // Back online: try again by itself (once; a new load arms it again).
    window.addEventListener('online', () => {
      if (isCurrent()) void load();
    }, { once: true });
  };

  const load = async () => {
    const mine = ++generation;
    const [answer, settings] = await Promise.all([fetchToday(scope), loadSettings()]);
    if (mine !== generation || !isCurrent()) return;
    if (!answer.ok) {
      if (answer.code === 'unknown_source' && scope !== null) {
        // The remembered source is gone: fall back to all sources.
        scope = null;
        rememberScope(null);
        void load();
        return;
      }
      failed(answer.message);
      return;
    }
    draw(answer.data, settings);
  };
  void load();
}
