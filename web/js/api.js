// @ts-check
/**
 * JSON calls to the app's API (design §8). Every error, including network
 * failures and non-JSON answers, comes back as `{ ok: false, message }`.
 */

/**
 * @template T
 * @typedef {{ ok: true, status: number, data: T } | { ok: false, status: number, code: string, message: string }} ApiResult
 */

/** A request that has not finished after this long is given up (`code: 'timeout'`). */
export const REQUEST_TIMEOUT_MS = 15_000;

/**
 * @template T
 * @param {string} url
 * @param {RequestInit} [init]
 * @param {number} [timeoutMs]
 * @returns {Promise<ApiResult<T>>}
 */
async function call(url, init, timeoutMs = REQUEST_TIMEOUT_MS) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  let response;
  /** @type {any} */
  let body = null;
  try {
    try {
      response = await fetch(url, { credentials: 'same-origin', ...init, signal: controller.signal });
    } catch {
      return controller.signal.aborted
        ? { ok: false, status: 0, code: 'timeout', message: 'The server did not answer in time.' }
        : { ok: false, status: 0, code: 'network', message: 'The server cannot be reached.' };
    }
    try {
      body = await response.json();
    } catch {
      if (controller.signal.aborted) {
        return { ok: false, status: 0, code: 'timeout', message: 'The server did not answer in time.' };
      }
      body = null;
    }
  } finally {
    clearTimeout(timer);
  }
  if (response.ok) {
    return { ok: true, status: response.status, data: /** @type {T} */ (body) };
  }
  const error = body && typeof body === 'object' && body.error ? body.error : null;
  return {
    ok: false,
    status: response.status,
    code: error && typeof error.code === 'string' ? error.code : 'http',
    message:
      error && typeof error.message === 'string'
        ? error.message
        : `The server answered ${response.status}.`,
  };
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
