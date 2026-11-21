// @ts-check
/**
 * The 1–5 self-rating scale (R-60, R-62; design §7.2; research §10). Pure
 * data and rules, no DOM: what each button says, how long the confirmation
 * toast stays, when the legend shows. 1 = best (Czech school grades), so
 * every safeguard against the "1 = fail" habit of other apps lives here.
 */

/**
 * @typedef {1 | 2 | 3 | 4 | 5} Grade
 * @typedef {{ grade: Grade, label: string, definition: string, short: string, pass: boolean }} Rating
 */

/** @type {readonly Rating[]} */
export const RATINGS = Object.freeze([
  { grade: 1, label: 'Instantly', definition: 'sure, no effort', short: 'knew it instantly', pass: true },
  { grade: 2, label: 'Knew it', definition: 'after a moment', short: 'after a moment', pass: true },
  { grade: 3, label: 'Shaky', definition: 'effort, doubt, or only worked out from context', short: 'with effort', pass: true },
  { grade: 4, label: 'Vague', definition: 'half-knew it, or wrong sense', short: 'half-knew it', pass: false },
  { grade: 5, label: 'No idea', definition: "didn't know it at all", short: 'no idea', pass: false },
]);

/** Reveal/Next ignore input this long after each change (double-tap guard). */
export const INPUT_GUARD_MS = 250;
/**
 * Ratings are ignored this long after the answer is revealed: a slow
 * double tap on "Show translation" must not rate the card that appears
 * under the finger (frontend review, bug 4).
 */
export const RATING_GUARD_MS = 500;
/** A rating tap this soon after the reveal tap… */
export const SAME_SPOT_MS = 600;
/** …and this close to it (CSS pixels) is the second half of a double tap. */
export const SAME_SPOT_PX = 24;
/** How long the "Rated … — Undo" toast stays. */
export const TOAST_MS = 4000;
/** …and after a suspected slip (a reflex "1" right after a failed card). */
export const SLIP_TOAST_MS = 8000;
/**
 * A "1" faster than this after reveal on a card last rated 4–5 is a
 * suspected slip. (Was 600 ms; ratings now wait 500 ms after the reveal,
 * so the window reaches past the guard to still catch a reflex.)
 */
export const SLIP_WINDOW_MS = 1000;
/** The legend stays visible during this many first finished blocks (legend "auto"). */
export const LEGEND_BLOCKS = 3;

/**
 * @param {number} grade
 * @returns {grade is Grade}
 */
export function isGrade(grade) {
  return Number.isInteger(grade) && grade >= 1 && grade <= 5;
}

/**
 * @param {Grade} grade
 * @returns {Rating}
 */
export function rating(grade) {
  const found = RATINGS[grade - 1];
  if (!found || found.grade !== grade) throw new Error(`no rating ${grade}`);
  return found;
}

/**
 * The rating row: pass grades (filled buttons) and fail grades (outlined),
 * with the divider between them.
 *
 * @returns {{ pass: Rating[], fail: Rating[] }}
 */
export function ratingGroups() {
  return { pass: RATINGS.filter((r) => r.pass), fail: RATINGS.filter((r) => !r.pass) };
}

/**
 * Accessible name of a rating button: never a bare digit.
 *
 * @param {Grade} grade
 */
export function ratingAriaLabel(grade) {
  return `Rate ${grade}, ${rating(grade).label}`;
}

/**
 * Text of the confirmation toast.
 *
 * @param {Grade} grade
 */
export function toastText(grade) {
  return `Rated ${grade} · ${rating(grade).label}`;
}

/**
 * May a rating be accepted this long after the reveal?
 *
 * @param {number} msSinceReveal
 */
export function guardOpen(msSinceReveal) {
  return msSinceReveal >= INPUT_GUARD_MS;
}

/**
 * May a rating be accepted this long after the reveal?
 *
 * @param {number} msSinceReveal
 */
export function ratingGuardOpen(msSinceReveal) {
  return msSinceReveal >= RATING_GUARD_MS;
}

/**
 * Is a rating tap the second tap of a double tap on the reveal button: soon
 * after the reveal tap and where it landed?
 *
 * @param {{ x: number, y: number, t: number } | null} revealTap
 * @param {{ x: number, y: number, t: number }} tap
 */
export function sameSpotTap(revealTap, tap) {
  if (!revealTap) return false;
  const dt = tap.t - revealTap.t;
  return dt >= 0 && dt < SAME_SPOT_MS && Math.hypot(tap.x - revealTap.x, tap.y - revealTap.y) <= SAME_SPOT_PX;
}

/**
 * How long the toast stays and whether it pulses: a "1" given within
 * a second of reveal on a card whose last rating was 4–5 is probably a
 * reflex from an app where 1 means "fail".
 *
 * @param {{ grade: Grade, msSinceReveal: number, lastFailed: boolean }} input
 * @returns {{ ms: number, pulse: boolean }}
 */
export function toastPlan({ grade, msSinceReveal, lastFailed }) {
  const slip = grade === 1 && lastFailed && msSinceReveal < SLIP_WINDOW_MS;
  return slip ? { ms: SLIP_TOAST_MS, pulse: true } : { ms: TOAST_MS, pulse: false };
}

/**
 * Is the legend shown permanently above the rating row?
 *
 * @param {'auto' | 'always' | 'off'} mode the `legend` setting
 * @param {number} finishedBlocks blocks finished so far
 */
export function legendVisible(mode, finishedBlocks) {
  if (mode === 'always') return true;
  if (mode === 'off') return false;
  return finishedBlocks < LEGEND_BLOCKS;
}
