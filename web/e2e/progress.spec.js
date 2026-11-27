// @ts-check
/**
 * M9: Progress, problem words and exports (design §6.5, §7.1 screens 4, 9
 * and 10, §8.1; R-55, R-57, R-72, R-86). Each screen and action on the
 * phone (WebKit, touch) and the desktop (Chromium). The learning history
 * is written into the database relative to the server's study day, so
 * every number on the screen is known in advance.
 */

import { readFile } from 'node:fs/promises';

import { expect, seedHistory, test, waitGuard } from './fixtures.js';

/**
 * The page never scrolls sideways (phone first).
 *
 * @param {import('@playwright/test').Page} page
 */
async function expectNoSidewaysScroll(page) {
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(0);
}

/**
 * Tap on the phone, click on the desktop.
 *
 * @param {import('@playwright/test').Locator} locator
 * @param {import('@playwright/test').TestInfo} info
 */
function press(locator, info) {
  return info.project.name === 'webkit-phone' ? locator.tap() : locator.click();
}

test('R-72 Progress shows each number with what it means', async ({ page, app, pageErrors }, info) => {
  void pageErrors;
  await seedHistory(app);
  await page.goto(`${app.url}/#/`);
  await press(page.getByRole('link', { name: 'Progress', exact: true }), info);
  await expect(page).toHaveURL(/#\/progress$/);
  await expect(page.getByTestId('progress-due-now')).toHaveText('1 card is due today (what Today’s blocks still hold).');
  await expect(page.getByTestId('progress-week')).toContainText('2 reviews come due in the next 7 days.');
  await expect(page.getByTestId('progress-totals')).toHaveText('5 words: 1 known, 1 familiar, 1 problem, 2 new.');
  await expect(page.getByTestId('status-all').getByRole('listitem')).toHaveText(['1 known', '1 familiar', '1 problem', '2 new']);
  const book = page.getByTestId('source-progress').filter({ hasText: 'An E2E Test Novel' });
  await expect(book).toContainText('Reading position: Chapter 7');
  await expect(book).toContainText('4 words: 1 known, 1 familiar, 1 problem, 1 new.');
  await expect(page.getByTestId('retention-understand')).toContainText(
    /^Understand: you remembered the word in 1 of 2 tests \(50%\); the app aims for 9\d%\. Too few tests yet to compare\.$/,
  );
  await expect(page.getByTestId('progress-activity')).toContainText('2 ratings and 1 new word.');
  // Charts are pictures of numbers that are also there as text.
  await expect(page.locator('svg.chart').first()).toHaveAttribute('aria-hidden', 'true');
  await expect(page.locator('.sr-only').filter({ hasText: 'Reviews due —' })).toHaveCount(1);
  await expect(page.getByTestId('leech')).toHaveCount(1);
  await expect(page.getByTestId('leech')).toContainText('Understand: forgotten on 4 different days.');
  await expectNoSidewaysScroll(page);
  // No streaks, scores or goals (R-74).
  await expect(page.locator('main')).not.toContainText(/streak|points|score/i);
});

test('R-55 a problem word on Progress: keep, undo, lower usefulness, suspend', async ({ page, app, pageErrors }, info) => {
  void pageErrors;
  await seedHistory(app);
  await page.goto(`${app.url}/#/progress`);
  const leech = page.getByTestId('leech');
  await expect(leech).toContainText('brontle');
  await expect(leech).toContainText('New problem');
  await expect(leech).toContainText('No memory hint yet');

  await press(leech.getByTestId('leech-keep'), info);
  const toast = page.getByTestId('progress-toast');
  await expect(toast).toContainText('brontle: Kept as it is.');
  await expect(leech).not.toContainText('New problem');
  await expect(leech).toContainText('you chose to keep practising it');
  await press(toast.getByRole('button', { name: 'Undo' }), info);
  await expect(leech).toContainText('New problem');

  await expect(leech.getByTestId('leech-usefulness-3')).toBeHidden();
  await press(leech.getByTestId('leech-usefulness'), info);
  await expect(leech.getByRole('group', { name: /Lower the usefulness of brontle \(now 8\)/ }).getByRole('button')).toHaveText([
    '7', '6', '5', '4', '3', '2', '1',
  ]);
  await press(leech.getByTestId('leech-usefulness-3'), info);
  await expect(toast).toContainText('Usefulness lowered to 3');
  const rows = await app.api('GET', '/api/words?q=brontle');
  expect(rows[0].usefulness).toBe(3);
  // Usefulness 3: "Suspend word" is now the suggested choice.
  await expect(leech.getByTestId('leech-suspend')).toHaveClass(/primary/);

  await press(leech.getByTestId('leech-suspend'), info);
  await expect(toast).toContainText('Suspended');
  await expect(page.getByTestId('no-leeches')).toBeVisible();
  await expect(page.getByTestId('progress-totals')).toContainText('1 suspended');
});

test('R-55 the leech screen opens on the answer side of a flagged card', async ({ page, app, pageErrors }, info) => {
  void pageErrors;
  await seedHistory(app);
  await app.api('PATCH', '/api/settings', { new_per_day: 0 });
  await page.goto(`${app.url}/#/`);
  await expect(page.getByTestId('problem-words')).toHaveText('1 problem word: see Progress.');
  await press(page.getByTestId('start'), info);
  await expect(page).toHaveURL(/#\/practice$/);
  await expect(page.getByTestId('headword-question')).toHaveText('brontle');
  await expect(page.getByTestId('leech-panel')).toHaveCount(0);
  await press(page.getByTestId('reveal'), info);
  const panel = page.getByTestId('leech-panel');
  await expect(panel).toContainText('This word keeps slipping');
  await expect(panel).toContainText('No memory hint yet');
  // brontle (usefulness 8, active): all four choices.
  await expect(panel.locator('.actions > button')).toHaveText([
    'Keep practising',
    'Recognise only (no typing)',
    'Lower usefulness',
    'Suspend word',
  ]);
  await press(panel.getByTestId('leech-keep'), info);
  await expect(page.getByTestId('leech-done')).toContainText('Kept as it is.');
  // Then rate as usual.
  await waitGuard();
  await press(page.getByRole('button', { name: /^Rate 2,/ }), info);
  await expect(page.getByText('Block complete')).toBeVisible();
  const cards = (await app.api('GET', `/api/words/${(await app.api('GET', '/api/words?q=brontle'))[0].id}`)).cards;
  expect(cards[0].leech).toBe('acknowledged');
});

test('R-55 suspending on the leech screen takes the card out of the block', async ({ page, app, pageErrors }, info) => {
  void pageErrors;
  await seedHistory(app);
  await app.api('PATCH', '/api/settings', { new_per_day: 0 });
  await page.goto(`${app.url}/#/`);
  await press(page.getByTestId('start'), info);
  await press(page.getByTestId('reveal'), info);
  await press(page.getByTestId('leech-panel').getByTestId('leech-suspend'), info);
  await expect(page.getByText('Block complete')).toBeVisible();
  const rows = await app.api('GET', '/api/words?q=brontle');
  expect(rows[0].suspended).toBe(true);
});

test('R-55 a choice in practice can be undone where it was made, before or after the rating', async ({ page, app, pageErrors }, info) => {
  void pageErrors;
  await seedHistory(app);
  await app.api('PATCH', '/api/settings', { new_per_day: 0 });
  const id = (await app.api('GET', '/api/words?q=brontle'))[0].id;
  const leechOf = async () => (await app.api('GET', `/api/words/${id}`)).cards[0].leech;
  await page.goto(`${app.url}/#/`);
  await press(page.getByTestId('start'), info);
  await press(page.getByTestId('reveal'), info);
  const panel = page.getByTestId('leech-panel');

  // A mis-tap on "Suspend word": the toast's Undo brings the card back,
  // on its answer side with the choices.
  await press(panel.getByTestId('leech-suspend'), info);
  await expect(page.getByText('Block complete')).toBeVisible();
  const toast = page.getByTestId('toast');
  await expect(toast).toContainText('Suspended');
  await press(toast.getByRole('button', { name: 'Undo' }), info);
  await expect(page.getByTestId('toast')).toContainText('Choice undone');
  await expect(panel.getByTestId('leech-suspend')).toBeVisible();
  expect((await app.api('GET', '/api/words?q=brontle'))[0].suspended).toBe(false);
  expect(await leechOf()).toBe('flagged');

  // The same with ↶ (the choice is the latest thing done).
  await press(panel.getByTestId('leech-suspend'), info);
  await expect(page.getByText('Block complete')).toBeVisible();
  await expect(page.getByTestId('undo')).toHaveAttribute('aria-label', 'Undo the problem-word choice (U)');
  await press(page.getByTestId('undo'), info);
  await expect(panel.getByTestId('leech-suspend')).toBeVisible();
  expect((await app.api('GET', '/api/words?q=brontle'))[0].suspended).toBe(false);

  // Keep practising, then rate: ↶ takes back the rating first, then the
  // choice.
  await press(panel.getByTestId('leech-keep'), info);
  await expect(page.getByTestId('leech-done')).toContainText('Kept as it is.');
  await waitGuard();
  await press(page.getByRole('button', { name: /^Rate 2,/ }), info);
  await expect(page.getByText('Block complete')).toBeVisible();
  await expect(page.getByTestId('undo')).toHaveAttribute('aria-label', 'Undo last rating (U)');
  await press(page.getByTestId('undo'), info);
  await expect(page.getByTestId('leech-done')).toContainText('Kept as it is.');
  expect(await leechOf()).toBe('acknowledged');
  await press(page.getByTestId('undo'), info);
  await expect(panel.getByTestId('leech-keep')).toBeVisible();
  expect(await leechOf()).toBe('flagged');
});

test('R-55 Words points problem words to Progress', async ({ page, app, pageErrors }, info) => {
  void pageErrors;
  await seedHistory(app);
  await page.goto(`${app.url}/#/words`);
  await expect(page.getByTestId('leech-hint')).toBeHidden();
  await press(page.getByTestId('filters-summary'), info);
  await page.getByRole('radio', { name: 'Problem word' }).check();
  await expect(page.getByTestId('word-count')).toHaveText('1 word');
  await press(page.getByTestId('leech-hint').getByRole('link'), info);
  await expect(page).toHaveURL(/#\/progress$/);
});

test('R-57 R-86 Settings shows the feedback for Claude and downloads both exports', async ({ page, app, pageErrors }, info) => {
  void pageErrors;
  await seedHistory(app);
  await page.goto(`${app.url}/#/settings`);
  await press(page.getByTestId('show-feedback'), info);
  const text = page.getByTestId('feedback-text');
  await expect(text).toBeVisible();
  const feedback = JSON.parse(await text.inputValue());
  expect(feedback.format).toBe('vocab-feedback');
  expect(feedback.needs_hint.map((/** @type {{ word: string }} */ w) => w.word)).toEqual(['brontle']);
  await expectNoSidewaysScroll(page);

  for (const [testId, format] of /** @type {const} */ ([
    ['download-feedback', 'vocab-feedback'],
    ['download-full', 'vocab-full-export'],
  ])) {
    const download = page.waitForEvent('download');
    await press(page.getByTestId(testId), info);
    const file = await (await download).path();
    const body = JSON.parse(await readFile(file, 'utf8'));
    expect(body.format).toBe(format);
  }
});
