// @ts-check
/** What Start will do and the end of the day (U1, F1); import wording (U2); guards (bug 4); busy retries. */

import assert from 'node:assert/strict';
import { test } from 'node:test';

import { busyAnswer, busyWaitMs } from '../js/api.js';
import { blockLabel, blockPreview, dayDone, dueTomorrow, moreNewCount, newWaiting, nextBlock, pausedForBacklog, typedLine } from '../js/day-plan.js';
import { describePointer } from '../js/import-issues.js';
import { RATING_GUARD_MS, ratingGuardOpen, sameSpotTap } from '../js/rating.js';

/**
 * @param {Partial<import('../js/session.js').Today>} over
 * @returns {import('../js/session.js').Today}
 */
function today(over = {}) {
  return {
    study_day: '2026-11-21',
    scope_source_id: null,
    due_now: 0,
    relearning: 0,
    forecast: [{ day: '2026-11-22', due: 7 }],
    new_available: 0,
    new_words: { per_day: 5, introduced_today: 5, left_today: 0, quota: 0, paused: false, backlog: false, anyway_today: false, due_now: 0 },
    active_session: null,
    finished_blocks: { total: 1, today: 1 },
    sources: [],
    ...over,
  };
}

test('U1 the Start label says what the block will hold, interleaved like the server', () => {
  assert.deepEqual(blockPreview({ due: 15, fresh: 5, blockSize: 20 }), { total: 20, due: 15, fresh: 5 });
  // Every 4th slot is new while due cards last.
  assert.deepEqual(blockPreview({ due: 30, fresh: 10, blockSize: 20 }), { total: 20, due: 15, fresh: 5 });
  assert.deepEqual(blockPreview({ due: 2, fresh: 10, blockSize: 10 }), { total: 10, due: 2, fresh: 8 });
  assert.deepEqual(blockPreview({ due: 0, fresh: 0, blockSize: 10 }), { total: 0, due: 0, fresh: 0 });
  assert.equal(blockLabel({ total: 20, due: 15, fresh: 5 }), '20 cards (15 due + 5 new)');
  assert.equal(blockLabel({ total: 4, due: 0, fresh: 4 }), '4 cards (4 new)');
  assert.equal(blockLabel({ total: 1, due: 1, fresh: 0 }), '1 card (1 due)');
});

test('F1 the day is done when nothing is due and no new words are left today', () => {
  assert.equal(dayDone(today(), 20), true);
  assert.equal(dayDone(today({ due_now: 1 }), 20), false);
  const fresh = today({ new_available: 3, new_words: { ...today().new_words, quota: 2, left_today: 2 } });
  assert.equal(dayDone(fresh, 20), false);
  assert.equal(dayDone(today({ active_session: { id: 'x', scope_source_id: null, block_no: 1, total: 4, remaining: 2 } }), 20), false);
  assert.equal(dueTomorrow(today({ due_tomorrow: 3 })), 3);
  assert.equal(dueTomorrow(today()), 7);
});

test('F1 "Learn N more" offers the new-per-day setting (5 when it is 0), at most what is waiting', () => {
  assert.equal(moreNewCount(today({ new_waiting: 40 }), 10), 10);
  assert.equal(moreNewCount(today({ new_waiting: 3 }), 10), 3);
  assert.equal(moreNewCount(today({ new_waiting: 40 }), 0), 5);
  assert.equal(moreNewCount(today({ new_waiting: 0 }), 10), 0);
  assert.equal(newWaiting(today({ new_available: 4 })), 4);
});

test('F1 the backlog pause is recognised from the new field and from the old ones', () => {
  assert.equal(pausedForBacklog(today({ new_paused_reason: 'backlog' })), true);
  assert.equal(pausedForBacklog(today({ new_paused_reason: null })), false);
  assert.equal(pausedForBacklog(today({ new_words: { ...today().new_words, paused: true, backlog: true } })), true);
});

test('U2 import issues name the headword and the sentence, not JSON pointers', () => {
  const at = (/** @type {number} */ i) => (i === 3 ? 'glimmerous' : null);
  assert.equal(describePointer('/words/3/sentences/0/cs', at), 'glimmerous, sentence 1 (Czech)');
  assert.equal(describePointer('/words/3/sentences/0/cs', at, { withinWord: true }), 'sentence 1 (Czech)');
  assert.equal(describePointer('/words/7/meaning', at), 'word 8, meaning');
  assert.equal(describePointer('/exported_at', at), 'export date');
  assert.equal(describePointer('', at), '');
});

test('bug 4 ratings wait 500 ms after the reveal; a tap where the reveal tap landed within 600 ms is ignored', () => {
  assert.equal(RATING_GUARD_MS, 500);
  assert.equal(ratingGuardOpen(499), false);
  assert.equal(ratingGuardOpen(500), true);
  const reveal = { x: 100, y: 700, t: 1000 };
  assert.equal(sameSpotTap(reveal, { x: 104, y: 705, t: 1550 }), true);
  assert.equal(sameSpotTap(reveal, { x: 104, y: 705, t: 1650 }), false);
  assert.equal(sameSpotTap(reveal, { x: 200, y: 705, t: 1550 }), false);
  assert.equal(sameSpotTap(null, { x: 100, y: 700, t: 1100 }), false);
});

test('busy servers are asked again after Retry-After; lock-outs and timeouts are not', () => {
  assert.equal(busyAnswer(503, 'overloaded', '5'), true);
  assert.equal(busyAnswer(503, 'busy', '5'), true);
  assert.equal(busyAnswer(429, 'rate_limited', '3'), true);
  assert.equal(busyAnswer(429, 'rate_limited', '120'), false);
  assert.equal(busyAnswer(429, 'locked', '60'), false);
  assert.equal(busyAnswer(503, 'timeout', null), false);
  assert.equal(busyAnswer(500, 'http', null), false);
  assert.equal(busyWaitMs('5', 0), 5000);
  assert.equal(busyWaitMs('60', 0), 10_000);
  assert.equal(busyWaitMs(null, 1), 4000);
});

test('R-52 Today says how many Produce cards wait for their first time and how many come today', () => {
  /** @param {number} waiting @param {Partial<import('../js/session.js').TypedAllowance>} [typed] */
  const line = (waiting, typed = {}) =>
    typedLine(
      today({
        typed_waiting: waiting,
        new_typed: { per_day: 10, introduced_today: 0, left_today: 10, quota: 10, paused: false, ...typed },
      }),
    );
  assert.equal(line(0), null);
  assert.equal(line(0, { introduced_today: 3 }), '3 Produce cards practised for the first time today.');
  assert.equal(line(1), '1 Produce card waits for the first time — it comes today.');
  assert.equal(line(4), '4 Produce cards wait for the first time — all come today.');
  assert.equal(line(90), '90 Produce cards wait for the first time — 10 today (limit 10 a day), the rest on later days.');
  assert.equal(
    line(90, { introduced_today: 4, left_today: 6, quota: 6 }),
    '90 Produce cards wait for the first time — 6 today (limit 10 a day), the rest on later days.',
  );
  assert.equal(
    line(80, { introduced_today: 10, left_today: 0, quota: 0 }),
    '80 Produce cards wait for the first time — today’s 10 are done (limit in Settings); more tomorrow.',
  );
  assert.equal(line(5, { paused: true, quota: 0 }), '5 Produce cards wait for the first time — held back until you catch up on reviews.');
  assert.equal(typedLine(today()), null, 'an older server without the counts');
});

test('R-52 cap 0 says first-time Produce cards are switched off and where to change it', () => {
  const t = today({
    typed_waiting: 4,
    typed_today: 0,
    new_typed: { per_day: 0, introduced_today: 0, left_today: 0, quota: 0, paused: false },
  });
  assert.equal(
    typedLine(t),
    '4 Produce cards wait for the first time — new Produce cards are switched off (New Produce cards per day in Settings).',
  );
});

test('R-52 Produce cards whose words already have a card today come on a later day, and the day is done', () => {
  const t = today({
    due_now: 0,
    typed_waiting: 1,
    typed_today: 0,
    next_block: { due: 0, new: 0 },
    new_typed: { per_day: 10, introduced_today: 0, left_today: 10, quota: 10, paused: false },
  });
  assert.equal(typedLine(t), '1 Produce card waits for the first time — its word has a card today already, so it comes on a later day.');
  assert.equal(dayDone(t, 20), true, 'no Start that yields nothing');
  const some = today({
    typed_waiting: 5,
    typed_today: 3,
    new_typed: { per_day: 10, introduced_today: 0, left_today: 10, quota: 10, paused: false },
  });
  assert.equal(typedLine(some), '5 Produce cards wait for the first time — 3 today (limit 10 a day), the rest on later days.');
});

test('R-52 the Start label is the server\'s own plan when it sends one', () => {
  // Counts alone would promise a card (due_now 1); the server's plan is empty.
  assert.deepEqual(nextBlock(today({ due_now: 1, next_block: { due: 0, new: 0 } }), 20), { total: 0, due: 0, fresh: 0 });
  assert.deepEqual(nextBlock(today({ due_now: 9, next_block: { due: 4, new: 1 } }), 5), { total: 5, due: 4, fresh: 1 });
  assert.deepEqual(nextBlock(today({ due_now: 2 }), 20), { total: 2, due: 2, fresh: 0 }, 'older server');
});
