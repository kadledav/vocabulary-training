// @ts-check
/**
 * JSON calls to the app's API (design §8). Every error, including network
 * failures and non-JSON answers, comes back as `{ ok: false, message }`.
 * A busy server (503, or 429 while another import runs) is asked again
 * politely after its `Retry-After`, a few times, while the app says "The
 * server is busy, trying again…". A refused request was not processed, so
 * asking again is safe (ratings and imports are idempotent anyway).
 */

/**
 * @template T
 * @typedef {{ ok: true, status: number, data: T } | { ok: false, status: number, code: string, message: string }} ApiResult
 */

/** A request that has not finished after this long is given up (`code: 'timeout'`). */
export const REQUEST_TIMEOUT_MS = 15_000;

/** Busy answers are retried this many times… */
const BUSY_RETRIES = 3;
/** …waiting what the server asks, but at least 1 s and at most this long. */
const MAX_BUSY_WAIT_MS = 10_000;

/** @type {() => void} */
let unauthenticated = () => {};
/** @type {(busy: boolean) => void} */
let busyChanged = () => {};
let busyCalls = 0;

/**
 * Called with true while some request waits for a busy server, false when
 * none does any more.
 *
 * @param {(busy: boolean) => void} handler
 */
export function onBusy(handler) {
  busyChanged = handler;
}

/**
 * Is this refusal a busy server worth asking again shortly? Not a login
 * lock-out (429 `locked`), not a request that ran too long (503 `timeout`:
 * it was processed), not a wait longer than a few seconds.
 *
 * @param {number} status
 * @param {string} code
 * @param {string | null} retryAfter
 */
export function busyAnswer(status, code, retryAfter) {
  if (status !== 503 && status !== 429) return false;
  if (code === 'locked' || code === 'timeout') return false;
  const seconds = retryAfter === null ? 0 : Number(retryAfter);
  return !(Number.isFinite(seconds) && seconds * 1000 > MAX_BUSY_WAIT_MS);
}

/**
 * How long to wait before asking a busy server again.
 *
 * @param {string | null} header `Retry-After` (seconds, or an HTTP date)
 * @param {number} attempt 0 for the first retry
 */
export function busyWaitMs(header, attempt) {
  let ms = (attempt + 1) * 2000;
  if (header) {
    const seconds = Number(header);
    if (Number.isFinite(seconds)) ms = seconds * 1000;
    else {
      const date = Date.parse(header);
      if (!Number.isNaN(date)) ms = date - Date.now();
    }
  }
  return Math.min(MAX_BUSY_WAIT_MS, Math.max(1000, ms));
}

/**
 * Called whenever the server answers 401 because there is no live session
 * (not for a wrong password at login): the app then shows the login screen.
 *
 * @param {() => void} handler
 */
export function onUnauthenticated(handler) {
  unauthenticated = handler;
}

/**
 * @template T
 * @param {string} url
 * @param {RequestInit} [init]
 * @param {number} [timeoutMs]
 * @returns {Promise<ApiResult<T>>}
 */
async function call(url, init, timeoutMs = REQUEST_TIMEOUT_MS) {
  let waiting = false;
  try {
    for (let attempt = 0; ; attempt++) {
      const result = await callOnce(url, init, timeoutMs);
      if (!result.busy || attempt >= BUSY_RETRIES) return result.result;
      if (!waiting) {
        waiting = true;
        if (busyCalls++ === 0) busyChanged(true);
      }
      const wait = busyWaitMs(result.retryAfter, attempt);
      await new Promise((resolve) => setTimeout(() => resolve(undefined), wait));
    }
  } finally {
    if (waiting && --busyCalls === 0) busyChanged(false);
  }
}

/**
 * @template T
 * @param {string} url
 * @param {RequestInit | undefined} init
 * @param {number} timeoutMs
 * @returns {Promise<{ result: ApiResult<T>, busy: boolean, retryAfter: string | null }>}
 */
async function callOnce(url, init, timeoutMs) {
  /** @param {ApiResult<T>} result */
  const done = (result) => ({ result, busy: false, retryAfter: null });
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  let response;
  /** @type {any} */
  let body = null;
  try {
    try {
      response = await fetch(url, { credentials: 'same-origin', ...init, signal: controller.signal });
    } catch {
      return done(
        controller.signal.aborted
          ? { ok: false, status: 0, code: 'timeout', message: 'The server did not answer in time.' }
          : { ok: false, status: 0, code: 'network', message: 'The server cannot be reached.' },
      );
    }
    try {
      body = await response.json();
    } catch {
      if (controller.signal.aborted) {
        return done({ ok: false, status: 0, code: 'timeout', message: 'The server did not answer in time.' });
      }
      body = null;
    }
  } finally {
    clearTimeout(timer);
  }
  if (response.ok) {
    // Only "204 No Content" may come without a body. A success answer that
    // cannot be read (cut off when the server went away, or not JSON) is a
    // failure: callers never see `data: null` they did not ask for.
    if (body === null && response.status !== 204) {
      return done({ ok: false, status: 0, code: 'network', message: 'The server’s answer was cut off.' });
    }
    return done({ ok: true, status: response.status, data: /** @type {T} */ (body) });
  }
  const error = body && typeof body === 'object' && body.error ? body.error : null;
  // No live session any more (`session_ended`: too many wrong current
  // passwords in Settings ended it): show the login screen.
  if (response.status === 401 && error && (error.code === 'unauthenticated' || error.code === 'session_ended')) unauthenticated();
  const code = error && typeof error.code === 'string' ? error.code : 'http';
  const retryAfter = response.headers.get('Retry-After');
  const busy = busyAnswer(response.status, code, retryAfter);
  /** @type {ApiResult<T>} */
  const result = {
    ok: false,
    status: response.status,
    code,
    message: busy
      ? 'The server is busy. Try again in a moment.'
      : error && typeof error.message === 'string'
        ? error.message
        : `The server answered ${response.status}.`,
  };
  return { result, busy, retryAfter };
}

/**
 * @template T
 * @param {string} url
 * @returns {Promise<ApiResult<T>>}
 */
export function getJson(url) {
  return call(url, { headers: { Accept: 'application/json' } });
}

/**
 * @template T
 * @param {string} url
 * @param {unknown} body
 * @param {{ timeoutMs?: number }} [options] a longer timeout for slow work (import)
 * @returns {Promise<ApiResult<T>>}
 */
export function postJson(url, body, { timeoutMs = REQUEST_TIMEOUT_MS } = {}) {
  return call(
    url,
    {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Accept: 'application/json' },
      body: JSON.stringify(body),
    },
    timeoutMs,
  );
}

/**
 * @template T
 * @param {string} url
 * @param {unknown} body
 * @returns {Promise<ApiResult<T>>}
 */
export function patchJson(url, body) {
  return call(url, {
    method: 'PATCH',
    headers: { 'Content-Type': 'application/json', Accept: 'application/json' },
    body: JSON.stringify(body),
  });
}

/**
 * `DELETE` with an empty JSON body (the server's CSRF guard asks every
 * state-changing request for a JSON content type, design §9.3).
 *
 * @template T
 * @param {string} url
 * @returns {Promise<ApiResult<T>>}
 */
export function deleteJson(url) {
  return call(url, {
    method: 'DELETE',
    headers: { 'Content-Type': 'application/json', Accept: 'application/json' },
    body: '{}',
  });
}
