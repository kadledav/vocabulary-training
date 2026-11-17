// @ts-check
/**
 * DOM of an Understand item (research wireframes 2, 3, 2b): the question
 * side (sentence with the word highlighted, or the word alone) and the
 * answer side (Czech sentence, word strip, gloss, meaning, "More"). All
 * pack text is inserted as text (R-84) with its language marked.
 */

import { h } from '../dom.js';
import { markedText } from '../marked.js';
import { displayIcon } from '../icon.js';
import { caption, exampleSentence } from './model.js';

/**
 * @typedef {import('../session.js').Item} Item
 * @typedef {import('../session.js').Sentence} Sentence
 * @typedef {{ speechOk: boolean, onSpeakSentence: () => void, onSpeakWord: () => void }} Audio
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
    h('span', { class: 'mode-name' }, 'Understand'),
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
 * @param {{ audio: Audio, moreOpen: boolean, onToggleMore: () => void, showMore: boolean }} options
 */
export function answerView(item, { audio, moreOpen, onToggleMore, showMore }) {
  const sentenceFormat = item.card.format === 'sentence' && item.sentence !== null;
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
