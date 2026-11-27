// @ts-check
/**
 * DOM of a practice item. Understand (research wireframes 2, 3, 2b): the
 * question side (sentence with the word highlighted, or the word alone)
 * and the answer side (Czech sentence, word strip, gloss, meaning,
 * "More"). Produce (wireframes 4, 4b; R-56): the sentence with the word
 * blanked and the Czech sentence as the cue — no audio, it would say the
 * answer — and on the answer side the check result above the same answer
 * view. All pack text is inserted as text (R-84) with its language marked.
 */

import { h } from '../dom.js';
import { markedText } from '../marked.js';
import { displayIcon } from '../icon.js';
import { caption, exampleSentence } from './model.js';
import { expectedAnswer } from '../grading.js';

/**
 * @typedef {import('../session.js').Item} Item
 * @typedef {import('../session.js').Sentence} Sentence
 * @typedef {{ speechOk: boolean, onSpeakSentence: () => void, onSpeakWord: () => void }} Audio
 * @typedef {import('../grading.js').Check} Check
 * @typedef {'offer' | 'saving' | 'saved' | 'failed'} AcceptState
 */

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
    h('span', { class: 'mode-name', 'data-testid': 'mode' }, item.card.kind === 'produce' ? 'Produce' : 'Understand'),
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
 * sentence, the Czech gloss. The hint (first letter and letter count)
 * shows once asked for.
 *
 * @param {Item} item
 * @param {{ pattern: string, letters: number } | null} hint
 */
export function produceQuestion(item, hint) {
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
    hint
      ? h(
          'p',
          { class: 'typed-hint', 'data-testid': 'hint' },
          h('span', { class: 'muted' }, 'Hint: '),
          h('span', { class: 'mono', lang: 'en' }, hint.pattern),
          h('span', { class: 'muted' }, ` (${hint.letters} ${hint.letters === 1 ? 'letter' : 'letters'})`),
        )
      : null,
  );
}

/**
 * What the check found, above the answer of a typed card (wireframe 4b):
 * ✓ Correct; ✗ Not quite with the typed answer right above the expected
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
