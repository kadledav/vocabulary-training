// @ts-check
/**
 * Minimal read-only word list and word view (M2: to inspect what an import
 * did; the full Words screens with editing arrive in M7). All pack text is
 * rendered as text (R-84).
 */

import { getJson } from '../api.js';
import { h, mount } from '../dom.js';
import { displayIcon } from '../icon.js';
import { markedText } from '../marked.js';

/**
 * @typedef {{ id: number, headword: string, meaning: string, czech: string[], usefulness: number,
 *             low_priority: boolean, sentence_count: number, open_conflicts: number, seq: number | null }} WordRow
 * @typedef {{ id: number, slug: string, type: string, title: string, reading_position: string | null,
 *             word_count: number }} Source
 * @typedef {import('../marked.js').TextParts} TextParts
 * @typedef {{ id: number, en: TextParts, cs: TextParts, origin: string, location: string | null,
 *             source_title: string | null, hidden: boolean }} Sentence
 * @typedef {{ id: number, field: string, current: unknown, incoming: unknown, created_at: string }} Conflict
 * @typedef {{ id: number, headword: string, grammar: string | null, pronunciation: string | null,
 *             speak: string | null, meaning: string, usefulness: number, goal: string | null,
 *             hint: string | null, spelling: boolean, spelling_note: string | null, note: string,
 *             icon: string | null, low_priority: boolean, suspended: boolean, czech: string[],
 *             accepted: string[], confusables: string[], forms: { form: string, common: boolean }[],
 *             sources: { source_id: number, title: string, seq: number, tag_kind: string | null, tag_text: string | null }[],
 *             sentences: Sentence[], conflicts: Conflict[] }} WordView
 */

/**
 * The list's search text, source filter and scroll position, kept while
 * the app is open so Back from a word returns to the same place (bug 5).
 */
const memory = { query: '', source: '', scrollY: 0, openedWord: false };

/**
 * @param {number} n
 * @param {string} one
 */
function plural(n, one) {
  return `${n} ${n === 1 ? one : `${one}s`}`;
}

/**
 * @param {HTMLElement} root
 * @param {import('../router.js').IsCurrent} isCurrent
 */
export function renderWords(root, isCurrent) {
  const search = h('input', {
    type: 'search',
    placeholder: 'Search headword, Czech or meaning',
    'aria-label': 'Search words',
    autocomplete: 'off',
    spellcheck: 'false',
    value: memory.query,
  });
  // Radio buttons rather than a <select>: WebKit's built-in select styles
  // are refused by the CSP (style-src 'self'), and radios are easier to tap.
  let source = memory.source;
  let restored = false;
  /**
   * @param {string} value
   * @param {string} label
   */
  const sourceOption = (value, label) =>
    h(
      'label',
      { class: 'choice-option' },
      h('input', {
        type: 'radio',
        name: 'word-source',
        value,
        checked: value === source,
        onchange: () => {
          source = value;
          memory.source = value;
          void load();
        },
      }),
      ' ',
      label,
    );
  const sourceChoice = h('fieldset', { class: 'word-sources' }, h('legend', {}, 'Source'), sourceOption('', 'All sources'));
  const list = h('div', { class: 'word-list' }, h('p', { class: 'muted' }, 'Loading…'));
  let generation = 0;

  const load = async () => {
    const mine = ++generation;
    const params = new URLSearchParams();
    if (search.value.trim()) params.set('q', search.value.trim());
    if (source) params.set('source', source);
    const answer = /** @type {import('../api.js').ApiResult<WordRow[]>} */ (
      await getJson(`/api/words?${params}`)
    );
    if (mine !== generation || !isCurrent()) return;
    if (!answer.ok) {
      mount(list, h('p', { class: 'error' }, answer.message));
      return;
    }
    const rows = answer.data;
    mount(
      list,
      h('p', { class: 'muted' }, plural(rows.length, 'word')),
      h(
        'ul',
        { class: 'rows word-rows' },
        rows.map((row) =>
          h(
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
              h('span', { class: 'word-row-head' }, h('span', { class: 'headword', lang: 'en' }, row.headword), h('span', { class: 'muted', lang: 'cs' }, ` — ${row.czech.join(', ')}`)),
              h(
                'span',
                { class: 'muted small' },
                `usefulness ${row.usefulness} · ${plural(row.sentence_count, 'sentence')}`,
                row.seq !== null ? ` · #${row.seq}` : '',
                row.low_priority ? h('span', { class: 'badge' }, 'low priority') : null,
                row.open_conflicts > 0 ? h('span', { class: 'badge outcome-merged' }, 'conflicts') : null,
              ),
            ),
          ),
        ),
      ),
    );
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
    memory.query = search.value;
    void load();
  });

  mount(
    root,
    h('h1', {}, 'Words'),
    h('div', { class: 'filters' }, search, sourceChoice),
    list,
  );

  const sourcesLoaded = (async () => {
    const answer = /** @type {import('../api.js').ApiResult<Source[]>} */ (await getJson('/api/sources'));
    if (answer.ok && isCurrent()) {
      for (const source of answer.data) {
        const label =
          `${source.title} (${source.word_count})` +
          (source.reading_position ? ` — ${source.reading_position}` : '');
        sourceChoice.append(sourceOption(String(source.id), label));
      }
    }
  })();
  void load();
}

/**
 * @param {string} label
 * @param {string | number | null | undefined | false} value
 */
function field(label, value) {
  if (value === null || value === undefined || value === false || value === '') return null;
  return h('div', { class: 'field' }, h('dt', {}, label), h('dd', {}, String(value)));
}

/** @param {unknown} value */
function show(value) {
  if (value === null || value === undefined) return '(empty)';
  return typeof value === 'string' ? value : JSON.stringify(value);
}

/** @param {WordView} word */
function wordView(word) {
  const forms = word.forms.map((f) => (f.common ? f.form : `${f.form} (rare)`)).join(', ');
  return h(
    'article',
    { class: 'word-view' },
    h('p', {}, h('a', { class: 'back-link', href: '#/words' }, '← Words')),
    h('h1', { lang: 'en' }, displayIcon(word.icon) ? `${displayIcon(word.icon)} ` : '', word.headword),
    h('p', { class: 'meaning', lang: 'en' }, word.meaning),
    h(
      'dl',
      { class: 'fields' },
      field('Czech', word.czech.join(', ')),
      field('Grammar', word.grammar),
      field('Pronunciation', word.pronunciation),
      field('Speak', word.speak),
      field('Forms', forms),
      field('Usefulness', word.usefulness),
      field('Goal', word.goal ?? 'from usefulness'),
      field('Accepted answers', word.accepted.join(', ')),
      field('Confusables', word.confusables.join(', ')),
      field('Hint', word.hint),
      field('Spelling', word.spelling ? word.spelling_note ?? 'yes' : false),
      field('Low priority', word.low_priority ? 'yes' : false),
      field(
        'Sources',
        word.sources
          .map((s) => `${s.title} #${s.seq}${s.tag_kind ? ` [${s.tag_kind}${s.tag_text ? `: ${s.tag_text}` : ''}]` : ''}`)
          .join('; '),
      ),
    ),
    word.note ? h('section', { class: 'note-text' }, h('h2', {}, 'Note'), h('p', { lang: 'en' }, word.note)) : null,
    word.conflicts.length > 0
      ? h(
          'section',
          { class: 'card' },
          h('h2', {}, 'Suggestions from later packs (existing values kept)'),
          h(
            'ul',
            {},
            word.conflicts.map((c) =>
              h('li', {}, h('code', {}, c.field), `: kept “${show(c.current)}”; suggested “${show(c.incoming)}”`),
            ),
          ),
        )
      : null,
    h('h2', {}, `Sentences (${word.sentences.length})`),
    h(
      'ol',
      { class: 'sentences' },
      word.sentences.map((s) =>
        h(
          'li',
          { class: s.hidden ? 'hidden-sentence' : '' },
          h('p', {}, markedText(s.en, 'en')),
          h('p', { class: 'muted' }, markedText(s.cs, 'cs')),
          h(
            'p',
            { class: 'muted small' },
            [s.origin, s.location, s.source_title, s.hidden ? 'hidden' : null].filter(Boolean).join(' · '),
          ),
        ),
      ),
    ),
  );
}

/**
 * @param {HTMLElement} root
 * @param {string} id
 * @param {import('../router.js').IsCurrent} isCurrent
 */
export async function renderWord(root, id, isCurrent) {
  mount(root, h('p', { class: 'muted' }, 'Loading…'));
  const answer = /** @type {import('../api.js').ApiResult<WordView>} */ (await getJson(`/api/words/${id}`));
  if (!isCurrent()) return;
  if (!answer.ok) {
    mount(root, h('p', {}, h('a', { class: 'back-link', href: '#/words' }, '← Words')), h('p', { class: 'error' }, answer.message));
    return;
  }
  mount(root, wordView(answer.data));
}
