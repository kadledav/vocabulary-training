// @ts-check
/**
 * Audio in M4: browser speech synthesis only (R-41; design §7.4). It is the
 * fallback path of the later neural-TTS clips: started only by a tap or a
 * key, English voice preferring en-US (en-GB when the UK voice is chosen).
 * Without the Web Speech API the play buttons say so and do nothing.
 *
 * Only voices on this device (`localService`) are ever used: a network
 * voice would send book sentences to a third party (R-24). Without an
 * on-device English voice nothing is spoken and the caller says so. For
 * iOS (usability U5): `speak()` runs directly in the tap handler, `cancel()`
 * only when something is speaking (then the new utterance follows on the
 * next tick), the voice comes from the list as of the last `voiceschanged`,
 * and `lang` is always set.
 */

/**
 * @typedef {{ lang: string, name: string, localService?: boolean, default?: boolean }} VoiceLike
 * @typedef {'us' | 'uk'} VoicePreference
 * @typedef {'1' | '0.75'} SpeedSetting
 */

/** @param {string} lang */
function normaliseLang(lang) {
  return lang.replace('_', '-').toLowerCase();
}

/**
 * The language tag asked for.
 *
 * @param {VoicePreference} preference
 */
export function speechLang(preference) {
  return preference === 'uk' ? 'en-GB' : 'en-US';
}

/**
 * Choose an on-device voice: the preferred English variant, then any
 * English one. `null` = no on-device English voice (nothing is spoken).
 *
 * @template {VoiceLike} V
 * @param {readonly V[]} voices
 * @param {VoicePreference} preference
 * @returns {V | null}
 */
export function pickVoice(voices, preference) {
  const wanted = speechLang(preference).toLowerCase();
  /** @param {V} v */
  const score = (v) => {
    if (v.localService !== true) return -1;
    const lang = normaliseLang(v.lang);
    if (lang === wanted) return 4;
    if (lang.startsWith('en-') || lang === 'en') return 2;
    return -1;
  };
  let best = null;
  let bestScore = -1;
  for (const voice of voices) {
    const s = score(voice);
    if (s > bestScore) {
      best = voice;
      bestScore = s;
    }
  }
  return best;
}

/**
 * Speech rate for a speed setting.
 *
 * @param {SpeedSetting} speed
 */
export function speechRate(speed) {
  return speed === '0.75' ? 0.75 : 1;
}

/** Can this browser speak? */
export function speechAvailable() {
  return typeof window !== 'undefined' && 'speechSynthesis' in window && typeof SpeechSynthesisUtterance === 'function';
}

/** @type {SpeechSynthesisVoice[]} */
let voices = [];
let watching = false;

/** Keep the voice list as of the last `voiceschanged` (never a stale voice object). */
function watchVoices() {
  if (watching || !speechAvailable()) return;
  watching = true;
  const synth = window.speechSynthesis;
  const update = () => {
    voices = synth.getVoices();
  };
  update();
  if (typeof synth.addEventListener === 'function') synth.addEventListener('voiceschanged', update);
}

/** Start loading the voice list early (it arrives asynchronously in some browsers). */
export function prepareSpeech() {
  watchVoices();
}

/**
 * @typedef {'speaking' | 'unavailable' | 'no-voice'} SpeakResult
 * @typedef {{ onStart?: () => void, onEnd?: () => void }} SpeakEvents
 */

/**
 * Speak `text` with an on-device English voice (stops anything that is
 * still speaking). Call it directly from the tap or key handler.
 *
 * @param {string} text
 * @param {{ voice: VoicePreference, speed: SpeedSetting }} options
 * @param {SpeakEvents} [events]
 * @returns {SpeakResult}
 */
export function speak(text, options, events = {}) {
  if (!speechAvailable() || text.trim() === '') return 'unavailable';
  watchVoices();
  const synth = window.speechSynthesis;
  if (voices.length === 0) voices = synth.getVoices();
  const voice = pickVoice(voices, options.voice);
  if (!voice) return 'no-voice';
  const utterance = new SpeechSynthesisUtterance(text);
  utterance.lang = voice.lang || speechLang(options.voice);
  utterance.voice = voice;
  utterance.rate = speechRate(options.speed);
  let ended = false;
  const end = () => {
    if (ended) return;
    ended = true;
    events.onEnd?.();
  };
  utterance.onend = end;
  utterance.onerror = end;
  events.onStart?.();
  // A safety net for engines that never report the end.
  setTimeout(() => end(), Math.min(30_000, 2_000 + text.length * 150));
  if (synth.speaking || synth.pending) {
    synth.cancel();
    setTimeout(() => synth.speak(utterance), 0);
  } else {
    synth.speak(utterance);
  }
  return 'speaking';
}

/** Stop speaking (leaving the practice screen). */
export function stopSpeaking() {
  if (speechAvailable() && (window.speechSynthesis.speaking || window.speechSynthesis.pending)) window.speechSynthesis.cancel();
}
