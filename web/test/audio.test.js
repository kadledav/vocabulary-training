// @ts-check
/** Recorded audio: which clip plays, and what Settings says (R-40, R-41). */

import assert from 'node:assert/strict';
import { test } from 'node:test';

import { clipFor, promptClipFor } from '../js/clips.js';
import { promptSpeech } from '../js/practice/model.js';
import { audioStatusText } from '../js/screens/settings.js';

const ready = (/** @type {string} */ hash) => ({
  url: `/api/audio/clips/${hash}.mp3`,
  ready: true,
  other_voice: false,
  approximate: false,
  whole_words: true,
});
const notReady = { url: null, ready: false, other_voice: false, approximate: false, whole_words: false };

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
  const counts = { enabled: true, connected: true, voice: 'us', ready: 3, pending: 2, failed: 0, approximate: 0 };
  assert.doesNotMatch(audioStatusText({ ...counts, storage: 'ok' }), /No new clips/);
  assert.match(
    audioStatusText({ ...counts, storage: 'cache_full', cache_bytes: 2 ** 31, cache_limit_bytes: 2 ** 31 }),
    /cache is full \(2\.0 GiB of 2\.0 GiB\)/,
  );
  assert.match(audioStatusText({ ...counts, storage: 'disk_low' }), /disk is nearly full/);
});

test('R-58 a Spelling prompt plays only a clip known to say no word letter by letter', () => {
  const clip = ready('a'.repeat(32));
  /** @param {import('../js/clips.js').ClipRef} word */
  const info = (word) => /** @type {import('../js/clips.js').WordClips} */ ({ enabled: true, word, sentences: [] });
  assert.equal(promptClipFor(info(clip))?.url, clip.url);
  // Spelled out, or not known (a clip of an older version): never a prompt...
  assert.equal(promptClipFor(info({ ...clip, whole_words: false })), null);
  assert.equal(promptClipFor(info(/** @type {any} */ ({ ...clip, whole_words: undefined }))), null);
  // ...but still the word's clip everywhere else, approximate or not.
  assert.equal(clipFor(info({ ...clip, whole_words: false, approximate: true }), 'word')?.url, clip.url);
  assert.equal(promptClipFor(info(notReady)), null);
  assert.equal(promptClipFor({ enabled: false }), null);
  assert.equal(promptClipFor(null), null);
});

test('R-58 the speech fallback of a Spelling prompt does not spell either', () => {
  /** @param {string} headword @param {string | null} [speak] */
  const word = (headword, speak = null) => /** @type {any} */ ({ headword, speak });
  assert.equal(promptSpeech(word('Zibberate')), 'zibberate');
  // Capitals are read as letters by speech engines.
  assert.equal(promptSpeech(word('ZIBB')), 'zibb');
  assert.equal(promptSpeech(word('glimmer', 'GLIM-er')), 'glim-er');
  assert.equal(promptSpeech(word('pore over')), 'pore over');
  assert.equal(promptSpeech(word('a cappella')), 'a cappella');
  // Single letters say the answer: no speech.
  for (const text of ['z x c', 'Z-X-C', 'z.x.c.', 'Z X']) assert.equal(promptSpeech(word('zxc', text)), null, text);
  assert.equal(promptSpeech(word('\u200b')), null);
});
