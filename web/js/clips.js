// @ts-check
/**
 * Recorded audio (M6, design §7.4, §10.3; R-40–R-42): the server's neural
 * TTS clips, played through a real media element (`<audio>`, so it is
 * audible with the iPhone's silent switch on, unlike speech synthesis).
 *
 * - `clipsFor(wordId)` asks the server once per word which clips are
 *   ready (`GET /api/audio/words/{id}`); answers are cached for the page.
 * - `clipUrl(info, what, sentenceId)` picks the URL to play, or null: then
 *   the caller falls back to the browser's speech (R-41).
 * - `playClip(url, …)` must run directly in the tap or key handler (iOS):
 *   it never awaits before `play()`.
 * - `preloadClips(info)` lets the browser fetch the next card's clips.
 */

import { getJson } from './api.js';
import { h } from './dom.js';

/**
 * @typedef {{ url: string | null, ready: boolean, other_voice: boolean, approximate: boolean }} ClipRef
 * @typedef {{ id: number } & ClipRef} SentenceClip
 * @typedef {{ enabled: false } | { enabled: true, word: ClipRef, sentences: SentenceClip[] }} WordClips
 * @typedef {'playing' | 'failed'} PlayResult
 * @typedef {{ onStart?: () => void, onEnd?: () => void, onFail?: () => void }} PlayEvents
 */

/** @type {Map<number, Promise<WordClips | null>>} */
const cache = new Map();

/** At most this many hidden preloading elements. */
const MAX_PRELOADED = 4;

/**
 * Which clips of a word are ready (cached per page; `null` when the server
 * could not say: the caller uses speech).
 *
 * @param {number} wordId
 * @returns {Promise<WordClips | null>}
 */
export function clipsFor(wordId) {
  let pending = cache.get(wordId);
  if (!pending) {
    pending = getJson(`/api/audio/words/${encodeURIComponent(String(wordId))}`).then((answer) => {
      if (!answer.ok) {
        cache.delete(wordId);
        return null;
      }
      const info = /** @type {WordClips} */ (answer.data);
      // Not everything ready yet: ask again next time (the queue drains).
      if (!info.enabled || !info.word.ready || info.sentences.some((s) => !s.ready)) {
        setTimeout(() => cache.delete(wordId), 30_000);
      }
      return info;
    });
    cache.set(wordId, pending);
  }
  return pending;
}

/** Forget what is cached (the voice setting changed). */
export function forgetClips() {
  cache.clear();
}

/**
 * The clip URL for the word or one of its sentences, or null (not ready,
 * audio off, or unknown): then the browser speaks instead.
 *
 * @param {WordClips | null | undefined} info
 * @param {'word' | 'sentence'} what
 * @param {number | null} [sentenceId]
 * @returns {ClipRef | null}
 */
export function clipFor(info, what, sentenceId = null) {
  if (!info || !info.enabled) return null;
  const clip = what === 'word' ? info.word : info.sentences.find((s) => s.id === sentenceId);
  return clip && clip.ready && typeof clip.url === 'string' && clip.url.startsWith('/api/audio/clips/') ? clip : null;
}

/** @type {HTMLAudioElement | null} */
let player = null;
/** @type {(() => void) | null} */
let stopCurrent = null;

/**
 * Play a clip at `speed` (pitch preserved). Runs `play()` synchronously in
 * the caller's gesture; reports failure through `onFail` (the caller then
 * speaks the text instead).
 *
 * @param {string} url
 * @param {number} speed
 * @param {PlayEvents} [events]
 */
export function playClip(url, speed, events = {}) {
  stopClip();
  if (!player) player = h('audio', { preload: 'auto' });
  const audio = player;
  let finished = false;
  const finish = (/** @type {boolean} */ failed) => {
    if (finished) return;
    finished = true;
    audio.removeEventListener('playing', started);
    audio.removeEventListener('ended', ended);
    audio.removeEventListener('error', errored);
    stopCurrent = null;
    if (failed) events.onFail?.();
    else events.onEnd?.();
  };
  const started = () => events.onStart?.();
  const ended = () => finish(false);
  const errored = () => finish(true);
  audio.addEventListener('playing', started);
  audio.addEventListener('ended', ended);
  audio.addEventListener('error', errored);
  stopCurrent = () => {
    audio.pause();
    finish(false);
  };
  audio.src = url;
  audio.playbackRate = speed;
  audio.defaultPlaybackRate = speed;
  audio.preservesPitch = true;
  const playing = audio.play();
  if (playing && typeof playing.catch === 'function') playing.catch(() => finish(true));
}

/** Stop the clip that is playing, if any. */
export function stopClip() {
  if (stopCurrent) stopCurrent();
}

/** @type {Map<string, HTMLAudioElement>} */
const preloaded = new Map();

/**
 * Let the browser fetch these clips ahead of time (the next card's).
 *
 * @param {WordClips | null | undefined} info
 */
export function preloadClips(info) {
  if (!info || !info.enabled) return;
  const urls = [info.word, ...info.sentences].filter((c) => c.ready && c.url).map((c) => /** @type {string} */ (c.url));
  for (const url of urls.slice(0, MAX_PRELOADED)) {
    if (preloaded.has(url)) continue;
    const element = h('audio', { preload: 'auto' });
    element.src = url;
    preloaded.set(url, element);
    while (preloaded.size > MAX_PRELOADED) {
      const oldest = preloaded.keys().next().value;
      if (oldest === undefined) break;
      const element = preloaded.get(oldest);
      preloaded.delete(oldest);
      // Removing `src` alone keeps the fetched media; load() releases it.
      element?.removeAttribute('src');
      element?.load();
    }
  }
}
