// @ts-check
/**
 * JSON calls to the app's API (design §8). Every error, including network
 * failures and non-JSON answers, comes back as `{ ok: false, message }`.
 */

/**
 * @template T
 * @typedef {{ ok: true, status: number, data: T } | { ok: false, status: number, code: string, message: string }} ApiResult
 */

/**
 * @template T
 * @param {string} url
 * @param {RequestInit} [init]
 * @returns {Promise<ApiResult<T>>}
 */
async function call(url, init) {
  let response;
  try {
    response = await fetch(url, { credentials: 'same-origin', ...init });
  } catch {
    return { ok: false, status: 0, code: 'network', message: 'The server cannot be reached.' };
  }
  /** @type {any} */
  let body = null;
  try {
    body = await response.json();
  } catch {
    body = null;
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
 * @returns {Promise<ApiResult<T>>}
 */
export function postJson(url, body) {
  return call(url, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', Accept: 'application/json' },
    body: JSON.stringify(body),
  });
}
