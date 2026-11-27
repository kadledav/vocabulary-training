// @ts-check
/**
 * What the Words screens (design §7.1 screens 6 and 7; R-04, R-25, R-73)
 * compute without a page: the list's query from its filters, labels for
 * statuses, cards and fields, and the `[[…]]` source of a sentence for
 * editing. No DOM here, so it is tested with `node --test`.
 */

/**
 * @typedef {{ q: string, source: string, sort: string, status: string, minU: string, maxU: string,
 *             flags: string[] }} ListState
 * @typedef {import('./marked.js').TextParts} TextParts
 */

/** Orders of the list; `source` is offered only with a source filter. */
/** @type {ReadonlyArray<readonly [string, string]>} */
export const SORTS = Object.freeze([
  ['alpha', 'A–Z'],
  ['usefulness', 'Most useful'],
  ['added', 'Newest'],
  ['due', 'Next due'],
  ['status', 'Status'],
]);

/** Learning statuses (server `WordStatus`) with their labels. */
/** @type {ReadonlyArray<readonly [string, string]>} */
export const STATUSES = Object.freeze([
  ['new', 'New'],
  ['learning', 'Learning'],
  ['familiar', 'Familiar'],
  ['known', 'Known'],
  ['leech', 'Problem word'],
]);

/** The list's flag filters. */
/** @type {ReadonlyArray<readonly [string, string]>} */
export const FLAGS = Object.freeze([
  ['conflicts', 'Has pack suggestions'],
  ['suspended', 'Suspended'],
  ['low_priority', 'Low priority'],
]);

/** @returns {ListState} */
export function emptyListState() {
  return { q: '', source: '', sort: '', status: '', minU: '', maxU: '', flags: [] };
}

/**
 * The query string of `GET /api/words` for the list's state (defaults are
 * left out).
 *
 * @param {ListState} state
 */
export function listParams(state) {
  const params = new URLSearchParams();
  if (state.q.trim()) params.set('q', state.q.trim());
  if (state.source) params.set('source', state.source);
  if (state.sort) params.set('sort', state.sort);
  if (state.status) params.set('status', state.status);
  const usefulness = (/** @type {string} */ text) => {
    const n = Number(text);
    return text.trim() !== '' && Number.isInteger(n) && n >= 1 && n <= 10 ? String(n) : '';
  };
  if (usefulness(state.minU)) params.set('min_u', usefulness(state.minU));
  if (usefulness(state.maxU)) params.set('max_u', usefulness(state.maxU));
  if (state.flags.length > 0) params.set('flags', [...state.flags].sort().join(','));
  return params;
}

/**
 * How many filters (not the search text, not the order) are set.
 *
 * @param {ListState} state
 */
export function activeFilters(state) {
  const params = listParams(state);
  return ['source', 'status', 'min_u', 'max_u'].filter((name) => params.has(name)).length + state.flags.length;
}

/** @param {string} status */
export function statusLabel(status) {
  const found = STATUSES.find(([value]) => value === status);
  return found ? found[1] : status;
}

/** @param {string} kind */
export function cardKindLabel(kind) {
  return { understand: 'Understand', produce: 'Produce', spelling: 'Spelling' }[kind] ?? kind;
}

/** @param {string} state */
export function cardStateLabel(state) {
  return { new: 'not practised yet', relearning: 'failed last time', review: 'in review' }[state] ?? state;
}

/**
 * A probability as a whole percentage.
 *
 * @param {number | null | undefined} p
 */
export function percent(p) {
  return typeof p === 'number' && Number.isFinite(p) ? `${Math.round(p * 100)} %` : '—';
}

/**
 * @param {number} n
 * @param {string} one
 * @param {string} [many]
 */
export function plural(n, one, many = `${one}s`) {
  return `${n} ${n === 1 ? one : many}`;
}

/**
 * The text of a sentence with its target marked, as the owner edits it.
 *
 * @param {TextParts} parts
 */
export function markedSource(parts) {
  return parts.target ? `${parts.pre}[[${parts.target}]]${parts.post}` : parts.pre;
}

/**
 * Does `text` mark exactly one span with `[[…]]`? (The server checks the
 * full rule; this only gives a hint before saving.)
 *
 * @param {string} text
 */
export function hasOneMark(text) {
  const open = text.split('[[').length - 1;
  const close = text.split(']]').length - 1;
  const start = text.indexOf('[[');
  const end = text.indexOf(']]');
  return open === 1 && close === 1 && end > start + 2;
}

/**
 * The editable word fields (server `OWNER_WORD_FIELDS`) with how they are
 * edited: `line` (one line), `text` (several lines), `max` characters,
 * `required` (cannot be cleared).
 *
 * @typedef {{ label: string, input: 'line' | 'text', max: number, required?: boolean, help?: string }} FieldSpec
 * @type {Readonly<Record<string, FieldSpec>>}
 */
export const FIELDS = Object.freeze({
  headword: { label: 'Headword', input: 'line', max: 100, required: true },
  meaning: { label: 'Meaning', input: 'text', max: 1500, required: true },
  grammar: { label: 'Grammar', input: 'line', max: 200 },
  pronunciation: { label: 'Pronunciation', input: 'line', max: 200, help: 'A respelling such as STRAY-tum.' },
  speak: { label: 'Text to speak', input: 'line', max: 200, help: 'Empty: the headword is spoken.' },
  hint: { label: 'Memory hint', input: 'text', max: 300 },
  spelling_note: { label: 'Spelling note', input: 'line', max: 200 },
  note: { label: 'Note', input: 'text', max: 6000 },
  icon: { label: 'Icon', input: 'line', max: 40, help: 'An emoji.' },
});

/** @param {string} field */
export function fieldLabel(field) {
  if (field === 'usefulness') return 'Usefulness';
  if (field === 'goal') return 'Goal';
  if (field === 'spelling') return 'Spelling practice';
  return FIELDS[field]?.label ?? field;
}

/**
 * A stored value as the owner reads it.
 *
 * @param {unknown} value
 */
export function showValue(value) {
  if (value === null || value === undefined || value === '') return '(empty)';
  if (value === true) return 'yes';
  if (value === false) return 'no';
  return String(value);
}

/**
 * The goal choices; `auto` (stored `null`) derives it from usefulness.
 *
 * @param {number} usefulness
 * @returns {[string, string][]}
 */
export function goalChoices(usefulness) {
  return [
    ['auto', `Automatic (${usefulness >= 6 ? 'produce' : 'recognise'}, from usefulness)`],
    ['recognise', 'Recognise only'],
    ['active', 'Also produce'],
  ];
}

/**
 * The goal value to send for a choice of {@link goalChoices}.
 *
 * @param {string} choice
 * @returns {string | null}
 */
export function goalValue(choice) {
  return choice === 'auto' ? null : choice;
}
