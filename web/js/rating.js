// @ts-check
/**
 * The 1–5 self-rating scale (R-60, R-62; design §7.2; research §10). Pure
 * data and rules, no DOM: what each button says, how long the confirmation
 * toast stays, when the legend shows. 1 = best (Czech school grades), so
 * every safeguard against the "1 = fail" habit of other apps lives here.
 */

/**
 * @typedef {1 | 2 | 3 | 4 | 5} Grade
 * @typedef {{ grade: Grade, label: string, definition: string, pass: boolean }} Rating
 */

/** @type {readonly Rating[]} */
export const RATINGS = Object.freeze([
  { grade: 1, label: 'Instantly', definition: 'sure, no effort', pass: true },
  { grade: 2, label: 'Knew it', definition: 'after a moment', pass: true },
  { grade: 3, label: 'Shaky', definition: 'effort, doubt, or only worked out from context', pass: true },
  { grade: 4, label: 'Vague', definition: 'half-knew it, or wrong sense', pass: false },
  { grade: 5, label: 'No idea', definition: "didn't know it at all", pass: false },
]);

/** Ratings are ignored this long after the answer is revealed (double-tap guard). */
export const INPUT_GUARD_MS = 250;
/** How long the "Rated … — Undo" toast stays. */
export const TOAST_MS = 4000;
/** …and after a suspected slip (a reflex "1" right after a failed card). */
export const SLIP_TOAST_MS = 8000;
/** A "1" faster than this after reveal on a card last rated 4–5 is a suspected slip. */
export const SLIP_WINDOW_MS = 600;
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
 * How long the toast stays and whether it pulses: a "1" given within
 * 600 ms of reveal on a card whose last rating was 4–5 is probably a
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
