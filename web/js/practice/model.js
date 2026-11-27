// @ts-check
/**
 * Pure helpers of the practice screen: which item is next, progress, and
 * the mechanical bits of presenting an item. No language logic (thin app):
 * text is shown as the pack wrote it, the server already split the marked
 * sentences into pre/target/post.
 */

/**
 * @typedef {import('../session.js').Session} Session
 * @typedef {import('../session.js').Item} Item
 * @typedef {import('../session.js').Sentence} Sentence
 * @typedef {import('../session.js').Word} Word
 * @typedef {import('../marked.js').TextParts} TextParts
 */

/**
 * The next item to answer (queue order), or `null` when all are rated.
 *
 * @param {Session} session
 * @returns {Item | null}
 */
export function currentItem(session) {
  return session.items.find((item) => item.answered === null) ?? null;
}

/**
 * "7 / 20": the position of the current item and the block length (which
 * grows when a 4/5 queues a repeat).
 *
 * @param {Session} session
 * @returns {{ position: number, total: number, answered: number }}
 */
export function progress(session) {
  const total = session.items.length;
  const answered = session.items.filter((item) => item.answered !== null).length;
  return { position: Math.min(answered + 1, total), total, answered };
}

/**
 * A marked sentence as plain text (for speech).
 *
 * @param {TextParts} parts
 */
export function plainText(parts) {
  return `${parts.pre}${parts.target}${parts.post}`;
}

/**
 * What the word's play button speaks: the pack's `speak` text if any.
 *
 * @param {Word} word
 */
export function wordSpeech(word) {
  return word.speak && word.speak.trim() !== '' ? word.speak : word.headword;
}

/**
 * The sentence a play-sentence control reads: the item's sentence, or for a
 * word-only card the example shown on the answer side.
 *
 * @param {Item} item
 * @returns {Sentence | null}
 */
export function exampleSentence(item) {
  return item.sentence ?? item.other_sentences[0] ?? null;
}

/**
 * The muted caption under a sentence: "A Test Novel, Chapter 3".
 *
 * @param {Sentence} sentence
 */
export function caption(sentence) {
  return [sentence.source_title, sentence.location].filter((part) => part && part.trim() !== '').join(', ');
}

/**
 * Is "More" open when the answer is revealed? New words open the full learn
 * view (R-52); otherwise it starts closed.
 *
 * @param {Item} item
 */
export function moreOpenOnReveal(item) {
  return item.card.is_new;
}

/**
 * Is the item a typed card — Produce (the owner types the English word for
 * the blanked sentence, R-56) or Spelling (he types the word he hears,
 * R-58)?
 *
 * @param {Item} item
 */
export function isTyped(item) {
  return item.card.kind === 'produce' || item.card.kind === 'spelling';
}

/**
 * Is the item a Spelling card (R-58): the question is the word's audio,
 * the answer is checked strictly, letter by letter?
 *
 * @param {Item} item
 */
export function isSpelling(item) {
  return item.card.kind === 'spelling';
}

/**
 * Has the typed card a hint (first letter and letter count)? Produce only:
 * on a Spelling card it would give the spelling away.
 *
 * @param {Item} item
 */
export function hasHint(item) {
  return item.card.kind === 'produce';
}

/**
 * Is "More" open when a typed card's answer shows? After a miss or "I
 * don't know" the note opens (research §3); a correct answer moves on. A
 * Spelling card keeps it closed: its letter diff and spelling note are
 * what matters there.
 *
 * @param {Item} item
 * @param {import('../grading.js').AnswerMatch} match
 */
export function moreOpenAfterCheck(item, match) {
  return !isSpelling(item) && hasMore(item) && (match === 'none' || match === 'dont_know');
}

/**
 * After a rating: a fail (4/5) keeps the answer on screen with "More" open
 * until Next (research §3, §9); a pass moves on at once.
 *
 * @param {number} grade
 * @returns {'feedback' | 'next'}
 */
export function afterRating(grade) {
  return grade >= 4 ? 'feedback' : 'next';
}

/**
 * Does "More" have anything to show?
 *
 * @param {Item} item
 */
export function hasMore(item) {
  const w = item.word;
  return Boolean(
    w.note || w.hint || w.grammar || w.forms.length > 0 || item.other_sentences.length > 0 || w.tag?.text,
  );
}
