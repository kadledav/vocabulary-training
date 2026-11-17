// @ts-check
/**
 * Audio in M4: browser speech synthesis only (R-41; design §7.4). It is the
 * fallback path of the later neural-TTS clips: started only by a tap or a
 * key, English voice preferring en-US (en-GB when the UK voice is chosen).
 * Without the Web Speech API the play buttons say so and do nothing.
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
 * Choose a voice: the preferred English variant, then any English voice;
 * local voices before network ones. `null` = let the browser decide from
 * the utterance's `lang`.
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
    const lang = normaliseLang(v.lang);
    let s = 0;
    if (lang === wanted) s += 4;
    else if (lang.startsWith('en-') || lang === 'en') s += 2;
    else return -1;
    if (v.localService) s += 1;
    return s;
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

/**
 * Speak `text` (stops anything that is still speaking). Returns false when
 * speech is not available.
 *
 * @param {string} text
 * @param {{ voice: VoicePreference, speed: SpeedSetting }} options
 */
export function speak(text, options) {
  if (!speechAvailable() || text.trim() === '') return false;
  const synth = window.speechSynthesis;
  const utterance = new SpeechSynthesisUtterance(text);
  utterance.lang = speechLang(options.voice);
  utterance.rate = speechRate(options.speed);
  const voice = pickVoice(synth.getVoices(), options.voice);
  if (voice) utterance.voice = voice;
  synth.cancel();
  synth.speak(utterance);
  return true;
}

/** Stop speaking (leaving the practice screen). */
export function stopSpeaking() {
  if (speechAvailable()) window.speechSynthesis.cancel();
}
