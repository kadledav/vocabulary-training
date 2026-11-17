// @ts-check
/**
 * Practice — Understand (design §7.1 screens 2/2b, §7.2–§7.5; research
 * wireframes 2, 3, 2b; R-52, R-53, R-54, R-60, R-62, R-70).
 *
 * Question side → reveal → answer side → rate 1–5. Every rating is posted
 * at once (idempotent by its client UUID); a 4/5 keeps the answer on screen
 * with "More" open until Next. The toast, ↶ and `U` undo the last rating.
 * When every item is rated, "See summary" finishes the block.
 *
 * Every phase change opens the 250 ms input guard again, for the reveal and
 * Next buttons as well as the ratings, so a double tap never acts on the
 * card that appears under the finger. A rating whose answer was lost
 * (network error, timeout) is never silently re-sent with another grade:
 * the owner retries the same rating or first finds out what was saved.
 */

import { h, mount } from '../dom.js';
import { keyAction, keyTarget } from '../keys.js';
import { answerView, criterion, questionView, revealLabel } from '../practice/card.js';
import {
  afterRating,
  currentItem,
  exampleSentence,
  hasMore,
  moreOpenOnReveal,
  plainText,
  progress,
  wordSpeech,
} from '../practice/model.js';
import { legend, ratingBar } from '../practice/rating-bar.js';
import { INPUT_GUARD_MS, guardOpen, isGrade, legendVisible, rating, toastPlan, toastText } from '../rating.js';
import { fetchCurrent, fetchToday, finishBlock, postRating, undoRating } from '../session.js';
import { DEFAULT_SETTINGS, loadSettings } from '../settings.js';
import { speak, speechAvailable, stopSpeaking } from '../speech.js';
import { setSummary } from '../summary-store.js';
import { Toast } from '../toast.js';

/**
 * @typedef {import('../session.js').Session} Session
 * @typedef {import('../session.js').Item} Item
 * @typedef {import('../keys.js').Phase} Phase
 * @typedef {import('../keys.js').Action} Action
 * @typedef {import('../rating.js').Grade} Grade
 * @typedef {{ itemId: number, grade: Grade, id: string }} PendingRating
 * @typedef {{ ok: false, status: number, code: string, message: string }} Failure
 */

/** Keyboard shortcuts listed in the help panel (design §7.3). */
const SHORTCUTS = [
  ['Space / Enter', 'show the answer · next'],
  ['1 – 5', 'rate (1 = best)'],
  ['U', 'undo the last rating'],
  ['M', 'more: note, forms, sentences'],
  ['S / R', 'speak sentence / word'],
  ['E', 'word detail'],
  ['Esc', 'close the help · end the block'],
  ['?', 'this help'],
];

/** "Saving…" appears only when a request takes longer than this. */
const SAVING_DELAY_MS = 150;
/** The "already saved" toast stays longer: it asks for a check. */
const ALREADY_SAVED_TOAST_MS = 8000;

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
   *           settings: import('../settings.js').Settings }} */
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
  };
  const speechOk = speechAvailable();

  const top = h('div', { class: 'practice-top' });
  const help = h('div', { class: 'practice-help' });
  const body = h('section', { class: 'practice-body', tabindex: '-1', 'aria-label': 'Card' });
  const toastSlot = h('div', { class: 'toast-slot' });
  const saving = h('p', { class: 'saving', 'data-testid': 'saving' });
  const bottomContent = h('div', { class: 'bottom-content' });
  const bottom = h('div', { class: 'practice-bottom' }, toastSlot, saving, bottomContent);
  const live = h('p', { class: 'sr-only', role: 'status', 'aria-live': 'polite' });
  const errorLine = h('p', { class: 'error practice-error', role: 'alert', 'data-testid': 'practice-error' });
  const toast = new Toast(toastSlot);
  const screen = h('div', { class: 'practice' }, top, help, errorLine, body, bottom, live);
  /** @type {(() => void) | null} */
  let releaseWakeLock = null;

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

  const speakSentence = () => {
    const item = state.item;
    if (!item || state.phase === 'complete') return;
    const sentence =
      item.card.format === 'sentence' && item.sentence ? item.sentence : state.phase === 'question' ? null : exampleSentence(item);
    if (!sentence) return;
    if (!speak(plainText(sentence.en), { voice: state.settings.voice, speed: state.settings.speed })) {
      announce('Speech is not available in this browser.');
    }
  };
  const speakWord = () => {
    const item = state.item;
    if (!item || state.phase === 'complete') return;
    // On the question side of a sentence card only the sentence plays.
    if (state.phase === 'question' && item.card.format === 'sentence' && item.sentence) return;
    if (!speak(wordSpeech(item.word), { voice: state.settings.voice, speed: state.settings.speed })) {
      announce('Speech is not available in this browser.');
    }
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
      h('button', { type: 'button', class: 'icon-button', 'aria-label': 'End block (Esc)', title: 'End block (Esc)', onclick: () => void end() }, '✕'),
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
      mount(body, questionView(item, audio));
      return;
    }
    mount(
      body,
      answerView(item, {
        audio,
        moreOpen: state.moreOpen,
        showMore: hasMore(item),
        onToggleMore: () => toggleMore(true),
      }),
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

  const drawBottom = () => {
    const item = state.item;
    if (state.phase === 'question' && item) {
      mount(
        bottomContent,
        h('p', { class: 'criterion' }, criterion(item)),
        h('button', { type: 'button', class: 'primary big guarded', 'data-guarded': true, 'data-testid': 'reveal', onclick: () => reveal() }, revealLabel(item)),
      );
    } else if (state.phase === 'answer' && state.unsure) {
      mount(bottomContent, unsurePanel());
    } else if (state.phase === 'answer') {
      mount(
        bottomContent,
        state.legendAlways && !state.helpOpen ? legend() : null,
        ratingBar((grade) => void rate(grade)),
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
    if (scroll) window.scrollTo(0, 0);
    if (focus) body.focus({ preventScroll: true });
  };

  // ── transitions ──────────────────────────────────────────────────────────

  /** Show the next unanswered item (or the "complete" state). */
  const advance = () => {
    const session = state.session;
    if (!session) return;
    state.item = currentItem(session);
    setPhase(state.item ? 'question' : 'complete');
    state.moreOpen = false;
    state.shownAt = now();
    render({ focus: true, scroll: true });
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
      setPhase('feedback');
      state.moreOpen = hasMore(item);
      render({ focus: true });
    } else {
      advance();
    }
  };

  const reveal = () => {
    if (state.phase !== 'question' || !state.item || !advanceReady()) return;
    setPhase('answer');
    state.revealedAt = state.phaseAt;
    state.moreOpen = moreOpenOnReveal(state.item);
    render({ focus: true });
    const word = state.item.word;
    announce(h('span', { lang: 'en' }, word.headword), ': ', h('span', { lang: 'cs' }, word.czech.join(', ')));
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
      if (answer.status === 409 || answer.status === 404) {
        showError('The block changed (perhaps in another window), so it was loaded again.');
        await reload();
        return;
      }
      showError(`The rating was not saved because ${reason(answer)}. Rate again.`);
      state.unsure = null;
      renderBottom();
      return;
    }
    clearError();
    state.pending = null;
    state.unsure = null;
    state.session = answer.data.session;
    const plan = toastPlan({ grade: pending.grade, msSinceReveal: sinceReveal, lastFailed: item.card.last_failed || item.card.is_repeat });
    toast.show({ text: toastText(pending.grade), grade: pending.grade, ms: plan.ms, pulse: plan.pulse, onUndo: () => void undo() });
    afterRated(item, pending.grade);
  };

  /** @param {Grade} grade */
  const rate = async (grade) => {
    const item = state.item;
    if (state.busy || state.phase !== 'answer' || !state.session || !item || !inputReady()) return;
    if (state.unsure) {
      // Only the unsure grade may be sent again until it is resolved.
      if (grade === state.unsure.grade) await retry();
      return;
    }
    // The same item rated the same way again (after a failed post) reuses
    // its id, so the server stores it at most once.
    const pending =
      state.pending && state.pending.itemId === item.item_id && state.pending.grade === grade
        ? state.pending
        : { itemId: item.item_id, grade, id: crypto.randomUUID() };
    await submit(pending);
  };

  const retry = async () => {
    const unsure = state.unsure;
    if (state.busy || !unsure) return;
    await submit({ itemId: unsure.itemId, grade: unsure.grade, id: unsure.id });
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
    state.item = state.session.items.find((i) => i.item_id === unsure.itemId) ?? currentItem(state.session);
    if (!state.item) {
      advance();
      return;
    }
    setPhase('answer');
    render();
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
      setPhase('answer');
      state.revealedAt = state.phaseAt;
      state.shownAt = state.phaseAt;
      state.moreOpen = moreOpenOnReveal(item);
    } else {
      setPhase('complete');
    }
    toast.show({ text: 'Rating undone', ms: 2500 });
    render({ focus: true, scroll: true });
  };

  const end = async () => {
    const session = state.session;
    if (state.busy || !session) return;
    setBusy(true);
    const answer = await finishBlock(session.id);
    setBusy(false);
    if (!isCurrent()) return;
    if (!answer.ok) {
      showError(`The block could not be ended because ${reason(answer)}. Try again.`);
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

  // ── load ─────────────────────────────────────────────────────────────────

  void (async () => {
    const [current, today, settings] = await Promise.all([fetchCurrent(), fetchToday(null), loadSettings()]);
    if (!isCurrent()) return;
    state.settings = settings;
    if (!current.ok) {
      mount(
        root,
        h('p', { class: 'error' }, `The block could not be loaded because ${reason(current)}.`),
        h('p', {}, h('a', { href: '#/' }, 'Back to Today')),
      );
      return;
    }
    if (!current.data) {
      mount(
        root,
        h('h1', {}, 'No block in progress'),
        h('p', {}, h('a', { href: '#/' }, 'Start one on the Today screen'), '.'),
      );
      return;
    }
    state.legendAlways = legendVisible(settings.legend, today.ok ? today.data.finished_blocks.total : 0);
    state.session = current.data;
    mount(root, screen);
    releaseWakeLock = wakeLock();
    advance();
  })();

  return () => {
    document.removeEventListener('keydown', onKeyDown);
    document.removeEventListener('keyup', onKeyUp);
    toast.clearTimer();
    if (savingTimer !== null) clearTimeout(savingTimer);
    if (guardTimer !== null) clearTimeout(guardTimer);
    stopSpeaking();
    if (releaseWakeLock) releaseWakeLock();
  };
}
