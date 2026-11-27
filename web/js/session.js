// @ts-check
/**
 * Calls of the practice API (design §8) and the shapes they return. Every
 * rating carries a client-generated UUID, so a retried post is idempotent.
 */

import { getJson, postJson } from './api.js';

/**
 * How a typed card was answered (design §6.8), sent with its rating.
 *
 * @typedef {{ typedAnswer: string | null, answerMatch: import('./grading.js').AnswerMatch, hintUsed: boolean,
 *             suggestedGrade: number | null }} AnswerDetails
 */

/**
 * @typedef {import('./marked.js').TextParts} TextParts
 * @typedef {{ id: number, en: TextParts, cs: TextParts, origin: string, location: string | null,
 *             source_id: number | null, source_title: string | null }} Sentence
 * @typedef {{ id: number, headword: string, pronunciation: string | null, speak: string | null,
 *             grammar: string | null, czech: string[], meaning: string, forms: { form: string, common: boolean }[],
 *             note: string, hint: string | null, icon: string | null, spelling_note: string | null,
 *             confusables: string[], tag: { kind: string, text: string | null } | null }} Word
 * @typedef {{ id: number, kind: 'understand' | 'produce' | 'spelling', format: 'sentence' | 'word_only' | 'cloze' | 'spelling',
 *             is_new: boolean, is_repeat: boolean, leech_flagged: boolean, last_failed: boolean }} ItemCard
 * @typedef {{ item_id: number, origin: 'due' | 'new' | 'repeat', answered: { review_id: number, raw_grade: number } | null,
 *             card: ItemCard, word: Word, sentence: Sentence | null, other_sentences: Sentence[],
 *             accept: string[] }} Item
 * @typedef {{ id: string, study_day: string, scope_source_id: number | null, block_no: number,
 *             block_size: number, status: 'active' | 'finished' | 'expired', started_at: string,
 *             finished_at: string | null, total: number, answered: number, remaining: number,
 *             can_undo: boolean, items: Item[] }} Session
 * @typedef {{ session: Session | null, resumed: boolean }} StartResult
 * @typedef {{ duplicate: boolean, repeat_item_id: number | null, session: Session }} RateResult
 * @typedef {{ undone_review_id: number, item_id: number, session: Session }} UndoResult
 * @typedef {{ session_id: string, status: string, answered: number, unanswered: number,
 *             ratings: [number, number, number, number, number], new_words: number, repeats: number,
 *             look_again: { word_id: number, headword: string }[] }} BlockSummary
 * @typedef {{ id: number, slug: string, type: string, title: string, reading_position: string | null,
 *             word_count: number }} SourceSummary
 * @typedef {{ per_day: number, introduced_today: number, left_today: number, quota: number, paused: boolean,
 *             backlog: boolean, anyway_today: boolean, due_now: number }} NewAllowance
 * @typedef {{ per_day: number, introduced_today: number, left_today: number, quota: number,
 *             paused: boolean }} TypedAllowance
 * @typedef {{ study_day: string, scope_source_id: number | null, due_now: number, relearning: number,
 *             forecast: { day: string, due: number }[], new_available: number, new_words: NewAllowance,
 *             active_session: { id: string, scope_source_id: number | null, block_no: number, total: number,
 *             remaining: number } | null, finished_blocks: { total: number, today: number },
 *             sources: SourceSummary[], new_waiting?: number, new_left_today?: number,
 *             new_paused_reason?: 'backlog' | null, extra_available?: number, due_tomorrow?: number,
 *             new_typed?: TypedAllowance, typed_waiting?: number, typed_today?: number,
 *             next_block?: { due: number, new: number } }} Today
 * @typedef {'start' | 'continue' | 'extra' | 'more_new'} StartKind
 * @typedef {import('./api.js').ApiResult<any>} AnyResult
 */

/**
 * @param {number | null} scope
 * @returns {Promise<import('./api.js').ApiResult<Today>>}
 */
export function fetchToday(scope) {
  return getJson(scope === null ? '/api/today' : `/api/today?scope=${encodeURIComponent(scope)}`);
}

/**
 * The active block of today, or `null` (204).
 *
 * @returns {Promise<import('./api.js').ApiResult<Session | null>>}
 */
export function fetchCurrent() {
  return getJson('/api/sessions/current');
}

/**
 * A block the Today or Summary screen just started, handed to the practice
 * screen so it does not fetch it again (usability U4: one round trip on a
 * slow connection). Taken once.
 *
 * @type {{ session: Session, finishedBlocks: number } | null}
 */
let started = null;

/**
 * Start (or resume) a block. `extra` practises words already seen, weakest
 * first; `more_new` introduces `count` more new words today.
 *
 * @param {{ kind: StartKind, scope: number | null, addNewAnyway?: boolean, count?: number, finishedBlocks?: number }} request
 * @returns {Promise<import('./api.js').ApiResult<StartResult>>}
 */
export async function startBlock({ kind, scope, addNewAnyway = false, count, finishedBlocks = 0 }) {
  /** @type {Record<string, unknown>} */
  const body = { kind };
  if (scope !== null) body.scope_source_id = scope;
  if (addNewAnyway) body.add_new_anyway = true;
  if (kind === 'more_new' && count !== undefined) body.count = count;
  /** @type {import('./api.js').ApiResult<StartResult>} */
  const answer = await postJson('/api/sessions', body);
  started = answer.ok && answer.data.session ? { session: answer.data.session, finishedBlocks } : null;
  return answer;
}

/**
 * The block just started by `startBlock`, once.
 *
 * @returns {{ session: Session, finishedBlocks: number } | null}
 */
export function takeStarted() {
  const value = started;
  started = null;
  return value;
}

/**
 * Post one rating; a typed card (Produce) also sends how it was answered.
 *
 * @param {string} sessionId
 * @param {{ clientReviewId: string, itemId: number, grade: number, durationMs: number, details?: AnswerDetails | null }} review
 * @returns {Promise<import('./api.js').ApiResult<RateResult>>}
 */
export function postRating(sessionId, { clientReviewId, itemId, grade, durationMs, details = null }) {
  /** @type {Record<string, unknown>} */
  const body = {
    client_review_id: clientReviewId,
    item_id: itemId,
    raw_grade: grade,
    hint_used: details?.hintUsed ?? false,
    duration_ms: Math.max(0, Math.min(Math.round(durationMs), 3_600_000)),
    client_ts: new Date().toISOString(),
  };
  if (details) {
    body.answer_match = details.answerMatch;
    if (details.typedAnswer !== null) body.typed_answer = details.typedAnswer;
    if (details.suggestedGrade !== null) body.suggested_grade = details.suggestedGrade;
  }
  return postJson(`/api/sessions/${encodeURIComponent(sessionId)}/reviews`, body);
}

/**
 * "Also accept … next time": add the owner's typed answer to the word's
 * accepted answers (stored as his own, design §6.8; the same call as in
 * word detail).
 *
 * @param {number} wordId
 * @param {string} value
 * @returns {Promise<AnyResult>}
 */
export function acceptAnswer(wordId, value) {
  return postJson(`/api/words/${encodeURIComponent(wordId)}/items`, { kind: 'accepted', value });
}

/**
 * @param {string} sessionId
 * @returns {Promise<import('./api.js').ApiResult<UndoResult>>}
 */
export function undoRating(sessionId) {
  return postJson(`/api/sessions/${encodeURIComponent(sessionId)}/undo`, {});
}

/**
 * @param {string} sessionId
 * @returns {Promise<import('./api.js').ApiResult<BlockSummary>>}
 */
export function finishBlock(sessionId) {
  return postJson(`/api/sessions/${encodeURIComponent(sessionId)}/finish`, {});
}
