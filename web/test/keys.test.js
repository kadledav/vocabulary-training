// @ts-check
/** Keyboard map of the practice screen (R-70; design §7.3). */

import assert from 'node:assert/strict';
import { test } from 'node:test';

import { keyAction, keyTarget } from '../js/keys.js';

/**
 * @param {string} key
 * @param {import('../js/keys.js').Phase} phase
 * @param {import('../js/keys.js').KeyTarget} [target]
 * @param {Partial<import('../js/keys.js').KeyPress>} [extra]
 */
const act = (key, phase, target = 'page', extra = {}) => keyAction({ key, ...extra }, phase, target).action;

test('R-70 Space and Enter reveal the answer', () => {
  assert.equal(act(' ', 'question'), 'reveal');
  assert.equal(act('Enter', 'question'), 'reveal');
});

test('R-70 digits 1-5 rate with the number shown on screen, only on the answer side', () => {
  for (const n of [1, 2, 3, 4, 5]) assert.deepEqual(act(String(n), 'answer'), { rate: n });
  assert.equal(act('1', 'question'), null);
  assert.equal(act('2', 'feedback'), null);
  assert.equal(act('6', 'answer'), null);
  assert.equal(act('0', 'answer'), null);
});

test('R-62 Space never rates, not even on a focused rating button', () => {
  assert.equal(act(' ', 'answer'), null);
  const onButton = keyAction({ key: ' ' }, 'answer', 'rating');
  assert.equal(onButton.action, null);
  assert.equal(onButton.preventDefault, true, 'the button must not be clicked by Space');
});

test('R-70 Enter on a focused button activates that button instead of a mapped action', () => {
  assert.deepEqual(keyAction({ key: 'Enter' }, 'question', 'control'), { action: null, preventDefault: false });
  assert.deepEqual(keyAction({ key: 'Enter' }, 'answer', 'rating'), { action: null, preventDefault: false });
  assert.deepEqual(keyAction({ key: ' ' }, 'question', 'control'), { action: null, preventDefault: false });
});

test('R-56 Enter on the answer side accepts the suggested grade; Space never rates', () => {
  assert.equal(act('Enter', 'answer'), 'accept');
  assert.equal(act('Enter', 'answer', 'page', { repeat: true }), null);
  assert.equal(act(' ', 'answer'), null);
  assert.equal(act('Enter', 'answer', 'text'), null, 'typing in a field is never a shortcut');
});

test('R-56 H asks for the hint on the question side only; typed letters are never shortcuts', () => {
  assert.equal(act('h', 'question'), 'hint');
  assert.equal(act('H', 'question'), 'hint');
  assert.equal(act('h', 'answer'), null);
  for (const key of ['h', 'u', 'e', '1', 'Enter', ' ', '?', 'Escape']) {
    assert.deepEqual(keyAction({ key }, 'question', 'text'), { action: null, preventDefault: false }, key);
  }
});

test('R-70 Space/Enter move on after a fail and from the finished block', () => {
  assert.equal(act(' ', 'feedback'), 'next');
  assert.equal(act('Enter', 'feedback'), 'next');
  assert.equal(act('Enter', 'complete'), 'next');
});

test('R-70 U undoes, M toggles More, S/R speak, E opens the word, Esc ends, ? helps', () => {
  assert.equal(act('u', 'question'), 'undo');
  assert.equal(act('U', 'answer'), 'undo');
  assert.equal(act('m', 'answer'), 'more');
  assert.equal(act('m', 'feedback'), 'more');
  assert.equal(act('m', 'question'), null);
  assert.equal(act('s', 'question'), 'speak-sentence');
  assert.equal(act('r', 'answer'), 'speak-word');
  assert.equal(act('e', 'answer'), 'detail');
  assert.equal(act('Escape', 'answer'), 'end');
  assert.equal(act('?', 'question'), 'help');
});

test('R-70 nothing is mapped in a text field or with modifier keys', () => {
  assert.deepEqual(keyAction({ key: '2' }, 'answer', 'text'), { action: null, preventDefault: false });
  assert.deepEqual(keyAction({ key: ' ' }, 'question', 'text'), { action: null, preventDefault: false });
  assert.equal(act('2', 'answer', 'page', { ctrlKey: true }), null);
  assert.equal(act('r', 'answer', 'page', { metaKey: true }), null, 'Cmd+R still reloads');
  assert.equal(act('u', 'answer', 'page', { altKey: true }), null);
});

test('R-70 held-down keys do not repeat actions', () => {
  assert.equal(act('2', 'answer', 'page', { repeat: true }), null);
  assert.equal(act(' ', 'question', 'page', { repeat: true }), null);
  assert.equal(act('u', 'answer', 'page', { repeat: true }), null);
});

/** A minimal element whose `closest` answers for the given selectors. */
/** @param {string[]} matches */
const fakeElement = (matches) => ({ closest: (/** @type {string} */ sel) => (matches.some((m) => sel.includes(m)) ? {} : null) });

test('R-70 key targets are classified from the focused element', () => {
  assert.equal(keyTarget(null), 'page');
  assert.equal(keyTarget(/** @type {any} */ (fakeElement(['input']))), 'text');
  assert.equal(keyTarget(/** @type {any} */ (fakeElement(['button[data-grade]']))), 'rating');
  assert.equal(keyTarget(/** @type {any} */ (fakeElement(['a[href]']))), 'control');
  assert.equal(keyTarget(/** @type {any} */ (fakeElement([]))), 'page');
});

test('R-70 Czech QWERTZ: the digit keys rate by their physical position (ě on Digit2 = 2)', () => {
  const czech = /** @type {[string, string, number][]} */ ([
    ['+', 'Digit1', 1],
    ['ě', 'Digit2', 2],
    ['š', 'Digit3', 3],
    ['č', 'Digit4', 4],
    ['ř', 'Digit5', 5],
  ]);
  for (const [key, code, grade] of czech) {
    assert.deepEqual(keyAction({ key, code }, 'answer', 'page').action, { rate: grade }, `${key} ${code}`);
  }
  assert.deepEqual(keyAction({ key: '3', code: 'Numpad3' }, 'answer', 'page').action, { rate: 3 });
  assert.equal(keyAction({ key: 'ž', code: 'Digit6' }, 'answer', 'page').action, null);
  // Numpad without NumLock is an arrow key, never a rating.
  assert.equal(keyAction({ key: 'ArrowDown', code: 'Numpad2' }, 'answer', 'page').action, null);
});

test('R-70 letter shortcuts also work by physical key on non-Latin layouts', () => {
  assert.equal(keyAction({ key: 'г', code: 'KeyU' }, 'answer', 'page').action, 'undo');
  assert.equal(keyAction({ key: 'ь', code: 'KeyM' }, 'answer', 'page').action, 'more');
  assert.equal(keyAction({ key: 'ы', code: 'KeyS' }, 'answer', 'page').action, 'speak-sentence');
  assert.equal(keyAction({ key: 'к', code: 'KeyR' }, 'answer', 'page').action, 'speak-word');
  assert.equal(keyAction({ key: 'у', code: 'KeyE' }, 'answer', 'page').action, 'detail');
  // A typed Latin letter wins over the physical key (e.g. Dvorak).
  assert.equal(keyAction({ key: 'g', code: 'KeyU' }, 'answer', 'page').action, null);
  assert.equal(keyAction({ key: 'u', code: 'KeyF' }, 'answer', 'page').action, 'undo');
});
