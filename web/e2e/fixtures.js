// @ts-check
/**
 * Shared e2e fixtures: a fresh server per test, set up with the logged
 * setup code, with the synthetic packs imported and the browser context
 * logged in (R-82), a stand-in for the
 * Web Speech API that records what was spoken, and a check that the page
 * logged no errors (CSP violations included).
 */

import { DatabaseSync } from 'node:sqlite';

import { test as base, expect } from '@playwright/test';

import { ARTICLE_PACK, BOOK_PACK } from './pack.js';
import { PASSWORD, startServer } from './server.js';

export { PASSWORD, startServer };

/** Name of the session cookie (design §9.1). */
export const SESSION_COOKIE = '__Host-vocab_session';

/**
 * `url`: the app's HTTPS origin for the browser. `api`: a logged-in API
 * call from the test itself (same session as the browser context).
 *
 * @typedef {{ url: string, serverUrl: string, dbPath: string, cookie: string, api: (method: string, path: string, body?: unknown) => Promise<any> }} App
 */

/**
 * `POST` JSON to the server as the page would; returns the session
 * cookie's value it sets.
 *
 * @param {string} serverUrl
 * @param {string} origin
 * @param {string} path
 * @param {unknown} body
 */
async function signIn(serverUrl, origin, path, body) {
  const response = await fetch(`${serverUrl}${path}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', origin },
    body: JSON.stringify(body),
  });
  if (!response.ok) throw new Error(`${path} failed: ${response.status} ${await response.text()}`);
  const header = response.headers.getSetCookie().find((c) => c.startsWith(`${SESSION_COOKIE}=`));
  if (!header) throw new Error(`${path} set no session cookie`);
  return header.slice(SESSION_COOKIE.length + 1).split(';')[0];
}

/**
 * Log in through the API with `password`; returns the session cookie's value.
 *
 * @param {{ serverUrl: string, url: string }} server
 * @param {string} [password]
 */
export function loginByApi(server, password = PASSWORD) {
  return signIn(server.serverUrl, server.url, '/api/auth/login', { password });
}

/**
 * A logged-in API caller with the session `cookie` (as the page sends it:
 * same origin, JSON also for empty bodies).
 *
 * @param {{ serverUrl: string, url: string }} server
 * @param {string} cookie
 * @returns {App['api']}
 */
export function apiFor(server, cookie) {
  return async (method, path, body) => {
    const response = await fetch(`${server.serverUrl}${path}`, {
      method,
      headers: {
        cookie: `${SESSION_COOKIE}=${cookie}`,
        origin: server.url,
        ...(method === 'GET' ? {} : { 'content-type': 'application/json' }),
      },
      body: method === 'GET' ? undefined : JSON.stringify(body ?? {}),
    });
    const text = await response.text();
    if (!response.ok) throw new Error(`${method} ${path}: ${response.status} ${text}`);
    return text ? JSON.parse(text) : null;
  };
}

/**
 * Import the synthetic packs and set four new words per block.
 *
 * @param {App['api']} api
 */
export async function importPacks(api) {
  for (const pack of [BOOK_PACK, ARTICLE_PACK]) {
    const report = await api('POST', '/api/import/commit', { text: JSON.stringify(pack) });
    if (report.packs[0].status !== 'imported') throw new Error(`import failed: ${JSON.stringify(report)}`);
  }
  // Four new words per block: exactly the book's words in a book-scoped block.
  await api('PATCH', '/api/settings', { new_per_day: 4, block_size: 10 });
}

/** Records `speechSynthesis.speak` calls in `window.spoken` (R-41 fallback). */
function installFakeSpeech() {
  /** @type {{ text: string, lang: string, rate: number }[]} */
  const spoken = [];
  class FakeUtterance {
    /** @param {string} text */
    constructor(text) {
      this.text = text;
      this.lang = '';
      this.rate = 1;
      /** @type {unknown} */
      this.voice = null;
    }
  }
  // One on-device English voice: only on-device voices are used (R-24).
  const voices = [
    { lang: 'en-US', name: 'Fake US voice', localService: true, default: true },
    { lang: 'en-GB', name: 'Fake UK voice', localService: true, default: false },
  ];
  const synth = {
    getVoices: () => voices,
    cancel: () => {},
    /** @param {FakeUtterance} u */
    speak: (u) => {
      spoken.push({ text: u.text, lang: u.lang, rate: u.rate });
    },
  };
  Object.defineProperty(window, 'speechSynthesis', { value: synth, configurable: true });
  Object.defineProperty(window, 'SpeechSynthesisUtterance', { value: FakeUtterance, configurable: true });
  Object.defineProperty(window, 'spoken', { value: spoken, configurable: true });
}

export const test = base.extend(
  /** @type {import('@playwright/test').Fixtures<{ app: App, pageErrors: string[], ttsUrl: string | null }, {}, import('@playwright/test').PlaywrightTestArgs & import('@playwright/test').PlaywrightTestOptions>} */ ({
    // The TTS service of the server (`test.use({ ttsUrl })`); null: none.
    ttsUrl: [null, { option: true }],
    app: async ({ context, ttsUrl }, use) => {
      const server = await startServer(ttsUrl ? { ttsUrl } : {});
      const cookie = await signIn(server.serverUrl, server.url, '/api/auth/setup', {
        code: server.setupCode,
        password: PASSWORD,
      });
      await context.addCookies([
        { name: SESSION_COOKIE, value: cookie, url: server.url, secure: true, httpOnly: true, sameSite: 'Strict' },
      ]);
      const api = apiFor(server, cookie);
      await importPacks(api);
      await use({ url: server.url, serverUrl: server.serverUrl, dbPath: server.dbPath, cookie, api });
      await server.stop();
    },
    pageErrors: async ({ page }, use) => {
      /** @type {string[]} */
      const errors = [];
      page.on('pageerror', (err) => errors.push(`pageerror: ${err.message}`));
      page.on('console', (msg) => {
        if (msg.type() === 'error') errors.push(`console: ${msg.text()}`);
      });
      await page.addInitScript(installFakeSpeech);
      await use(errors);
      expect(errors, 'no errors or CSP violations in the page').toEqual([]);
    },
  }),
);

export { expect };

/**
 * Make every word known on its Understand card (nothing due, no new word)
 * and give `headwords` a Produce card that waits for its first review
 * since today, as an unlock leaves it (design §6.4).
 *
 * @param {App} app
 * @param {string[]} headwords
 */
export async function waitingProduce(app, headwords) {
  const today = await app.api('GET', '/api/today');
  const db = new DatabaseSync(app.dbPath);
  db.prepare(
    `UPDATE card SET state = 'review', stability = 40.0, difficulty = 5.0, due_day = '2099-01-01',
     last_review_day = ?, introduced_day = '2026-10-01', reps = 4, format = 'word_only' WHERE kind = 'understand'`,
  ).run(today.study_day);
  const insert = db.prepare(
    `INSERT INTO card (word_id, kind, state, due_day, created_at)
     SELECT id, 'produce', 'new', ?, '2026-11-01T00:00:00Z' FROM word WHERE headword = ?`,
  );
  for (const headword of headwords) insert.run(today.study_day, headword);
  db.close();
}

/**
 * Make every word known on its Understand card (nothing due, no new word),
 * mark `headwords` for spelling practice with `note` as the spelling note,
 * and give each a Spelling card that waits for its first review since
 * today, as an unlock leaves it (design §6.4).
 *
 * @param {App} app
 * @param {string[]} headwords
 * @param {string | null} [note]
 */
export async function waitingSpelling(app, headwords, note = null) {
  const today = await app.api('GET', '/api/today');
  const db = new DatabaseSync(app.dbPath);
  db.prepare(
    `UPDATE card SET state = 'review', stability = 40.0, difficulty = 5.0, due_day = '2099-01-01',
     last_review_day = ?, introduced_day = '2026-10-01', reps = 4, format = 'word_only' WHERE kind = 'understand'`,
  ).run(today.study_day);
  const mark = db.prepare('UPDATE word SET spelling = 1, spelling_note = ? WHERE headword = ?');
  const insert = db.prepare(
    `INSERT INTO card (word_id, kind, state, due_day, created_at)
     SELECT id, 'spelling', 'new', ?, '2026-11-01T00:00:00Z' FROM word WHERE headword = ?`,
  );
  for (const headword of headwords) {
    mark.run(note, headword);
    insert.run(today.study_day, headword);
  }
  db.close();
}

/**
 * `day` plus `n` days (`YYYY-MM-DD`).
 *
 * @param {string} day
 * @param {number} n
 */
function addDays(day, n) {
  const date = new Date(`${day}T12:00:00Z`);
  date.setUTCDate(date.getUTCDate() + n);
  return date.toISOString().slice(0, 10);
}

/**
 * A known, a familiar and a problem word (brontle, due today), two new
 * words; two recall tests yesterday (one passed). Returns the study day.
 *
 * @param {import('./fixtures.js').App} app
 */
export async function seedHistory(app) {
  const today = (await app.api('GET', '/api/today')).study_day;
  const db = new DatabaseSync(app.dbPath);
  const card = db.prepare(
    `UPDATE card SET state = 'review', format = ?, stability = ?, difficulty = 5.0, due_day = ?,
       last_review_day = ?, introduced_day = ?, reps = ?, lapses = ?, leech = ?
     WHERE kind = 'understand' AND word_id = (SELECT id FROM word WHERE headword = ?)`,
  );
  const yesterday = addDays(today, -1);
  card.run('sentence', 30.0, addDays(today, 3), yesterday, addDays(today, -20), 5, 0, 'none', 'glimmerous');
  card.run('sentence', 10.0, addDays(today, 1), yesterday, addDays(today, -40), 6, 1, 'none', 'quabble');
  card.run('word_only', 2.0, today, yesterday, addDays(today, -40), 9, 4, 'flagged', 'brontle');
  const review = db.prepare(
    `INSERT INTO review_log (client_review_id, card_id, reviewed_at, review_day, raw_grade, fsrs_rating,
       format, elapsed_days, before_json)
     SELECT ?, c.id, ?, ?, ?, 3, 'sentence', ?, '{"state":"review"}'
     FROM card c JOIN word w ON w.id = c.word_id WHERE w.headword = ? AND c.kind = 'understand'`,
  );
  review.run('test-e2e-review-1', `${yesterday}T10:00:00Z`, yesterday, 2, 5, 'glimmerous');
  review.run('test-e2e-review-2', `${yesterday}T10:01:00Z`, yesterday, 4, 3, 'quabble');
  db.close();
  return today;
}

/**
 * Start a block in the book's scope from the Today screen.
 *
 * @param {import('@playwright/test').Page} page
 * @param {App} app
 * @param {{ touch?: boolean }} [options]
 */
export async function startBookBlock(page, app, { touch = false } = {}) {
  await page.goto(`${app.url}/#/`);
  const scope = page.getByLabel(/An E2E Test Novel/);
  const start = page.getByTestId('start');
  if (touch) {
    await scope.tap();
    await start.tap();
  } else {
    await scope.check();
    await start.click();
  }
  await expect(page).toHaveURL(/#\/practice$/);
  await expect(page.getByTestId('progress')).toHaveText('1 / 4');
}

/**
 * The headword of the item on screen (question or answer side).
 *
 * @param {import('@playwright/test').Page} page
 */
export async function headwordOnScreen(page) {
  const sentence = page.getByTestId('sentence-en').locator('strong.target');
  const alone = page.getByTestId('headword-question');
  const strip = page.getByTestId('headword');
  if (await strip.count()) return (await strip.textContent())?.trim() ?? '';
  if (await alone.count()) return (await alone.textContent())?.trim() ?? '';
  return (await sentence.first().textContent())?.trim() ?? '';
}

/** Wait out the rating guards after a reveal (500 ms; 600 ms for a tap on the reveal spot, bug 4). */
export function waitGuard() {
  return new Promise((resolve) => setTimeout(() => resolve(undefined), 650));
}

/**
 * Control the page's own clock for a test that depends on a product time
 * window (input guard, rating guard, slip window, toast lifetime). Call it
 * once, before the first `page.goto`. The clock runs at real speed until
 * `freezeClock`.
 *
 * Why not `waitForTimeout`: it measures on the Node side, and the polling
 * and input latency of Playwright add an unknown amount on a loaded
 * machine, so a tap meant to land inside (or outside) a window may not.
 * The product reads `performance.now()` and `setTimeout`, both of which
 * Playwright's clock replaces.
 *
 * @param {import('@playwright/test').Page} page
 */
export function installClock(page) {
  return page.clock.install();
}

/**
 * Stop the page's clock: from here on page time passes only through
 * `advanceClock`, however long Playwright, the server or the machine take.
 * Also works again after a reload.
 *
 * After a reload (or anything that draws a card asynchronously), wait for
 * the card to be drawn before `advanceClock`: a side drawn on the frozen
 * clock starts its input guard then, and the guarded buttons (Check,
 * Show answer, Next) stay disabled until page time passes again — a
 * click on one would wait for the test's timeout. Advancing first lets
 * the time pass before the guard has even started.
 *
 * @param {import('@playwright/test').Page} page
 */
export async function freezeClock(page) {
  const now = await page.evaluate(() => Date.now());
  await page.clock.pauseAt(now + 1_000);
}

/**
 * Let exactly `ms` of page time pass on a frozen clock, running the timers
 * (guards, toasts) that fall due on the way.
 *
 * @param {import('@playwright/test').Page} page
 * @param {number} ms
 */
export function advanceClock(page, ms) {
  return page.clock.runFor(ms);
}
