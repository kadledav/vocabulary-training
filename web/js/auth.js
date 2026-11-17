// @ts-check
/**
 * Login and logout (design §9.1). The session itself is an HttpOnly cookie
 * the page never sees; the page only remembers where to go back to after
 * logging in.
 */

import { postJson } from './api.js';

const RETURN_KEY = 'vocab.after-login';
const LOGIN_HASH = '#/login';

/**
 * Remember the screen the owner was on, so the login can return there.
 *
 * @param {string} hash e.g. `#/words/12`
 */
export function rememberReturn(hash) {
  if (!hash || hash.startsWith(LOGIN_HASH)) return;
  try {
    sessionStorage.setItem(RETURN_KEY, hash);
  } catch {
    // Storage unavailable (private mode): the login returns to Today.
  }
}

/**
 * The remembered screen (once), or Today.
 *
 * @returns {string}
 */
export function takeReturn() {
  let hash = null;
  try {
    hash = sessionStorage.getItem(RETURN_KEY);
    sessionStorage.removeItem(RETURN_KEY);
  } catch {
    hash = null;
  }
  return hash && hash.startsWith('#/') && !hash.startsWith(LOGIN_HASH) ? hash : '#/';
}

/** Show the login screen, coming back to the current screen afterwards. */
export function goToLogin() {
  if (window.location.hash.startsWith(LOGIN_HASH)) return;
  rememberReturn(window.location.hash || '#/');
  window.location.hash = LOGIN_HASH;
}

/**
 * @param {string} password
 * @returns {Promise<import('./api.js').ApiResult<{ status: string }>>}
 */
export function login(password) {
  return postJson('/api/auth/login', { password });
}

/** @returns {Promise<import('./api.js').ApiResult<{ status: string }>>} */
export function logout() {
  return postJson('/api/auth/logout', {});
}
