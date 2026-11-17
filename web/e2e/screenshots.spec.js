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
  });
}
