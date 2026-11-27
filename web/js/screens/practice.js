// @ts-check
/**
 * Practice — Understand, Produce and Spelling (design §7.1 screens
 * 2/2b/3/3b, §6.8, §7.2–§7.5; research wireframes 2, 3, 2b, 4, 4b; R-52,
 * R-53, R-54, R-56, R-58, R-60, R-62, R-70).
 *
 * Produce: the owner types the English word for the blanked sentence (the
 * answer field and its buttons sit in the bottom bar, which the on-screen
 * keyboard never covers) → Check / I don't know / I'll just rate myself →
 * the answer side shows what the mechanical check found and rings the
 * suggested grade; his 1–5 rating is the final word. What he typed stays
 * (also across a reload) until the card is checked, and goes with the
 * rating, so a failed post never loses it.
 *
 * Spelling: the same answer field and buttons (no hint); the question is
 * the word's audio — it starts by itself where the browser lets it (the
 * shared media element was unlocked by an earlier tap in the block), else
 * ▶ Listen is marked and says to tap it; a ▶ also sits next to the field,
 * so it stays in reach above the on-screen keyboard. The check is strict and the answer
 * side shows the letter diff and the spelling note. Without any sound on
 * this device the Czech gloss is the cue (never a dead end).
 *
 * Question side → reveal → answer side → rate 1–5. Every rating is posted
 * at once (idempotent by its client UUID); a 4/5 keeps the answer on screen
 * with "More" open until Next. The toast, ↶ and `U` undo the last rating.
 * When every item is rated, "See summary" finishes the block.
 *
 * Every phase change opens the 250 ms input guard again for the reveal and
 * Next buttons; ratings wait 500 ms after the reveal, and a rating tap that
 * lands where the reveal tap landed within 600 ms is ignored, so even a
 * slow double tap never rates the card that appears under the finger. The
 * tapped rating shows pressed at once; the "Rated … Undo" toast is a row of
 * the bottom bar, never over the card. A rating refused because the study
 * day rolled over is reported on Today. A rating whose answer was lost
 * (network error, timeout) is never silently re-sent with another grade:
 * the owner retries the same rating or first finds out what was saved.
 */

import { clipFor, clipsFor, playClip, preloadClips, primeClips, promptClipFor, stopClip } from '../clips.js';
import { h, mount } from '../dom.js';
import { keyAction, keyTarget } from '../keys.js';
import {
  answerView,
  checkResult,
  criterion,
  produceQuestion,
  questionView,
  revealLabel,
  showListen,
  spellingQuestion,
  spellingResult,
  typedHint,
} from '../practice/card.js';
import {
  TYPED_MAX,
  answersFor,
  canAddAccepted,
  checkSpelling,
  checkTyped,
  checkable,
  diffSummary,
  dontKnow,
  expectedAnswer,
  hint,
  selfRated,
  spellingDiff,
  typedText,
} from '../grading.js';
import {
  afterRating,
  currentItem,
  exampleSentence,
  hasHint,
  hasMore,
  isSpelling,
  isTyped,
  moreOpenAfterCheck,
  moreOpenOnReveal,
  plainText,
  progress,
  promptSpeech,
  wordSpeech,
} from '../practice/model.js';
import { clearDraft, loadDraft, saveDraft } from '../practice/draft.js';
import { legend, legendLine, ratingBar, suggestionLine } from '../practice/rating-bar.js';
import {
  INPUT_GUARD_MS,
  guardOpen,
  isGrade,
  legendVisible,
  rating,
  ratingGuardOpen,
  sameSpotTap,
  toastPlan,
  toastText,
} from '../rating.js';
import { setNotice } from '../notice.js';
import { acceptAnswer, fetchCurrent, fetchToday, finishBlock, postRating, takeStarted, undoRating } from '../session.js';
import { DEFAULT_SETTINGS, loadSettings } from '../settings.js';
import { prepareSpeech, speak, speechAvailable, stopSpeaking } from '../speech.js';
import { setSummary } from '../summary-store.js';
import { Toast } from '../toast.js';

/**
 * @typedef {import('../session.js').Session} Session
 * @typedef {import('../session.js').Item} Item
 * @typedef {import('../keys.js').Phase} Phase
 * @typedef {import('../keys.js').Action} Action
 * @typedef {import('../rating.js').Grade} Grade
 * @typedef {import('../grading.js').Check} Check
 * @typedef {import('../practice/card.js').AcceptState} AcceptState
 * @typedef {import('../practice/card.js').ListenState} ListenState
 * @typedef {import('../session.js').AnswerDetails} AnswerDetails
 * @typedef {{ itemId: number, grade: Grade, id: string, details: AnswerDetails | null }} PendingRating
 * @typedef {{ ok: false, status: number, code: string, message: string }} Failure
 */

/** Keyboard shortcuts listed in the help panel (design §7.3). */
const SHORTCUTS = [
  ['Space / Enter', 'show the answer · next'],
  ['Enter', 'Produce, Spelling: check · accept the suggested grade'],
  ['H', 'Produce: hint (outside the answer field)'],
  ['1 – 5', 'rate (1 = best)'],
  ['U', 'undo the last rating'],
  ['M', 'more: note, forms, sentences'],
  ['S / R', 'speak sentence / word (Spelling: R or ▶ replays the word)'],
  ['E', 'word detail'],
  ['Esc', 'close the help · pause the block'],
  ['?', 'this help'],
];

/** Below this visible height with the keyboard open, the bottom bar is compact. */
const KEYBOARD_TIGHT_PX = 300;
/** "Saving…" appears only when a request takes longer than this. */
const SAVING_DELAY_MS = 150;
/** The "already saved" toast stays longer: it asks for a check. */
const ALREADY_SAVED_TOAST_MS = 8000;
/** The silent-switch hint has been dismissed on this device. */
const SILENT_HINT_KEY = 'vocab.silent-hint-seen';
/** Is this a touch device (where the phone's silent switch may mute speech)? */
function touchDevice() {
  return typeof matchMedia === 'function' && matchMedia('(pointer: coarse)').matches;
}

function silentHintSeen() {
  try {
    return localStorage.getItem(SILENT_HINT_KEY) === '1';
  } catch {
    return true;
  }
}

function rememberSilentHint() {
  try {
    localStorage.setItem(SILENT_HINT_KEY, '1');
  } catch {
    // No storage: the hint may show again next time.
  }
}

/** Elapsed milliseconds on a monotonic clock. */
function now() {
  return performance.now();
}

/**
 * Why a request failed, as the end of a sentence for the owner (never the
 * server's technical message).
 *
 * @param {Failure} failure
 */
function reason(failure) {
  if (failure.code === 'network') return 'the server could not be reached';
  if (failure.code === 'timeout') return 'the server did not answer in time';
  return `the server reported a problem (${failure.status})`;
}

/** @param {Failure} failure */
function lostAnswer(failure) {
  return failure.status === 0;
}

/** @param {Grade} grade */
function gradeName(grade) {
  return `${grade} · ${rating(grade).label}`;
}

/**
 * Keep the screen on while a block is practised, where the browser
 * supports it; the lock is dropped by the browser when the page is hidden,
 * so it is requested again when the page becomes visible.
 */
function wakeLock() {
  /** @type {any} */
  let sentinel = null;
  let active = true;
  const nav = /** @type {any} */ (navigator);
  const acquire = () => {
    if (!active || document.visibilityState !== 'visible') return;
    if (!nav.wakeLock || typeof nav.wakeLock.request !== 'function') return;
    nav.wakeLock
      .request('screen')
      .then((/** @type {any} */ s) => {
        if (active) sentinel = s;
        else void s.release().catch(() => {});
      })
      .catch(() => {});
  };
  const onVisibility = () => {
    if (document.visibilityState === 'visible') acquire();
  };
  document.addEventListener('visibilitychange', onVisibility);
  acquire();
  return () => {
    active = false;
    document.removeEventListener('visibilitychange', onVisibility);
    if (sentinel) void sentinel.release().catch(() => {});
    sentinel = null;
  };
}

/**
 * @param {HTMLElement} root
 * @param {import('../router.js').IsCurrent} isCurrent
 * @returns {() => void} cleanup when the screen is left
 */
export function renderPractice(root, isCurrent) {
  mount(root, h('p', { class: 'muted' }, 'Loading…'));

  /** @type {{ session: Session | null, item: Item | null, phase: Phase, phaseAt: number, revealedAt: number,
   *           shownAt: number, moreOpen: boolean, helpOpen: boolean, legendAlways: boolean, busy: boolean,
   *           phaseKey: string | null, pending: PendingRating | null, unsure: (PendingRating & { why: string }) | null,
   *           settings: import('../settings.js').Settings, revealTap: { x: number, y: number, t: number } | null,
   *           chosen: Grade | null, typed: string, hintShown: boolean, check: Check | null,
   *           accept: AcceptState | null, typedNote: string, listen: ListenState }} */
  const state = {
    session: null,
    item: null,
    phase: 'question',
    // The first card has no tap before it to guard against.
    phaseAt: -Infinity,
    phaseKey: null,
    revealedAt: 0,
    shownAt: now(),
    moreOpen: false,
    helpOpen: false,
    legendAlways: false,
    busy: false,
    pending: null,
    unsure: null,
    settings: DEFAULT_SETTINGS,
    revealTap: null,
    chosen: null,
    // Produce: what is typed, whether the hint shows, the check result,
    // the "also accept" offer and a note under the field.
    typed: '',
    hintShown: false,
    check: null,
    accept: null,
    typedNote: '',
    // Spelling: where the question's sound stands.
    listen: 'waiting',
  };
  /** Check results of this visit by item (undo returns to the checked answer). */
  /** @type {Map<number, Check>} */
  const checks = new Map();
  /** Items whose typed answer was added to the accepted answers. */
  /** @type {Set<number>} */
  const acceptedItems = new Set();
  /**
   * Answers accepted with "Also accept" in this visit, by word id: every
   * later item of the word (a repeat in this block) accepts them at once.
   *
   * @type {Map<number, string[]>}
   */
  const alsoAccepted = new Map();
  const speechOk = speechAvailable();
  prepareSpeech();
  /** Recorded-clip info per word id, as far as it has arrived (M6). */
  /** @type {Map<number, import('../clips.js').WordClips | null>} */
  const clips = new Map();
  /**
   * Ask for the clips of the current item and the next unanswered one,
   * and let the browser fetch the next one's clips ahead (R-40).
   */
  const loadClips = () => {
    const session = state.session;
    const item = state.item;
    if (!session || !item) return;
    const later = session.items.filter((i) => i.answered === null && i.item_id !== item.item_id);
    for (const [index, wanted] of [item, ...later.slice(0, 1)].entries()) {
      const id = wanted.word.id;
      void clipsFor(id).then((info) => {
        clips.set(id, info);
        if (index > 0) preloadClips(info);
        else if (info && state.item?.word.id === id) markApproximate(info);
      });
    }
  };

  const top = h('div', { class: 'practice-top' });
  const help = h('div', { class: 'practice-help' });
  const body = h('section', { class: 'practice-body', tabindex: '-1', 'aria-label': 'Card' });
  const toastSlot = h('div', { class: 'toast-slot' });
  const saving = h('p', { class: 'saving', 'data-testid': 'saving' });
  const bottomContent = h('div', { class: 'bottom-content' });
  const bottom = h('div', { class: 'practice-bottom' }, toastSlot, saving, bottomContent);
  const live = h('p', { class: 'sr-only', role: 'status', 'aria-live': 'polite' });
  const errorLine = h('p', { class: 'error practice-error', role: 'alert', 'data-testid': 'practice-error' });
  const audioNote = h('div', { class: 'audio-note', 'data-testid': 'audio-note' });
  const toast = new Toast(toastSlot);
  const screen = h('div', { class: 'practice' }, top, help, errorLine, audioNote, body, bottom, live);
  /** @type {(() => void) | null} */
  let releaseWakeLock = null;

  /**
   * The on-screen keyboard covers the bottom of the layout viewport (iOS,
   * and Android in its default mode): the bottom bar, with the answer field
   * and its buttons, moves up by the covered height (design §7.1 screen 3).
   */
  const viewport = window.visualViewport ?? null;
  const onViewport = () => {
    if (!viewport) return;
    const inset = Math.max(0, Math.round(window.innerHeight - viewport.height - viewport.offsetTop));
    screen.style.setProperty('--keyboard-inset', `${inset}px`);
    screen.classList.toggle('keyboard-open', inset > 0);
    // Little room left (a phone in landscape with the keyboard open): the
    // bar shrinks to the answer field so the sentence with the blank stays
    // in view above it.
    const tight = inset > 0 && viewport.height < KEYBOARD_TIGHT_PX;
    screen.classList.toggle('keyboard-tight', tight);
    if (tight) keepClozeInView();
  };
  /** Scroll the blanked sentence into the room above the bottom bar. */
  const keepClozeInView = () => {
    const cloze = body.querySelector('[data-testid="cloze"]');
    if (!(cloze instanceof HTMLElement)) return;
    const room = bottom.getBoundingClientRect().top;
    const box = cloze.getBoundingClientRect();
    if (box.bottom > room) window.scrollBy(0, Math.min(box.top, box.bottom - room + 4));
    else if (box.top < 0) window.scrollBy(0, box.top - 4);
  };
  viewport?.addEventListener('resize', onViewport);
  viewport?.addEventListener('scroll', onViewport);

  // ── Produce: the answer field ────────────────────────────────────────────

  // No autocorrect, capitalisation or spellcheck: they would change a
  // correct rare word or give the answer away (research §3).
  const answerInput = h('input', {
    type: 'text',
    class: 'answer-input',
    id: 'typed-answer',
    name: 'typed-answer',
    autocomplete: 'off',
    autocorrect: 'off',
    autocapitalize: 'none',
    spellcheck: 'false',
    enterkeyhint: 'go',
    maxlength: TYPED_MAX,
    lang: 'en',
    placeholder: 'Type the English word',
    'aria-label': 'Your answer: the English word for the blank',
    'data-testid': 'answer-input',
  });
  answerInput.addEventListener('input', () => {
    state.typed = answerInput.value;
    if (state.item) saveDraft(state.item.item_id, { typed: state.typed, hint: state.hintShown, check: null });
  });
  answerInput.addEventListener('keydown', (event) => {
    if (event.key !== 'Enter' || event.isComposing) return;
    event.preventDefault();
    if (event.repeat) return;
    handlingKey = 'Enter';
    try {
      checkAnswer();
    } finally {
      handlingKey = null;
    }
  });

  /** @param {...(Node | string)} parts */
  const announce = (...parts) => {
    mount(live);
    live.append(...parts);
  };

  /** @param {string} message a complete sentence */
  const showError = (message) => {
    errorLine.textContent = message;
  };
  const clearError = () => {
    errorLine.textContent = '';
  };

  /** @type {ReturnType<typeof setTimeout> | null} */
  let savingTimer = null;
  /** @param {boolean} busy */
  const setBusy = (busy) => {
    state.busy = busy;
    if (savingTimer !== null) clearTimeout(savingTimer);
    savingTimer = null;
    if (busy) {
      savingTimer = setTimeout(() => {
        saving.textContent = 'Saving…';
      }, SAVING_DELAY_MS);
    } else {
      saving.textContent = '';
    }
  };

  /** The key whose press is being handled (null for pointer input). */
  /** @type {string | null} */
  let handlingKey = null;

  /** @param {Phase} phase */
  const setPhase = (phase) => {
    state.phase = phase;
    state.phaseAt = now();
    state.phaseKey = handlingKey;
  };
  /** Has the input guard passed since the last phase change? */
  const inputReady = () => guardOpen(now() - state.phaseAt);
  /**
   * May Space/Enter or a tap reveal or go on now? Taps and clicks wait for
   * the guard (a double tap must not act on the new card); a key waits only
   * when the same key just changed the phase (a bounced double press).
   */
  const advanceReady = () => inputReady() || (handlingKey !== null && handlingKey !== state.phaseKey);

  /** @type {ReturnType<typeof setTimeout> | null} */
  let guardTimer = null;
  /**
   * Reveal/Next buttons stay disabled while the guard runs, so a second tap
   * of a double tap lands on nothing (no dimming: see `.guarded` in CSS).
   */
  const armGuard = () => {
    if (guardTimer !== null) clearTimeout(guardTimer);
    guardTimer = null;
    const left = state.phaseAt + INPUT_GUARD_MS - now();
    const buttons = [...bottomContent.querySelectorAll('button[data-guarded]')];
    if (left <= 0) return;
    for (const button of buttons) {
      if (button instanceof HTMLButtonElement) button.disabled = true;
    }
    guardTimer = setTimeout(() => {
      guardTimer = null;
      for (const button of buttons) {
        if (button instanceof HTMLButtonElement) button.disabled = false;
      }
    }, left);
  };

  // ── audio ────────────────────────────────────────────────────────────────

  /** The last pointer-down on the screen, to tell where the reveal tap landed. */
  /** @type {{ x: number, y: number, t: number } | null} */
  let lastPointer = null;
  /** The rating gesture in progress started where the reveal tap landed. */
  let ignoreRatingTap = false;

  /**
   * Say why nothing plays, or show the silent-switch hint once on touch
   * devices (U5). Both sit above the card, never over it.
   *
   * @param {import('../speech.js').SpeakResult} result
   */
  const afterSpeak = (result) => {
    if (result === 'unavailable') {
      announce('Speech is not available in this browser.');
      return;
    }
    if (result === 'no-voice') {
      mount(audioNote, h('p', { class: 'hint', role: 'status' }, 'No audio on this device yet: it has no English voice of its own.'));
      return;
    }
    if (touchDevice() && !silentHintSeen() && audioNote.childElementCount === 0) {
      const ok = h('button', { type: 'button', class: 'hint-ok' }, 'OK');
      ok.addEventListener('click', () => {
        rememberSilentHint();
        mount(audioNote);
      });
      mount(audioNote, h('div', { class: 'hint', 'data-testid': 'silent-hint' }, h('p', {}, 'No sound? Check your phone’s silent switch.'), ok));
    }
  };

  /**
   * The ▶ controls of `what` on screen (a Spelling question has two: ▶
   * Listen on the card and ▶ next to the answer field).
   *
   * @param {'sentence' | 'word'} what
   */
  const playButtons = (what) => [...screen.querySelectorAll(`button.play-${what}`)];

  /** Is the Spelling question of the current item on screen? */
  const spellingPrompt = () => state.phase === 'question' && state.item !== null && isSpelling(state.item);

  /** @param {import('../clips.js').WordClips} info */
  const markApproximate = (info) => {
    if (!info.enabled || !info.word.approximate) return;
    // A Spelling question plays no clip that is not a prompt clip.
    if (spellingPrompt() && !promptClipFor(info)) return;
    for (const word of playButtons('word')) {
      if (word.hasAttribute('data-approximate')) continue;
      word.setAttribute('data-approximate', '');
      word.setAttribute('title', `${word.getAttribute('title') ?? 'Play word'} — approximate pronunciation`);
    }
  };

  /**
   * Counts the Spelling prompt's sound requests: an auto-play still
   * waiting for the clip information gives up when a tap came after it.
   */
  let promptTurn = 0;

  /**
   * Speak `text` with the browser (the fallback, R-41).
   *
   * @param {string} text
   * @param {Element[]} buttons
   */
  const speakFallback = (text, buttons) => {
    // Never two sounds at once: a clip still playing (another button's)
    // stops before the browser speaks.
    stopClip();
    const result = speak(text, { voice: state.settings.voice, speed: state.settings.speed }, {
      onStart: () => buttons.forEach((b) => b.classList.add('playing')),
      onEnd: () => buttons.forEach((b) => b.classList.remove('playing')),
    });
    afterSpeak(result);
    return result;
  };

  /**
   * Play the recorded clip of `what` if one is ready, else speak `text`,
   * showing loading/playing/failed on the play button of that kind. Runs
   * synchronously in the tap or key handler (iOS starts media only there).
   * `clipOnly`: auto-play, which never falls back to speech.
   *
   * The word of a Spelling question plays only a clip that says no word
   * letter by letter, and speech only when it would not spell either
   * ([`promptClipFor`], [`promptSpeech`]; R-58). A tap before the clip
   * information arrived cancels the question's pending auto-play and
   * speaks at once: one voice at a time.
   *
   * @param {string} text
   * @param {'sentence' | 'word'} what
   * @param {number | null} sentenceId
   * @param {boolean} [clipOnly]
   */
  const play = (text, what, sentenceId, clipOnly = false) => {
    const buttons = playButtons(what);
    const item = state.item;
    const prompt = what === 'word' && spellingPrompt();
    if (prompt) promptTurn++;
    const info = item ? clips.get(item.word.id) : null;
    const clip = !item ? null : prompt ? promptClipFor(info) : clipFor(info, what, sentenceId);
    const fallback = prompt && item ? promptSpeech(item.word) : text;
    if (!clip || clip.url === null) {
      if (clipOnly) return;
      if (fallback === null) setListen('none');
      else spoken(speakFallback(fallback, buttons));
      return;
    }
    stopSpeaking();
    buttons.forEach((b) => b.classList.remove('failed'));
    buttons.forEach((b) => b.classList.add('loading'));
    playClip(clip.url, state.settings.speed === '0.75' ? 0.75 : 1, {
      onStart: () => {
        buttons.forEach((b) => b.classList.remove('loading'));
        buttons.forEach((b) => b.classList.add('playing'));
        if (what === 'word') heard();
      },
      onEnd: () => buttons.forEach((b) => b.classList.remove('loading', 'playing')),
      onFail: () => {
        buttons.forEach((b) => b.classList.remove('loading', 'playing'));
        buttons.forEach((b) => b.classList.add('failed'));
        setTimeout(() => buttons.forEach((b) => b.classList.remove('failed')), 2_000);
        if (clipOnly) return;
        if (fallback === null) setListen('none');
        else spoken(speakFallback(fallback, buttons));
      },
    });
  };

  /**
   * Where the Spelling question's sound stands now (shown under ▶ Listen).
   *
   * @param {ListenState} listen
   */
  const setListen = (listen) => {
    if (state.listen === listen) return;
    state.listen = listen;
    // In place: rebuilding the card would drop the focus on ▶ Listen.
    if (spellingPrompt()) showListen(body, listen);
  };
  /** The word's sound started: the Spelling question has been heard. */
  const heard = () => {
    if (spellingPrompt()) setListen('heard');
  };
  /**
   * What the browser's speech did for the word: a Spelling question with
   * no English voice on this device falls back to its Czech cue.
   *
   * @param {import('../speech.js').SpeakResult} result
   */
  const spoken = (result) => {
    if (result === 'speaking') heard();
    else if (spellingPrompt()) setListen('none');
  };

  /**
   * The Spelling question plays its word by itself (design §7.4): the
   * recorded clip, once its address has arrived, through the shared media
   * element — which an earlier tap unlocked on iOS. Where the browser still
   * refuses, ▶ Listen is marked for a tap; without a clip the browser's
   * speech waits for that tap (it may not start by itself); with neither
   * the Czech cue is all there is. Only a clip and speech that do not
   * spell the word out count (R-58). A tap on ▶ in the meantime takes over
   * (`promptTurn`).
   *
   * @param {Item} item
   */
  const autoplayPrompt = (item) => {
    const turn = ++promptTurn;
    const still = () => state.item?.item_id === item.item_id && state.phase === 'question' && promptTurn === turn;
    void clipsFor(item.word.id).then((info) => {
      clips.set(item.word.id, info);
      if (!still()) return;
      const clip = promptClipFor(info);
      if (!clip || clip.url === null) {
        setListen(speechOk && promptSpeech(item.word) !== null ? 'tap' : 'none');
        return;
      }
      setListen('playing');
      const buttons = playButtons('word');
      stopSpeaking();
      playClip(clip.url, state.settings.speed === '0.75' ? 0.75 : 1, {
        onStart: () => {
          buttons.forEach((b) => b.classList.add('playing'));
          heard();
        },
        onEnd: () => playButtons('word').forEach((b) => b.classList.remove('playing')),
        onFail: () => {
          playButtons('word').forEach((b) => b.classList.remove('playing'));
          if (still() && state.listen !== 'heard') setListen('tap');
        },
      });
    });
  };

  const speakSentence = () => {
    const item = state.item;
    if (!item || state.phase === 'complete') return;
    // A typed question plays no sentence: it would say the answer.
    if (state.phase === 'question' && isTyped(item)) return;
    const sentence =
      item.card.format === 'sentence' && item.sentence ? item.sentence : state.phase === 'question' ? null : exampleSentence(item);
    if (!sentence) return;
    play(plainText(sentence.en), 'sentence', sentence.id);
  };
  const speakWord = () => {
    const item = state.item;
    if (!item || state.phase === 'complete') return;
    // A Produce question plays nothing; a Spelling question is the word's sound.
    if (state.phase === 'question' && isTyped(item) && !isSpelling(item)) return;
    // On the question side of a sentence card only the sentence plays.
    if (state.phase === 'question' && item.card.format === 'sentence' && item.sentence) return;
    play(wordSpeech(item.word), 'word', null);
  };
  const audio = { speechOk, onSpeakSentence: speakSentence, onSpeakWord: speakWord };

  // ── rendering ────────────────────────────────────────────────────────────

  const renderTop = () => {
    const session = state.session;
    if (!session) return;
    const p = progress(session);
    const shown = state.phase === 'complete' ? p.total : state.phase === 'question' || state.phase === 'answer' ? p.position : p.answered;
    mount(
      top,
      h(
        'button',
        {
          type: 'button',
          class: 'pause-button',
          'aria-label': 'Pause: end this block for now; unrated cards stay due (Esc)',
          title: 'Pause — unrated cards stay due (Esc)',
          'data-testid': 'pause',
          onclick: () => void end(),
        },
        'Pause',
      ),
      h(
        'div',
        { class: 'progress' },
        h('span', { class: 'progress-text', 'data-testid': 'progress' }, `${shown} / ${p.total}`),
        h('progress', { max: String(p.total), value: String(p.answered), 'aria-label': `${p.answered} of ${p.total} rated` }),
      ),
      h(
        'button',
        {
          type: 'button',
          class: 'icon-button',
          'aria-label': 'Help: rating legend and keys (?)',
          'aria-expanded': state.helpOpen ? 'true' : 'false',
          title: 'Legend and keys (?)',
          onclick: () => toggleHelp(),
        },
        '?',
      ),
      h(
        'button',
        {
          type: 'button',
          class: 'icon-button',
          'aria-label': 'Undo last rating (U)',
          title: 'Undo last rating (U)',
          disabled: !session.can_undo,
          'data-testid': 'undo',
          onclick: () => void undo(),
        },
        '↶',
      ),
    );
  };

  const renderHelp = () => {
    if (!state.helpOpen) {
      mount(help);
      return;
    }
    mount(
      help,
      h(
        'div',
        { class: 'card help-card' },
        h('h2', {}, 'Rating: 1 = best'),
        legend(),
        h('h2', {}, 'Keys'),
        h(
          'dl',
          { class: 'shortcuts' },
          SHORTCUTS.map(([key, what]) => h('div', {}, h('dt', {}, h('kbd', {}, key)), h('dd', {}, what))),
        ),
      ),
    );
  };

  const renderBody = () => {
    const item = state.item;
    const session = state.session;
    if (!session) return;
    if (state.phase === 'complete' || !item) {
      mount(
        body,
        h(
          'div',
          { class: 'block-complete' },
          h('h1', {}, 'Block complete'),
          h('p', {}, `All ${session.items.length} cards rated.`),
          session.can_undo ? h('p', { class: 'muted' }, 'You can still undo the last rating.') : null,
        ),
      );
      return;
    }
    if (state.phase === 'question') {
      if (isSpelling(item)) mount(body, spellingQuestion(item, { onListen: speakWord, listen: state.listen }));
      else mount(body, isTyped(item) ? produceQuestion(item) : questionView(item, audio));
      return;
    }
    const check = isTyped(item) ? state.check : null;
    /** @type {Node | null} */
    let lead = null;
    if (check && isSpelling(item)) lead = spellingResult(item, check);
    else if (check) lead = checkResult(item, check, { state: state.accept, onAccept: () => void acceptTyped() });
    mount(
      body,
      answerView(item, {
        audio,
        moreOpen: state.moreOpen,
        showMore: hasMore(item),
        onToggleMore: () => toggleMore(true),
        lead,
      }),
      state.phase === 'answer' && !state.unsure && state.legendAlways && !state.helpOpen ? legendLine() : null,
    );
  };

  /** The rating may not have been saved: retry it or find out first. */
  const unsurePanel = () => {
    const unsure = state.unsure;
    if (!unsure) return null;
    return h(
      'div',
      { class: 'unsure', 'data-testid': 'unsure' },
      h('p', {}, `Your rating ${gradeName(unsure.grade)} may not have been saved because ${unsure.why}.`),
      h(
        'div',
        { class: 'actions' },
        h('button', { type: 'button', class: 'primary', 'data-testid': 'retry', onclick: () => void retry() }, `Retry ${unsure.grade}`),
        h('button', { type: 'button', 'data-testid': 'rate-differently', onclick: () => void rateDifferently() }, 'Rate differently'),
      ),
    );
  };

  const renderBottom = () => {
    drawBottom();
    armGuard();
  };

  /**
   * The answer field, Check, Hint (Produce), I don't know and "I'll just
   * rate myself"; a Spelling card has ▶ next to the field instead of the
   * hint, in reach above the on-screen keyboard.
   */
  const drawTypedQuestion = () => {
    const hadFocus = document.activeElement === answerInput;
    const item = state.item;
    const spelling = item !== null && isSpelling(item);
    answerInput.placeholder = spelling ? 'Type the word you hear' : 'Type the English word';
    answerInput.setAttribute('aria-label', spelling ? 'Your answer: spell the word you hear' : 'Your answer: the English word for the blank');
    mount(
      bottomContent,
      state.hintShown && item && hasHint(item) ? typedHint(hint(expectedAnswer(item))) : null,
      h(
        'div',
        { class: 'answer-row' },
        spelling
          ? h(
              'button',
              { type: 'button', class: 'play play-word replay', 'aria-label': 'Play the word again', title: 'Play the word again', 'data-testid': 'replay', onclick: () => speakWord() },
              h('span', { 'aria-hidden': 'true' }, '▶'),
            )
          : null,
        answerInput,
        h('button', { type: 'button', class: 'primary check-button guarded', 'data-guarded': true, 'data-testid': 'check', onclick: () => checkAnswer() }, 'Check'),
      ),
      state.typedNote ? h('p', { class: 'typed-note', role: 'status', 'data-testid': 'typed-note' }, state.typedNote) : null,
      h(
        'div',
        { class: `typed-actions${spelling ? ' two' : ''}` },
        spelling
          ? null
          : h('button', { type: 'button', class: 'guarded', 'data-guarded': true, 'data-testid': 'hint-button', disabled: state.hintShown, title: 'Hint: first letter and letter count (H)', onclick: () => showHint() }, 'Hint'),
        h('button', { type: 'button', class: 'guarded', 'data-guarded': true, 'data-testid': 'dont-know', onclick: () => giveUp() }, 'I don’t know'),
        h(
          'button',
          { type: 'button', class: 'guarded', 'data-guarded': true, 'data-testid': 'self-rate', 'aria-label': 'I’ll just rate myself', title: 'I’ll just rate myself: show the answer without checking', onclick: () => rateMyself() },
          'Rate myself',
        ),
      ),
    );
    if (hadFocus) answerInput.focus({ preventScroll: true });
  };

  const drawBottom = () => {
    const item = state.item;
    if (state.phase === 'question' && item && isTyped(item)) {
      drawTypedQuestion();
    } else if (state.phase === 'question' && item) {
      mount(
        bottomContent,
        h('p', { class: 'criterion' }, criterion(item)),
        h('button', { type: 'button', class: 'primary big guarded', 'data-guarded': true, 'data-testid': 'reveal', onclick: () => reveal() }, revealLabel(item)),
      );
    } else if (state.phase === 'answer' && state.unsure) {
      mount(bottomContent, unsurePanel());
    } else if (state.phase === 'answer') {
      const typed = item !== null && isTyped(item);
      const suggested = typed ? state.check?.suggested ?? null : null;
      mount(
        bottomContent,
        typed ? suggestionLine(suggested) : null,
        ratingBar((grade) => void rate(grade), state.busy ? state.chosen : null, suggested),
      );
    } else if (state.phase === 'feedback') {
      mount(
        bottomContent,
        h('button', { type: 'button', class: 'primary big guarded', 'data-guarded': true, 'data-testid': 'next', onclick: () => next() }, 'Next'),
      );
    } else {
      mount(
        bottomContent,
        h('button', { type: 'button', class: 'primary big guarded', 'data-guarded': true, 'data-testid': 'see-summary', onclick: () => next() }, 'See summary'),
      );
    }
  };

  /** @param {{ focus?: boolean, scroll?: boolean }} [options] */
  const render = ({ focus = false, scroll = false } = {}) => {
    renderTop();
    renderHelp();
    renderBody();
    renderBottom();
    // The play buttons are new: mark them again from what is known.
    const info = state.item ? clips.get(state.item.word.id) : null;
    if (info) markApproximate(info);
    if (scroll) window.scrollTo(0, 0);
    if (!focus) return;
    // A Produce question: straight into the answer field.
    if (state.phase === 'question' && state.item && isTyped(state.item)) answerInput.focus({ preventScroll: true });
    else body.focus({ preventScroll: true });
  };

  // ── transitions ──────────────────────────────────────────────────────────

  /**
   * A fresh question side for `item`: nothing typed — or what this tab
   * typed for it before a reload — no hint, no check. Drafts of other
   * items stay.
   *
   * @param {Item | null} item
   */
  const resetTyped = (item) => {
    const draft = item && isTyped(item) ? loadDraft(item.item_id) : null;
    state.typed = draft?.typed ?? '';
    state.hintShown = item !== null && hasHint(item) && (draft?.hint ?? false);
    state.check = null;
    state.accept = null;
    state.typedNote = '';
    state.listen = 'waiting';
    answerInput.value = state.typed;
  };

  /**
   * What "Also accept" offers for `item` after `check`.
   *
   * @param {Item} item
   * @param {Check | null} check
   * @returns {AcceptState | null}
   */
  const acceptStateFor = (item, check) => {
    // A misspelling is never an accepted answer.
    if (isSpelling(item) || !canAddAccepted(check)) return null;
    return acceptedItems.has(item.item_id) ? 'saved' : 'offer';
  };

  /**
   * Back on the answer side of `item` (after an undo, or to rate it again):
   * a Produce card shows its check of this visit again; one not checked in
   * this visit (the page was reloaded) starts at its question.
   *
   * @param {Item} item
   */
  const reopen = (item) => {
    state.item = item;
    const check = isTyped(item) ? checks.get(item.item_id) ?? null : null;
    if (isTyped(item) && !check) {
      resetTyped(item);
      setPhase('question');
      return;
    }
    state.check = check;
    state.accept = acceptStateFor(item, check);
    setPhase('answer');
    state.revealedAt = state.phaseAt;
    state.moreOpen = check ? moreOpenAfterCheck(item, check.match) : moreOpenOnReveal(item);
  };

  /**
   * Show the next unanswered item (or the "complete" state). A Produce
   * card checked before a reload returns to its checked answer side.
   */
  const advance = () => {
    const session = state.session;
    if (!session) return;
    const item = currentItem(session);
    const stored = item && isTyped(item) ? loadDraft(item.item_id)?.check ?? null : null;
    if (item && stored) {
      checks.set(item.item_id, stored);
      reopen(item);
    } else {
      state.item = item;
      resetTyped(item);
      setPhase(item ? 'question' : 'complete');
      state.moreOpen = false;
    }
    state.shownAt = now();
    stopClip();
    render({ focus: true, scroll: true });
    loadClips();
    soundQuestion();
  };

  /**
   * A Spelling question just came on screen (the next card, an undo back
   * to it, a rating to give again): its word plays, or the status says
   * why not — never left at "Getting the sound…".
   */
  const soundQuestion = () => {
    if (state.item && spellingPrompt()) autoplayPrompt(state.item);
  };

  /**
   * Show `item` after its rating `grade`: a fail stays on screen with
   * "More" open until Next, a pass moves on.
   *
   * @param {Item} item
   * @param {number} grade
   */
  const afterRated = (item, grade) => {
    if (afterRating(grade) === 'feedback') {
      state.item = item;
      state.check = isTyped(item) ? checks.get(item.item_id) ?? null : null;
      state.accept = acceptStateFor(item, state.check);
      setPhase('feedback');
      state.moreOpen = hasMore(item);
      render({ focus: true });
    } else {
      advance();
    }
  };

  const reveal = () => {
    if (state.phase !== 'question' || !state.item) return;
    // A Produce card is answered by typing (Space on the page goes there).
    if (isTyped(state.item)) {
      answerInput.focus();
      return;
    }
    if (!advanceReady()) return;
    // Where the finger was when the answer appeared (null for keys).
    state.revealTap = handlingKey === null && lastPointer && now() - lastPointer.t < 1000 ? lastPointer : null;
    setPhase('answer');
    state.revealedAt = state.phaseAt;
    state.moreOpen = moreOpenOnReveal(state.item);
    render({ focus: true });
    const word = state.item.word;
    announce(h('span', { lang: 'en' }, word.headword), ': ', h('span', { lang: 'cs' }, word.czech.join(', ')));
    // Auto-play (off by default): the word's recorded clip, started by the
    // reveal tap or key itself; never browser speech.
    if (state.settings.autoplay) play(wordSpeech(word), 'word', null, true);
  };

  // ── Produce: check, hint, I don't know, rate myself, also accept ─────────

  /**
   * Show the answer side of the Produce card after `check` (wireframe 4b).
   *
   * @param {Check} check
   */
  const showChecked = (check) => {
    const item = state.item;
    if (!item) return;
    // Where the finger was (the Check button sits where ratings appear).
    state.revealTap = handlingKey === null && lastPointer && now() - lastPointer.t < 1000 ? lastPointer : null;
    state.check = check;
    checks.set(item.item_id, check);
    state.accept = acceptStateFor(item, check);
    state.typedNote = '';
    // A reload now returns to this answer side, not to an empty question.
    saveDraft(item.item_id, { typed: check.typed ?? state.typed, hint: check.hintUsed, check });
    setPhase('answer');
    state.revealedAt = state.phaseAt;
    state.moreOpen = moreOpenAfterCheck(item, check.match);
    // Closes the on-screen keyboard: the answer side needs the screen.
    answerInput.blur();
    render({ focus: true, scroll: true });
    if (isSpelling(item)) {
      announceSpelling(item, check);
      return;
    }
    const answer = expectedAnswer(item);
    if (check.verdict === 'correct') announce('Correct: ', h('span', { lang: 'en' }, answer), '.');
    else if (check.verdict === 'right_word') announce('Right word. Here it is: ', h('span', { lang: 'en' }, answer), '.');
    else if (check.verdict === 'alternative') announce('Accepted. This sentence has ', h('span', { lang: 'en' }, answer), '.');
    else if (check.match === 'none') announce('Not quite. You typed ', h('span', { lang: 'en' }, check.typed ?? ''), '; the answer is ', h('span', { lang: 'en' }, answer), '.');
    else announce('The answer: ', h('span', { lang: 'en' }, answer), '.');
  };

  /**
   * Tell a screen reader what the Spelling check found, the differences in
   * words.
   *
   * @param {Item} item
   * @param {Check} check
   */
  const announceSpelling = (item, check) => {
    const word = h('span', { lang: 'en' }, item.word.headword);
    if (check.match === 'exact') announce('Correct spelling: ', word, '.');
    else if (check.match === 'none') announce('Not quite: ', diffSummary(spellingDiff(check.typed ?? '', item.word.headword)), '. The spelling is ', word, '.');
    else announce('The spelling: ', word, '.');
  };

  /** May the typed question act now (not the second tap of a double tap)? */
  const typedQuestionReady = () => state.phase === 'question' && state.item !== null && isTyped(state.item) && advanceReady();

  const checkAnswer = () => {
    const item = state.item;
    if (!item || !typedQuestionReady()) return;
    const typed = typedText(answerInput.value);
    if (!checkable(typed)) {
      state.typedNote = 'Type the word first, or choose “I don’t know”.';
      renderBottom();
      answerInput.focus();
      announce(state.typedNote);
      return;
    }
    showChecked(
      isSpelling(item)
        ? checkSpelling(typed, item.word.headword)
        : checkTyped(typed, answersFor(item, alsoAccepted.get(item.word.id)), state.hintShown),
    );
  };

  const giveUp = () => {
    if (typedQuestionReady()) showChecked(dontKnow(state.hintShown));
  };

  const rateMyself = () => {
    if (typedQuestionReady()) showChecked(selfRated(state.hintShown));
  };

  /** The hint: first letter and letter count (it lowers a match to 3). */
  const showHint = () => {
    const item = state.item;
    if (state.phase !== 'question' || !item || !hasHint(item) || state.hintShown) return;
    state.hintShown = true;
    saveDraft(item.item_id, { typed: answerInput.value, hint: true, check: null });
    renderBody();
    renderBottom();
    answerInput.focus({ preventScroll: true });
    const shown = hint(expectedAnswer(item));
    announce(`Hint: starts with ${shown.pattern.charAt(0)}, ${shown.letters} letters.`);
  };

  /** Enter on the answer side of a Produce card takes the suggested grade. */
  const acceptSuggested = () => {
    const item = state.item;
    const suggested = state.check?.suggested ?? null;
    if (state.phase === 'answer' && item && isTyped(item) && suggested !== null) void rate(suggested);
  };

  /** "Also accept … next time": the typed answer joins the word's accepted answers. */
  const acceptTyped = async () => {
    const item = state.item;
    const check = state.check;
    if (!item || !check || check.typed === null || !canAddAccepted(check)) return;
    if (state.accept !== 'offer' && state.accept !== 'failed') return;
    state.accept = 'saving';
    renderBody();
    const answer = await acceptAnswer(item.word.id, check.typed);
    if (!isCurrent()) return;
    // Already on the list (added elsewhere meanwhile): just as good.
    const added = answer.ok || answer.code === 'item_exists';
    if (added) {
      acceptedItems.add(item.item_id);
      alsoAccepted.set(item.word.id, [...(alsoAccepted.get(item.word.id) ?? []), check.typed]);
    }
    // Still on that card: show the outcome there.
    if (state.item?.item_id !== item.item_id || state.phase === 'question' || state.phase === 'complete') return;
    state.accept = added ? 'saved' : 'failed';
    if (added) clearError();
    else showError(`“${check.typed}” was not added because ${reason(answer)}. Try again.`);
    renderBody();
  };

  const next = () => {
    if (!advanceReady()) return;
    if (state.phase === 'feedback') advance();
    else if (state.phase === 'complete') void end();
  };

  /** @param {boolean} fromToggle the toggle itself was used: keep focus on it */
  const toggleMore = (fromToggle) => {
    if (!state.item || !(state.phase === 'answer' || state.phase === 'feedback') || !hasMore(state.item)) return;
    state.moreOpen = !state.moreOpen;
    renderBody();
    if (fromToggle) {
      const toggle = body.querySelector('button.more-toggle');
      if (toggle instanceof HTMLElement) toggle.focus();
    }
  };

  const toggleHelp = () => {
    state.helpOpen = !state.helpOpen;
    renderTop();
    renderHelp();
    if (state.phase === 'answer') renderBody();
    renderBottom();
  };

  /**
   * Load the block again from the server (after a conflict or to find out
   * what was saved). Returns false when there is no block any more.
   */
  const refresh = async () => {
    const answer = await fetchCurrent();
    if (!isCurrent()) return false;
    if (!answer.ok) {
      showError(`The block could not be loaded because ${reason(answer)}. Try again in a moment.`);
      return false;
    }
    state.pending = null;
    state.unsure = null;
    if (!answer.data) {
      window.location.hash = '#/';
      return false;
    }
    state.session = answer.data;
    return true;
  };

  const reload = async () => {
    if (await refresh()) advance();
  };

  /**
   * The rating of item `itemId` stored on the server, if any.
   *
   * @param {number} itemId
   * @returns {Grade | null}
   */
  const storedGrade = (itemId) => {
    const grade = state.session?.items.find((i) => i.item_id === itemId)?.answered?.raw_grade ?? null;
    return grade !== null && isGrade(grade) ? grade : null;
  };

  /**
   * The rating was saved before (a lost answer or another window): say so
   * and offer Undo.
   *
   * @param {number} itemId
   */
  const showAlreadySaved = (itemId) => {
    clearDraft(itemId);
    const grade = storedGrade(itemId);
    const item = state.session?.items.find((i) => i.item_id === itemId) ?? null;
    if (!grade || !item) {
      advance();
      return;
    }
    clearError();
    toast.show({
      text: `Already saved as ${gradeName(grade)} — undo if that's wrong.`,
      grade,
      ms: ALREADY_SAVED_TOAST_MS,
      onUndo: () => void undo(),
    });
    afterRated(item, grade);
  };

  /** @param {PendingRating} pending */
  const submit = async (pending) => {
    const session = state.session;
    const item = state.item;
    if (!session || !item) return;
    const sinceReveal = now() - state.revealedAt;
    state.pending = pending;
    setBusy(true);
    const answer = await postRating(session.id, {
      clientReviewId: pending.id,
      itemId: pending.itemId,
      grade: pending.grade,
      durationMs: now() - state.shownAt,
      details: pending.details,
    });
    setBusy(false);
    if (!isCurrent()) return;
    if (!answer.ok) {
      if (lostAnswer(answer)) {
        // It may have been stored: keep the same id and grade for a retry.
        state.unsure = { ...pending, why: reason(answer) };
        clearError();
        renderBottom();
        announce(`Your rating ${gradeName(pending.grade)} may not have been saved because ${reason(answer)}.`);
        return;
      }
      if (answer.code === 'already_answered') {
        if (await refresh()) showAlreadySaved(pending.itemId);
        return;
      }
      if (answer.code === 'session_expired') {
        dayRolledOver();
        return;
      }
      if (answer.status === 409 || answer.status === 404) {
        showError('The block changed (perhaps in another window), so it was loaded again.');
        await reload();
        return;
      }
      showError(`The rating was not saved because ${reason(answer)}. Rate again.`);
      state.unsure = null;
      state.chosen = null;
      renderBottom();
      return;
    }
    clearError();
    clearDraft(pending.itemId);
    state.pending = null;
    state.unsure = null;
    state.chosen = null;
    state.session = answer.data.session;
    const plan = toastPlan({ grade: pending.grade, msSinceReveal: sinceReveal, lastFailed: item.card.last_failed || item.card.is_repeat });
    toast.show({ text: toastText(pending.grade), grade: pending.grade, ms: plan.ms, pulse: plan.pulse, onUndo: () => void undo() });
    afterRated(item, pending.grade);
  };

  /** @param {Grade} grade */
  const rate = async (grade) => {
    const item = state.item;
    const tapIgnored = ignoreRatingTap;
    ignoreRatingTap = false;
    if (state.busy || state.phase !== 'answer' || !state.session || !item) return;
    // Too soon after the reveal, or the second tap of a double tap on it.
    if (!ratingGuardOpen(now() - state.revealedAt) || (handlingKey === null && tapIgnored)) return;
    if (state.unsure) {
      // Only the unsure grade may be sent again until it is resolved.
      if (grade === state.unsure.grade) await retry();
      return;
    }
    // The same item rated the same way again (after a failed post) reuses
    // its id, so the server stores it at most once.
    // A Produce card's answer goes with its rating (and with any retry).
    const check = isTyped(item) ? state.check : null;
    /** @type {AnswerDetails | null} */
    const details = check
      ? { typedAnswer: check.typed, answerMatch: check.match, hintUsed: check.hintUsed, suggestedGrade: check.suggested }
      : null;
    const pending =
      state.pending && state.pending.itemId === item.item_id && state.pending.grade === grade
        ? state.pending
        : { itemId: item.item_id, grade, id: crypto.randomUUID(), details };
    // Acknowledge the tap at once (U3): the button shows pressed while saving.
    state.chosen = grade;
    for (const button of bottomContent.querySelectorAll('button.rate')) {
      button.classList.toggle('chosen', button.getAttribute('data-grade') === String(grade));
    }
    await submit(pending);
  };

  const retry = async () => {
    const unsure = state.unsure;
    if (state.busy || !unsure) return;
    await submit({ itemId: unsure.itemId, grade: unsure.grade, id: unsure.id, details: unsure.details });
  };

  /** Find out whether the unsure rating was saved before allowing another grade. */
  const rateDifferently = async () => {
    const unsure = state.unsure;
    if (state.busy || !unsure) return;
    setBusy(true);
    const answer = await fetchCurrent();
    setBusy(false);
    if (!isCurrent()) return;
    if (!answer.ok) {
      showError(`It is not known yet whether the rating was saved, because ${reason(answer)}. Try again in a moment.`);
      return;
    }
    state.pending = null;
    state.unsure = null;
    if (!answer.data) {
      window.location.hash = '#/';
      return;
    }
    state.session = answer.data;
    if (storedGrade(unsure.itemId)) {
      showAlreadySaved(unsure.itemId);
      return;
    }
    // Not saved: rate the same card again with any grade.
    clearError();
    const again = state.session.items.find((i) => i.item_id === unsure.itemId) ?? currentItem(state.session);
    if (!again) {
      advance();
      return;
    }
    reopen(again);
    render();
    loadClips();
    soundQuestion();
    announce('The rating was not saved. Rate the word again.');
  };

  const undo = async () => {
    const session = state.session;
    if (state.busy || !session || !session.can_undo) return;
    setBusy(true);
    const answer = await undoRating(session.id);
    setBusy(false);
    if (!isCurrent()) return;
    if (!answer.ok) {
      if (answer.code === 'nothing_to_undo') {
        showError('There is no rating to undo.');
        await reload();
      } else if (answer.code === 'session_expired') {
        dayRolledOver();
      } else if (answer.status === 409 || answer.status === 404) {
        showError('The block changed (perhaps in another window), so it was loaded again.');
        await reload();
      } else {
        showError(`Undo did not work because ${reason(answer)}. Try again.`);
      }
      return;
    }
    clearError();
    state.pending = null;
    state.unsure = null;
    state.session = answer.data.session;
    const item = state.session.items.find((i) => i.item_id === answer.data.item_id) ?? currentItem(state.session);
    state.item = item;
    if (item) {
      // Back on the answer side of the undone item: rate it again.
      reopen(item);
      state.shownAt = state.phaseAt;
    } else {
      setPhase('complete');
    }
    toast.show({ text: 'Rating undone', ms: 2500 });
    stopClip();
    render({ focus: true, scroll: true });
    loadClips();
    soundQuestion();
  };

  /** The study day rolled over: this block is gone; Today says why (bug 12). */
  const dayRolledOver = () => {
    setNotice('A new study day has started — your last rating was not saved.');
    window.location.replace('#/');
  };

  const end = async () => {
    const session = state.session;
    if (state.busy || !session) return;
    setBusy(true);
    const answer = await finishBlock(session.id);
    setBusy(false);
    if (!isCurrent()) return;
    if (!answer.ok) {
      if (answer.code === 'session_expired') {
        setNotice('A new study day has started, so the block was closed.');
        window.location.replace('#/');
        return;
      }
      showError(`The block could not be paused because ${reason(answer)}. Try again.`);
      return;
    }
    setSummary({ summary: answer.data, scope: session.scope_source_id, blockSize: state.settings.block_size });
    window.location.hash = '#/summary';
  };

  // ── keyboard ─────────────────────────────────────────────────────────────

  /** @param {Action} action */
  const perform = (action) => {
    if (typeof action === 'object') {
      if (isGrade(action.rate)) void rate(action.rate);
      return;
    }
    switch (action) {
      case 'reveal':
        reveal();
        break;
      case 'next':
        next();
        break;
      case 'undo':
        void undo();
        break;
      case 'more':
        toggleMore(false);
        break;
      case 'speak-sentence':
        speakSentence();
        break;
      case 'speak-word':
        speakWord();
        break;
      case 'detail':
        if (state.item && state.phase !== 'question') window.location.hash = `#/words/${state.item.word.id}`;
        break;
      case 'end':
        // Esc closes the help first; only then does it end the block.
        if (state.helpOpen) toggleHelp();
        else void end();
        break;
      case 'help':
        toggleHelp();
        break;
      case 'hint':
        showHint();
        break;
      case 'accept':
        acceptSuggested();
        break;
    }
  };

  /** @param {KeyboardEvent} event */
  const onKeyDown = (event) => {
    if (!state.session) return;
    const result = keyAction(event, state.phase, keyTarget(event.target));
    if (result.preventDefault) event.preventDefault();
    if (!result.action) return;
    handlingKey = event.key;
    try {
      perform(result.action);
    } finally {
      handlingKey = null;
    }
  };
  /** Space on a focused rating button must not click it on key-up either. */
  /** @param {KeyboardEvent} event */
  const onKeyUp = (event) => {
    if ((event.key === ' ' || event.key === 'Spacebar') && keyTarget(event.target) === 'rating') event.preventDefault();
  };
  document.addEventListener('keydown', onKeyDown);
  document.addEventListener('keyup', onKeyUp);

  /** @param {PointerEvent} event */
  const onPointerDown = (event) => {
    const tap = { x: event.clientX, y: event.clientY, t: now() };
    const onRating = event.target instanceof Element && event.target.closest('button.rate') !== null;
    if (onRating) ignoreRatingTap = state.phase === 'answer' && sameSpotTap(state.revealTap, tap);
    lastPointer = tap;
  };
  screen.addEventListener('pointerdown', onPointerDown, true);

  /**
   * In a block with Spelling cards, a tap or key press unlocks the media
   * element (iOS), so the next Spelling card can play its word by itself.
   */
  const onGesture = () => {
    if (state.session?.items.some((item) => isSpelling(item))) primeClips();
  };
  screen.addEventListener('click', onGesture, true);
  document.addEventListener('keydown', onGesture, true);

  // ── load ─────────────────────────────────────────────────────────────────

  /** @param {Session} session @param {number} finishedBlocks */
  const open = (session, finishedBlocks) => {
    state.legendAlways = legendVisible(state.settings.legend, finishedBlocks);
    state.session = session;
    mount(root, screen);
    releaseWakeLock = wakeLock();
    advance();
  };

  const load = async () => {
    // A block just started on Today or the summary is used as it is (U4).
    const started = takeStarted();
    if (started) {
      state.settings = await loadSettings();
      if (!isCurrent()) return;
      open(started.session, started.finishedBlocks);
      return;
    }
    const [current, today, settings] = await Promise.all([fetchCurrent(), fetchToday(null), loadSettings()]);
    if (!isCurrent()) return;
    state.settings = settings;
    if (!current.ok) {
      const retry = h('button', { type: 'button', 'data-testid': 'try-again' }, 'Try again');
      retry.addEventListener('click', () => {
        mount(root, h('p', { class: 'muted' }, 'Loading…'));
        void load();
      });
      mount(
        root,
        h('p', { class: 'error' }, `The block could not be loaded because ${reason(current)}.`),
        h('div', { class: 'actions' }, retry, h('a', { class: 'button', href: '#/' }, 'Back to Today')),
      );
      window.addEventListener('online', () => {
        if (isCurrent() && !state.session) void load();
      }, { once: true });
      return;
    }
    if (!current.data) {
      // No block in progress: Today is where one starts (U7).
      window.location.replace('#/');
      return;
    }
    open(current.data, today.ok ? today.data.finished_blocks.total : 0);
  };
  void load();

  return () => {
    document.removeEventListener('keydown', onKeyDown);
    document.removeEventListener('keyup', onKeyUp);
    viewport?.removeEventListener('resize', onViewport);
    viewport?.removeEventListener('scroll', onViewport);
    screen.removeEventListener('pointerdown', onPointerDown, true);
    screen.removeEventListener('click', onGesture, true);
    document.removeEventListener('keydown', onGesture, true);
    toast.clearTimer();
    if (savingTimer !== null) clearTimeout(savingTimer);
    if (guardTimer !== null) clearTimeout(guardTimer);
    stopSpeaking();
    stopClip();
    if (releaseWakeLock) releaseWakeLock();
  };
}
