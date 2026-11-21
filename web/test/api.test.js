// @ts-check
/**
 * A success answer that cannot be read (the server went away mid-answer)
 * is a failure, never `data: null` (gate run defect: `today.sources` of null).
 */

import assert from 'node:assert/strict';
import { afterEach, test } from 'node:test';

import { getJson } from '../js/api.js';

const realFetch = globalThis.fetch;
afterEach(() => {
  globalThis.fetch = realFetch;
});

/** @param {() => Response} answer */
function serve(answer) {
  globalThis.fetch = async () => answer();
}

test('R-74 a cut-off 200 answer is a failure, not data: null', async () => {
  serve(() => new Response('', { status: 200, headers: { 'content-type': 'application/json' } }));
  const empty = await getJson('/api/today');
  assert.equal(empty.ok, false);
  serve(() => new Response('{"study_day":"2026-11-', { status: 200 }));
  const truncated = await getJson('/api/today');
  assert.equal(truncated.ok, false);
});

test('R-74 204 No Content is the one success without a body', async () => {
  serve(() => new Response(null, { status: 204 }));
  const none = await getJson('/api/sessions/current');
  assert.deepEqual(none, { ok: true, status: 204, data: null });
  serve(() => new Response('{"a":1}', { status: 200 }));
  const some = await getJson('/api/today');
  assert.deepEqual(some, { ok: true, status: 200, data: { a: 1 } });
});

test('R-74 a network failure is reported as one', async () => {
  globalThis.fetch = async () => {
    throw new TypeError('Load failed');
  };
  const lost = await getJson('/api/today');
  assert.equal(lost.ok, false);
  assert.equal(lost.ok ? '' : lost.code, 'network');
});
