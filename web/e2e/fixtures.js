// @ts-check
/**
 * Shared e2e fixtures: a fresh server per test with the synthetic packs
 * imported, a stand-in for the Web Speech API that records what was
 * spoken, and a check that the page logged no errors (CSP violations
 * included).
 */

import { test as base, expect } from '@playwright/test';

import { ARTICLE_PACK, BOOK_PACK } from './pack.js';
import { startServer } from './server.js';

/**
 * @typedef {{ url: string, dbPath: string, api: (method: string, path: string, body?: unknown) => Promise<any> }} App
 */

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
  const synth = {
    getVoices: () => [],
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
  /** @type {import('@playwright/test').Fixtures<{ app: App, pageErrors: string[] }, {}, import('@playwright/test').PlaywrightTestArgs & import('@playwright/test').PlaywrightTestOptions>} */ ({
    app: async ({}, use) => {
      const server = await startServer();
      /** @type {App['api']} */
      const api = async (method, path, body) => {
        const response = await fetch(`${server.url}${path}`, {
          method,
          headers: body === undefined ? {} : { 'content-type': 'application/json' },
          body: body === undefined ? undefined : JSON.stringify(body),
        });
        const text = await response.text();
        if (!response.ok) throw new Error(`${method} ${path}: ${response.status} ${text}`);
        return text ? JSON.parse(text) : null;
      };
      for (const pack of [BOOK_PACK, ARTICLE_PACK]) {
        const report = await api('POST', '/api/import/commit', { text: JSON.stringify(pack) });
        if (report.packs[0].status !== 'imported') throw new Error(`import failed: ${JSON.stringify(report)}`);
      }
      // Four new words per block: exactly the book's words in a book-scoped block.
      await api('PATCH', '/api/settings', { new_per_day: 4, block_size: 10 });
      await use({ url: server.url, dbPath: server.dbPath, api });
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

/** Wait out the 250 ms input guard after a reveal. */
export function waitGuard() {
  return new Promise((resolve) => setTimeout(() => resolve(undefined), 300));
}
