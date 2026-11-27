// @ts-check
/**
 * What the owner typed for a Produce card — and, once checked, the check
 * itself — kept across a reload in this tab (sessionStorage), per item:
 * a reload after Check returns to the checked answer side, and undoing
 * the previous card never touches the current card's draft. An item's
 * entry goes when it is rated. Pure apart from the storage it is given.
 */

/**
 * @typedef {import('../grading.js').Check} Check
 * @typedef {{ typed: string, hint: boolean, check: Check | null }} Draft
 * @typedef {{ getItem(key: string): string | null, setItem(key: string, value: string): void,
 *             removeItem(key: string): void }} DraftStorage
 */

const KEY = 'vocab.typed-draft';
/** Entries kept (the newest): the current card and a few undone ones. */
const KEEP = 8;
const MATCHES = ['exact', 'none', 'dont_know', 'skipped'];
const VERDICTS = ['correct', 'right_word', 'alternative', 'miss', 'dont_know', 'skipped'];

/** This tab's sessionStorage, or null where there is none. */
function tabStorage() {
  try {
    return globalThis.sessionStorage ?? null;
  } catch {
    return null;
  }
}

/**
 * A stored check, if it has the shape of one.
 *
 * @param {unknown} value
 * @returns {Check | null}
 */
export function storedCheck(value) {
  if (typeof value !== 'object' || value === null) return null;
  const c = /** @type {Record<string, unknown>} */ (value);
  const suggested = c.suggested;
  if (
    typeof c.match !== 'string' ||
    !MATCHES.includes(c.match) ||
    typeof c.verdict !== 'string' ||
    !VERDICTS.includes(c.verdict) ||
    !(suggested === null || (typeof suggested === 'number' && [2, 3, 4, 5].includes(suggested))) ||
    !(c.typed === null || typeof c.typed === 'string') ||
    typeof c.hintUsed !== 'boolean'
  ) {
    return null;
  }
  return /** @type {Check} */ ({ match: c.match, verdict: c.verdict, suggested, typed: c.typed, hintUsed: c.hintUsed });
}

/**
 * All entries, newest last.
 *
 * @param {DraftStorage | null} storage
 * @returns {[number, Draft][]}
 */
function readAll(storage) {
  if (!storage) return [];
  try {
    const raw = storage.getItem(KEY);
    const data = raw ? JSON.parse(raw) : null;
    /** @type {unknown[]} */
    const entries = Array.isArray(data?.entries) ? data.entries : [];
    /** @type {[number, Draft][]} */
    const out = [];
    for (const entry of entries) {
      if (!Array.isArray(entry) || typeof entry[0] !== 'number' || typeof entry[1]?.typed !== 'string') continue;
      out.push([entry[0], { typed: entry[1].typed, hint: entry[1].hint === true, check: storedCheck(entry[1].check) }]);
    }
    return out;
  } catch {
    // A damaged entry: no drafts.
    return [];
  }
}

/**
 * @param {DraftStorage | null} storage
 * @param {[number, Draft][]} entries
 */
function writeAll(storage, entries) {
  if (!storage) return;
  try {
    if (entries.length === 0) storage.removeItem(KEY);
    else storage.setItem(KEY, JSON.stringify({ entries: entries.slice(-KEEP) }));
  } catch {
    // No storage: the draft lives only on screen.
  }
}

/**
 * The draft of item `itemId`, if this tab has one.
 *
 * @param {number} itemId
 * @param {DraftStorage | null} [storage]
 * @returns {Draft | null}
 */
export function loadDraft(itemId, storage = tabStorage()) {
  return readAll(storage).find(([id]) => id === itemId)?.[1] ?? null;
}

/**
 * Keep `draft` for item `itemId` (replacing its earlier one).
 *
 * @param {number} itemId
 * @param {Draft} draft
 * @param {DraftStorage | null} [storage]
 */
export function saveDraft(itemId, draft, storage = tabStorage()) {
  const entries = readAll(storage).filter(([id]) => id !== itemId);
  entries.push([itemId, draft]);
  writeAll(storage, entries);
}

/**
 * Forget the draft of item `itemId` (it was rated).
 *
 * @param {number} itemId
 * @param {DraftStorage | null} [storage]
 */
export function clearDraft(itemId, storage = tabStorage()) {
  const entries = readAll(storage);
  const kept = entries.filter(([id]) => id !== itemId);
  if (kept.length !== entries.length) writeAll(storage, kept);
}
