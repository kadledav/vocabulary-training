// @ts-check
/**
 * Words (design §7.1 screen 6, R-73): search by headword, Czech or
 * meaning; sort (A–Z, most useful, newest, next due, status, source order);
 * filters (source, usefulness range, status, pack suggestions, suspended,
 * low priority) folded into one "Sort and filter" panel so the list starts
 * high on a phone. Rows are drawn 100 at a time ("Show more"). Every row
 * opens the word (`screens/word.js`). All pack text is rendered as text
 * (R-84).
 */

import { getJson } from '../api.js';
import { h, keepFocus, mount } from '../dom.js';
import { takeNotice } from '../notice.js';
import {
  FLAGS,
  SORTS,
  STATUSES,
  activeFilters,
  emptyListState,
  listParams,
  plural,
  statusLabel,
} from '../words-model.js';

/**
 * @typedef {{ id: number, headword: string, meaning: string, czech: string[], usefulness: number,
 *             low_priority: boolean, included: boolean, suspended: boolean, sentence_count: number,
 *             open_conflicts: number, seq: number | null, created_at: string, status: string,
 *             next_due: string | null }} WordRow
 * @typedef {{ id: number, slug: string, type: string, title: string, reading_position: string | null,
 *             word_count: number }} Source
 */

/** Rows drawn at first and per "Show more". */
const PAGE = 100;

/**
 * The list's filters, how many rows are shown and the scroll position,
 * kept while the app is open so Back from a word returns to the same place
 * (bug 5).
 */
const memory = { state: emptyListState(), shown: PAGE, scrollY: 0, openedWord: false, filtersOpen: false };

/**
 * A group of radio choices.
 *
 * @param {string} name
 * @param {string} legend
 * @param {readonly (readonly [string, string])[]} options
 * @param {string} current
 * @param {(value: string) => void} onChange
 */
function radios(name, legend, options, current, onChange) {
  return h(
    'fieldset',
    { class: `choices ${name}` },
    h('legend', {}, legend),
    options.map(([value, label]) =>
      h(
        'label',
        { class: 'choice-option' },
        h('input', { type: 'radio', name, value, checked: value === current, onchange: () => onChange(value) }),
        ' ',
        label,
      ),
    ),
  );
}

/** @param {WordRow} row */
function rowView(row) {
  const facts = [`usefulness ${row.usefulness}`, statusLabel(row.status)];
  if (row.next_due) facts.push(`due ${row.next_due}`);
  facts.push(plural(row.sentence_count, 'sentence'));
  if (row.seq !== null) facts.push(`#${row.seq}`);
  return h(
    'li',
    {},
    // The whole row is the link: an easy tap target (bug 8).
    h(
      'a',
      {
        class: 'word-row',
        href: `#/words/${row.id}`,
        onclick: () => {
          memory.openedWord = true;
          memory.scrollY = window.scrollY;
        },
      },
      h(
        'span',
        { class: 'word-row-head' },
        h('span', { class: 'headword', lang: 'en' }, row.headword),
        h('span', { class: 'muted', lang: 'cs' }, ` — ${row.czech.join(', ')}`),
      ),
      h(
        'span',
        { class: 'muted small' },
        facts.join(' · '),
        row.suspended ? h('span', { class: 'badge' }, 'suspended') : null,
        row.low_priority ? h('span', { class: 'badge' }, row.included ? 'low priority, included' : 'low priority') : null,
        row.open_conflicts > 0 ? h('span', { class: 'badge outcome-merged' }, 'pack suggestions') : null,
      ),
    ),
  );
}

/**
 * @param {HTMLElement} root
 * @param {import('../router.js').IsCurrent} isCurrent
 */
export function renderWords(root, isCurrent) {
  const state = memory.state;
  /** @type {Source[]} */
  let sources = [];
  let restored = false;
  let generation = 0;
  const notice = takeNotice();

  const search = h('input', {
    type: 'search',
    placeholder: 'Search headword, Czech or meaning',
    'aria-label': 'Search words',
    autocomplete: 'off',
    spellcheck: 'false',
    maxlength: '200',
    value: state.q,
  });
  const summary = h('summary', { 'data-testid': 'filters-summary' });
  const panel = h('div', { class: 'filter-panel' });
  const filters = h('details', { class: 'word-filters', open: memory.filtersOpen }, summary, panel);
  filters.addEventListener('toggle', () => {
    memory.filtersOpen = filters.open;
  });
  const count = h('p', { class: 'muted', role: 'status', 'data-testid': 'word-count' }, 'Loading…');
  const list = h('div', { class: 'word-list' });

  const changed = () => {
    memory.shown = PAGE;
    drawPanel();
    void load();
  };

  const usefulnessInput = (/** @type {'minU' | 'maxU'} */ key, /** @type {string} */ label) => {
    const input = h('input', {
      type: 'number',
      min: '1',
      max: '10',
      step: '1',
      inputmode: 'numeric',
      'aria-label': label,
      value: state[key],
      class: 'small-number',
    });
    input.addEventListener('change', () => {
      state[key] = input.value;
      changed();
    });
    return input;
  };

  const drawPanel = () => {
    const n = activeFilters(state);
    summary.textContent = n > 0 ? `Sort and filter · ${plural(n, 'filter')} on` : 'Sort and filter';
    const sorts = state.source ? [...SORTS, ['source', 'Source order']] : SORTS;
    const currentSort = state.sort || (state.source ? 'source' : 'alpha');
    keepFocus(panel, () =>
      mount(
        panel,
        radios('word-sort', 'Sort', /** @type {[string, string][]} */ (sorts), currentSort, (value) => {
          state.sort = value;
          changed();
        }),
        radios(
          'word-source',
          'Source',
          [['', 'All sources'], ...sources.map((s) => /** @type {[string, string]} */ ([
            String(s.id),
            `${s.title} (${s.word_count})${s.reading_position ? ` — ${s.reading_position}` : ''}`,
          ]))],
          state.source,
          (value) => {
            state.source = value;
            if (state.sort === 'source' && !value) state.sort = '';
            changed();
          },
        ),
        radios('word-status', 'Status', [['', 'Any'], ...STATUSES], state.status, (value) => {
          state.status = value;
          changed();
        }),
        h(
          'fieldset',
          { class: 'choices usefulness-range' },
          h('legend', {}, 'Usefulness'),
          h('label', { class: 'range-part' }, 'from ', usefulnessInput('minU', 'Usefulness from')),
          h('label', { class: 'range-part' }, 'to ', usefulnessInput('maxU', 'Usefulness to')),
        ),
        h(
          'fieldset',
          { class: 'choices word-flags' },
          h('legend', {}, 'Only'),
          FLAGS.map(([value, label]) =>
            h(
              'label',
              { class: 'choice-option' },
              h('input', {
                type: 'checkbox',
                name: 'word-flag',
                value,
                checked: state.flags.includes(value),
                onchange: (/** @type {Event} */ event) => {
                  const on = /** @type {HTMLInputElement} */ (event.target).checked;
                  state.flags = on ? [...state.flags, value] : state.flags.filter((f) => f !== value);
                  changed();
                },
              }),
              ' ',
              label,
            ),
          ),
        ),
        n > 0 || state.sort
          ? h(
              'button',
              {
                type: 'button',
                class: 'link-button',
                onclick: () => {
                  const q = state.q;
                  Object.assign(state, emptyListState(), { q });
                  changed();
                },
              },
              'Clear sort and filters',
            )
          : null,
      ),
    );
  };

  /** @param {WordRow[]} rows */
  const drawRows = (rows) => {
    const shown = rows.slice(0, memory.shown);
    const rest = rows.length - shown.length;
    mount(
      list,
      rows.length === 0 ? h('p', { class: 'muted' }, 'No word matches.') : null,
      h('ul', { class: 'rows word-rows' }, shown.map(rowView)),
      rest > 0
        ? h(
            'button',
            {
              type: 'button',
              class: 'big',
              'data-testid': 'show-more',
              onclick: () => {
                memory.shown += PAGE;
                drawRows(rows);
              },
            },
            `Show ${Math.min(PAGE, rest)} more (${rest} left)`,
          )
        : null,
    );
  };

  const load = async () => {
    const mine = ++generation;
    const answer = /** @type {import('../api.js').ApiResult<WordRow[]>} */ (
      await getJson(`/api/words?${listParams(state)}`)
    );
    if (mine !== generation || !isCurrent()) return;
    if (!answer.ok) {
      count.textContent = '';
      mount(list, h('p', { class: 'error', role: 'alert' }, answer.message));
      return;
    }
    count.textContent = plural(answer.data.length, 'word');
    drawRows(answer.data);
    if (memory.openedWord && !restored) {
      restored = true;
      // The source choices above the list change its position: wait for them.
      await sourcesLoaded;
      if (mine === generation && isCurrent()) {
        memory.openedWord = false;
        window.scrollTo(0, memory.scrollY);
      }
    }
  };

  search.addEventListener('input', () => {
    state.q = search.value;
    memory.shown = PAGE;
    void load();
  });

  mount(
    root,
    h('h1', {}, 'Words'),
    notice ? h('p', { class: 'notice-box', role: 'status', 'data-testid': 'notice' }, notice) : null,
    h('div', { class: 'filters' }, search, filters),
    count,
    list,
  );
  drawPanel();

  const sourcesLoaded = (async () => {
    const answer = /** @type {import('../api.js').ApiResult<Source[]>} */ (await getJson('/api/sources'));
    if (answer.ok && isCurrent()) {
      sources = answer.data;
      drawPanel();
    }
  })();
  void load();
}
