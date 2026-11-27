// @ts-check
/** Typed-answer checking (R-56; design §6.8, D-5, D-6). Synthetic words only. */

import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  TYPED_MAX,
  acceptKeys,
  canAddAccepted,
  checkTyped,
  checkable,
  dontKnow,
  expectedAnswer,
  hint,
  normalise,
  selfRated,
  typedText,
} from '../js/grading.js';

test('R-56 normalisation: case, spaces, punctuation, apostrophes, dashes and diacritics only', () => {
  /** @type {[string, string][]} */
  const table = [
    ['Clattered', 'clattered'],
    ['CLATTERED', 'clattered'],
    ['  clattered  ', 'clattered'],
    ['\tclattered\n', 'clattered'],
    ['pore  over', 'poreover'],
    ['pore over', 'poreover'],
    ['pot-luck', 'potluck'],
    ['pot‐luck', 'potluck'], // U+2010 hyphen
    ['pot‑luck', 'potluck'], // U+2011 non-breaking hyphen
    ['pot–luck', 'potluck'], // en dash
    ['pot—luck', 'potluck'], // em dash
    ['pot−luck', 'potluck'], // minus sign
    ['pot luck', 'potluck'], // no-break space
    ["don't", 'dont'],
    ['don’t', 'dont'],
    ['don‘t', 'dont'],
    ['donʼt', 'dont'],
    ['don`t', 'dont'],
    ['"verbatim"', 'verbatim'],
    ['“verbatim”', 'verbatim'],
    ['«verbatim»', 'verbatim'],
    ['verbatim.', 'verbatim'],
    ['verbatim!', 'verbatim'],
    ['verbatim?!', 'verbatim'],
    ['verbatim…', 'verbatim'],
    ['(verbatim)', 'verbatim'],
    ['café', 'cafe'],
    ['CAFÉ', 'cafe'],
    ['naïve', 'naive'],
    ['ﬁnesse', 'finesse'], // ligature folds under NFKD
    ['ｖｅｒｂａｔｉｍ', 'verbatim'], // full-width letters
    ['İnk', 'ink'],
    ['b2b', 'b2b'],
    ['', ''],
    ['  ', ''],
    ['?!', ''],
  ];
  for (const [input, key] of table) assert.equal(normalise(input), key, JSON.stringify(input));
});

test('R-56 normalisation is idempotent and keeps letters of other scripts', () => {
  for (const text of ['Pore Over', 'pot–luck', 'café', 'řeřicha', 'ß']) {
    assert.equal(normalise(normalise(text)), normalise(text));
  }
  assert.equal(normalise('řeřicha'), 'rericha');
  assert.equal(normalise('Straße'), 'straße', 'no language rules such as ß → ss');
});

/** An item's accept set as the server sends it (D-5). */
const ACCEPT = ['test-clatter', 'test-clattered', 'test-clattering', 'test-rattle'];

test('R-56 a typed answer matching the accept set suggests 2, after the hint 3', () => {
  /** @type {string[]} */
  const matches = [
    'test-clattered',
    'Test-Clattered',
    ' test clattered ',
    'testclattered',
    'test–clattered.',
    'TEST CLATTERED!',
    'test-clatter', // the headword is in the set (D-5)
    'test-rattle', // the pack's accepted alternative
  ];
  for (const typed of matches) {
    const check = checkTyped(typed, ACCEPT, false);
    assert.deepEqual(check, { match: 'exact', suggested: 2, typed, hintUsed: false }, typed);
    assert.equal(checkTyped(typed, ACCEPT, true).suggested, 3, `${typed} after the hint`);
  }
});

test('R-56 everything else is a no-match with 4, never a pass', () => {
  /** @type {[string, string][]} */
  const misses = [
    ['test-clatterd', 'one letter missing'],
    ['test-clattred', 'one letter missing inside'],
    ['test-clatered', 'single t'],
    ['test-clatteredd', 'one letter extra'],
    ['test-clsttered', 'one wrong letter (typo)'],
    ['test-clatterde', 'two letters swapped'],
    ['test-clatters', 'a form the pack did not list'],
    ['to test-clatter', '"to " is not dropped (no language logic)'],
    ['the test-clatter', 'articles are not dropped'],
    ['a test-rattle', 'articles are not dropped'],
    ['test-banging', 'another word of the pack, not accepted here'],
    ['test-clattered test-clattered', 'said twice'],
    ['test', 'only a part'],
    ['clattered', 'only a part'],
    ['test-clattered2', 'a digit more'],
  ];
  for (const [typed, why] of misses) {
    const check = checkTyped(typed, ACCEPT, false);
    assert.equal(check.match, 'none', `${typed}: ${why}`);
    assert.equal(check.suggested, 4, `${typed}: ${why}`);
    assert.equal(checkTyped(typed, ACCEPT, true).suggested, 4, `${typed} with the hint is still a 4`);
  }
});

test('R-56 British/American variants match only when the pack lists them', () => {
  assert.equal(checkTyped('test-colour', ['test-color'], false).match, 'none');
  assert.equal(checkTyped('test-colour', ['test-color', 'test-colour'], false).match, 'exact');
});

test('R-56 an empty or punctuation-only answer never matches, even an empty accept entry', () => {
  for (const typed of ['', '   ', '-', '…', "'"]) {
    assert.equal(checkable(typed), false, JSON.stringify(typed));
    assert.equal(checkTyped(typed, ['', '-', ...ACCEPT], false).match, 'none');
  }
  assert.deepEqual([...acceptKeys(['', ' - ', 'A b', 'a-b'])], ['ab']);
});

test('R-56 checking is deterministic and does not depend on accept order', () => {
  const a = checkTyped('Test Rattle', ACCEPT, false);
  const b = checkTyped('Test Rattle', [...ACCEPT].reverse(), false);
  assert.deepEqual(a, b);
  assert.deepEqual(checkTyped('x', [], false), { match: 'none', suggested: 4, typed: 'x', hintUsed: false });
});

test('R-56 "I don\'t know" suggests 5, "I\'ll just rate myself" suggests nothing; never a 1', () => {
  assert.deepEqual(dontKnow(false), { match: 'dont_know', suggested: 5, typed: null, hintUsed: false });
  assert.deepEqual(dontKnow(true).hintUsed, true);
  assert.deepEqual(selfRated(false), { match: 'skipped', suggested: null, typed: null, hintUsed: false });
  for (const typed of ['test-clattered', 'nope', '']) {
    for (const hintUsed of [false, true]) assert.notEqual(checkTyped(typed, ACCEPT, hintUsed).suggested, 1);
  }
});

test('R-56 the hint shows the first letter and the letter count', () => {
  assert.deepEqual(hint('clattered'), { pattern: 'c _ _ _ _ _ _ _ _', letters: 9 });
  assert.deepEqual(hint('pore over'), { pattern: 'p _ _ _   _ _ _ _', letters: 8 });
  assert.deepEqual(hint('pot-luck'), { pattern: 'p _ _ - _ _ _ _', letters: 7 });
  assert.deepEqual(hint("o'clock"), { pattern: "o ' _ _ _ _ _", letters: 6 });
  assert.deepEqual(hint('  Ab  '), { pattern: 'A _', letters: 2 });
  assert.deepEqual(hint('x'), { pattern: 'x', letters: 1 });
});

test('R-56 the typed text is cleaned for storage, never changed otherwise', () => {
  assert.equal(typedText('  test-clattered \n'), 'test-clattered');
  assert.equal(typedText('test\tclattered'), 'test clattered');
  assert.equal(typedText('Test–Clattered!'), 'Test–Clattered!');
  assert.equal([...typedText('ř'.repeat(TYPED_MAX + 20))].length, TYPED_MAX);
  assert.equal(typedText('\u0000\u0007'), '');
});

test('R-56 the expected answer is the sentence target, else the headword', () => {
  const word = { headword: 'test-clatter' };
  assert.equal(expectedAnswer({ word, sentence: { en: { target: 'test-clattered' } } }), 'test-clattered');
  assert.equal(expectedAnswer({ word, sentence: { en: { target: '  ' } } }), 'test-clatter');
  assert.equal(expectedAnswer({ word, sentence: null }), 'test-clatter');
});

test('R-56 "also accept" is offered only for a typed answer that did not match', () => {
  assert.equal(canAddAccepted(checkTyped('test-clatterd', ACCEPT, false)), true);
  assert.equal(canAddAccepted(checkTyped('test-clattered', ACCEPT, false)), false);
  assert.equal(canAddAccepted(dontKnow(false)), false);
  assert.equal(canAddAccepted(selfRated(false)), false);
  assert.equal(canAddAccepted(checkTyped('!!', ACCEPT, false)), false);
  assert.equal(canAddAccepted(null), false);
});
