// @ts-check
/**
 * Today (design §7.1 screen 1; R-54, R-59): scope chooser (all sources or
 * one source with its reading position, remembered on this device), due
 * and new-word counts, the 7-day forecast, Start / Resume / Continue +N and
 * the backlog notice with "Add new words anyway (today)".
 */

import { h, mount } from '../dom.js';
import { fetchToday, startBlock } from '../session.js';
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

/**
 * @param {number} n
 * @param {string} one
 * @param {string} [many]
 */
function plural(n, one, many) {
  return `${n} ${n === 1 ? one : many ?? `${one}s`}`;
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
  const message = h('p', { class: 'status', role: 'status' });
  const view = h('div', { class: 'today' }, h('p', { class: 'muted' }, 'Loading…'));
  mount(root, h('h1', {}, 'Today'), view, message);

  /**
   * @param {{ kind: 'start' | 'continue', addNewAnyway?: boolean }} request
   * @param {HTMLButtonElement} button
   */
  const start = async (request, button) => {
    button.disabled = true;
    message.textContent = '';
    const answer = await startBlock({ ...request, scope });
    if (!isCurrent()) return;
    button.disabled = false;
    if (!answer.ok) {
      message.textContent = answer.message;
      return;
    }
    if (!answer.data.session) {
      message.textContent = 'Nothing to practise in this scope right now.';
      return;
    }
    window.location.hash = '#/practice';
  };

  /**
   * @param {Today} today
   * @param {number} blockSize
   */
  const draw = (today, blockSize) => {
    if (today.sources.length === 0) {
      mount(
        view,
        h('section', { class: 'card' }, h('p', {}, 'No words yet.'), h('p', {}, h('a', { href: '#/import' }, 'Import a pack'), ' to start.')),
      );
      return;
    }
    const sourceName = (/** @type {number | null} */ id) =>
      id === null ? 'All sources' : today.sources.find((s) => s.id === id)?.title ?? 'one source';

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
    const options = [{ id: null, title: 'All sources', detail: plural(today.sources.reduce((n, s) => n + s.word_count, 0), 'word') }];
    for (const s of today.sources) options.push({ id: s.id, title: s.title, detail: sourceDetail(s) });
    const radios = options.map((option) =>
        h(
          'label',
          { class: 'scope-option' },
          h('input', {
            type: 'radio',
            name: 'scope',
            value: option.id === null ? 'all' : String(option.id),
            checked: option.id === scope,
            onchange: () => {
              scope = option.id;
              rememberScope(scope);
              void load();
            },
          }),
          h('span', { class: 'scope-text' }, h('span', { class: 'scope-title' }, option.title), h('span', { class: 'scope-detail muted small' }, option.detail)),
        ),
      );

    const nw = today.new_words;
    /** @type {(Node | null)[]} */
    const newLines = [];
    if (nw.paused) {
      const anyway = h(
        'button',
        { type: 'button', 'data-testid': 'add-new-anyway' },
        'Add new words anyway (today)',
      );
      anyway.addEventListener('click', () => void start({ kind: today.finished_blocks.today > 0 ? 'continue' : 'start', addNewAnyway: true }, anyway));
      newLines.push(
        h('p', { class: 'notice', 'data-testid': 'backlog' }, `New words paused — backlog of ${plural(nw.due_now, 'review')}.`),
        active ? null : anyway,
      );
    } else {
      newLines.push(
        h(
          'p',
          { 'data-testid': 'new-left' },
          `New words: ${nw.left_today} of ${nw.per_day} left today`,
          nw.anyway_today ? ' (added anyway despite the backlog)' : '',
          today.new_available === 0 ? ' — none waiting in this scope' : '',
        ),
      );
    }

    const startKind = today.finished_blocks.today > 0 ? 'continue' : 'start';
    const startButton = h(
      'button',
      { type: 'button', class: 'primary big', 'data-testid': 'start' },
      startKind === 'continue' ? `Continue +${blockSize}` : 'Start',
    );
    startButton.addEventListener('click', () => void start({ kind: startKind }, startButton));

    mount(
      view,
      resume,
      h(
        'fieldset',
        { class: 'scope' },
        h('legend', {}, 'Practise'),
        radios,
      ),
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
      active ? null : startButton,
    );
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
      mount(view, h('p', { class: 'error' }, answer.message));
      return;
    }
    draw(answer.data, settings.block_size);
  };
  void load();
}
