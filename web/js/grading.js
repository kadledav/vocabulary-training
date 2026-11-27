// @ts-check
/**
 * Mechanical checking of a typed answer (design §6.8, R-56; research §3
 * "Typed answers"). Pure, no DOM, deterministic.
 *
 * The app makes no language judgement (requirements §1): no edit
 * distance, no stemming, no synonyms, no dropping of articles or "to ".
 * An answer counts only when it equals, after normalisation, one of the
 * answers the server supplies for the item (pack `accepted` ∪ headword ∪
 * the target span of the shown sentence ∪ the owner's accepted answers)
 * or a listed form of the word; with a sentence shown, only its target
 * span is fully correct. Everything else — a one-letter typo included — is a no-match with a
 * suggested 4, and the owner, who sees what he typed above the answer,
 * rates it himself. The suggestion never says 1 ("Instantly" is his call)
 * and the owner's 1–5 rating is always the final word.
 */

/**
 * How a typed answer compared, as stored with the review:
 * - `exact`: matched an accepted answer (the sentence's own target span,
 *   or — a friendly verdict, suggestion 3 — another form of the word or an
 *   accepted alternative);
 * - `none`: typed, no match;
 * - `dont_know`: "I don't know";
 * - `skipped`: "I'll just rate myself" (nothing typed, no suggestion).
 *
 * What the owner is told (design §6.8, guarantor decision of 2026-11-27):
 * - `correct`: the target span of the shown sentence (without a sentence:
 *   any accepted answer) — "✓ Correct", 2 (3 after the hint);
 * - `right_word`: a sentence is shown and the headword or another listed
 *   form of the word was typed — "Right word — here it is …", 3;
 * - `alternative`: a sentence is shown and another accepted answer was
 *   typed (the pack's and the owner's lists do not say whether it fits
 *   this blank) — "Accepted — this sentence has …", 3;
 * - `miss`, `dont_know`, `skipped` as their match.
 *
 * @typedef {'exact' | 'none' | 'dont_know' | 'skipped'} AnswerMatch
 * @typedef {'correct' | 'right_word' | 'alternative' | 'miss' | 'dont_know' | 'skipped'} Verdict
 * @typedef {import('./rating.js').Grade} Grade
 * @typedef {{ match: AnswerMatch, verdict: Verdict, suggested: Grade | null, typed: string | null, hintUsed: boolean }} Check
 * @typedef {{ accept: readonly string[], target?: string | null, forms?: readonly string[] }} Answers
 */

/**
 * Modifier letters and other letters that keyboards use as apostrophes,
 * primes or accents (ʹ ʺ ʻ ʼ ʽ ʾ ʿ ˈ ˊ ˋ, saltillo ꞌ Ꞌ).
 */
const APOSTROPHE_LETTERS = /[ʹ-ʿˈˊˋꞋꞌ]/gu;

/**
 * Letters NFKD leaves whole that are ligatures or letters with a stroke —
 * folded like diacritics. No language rules: ß stays ß.
 *
 * @type {Record<string, string>}
 */
const FOLDS = { æ: 'ae', œ: 'oe', ø: 'o', ł: 'l', đ: 'd' };
const FOLDABLE = /[æœøłđ]/gu;

/**
 * The comparison key of an answer: lowercase, diacritics and ligatures
 * folded (NFKD without combining marks, æ → ae), apostrophes, quotes and
 * other punctuation dropped (don’t = dont), invisible characters dropped,
 * and white space significant: hyphens, dashes and runs of white space
 * become one space, so "pot-luck" = "pot luck" but neither is "potluck",
 * and "a part" is not "apart" — a spaced and a closed form match only
 * when both are listed.
 *
 * @param {string} text
 */
export function normalise(text) {
  return text
    .toLowerCase()
    .normalize('NFKD')
    .replace(/\p{M}/gu, '')
    .replace(FOLDABLE, (c) => FOLDS[c] ?? c)
    .replace(APOSTROPHE_LETTERS, '')
    .replace(/[\s\p{Pd}\u2212]+/gu, ' ')
    .replace(/[^\p{L}\p{N} ]/gu, '')
    .replace(/ {2,}/g, ' ')
    .trim();
}

/** Longest typed answer the server stores (characters). */
export const TYPED_MAX = 300;

/**
 * The typed text as it is checked and stored: control characters (a
 * pasted tab) become spaces, surrounding space goes, at most
 * [`TYPED_MAX`] characters.
 *
 * @param {string} raw
 */
export function typedText(raw) {
  return [...raw.replace(/\p{Cc}/gu, ' ').trim()].slice(0, TYPED_MAX).join('').trim();
}

/**
 * Has the typed text anything to compare (a letter or digit)?
 *
 * @param {string} typed
 */
export function checkable(typed) {
  return normalise(typed) !== '';
}

/**
 * The comparison keys of the accept set (empty keys dropped).
 *
 * @param {readonly string[]} accept
 * @returns {Set<string>}
 */
export function acceptKeys(accept) {
  return new Set(accept.map(normalise).filter((key) => key !== ''));
}

/**
 * What a typed answer of `item` is compared with: its accept set (plus
 * answers the owner accepted during this block), the target span of the
 * shown sentence and the word's headword and listed forms.
 *
 * @param {{ accept: readonly string[], sentence: { en: { target: string } } | null,
 *           word: { headword: string, forms: readonly { form: string }[] } }} item
 * @param {readonly string[]} [alsoAccepted]
 * @returns {Answers}
 */
export function answersFor(item, alsoAccepted = []) {
  const target = item.sentence?.en.target.trim() ?? '';
  return {
    accept: [...item.accept, ...alsoAccepted],
    target: target !== '' ? target : null,
    forms: [item.word.headword, ...item.word.forms.map((f) => f.form)],
  };
}

/**
 * Check a typed answer (verdicts above): the sentence's target span — or,
 * without a sentence, any accepted answer — suggests 2 (3 after the
 * hint); another form of the word or another accepted answer while a
 * sentence is shown suggests 3; no match suggests 4.
 *
 * @param {string} typed what the owner typed
 * @param {Answers} answers the item's answers ([`answersFor`])
 * @param {boolean} hintUsed
 * @returns {Check}
 */
export function checkTyped(typed, { accept, target = null, forms = [] }, hintUsed) {
  const key = normalise(typed);
  const targetKey = target === null ? '' : normalise(target);
  /** @type {Verdict} */
  let verdict = 'miss';
  if (key !== '' && targetKey !== '') {
    if (key === targetKey) verdict = 'correct';
    else if (acceptKeys(forms).has(key)) verdict = 'right_word';
    else if (acceptKeys(accept).has(key)) verdict = 'alternative';
  } else if (key !== '' && acceptKeys(accept).has(key)) {
    verdict = 'correct';
  }
  /** @type {Grade} */
  const suggested = verdict === 'miss' ? 4 : verdict === 'correct' && !hintUsed ? 2 : 3;
  return { match: verdict === 'miss' ? 'none' : 'exact', verdict, suggested, typed, hintUsed };
}

/**
 * "I don't know": suggest 5.
 *
 * @param {boolean} hintUsed
 * @returns {Check}
 */
export function dontKnow(hintUsed) {
  return { match: 'dont_know', verdict: 'dont_know', suggested: 5, typed: null, hintUsed };
}

/**
 * "I'll just rate myself": reveal, no suggestion.
 *
 * @param {boolean} hintUsed
 * @returns {Check}
 */
export function selfRated(hintUsed) {
  return { match: 'skipped', verdict: 'skipped', suggested: null, typed: null, hintUsed };
}

/**
 * The hint (design §6.8, D-6): the first letter and how many letters the
 * answer has, e.g. "c _ _ _ _ _ _ _ _ _" for "clattered". Spaces stay as
 * gaps and other characters (hyphen, apostrophe) as they are, so the
 * shape of a phrase shows ("p _ _ _   o _ _ _" for "pore over").
 *
 * @param {string} answer the expected answer (the sentence's target, else the headword)
 * @returns {{ pattern: string, letters: number }}
 */
export function hint(answer) {
  const chars = [...answer.trim()];
  let letters = 0;
  const parts = chars.map((ch) => {
    if (/^\s$/u.test(ch)) return ' ';
    if (!/^[\p{L}\p{N}]$/u.test(ch)) return ch;
    letters++;
    return letters === 1 ? ch : '_';
  });
  return { pattern: parts.join(' ').replace(/ {3,}/g, '   '), letters };
}

/**
 * The answer a hint and the "answer:" line show: the target span of the
 * shown sentence (what fits the blank), else the headword.
 *
 * @param {{ sentence: { en: { target: string } } | null, word: { headword: string } }} item
 */
export function expectedAnswer(item) {
  const target = item.sentence?.en.target.trim() ?? '';
  return target !== '' ? target : item.word.headword;
}

/** Longest accepted answer the word's list takes (characters, R-04). */
export const ACCEPTED_MAX = 200;

/**
 * Should "Also accept “…” next time?" be offered? Only for a typed answer
 * that did not match (adding it makes it match from then on) and that
 * the word's list can take.
 *
 * @param {Check | null} check
 */
export function canAddAccepted(check) {
  return (
    check !== null &&
    check.match === 'none' &&
    check.typed !== null &&
    checkable(check.typed) &&
    [...check.typed.trim()].length <= ACCEPTED_MAX
  );
}

// ── Spelling (R-58, design §6.8) ────────────────────────────────────────────

/**
 * Apostrophe look-alikes a phone keyboard types for the apostrophe key
 * (smart punctuation: ’ ‘ ʼ ′).
 */
const APOSTROPHE_GLYPHS = /[‘’ʼ′]/gu;

/**
 * A spelling as it is compared letter by letter: Unicode NFC (the same
 * letters in another encoding are the same text), surrounding white space
 * trimmed, a run of white space as one space and typographic apostrophes
 * as `'` (what the keyboard typed for the apostrophe key). Nothing else
 * changes — diacritics, hyphens, apostrophes and spaces are part of a
 * spelling. Case is compared per letter ([`spellingDiff`]), not changed
 * here, so the diff shows the letters as they were typed.
 *
 * @param {string} text
 */
export function spellingText(text) {
  return text.normalize('NFC').replace(APOSTROPHE_GLYPHS, "'").replace(/\s+/gu, ' ').trim();
}

/**
 * One column of a spelling diff:
 * - `same`: the right letter (case does not count);
 * - `wrong`: another letter typed in its place;
 * - `missing`: a letter of the word that was not typed;
 * - `extra`: a typed letter the word does not have.
 *
 * @typedef {'same' | 'wrong' | 'missing' | 'extra'} DiffMark
 * @typedef {{ mark: DiffMark, typed: string | null, correct: string | null }} DiffColumn
 */

/** @param {string} ch */
function letterKey(ch) {
  return ch.toLowerCase();
}

/**
 * The letter-by-letter comparison of what was typed with the correct
 * spelling (design §6.8): a Levenshtein alignment of the two (after
 * [`spellingText`]; letters compared without case) as columns in reading
 * order. Of the alignments with the fewest edits it takes the one that
 * keeps the most letters matched (“dessicated” → one extra s and one
 * missing c, not two wrong letters; a swapped pair → one letter extra and
 * one missing on either side of the letter it jumped); among those,
 * reading from the end, a matching or substituted letter first, then a
 * missing one. Pure and deterministic.
 *
 * @param {string} typed
 * @param {string} correct
 * @returns {DiffColumn[]}
 */
export function spellingDiff(typed, correct) {
  const a = [...spellingText(typed)];
  const b = [...spellingText(correct)];
  const ka = a.map(letterKey);
  const kb = b.map(letterKey);
  const cols = b.length + 1;
  // Cost of an edit, plus one for a substitution: fewest edits first, then
  // the fewest substitutions (= the most letters matched).
  const edit = a.length + b.length + 1;
  /** @param {boolean} same */
  const step = (same) => (same ? 0 : edit + 1);
  // cost[i * cols + j]: between the first i typed and j correct letters.
  const cost = new Float64Array((a.length + 1) * cols);
  for (let i = 0; i <= a.length; i++) cost[i * cols] = i * edit;
  for (let j = 0; j <= b.length; j++) cost[j] = j * edit;
  for (let i = 1; i <= a.length; i++) {
    for (let j = 1; j <= b.length; j++) {
      const diagonal = cost[(i - 1) * cols + j - 1] + step(ka[i - 1] === kb[j - 1]);
      cost[i * cols + j] = Math.min(diagonal, cost[(i - 1) * cols + j] + edit, cost[i * cols + j - 1] + edit);
    }
  }
  /** @type {DiffColumn[]} */
  const out = [];
  let i = a.length;
  let j = b.length;
  while (i > 0 || j > 0) {
    const here = cost[i * cols + j];
    const same = i > 0 && j > 0 && ka[i - 1] === kb[j - 1];
    if (i > 0 && j > 0 && here === cost[(i - 1) * cols + j - 1] + step(same)) {
      out.push({ mark: same ? 'same' : 'wrong', typed: a[i - 1], correct: b[j - 1] });
      i--;
      j--;
    } else if (j > 0 && here === cost[i * cols + j - 1] + edit) {
      out.push({ mark: 'missing', typed: null, correct: b[j - 1] });
      j--;
    } else {
      out.push({ mark: 'extra', typed: a[i - 1], correct: null });
      i--;
    }
  }
  return out.reverse();
}

/**
 * Check a typed spelling (design §6.8): strict — exactly the spelling
 * (only case and the [`spellingText`] form do not count) suggests 2;
 * anything else, a one-letter slip included, suggests 4. No hint and no
 * list of accepted answers: the spelling is the point.
 *
 * @param {string} typed what the owner typed
 * @param {string} correct the word's spelling (its headword)
 * @returns {Check}
 */
export function checkSpelling(typed, correct) {
  const exact = spellingText(typed) !== '' && spellingDiff(typed, correct).every((column) => column.mark === 'same');
  return exact
    ? { match: 'exact', verdict: 'correct', suggested: 2, typed, hintUsed: false }
    : { match: 'none', verdict: 'miss', suggested: 4, typed, hintUsed: false };
}

/**
 * The differences of a spelling diff in words, for a screen reader (the
 * grid of letters is hidden from it): "letter 4: you typed “i”, the word
 * has “e”; letter 6 missing: “n”". Empty when there is none.
 *
 * @param {readonly DiffColumn[]} diff
 */
export function diffSummary(diff) {
  /** @param {string | null} ch */
  const say = (ch) => (ch === ' ' ? 'a space' : `“${ch ?? ''}”`);
  /** @type {string[]} */
  const parts = [];
  let position = 0;
  for (const column of diff) {
    if (column.correct !== null) position++;
    if (column.mark === 'wrong') parts.push(`letter ${position}: you typed ${say(column.typed)}, the word has ${say(column.correct)}`);
    else if (column.mark === 'missing') parts.push(`letter ${position} missing: ${say(column.correct)}`);
    else if (column.mark === 'extra') parts.push(`extra ${say(column.typed)} ${position === 0 ? 'at the start' : `after letter ${position}`}`);
  }
  return parts.join('; ');
}
