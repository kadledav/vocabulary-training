// @ts-check
/** The Progress screen's and the leech choices' pure helpers (R-55, R-72). */

import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  activitySummary,
  barHeights,
  forecastSummary,
  leechChoices,
  leechDoneText,
  lowerUsefulness,
  percentText,
  reasonText,
  retentionText,
  shortDay,
  statusLine,
  statusSegments,
} from '../js/progress-model.js';

const counts = (/** @type {Partial<import('../js/progress-model.js').StatusCounts>} */ c) => ({
  words: 0,
  new: 0,
  learning: 0,
  familiar: 0,
  known: 0,
  problem: 0,
  suspended: 0,
  can_produce: 0,
  ...c,
});

test('R-72 retention says the measured share against the aim in words', () => {
  const r = { kind: /** @type {const} */ ('understand'), tested: 120, passed: 104, measured: 104 / 120, target: 0.9, few: false };
  assert.equal(
    retentionText(r),
    'Understand: you remembered 104 of 120 cards (87%); the app aims for 90%. That is below the aim.',
  );
  assert.match(retentionText({ ...r, passed: 109, measured: 109 / 120 }), /That is on target\.$/);
  assert.match(retentionText({ ...r, passed: 118, measured: 118 / 120 }), /That is above the aim\.$/);
  assert.match(retentionText({ ...r, tested: 7, passed: 5, measured: 5 / 7, few: true }), /Too few reviews yet to compare\.$/);
  assert.equal(
    retentionText({ kind: 'produce', tested: 0, passed: 0, measured: null, target: null, few: true }),
    'Produce: no reviews of earlier-seen cards in these 30 days.',
  );
  assert.equal(percentText(0.926), '93%');
});

test('R-55 reasons name the rule in plain words', () => {
  const card = { kind: /** @type {const} */ ('understand'), lapses: 5, stability: 2, last_review_day: '2026-11-19' };
  assert.equal(reasonText({ ...card, reason: { kind: 'lapses', lapses: 5 } }), 'Understand: forgotten on 5 different days.');
  assert.match(reasonText({ ...card, kind: 'produce', reason: { kind: 'forgot_when_known' } }), /^Produce: rated 5 \(No idea\) twice in a row/);
  assert.match(reasonText({ ...card, reason: { kind: 'acknowledged' } }), /three weeks/);
});

test('R-55 leech choices follow the word: recognise-only for active words, suspend first when not useful', () => {
  assert.deepEqual(
    leechChoices({ usefulness: 7, goal: 'active' }).map((c) => [c.action, c.primary]),
    [
      ['leech_keep', true],
      ['leech_recognise', false],
      ['leech_usefulness', false],
      ['leech_suspend', false],
    ],
  );
  assert.deepEqual(
    leechChoices({ usefulness: 3, goal: 'recognise' }).map((c) => [c.action, c.primary]),
    [
      ['leech_suspend', true],
      ['leech_keep', false],
      ['leech_usefulness', false],
    ],
  );
  assert.deepEqual(
    leechChoices({ usefulness: 1, goal: 'recognise' }).map((c) => c.action),
    ['leech_suspend', 'leech_keep'],
  );
  assert.deepEqual(lowerUsefulness(4), [3, 2, 1]);
  assert.deepEqual(lowerUsefulness(1), []);
  assert.match(leechDoneText('leech_usefulness', 3), /lowered to 3/);
  assert.match(leechDoneText('leech_keep'), /four more days/);
});

test('R-72 bars are proportional, a non-zero day never vanishes', () => {
  assert.deepEqual(barHeights([0, 1, 50, 100], 40), [0, 1, 20, 40]);
  assert.deepEqual(barHeights([0, 0], 40), [0, 0]);
  assert.deepEqual(barHeights([], 40), []);
});

test('R-72 status segments fill the width exactly in a fixed order', () => {
  const segments = statusSegments(counts({ words: 6, new: 1, learning: 1, familiar: 1, known: 1, problem: 1, suspended: 1 }), 300);
  assert.deepEqual(
    segments.map(([key]) => key),
    ['known', 'familiar', 'learning', 'problem', 'new', 'suspended'],
  );
  assert.equal(segments.reduce((n, [, , , w]) => n + w, 0), 300);
  assert.equal(segments[0][2], 0);
  for (let i = 1; i < segments.length; i++) assert.equal(segments[i][2], segments[i - 1][2] + segments[i - 1][3]);
  assert.deepEqual(statusSegments(counts({}), 300), []);
});

test('R-72 text alternatives carry the numbers', () => {
  assert.equal(statusLine(counts({ words: 3, known: 2, new: 1, can_produce: 1 })), '3 words: 2 known, 1 new.');
  assert.equal(statusLine(counts({})), '0 words.');
  assert.equal(shortDay('2026-11-23'), 'Mon 23');
  assert.equal(
    forecastSummary([
      { day: '2026-11-21', due: 2 },
      { day: '2026-11-22', due: 0 },
    ]),
    'Reviews due — Sat 21: 2.',
  );
  assert.equal(forecastSummary([{ day: '2026-11-21', due: 0 }]), 'Nothing is due in the next 14 days.');
  assert.equal(
    activitySummary({ days: [{ day: '2026-11-19', reviews: 12, new_words: 2 }, { day: '2026-11-20', reviews: 0, new_words: 0 }], reviews: 12, new_words: 2 }),
    'Ratings per day — Thu 19: 12.',
  );
});
