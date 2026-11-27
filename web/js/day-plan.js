// @ts-check
/**
 * What the next block will hold and what is left of the study day, from
 * `GET /api/today` (usability U1, owner feedback F1). Pure rules, no DOM.
 */

/**
 * @typedef {import('./session.js').Today} Today
 * @typedef {{ total: number, due: number, fresh: number }} BlockPreview
 */

/** The block builder puts a new card at every 4th position (design §6.7 step 5). */
const NEW_EVERY = 4;
/** "Learn N more" offers this many when the new-per-day setting is 0. */
export const DEFAULT_MORE_NEW = 5;

/**
 * @param {number} n
 * @param {string} one
 * @param {string} [many]
 */
export function plural(n, one, many) {
  return `${n} ${n === 1 ? one : many ?? `${one}s`}`;
}

/**
 * How many due and new cards a block started now would get: the same
 * interleaving as the server's block builder, from the counts Today has.
 *
 * @param {{ due: number, fresh: number, blockSize: number }} counts
 * @returns {BlockPreview}
 */
export function blockPreview({ due, fresh, blockSize }) {
  let d = Math.max(0, due);
  let n = Math.max(0, fresh);
  const out = { total: 0, due: 0, fresh: 0 };
  while (out.total < blockSize) {
    const slotForNew = out.total % NEW_EVERY === NEW_EVERY - 1;
    if (slotForNew ? n > 0 : d === 0 && n > 0) {
      n--;
      out.fresh++;
    } else if (d > 0) {
      d--;
      out.due++;
    } else {
      break;
    }
    out.total++;
  }
  return out;
}

/**
 * The block a Start would build today.
 *
 * @param {Today} today
 * @param {number} blockSize
 */
export function nextBlock(today, blockSize) {
  return blockPreview({ due: today.due_now, fresh: Math.min(today.new_words.quota, today.new_available), blockSize });
}

/**
 * "10 cards (6 due + 4 new)".
 *
 * @param {BlockPreview} preview
 */
export function blockLabel(preview) {
  const parts = [preview.due > 0 ? `${preview.due} due` : null, preview.fresh > 0 ? `${preview.fresh} new` : null].filter(Boolean);
  return `${plural(preview.total, 'card')} (${parts.join(' + ')})`;
}

/**
 * Words not yet introduced in this scope (older servers: `new_available`).
 *
 * @param {Today} today
 */
export function newWaiting(today) {
  return today.new_waiting ?? today.new_available;
}

/**
 * Are new words held back because reviews piled up?
 *
 * @param {Today} today
 */
export function pausedForBacklog(today) {
  if (today.new_paused_reason !== undefined) return today.new_paused_reason === 'backlog';
  return today.new_words.paused && today.new_words.backlog;
}

/**
 * Cards due on the next study day.
 *
 * @param {Today} today
 */
export function dueTomorrow(today) {
  return today.due_tomorrow ?? today.forecast[0]?.due ?? 0;
}

/**
 * How many new words "Learn N more new words" offers: the new-per-day
 * setting (5 when it is 0), at most as many as are waiting.
 *
 * @param {Today} today
 * @param {number} perDay the new-per-day setting
 */
export function moreNewCount(today, perDay) {
  return Math.max(0, Math.min(newWaiting(today), perDay > 0 ? perDay : DEFAULT_MORE_NEW));
}

/**
 * Is the daily work done: nothing due and no new words left for today?
 *
 * @param {Today} today
 * @param {number} blockSize
 */
export function dayDone(today, blockSize) {
  return today.active_session === null && nextBlock(today, blockSize).total === 0;
}

/**
 * What Today says about Produce cards waiting for their first time (design
 * §17 M8): how many wait, how many today's limit lets in, or that they are
 * held back with new words while reviews pile up. `null` when there is
 * nothing to say (or an older server sent no counts).
 *
 * @param {Today} today
 * @returns {string | null}
 */
export function typedLine(today) {
  const typed = today.new_typed;
  const waiting = today.typed_waiting ?? 0;
  if (!typed) return null;
  if (waiting === 0) {
    return typed.introduced_today > 0 ? `${plural(typed.introduced_today, 'Produce card')} practised for the first time today.` : null;
  }
  const cards = plural(waiting, 'Produce card');
  const verb = waiting === 1 ? 'waits' : 'wait';
  if (typed.paused) return `${cards} ${verb} for the first time — held back until you catch up on reviews.`;
  const comingToday = Math.min(waiting, typed.quota);
  if (comingToday === waiting) return `${cards} ${verb} for the first time — ${waiting === 1 ? 'it comes' : 'all come'} today.`;
  if (comingToday === 0) {
    return `${cards} ${verb} for the first time — today’s ${typed.per_day} are done (limit in Settings); more tomorrow.`;
  }
  return `${cards} ${verb} for the first time — ${comingToday} today (limit ${typed.per_day} a day), the rest on later days.`;
}
