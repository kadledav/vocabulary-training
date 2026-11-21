// @ts-check
/**
 * Practice UI: Understand (design §17 M4 "done when"): a full block by
 * touch (WebKit phone) and by keyboard (Chromium desktop), the
 * inverted-scale safeguards, resume after reload and undo.
 */

import { expect, headwordOnScreen, startBookBlock, test, waitGuard } from './fixtures.js';

/** The block of the book scope in queue order, with the grade each gets. */
const PLAN = [
  { headword: 'glimmerous', format: 'sentence', grade: 2 },
  { headword: 'brontle', format: 'word_only', grade: 5 },
  { headword: 'quabble', format: 'sentence', grade: 1 },
  { headword: 'snorkish', format: 'word_only', grade: 3 },
  // brontle's in-session repeat (fewer than 3 items were left: appended).
  { headword: 'brontle', format: 'word_only', grade: 2 },
];

/**
 * @param {import('@playwright/test').Page} page
 * @param {{ format: string }} step
 */
async function expectQuestionFormat(page, step) {
  if (step.format === 'sentence') {
    await expect(page.getByTestId('sentence-en')).toBeVisible();
    await expect(page.getByTestId('reveal')).toHaveText('Show translation');
  } else {
    await expect(page.getByTestId('headword-question')).toBeVisible();
    await expect(page.getByTestId('reveal')).toHaveText('Show answer');
  }
}

/**
 * @param {import('@playwright/test').Page} page
 */
async function expectSummary(page) {
  await expect(page).toHaveURL(/#\/summary$/);
  await expect(page.getByRole('heading', { name: 'Done' })).toBeVisible();
  await expect(page.getByTestId('summary-line')).toHaveText('5 cards · 4 new words · 1 repeat');
  for (const [grade, count] of [
    [1, 1],
    [2, 2],
    [3, 1],
    [4, 0],
    [5, 1],
  ]) {
    await expect(page.getByTestId(`count-${grade}`)).toHaveText(String(count));
  }
  await expect(page.getByRole('link', { name: 'brontle' })).toBeVisible();
}

test('R-54 R-53 R-52 R-60 a full block by touch on the phone', async ({ page, app, pageErrors }, info) => {
  test.skip(info.project.name !== 'webkit-phone', 'touch flow runs on the phone viewport');
  void pageErrors;
  await startBookBlock(page, app, { touch: true });

  for (const [index, step] of PLAN.entries()) {
    await expect(page.getByTestId('progress')).toHaveText(`${index + 1} / ${index < 2 ? 4 : 5}`);
    await expectQuestionFormat(page, step);
    expect(await headwordOnScreen(page)).toBe(step.headword);
    await page.getByTestId('reveal').tap();
    await expect(page.getByTestId('headword')).toHaveText(step.headword);
    if (step.format === 'sentence') await expect(page.getByTestId('sentence-cs')).toBeVisible();
    // New words open the full learn view (R-52).
    if (index < 4) await expect(page.getByTestId('more')).toBeVisible();
    await waitGuard();
    await page.getByRole('button', { name: new RegExp(`^Rate ${step.grade},`) }).tap();
    await expect(page.getByTestId('toast')).toContainText(`Rated ${step.grade}`);
    if (step.grade >= 4) {
      // A fail keeps the answer with "More" open until Next.
      await expect(page.getByTestId('more')).toBeVisible();
      await page.getByTestId('next').tap();
    }
  }
  await expect(page.getByRole('heading', { name: 'Block complete' })).toBeVisible();
  await page.getByTestId('see-summary').tap();
  await expectSummary(page);

  // Nothing is left today in this scope: an honest end, never "Continue" (F1).
  await expect(page.getByTestId('end-of-day')).toContainText('Done for today');
  await expect(page.getByTestId('continue')).toHaveCount(0);
  await page.getByTestId('done').tap();
  await expect(page.getByTestId('end-of-day')).toBeVisible();
  await expect(page.getByTestId('start')).toHaveCount(0);
});

test('R-70 a full block by keyboard on the desktop', async ({ page, app, pageErrors }, info) => {
  test.skip(info.project.name !== 'chromium-desktop', 'keyboard flow runs on the desktop viewport');
  void pageErrors;
  await startBookBlock(page, app);

  for (const [index, step] of PLAN.entries()) {
    await expectQuestionFormat(page, step);
    expect(await headwordOnScreen(page)).toBe(step.headword);
    // Space reveals; Enter would too.
    await page.keyboard.press(index % 2 === 0 ? 'Space' : 'Enter');
    await expect(page.getByTestId('headword')).toHaveText(step.headword);
    if (index === 0) {
      // M toggles More; ? shows the help with the legend and the keys.
      await expect(page.getByTestId('more')).toBeVisible();
      await page.keyboard.press('m');
      await expect(page.getByTestId('more')).toHaveCount(0);
      await page.keyboard.press('m');
      await expect(page.getByTestId('more')).toBeVisible();
      await page.keyboard.press('?');
      await expect(page.getByRole('heading', { name: 'Keys' })).toBeVisible();
      await page.keyboard.press('?');
      await expect(page.getByRole('heading', { name: 'Keys' })).toHaveCount(0);
      // S and R speak the sentence and the word (browser speech, R-41).
      await page.keyboard.press('s');
      await page.keyboard.press('r');
      const spoken = await page.evaluate(() => /** @type {any} */ (window).spoken.map((/** @type {any} */ s) => s.text));
      expect(spoken).toEqual(['The glimmerous lamp swung above the door.', 'glimmerous']);
    }
    await waitGuard();
    // Space never rates.
    await page.keyboard.press('Space');
    await expect(page.getByTestId('toast')).toHaveCount(0);
    await page.keyboard.press(String(step.grade));
    await expect(page.getByTestId('toast')).toContainText(`Rated ${step.grade}`);
    if (step.grade >= 4) {
      await expect(page.getByTestId('next')).toBeVisible();
      await page.keyboard.press('Enter');
    }
    await page.evaluate(() => document.querySelector('[data-testid="toast"]')?.remove());
  }
  await expect(page.getByRole('heading', { name: 'Block complete' })).toBeVisible();
  await page.keyboard.press('Enter');
  await expectSummary(page);
});

test('R-62 the inverted scale is unmistakable: labels, colours, divider, filled/outlined, guard, toast', async ({
  page,
  app,
  pageErrors,
}) => {
  void pageErrors;
  await startBookBlock(page, app);
  await page.getByTestId('reveal').click();

  const labels = ['Instantly', 'Knew it', 'Shaky', 'Vague', 'No idea'];
  const buttons = page.locator('button.rate');
  await expect(buttons).toHaveCount(5);
  for (const [i, label] of labels.entries()) {
    const button = buttons.nth(i);
    await expect(button).toHaveAccessibleName(`Rate ${i + 1}, ${label}`);
    await expect(button.locator('.rate-num')).toHaveText(String(i + 1));
    await expect(button.locator('.rate-label')).toHaveText(label);
    await expect(button.locator('.rate-label')).toBeVisible();
    const box = await button.boundingBox();
    expect(box && box.height >= 44 && box.width >= 44, `button ${i + 1} is at least 44 px`).toBe(true);
  }

  const styles = await buttons.evaluateAll((els) =>
    els.map((el) => {
      const s = getComputedStyle(el);
      return { bg: s.backgroundColor, border: s.borderTopColor };
    }),
  );
  /** @param {string} css */
  const rgb = (css) => (css.match(/[\d.]+/g) ?? []).map(Number);
  const transparent = (/** @type {string} */ css) => css === 'transparent' || rgb(css)[3] === 0;
  // 1–3 filled, 4–5 outlined.
  for (const i of [0, 1, 2]) expect(transparent(styles[i].bg), `button ${i + 1} filled`).toBe(false);
  for (const i of [3, 4]) expect(transparent(styles[i].bg), `button ${i + 1} outlined`).toBe(true);
  // Green on the left, red on the right.
  const [r1, g1] = rgb(styles[0].bg);
  const [r5, g5] = rgb(styles[4].border);
  expect(g1).toBeGreaterThan(r1);
  expect(r5).toBeGreaterThan(g5);
  // Five different colours.
  expect(new Set([styles[0].bg, styles[1].bg, styles[2].bg, styles[3].border, styles[4].border]).size).toBe(5);

  // The divider sits in a wider gap between 3 and 4.
  const boxes = await buttons.evaluateAll((els) => els.map((el) => el.getBoundingClientRect().toJSON()));
  const gap12 = boxes[1].left - boxes[0].right;
  const gap34 = boxes[3].left - boxes[2].right;
  expect(gap34).toBeGreaterThan(gap12 + 8);
  const divider = await page.getByTestId('rating-divider').boundingBox();
  expect(divider && divider.x >= boxes[2].right && divider.x + divider.width <= boxes[3].left).toBe(true);
  await expect(page.getByText('pass · 1 = best')).toBeVisible();

  // The legend is visible during the first blocks.
  await expect(page.getByTestId('legend')).toBeVisible();
  // One compact line in the card's flow, not inside the pinned bar (bug 1).
  await expect(page.getByTestId('legend')).toHaveText(
    '1 = knew it instantly · 2 = after a moment · 3 = with effort · 4 = half-knew it · 5 = no idea',
  );
  expect(await page.locator('.practice-bottom [data-testid="legend"]').count()).toBe(0);

  // Rate 2: the toast confirms in the rating's colour. Then input guard:
  // reveal the next card and press 2 in the same instant — ignored.
  await waitGuard();
  await buttons.nth(1).click();
  await expect(page.getByTestId('toast')).toHaveText(/Rated 2 · Knew it\s*Undo/);
  const toastColour = await page.getByTestId('toast').evaluate((el) => getComputedStyle(el).borderTopColor);
  expect(toastColour).toBe(styles[1].bg);
  await page.evaluate(() => {
    const press = (/** @type {string} */ key) => document.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true }));
    press(' ');
    press('2');
  });
  await expect(page.locator('button.rate')).toHaveCount(5);
  await page.waitForTimeout(400);
  const current = await app.api('GET', '/api/sessions/current');
  expect(current.answered, 'the guarded press did not rate').toBe(1);

  // The toast leaves after about 4 s.
  await expect(page.getByTestId('toast')).toHaveCount(0, { timeout: 6000 });
});

test('R-54 a block resumes after a reload', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await startBookBlock(page, app);
  for (const grade of [2, 3]) {
    await page.getByTestId('reveal').click();
    await waitGuard();
    await page.getByRole('button', { name: new RegExp(`^Rate ${grade},`) }).click();
  }
  await expect(page.getByTestId('progress')).toHaveText('3 / 4');
  const before = await headwordOnScreen(page);
  expect(before).toBe('quabble');

  await page.reload();
  await expect(page.getByTestId('progress')).toHaveText('3 / 4');
  expect(await headwordOnScreen(page)).toBe('quabble');
  await expect(page.getByTestId('undo')).toBeEnabled();

  await page.goto(`${app.url}/#/`);
  await expect(page.getByTestId('resume')).toHaveText('Resume — 2 of 4 left');
  await page.getByTestId('resume').click();
  await expect(page.getByTestId('progress')).toHaveText('3 / 4');
});

test('R-54 undo from the toast, the ↶ button and the U key', async ({ page, app, pageErrors }, info) => {
  void pageErrors;
  const phone = info.project.name === 'webkit-phone';
  await startBookBlock(page, app);
  const rate = async (/** @type {number} */ grade) => {
    await page.getByTestId('reveal').click();
    await waitGuard();
    await page.getByRole('button', { name: new RegExp(`^Rate ${grade},`) }).click();
  };
  await expect(page.getByTestId('undo')).toBeDisabled();

  // Toast Undo: back on the answer side of the same word.
  await rate(3);
  await expect(page.getByTestId('progress')).toHaveText('2 / 4');
  await page.getByTestId('toast').getByRole('button', { name: 'Undo' }).click();
  await expect(page.getByTestId('progress')).toHaveText('1 / 4');
  await expect(page.getByTestId('headword')).toHaveText('glimmerous');
  await expect(page.locator('button.rate')).toHaveCount(5);
  expect((await app.api('GET', '/api/sessions/current')).answered).toBe(0);

  // Re-rate, then a 5 on brontle queues a repeat; ↶ (or U) removes it again.
  await waitGuard();
  await page.getByRole('button', { name: /^Rate 2,/ }).click();
  await rate(5);
  await expect(page.getByTestId('next')).toBeVisible();
  expect((await app.api('GET', '/api/sessions/current')).total).toBe(5);
  if (phone) await page.getByTestId('undo').tap();
  else await page.keyboard.press('u');
  await expect(page.getByTestId('headword')).toHaveText('brontle');
  await expect(page.locator('button.rate')).toHaveCount(5);
  const after = await app.api('GET', '/api/sessions/current');
  expect(after.total).toBe(4);
  expect(after.answered).toBe(1);

  // Undo again reverts glimmerous too (step by step).
  await page.getByTestId('undo').click();
  await expect(page.getByTestId('headword')).toHaveText('glimmerous');
  expect((await app.api('GET', '/api/sessions/current')).answered).toBe(0);
});

test('R-54 Esc ends the block early and the summary says what stays due', async ({ page, app, pageErrors }, info) => {
  test.skip(info.project.name !== 'chromium-desktop', 'keyboard only');
  void pageErrors;
  await startBookBlock(page, app);
  await page.keyboard.press('Space');
  await waitGuard();
  await page.keyboard.press('1');
  await expect(page.getByTestId('progress')).toHaveText('2 / 4');
  await page.keyboard.press('Escape');
  await expect(page.getByRole('heading', { name: 'Block ended' })).toBeVisible();
  await expect(page.getByText('3 cards not rated — they stay due.')).toBeVisible();
  await expect(page.getByTestId('count-1')).toHaveText('1');
});
