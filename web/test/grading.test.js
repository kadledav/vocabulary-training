// @ts-check
/** Typed-answer checking (R-56; design §6.8, D-5, D-6). Synthetic words only. */

import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  ACCEPTED_MAX,
  TYPED_MAX,
  acceptKeys,
  answersFor,
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

test('R-56 normalisation: case, punctuation, apostrophes and diacritics go; inner space is one space', () => {
  /** @type {[string, string][]} */
  const table = [
    ['Clattered', 'clattered'],
    ['CLATTERED', 'clattered'],
    ['  clattered  ', 'clattered'],
    ['\tclattered\n', 'clattered'],
    ['pore  over', 'pore over'],
    ['pore over', 'pore over'],
    ['pore\tover', 'pore over'],
    ['pot-luck', 'pot luck'],
    ['pot‐luck', 'pot luck'], // U+2010 hyphen
    ['pot‑luck', 'pot luck'], // U+2011 non-breaking hyphen
    ['pot–luck', 'pot luck'], // en dash
    ['pot—luck', 'pot luck'], // em dash
    ['pot−luck', 'pot luck'], // minus sign
    ['pot - luck', 'pot luck'],
    ['pot\u00a0luck', 'pot luck'], // no-break space
    ['pot\u00adluck', 'potluck'], // soft hyphen: invisible
    ['pot\u200bluck', 'potluck'], // zero-width space: invisible
    ['\u202epotluck', 'potluck'], // direction override: invisible
    ['well-known', 'well known'],
    ['a part', 'a part'],
    ['any way', 'any way'],
    ["don't", 'dont'],
    ['don’t', 'dont'],
    ['don‘t', 'dont'],
    ['donʼt', 'dont'],
    ['don`t', 'dont'],
    ['donꞌt', 'dont'], // U+A78C saltillo
    ['donꞋt', 'dont'], // U+A78B capital saltillo
    ['"verbatim"', 'verbatim'],
    ['“verbatim”', 'verbatim'],
    ['«verbatim»', 'verbatim'],
    ['verbatim.', 'verbatim'],
    ['verbatim!', 'verbatim'],
    ['verbatim?!', 'verbatim'],
    ['verbatim…', 'verbatim'],
    ['(verbatim)', 'verbatim'],
    ['( verbatim )', 'verbatim'],
    ['café', 'cafe'],
    ['CAFÉ', 'cafe'],
    ['naïve', 'naive'],
    ['encyclopædia', 'encyclopaedia'],
    ['Œuvre', 'oeuvre'],
    ['ﬁnesse', 'finesse'], // ligature folds under NFKD
    ['ｖｅｒｂａｔｉｍ', 'verbatim'], // full-width letters
    ['İnk', 'ink'],
    ['b2b', 'b2b'],
    ['', ''],
    ['  ', ''],
    ['?!', ''],
    [' - ', ''],
  ];
  for (const [input, key] of table) assert.equal(normalise(input), key, JSON.stringify(input));
});

test('R-56 a spaced and a closed form are different answers', () => {
  for (const [a, b] of [
    ['a part', 'apart'],
    ['any way', 'anyway'],
    ['pot luck', 'potluck'],
    ['well-known', 'wellknown'],
  ]) {
    assert.notEqual(normalise(a), normalise(b), `${a} / ${b}`);
    assert.equal(checkTyped(a, { accept: [b] }, false).match, 'none', `${a} does not match ${b}`);
    assert.equal(checkTyped(a, { accept: [b, a] }, false).match, 'exact', 'unless both are listed');
  }
  assert.equal(checkTyped('well known', { accept: ['well-known'] }, false).match, 'exact', 'a hyphen is a space');
});

test('R-56 normalisation is idempotent and keeps letters of other scripts', () => {
  for (const text of ['Pore Over', 'pot–luck', 'café', 'řeřicha', 'ß', ' a  - b ']) {
    assert.equal(normalise(normalise(text)), normalise(text));
  }
  assert.equal(normalise('řeřicha'), 'rericha');
  assert.equal(normalise('Straße'), 'straße', 'no language rules such as ß → ss');
});

/** An item's accept set as the server sends it (D-5). */
const ACCEPT = ['test-clatter', 'test-clattered', 'test-clattering', 'test-rattle'];

/** Without a sentence (a gloss-only card) every accepted answer is correct. */
const GLOSS = { accept: ACCEPT };

/** A sentence whose blank wants "test-clattered". */
const CLOZE = { accept: ACCEPT, target: 'test-clattered', forms: ['test-clatter', 'test-clattered', 'test-clattering', 'test-clatters'] };

test('R-56 the target span of the shown sentence is correct: 2, after the hint 3', () => {
  /** @type {string[]} */
  const matches = ['test-clattered', 'Test-Clattered', ' test clattered ', 'test–clattered.', 'TEST CLATTERED!'];
  for (const typed of matches) {
    const check = checkTyped(typed, CLOZE, false);
    assert.deepEqual(check, { match: 'exact', verdict: 'correct', suggested: 2, typed, hintUsed: false }, typed);
    assert.equal(checkTyped(typed, CLOZE, true).suggested, 3, `${typed} after the hint`);
  }
});

test('R-56 another form of the word is the right word, not correct: 3 (guarantor decision 2026-11-27)', () => {
  for (const typed of ['test-clatter', 'Test Clatter', 'test-clattering', 'test-clatters']) {
    const check = checkTyped(typed, CLOZE, false);
    assert.deepEqual(check, { match: 'exact', verdict: 'right_word', suggested: 3, typed, hintUsed: false }, typed);
    assert.equal(checkTyped(typed, CLOZE, true).suggested, 3, 'the hint changes nothing');
  }
  // An accepted answer that is not a listed form: accepted, also 3.
  assert.deepEqual(checkTyped('test-rattle', CLOZE, false), {
    match: 'exact',
    verdict: 'alternative',
    suggested: 3,
    typed: 'test-rattle',
    hintUsed: false,
  });
});

test('R-56 without a sentence the headword and every accepted answer are correct', () => {
  for (const typed of ['test-clatter', 'test-clattered', 'test-rattle', 'TEST CLATTER']) {
    assert.equal(checkTyped(typed, GLOSS, false).verdict, 'correct', typed);
    assert.equal(checkTyped(typed, GLOSS, false).suggested, 2, typed);
  }
  // A blank target counts as no sentence.
  assert.equal(checkTyped('test-clatter', { ...CLOZE, target: ' ' }, false).verdict, 'correct');
});

test('R-56 answersFor takes the sentence target, the forms and answers accepted in this block', () => {
  const item = {
    accept: ['test-clatter', 'test-clattered'],
    sentence: { en: { target: 'test-clattered' } },
    word: { headword: 'test-clatter', forms: [{ form: 'test-clatters' }] },
  };
  assert.deepEqual(answersFor(item, ['test-rattled']), {
    accept: ['test-clatter', 'test-clattered', 'test-rattled'],
    target: 'test-clattered',
    forms: ['test-clatter', 'test-clatters'],
  });
  assert.equal(answersFor({ ...item, sentence: null }).target, null);
  assert.equal(checkTyped('test-rattled', answersFor(item, ['test-rattled']), false).match, 'exact');
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
    ['test-clatterz', 'a form the pack did not list'],
    ['to test-clatter', '"to " is not dropped (no language logic)'],
    ['the test-clatter', 'articles are not dropped'],
    ['a test-rattle', 'articles are not dropped'],
    ['test-banging', 'another word of the pack, not accepted here'],
    ['test-clattered test-clattered', 'said twice'],
    ['test', 'only a part'],
    ['clattered', 'only a part'],
    ['testclattered', 'closed up: the space (hyphen) counts'],
    ['test-clattered2', 'a digit more'],
  ];
  for (const answers of [GLOSS, CLOZE]) {
    for (const [typed, why] of misses) {
      const check = checkTyped(typed, answers, false);
      assert.equal(check.match, 'none', `${typed}: ${why}`);
      assert.equal(check.verdict, 'miss', `${typed}: ${why}`);
      assert.equal(check.suggested, 4, `${typed}: ${why}`);
      assert.equal(checkTyped(typed, answers, true).suggested, 4, `${typed} with the hint is still a 4`);
    }
  }
});

test('R-56 British/American variants match only when the pack lists them', () => {
  assert.equal(checkTyped('test-colour', { accept: ['test-color'] }, false).match, 'none');
  assert.equal(checkTyped('test-colour', { accept: ['test-color', 'test-colour'] }, false).match, 'exact');
});

test('R-56 an empty or punctuation-only answer never matches, even an empty accept entry', () => {
  for (const typed of ['', '   ', '-', '…', "'", '\u200b', '\ufeff']) {
    assert.equal(checkable(typed), false, JSON.stringify(typed));
    assert.equal(checkTyped(typed, { accept: ['', '-', ...ACCEPT] }, false).match, 'none');
    assert.equal(checkTyped(typed, { ...CLOZE, target: '-' }, false).match, 'none');
  }
  assert.deepEqual([...acceptKeys(['', ' - ', 'A b', 'a-b'])], ['a b']);
});

test('R-56 checking is deterministic and does not depend on accept order', () => {
  const a = checkTyped('Test Rattle', { accept: ACCEPT }, false);
  const b = checkTyped('Test Rattle', { accept: [...ACCEPT].reverse() }, false);
  assert.deepEqual(a, b);
  assert.deepEqual(checkTyped('x', { accept: [] }, false), { match: 'none', verdict: 'miss', suggested: 4, typed: 'x', hintUsed: false });
});

test('R-56 "I don\'t know" suggests 5, "I\'ll just rate myself" suggests nothing; never a 1', () => {
  assert.deepEqual(dontKnow(false), { match: 'dont_know', verdict: 'dont_know', suggested: 5, typed: null, hintUsed: false });
  assert.deepEqual(dontKnow(true).hintUsed, true);
  assert.deepEqual(selfRated(false), { match: 'skipped', verdict: 'skipped', suggested: null, typed: null, hintUsed: false });
  for (const typed of ['test-clattered', 'test-clatter', 'nope', '']) {
    for (const hintUsed of [false, true]) {
      assert.notEqual(checkTyped(typed, GLOSS, hintUsed).suggested, 1);
      assert.notEqual(checkTyped(typed, CLOZE, hintUsed).suggested, 1);
    }
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

test('R-56 "also accept" is offered only for a typed answer that did not match and fits the list', () => {
  assert.equal(canAddAccepted(checkTyped('test-clatterd', GLOSS, false)), true);
  assert.equal(canAddAccepted(checkTyped('test-clattered', GLOSS, false)), false);
  assert.equal(canAddAccepted(checkTyped('test-clatter', CLOZE, false)), false, 'the right word is not offered');
  assert.equal(canAddAccepted(checkTyped('x'.repeat(ACCEPTED_MAX), GLOSS, false)), true);
  assert.equal(canAddAccepted(checkTyped('x'.repeat(ACCEPTED_MAX + 1), GLOSS, false)), false, 'the list takes 200 characters');
  assert.equal(canAddAccepted(dontKnow(false)), false);
  assert.equal(canAddAccepted(selfRated(false)), false);
  assert.equal(canAddAccepted(checkTyped('!!', GLOSS, false)), false);
  assert.equal(canAddAccepted(null), false);
});
