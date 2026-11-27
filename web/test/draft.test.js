// @ts-check
/** The Produce draft and check kept across a reload (R-56; M8 review findings 6 and 11). */

import assert from 'node:assert/strict';
import { test } from 'node:test';

import { checkTyped, dontKnow } from '../js/grading.js';
import { clearDraft, loadDraft, saveDraft, storedCheck } from '../js/practice/draft.js';

/** An in-memory sessionStorage. */
function memory() {
  /** @type {Map<string, string>} */
  const data = new Map();
  return {
    /** @param {string} k */
    getItem: (k) => data.get(k) ?? null,
    /** @param {string} k @param {string} v */
    setItem: (k, v) => void data.set(k, v),
    /** @param {string} k */
    removeItem: (k) => void data.delete(k),
    data,
  };
}

test('R-56 a checked answer survives a reload until the item is rated', () => {
  const storage = memory();
  const check = checkTyped('test-clatter', { accept: ['test-clatter'], target: 'test-clattered', forms: ['test-clatter'] }, true);
  saveDraft(7, { typed: 'test-clatter', hint: true, check }, storage);
  assert.deepEqual(loadDraft(7, storage), { typed: 'test-clatter', hint: true, check });
  assert.equal(loadDraft(7, storage)?.check?.verdict, 'right_word');
  clearDraft(7, storage);
  assert.equal(loadDraft(7, storage), null);
  assert.equal(storage.data.size, 0);
});

test('R-56 drafts are per item: undoing the previous card leaves the current draft alone', () => {
  const storage = memory();
  saveDraft(8, { typed: 'test-half', hint: false, check: null }, storage);
  // The previous item (7) is undone and reopened: nothing stored for it.
  assert.equal(loadDraft(7, storage), null);
  clearDraft(7, storage);
  assert.deepEqual(loadDraft(8, storage), { typed: 'test-half', hint: false, check: null });
  // Typing replaces the item's own entry only.
  saveDraft(7, { typed: 'x', hint: false, check: dontKnow(false) }, storage);
  saveDraft(8, { typed: 'test-halves', hint: true, check: null }, storage);
  assert.equal(loadDraft(8, storage)?.typed, 'test-halves');
  assert.equal(loadDraft(7, storage)?.check?.match, 'dont_know');
});

test('R-56 damaged or foreign entries are ignored; only a few items are kept', () => {
  const storage = memory();
  storage.setItem('vocab.typed-draft', '{not json');
  assert.equal(loadDraft(1, storage), null);
  storage.setItem('vocab.typed-draft', JSON.stringify({ itemId: 1, typed: 'old format' }));
  assert.equal(loadDraft(1, storage), null);
  storage.setItem('vocab.typed-draft', JSON.stringify({ entries: [[1, { typed: 'a', check: { match: 'exact', verdict: 'correct', suggested: 1, typed: 'a', hintUsed: false } }]] }));
  assert.deepEqual(loadDraft(1, storage), { typed: 'a', hint: false, check: null }, 'a check suggesting 1 is no check');
  assert.equal(storedCheck({ match: 'fuzzy', verdict: 'correct', suggested: 2, typed: 'a', hintUsed: false }), null);
  for (let id = 1; id <= 20; id++) saveDraft(id, { typed: String(id), hint: false, check: null }, storage);
  assert.equal(loadDraft(1, storage), null, 'the oldest go');
  assert.equal(loadDraft(20, storage)?.typed, '20');
  assert.equal(loadDraft(5, null), null, 'no storage at all');
});
