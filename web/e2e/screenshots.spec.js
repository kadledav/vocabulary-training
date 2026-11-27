// @ts-check
/**
 * Screenshots of the key screens for review (not a test of behaviour).
 * Runs only when SCREENSHOT_DIR is set: phone and desktop, dark and light.
 */

import path from 'node:path';

import { expect, startBookBlock, test, waitGuard, waitingProduce, waitingSpelling } from './fixtures.js';

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

/** M8: the Produce card's question and answer states, Today and Settings. */
for (const theme of /** @type {const} */ (['dark', 'light'])) {
  test(`M8 Produce screens, ${theme}`, async ({ page, app }, info) => {
    test.skip(!dir, 'set SCREENSHOT_DIR to take screenshots');
    const shot = (/** @type {string} */ name) =>
      page.screenshot({ path: path.join(/** @type {string} */ (dir), `m8-${info.project.name}-${theme}-${name}.png`) });
    await app.api('PATCH', '/api/settings', { theme, new_typed_per_day: 3 });
    await waitingProduce(app, ['glimmerous', 'brontle', 'quabble', 'snorkish']);
    await page.goto(`${app.url}/#/`);
    await expect(page.getByTestId('typed-waiting')).toBeVisible();
    await shot('1-today');
    await page.getByTestId('start').click();
    await expect(page.getByTestId('answer-input')).toBeVisible();
    await shot('2-question');
    await page.getByTestId('answer-input').fill('glimmerous');
    await page.getByTestId('check').click();
    await expect(page.getByTestId('check-result')).toBeVisible();
    await shot('3-answer-correct');
    await waitGuard();
    await page.getByRole('button', { name: /^Rate 2,/ }).click();
    await expect(page.getByTestId('cloze')).toBeVisible();
    await page.waitForTimeout(300);
    await page.getByTestId('hint-button').click();
    await page.getByTestId('answer-input').fill('brontel');
    await shot('4-question-hint-typed');
    if (info.project.name === 'webkit-phone') {
      // The on-screen keyboard (45 % of the screen): the bar sits above it.
      await page.evaluate(() => {
        const vv = /** @type {VisualViewport} */ (window.visualViewport);
        Object.defineProperty(vv, 'height', { configurable: true, get: () => 450 });
        vv.dispatchEvent(new Event('resize'));
      });
      await shot('4b-question-keyboard-450px');
      await page.evaluate(() => {
        const vv = /** @type {VisualViewport} */ (window.visualViewport);
        Object.defineProperty(vv, 'height', { configurable: true, get: () => window.innerHeight });
        vv.dispatchEvent(new Event('resize'));
      });
    }
    await page.getByTestId('check').click();
    await expect(page.getByTestId('typed')).toBeVisible();
    await shot('5-answer-not-quite');
    await page.getByTestId('accept-typed').click();
    await expect(page.getByTestId('accepted-note')).toBeVisible();
    await waitGuard();
    await page.getByRole('button', { name: /^Rate 4,/ }).click();
    await expect(page.getByTestId('next')).toBeVisible();
    await shot('6-feedback-after-4');
    await page.getByTestId('next').click();
    await page.waitForTimeout(300);
    await page.getByTestId('dont-know').click();
    await shot('7-answer-dont-know');
    await page.goto(`${app.url}/#/settings`);
    await expect(page.getByLabel('New Produce cards per day')).toBeVisible();
    await page.getByLabel('New Produce cards per day').scrollIntoViewIfNeeded();
    await shot('8-settings');
  });
}

/** M10: the Spelling card's question and answer states. */
for (const theme of /** @type {const} */ (['dark', 'light'])) {
  test(`M10 Spelling screens, ${theme}`, async ({ page, app }, info) => {
    test.skip(!dir, 'set SCREENSHOT_DIR to take screenshots');
    const shot = (/** @type {string} */ name) =>
      page.screenshot({ path: path.join(/** @type {string} */ (dir), `m10-${info.project.name}-${theme}-${name}.png`) });
    await app.api('PATCH', '/api/settings', { theme });
    await waitingSpelling(app, ['glimmerous', 'brontle', 'quabble', 'snorkish'], 'Two m, then -ous.');
    await page.goto(`${app.url}/#/`);
    await expect(page.getByTestId('typed-waiting')).toBeVisible();
    await shot('1-today');
    await page.getByTestId('start').click();
    await expect(page.getByTestId('listen-status')).toHaveText(/Tap/);
    await shot('2-question-tap-to-listen');
    await page.getByTestId('listen').click();
    await page.getByTestId('answer-input').fill('glimerous');
    await shot('3-question-heard-typed');
    if (info.project.name === 'webkit-phone') {
      await page.evaluate(() => {
        const vv = /** @type {VisualViewport} */ (window.visualViewport);
        Object.defineProperty(vv, 'height', { configurable: true, get: () => 450 });
        vv.dispatchEvent(new Event('resize'));
      });
      await shot('3b-question-keyboard-450px');
      await page.evaluate(() => {
        const vv = /** @type {VisualViewport} */ (window.visualViewport);
        Object.defineProperty(vv, 'height', { configurable: true, get: () => window.innerHeight });
        vv.dispatchEvent(new Event('resize'));
      });
    }
    await page.getByTestId('check').click();
    await expect(page.getByTestId('spell-diff')).toBeVisible();
    await shot('4-answer-letter-diff');
    await waitGuard();
    await page.getByRole('button', { name: /^Rate 4,/ }).click();
    await expect(page.getByTestId('next')).toBeVisible();
    await shot('5-feedback-after-4');
    await page.getByTestId('next').click();
    await page.waitForTimeout(300);
    await page.getByTestId('answer-input').fill('Brontle');
    await page.getByTestId('check').click();
    await expect(page.getByTestId('check-result')).toHaveAttribute('data-match', 'exact');
    await shot('6-answer-correct');
    await waitGuard();
    await page.getByRole('button', { name: /^Rate 2,/ }).click();
    await page.waitForTimeout(300);
    await page.getByTestId('answer-input').fill('quabbel the long way round to test wrapping');
    await page.getByTestId('check').click();
    await expect(page.getByTestId('spell-diff')).toBeVisible();
    await shot('7-answer-long-diff');
    await waitGuard();
    await page.getByRole('button', { name: /^Rate 3,/ }).click();
    await page.waitForTimeout(300);
    await page.getByTestId('dont-know').click();
    await shot('8-answer-dont-know');
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

/** M7: the Words list and a word's screen, phone, both themes. */
for (const theme of /** @type {const} */ (['dark', 'light'])) {
  test(`M7 words screens, ${theme}`, async ({ page, app }, info) => {
    test.skip(!dir || info.project.name !== 'webkit-phone', 'set SCREENSHOT_DIR; phone project only');
    const shot = (/** @type {string} */ name, fullPage = false) =>
      page.screenshot({ path: path.join(/** @type {string} */ (dir), `m7-${theme}-${name}.png`), fullPage });
    await app.api('PATCH', '/api/settings', { theme });
    const rows = await app.api('GET', '/api/words?q=glimmerous');
    const id = rows[0].id;
    // A suggestion from a later pack, and one review for the history.
    await app.api('PATCH', `/api/words/${id}`, { meaning: 'Glowing on and off (my own words).' });
    const { BOOK_PACK } = await import('./pack.js');
    await app.api('POST', '/api/import/commit', {
      text: JSON.stringify({ ...BOOK_PACK, pack_id: 'test-e2e-novel--2026-11-02--1' }),
    });
    const start = await app.api('POST', '/api/sessions', { kind: 'start' });
    const item = start.session.items.find((/** @type {any} */ i) => i.word.headword === 'glimmerous');
    await app.api('POST', `/api/sessions/${start.session.id}/reviews`, {
      client_review_id: crypto.randomUUID(),
      item_id: item.item_id,
      raw_grade: 2,
    });

    await page.goto(`${app.url}/#/words`);
    await expect(page.getByTestId('word-count')).toHaveText('5 words');
    await shot('1-list');
    await page.getByTestId('filters-summary').click();
    await page.getByRole('radio', { name: 'Most useful' }).check();
    await page.getByRole('radio', { name: 'New', exact: true }).check();
    await expect(page.getByTestId('word-count')).toHaveText('4 words');
    await shot('2-list-filters', true);
    await page.goto(`${app.url}/#/words/${id}`);
    await expect(page.getByTestId('conflicts')).toBeVisible();
    await shot('3-word-top');
    await shot('4-word-full', true);
    await page.getByRole('button', { name: 'Edit meaning' }).click();
    await shot('5-field-editor');
    await page.getByRole('button', { name: 'Cancel' }).click();
    await page.getByTestId('list-czech').scrollIntoViewIfNeeded();
    await shot('6-lists');
    const sentence = page.locator('ol.sentences li').first();
    await sentence.getByRole('button', { name: 'Edit' }).click();
    await sentence.scrollIntoViewIfNeeded();
    await shot('7-sentence-editor');
    await sentence.getByRole('button', { name: 'Cancel' }).click();
    await page.getByTestId('reset-understand').click();
    await page.getByTestId('card-understand').scrollIntoViewIfNeeded();
    await shot('8-reset-confirm');
    await page.getByTestId('delete-word').click();
    await page.getByTestId('delete-word').or(page.getByTestId('delete-word-confirm')).scrollIntoViewIfNeeded();
    await shot('9-delete-confirm');
    await page.getByRole('button', { name: 'Raise usefulness' }).click();
    await expect(page.getByTestId('toast')).toBeVisible();
    await shot('10-toast-undo');
  });
}
