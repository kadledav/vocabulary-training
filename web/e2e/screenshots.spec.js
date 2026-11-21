// @ts-check
/**
 * Screenshots of the key screens for review (not a test of behaviour).
 * Runs only when SCREENSHOT_DIR is set: phone and desktop, dark and light.
 */

import path from 'node:path';

import { expect, startBookBlock, test, waitGuard } from './fixtures.js';

const dir = process.env.SCREENSHOT_DIR;

for (const theme of /** @type {const} */ (['dark', 'light'])) {
  // No page-error check here: WebKit's screenshot machinery injects a style
  // element that the CSP refuses (the behaviour tests check the app itself).
  test(`screens in the ${theme} theme`, async ({ page, app }, info) => {
    test.skip(!dir, 'set SCREENSHOT_DIR to take screenshots');
    const shot = (/** @type {string} */ name) =>
      page.screenshot({
        path: path.join(/** @type {string} */ (dir), `${info.project.name}-${theme}-${name}.png`),
      });
    await app.api('PATCH', '/api/settings', { theme });
    await page.goto(`${app.url}/#/`);
    await expect(page.getByTestId('start')).toBeVisible();
    await page.evaluate(() => new Promise((resolve) => setTimeout(() => resolve(undefined), 200)));
    await shot('1-today');
    await startBookBlock(page, app);
    await shot('2-question-sentence');
    await page.getByTestId('reveal').click();
    await shot('3-answer-new-word');
    // The end of "More" scrolls clear of the rating bar and legend.
    await page.evaluate(() => window.scrollTo(0, document.documentElement.scrollHeight));
    await page.waitForTimeout(100);
    await shot('3b-answer-scrolled-to-end');
    await page.evaluate(() => window.scrollTo(0, 0));
    await waitGuard();
    await page.getByRole('button', { name: /^Rate 2,/ }).click();
    await expect(page.getByTestId('headword-question')).toBeVisible();
    await shot('4-question-word-only-toast');
    await page.getByTestId('reveal').click();
    await waitGuard();
    await page.getByRole('button', { name: /^Rate 5,/ }).click();
    await expect(page.getByTestId('next')).toBeVisible();
    await shot('5-feedback-after-5');
    await page.getByTestId('next').click();
    await page.getByRole('button', { name: /^Help/ }).click();
    await shot('6-help');
    // Esc closes the help first, then ends the block.
    await page.keyboard.press('Escape');
    await page.keyboard.press('Escape');
    await expect(page).toHaveURL(/#\/summary$/);
    await shot('7-summary');
    await page.goto(`${app.url}/#/settings`);
    await expect(page.getByLabel('New words per day')).toBeVisible();
    await shot('8-settings');
    await page.getByTestId('logout').click();
    await expect(page.getByLabel('Password')).toBeVisible();
    await page.getByLabel('Password').fill('test-password-wrong');
    await page.getByRole('button', { name: 'Log in' }).click();
    await expect(page.getByTestId('login-status')).toHaveText('Wrong password. Try again.');
    await shot('9-login');
  });
}

/** Fix round R1-C: end of the day, answer side with legend, import done, settings. */
const VIEWS = /** @type {const} */ ([
  ['phone', 375, 812],
  ['small', 320, 568],
  ['landscape', 812, 375],
]);
for (const theme of /** @type {const} */ (['dark', 'light'])) {
  for (const [label, width, height] of VIEWS) {
    test(`R1-C screens, ${theme}, ${label}`, async ({ page, app }, info) => {
      test.skip(!dir || info.project.name !== 'webkit-phone', 'set SCREENSHOT_DIR; phone project only');
      const shot = (/** @type {string} */ name) =>
        page.screenshot({ path: path.join(/** @type {string} */ (dir), `r1c-${theme}-${label}-${name}.png`) });
      await page.setViewportSize({ width, height });
      await app.api('PATCH', '/api/settings', { theme });
      await startBookBlock(page, app);
      await page.getByTestId('reveal').click();
      await expect(page.getByTestId('legend')).toBeVisible();
      await waitGuard();
      await shot('1-answer-legend');
      await page.getByRole('button', { name: /^Rate 2,/ }).click();
      await expect(page.getByTestId('toast')).toBeVisible();
      await shot('2-question-toast');
      // Finish the day through the API: the rest of this block, then the article word.
      const current = await app.api('GET', '/api/sessions/current');
      let session = current;
      for (;;) {
        const item = session.items.find((/** @type {any} */ i) => !i.answered);
        if (!item) break;
        session = (await app.api('POST', `/api/sessions/${session.id}/reviews`, {
          client_review_id: crypto.randomUUID(),
          item_id: item.item_id,
          raw_grade: 3,
        })).session;
      }
      await app.api('POST', `/api/sessions/${session.id}/finish`, {});
      await page.goto(`${app.url}/#/`);
      await page.getByLabel(/All sources/).check();
      await expect(page.getByTestId('end-of-day')).toBeVisible();
      await shot('3-today-end-of-day');
      await page.goto(`${app.url}/#/import`);
      const pack = {
        format: 'vocab-pack',
        format_version: 1,
        pack_id: 'test-e2e-shot--2026-11-02--1',
        exported_at: '2026-11-02',
        source: { id: 'test-e2e-shot', type: 'general', title: 'Screenshot words' },
        words: ['test-shot-alpha', 'test-shot-beta'].map((headword) => ({
          headword,
          meaning: 'An invented word.',
          czech: ['slovo'],
          usefulness: 3,
          sentences: [{ en: `A [[${headword}]] here.`, cs: 'Tady [[slovo]].', origin: 'example' }],
        })),
      };
      await page.getByTestId('file-input').setInputFiles({ name: 'pack.json', mimeType: 'application/json', buffer: Buffer.from(JSON.stringify(pack)) });
      await page.getByTestId('preview').click();
      await page.getByTestId('import').click();
      await expect(page.getByTestId('import-done')).toBeVisible();
      await page.evaluate(() => window.scrollTo(0, 0));
      await shot('4-import-done');
      await page.goto(`${app.url}/#/settings`);
      await page.getByLabel(/New words per day/).fill('101');
      await page.getByLabel(/New words per day/).blur();
      await shot('5-settings');
    });
  }
}
