// @ts-check
/** Theme, speech voice choice and routes (R-71, R-41, R-74). */

import assert from 'node:assert/strict';
import { test } from 'node:test';

import { parseRoute } from '../js/router.js';
import { weekday } from '../js/screens/today.js';
import { pickVoice, speechLang, speechRate } from '../js/speech.js';
import { isThemeSetting, themeAttribute } from '../js/theme.js';

test('R-71 dark and light are forced; system follows prefers-color-scheme', () => {
  assert.equal(themeAttribute('dark'), 'dark');
  assert.equal(themeAttribute('light'), 'light');
  assert.equal(themeAttribute('system'), null);
  assert.equal(isThemeSetting('dark'), true);
  assert.equal(isThemeSetting('blue'), false);
  assert.equal(isThemeSetting(null), false);
});

const voices = [
  { lang: 'cs-CZ', name: 'Zuzana', localService: true },
  { lang: 'en-GB', name: 'Daniel', localService: true },
  { lang: 'en_US', name: 'Samantha', localService: false },
  { lang: 'en-US', name: 'Alex', localService: true },
];

test('R-41 browser speech prefers a US English voice, local first', () => {
  assert.equal(pickVoice(voices, 'us')?.name, 'Alex');
  assert.equal(speechLang('us'), 'en-US');
});

test('R-42 the UK setting prefers British English', () => {
  assert.equal(pickVoice(voices, 'uk')?.name, 'Daniel');
  assert.equal(speechLang('uk'), 'en-GB');
});

test('R-41 any on-device English voice is better than none; without one nothing is chosen', () => {
  assert.equal(pickVoice([{ lang: 'en-AU', name: 'Karen', localService: true }, { lang: 'de-DE', name: 'Anna', localService: true }], 'us')?.name, 'Karen');
  assert.equal(pickVoice([{ lang: 'de-DE', name: 'Anna', localService: true }], 'us'), null);
  assert.equal(pickVoice([], 'us'), null);
});

test('R-24 U5 network voices are never used: book sentences stay on the device', () => {
  assert.equal(pickVoice([{ lang: 'en-US', name: 'Cloud', localService: false }], 'us'), null);
  assert.equal(pickVoice([{ lang: 'en-US', name: 'Unknown' }], 'us'), null);
  assert.equal(pickVoice(voices, 'us')?.name, 'Alex');
});

test('R-41 speed 0.75x slows speech down', () => {
  assert.equal(speechRate('1'), 1);
  assert.equal(speechRate('0.75'), 0.75);
});

test('R-74 routes of the practice screens', () => {
  assert.deepEqual(parseRoute(''), { name: 'home', params: {} });
  assert.deepEqual(parseRoute('#/'), { name: 'home', params: {} });
  assert.deepEqual(parseRoute('#/practice'), { name: 'practice', params: {} });
  assert.deepEqual(parseRoute('#/summary'), { name: 'summary', params: {} });
  assert.deepEqual(parseRoute('#/settings'), { name: 'settings', params: {} });
  assert.deepEqual(parseRoute('#/words/12'), { name: 'word', params: { id: '12' } });
  assert.deepEqual(parseRoute('#/practice/x'), { name: 'not-found', params: {} });
  assert.deepEqual(parseRoute('#/login'), { name: 'login', params: {} });
});

test('R-54 the forecast labels study days with their weekday', () => {
  assert.equal(weekday('2026-11-20'), 'Fri');
  assert.equal(weekday('2026-11-23'), 'Mon');
  assert.equal(weekday('nonsense'), 'nonsense');
});
