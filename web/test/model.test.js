// @ts-check
/** Pure helpers of the practice screen (R-52, R-53, R-54, R-41). */

import assert from 'node:assert/strict';
import { test } from 'node:test';

import { displayIcon } from '../js/icon.js';
import {
  afterRating,
  caption,
  currentItem,
  exampleSentence,
  hasHint,
  hasMore,
  isSpelling,
  isTyped,
  moreOpenAfterCheck,
  moreOpenOnReveal,
  plainText,
  progress,
  wordSpeech,
} from '../js/practice/model.js';

/** @param {Partial<{ answered: any, is_new: boolean, sentence: any, other_sentences: any[], word: any, kind: string }>} o */
function item(o = {}) {
  return /** @type {import('../js/session.js').Item} */ ({
    item_id: Math.floor(Math.random() * 1e9),
    origin: 'new',
    answered: o.answered ?? null,
    card: { id: 1, kind: o.kind ?? 'understand', format: 'sentence', is_new: o.is_new ?? false, is_repeat: false, leech_flagged: false, last_failed: false },
    word: o.word ?? { headword: 'test-word', speak: null, note: '', hint: null, grammar: null, forms: [], tag: null },
    sentence: o.sentence ?? null,
    other_sentences: o.other_sentences ?? [],
    accept: [],
  });
}

/** @param {any[]} items */
const session = (items) => /** @type {import('../js/session.js').Session} */ ({ items });

const answered = { review_id: 1, raw_grade: 2 };

test('R-54 the next item is the first unanswered one in queue order', () => {
  const a = item({ answered });
  const b = item();
  const c = item();
  assert.equal(currentItem(session([a, b, c])), b);
  assert.equal(currentItem(session([a])), null);
});

test('R-54 progress shows the current position of the (growing) block', () => {
  assert.deepEqual(progress(session([item({ answered }), item(), item()])), { position: 2, total: 3, answered: 1 });
  assert.deepEqual(progress(session([item({ answered }), item({ answered })])), { position: 2, total: 2, answered: 2 });
});

test('R-41 speech reads the plain sentence and the pack speak text', () => {
  assert.equal(plainText({ pre: 'The ', target: 'glim', post: ' lamp.' }), 'The glim lamp.');
  assert.equal(wordSpeech(/** @type {any} */ ({ headword: 'lead', speak: 'led' })), 'led');
  assert.equal(wordSpeech(/** @type {any} */ ({ headword: 'lead', speak: null })), 'lead');
  assert.equal(wordSpeech(/** @type {any} */ ({ headword: 'lead', speak: '  ' })), 'lead');
});

test('R-52 word-only cards show one example: the item sentence, else the first other sentence', () => {
  const s1 = { id: 1 };
  const s2 = { id: 2 };
  assert.equal(exampleSentence(item({ sentence: s1, other_sentences: [s2] })), s1);
  assert.equal(exampleSentence(item({ other_sentences: [s2] })), s2);
  assert.equal(exampleSentence(item()), null);
});

test('R-52 new words open the full learn view after reveal', () => {
  assert.equal(moreOpenOnReveal(item({ is_new: true })), true);
  assert.equal(moreOpenOnReveal(item({ is_new: false })), false);
});

test('R-54 a 4 or 5 keeps the answer on screen until Next; 1-3 move on', () => {
  assert.equal(afterRating(1), 'next');
  assert.equal(afterRating(3), 'next');
  assert.equal(afterRating(4), 'feedback');
  assert.equal(afterRating(5), 'feedback');
});

test('R-53 the caption names the source and location when known', () => {
  assert.equal(caption(/** @type {any} */ ({ source_title: 'A Test Novel', location: 'Chapter 3' })), 'A Test Novel, Chapter 3');
  assert.equal(caption(/** @type {any} */ ({ source_title: 'A Test Novel', location: null })), 'A Test Novel');
  assert.equal(caption(/** @type {any} */ ({ source_title: null, location: '' })), '');
});

test('R-53 "More" is offered only when there is something in it', () => {
  assert.equal(hasMore(item()), false);
  assert.equal(hasMore(item({ word: { note: 'n', forms: [], tag: null } })), true);
  assert.equal(hasMore(item({ other_sentences: [{}] })), true);
});

test('R-74 only emoji icons are shown; icon-set names are not', () => {
  assert.equal(displayIcon('✨'), '✨');
  assert.equal(displayIcon('book-open'), null);
  assert.equal(displayIcon(null), null);
});

test('R-58 Produce and Spelling are typed cards; only Produce has a hint', () => {
  assert.deepEqual(
    ['understand', 'produce', 'spelling'].map((kind) => [isTyped(item({ kind })), isSpelling(item({ kind })), hasHint(item({ kind }))]),
    [
      [false, false, false],
      [true, false, true],
      [true, true, false],
    ],
  );
});

test('R-58 a missed Spelling card keeps "More" closed: the diff and the spelling note come first', () => {
  const word = { headword: 'test-word', note: 'A note.', hint: null, grammar: null, forms: [], tag: null };
  assert.equal(moreOpenAfterCheck(item({ kind: 'produce', word }), 'none'), true);
  assert.equal(moreOpenAfterCheck(item({ kind: 'spelling', word }), 'none'), false);
  assert.equal(moreOpenAfterCheck(item({ kind: 'spelling', word }), 'dont_know'), false);
});
