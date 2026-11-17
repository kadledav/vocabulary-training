// @ts-check
/**
 * Calls of the practice API (design §8) and the shapes they return. Every
 * rating carries a client-generated UUID, so a retried post is idempotent.
 */

import { getJson, postJson } from './api.js';

/**
 * @typedef {import('./marked.js').TextParts} TextParts
 * @typedef {{ id: number, en: TextParts, cs: TextParts, origin: string, location: string | null,
 *             source_id: number | null, source_title: string | null }} Sentence
 * @typedef {{ id: number, headword: string, pronunciation: string | null, speak: string | null,
 *             grammar: string | null, czech: string[], meaning: string, forms: { form: string, common: boolean }[],
 *             note: string, hint: string | null, icon: string | null, spelling_note: string | null,
 *             confusables: string[], tag: { kind: string, text: string | null } | null }} Word
 * @typedef {{ id: number, kind: string, format: 'sentence' | 'word_only' | 'cloze' | 'spelling',
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
 * @typedef {{ study_day: string, scope_source_id: number | null, due_now: number, relearning: number,
 *             forecast: { day: string, due: number }[], new_available: number, new_words: NewAllowance,
 *             active_session: { id: string, scope_source_id: number | null, block_no: number, total: number,
 *             remaining: number } | null, finished_blocks: { total: number, today: number },
 *             sources: SourceSummary[] }} Today
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
 * @param {{ kind: 'start' | 'continue', scope: number | null, addNewAnyway?: boolean }} request
 * @returns {Promise<import('./api.js').ApiResult<StartResult>>}
 */
export function startBlock({ kind, scope, addNewAnyway = false }) {
  /** @type {Record<string, unknown>} */
  const body = { kind };
  if (scope !== null) body.scope_source_id = scope;
  if (addNewAnyway) body.add_new_anyway = true;
  return postJson('/api/sessions', body);
}

/**
 * @param {string} sessionId
 * @param {{ clientReviewId: string, itemId: number, grade: number, durationMs: number }} review
 * @returns {Promise<import('./api.js').ApiResult<RateResult>>}
 */
export function postRating(sessionId, { clientReviewId, itemId, grade, durationMs }) {
  return postJson(`/api/sessions/${encodeURIComponent(sessionId)}/reviews`, {
    client_review_id: clientReviewId,
    item_id: itemId,
    raw_grade: grade,
    hint_used: false,
    duration_ms: Math.max(0, Math.min(Math.round(durationMs), 3_600_000)),
    client_ts: new Date().toISOString(),
  });
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
