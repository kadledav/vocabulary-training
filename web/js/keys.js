// @ts-check
/**
 * Keyboard map of the practice screen (R-70; design §7.3; research §7).
 * Pure: a key press plus the screen's phase in, an action out. Digits are
 * the same numbers as on screen (1 = best) and Space never rates.
 */

/**
 * - `question`: the question side; Space/Enter reveal.
 * - `answer`: revealed, waiting for a 1–5 rating.
 * - `feedback`: rated 4/5; the answer stays until Next.
 * - `complete`: every item is rated; Next shows the summary.
 *
 * @typedef {'question' | 'answer' | 'feedback' | 'complete'} Phase
 */

/**
 * Where the key press happened.
 * - `text`: a text input or textarea (nothing is mapped there);
 * - `rating`: a focused rating button (Enter rates it, Space never does);
 * - `control`: another focused button or link (Enter/Space activate it);
 * - `page`: anywhere else.
 *
 * @typedef {'text' | 'rating' | 'control' | 'page'} KeyTarget
 */

/**
 * @typedef {{ key: string, code?: string, ctrlKey?: boolean, metaKey?: boolean, altKey?: boolean, repeat?: boolean }} KeyPress
 * @typedef {'reveal' | 'next' | 'undo' | 'more' | 'speak-sentence' | 'speak-word' | 'detail' | 'end' | 'help'
 *           | { rate: 1 | 2 | 3 | 4 | 5 }} Action
 * @typedef {{ action: Action | null, preventDefault: boolean }} KeyResult
 */

/** @type {KeyResult} */
const NOTHING = Object.freeze({ action: null, preventDefault: false });

/**
 * @param {Action | null} action
 * @returns {KeyResult}
 */
function handled(action) {
  return { action, preventDefault: true };
}

/**
 * What Space or Enter does in a phase (never a rating).
 *
 * @param {Phase} phase
 * @returns {Action | null}
 */
function advance(phase) {
  if (phase === 'question') return 'reveal';
  if (phase === 'feedback' || phase === 'complete') return 'next';
  return null;
}

/**
 * The rating digit of a key press. The physical key counts too, so 1–5
 * work on layouts that need Shift for digits (Czech QWERTZ: the "2" key
 * gives "ě"). The numeric keypad counts only when it types a digit.
 *
 * @param {KeyPress} press
 * @returns {string | null}
 */
export function pressedDigit(press) {
  if (/^[1-5]$/.test(press.key)) return press.key;
  const code = /^Digit([1-5])$/.exec(press.code ?? '');
  return code ? code[1] : null;
}

/**
 * The letter of a key press: the typed letter when it is a plain ASCII
 * letter, else the physical key (non-Latin layouts).
 *
 * @param {KeyPress} press
 * @returns {string | null}
 */
export function pressedLetter(press) {
  if (/^[a-zA-Z]$/.test(press.key)) return press.key.toLowerCase();
  const code = /^Key([A-Z])$/.exec(press.code ?? '');
  return code && press.key.length === 1 ? code[1].toLowerCase() : null;
}

/**
 * Map a key press to a practice action.
 *
 * @param {KeyPress} press
 * @param {Phase} phase
 * @param {KeyTarget} target
 * @returns {KeyResult}
 */
export function keyAction(press, phase, target) {
  if (press.ctrlKey || press.metaKey || press.altKey) return NOTHING;
  if (target === 'text') return NOTHING;
  const { key } = press;

  if (key === ' ' || key === 'Spacebar') {
    // A focused rating button must not be "clicked" by Space.
    if (target === 'control') return NOTHING;
    if (press.repeat) return handled(null);
    return handled(advance(phase));
  }
  if (key === 'Enter') {
    // Enter on a focused button activates that button (keyboard access).
    if (target === 'control' || target === 'rating') return NOTHING;
    if (press.repeat) return handled(null);
    const action = advance(phase);
    return action ? handled(action) : NOTHING;
  }
  if (press.repeat) return NOTHING;
  if (key === 'Escape') return handled('end');
  if (key === '?') return handled('help');
  const digit = pressedDigit(press);
  if (digit) {
    if (phase !== 'answer') return NOTHING;
    return handled({ rate: /** @type {1 | 2 | 3 | 4 | 5} */ (Number(digit)) });
  }
  switch (pressedLetter(press)) {
    case 'u':
      return handled('undo');
    case 'm':
      return phase === 'answer' || phase === 'feedback' ? handled('more') : NOTHING;
    case 's':
      return handled('speak-sentence');
    case 'r':
      return handled('speak-word');
    case 'e':
      return handled('detail');
    default:
      return NOTHING;
  }
}

/**
 * Classify the element a key press came from.
 *
 * @param {EventTarget | null} target
 * @returns {KeyTarget}
 */
export function keyTarget(target) {
  if (!target || typeof (/** @type {any} */ (target).closest) !== 'function') return 'page';
  const el = /** @type {Element} */ (target);
  if (el.closest('input, textarea, select, [contenteditable=""], [contenteditable="true"]')) return 'text';
  if (el.closest('button[data-grade]')) return 'rating';
  if (el.closest('button, a[href], summary, [role="button"]')) return 'control';
  return 'page';
}
