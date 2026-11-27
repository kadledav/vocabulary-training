// @ts-check
/**
 * Mechanical checking of a typed answer (design §6.8, R-56; research §3
 * "Typed answers"). Pure, no DOM, deterministic.
 *
 * The app makes no language judgement (requirements §1): no edit
 * distance, no stemming, no synonyms, no dropping of articles or "to ".
 * An answer is right only when it equals, after normalisation, one of the
 * answers the server supplies for the item (pack `accepted` ∪ headword ∪
 * the target span of the shown sentence ∪ the owner's accepted answers).
 * Everything else — a one-letter typo included — is a no-match with a
 * suggested 4, and the owner, who sees what he typed above the answer,
 * rates it himself. The suggestion never says 1 ("Instantly" is his call)
 * and the owner's 1–5 rating is always the final word.
 */

/**
 * - `exact`: matched an accepted answer;
 * - `none`: typed, no match;
 * - `dont_know`: "I don't know";
 * - `skipped`: "I'll just rate myself" (nothing typed, no suggestion).
 *
 * @typedef {'exact' | 'none' | 'dont_know' | 'skipped'} AnswerMatch
 * @typedef {import('./rating.js').Grade} Grade
 * @typedef {{ match: AnswerMatch, suggested: Grade | null, typed: string | null, hintUsed: boolean }} Check
 */

/** Modifier letters that look like apostrophes, primes or accents. */
const MODIFIER_APOSTROPHES = /[ʹ-ʿˈˊˋ]/gu;

/**
 * The comparison key of an answer: lowercase, diacritics folded (NFKD
 * without combining marks), and only letters and digits kept — spaces,
 * hyphens and dashes, apostrophes, quotes and punctuation all go, so
 * "pot-luck" = "pot luck" = "potluck" and "don’t" = "don't". The modifier
 * letters that keyboards use as apostrophes and primes (ʼ ʻ ʹ ˈ …) are
 * Unicode letters, so they are dropped by name.
 *
 * @param {string} text
 */
export function normalise(text) {
  return text
    .toLowerCase()
    .normalize('NFKD')
    .replace(/\p{M}/gu, '')
    .replace(MODIFIER_APOSTROPHES, '')
    .replace(/[^\p{L}\p{N}]/gu, '');
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
 * Check a typed answer: a match suggests 2 (3 when the hint was used), no
 * match suggests 4.
 *
 * @param {string} typed what the owner typed
 * @param {readonly string[]} accept the item's accepted answers
 * @param {boolean} hintUsed
 * @returns {Check}
 */
export function checkTyped(typed, accept, hintUsed) {
  const key = normalise(typed);
  const match = key !== '' && acceptKeys(accept).has(key);
  return {
    match: match ? 'exact' : 'none',
    suggested: match ? (hintUsed ? 3 : 2) : 4,
    typed,
    hintUsed,
  };
}

/**
 * "I don't know": suggest 5.
 *
 * @param {boolean} hintUsed
 * @returns {Check}
 */
export function dontKnow(hintUsed) {
  return { match: 'dont_know', suggested: 5, typed: null, hintUsed };
}

/**
 * "I'll just rate myself": reveal, no suggestion.
 *
 * @param {boolean} hintUsed
 * @returns {Check}
 */
export function selfRated(hintUsed) {
  return { match: 'skipped', suggested: null, typed: null, hintUsed };
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

/**
 * Should "Also accept “…” next time?" be offered? Only for a typed answer
 * that did not match (adding it makes it match from then on).
 *
 * @param {Check | null} check
 */
export function canAddAccepted(check) {
  return check !== null && check.match === 'none' && check.typed !== null && checkable(check.typed);
}
