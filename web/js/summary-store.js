// @ts-check
/**
 * Hands the block summary from the practice screen to the summary screen
 * (`#/summary`). Kept in memory only: after a reload there is no summary
 * to show and the summary screen links back to Today.
 */

/**
 * @typedef {{ summary: import('./session.js').BlockSummary, scope: number | null, blockSize: number }} SummaryEntry
 */

/** @type {SummaryEntry | null} */
let last = null;

/** @param {SummaryEntry} entry */
export function setSummary(entry) {
  last = entry;
}

/** @returns {SummaryEntry | null} */
export function getSummary() {
  return last;
}
