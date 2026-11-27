// @ts-check
/** The Words screens' pure helpers (R-04, R-25, R-73). */

import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  activeFilters,
  emptyListState,
  fieldLabel,
  goalChoices,
  goalValue,
  hasOneMark,
  listParams,
  markedSource,
  percent,
  plural,
  showValue,
  statusLabel,
} from '../js/words-model.js';

test('R-73 the list query leaves out defaults and invalid usefulness bounds', () => {
  const state = emptyListState();
  assert.equal(listParams(state).toString(), '');
  state.q = '  lull ';
  state.source = '3';
  state.sort = 'due';
  state.status = 'learning';
  state.minU = '4';
  state.maxU = '11';
  state.flags = ['suspended', 'conflicts'];
  assert.equal(
    listParams(state).toString(),
    'q=lull&source=3&sort=due&status=learning&min_u=4&flags=conflicts%2Csuspended',
  );
  assert.equal(activeFilters(state), 5, 'source, status, min_u and two flags');
  state.minU = '2.5';
  assert.equal(listParams(state).has('min_u'), false);
});

test('R-25 a sentence is edited with its [[…]] marks', () => {
  assert.equal(markedSource({ pre: 'The sea ', target: 'lulled', post: ' us.' }), 'The sea [[lulled]] us.');
  assert.equal(markedSource({ pre: 'Plain', target: '', post: '' }), 'Plain');
  assert.equal(hasOneMark('The sea [[lulled]] us.'), true);
  assert.equal(hasOneMark('The sea lulled us.'), false);
  assert.equal(hasOneMark('[[a]] and [[b]]'), false);
  assert.equal(hasOneMark('[[]]'), false);
  assert.equal(hasOneMark(']] [['), false);
});

test('R-04 labels and values read as plain words', () => {
  assert.equal(fieldLabel('speak'), 'Text to speak');
  assert.equal(fieldLabel('usefulness'), 'Usefulness');
  assert.equal(fieldLabel('unknown_field'), 'unknown_field');
  assert.equal(showValue(null), '(empty)');
  assert.equal(showValue(''), '(empty)');
  assert.equal(showValue(true), 'yes');
  assert.equal(showValue(7), '7');
  assert.equal(statusLabel('leech'), 'Problem word');
  assert.equal(percent(0.934), '93 %');
  assert.equal(percent(null), '—');
  assert.equal(plural(1, 'word'), '1 word');
  assert.equal(plural(3, 'sentence'), '3 sentences');
});

test('R-52 the goal switch offers automatic, recognise and produce', () => {
  assert.deepEqual(
    goalChoices(7).map(([value]) => value),
    ['auto', 'recognise', 'active'],
  );
  assert.match(goalChoices(7)[0][1], /produce/);
  assert.match(goalChoices(5)[0][1], /recognise/);
  assert.equal(goalValue('auto'), null);
  assert.equal(goalValue('active'), 'active');
});
