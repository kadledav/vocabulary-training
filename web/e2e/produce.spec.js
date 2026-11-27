// @ts-check
/**
 * Practice UI: Produce (design §17 M8 "done when"; R-52, R-56, R-70; design
 * §6.8, §7.1 screen 3): the blanked sentence with the Czech cue and no
 * audio, the answer field without autocorrect, Check / Hint / I don't know
 * / I'll just rate myself, the suggested grade, "also accept", the typed
 * answer kept through errors and reloads, the bottom bar above the
 * on-screen keyboard, and Today's count of waiting Produce cards. Product
 * time windows (input and rating guards) run on the page clock.
 */

import { DatabaseSync } from 'node:sqlite';

import { advanceClock, expect, freezeClock, installClock, test, waitingProduce } from './fixtures.js';

/**
 * Reviews stored so far: what the answer details look like in the log.
 *
 * @param {import('./fixtures.js').App} app
 */
function storedReviews(app) {
  const db = new DatabaseSync(app.dbPath);
  const rows = db
    .prepare(
      `SELECT w.headword, r.raw_grade, r.suggested_grade, r.typed_answer, r.answer_match, r.hint_used, r.format
       FROM review_log r JOIN card c ON c.id = r.card_id JOIN word w ON w.id = c.word_id
       WHERE r.undone = 0 ORDER BY r.id`,
    )
    .all();
  db.close();
  return rows.map((r) => ({ ...r }));
}

/**
 * Start the block from Today (all sources), on the page clock.
 *
 * @param {import('@playwright/test').Page} page
 * @param {import('./fixtures.js').App} app
 * @param {{ touch?: boolean, waiting?: boolean }} [options] `waiting`: the Produce cards wait for their first review
 */
async function startProduceBlock(page, app, { touch = false, waiting = true } = {}) {
  await installClock(page);
  await page.goto(`${app.url}/#/`);
  if (waiting) await expect(page.getByTestId('typed-waiting')).toBeVisible();
  const start = page.getByTestId('start');
  if (touch) await start.tap();
  else await start.click();
  await expect(page).toHaveURL(/#\/practice$/);
  await expect(page.getByTestId('mode')).toHaveText('Produce');
  // From here page time passes only when the test says so.
  await freezeClock(page);
  await advanceClock(page, 300); // past the 250 ms input guard of the first card
}

/**
 * @param {import('@playwright/test').Page} page
 * @param {boolean} touch
 * @param {import('@playwright/test').Locator} target
 */
function press(page, touch, target) {
  void page;
  return touch ? target.tap() : target.click();
}

/** @param {import('@playwright/test').Page} page */
function input(page) {
  return page.getByTestId('answer-input');
}

/**
 * The question side of a Produce card: cloze, Czech cue, no audio, no
 * answer on screen.
 *
 * @param {import('@playwright/test').Page} page
 * @param {string} headword
 */
async function expectQuestion(page, headword) {
  await expect(page.getByTestId('mode')).toHaveText('Produce');
  await expect(page.getByTestId('cloze')).toBeVisible();
  await expect(page.getByTestId('cloze')).not.toContainText(headword);
  await expect(page.getByTestId('sentence-cs')).toBeVisible();
  await expect(page.locator('.practice-body button.play')).toHaveCount(0);
  await expect(page.getByTestId('headword')).toHaveCount(0);
  await expect(input(page)).toBeVisible();
  await expect(input(page)).toHaveValue('');
}

test('R-56 R-52 Produce cards by touch on the phone: correct, hint, a miss added as accepted', async ({ page, app, pageErrors }, info) => {
  test.skip(info.project.name !== 'webkit-phone', 'touch flow runs on the phone viewport');
  void pageErrors;
  // Queue order: most useful first (all wait since today).
  await waitingProduce(app, ['glimmerous', 'brontle', 'quabble']);
  await startProduceBlock(page, app, { touch: true });

  // 1 · glimmerous: typed with odd case and spaces → ✓ Correct, 2 suggested.
  await expect(page.getByTestId('progress')).toHaveText('1 / 3');
  await expectQuestion(page, 'glimmerous');
  for (const [name, value] of [
    ['autocomplete', 'off'],
    ['autocorrect', 'off'],
    ['autocapitalize', 'none'],
    ['spellcheck', 'false'],
    ['enterkeyhint', 'go'],
  ]) {
    await expect(input(page)).toHaveAttribute(name, value);
  }
  await input(page).tap();
  await input(page).fill(' Glimmerous ');
  await press(page, true, page.getByTestId('check'));
  await expect(page.getByTestId('check-result')).toHaveAttribute('data-match', 'exact');
  await expect(page.getByTestId('check-result')).toContainText('Correct');
  await expect(page.getByTestId('headword')).toHaveText('glimmerous');
  await expect(page.getByTestId('suggestion')).toContainText('Suggested: 2 · Knew it');
  await expect(page.locator('button.rate[data-suggested]')).toHaveAttribute('data-grade', '2');
  // Audio is allowed on the answer side.
  await expect(page.locator('.practice-body button.play').first()).toBeVisible();
  // A tap right after Check is the second half of a double tap: ignored.
  await page.getByRole('button', { name: /^Rate 2,/ }).tap();
  await expect(page.getByTestId('toast')).toHaveCount(0);
  await advanceClock(page, 650);
  await page.getByRole('button', { name: /^Rate 2,/ }).tap();
  await expect(page.getByTestId('toast')).toContainText('Rated 2');

  // 2 · brontle: a one-letter slip → ✗ Not quite, 4 suggested; added as
  // accepted; the owner rates 3 himself.
  await expect(page.getByTestId('progress')).toHaveText('2 / 3');
  await expectQuestion(page, 'brontle');
  await advanceClock(page, 300);
  await input(page).fill('brontel');
  await press(page, true, page.getByTestId('check'));
  await expect(page.getByTestId('check-result')).toHaveAttribute('data-match', 'none');
  await expect(page.getByTestId('typed')).toHaveText('brontel');
  await expect(page.getByTestId('expected')).toHaveText('brontle');
  await expect(page.getByTestId('check-result')).toContainText('Just a typo? Choose 3.');
  await expect(page.getByTestId('suggestion')).toContainText('Suggested: 4 · Vague');
  // The note opens after a miss.
  await expect(page.getByTestId('more')).toBeVisible();
  await page.getByTestId('accept-typed').tap();
  await expect(page.getByTestId('accepted-note')).toHaveText('“brontel” will be accepted next time.');
  const word = await app.api('GET', `/api/words/${await wordId(app, 'brontle')}`);
  expect(JSON.stringify(word)).toContain('"brontel"');
  await advanceClock(page, 650);
  await page.getByRole('button', { name: /^Rate 3,/ }).tap();
  await expect(page.getByTestId('toast')).toContainText('Rated 3');

  // 3 · quabble: the hint (first letter, letter count), then a match → 3.
  await expectQuestion(page, 'quabble');
  await advanceClock(page, 300);
  await page.getByTestId('hint-button').tap();
  await expect(page.getByTestId('hint')).toContainText('q _ _ _ _ _ _');
  await expect(page.getByTestId('hint')).toContainText('(7 letters)');
  await input(page).fill('quabble');
  await input(page).press('Enter');
  await expect(page.getByTestId('check-result')).toContainText('Correct (with the hint)');
  await expect(page.getByTestId('suggestion')).toContainText('Suggested: 3 · Shaky');
  await advanceClock(page, 650);
  await page.getByRole('button', { name: /^Rate 3,/ }).tap();
  await expect(page.getByRole('heading', { name: 'Block complete' })).toBeVisible();

  expect(storedReviews(app)).toEqual([
    { headword: 'glimmerous', raw_grade: 2, suggested_grade: 2, typed_answer: 'Glimmerous', answer_match: 'exact', hint_used: 0, format: 'cloze' },
    { headword: 'brontle', raw_grade: 3, suggested_grade: 4, typed_answer: 'brontel', answer_match: 'none', hint_used: 0, format: 'cloze' },
    { headword: 'quabble', raw_grade: 3, suggested_grade: 3, typed_answer: 'quabble', answer_match: 'exact', hint_used: 1, format: 'cloze' },
  ]);
  // Nothing scrolls sideways on the phone.
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth)).toBe(true);
});

/**
 * The id of the word `headword`.
 *
 * @param {import('./fixtures.js').App} app
 * @param {string} headword
 */
async function wordId(app, headword) {
  const rows = await app.api('GET', `/api/words?q=${encodeURIComponent(headword)}`);
  const row = rows.find((/** @type {{ headword: string }} */ r) => r.headword === headword);
  if (!row) throw new Error(`no word ${headword}`);
  return row.id;
}

test('R-56 R-70 Produce cards by keyboard: Enter checks and accepts, I don’t know, rate myself', async ({ page, app, pageErrors }, info) => {
  test.skip(info.project.name !== 'chromium-desktop', 'keyboard flow runs on the desktop');
  void pageErrors;
  await waitingProduce(app, ['glimmerous', 'snorkish', 'quabble']);
  await startProduceBlock(page, app);

  // 1 · glimmerous: the field has focus; Enter checks, Enter takes the 2.
  await expectQuestion(page, 'glimmerous');
  await expect(input(page)).toBeFocused();
  // Digits and letters are typed, never shortcuts.
  await page.keyboard.type('glimm3rous');
  await expect(input(page)).toHaveValue('glimm3rous');
  await expect(page.getByTestId('toast')).toHaveCount(0);
  await input(page).fill('');
  // Enter with nothing typed asks for a word.
  await page.keyboard.press('Enter');
  await expect(page.getByTestId('typed-note')).toHaveText('Type the word first, or choose “I don’t know”.');
  await expect(input(page)).toBeFocused();
  await page.keyboard.type('glimmerous');
  await page.keyboard.press('Enter');
  await expect(page.getByTestId('check-result')).toHaveAttribute('data-match', 'exact');
  // A bounced Enter right after the check does not rate.
  await page.keyboard.press('Enter');
  await page.waitForTimeout(100); // real time: lets a (wrong) rating request show up
  expect(storedReviews(app)).toEqual([]);
  await advanceClock(page, 650);
  await page.keyboard.press('Enter');
  await expect(page.getByTestId('toast')).toContainText('Rated 2');

  // 2 · quabble (usefulness 7 before snorkish's 6): Space on the page goes
  // to the field; "I don't know" → 5 suggested, Enter rates 5, the answer
  // stays until Next.
  await expectQuestion(page, 'quabble');
  await page.locator('.practice-body').focus();
  await page.keyboard.press('Space');
  await expect(input(page)).toBeFocused();
  await expect(page.locator('button.rate')).toHaveCount(0);
  await advanceClock(page, 300);
  await page.getByTestId('dont-know').click();
  await expect(page.getByTestId('check-result')).toHaveAttribute('data-match', 'dont_know');
  await expect(page.getByTestId('check-result')).toContainText('The answer: quabble');
  await expect(page.getByTestId('suggestion')).toContainText('Suggested: 5 · No idea');
  await advanceClock(page, 650);
  await page.keyboard.press('Enter');
  await expect(page.getByTestId('toast')).toContainText('Rated 5');
  await expect(page.getByTestId('next')).toBeVisible();
  await advanceClock(page, 300);
  await page.keyboard.press('Enter');

  // 3 · snorkish: "I'll just rate myself" → no suggestion; Enter does
  // nothing, a digit rates.
  await expectQuestion(page, 'snorkish');
  await advanceClock(page, 300);
  await page.getByTestId('self-rate').click();
  await expect(page.getByTestId('check-result')).toHaveCount(0);
  await expect(page.getByTestId('headword')).toHaveText('snorkish');
  await expect(page.getByTestId('suggestion')).toHaveText('Rate yourself: how well did you know the word?');
  await expect(page.locator('button.rate[data-suggested]')).toHaveCount(0);
  await advanceClock(page, 650);
  await page.keyboard.press('Enter');
  await page.waitForTimeout(100);
  expect(storedReviews(app).length).toBe(2);
  await page.keyboard.press('2');
  await expect(page.getByTestId('toast')).toContainText('Rated 2');

  const rows = storedReviews(app);
  expect(rows.map((r) => [r.headword, r.raw_grade, r.suggested_grade, r.typed_answer, r.answer_match])).toEqual([
    ['glimmerous', 2, 2, 'glimmerous', 'exact'],
    ['quabble', 5, 5, null, 'dont_know'],
    ['snorkish', 2, null, null, 'skipped'],
  ]);
});

test('R-56 the typed answer survives a failed post, a retry and a reload', async ({ page, app, pageErrors }) => {
  await waitingProduce(app, ['glimmerous', 'brontle']);
  await startProduceBlock(page, app);

  // A reload while typing keeps what was typed (and the hint).
  await input(page).fill('glimm');
  await page.getByTestId('hint-button').click();
  await page.reload();
  await freezeClock(page);
  await advanceClock(page, 300);
  await expect(input(page)).toHaveValue('glimm');
  await expect(page.getByTestId('hint')).toBeVisible();
  await input(page).fill('glimmerus');
  await page.getByTestId('check').click();
  await expect(page.getByTestId('typed')).toHaveText('glimmerus');

  // The rating post is lost on the way: the answer stays, the same rating
  // with the same answer is retried.
  /** @type {any[]} */
  const posted = [];
  let fail = true;
  await page.route('**/api/sessions/*/reviews', async (route) => {
    posted.push(route.request().postDataJSON());
    if (fail) {
      fail = false;
      await route.abort('failed');
    } else {
      await route.continue();
    }
  });
  await advanceClock(page, 650);
  await page.getByRole('button', { name: /^Rate 4,/ }).click();
  await expect(page.getByTestId('unsure')).toBeVisible();
  await expect(page.getByTestId('typed')).toHaveText('glimmerus');
  await page.getByTestId('retry').click();
  await expect(page.getByTestId('toast')).toContainText('Rated 4');
  // The lost request is the one failure the page may log.
  for (let i = pageErrors.length - 1; i >= 0; i -= 1) if (/ERR_FAILED|Load failed|network connection/i.test(pageErrors[i] ?? '')) pageErrors.splice(i, 1);
  expect(posted).toHaveLength(2);
  for (const body of posted) {
    expect(body).toMatchObject({ raw_grade: 4, typed_answer: 'glimmerus', answer_match: 'none', hint_used: true, suggested_grade: 4 });
  }
  expect(posted[1].client_review_id).toBe(posted[0].client_review_id);
  // A 4 keeps the answer (with what was typed) until Next.
  await expect(page.getByTestId('typed')).toHaveText('glimmerus');

  // Undo brings the checked answer back to be rated again.
  await page.getByTestId('undo').click();
  await expect(page.getByTestId('toast')).toContainText('Rating undone');
  await expect(page.getByTestId('typed')).toHaveText('glimmerus');
  await expect(page.locator('button.rate')).toHaveCount(5);
});

test('R-70 the answer field and its buttons stay above the on-screen keyboard', async ({ page, app, pageErrors }, info) => {
  test.skip(info.project.name !== 'webkit-phone', 'the on-screen keyboard is a phone matter');
  void pageErrors;
  await waitingProduce(app, ['glimmerous']);
  await startProduceBlock(page, app, { touch: true });
  await page.getByTestId('hint-button').tap();
  await input(page).tap();
  // The keyboard takes the lower 45 % of the 812 px screen: the visual
  // viewport shrinks to 450 px (as on an iPhone).
  await page.evaluate(() => {
    const vv = /** @type {VisualViewport} */ (window.visualViewport);
    Object.defineProperty(vv, 'height', { configurable: true, get: () => 450 });
    Object.defineProperty(vv, 'offsetTop', { configurable: true, get: () => 0 });
    vv.dispatchEvent(new Event('resize'));
  });
  for (const id of ['hint', 'answer-input', 'check', 'hint-button', 'dont-know', 'self-rate']) {
    const box = await page.getByTestId(id).boundingBox();
    expect(box, id).not.toBeNull();
    const b = /** @type {{ x: number, y: number, width: number, height: number }} */ (box);
    expect(b.y, `${id} below the top`).toBeGreaterThanOrEqual(0);
    expect(b.y + b.height, `${id} above the keyboard`).toBeLessThanOrEqual(450);
    expect(b.x + b.width, `${id} inside the screen width`).toBeLessThanOrEqual(375);
  }
  // The keyboard goes away: the bar is back at the bottom.
  await page.evaluate(() => {
    const vv = /** @type {VisualViewport} */ (window.visualViewport);
    Object.defineProperty(vv, 'height', { configurable: true, get: () => window.innerHeight });
    vv.dispatchEvent(new Event('resize'));
  });
  const check = await page.getByTestId('check').boundingBox();
  expect((check?.y ?? 0) + (check?.height ?? 0)).toBeGreaterThan(700);
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth)).toBe(true);
});

test('R-55 on a flagged Produce card the leech panel stays folded, so the checked answer stays in sight', async ({ page, app, pageErrors }, info) => {
  test.skip(info.project.name !== 'webkit-phone', 'the on-screen keyboard is a phone matter');
  void pageErrors;
  await waitingProduce(app, ['glimmerous']);
  const db = new DatabaseSync(app.dbPath);
  db.prepare("UPDATE card SET state = 'review', stability = 2.0, difficulty = 6.0, reps = 6, lapses = 4, leech = 'flagged', format = 'cloze' WHERE kind = 'produce'").run();
  db.close();
  await startProduceBlock(page, app, { touch: true, waiting: false });
  // The keyboard open: 375 × 420 is what is left.
  await page.setViewportSize({ width: 375, height: 420 });
  await input(page).tap();
  await input(page).fill('glimmerous');
  await press(page, true, page.getByTestId('check'));
  await expect(page.getByTestId('check-result')).toHaveAttribute('data-match', 'exact');
  await expect(page.getByTestId('check-result')).toBeInViewport();
  const panel = page.getByTestId('leech-panel');
  await expect(panel.getByTestId('leech-open')).toBeVisible();
  await expect(panel.getByTestId('leech-suspend')).toBeHidden();
  const box = await panel.boundingBox();
  expect(box?.height ?? 999).toBeLessThan(80);
  // One tap opens the choices.
  await panel.getByTestId('leech-open').tap();
  await expect(panel.getByTestId('leech-suspend')).toBeVisible();
});

test('R-52 Today counts the Produce cards waiting and the daily limit is a setting', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await waitingProduce(app, ['glimmerous', 'brontle', 'quabble', 'snorkish']);
  await page.goto(`${app.url}/#/settings`);
  const limit = page.getByLabel('New Produce cards per day');
  await expect(limit).toHaveValue('10');
  await limit.fill('3');
  await limit.press('Tab');
  await expect(page.getByTestId('settings-status')).toHaveText('Saved.');
  expect((await app.api('GET', '/api/settings')).new_typed_per_day).toBe(3);

  await page.goto(`${app.url}/#/`);
  await expect(page.getByTestId('typed-waiting')).toHaveText(
    '4 Produce cards wait for the first time — 3 today (limit 3 a day), the rest on later days.',
  );
  await expect(page.getByTestId('due-now')).toContainText('3 due now');
  await page.getByTestId('start').click();
  await expect(page).toHaveURL(/#\/practice$/);
  await expect(page.getByTestId('progress')).toHaveText('1 / 3');
});

/** A word whose sentence wants a form other than the headword. */
const FORMS_PACK = {
  format: 'vocab-pack',
  format_version: 1,
  pack_id: 'test-e2e-forms--2026-11-01--1',
  exported_at: '2026-11-01',
  source: { id: 'test-e2e-forms', type: 'article', title: 'An E2E Forms Article' },
  words: [
    {
      headword: 'plindle',
      meaning: 'To coil a rope neatly.',
      czech: ['smotat'],
      usefulness: 10,
      goal: 'active',
      forms: [
        { form: 'plindles', common: true },
        { form: 'plindled', common: true },
      ],
      accepted: ['plindle', 'plindled'],
      sentences: [{ en: 'She [[plindled]] the rope on the deck.', cs: 'Na palubě [[smotala]] lano.', origin: 'example', location: null }],
    },
  ],
};

test('R-56 another form is the right word (3); "also accept" reaches the repeat, an existing entry is fine', async ({ page, app, pageErrors }, info) => {
  test.skip(info.project.name !== 'chromium-desktop', 'one flow is enough');
  const report = await app.api('POST', '/api/import/commit', { text: JSON.stringify(FORMS_PACK) });
  expect(report.packs[0].status).toBe('imported');
  await waitingProduce(app, ['plindle', 'brontle']);
  await startProduceBlock(page, app);

  // 1 · plindle: the headword where the sentence needs "plindled".
  await input(page).fill('plindle');
  await page.getByTestId('check').click();
  await expect(page.getByTestId('check-result')).toHaveAttribute('data-match', 'right_word');
  await expect(page.getByTestId('check-result')).toHaveText('Right word — here it is “plindled”');
  await expect(page.getByTestId('check-result')).not.toContainText('Correct');
  await expect(page.getByTestId('suggestion')).toContainText('Suggested: 3');
  await expect(page.getByTestId('accept-typed')).toHaveCount(0);
  await advanceClock(page, 650);
  await page.keyboard.press('Enter');
  await expect(page.getByTestId('toast')).toContainText('Rated 3');

  // 2 · brontle: a miss; the answer is meanwhile added elsewhere (409).
  await expectQuestion(page, 'brontle');
  await advanceClock(page, 300);
  await input(page).fill('brontel');
  await page.getByTestId('check').click();
  await expect(page.getByTestId('check-result')).toHaveAttribute('data-match', 'none');
  await app.api('POST', `/api/words/${await wordId(app, 'brontle')}/items`, { kind: 'accepted', value: 'brontel' });
  await page.getByTestId('accept-typed').click();
  await expect(page.getByTestId('accepted-note')).toHaveText('“brontel” will be accepted next time.');
  await expect(page.getByTestId('practice-error')).toBeEmpty();
  await advanceClock(page, 650);
  await page.getByRole('button', { name: /^Rate 4,/ }).click();
  await expect(page.getByTestId('toast')).toContainText('Rated 4');
  await advanceClock(page, 700);
  await page.getByTestId('next').click();

  // 3 · the repeat of brontle in the same block accepts it at once.
  await expectQuestion(page, 'brontle');
  await advanceClock(page, 300);
  await input(page).fill('brontel');
  await page.getByTestId('check').click();
  await expect(page.getByTestId('check-result')).not.toHaveAttribute('data-match', 'none');
  await expect(page.getByTestId('suggestion')).toContainText('Suggested: 3');
  await advanceClock(page, 650);
  await page.keyboard.press('Enter');
  await expect(page.getByRole('heading', { name: 'Block complete' })).toBeVisible();
  // The 409 of the entry added meanwhile is the one failure the page logs.
  for (let i = pageErrors.length - 1; i >= 0; i -= 1) if (/409/.test(pageErrors[i] ?? '')) pageErrors.splice(i, 1);

  expect(storedReviews(app).map((r) => [r.headword, r.raw_grade, r.suggested_grade, r.typed_answer, r.answer_match])).toEqual([
    ['plindle', 3, 3, 'plindle', 'exact'],
    ['brontle', 4, 4, 'brontel', 'none'],
    ['brontle', 3, 3, 'brontel', 'exact'],
  ]);
});

test('R-56 a reload after Check returns to the checked answer; Undo after a reload keeps the next draft', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await waitingProduce(app, ['glimmerous', 'brontle']);
  await startProduceBlock(page, app);

  await input(page).fill('glimmerus');
  await page.getByTestId('hint-button').click();
  await page.getByTestId('check').click();
  await expect(page.getByTestId('check-result')).toHaveAttribute('data-match', 'none');
  await page.reload();
  await freezeClock(page);
  // The checked side, not an empty question to retype the answer.
  await expect(page.getByTestId('check-result')).toHaveAttribute('data-match', 'none');
  await expect(page.getByTestId('typed')).toHaveText('glimmerus');
  await expect(page.getByTestId('suggestion')).toContainText('Suggested: 4');
  // Open the rating guard only once the side is drawn: on a slow load the
  // side can appear after the clock was frozen and advanced.
  await advanceClock(page, 700);
  await page.getByRole('button', { name: /^Rate 3,/ }).click();
  await expect(page.getByTestId('toast')).toContainText('Rated 3');

  // brontle: half typed, reload, Undo the previous card.
  await expectQuestion(page, 'brontle');
  await advanceClock(page, 300);
  await input(page).fill('bront');
  await page.reload();
  await freezeClock(page);
  await advanceClock(page, 300);
  await expect(input(page)).toHaveValue('bront');
  await page.getByTestId('undo').click();
  await expect(page.getByTestId('toast')).toContainText('Rating undone');
  await expect(page.getByTestId('cloze')).toBeVisible();
  await advanceClock(page, 300);
  await input(page).fill('glimmerous');
  await page.getByTestId('check').click();
  await advanceClock(page, 650);
  await page.getByRole('button', { name: /^Rate 2,/ }).click();
  // Back on brontle: the draft is still there.
  await expectQuestionWithDraft(page, 'bront');
  expect(storedReviews(app).map((r) => [r.headword, r.raw_grade, r.hint_used])).toEqual([['glimmerous', 2, 0]]);
});

/**
 * @param {import('@playwright/test').Page} page
 * @param {string} draft
 */
async function expectQuestionWithDraft(page, draft) {
  await expect(page.getByTestId('cloze')).toBeVisible();
  await expect(input(page)).toHaveValue(draft);
}

test('R-56 "also accept" is not offered for an answer longer than the list takes', async ({ page, app, pageErrors }, info) => {
  test.skip(info.project.name !== 'chromium-desktop', 'one flow is enough');
  void pageErrors;
  await waitingProduce(app, ['glimmerous']);
  await startProduceBlock(page, app);
  await input(page).fill('g'.repeat(250));
  await page.getByTestId('check').click();
  await expect(page.getByTestId('check-result')).toHaveAttribute('data-match', 'none');
  await expect(page.getByTestId('accept-typed')).toHaveCount(0);
});

test('R-70 phone in landscape with the keyboard open: the sentence with the blank stays visible', async ({ page, app, pageErrors }, info) => {
  test.skip(info.project.name !== 'webkit-phone', 'the on-screen keyboard is a phone matter');
  void pageErrors;
  await page.setViewportSize({ width: 812, height: 375 });
  await waitingProduce(app, ['glimmerous']);
  await startProduceBlock(page, app, { touch: true });
  await page.getByTestId('hint-button').tap();
  await input(page).tap();
  // The keyboard leaves about 180 px.
  await page.evaluate(() => {
    const vv = /** @type {VisualViewport} */ (window.visualViewport);
    Object.defineProperty(vv, 'height', { configurable: true, get: () => 180 });
    Object.defineProperty(vv, 'offsetTop', { configurable: true, get: () => 0 });
    vv.dispatchEvent(new Event('resize'));
  });
  const box = async (/** @type {string} */ id) => {
    const b = await page.getByTestId(id).boundingBox();
    expect(b, id).not.toBeNull();
    return /** @type {{ x: number, y: number, width: number, height: number }} */ (b);
  };
  const cloze = await box('cloze');
  const field = await box('answer-input');
  expect(cloze.y, 'sentence below the top').toBeGreaterThanOrEqual(0);
  expect(cloze.y + cloze.height, 'sentence above the answer field').toBeLessThanOrEqual(field.y);
  expect(field.y + field.height, 'answer field above the keyboard').toBeLessThanOrEqual(180);
  await expect(page.getByTestId('hint')).toBeVisible();
  await expect(page.getByTestId('dont-know')).toBeHidden();
  // The keyboard goes away: the full bar is back.
  await page.evaluate(() => {
    const vv = /** @type {VisualViewport} */ (window.visualViewport);
    Object.defineProperty(vv, 'height', { configurable: true, get: () => window.innerHeight });
    vv.dispatchEvent(new Event('resize'));
  });
  await expect(page.getByTestId('dont-know')).toBeVisible();
});
