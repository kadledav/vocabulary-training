// @ts-check
/**
 * DOM of a practice item. Understand (research wireframes 2, 3, 2b): the
 * question side (sentence with the word highlighted, or the word alone)
 * and the answer side (Czech sentence, word strip, gloss, meaning,
 * "More"). Produce (wireframes 4, 4b; R-56): the sentence with the word
 * blanked and the Czech sentence as the cue — no audio, it would say the
 * answer — and on the answer side the check result above the same answer
 * view. Spelling (R-58): the word's audio with a big ▶ Listen and the
 * Czech gloss as the cue — nothing that shows how the word is written —
 * and on the answer side a letter-by-letter diff and the spelling note.
 * All pack text is inserted as text (R-84) with its language marked.
 */

import { h } from '../dom.js';
import { markedText } from '../marked.js';
import { displayIcon } from '../icon.js';
import { caption, exampleSentence } from './model.js';
import { diffSummary, expectedAnswer, spellingDiff } from '../grading.js';

/**
 * @typedef {import('../session.js').Item} Item
 * @typedef {import('../session.js').Sentence} Sentence
 * @typedef {{ speechOk: boolean, onSpeakSentence: () => void, onSpeakWord: () => void }} Audio
 * @typedef {import('../grading.js').Check} Check
 * @typedef {'offer' | 'saving' | 'saved' | 'failed'} AcceptState
 * @typedef {import('../grading.js').DiffColumn} DiffColumn
 * @typedef {'waiting' | 'playing' | 'heard' | 'tap' | 'none'} ListenState
 *   The Spelling prompt: its sound is on its way or starting by itself; it
 *   played; it could not start by itself (tap ▶); this device has no sound
 *   for the word.
 */

/** @type {Record<string, string>} */
const MODE_NAMES = { understand: 'Understand', produce: 'Produce', spelling: 'Spelling' };

/**
 * A ▶ control (browser speech, started only by a tap: R-41).
 *
 * @param {'sentence' | 'word'} what
 * @param {Audio} audio
 */
function playButton(what, audio) {
  const label = what === 'sentence' ? 'Play sentence' : 'Play word';
  return h(
    'button',
    {
      type: 'button',
      class: `play play-${what}`,
      'aria-label': audio.speechOk ? label : `${label} (speech is not available in this browser)`,
      title: audio.speechOk ? `${label} (${what === 'sentence' ? 'S' : 'R'})` : 'Speech is not available in this browser',
      disabled: !audio.speechOk,
      onclick: what === 'sentence' ? audio.onSpeakSentence : audio.onSpeakWord,
    },
    h('span', { 'aria-hidden': 'true' }, '▶'),
  );
}

/** @param {Item} item */
function modeLine(item) {
  return h(
    'p',
    { class: 'mode' },
    h('span', { class: 'mode-name', 'data-testid': 'mode' }, MODE_NAMES[item.card.kind] ?? 'Understand'),
    item.card.is_new ? h('span', { class: 'badge badge-new' }, 'New') : null,
    item.card.is_repeat ? h('span', { class: 'badge' }, 'Again') : null,
  );
}

/**
 * @param {Sentence} sentence
 * @param {Audio} audio
 * @param {boolean} small
 */
function englishSentence(sentence, audio, small) {
  const text = caption(sentence);
  return h(
    'div',
    { class: `sentence-block${small ? ' small' : ''}` },
    h(
      'div',
      { class: 'sentence-row' },
      h('p', { class: 'sentence', 'data-testid': 'sentence-en' }, markedText(sentence.en, 'en')),
      playButton('sentence', audio),
    ),
    text ? h('p', { class: 'caption' }, `— ${text}`) : null,
  );
}

/** @param {Sentence} sentence */
function czechSentence(sentence) {
  return h(
    'div',
    { class: 'cz-box', 'data-testid': 'sentence-cs' },
    h('span', { class: 'cz-tag', 'aria-hidden': 'true' }, 'CZ'),
    h('p', { class: 'sentence-cs' }, markedText(sentence.cs, 'cs')),
  );
}

/**
 * Headword, respelling, ▶ word, Czech gloss, one-line meaning.
 *
 * @param {Item} item
 * @param {Audio} audio
 */
function wordStrip(item, audio) {
  const w = item.word;
  const icon = displayIcon(w.icon);
  return h(
    'div',
    { class: 'word-strip' },
    h(
      'div',
      { class: 'word-line' },
      icon ? h('span', { class: 'word-icon', 'aria-hidden': 'true' }, icon) : null,
      h('span', { class: 'word-headword', lang: 'en', 'data-testid': 'headword' }, w.headword),
      w.pronunciation ? h('span', { class: 'word-pron' }, w.pronunciation) : null,
      playButton('word', audio),
    ),
    w.czech.length > 0 ? h('p', { class: 'word-gloss', lang: 'cs' }, w.czech.join(', ')) : null,
    h('p', { class: 'word-meaning', lang: 'en' }, w.meaning),
  );
}

/**
 * The English sentence with the target blanked (the word is the answer).
 *
 * @param {Sentence} sentence
 */
function clozeSentence(sentence) {
  const text = caption(sentence);
  return h(
    'div',
    { class: 'sentence-block' },
    h(
      'p',
      { class: 'sentence cloze', lang: 'en', 'data-testid': 'cloze' },
      sentence.en.pre,
      h('span', { class: 'blank', role: 'img', 'aria-label': 'blank' }, '\u2009'),
      sentence.en.post,
    ),
    text ? h('p', { class: 'caption' }, `— ${text}`) : null,
  );
}

/**
 * The question side of a Produce card: the blanked sentence and the Czech
 * sentence as the cue (the matching Czech words highlighted); without a
 * sentence, the Czech gloss.
 *
 * @param {Item} item
 */
export function produceQuestion(item) {
  const sentence = item.sentence;
  return h(
    'div',
    { class: 'item item-question produce' },
    modeLine(item),
    sentence ? clozeSentence(sentence) : null,
    sentence
      ? czechSentence(sentence)
      : h(
          'div',
          { class: 'cz-box', 'data-testid': 'gloss-cue' },
          h('span', { class: 'cz-tag', 'aria-hidden': 'true' }, 'CZ'),
          h('p', { class: 'sentence-cs', lang: 'cs' }, item.word.czech.join(', ')),
        ),
  );
}

/** What the line under the Spelling prompt's ▶ says. */
/** @type {Record<ListenState, string>} */
const LISTEN_TEXT = {
  waiting: 'Getting the sound…',
  playing: 'Listen…',
  heard: 'Type the word you heard. Replay it as often as you like.',
  tap: 'Tap ▶ Listen to hear the word.',
  none: 'No sound for this word on this device: spell it from the Czech below.',
};

/**
 * The question side of a Spelling card (design §7.1 screen 3b): ▶ Listen
 * (it plays by itself where the browser allows it; when it cannot, the
 * button is marked and says to tap it) and the Czech gloss as the cue for
 * which word is meant. Nothing shows how the word is written: no headword,
 * no sentence, no pronunciation respelling, no meaning.
 *
 * @param {Item} item
 * @param {{ onListen: () => void, listen: ListenState }} prompt
 */
export function spellingQuestion(item, { onListen, listen }) {
  return h(
    'div',
    { class: 'item item-question spelling' },
    modeLine(item),
    h(
      'div',
      { class: 'listen' },
      h(
        'button',
        {
          type: 'button',
          class: `play play-word listen-big${listen === 'tap' ? ' needs-tap' : ''}`,
          'aria-label': 'Listen to the word (R)',
          title: 'Listen to the word (R)',
          'data-testid': 'listen',
          onclick: onListen,
        },
        h('span', { 'aria-hidden': 'true' }, '▶'),
        ' Listen',
      ),
      h('p', { class: `listen-status${listen === 'tap' || listen === 'none' ? ' attention' : ''}`, role: 'status', 'data-testid': 'listen-status' }, LISTEN_TEXT[listen]),
    ),
    item.word.czech.length > 0
      ? h(
          'div',
          { class: 'cz-box', 'data-testid': 'gloss-cue' },
          h('span', { class: 'cz-tag', 'aria-hidden': 'true' }, 'CZ'),
          h('p', { class: 'sentence-cs', lang: 'cs' }, item.word.czech.join(', ')),
        )
      : null,
  );
}

/**
 * How a letter shows in the diff: a space as ␣, a gap as ·, and anything
 * else that would show as nothing (other white space, a format character
 * the comparison keeps) as �, never as an empty cell.
 *
 * @param {string | null} ch
 */
function diffLetter(ch) {
  if (ch === null) return '·';
  if (ch === ' ') return '␣';
  return /^[\p{Z}\p{C}]$/u.test(ch) ? '\ufffd' : ch;
}

/** The symbol under a column: not colour alone (design §6.8). */
/** @type {Record<import('../grading.js').DiffMark, string>} */
const DIFF_SYMBOL = { same: '\u00a0', wrong: '×', missing: '+', extra: '−' };

/**
 * The letter-by-letter diff (design §6.8): one column per letter, what was
 * typed above the word's spelling, a symbol under every difference and
 * colour on top. The grid is for the eyes; a screen reader gets the same in
 * words.
 *
 * @param {readonly DiffColumn[]} diff
 */
export function spellingDiffView(diff) {
  return h(
    'div',
    { class: 'spell-diff-box' },
    h(
      'div',
      { class: 'spell-diff', 'aria-hidden': 'true', 'data-testid': 'spell-diff' },
      h('span', { class: 'diff-col diff-labels' }, h('span', {}, 'typed'), h('span', {}, 'word'), h('span', {}, '\u00a0')),
      diff.map((column) =>
        h(
          'span',
          { class: `diff-col diff-${column.mark}`, 'data-mark': column.mark },
          h('span', { class: 'diff-typed mono', lang: 'en' }, diffLetter(column.typed)),
          h('span', { class: 'diff-correct mono', lang: 'en' }, diffLetter(column.correct)),
          h('span', { class: 'diff-mark' }, DIFF_SYMBOL[column.mark]),
        ),
      ),
    ),
    h('p', { class: 'sr-only' }, diffSummary(diff)),
    h('p', { class: 'diff-legend muted small', 'aria-hidden': 'true' }, '× wrong letter · + missing · − extra'),
  );
}

/**
 * The answer of a Spelling card: what the strict check found — ✓ Correct
 * spelling, or ✗ Not quite with the letter diff and "Just a typo? Choose
 * 3." — or the spelling after "I don't know" / "I'll just rate myself",
 * then the pack's spelling note (the trap).
 *
 * @param {Item} item
 * @param {Check} check
 */
export function spellingResult(item, check) {
  const word = item.word.headword;
  const note = item.word.spelling_note
    ? h('p', { class: 'spelling-note', lang: 'en', 'data-testid': 'spelling-note' }, h('strong', {}, 'Trap: '), item.word.spelling_note)
    : null;
  const spelled = h('p', { class: 'spelled mono', lang: 'en', 'data-testid': 'expected' }, word);
  if (check.match === 'exact') {
    return h(
      'div',
      { class: 'check check-ok', 'data-testid': 'check-result', 'data-match': 'exact' },
      h('p', { class: 'check-title' }, h('span', { 'aria-hidden': 'true' }, '✓ '), 'Correct spelling'),
      spelled,
      note,
    );
  }
  if (check.match === 'none') {
    return h(
      'div',
      { class: 'check check-miss', 'data-testid': 'check-result', 'data-match': 'none' },
      h('p', { class: 'check-title' }, h('span', { 'aria-hidden': 'true' }, '✗ '), 'Not quite'),
      spellingDiffView(spellingDiff(check.typed ?? '', word)),
      h('p', { class: 'muted small' }, 'Just a typo? Choose 3.'),
      note,
    );
  }
  return h(
    'div',
    { class: 'check', 'data-testid': 'check-result', 'data-match': check.match },
    h('p', { class: 'check-title' }, 'The spelling:'),
    spelled,
    note,
  );
}

/**
 * The hint line (first letter and letter count), shown right above the
 * answer field so the on-screen keyboard never hides it.
 *
 * @param {{ pattern: string, letters: number }} hint
 */
export function typedHint(hint) {
  return h(
    'p',
    { class: 'typed-hint', 'data-testid': 'hint' },
    h('span', { class: 'muted' }, 'Hint: '),
    h('span', { class: 'mono', lang: 'en' }, hint.pattern),
    h('span', { class: 'muted' }, ` (${hint.letters} ${hint.letters === 1 ? 'letter' : 'letters'})`),
  );
}

/**
 * What the check found, above the answer of a typed card (wireframe 4b):
 * ✓ Correct; "Right word — here it is …" for another form of the word
 * (and "Accepted — this sentence has …" for another accepted answer)
 * while a sentence is shown — friendly, never an error; ✗ Not quite with the typed answer right above the expected
 * one in the same monospace layout (a one-letter slip is easy to see) and
 * "Just a typo? Choose 3."; the answer after "I don't know"; nothing
 * after "I'll just rate myself". A missed typed answer can be added to the
 * accepted answers from here.
 *
 * @param {Item} item
 * @param {Check} check
 * @param {{ state: AcceptState | null, onAccept: () => void }} accept
 */
export function checkResult(item, check, accept) {
  const answer = expectedAnswer(item);
  if (check.match === 'skipped') return null;
  if (check.verdict === 'right_word' || check.verdict === 'alternative') {
    return h(
      'div',
      { class: 'check check-near', 'data-testid': 'check-result', 'data-match': check.verdict },
      h(
        'p',
        { class: 'check-title' },
        check.verdict === 'right_word' ? 'Right word — here it is ' : 'Accepted — this sentence has ',
        h('span', { class: 'mono', lang: 'en', 'data-testid': 'expected' }, `“${answer}”`),
      ),
    );
  }
  if (check.match === 'exact') {
    return h(
      'div',
      { class: 'check check-ok', 'data-testid': 'check-result', 'data-match': 'exact' },
      h('p', { class: 'check-title' }, h('span', { 'aria-hidden': 'true' }, '✓ '), 'Correct', check.hintUsed ? ' (with the hint)' : ''),
    );
  }
  if (check.match === 'dont_know') {
    return h(
      'div',
      { class: 'check check-miss', 'data-testid': 'check-result', 'data-match': 'dont_know' },
      h('p', { class: 'check-title' }, 'The answer: ', h('span', { class: 'mono', lang: 'en' }, answer)),
    );
  }
  /** @type {Node | null} */
  let acceptLine = null;
  if (accept.state === 'offer' || accept.state === 'failed') {
    acceptLine = h(
      'button',
      { type: 'button', class: 'accept-typed', 'data-testid': 'accept-typed', onclick: accept.onAccept },
      `Also accept “${check.typed}” next time`,
    );
  } else if (accept.state === 'saving') {
    acceptLine = h('p', { class: 'muted small', role: 'status' }, 'Adding…');
  } else if (accept.state === 'saved') {
    acceptLine = h('p', { class: 'muted small', role: 'status', 'data-testid': 'accepted-note' }, `“${check.typed}” will be accepted next time.`);
  }
  return h(
    'div',
    { class: 'check check-miss', 'data-testid': 'check-result', 'data-match': 'none' },
    h('p', { class: 'check-title' }, h('span', { 'aria-hidden': 'true' }, '✗ '), 'Not quite'),
    h(
      'dl',
      { class: 'typed-vs' },
      h('dt', {}, 'you typed:'),
      h('dd', { class: 'mono', lang: 'en', 'data-testid': 'typed' }, check.typed ?? ''),
      h('dt', {}, 'answer:'),
      h('dd', { class: 'mono', lang: 'en', 'data-testid': 'expected' }, answer),
    ),
    h('p', { class: 'muted small' }, 'Just a typo? Choose 3.'),
    acceptLine,
  );
}

/**
 * The question side.
 *
 * @param {Item} item
 * @param {Audio} audio
 */
export function questionView(item, audio) {
  if (item.card.format === 'sentence' && item.sentence) {
    return h('div', { class: 'item item-question' }, modeLine(item), englishSentence(item.sentence, audio, false));
  }
  return h(
    'div',
    { class: 'item item-question word-only' },
    modeLine(item),
    h(
      'div',
      { class: 'word-alone' },
      h('p', { class: 'word-big', lang: 'en', 'data-testid': 'headword-question' }, item.word.headword),
      playButton('word', audio),
    ),
  );
}

/**
 * The criterion printed above the reveal button (research §10d).
 *
 * @param {Item} item
 */
export function criterion(item) {
  if (item.card.kind === 'spelling') return 'Type the word you hear: every letter counts.';
  if (item.card.kind === 'produce') return 'Type the English word for the blank: the Czech sentence is your cue.';
  return item.card.format === 'sentence' && item.sentence
    ? 'Rate the highlighted word: would you have understood it here, without help?'
    : 'Rate the word: would you have known what it means, without help?';
}

/**
 * The text of the reveal button.
 *
 * @param {Item} item
 */
export function revealLabel(item) {
  return item.card.format === 'sentence' && item.sentence ? 'Show translation' : 'Show answer';
}

/**
 * "More": hint, note, grammar, forms, the word's other sentences, tag.
 *
 * @param {Item} item
 * @param {Sentence | null} shown the sentence already on screen
 */
function moreContent(item, shown) {
  const w = item.word;
  const others = item.other_sentences.filter((s) => !shown || s.id !== shown.id);
  return h(
    'div',
    { class: 'more', id: 'more-content', 'data-testid': 'more' },
    w.hint ? h('p', { class: 'hint', lang: 'en' }, h('strong', {}, 'Hint: '), w.hint) : null,
    w.note ? h('p', { class: 'note-text', lang: 'en' }, w.note) : null,
    w.grammar ? h('p', { class: 'grammar muted' }, w.grammar) : null,
    w.forms.length > 0
      ? h(
          'p',
          { class: 'forms', lang: 'en' },
          h('span', { class: 'muted' }, 'Forms: '),
          w.forms.map((f, i) =>
            h(
              'span',
              { class: f.common ? 'form' : 'form rare' },
              i > 0 ? ', ' : '',
              f.form,
              f.common ? '' : ' (rare)',
            ),
          ),
        )
      : null,
    others.length > 0
      ? h(
          'div',
          { class: 'other-sentences' },
          h('p', { class: 'muted' }, 'Other sentences'),
          h(
            'ul',
            {},
            others.map((s) =>
              h(
                'li',
                {},
                h('p', {}, markedText(s.en, 'en')),
                h('p', { class: 'muted' }, markedText(s.cs, 'cs')),
                caption(s) ? h('p', { class: 'caption' }, `— ${caption(s)}`) : null,
              ),
            ),
          ),
        )
      : null,
    w.tag?.text ? h('p', { class: 'muted small' }, w.tag.text) : null,
  );
}

/**
 * The answer side.
 *
 * @param {Item} item
 * @param {{ audio: Audio, moreOpen: boolean, onToggleMore: () => void, showMore: boolean, lead?: Node | null }} options
 *   `lead`: shown first (a typed card's check result)
 */
export function answerView(item, { audio, moreOpen, onToggleMore, showMore, lead = null }) {
  // A Produce card's answer shows its sentence like a sentence card.
  const sentenceFormat = (item.card.format === 'sentence' || item.card.format === 'cloze') && item.sentence !== null;
  const example = sentenceFormat ? item.sentence : exampleSentence(item);
  const toggle = showMore
    ? h(
        'button',
        {
          type: 'button',
          class: 'more-toggle',
          'aria-expanded': moreOpen ? 'true' : 'false',
          'aria-controls': 'more-content',
          onclick: onToggleMore,
        },
        moreOpen ? '▾ Less' : '▸ More: note · forms',
      )
    : null;
  return h(
    'div',
    { class: `item item-answer${sentenceFormat ? '' : ' word-only'}` },
    modeLine(item),
    lead,
    sentenceFormat && example ? englishSentence(example, audio, true) : null,
    sentenceFormat && example ? czechSentence(example) : null,
    wordStrip(item, audio),
    !sentenceFormat && example
      ? h('div', { class: 'example' }, englishSentence(example, audio, true), czechSentence(example))
      : null,
    toggle,
    showMore && moreOpen ? moreContent(item, example) : null,
  );
}
