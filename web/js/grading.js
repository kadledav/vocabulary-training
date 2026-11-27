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
