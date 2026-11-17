// @ts-check
/** Rating-bar model (R-60, R-62; design §7.2; research §10). */

import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  INPUT_GUARD_MS,
  LEGEND_BLOCKS,
  RATINGS,
  SLIP_TOAST_MS,
  TOAST_MS,
  guardOpen,
  isGrade,
  legendVisible,
  rating,
  ratingAriaLabel,
  ratingGroups,
  toastPlan,
  toastText,
} from '../js/rating.js';

test('R-60 the scale runs 1 = best to 5 = worst with the agreed English labels', () => {
  assert.deepEqual(
    RATINGS.map((r) => [r.grade, r.label]),
    [
      [1, 'Instantly'],
      [2, 'Knew it'],
      [3, 'Shaky'],
      [4, 'Vague'],
      [5, 'No idea'],
    ],
  );
  for (const r of RATINGS) assert.ok(r.definition.length > 0, `grade ${r.grade} has a legend line`);
});

test('R-60 the pass/fail line sits between 3 and 4', () => {
  const { pass, fail } = ratingGroups();
  assert.deepEqual(pass.map((r) => r.grade), [1, 2, 3]);
  assert.deepEqual(fail.map((r) => r.grade), [4, 5]);
});

test('R-62 buttons are never a bare digit: accessible names carry the label', () => {
  assert.equal(ratingAriaLabel(1), 'Rate 1, Instantly');
  assert.equal(ratingAriaLabel(2), 'Rate 2, Knew it');
  assert.equal(ratingAriaLabel(5), 'Rate 5, No idea');
});

test('R-62 the toast names the number and the label', () => {
  assert.equal(toastText(2), 'Rated 2 · Knew it');
  assert.equal(toastText(4), 'Rated 4 · Vague');
});

test('R-62 a rating within 250 ms of the reveal is ignored (double-tap guard)', () => {
  assert.equal(INPUT_GUARD_MS, 250);
  assert.equal(guardOpen(0), false);
  assert.equal(guardOpen(249), false);
  assert.equal(guardOpen(250), true);
  assert.equal(guardOpen(5000), true);
});

test('R-62 the toast stays 4 s normally', () => {
  assert.deepEqual(toastPlan({ grade: 2, msSinceReveal: 100, lastFailed: true }), { ms: TOAST_MS, pulse: false });
  assert.deepEqual(toastPlan({ grade: 1, msSinceReveal: 2000, lastFailed: true }), { ms: TOAST_MS, pulse: false });
  assert.deepEqual(toastPlan({ grade: 1, msSinceReveal: 100, lastFailed: false }), { ms: TOAST_MS, pulse: false });
  assert.equal(TOAST_MS, 4000);
});

test('R-62 a reflex 1 right after a failed card keeps the toast 8 s and pulses (slip guard)', () => {
  assert.deepEqual(toastPlan({ grade: 1, msSinceReveal: 599, lastFailed: true }), { ms: SLIP_TOAST_MS, pulse: true });
  assert.deepEqual(toastPlan({ grade: 1, msSinceReveal: 600, lastFailed: true }), { ms: TOAST_MS, pulse: false });
  assert.equal(SLIP_TOAST_MS, 8000);
});

test('R-62 the legend shows during the first 3 finished blocks in auto mode', () => {
  assert.equal(LEGEND_BLOCKS, 3);
  assert.equal(legendVisible('auto', 0), true);
  assert.equal(legendVisible('auto', 2), true);
  assert.equal(legendVisible('auto', 3), false);
  assert.equal(legendVisible('always', 50), true);
  assert.equal(legendVisible('off', 0), false);
});

test('R-60 grades outside 1-5 are rejected', () => {
  assert.equal(isGrade(0), false);
  assert.equal(isGrade(6), false);
  assert.equal(isGrade(2.5), false);
  assert.equal(isGrade(3), true);
  assert.throws(() => rating(/** @type {any} */ (7)));
});
