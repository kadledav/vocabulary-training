// @ts-check
/**
 * First-run setup, login, logout, password change and the connector token
 * (design §9.1, §9.2). The session itself is an HttpOnly cookie the page
 * never sees; the page only remembers where to go back to after logging in.
 */

import { getJson, postJson } from './api.js';

/** Shortest new password, in characters (the server checks it too). */
export const MIN_PASSWORD_CHARS = 12;

/**
 * Why `password` cannot be a new password (with its repetition), or null.
 *
 * @param {string} password
 * @param {string} repeated
 * @returns {string | null}
 */
export function newPasswordProblem(password, repeated) {
  if ([...password].length < MIN_PASSWORD_CHARS) {
    return `The new password needs at least ${MIN_PASSWORD_CHARS} characters.`;
  }
  if (password !== repeated) return 'The two new passwords differ.';
  return null;
}

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

/**
 * Show the login screen, coming back to the current screen afterwards. The
 * login replaces the current history entry, so Back after logging in never
 * shows the login form again.
 */
export function goToLogin() {
  if (window.location.hash.startsWith(LOGIN_HASH)) return;
  rememberReturn(window.location.hash || '#/');
  window.location.replace(LOGIN_HASH);
}

/** After a login or setup: back to the screen that asked, replacing the login entry. */
export function leaveLogin() {
  window.location.replace(takeReturn());
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

/**
 * End every session of the owner, on every device (this one included).
 *
 * @returns {Promise<import('./api.js').ApiResult<unknown>>}
 */
export function logoutEverywhere() {
  return postJson('/api/auth/logout-all', {});
}

/**
 * Whether the app still needs to be set up (no password yet).
 *
 * @returns {Promise<import('./api.js').ApiResult<{ required: boolean }>>}
 */
export function setupStatus() {
  return getJson('/api/auth/setup');
}

/**
 * First run: the setup code from the server log and the new password.
 *
 * @param {string} code
 * @param {string} password
 * @returns {Promise<import('./api.js').ApiResult<{ status: string }>>}
 */
export function setUp(code, password) {
  return postJson('/api/auth/setup', { code, password });
}

/**
 * @param {string} currentPassword
 * @param {string} newPassword
 * @returns {Promise<import('./api.js').ApiResult<{ status: string }>>}
 */
export function changePassword(currentPassword, newPassword) {
  return postJson('/api/auth/password', { current_password: currentPassword, new_password: newPassword });
}

/** @returns {Promise<import('./api.js').ApiResult<{ exists: boolean, created_at: string | null }>>} */
export function connectorTokenStatus() {
  return getJson('/api/auth/connector-token');
}

/**
 * A new connector token (shown once; replaces the old one). Asks for the
 * current password again.
 *
 * @param {string} currentPassword
 * @returns {Promise<import('./api.js').ApiResult<{ token: string }>>}
 */
export function createConnectorToken(currentPassword) {
  return postJson('/api/auth/connector-token', { current_password: currentPassword });
}
