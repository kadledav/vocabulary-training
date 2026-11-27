// @ts-check
/** Spelling: strict comparison and the letter diff (R-58; design §6.8). Invented words only. */

import assert from 'node:assert/strict';
import { test } from 'node:test';

import { readFileSync } from 'node:fs';

import { DIFF_SUMMARY_MAX, INVISIBLE_RANGES, checkSpelling, diffSummary, normalise, spellingDiff, spellingText, typedText } from '../js/grading.js';
import { storedCheck } from '../js/practice/draft.js';

/**
 * A diff in one line: same letters as they are, [x>y] wrong (typed x, the
 * word has y), [+y] missing, [-x] extra.
 *
 * @param {string} typed
 * @param {string} correct
 */
function line(typed, correct) {
  return spellingDiff(typed, correct)
    .map((c) => {
      if (c.mark === 'same') return c.typed;
      if (c.mark === 'wrong') return `[${c.typed}>${c.correct}]`;
      if (c.mark === 'missing') return `[+${c.correct}]`;
      return `[-${c.typed}]`;
    })
    .join('');
}

test('R-58 the letter diff: a broad table of slips', () => {
  /** @type {[string, string, string][]} typed, the word, the diff */
  const table = [
    // exact, and case only
    ['zibberate', 'zibberate', 'zibberate'],
    ['Zibberate', 'zibberate', 'Zibberate'],
    ['ZIBBERATE', 'zibberate', 'ZIBBERATE'],
    // substitution
    ['zibberote', 'zibberate', 'zibber[o>a]te'],
    // insertion (an extra letter) and deletion (a missing one)
    ['zibberrate', 'zibberate', 'zibbe[-r]rate'],
    ['ziberate', 'zibberate', 'zi[+b]berate'],
    // doubled letters, both ways and both at once
    ['dessiccant', 'desiccant', 'de[-s]siccant'],
    ['desicant', 'desiccant', 'desi[+c]cant'],
    ['dessicant', 'desiccant', 'de[-s]si[+c]cant'],
    // transposition: one extra and one missing around the letter it jumped
    ['quiet', 'quite', 'qui[-e]t[+e]'],
    ['ab', 'ba', '[-a]b[+a]'],
    ['flimbre', 'flimber', 'flimb[-r]e[+r]'],
    // hyphens, spaces and apostrophes are part of the spelling
    ['gorp bomb', 'gorp-bomb', 'gorp[ >-]bomb'],
    ['gorpbomb', 'gorp-bomb', 'gorp[+-]bomb'],
    ['pore over', 'pore-over', 'pore[ >-]over'],
    ['poreover', 'pore over', 'pore[+ ]over'],
    ['po reover', 'pore over', 'po[- ]re[+ ]over'],
    ["o'clonk", "o'clonk", "o'clonk"],
    ['oclonk', "o'clonk", "o[+']clonk"],
    ['o’clonk', "o'clonk", "o'clonk"],
    // multi-word answers
    ['take a gander', 'take a gander', 'take a gander'],
    ['take a gandre', 'take a gander', 'take a gand[-r]e[+r]'],
    ['take gander', 'take a gander', 'take[+ ][+a] gander'],
    // diacritics are letters of their own
    ['naive', 'naïve', 'na[i>ï]ve'],
    // empty input: every letter missing
    ['', 'zib', '[+z][+i][+b]'],
    ['   ', 'zib', '[+z][+i][+b]'],
    // nothing in common
    ['xyz', 'abc', '[x>a][y>b][z>c]'],
  ];
  for (const [typed, correct, expected] of table) {
    assert.equal(line(typed, correct), expected, `${typed} → ${correct}`);
  }
});

test('R-58 every diff column keeps the letters: typed ones read back what was typed, correct ones the word', () => {
  const pairs = [
    ['dessicant', 'desiccant'],
    ['Quiet', 'quite'],
    ['take gander', 'take a gander'],
    ['', 'zib'],
    ['zib', ''],
  ];
  for (const [typed, correct] of pairs) {
    const diff = spellingDiff(typed, correct);
    assert.equal(diff.map((c) => c.typed ?? '').join(''), spellingText(typed), typed);
    assert.equal(diff.map((c) => c.correct ?? '').join(''), spellingText(correct), correct);
  }
});

test('R-58 a very long input is compared in good time and every letter is accounted for', () => {
  const word = 'zibberate';
  const typed = 'zibberote '.repeat(30).trim();
  const started = performance.now();
  const diff = spellingDiff(typed, word);
  assert.ok(performance.now() - started < 500, 'well under half a second');
  assert.equal(diff.filter((c) => c.typed !== null).length, [...typed].length);
  assert.equal(diff.filter((c) => c.correct !== null).length, word.length);
  assert.equal(checkSpelling(typed, word).suggested, 4);
});

test('R-58 the comparison is strict: only case, NFC, outer and repeated white space and the apostrophe glyph do not count', () => {
  /** @type {[string, string, 2 | 4][]} */
  const table = [
    ['zibberate', 'zibberate', 2],
    ['Zibberate', 'zibberate', 2],
    ['  zibberate \n', 'zibberate', 2],
    ['take  a\tgander', 'take a gander', 2],
    ['o’clonk', "o'clonk", 2],
    ['o‘clonk', "o'clonk", 2],
    ['naïve', 'naïve', 2], // decomposed ï is the same text
    ['zibberat', 'zibberate', 4],
    ['zibberatee', 'zibberate', 4],
    ['naive', 'naïve', 4], // no diacritic folding
    ['gorp bomb', 'gorp-bomb', 4], // a hyphen is part of the spelling
    ['gorpbomb', 'gorp bomb', 4],
    ['oclonk', "o'clonk", 4],
    ['zibberate.', 'zibberate', 4], // punctuation is not dropped
    ['', 'zibberate', 4],
  ];
  for (const [typed, correct, suggested] of table) {
    const check = checkSpelling(typed, correct);
    assert.equal(check.suggested, suggested, `${typed} → ${correct}`);
    assert.equal(check.match, suggested === 2 ? 'exact' : 'none');
    assert.equal(check.verdict, suggested === 2 ? 'correct' : 'miss');
    assert.equal(check.typed, typed, 'the typed text is kept as typed');
    assert.equal(check.hintUsed, false, 'Spelling has no hint');
  }
});

test('R-58 a Spelling check survives a reload like a Produce check', () => {
  for (const check of [checkSpelling('zibberate', 'zibberate'), checkSpelling('ziberate', 'zibberate')]) {
    assert.deepEqual(storedCheck(JSON.parse(JSON.stringify(check))), check);
  }
});

test('R-58 the diff in words for a screen reader', () => {
  assert.equal(diffSummary(spellingDiff('zibberate', 'zibberate')), '');
  assert.equal(diffSummary(spellingDiff('zibberote', 'zibberate')), 'letter 7: you typed “o”, the word has “a”');
  assert.equal(diffSummary(spellingDiff('ziberate', 'zibberate')), 'letter 3 missing: “b”');
  assert.equal(diffSummary(spellingDiff('zibberrate', 'zibberate')), 'extra “r” after letter 5');
  assert.equal(diffSummary(spellingDiff('ab', 'ba')), 'extra “a” at the start; letter 2 missing: “a”');
  assert.equal(diffSummary(spellingDiff('poreover', 'pore over')), 'letter 5 missing: a space');
});

test('R-58 invisible characters do not make a right spelling wrong', () => {
  for (const typed of [
    'zibberate\u200b',
    'zib\u200bberate',
    '\u200ezibberate',
    'zibber\u200fate',
    'zib\u00adberate',
    '\ufeffzibberate',
    '\u202ezibberate\u202c',
    'zibb\u2066er\u2069ate',
    'zibberate\u2060',
  ]) {
    assert.equal(checkSpelling(typed, 'zibberate').match, 'exact', JSON.stringify(typed));
    assert.equal(typedText(typed), 'zibberate', JSON.stringify(typed));
  }
  // Only invisible characters: nothing typed.
  assert.equal(spellingText('\u200b\u202e'), '');
  assert.equal(checkSpelling('\u200b', 'zibberate').match, 'none');
  // A wrong answer's diff never has a column for an invisible character.
  const diff = spellingDiff('zib\u200bbrate', 'zibberate');
  assert.ok(diff.every((c) => c.typed !== '\u200b'));
  assert.equal(diffSummary(diff), 'letter 5 missing: “e”');
});

test('R-58 the invisible characters are the server’s set (vocab_pack::keys::is_invisible)', () => {
  const source = readFileSync(new URL('../../crates/vocab-pack/src/keys.rs', import.meta.url), 'utf8');
  const body = source.slice(source.indexOf('pub fn is_invisible'), source.indexOf('pub fn strip_invisible'));
  const code = (/** @type {string} */ hex) => parseInt(hex, 16);
  const server = [...body.matchAll(/'\\u\{([0-9A-Fa-f]+)\}'(?:\.\.='\\u\{([0-9A-Fa-f]+)\}')?/g)].map((m) => [code(m[1]), code(m[2] ?? m[1])]);
  const sort = (/** @type {ReadonlyArray<readonly number[]>} */ ranges) => [...ranges].map((r) => [...r]).sort((a, b) => a[0] - b[0]);
  assert.ok(server.length >= 5, 'the function was found');
  assert.deepEqual(sort(INVISIBLE_RANGES), sort(server));
  // Produce compares without them too.
  assert.equal(normalise('pot\u200bluck'), normalise('potluck'));
});

test('R-58 a long wrong answer is summed up briefly', () => {
  assert.equal(diffSummary(spellingDiff('zibberate' + 'q'.repeat(12), 'zibberate')), '12 extra letters after letter 9');
  assert.equal(diffSummary(spellingDiff('q'.repeat(12) + 'zibberate', 'zibberate')), '12 extra letters at the start');
  assert.equal(diffSummary(spellingDiff('zibberateqq', 'zibberate')), '2 extra letters “qq” after letter 9');
  assert.equal(diffSummary(spellingDiff('zirate', 'zibberate')), 'letters 3 to 5 missing: “bbe”');
  assert.equal(diffSummary(spellingDiff('zixxxrate', 'zibberate')), 'letters 3 to 5: you typed “xxx”, the word has “bbe”');
  // Hopeless: fewer than half the letters right.
  assert.equal(diffSummary(spellingDiff('q'.repeat(300), 'quabb')), 'very different from the word, which is spelled “quabb”: q, u, a, b, b');
  assert.equal(diffSummary(spellingDiff('x'.repeat(300), 'y'.repeat(300))).length <= DIFF_SUMMARY_MAX, true);
  // Many scattered slips: capped.
  const word = 'zibberatequabblingglimmerousbrontle';
  const typed = [...word].map((c, i) => (i % 3 === 2 ? 'x' : c)).join('');
  const summary = diffSummary(spellingDiff(typed, word));
  assert.ok(summary.length <= DIFF_SUMMARY_MAX, summary);
  assert.match(summary, /…and \d+ more differences$/);
});
