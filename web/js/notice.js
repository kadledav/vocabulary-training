// @ts-check
/**
 * A message carried from one screen to the next (e.g. practice → Today
 * after the study day rolled over). In memory only; taken once.
 */

/** @type {string | null} */
let pending = null;

/** @param {string} message a complete sentence */
export function setNotice(message) {
  pending = message;
}

/** @returns {string | null} */
export function takeNotice() {
  const message = pending;
  pending = null;
  return message;
}
