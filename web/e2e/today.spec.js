// @ts-check
/**
 * Today, Settings, themes and browser speech (R-59, R-54, R-71, R-41).
 */

import { DatabaseSync } from 'node:sqlite';

import { expect, startBookBlock, test } from './fixtures.js';

test('R-59 the scope chooser lists the sources with their reading position and is remembered', async ({
  page,
  app,
  pageErrors,
}) => {
  void pageErrors;
  await page.goto(`${app.url}/#/`);
  await expect(page.getByLabel(/All sources/)).toBeChecked();
  await expect(page.getByText('reading: Chapter 7 · 4 words')).toBeVisible();
  await expect(page.getByTestId('new-left')).toHaveText('New words: 4 of 4 left today');
  await expect(page.locator('ol.forecast li')).toHaveCount(7);

  await page.getByLabel(/An E2E Test Article/).check();
  // Let the Today request of the new scope finish: a reload that cancels it
  // makes WebKit log the cancelled fetch as an error.
  await page.waitForLoadState('networkidle');
  await page.reload();
  await expect(page.getByLabel(/An E2E Test Article/)).toBeChecked();
  await page.getByTestId('start').click();
  await expect(page.getByTestId('progress')).toHaveText('1 / 1');
  await expect(page.getByTestId('headword-question')).toHaveText('flumpet');
});

test('R-54 a backlog pauses new words until "Add new words anyway"', async ({ page, app, pageErrors }, info) => {
  test.skip(info.project.name !== 'chromium-desktop', 'one viewport is enough');
  void pageErrors;
  // 41 reviewed words due today: more than twice the floor of 20 reviews/day.
  const words = Array.from({ length: 41 }, (_, i) => ({
    headword: `test-backlog-${i}`,
    meaning: 'An invented backlog word.',
    czech: ['zkouška'],
    usefulness: 3,
    sentences: [{ en: `A [[test-backlog-${i}]] here.`, cs: `Tady [[zkouška]].`, origin: 'example' }],
  }));
  const pack = {
    format: 'vocab-pack',
    format_version: 1,
    pack_id: 'test-e2e-backlog--2026-11-01--1',
    exported_at: '2026-11-01',
    source: { id: 'test-e2e-backlog', type: 'general', title: 'Backlog' },
    words,
  };
  await app.api('POST', '/api/import/commit', { text: JSON.stringify(pack) });
  const today = await app.api('GET', '/api/today');
  const db = new DatabaseSync(app.dbPath);
  db.prepare(
    `UPDATE card SET state = 'review', stability = 5.0, difficulty = 5.0, due_day = ?, last_review_day = '2026-11-01',
     introduced_day = '2026-11-01', reps = 2, format = 'word_only'
     WHERE word_id IN (SELECT id FROM word WHERE headword LIKE 'test-backlog-%')`,
  ).run(today.study_day);
  db.close();

  await page.goto(`${app.url}/#/`);
  await expect(page.getByTestId('backlog')).toHaveText('New words paused — backlog of 41 reviews.');
  await page.getByTestId('add-new-anyway').click();
  await expect(page).toHaveURL(/#\/practice$/);
  const session = await app.api('GET', '/api/sessions/current');
  expect(session.items.some((/** @type {any} */ i) => i.card.is_new)).toBe(true);
});

test('R-71 dark by default, light and system themes from Settings', async ({ page, app, pageErrors }) => {
  void pageErrors;
  const background = () => page.evaluate(() => getComputedStyle(document.body).backgroundColor);
  const theme = () => page.evaluate(() => document.documentElement.getAttribute('data-theme'));
  await page.emulateMedia({ colorScheme: 'light' });
  await page.goto(`${app.url}/#/settings`);
  await expect(page.getByLabel('Dark')).toBeChecked();
  expect(await theme()).toBe('dark');
  expect(await background()).toBe('rgb(21, 23, 28)');

  await page.getByLabel('Light').check();
  await expect(page.getByTestId('settings-status')).toHaveText('Saved.');
  expect(await theme()).toBe('light');
  expect(await background()).toBe('rgb(247, 247, 245)');
  await page.reload();
  await expect(page.getByLabel('Light')).toBeChecked();
  expect(await background()).toBe('rgb(247, 247, 245)');

  // System follows prefers-color-scheme.
  await page.getByLabel('Same as the system').check();
  await expect(page.getByTestId('settings-status')).toHaveText('Saved.');
  expect(await theme()).toBe(null);
  expect(await background()).toBe('rgb(247, 247, 245)');
  await page.emulateMedia({ colorScheme: 'dark' });
  expect(await background()).toBe('rgb(21, 23, 28)');
});

test('R-54 Settings saves new words per day, block size and the legend', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await page.goto(`${app.url}/#/settings`);
  await page.getByLabel('New words per day').fill('7');
  await page.getByLabel('New words per day').press('Tab');
  await expect(page.getByTestId('settings-status')).toHaveText('Saved.');
  await page.getByLabel('Cards per block').fill('12');
  await page.getByLabel('Cards per block').press('Tab');
  await expect(page.getByTestId('settings-status')).toHaveText('Saved.');
  await page.getByLabel('Off (press ? to see it)').check();
  await expect(page.getByTestId('settings-status')).toHaveText('Saved.');
  const settings = await app.api('GET', '/api/settings');
  expect(settings).toMatchObject({ new_per_day: 7, block_size: 12, legend: 'off' });

  // Legend off: not shown above the rating buttons, still behind "?".
  await startBookBlock(page, app);
  await page.getByTestId('reveal').click();
  await expect(page.getByTestId('legend')).toHaveCount(0);
  await page.getByRole('button', { name: /^Help/ }).click();
  await expect(page.getByTestId('legend')).toBeVisible();
});

test('R-41 play buttons speak with the browser voice (no recorded audio yet)', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await app.api('PATCH', '/api/settings', { voice: 'uk', speed: '0.75' });
  await startBookBlock(page, app);
  await page.getByRole('button', { name: 'Play sentence' }).click();
  await page.getByTestId('reveal').click();
  await page.getByRole('button', { name: 'Play word' }).click();
  const spoken = await page.evaluate(() => /** @type {any} */ (window).spoken);
  expect(spoken).toEqual([
    { text: 'The glimmerous lamp swung above the door.', lang: 'en-GB', rate: 0.75 },
    { text: 'glimmerous', lang: 'en-GB', rate: 0.75 },
  ]);
});
