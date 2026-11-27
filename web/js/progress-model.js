// @ts-check
/**
 * What the Progress screen and the leech choices (design §6.5, §7.1
 * screen 9; R-55, R-72) say in plain words, computed without a page: the
 * explanation of each number, why a word is a problem word, which choices
 * its leech screen offers, and the bar heights of the charts. No DOM here,
 * so it is tested with `node --test`. No streaks, scores or goals (R-74).
 */

import { plural } from './words-model.js';

/**
 * @typedef {{ kind: 'lapses', lapses: number } | { kind: 'forgot_when_known' } | { kind: 'acknowledged' }} LeechReason
 * @typedef {{ kind: 'understand' | 'produce' | 'spelling', reason: LeechReason, lapses: number,
 *             stability: number | null, last_review_day: string | null }} LeechCard
 * @typedef {{ id: number, headword: string, czech: string[], usefulness: number, hint: string | null,
 *             confusables: string[], sources: string[], goal: 'active' | 'recognise', flagged: boolean,
 *             cards: LeechCard[] }} LeechEntry
 * @typedef {{ words: number, new: number, learning: number, familiar: number, known: number,
 *             problem: number, suspended: number, can_produce: number }} StatusCounts
 * @typedef {StatusCounts & { id: number, title: string, type: string, reading_position: string | null,
 *             reading_position_exported_at: string | null }} SourceProgress
 * @typedef {{ kind: 'understand' | 'produce' | 'spelling', tested: number, passed: number,
 *             measured: number | null, target: number | null, few: boolean }} KindRetention
 * @typedef {{ day: string, reviews: number, new_words: number }} DayActivity
 * @typedef {{ word_id: number, headword: string, open: number }} ConflictEntry
 * @typedef {{ study_day: string, due_now: number, forecast: { day: string, due: number }[],
 *             due_next_7_days: number, totals: StatusCounts, sources: SourceProgress[],
 *             retention: { from: string, to: string, kinds: KindRetention[] },
 *             activity: { days: DayActivity[], reviews: number, new_words: number },
 *             leeches: LeechEntry[], conflicts: ConflictEntry[] }} Progress
 * @typedef {'leech_keep' | 'leech_recognise' | 'leech_usefulness' | 'leech_suspend'} LeechActionName
 */

/**
 * The statuses in the order the bars and legends show them, with their
 * plain-language meaning.
 *
 * @type {ReadonlyArray<readonly [keyof StatusCounts, string, string]>}
 */
export const STATUS_PARTS = Object.freeze([
  ['known', 'Known', 'remembered for three weeks or more'],
  ['familiar', 'Familiar', 'remembered for one to three weeks'],
  ['learning', 'Learning', 'failed last time, or remembered for less than a week'],
  ['problem', 'Problem', 'keeps slipping (see Problem words)'],
  ['new', 'New', 'not practised yet'],
  ['suspended', 'Suspended', 'not practised: you suspended it'],
]);

/** @param {'understand' | 'produce' | 'spelling'} kind */
export function kindLabel(kind) {
  return { understand: 'Understand', produce: 'Produce', spelling: 'Spelling' }[kind] ?? kind;
}

/**
 * A share as a whole percent ("87%").
 *
 * @param {number} share 0–1
 */
export function percentText(share) {
  return `${Math.round(share * 100)}%`;
}

/**
 * What the retention line of one card kind says.
 *
 * @param {KindRetention} r
 * @returns {string}
 */
export function retentionText(r) {
  const label = kindLabel(r.kind);
  if (r.tested === 0 || r.measured === null || r.target === null) {
    return `${label}: no reviews of earlier-seen cards in these 30 days.`;
  }
  const line = `${label}: you remembered ${r.passed} of ${plural(r.tested, 'card')} (${percentText(r.measured)}); the app aims for ${percentText(r.target)}.`;
  if (r.few) return `${line} Too few reviews yet to compare.`;
  const gap = r.measured - r.target;
  if (Math.abs(gap) < 0.03) return `${line} That is on target.`;
  return gap < 0 ? `${line} That is below the aim.` : `${line} That is above the aim.`;
}

/**
 * Why a card makes its word a problem word.
 *
 * @param {LeechCard} card
 */
export function reasonText(card) {
  const kind = kindLabel(card.kind);
  switch (card.reason.kind) {
    case 'lapses':
      return `${kind}: forgotten on ${plural(card.reason.lapses, 'different day')}.`;
    case 'forgot_when_known':
      return `${kind}: rated 5 (No idea) twice in a row after you had known it for three weeks.`;
    default:
      return `${kind}: you chose to keep practising it; it leaves this list once you remember it for three weeks (four more slips bring it back here sooner).`;
  }
}

/**
 * The leech screen's choices for a word (design §6.5): recognise-only only
 * for an `active` word, lower usefulness only above 1. With usefulness ≤ 4
 * "Suspend" is the suggested choice, otherwise "Keep practising".
 *
 * @param {{ usefulness: number, goal: 'active' | 'recognise' }} word
 * @returns {{ action: LeechActionName, label: string, primary: boolean }[]}
 */
export function leechChoices(word) {
  const suspendFirst = word.usefulness <= 4;
  /** @type {{ action: LeechActionName, label: string, primary: boolean }[]} */
  const choices = [{ action: 'leech_keep', label: 'Keep practising', primary: !suspendFirst }];
  if (word.goal === 'active') choices.push({ action: 'leech_recognise', label: 'Recognise only (no typing)', primary: false });
  if (word.usefulness > 1) choices.push({ action: 'leech_usefulness', label: 'Lower usefulness', primary: false });
  choices.push({ action: 'leech_suspend', label: 'Suspend word', primary: suspendFirst });
  return suspendFirst ? [choices[choices.length - 1], ...choices.slice(0, -1)] : choices;
}

/**
 * The usefulness values below `current`, highest first (the picker of
 * "Lower usefulness").
 *
 * @param {number} current
 */
export function lowerUsefulness(current) {
  /** @type {number[]} */
  const values = [];
  for (let v = current - 1; v >= 1; v--) values.push(v);
  return values;
}

/**
 * What the owner is told after a leech choice.
 *
 * @param {LeechActionName} action
 * @param {number} [usefulness] the new usefulness (`leech_usefulness`)
 */
export function leechDoneText(action, usefulness) {
  switch (action) {
    case 'leech_keep':
      return 'Kept as it is. If it slips on four more days, you will be asked again.';
    case 'leech_recognise':
      return 'Now recognise-only: you will not be asked to type it.';
    case 'leech_usefulness':
      return `Usefulness lowered to ${usefulness}: it comes up a little less often.`;
    default:
      return 'Suspended: it is not practised any more (Unsuspend in the word brings it back).';
  }
}

/**
 * Bar heights for a chart `height` units tall: proportional to the
 * largest value, at least 1 unit for any value above 0 (a day with one
 * review stays visible), 0 for 0.
 *
 * @param {number[]} values
 * @param {number} height
 */
export function barHeights(values, height) {
  const max = Math.max(0, ...values);
  if (max === 0) return values.map(() => 0);
  return values.map((v) => (v <= 0 ? 0 : Math.max(1, Math.round((v / max) * height))));
}

/**
 * The segments of a status bar `width` units wide, in [`STATUS_PARTS`]
 * order: `[key, label, start, width]`, empty parts left out. Widths are
 * rounded so they add up to `width` exactly.
 *
 * @param {StatusCounts} counts
 * @param {number} width
 * @returns {[string, string, number, number][]}
 */
export function statusSegments(counts, width) {
  const total = STATUS_PARTS.reduce((n, [key]) => n + counts[key], 0);
  if (total === 0) return [];
  /** @type {[string, string, number, number][]} */
  const out = [];
  let used = 0;
  let sum = 0;
  for (const [key, label] of STATUS_PARTS) {
    const n = counts[key];
    if (n === 0) continue;
    sum += n;
    const end = Math.round((sum / total) * width);
    out.push([key, label, used, end - used]);
    used = end;
  }
  return out;
}

/**
 * One line with a source's (or all) words by status, for screen readers
 * and as the visible legend: "12 words: 4 known, 3 familiar, …".
 *
 * @param {StatusCounts} counts
 */
export function statusLine(counts) {
  const parts = STATUS_PARTS.filter(([key]) => counts[key] > 0).map(([key, label]) => `${counts[key]} ${label.toLowerCase()}`);
  return `${plural(counts.words, 'word')}${parts.length ? `: ${parts.join(', ')}` : ''}.`;
}

/**
 * "Mon 23" for a study day `YYYY-MM-DD`.
 *
 * @param {string} day
 */
export function shortDay(day) {
  const date = new Date(`${day}T12:00:00Z`);
  if (Number.isNaN(date.getTime())) return day;
  return `${date.toLocaleDateString('en-GB', { weekday: 'short', timeZone: 'UTC' })} ${date.getUTCDate()}`;
}

/**
 * The forecast as one sentence for screen readers.
 *
 * @param {{ day: string, due: number }[]} forecast
 */
export function forecastSummary(forecast) {
  const busy = forecast.filter((f) => f.due > 0).map((f) => `${shortDay(f.day)}: ${f.due}`);
  return busy.length === 0 ? 'Nothing is due in the next 14 days.' : `Reviews due — ${busy.join(', ')}.`;
}

/**
 * The activity strip as one sentence for screen readers.
 *
 * @param {{ days: DayActivity[], reviews: number, new_words: number }} activity
 */
export function activitySummary(activity) {
  const active = activity.days.filter((d) => d.reviews > 0 || d.new_words > 0);
  if (active.length === 0) return 'No practice in the last 30 days.';
  return `Ratings per day — ${active.map((d) => `${shortDay(d.day)}: ${d.reviews}`).join(', ')}.`;
}
