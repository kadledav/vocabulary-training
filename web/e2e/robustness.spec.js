// @ts-check
/**
 * Regression tests of the M4 fix round (review probes and guarantor
 * decisions H1–H11): double taps, lost or stalled rating requests, focus,
 * Esc with the help open, Czech keyboard layout, wake lock, slip pulse,
 * and the rating bar never hiding the end of the answer.
 */

import { expect, startBookBlock, test, waitGuard } from './fixtures.js';

/**
 * Collect the bodies of the rating posts.
 *
 * @param {import('@playwright/test').Page} page
 */
function reviewPosts(page) {
  /** @type {any[]} */
  const posts = [];
  page.on('request', (request) => {
    if (request.url().endsWith('/reviews') && request.method() === 'POST') posts.push(JSON.parse(request.postData() ?? '{}'));
  });
  return posts;
}

/**
 * The browser logs failed requests that a test causes on purpose; drop
 * exactly those from the page errors (anything else still fails the test).
 *
 * @param {string[]} errors
 * @param {RegExp} pattern
 */
function forgive(errors, pattern) {
  for (let i = errors.length - 1; i >= 0; i -= 1) if (pattern.test(errors[i])) errors.splice(i, 1);
}
const LOST = /^console: Failed to load resource: net::ERR_FAILED$/;
const CONFLICT = /^console: Failed to load resource: the server responded with a status of 409 \(Conflict\)$/;

/** @param {import('./fixtures.js').App} app */
const current = (app) => app.api('GET', '/api/sessions/current');

/**
 * Tap (phone) or click (desktop) twice at the centre of `locator`, `gap` ms apart.
 *
 * @param {import('@playwright/test').Page} page
 * @param {import('@playwright/test').Locator} locator
 * @param {boolean} touch
 * @param {number} gap
 */
async function doubleTap(page, locator, touch, gap) {
  const box = await locator.boundingBox();
  if (!box) throw new Error('target not visible');
  const x = box.x + box.width / 2;
  const y = box.y + box.height / 2;
  if (touch) await page.touchscreen.tap(x, y);
  else await page.mouse.click(x, y);
  await page.waitForTimeout(gap);
  if (touch) await page.touchscreen.tap(x, y);
  else await page.mouse.click(x, y);
}

/**
 * Let the first rating post reach the server but lose its answer (or lose
 * the request before it arrives), like a dropped connection.
 *
 * @param {import('@playwright/test').Page} page
 * @param {{ commit: boolean }} options
 */
async function loseNextRatingAnswer(page, { commit }) {
  let armed = true;
  await page.route('**/reviews', async (route) => {
    if (!armed) {
      await route.continue();
      return;
    }
    armed = false;
    if (commit) await route.fetch();
    await route.abort('failed');
  });
}

/** @param {import('@playwright/test').Page} page */
async function reveal(page) {
  await page.getByTestId('reveal').click();
  await waitGuard();
}

test('R-62 a double tap on a rating does not act on the next card (H1)', async ({ page, app, pageErrors }, info) => {
  void pageErrors;
  const touch = info.project.name === 'webkit-phone';
  const posts = reviewPosts(page);
  await startBookBlock(page, app, { touch });
  await reveal(page);
  // The second tap lands where "Show answer" of the next card appears.
  await doubleTap(page, page.getByRole('button', { name: /^Rate 2,/ }), touch, 130);
  await page.waitForTimeout(300);
  await expect(page.getByTestId('reveal')).toBeVisible();
  await expect(page.getByTestId('progress')).toHaveText('2 / 4');
  expect(posts).toHaveLength(1);

  // A double tap on a 5 must not skip the "Next" step either.
  await page.getByTestId('reveal').click();
  await waitGuard();
  await doubleTap(page, page.getByRole('button', { name: /^Rate 5,/ }), touch, 130);
  await page.waitForTimeout(300);
  await expect(page.getByTestId('next')).toBeVisible();
  await expect(page.getByTestId('headword')).toHaveText('brontle');
  expect(posts).toHaveLength(2);
});

test('R-54 a rating whose answer was lost is retried with the same id, never silently changed (H2)', async ({
  page,
  app,
  pageErrors,
}) => {
  const posts = reviewPosts(page);
  await startBookBlock(page, app);
  await loseNextRatingAnswer(page, { commit: true });
  await reveal(page);
  await page.getByRole('button', { name: /^Rate 2,/ }).click();
  await expect(page.getByTestId('unsure')).toContainText(
    'Your rating 2 · Knew it may not have been saved because the server could not be reached.',
  );
  // Another grade is not sent while the first one is unresolved.
  await page.keyboard.press('3');
  await page.waitForTimeout(200);
  expect(posts).toHaveLength(1);

  await page.getByTestId('retry').click();
  await expect(page.getByTestId('toast')).toContainText('Rated 2 · Knew it');
  await expect(page.getByTestId('progress')).toHaveText('2 / 4');
  expect(posts.map((p) => [p.raw_grade, p.client_review_id])).toEqual([
    [2, posts[0].client_review_id],
    [2, posts[0].client_review_id],
  ]);
  const session = await current(app);
  expect(session.answered).toBe(1);
  expect(session.items[0].answered.raw_grade).toBe(2);
  forgive(pageErrors, LOST);
});

test('R-54 "Rate differently" first finds out what was saved and offers Undo (H2)', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await startBookBlock(page, app);
  await loseNextRatingAnswer(page, { commit: true });
  await reveal(page);
  await page.getByRole('button', { name: /^Rate 2,/ }).click();
  await page.getByTestId('rate-differently').click();
  const toast = page.getByTestId('toast');
  await expect(toast).toContainText("Already saved as 2 · Knew it — undo if that's wrong.");
  await expect(page.getByTestId('progress')).toHaveText('2 / 4');
  await toast.getByRole('button', { name: 'Undo' }).click();
  await expect(page.getByTestId('headword')).toHaveText('glimmerous');
  await waitGuard();
  await page.getByRole('button', { name: /^Rate 4,/ }).click();
  await expect(page.getByTestId('next')).toBeVisible();
  expect((await current(app)).items[0].answered.raw_grade).toBe(4);
  forgive(pageErrors, LOST);
});

test('R-54 "Rate differently" after a request that never arrived allows any grade (H2)', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await startBookBlock(page, app);
  await loseNextRatingAnswer(page, { commit: false });
  await reveal(page);
  await page.getByRole('button', { name: /^Rate 2,/ }).click();
  await expect(page.getByTestId('unsure')).toBeVisible();
  await page.getByTestId('rate-differently').click();
  await expect(page.locator('button.rate')).toHaveCount(5);
  await waitGuard();
  await page.getByRole('button', { name: /^Rate 3,/ }).click();
  await expect(page.getByTestId('toast')).toContainText('Rated 3 · Shaky');
  expect((await current(app)).items[0].answered.raw_grade).toBe(3);
  forgive(pageErrors, LOST);
});

test('R-54 a rating already saved elsewhere is shown, in plain words, with Undo (H2)', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await startBookBlock(page, app);
  await reveal(page);
  // Another window rates the same card meanwhile.
  const block = await current(app);
  await app.api('POST', `/api/sessions/${block.id}/reviews`, {
    client_review_id: crypto.randomUUID(),
    item_id: block.items[0].item_id,
    raw_grade: 1,
  });
  await page.getByRole('button', { name: /^Rate 3,/ }).click();
  await expect(page.getByTestId('toast')).toContainText("Already saved as 1 · Instantly — undo if that's wrong.");
  await expect(page.getByTestId('progress')).toHaveText('2 / 4');
  const text = await page.locator('.practice').innerText();
  expect(text).not.toMatch(/client_review_id|already_answered|_id\b/);
  forgive(pageErrors, CONFLICT);
});

test('R-54 a stalled request shows "Saving…" and gives up after 15 s (H3)', async ({ page, app, pageErrors }, info) => {
  test.skip(info.project.name !== 'chromium-desktop', 'one viewport is enough for a 15 s wait');
  test.slow(); // waits out the 15 s request timeout
  void pageErrors;
  await startBookBlock(page, app);
  await page.route('**/reviews', () => {}); // never answers
  await reveal(page);
  await page.getByRole('button', { name: /^Rate 2,/ }).click();
  await expect(page.getByTestId('saving')).toHaveText('Saving…');
  await expect(page.getByTestId('unsure')).toContainText(
    'Your rating 2 · Knew it may not have been saved because the server did not answer in time.',
    { timeout: 20_000 },
  );
  await expect(page.getByTestId('saving')).toHaveText('');
  await page.unroute('**/reviews');
  await page.getByTestId('retry').click();
  await expect(page.getByTestId('toast')).toContainText('Rated 2 · Knew it');
});

test('R-54 undo after an unsure rating forgets it: the next rating gets a new id (H4)', async ({ page, app, pageErrors }) => {
  void pageErrors;
  const posts = reviewPosts(page);
  await startBookBlock(page, app);
  await reveal(page);
  await page.getByRole('button', { name: /^Rate 2,/ }).click();
  await expect(page.getByTestId('progress')).toHaveText('2 / 4');
  await loseNextRatingAnswer(page, { commit: true });
  await reveal(page);
  await page.getByRole('button', { name: /^Rate 3,/ }).click();
  await expect(page.getByTestId('unsure')).toBeVisible();
  // The lost rating was stored, so undo reverts it.
  await page.keyboard.press('u');
  await expect(page.getByTestId('headword')).toHaveText('brontle');
  await expect(page.locator('button.rate')).toHaveCount(5);
  await waitGuard();
  await page.keyboard.press('3');
  await expect(page.getByTestId('toast')).toContainText('Rated 3 · Shaky');
  await expect(page.getByTestId('practice-error')).toHaveText('');
  expect(posts).toHaveLength(3);
  expect(posts[2].client_review_id).not.toBe(posts[1].client_review_id);
  expect((await current(app)).answered).toBe(2);
  forgive(pageErrors, LOST);
});

test('R-70 focus stays on the More/Less toggle after using it (H5)', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await startBookBlock(page, app);
  await reveal(page);
  const toggle = page.locator('button.more-toggle');
  await toggle.click();
  await expect(toggle).toBeFocused();
  await expect(toggle).toHaveAttribute('aria-expanded', 'false');
  await page.keyboard.press('Enter');
  await expect(toggle).toBeFocused();
  await expect(toggle).toHaveAttribute('aria-expanded', 'true');
});

test('R-70 Esc with the help open closes only the help (H7)', async ({ page, app, pageErrors }, info) => {
  test.skip(info.project.name !== 'chromium-desktop', 'keyboard');
  void pageErrors;
  await startBookBlock(page, app);
  await page.keyboard.press('?');
  await expect(page.getByRole('heading', { name: 'Keys' })).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(page.getByRole('heading', { name: 'Keys' })).toHaveCount(0);
  await expect(page).toHaveURL(/#\/practice$/);
  await page.keyboard.press('Escape');
  await expect(page).toHaveURL(/#\/summary$/);
});

test('R-70 on a Czech keyboard the digit row rates without Shift (ě = 2) (H8)', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await startBookBlock(page, app);
  await reveal(page);
  await page.evaluate(() => {
    document.dispatchEvent(new KeyboardEvent('keydown', { key: 'ě', code: 'Digit2', bubbles: true }));
  });
  await expect(page.getByTestId('toast')).toContainText('Rated 2 · Knew it');
  expect((await current(app)).items[0].answered.raw_grade).toBe(2);
  // U by its physical key on a non-Latin layout.
  await page.evaluate(() => {
    document.dispatchEvent(new KeyboardEvent('keydown', { key: 'г', code: 'KeyU', bubbles: true }));
  });
  await expect(page.getByTestId('headword')).toHaveText('glimmerous');
});

test('R-70 the screen stays awake while practising, again after the page was hidden (H9)', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await page.addInitScript(() => {
    const w = /** @type {any} */ (window);
    w.wakeRequests = 0;
    w.pageVisible = 'visible';
    Object.defineProperty(document, 'visibilityState', { get: () => w.pageVisible, configurable: true });
    Object.defineProperty(navigator, 'wakeLock', {
      configurable: true,
      value: {
        request: async () => {
          w.wakeRequests += 1;
          return { release: async () => {} };
        },
      },
    });
  });
  const requests = () => page.evaluate(() => /** @type {any} */ (window).wakeRequests);
  // No block: no wake lock.
  await page.goto(`${app.url}/#/practice`);
  await expect(page.getByRole('heading', { name: 'No block in progress' })).toBeVisible();
  expect(await requests()).toBe(0);

  await startBookBlock(page, app);
  await expect.poll(requests).toBe(1);
  await page.evaluate(() => {
    const w = /** @type {any} */ (window);
    w.pageVisible = 'hidden';
    document.dispatchEvent(new Event('visibilitychange'));
    w.pageVisible = 'visible';
    document.dispatchEvent(new Event('visibilitychange'));
  });
  await expect.poll(requests).toBe(2);
  // Leaving practice: no more requests.
  await page.goto(`${app.url}/#/`);
  await page.evaluate(() => document.dispatchEvent(new Event('visibilitychange')));
  expect(await requests()).toBe(2);
});

test('R-62 the slip-guard toast pulses exactly once (H11)', async ({ page, app, pageErrors }, info) => {
  test.skip(info.project.name !== 'chromium-desktop', 'keyboard');
  void pageErrors;
  await startBookBlock(page, app);
  for (const grade of [2, 5, 2, 2]) {
    await page.keyboard.press('Space');
    await waitGuard();
    await page.keyboard.press(String(grade));
    await expect(page.getByTestId('toast')).toContainText(`Rated ${grade}`);
    if (grade === 5) await page.keyboard.press('Enter');
  }
  // brontle's repeat (last rated 5): a reflex 1 right after the guard.
  await expect(page.getByTestId('headword-question')).toHaveText('brontle');
  await page.keyboard.press('Space');
  await waitGuard();
  await page.keyboard.press('1');
  const toast = page.getByTestId('toast');
  await expect(toast).toHaveClass(/pulse/);
  expect(await toast.evaluate((el) => getComputedStyle(el).animationIterationCount)).toBe('1');
});

test('R-74 with More open the end of the answer scrolls clear of the rating bar (G)', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await page.setViewportSize({ width: 375, height: 812 });
  await startBookBlock(page, app);
  await reveal(page);
  await expect(page.getByTestId('more')).toBeVisible();
  await page.evaluate(() => window.scrollTo(0, document.documentElement.scrollHeight));
  await page.waitForTimeout(100);
  const geometry = await page.evaluate(() => {
    const more = document.querySelector('[data-testid="more"]');
    const bar = document.querySelector('.practice-bottom');
    if (!more || !bar) return null;
    const last = more.lastElementChild ?? more;
    return { lastBottom: last.getBoundingClientRect().bottom, barTop: bar.getBoundingClientRect().top };
  });
  expect(geometry).not.toBeNull();
  expect(/** @type {any} */ (geometry).lastBottom).toBeLessThanOrEqual(/** @type {any} */ (geometry).barTop);
});
