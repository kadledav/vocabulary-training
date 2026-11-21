// @ts-check
/** Recorded audio: which clip plays, and what Settings says (R-40, R-41). */

import assert from 'node:assert/strict';
import { test } from 'node:test';

import { clipFor } from '../js/clips.js';
import { audioStatusText } from '../js/screens/settings.js';

const ready = (/** @type {string} */ hash) => ({
  url: `/api/audio/clips/${hash}.mp3`,
  ready: true,
  other_voice: false,
  approximate: false,
});
const notReady = { url: null, ready: false, other_voice: false, approximate: false };

test('R-40 a ready clip plays from the server', () => {
  /** @type {import('../js/clips.js').WordClips} */
  const info = { enabled: true, word: ready('a'.repeat(32)), sentences: [{ id: 7, ...ready('b'.repeat(32)) }] };
  assert.equal(clipFor(info, 'word')?.url, `/api/audio/clips/${'a'.repeat(32)}.mp3`);
  assert.equal(clipFor(info, 'sentence', 7)?.url, `/api/audio/clips/${'b'.repeat(32)}.mp3`);
});

test('R-41 no clip (not ready, unknown sentence, audio off, foreign URL) means browser speech', () => {
  /** @type {import('../js/clips.js').WordClips} */
  const info = { enabled: true, word: notReady, sentences: [{ id: 7, ...notReady }] };
  assert.equal(clipFor(info, 'word'), null);
  assert.equal(clipFor(info, 'sentence', 7), null);
  assert.equal(clipFor(info, 'sentence', 8), null);
  assert.equal(clipFor({ enabled: false }, 'word'), null);
  assert.equal(clipFor(null, 'word'), null);
  /** @type {import('../js/clips.js').WordClips} */
  const foreign = { enabled: true, word: { ...ready('a'.repeat(32)), url: 'https://example.com/x.mp3' }, sentences: [] };
  assert.equal(clipFor(foreign, 'word'), null);
});

test('R-40 Settings says how far the audio is and what is approximate', () => {
  assert.match(audioStatusText({ enabled: false }), /browser’s own speech/);
  assert.match(
    audioStatusText({ enabled: true, connected: false, voice: 'us', ready: 0, pending: 0, failed: 0, approximate: 0 }),
    /not answered yet/,
  );
  const text = audioStatusText({ enabled: true, connected: true, voice: 'us', ready: 40, pending: 12, failed: 1, approximate: 2 });
  assert.match(text, /40 clips ready, 12 being made, 1 failed\./);
  assert.match(text, /2 have an approximate pronunciation/);
  assert.match(audioStatusText(null), /could not be loaded/);
});
