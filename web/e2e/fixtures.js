// @ts-check
/**
 * Shared e2e fixtures: a fresh server per test, set up with the logged
 * setup code, with the synthetic packs imported and the browser context
 * logged in (R-82), a stand-in for the
 * Web Speech API that records what was spoken, and a check that the page
 * logged no errors (CSP violations included).
 */

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
