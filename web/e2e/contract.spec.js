// @ts-check
/**
 * Contract tests for M4 "Practice UI: Understand", written from the
 * requirements (R-41, R-52, R-53, R-54, R-59, R-60, R-62, R-70, R-71, R-74),
 * design §6.7/§7 and the UX research (§4, §7, §10), not from the code.
 * Synthetic data only.
 */

import { DatabaseSync } from 'node:sqlite';

import { expect, headwordOnScreen, startBookBlock, test, waitGuard } from './fixtures.js';

// ── helpers ──────────────────────────────────────────────────────────────────

/** A pack built to stress layout: long words, long note, many forms, other sentences. */
const STRESS_PACK = {
  format: 'vocab-pack',
  format_version: 1,
  pack_id: 'test-e2e-stress--2026-11-01--1',
  exported_at: '2026-11-01',
  source: { id: 'test-e2e-stress', type: 'book', title: 'E2E Stress Source' },
  reading_position: 'Chapter 99',
  words: [
    {
      headword: 'antidisestablishmentarianistic',
      meaning: 'An invented adjective with a deliberately long headword and a meaning that runs on for a while.',
      czech: ['protistátní', 'dlouhé slovo', 'ještě delší překlad'],
      usefulness: 10,
      pronunciation: 'an-ti-dis-es-tab-lish-ment-air-ee-an-is-tik',
      speak: 'kwux',
      icon: '🧪',
      hint: 'A hint line that is long enough to wrap over several lines on a very narrow phone screen.',
      note: Array.from({ length: 24 }, (_, i) => `Note line ${i + 1}: an invented explanation sentence that keeps going to fill space.`).join(' '),
      forms: Array.from({ length: 12 }, (_, i) => ({ form: `antidisestablishmentarianistic-form-${i}`, common: i % 2 === 0 })),
      sentences: [
        { en: 'The [[antidisestablishmentarianistic]] mood lasted well past midnight in the hall.', cs: 'Ta [[protistátní]] nálada trvala v sále dlouho po půlnoci.', origin: 'example', location: 'Chapter 1' },
        { en: 'A second [[antidisestablishmentarianistic]] remark made everybody laugh out loud.', cs: 'Druhá [[protistátní]] poznámka všechny rozesmála.', origin: 'example', location: 'Chapter 2' },
        { en: 'Nobody liked the [[antidisestablishmentarianistic]] speech, not even its author.', cs: 'Žádný [[protistátní]] proslov se nelíbil, ani jeho autorovi.', origin: 'example', location: 'Chapter 3' },
        { en: 'Yet another [[antidisestablishmentarianistic]] sentence with quite a few extra words in it.', cs: 'Další [[protistátní]] věta s hodně slovy navíc.', origin: 'example', location: 'Chapter 4' },
      ],
    },
    {
      headword: 'floobertigibbet',
      meaning: 'An invented noun for a restless person.',
      czech: ['neposeda'],
      usefulness: 9,
      note: 'Short note.',
      sentences: [{ en: 'What a [[floobertigibbet]] that boy is, never still for a second.', cs: 'To je ale [[neposeda]], ani vteřinu nevydrží v klidu.', origin: 'example' }],
    },
  ],
};

/**
 * @param {import('./fixtures.js').App} app
 * @param {{ long?: boolean }} [options] `long: false` shortens the 30-letter headword to 16 letters
 */
async function importStress(app, { long = true } = {}) {
  const text = JSON.stringify(STRESS_PACK);
  const report = await app.api('POST', '/api/import/commit', {
    text: long ? text : text.replaceAll('antidisestablishmentarianistic', 'antidisestablish'),
  });
  if (report.packs[0].status !== 'imported') throw new Error(JSON.stringify(report));
}

/**
 * @param {import('@playwright/test').Page} page
 * @param {string} base
 */
async function openToday(page, base) {
  await page.goto(`${base}/#/`);
  await expect(page.getByTestId('start')).toBeVisible();
}

/** @param {import('./fixtures.js').App} app */
async function currentSession(app) {
  return app.api('GET', '/api/sessions/current');
}

/**
 * The raw grade of the most recent rating of the open block.
 *
 * @param {import('./fixtures.js').App} app
 */
async function lastRaw(app) {
  const session = await currentSession(app);
  const answered = session.items.filter((/** @type {any} */ i) => i.answered).map((/** @type {any} */ i) => i.answered);
  answered.sort((/** @type {any} */ a, /** @type {any} */ b) => a.review_id - b.review_id);
  return answered.length === 0 ? null : answered[answered.length - 1].raw_grade;
}

/**
 * Reveal the current card by keyboard and wait out the input guard.
 *
 * @param {import('@playwright/test').Page} page
 */
async function revealByKey(page) {
  await page.keyboard.press('Space');
  await expect(page.locator('button.rate')).toHaveCount(5);
  await waitGuard();
}

/**
 * Rate by key and wait until the rating was saved and the screen moved on.
 *
 * @param {import('@playwright/test').Page} page
 * @param {number} grade
 */
async function rateByKey(page, grade) {
  await page.keyboard.press(String(grade));
  await expect(page.getByTestId('toast')).toContainText(`Rated ${grade}`);
}

/**
 * Make a block's first card a reviewed (not new) card so "More" starts closed.
 *
 * @param {import('./fixtures.js').App} app
 * @param {string} headword
 */
async function makeReview(app, headword) {
  const today = await app.api('GET', '/api/today');
  const db = new DatabaseSync(app.dbPath);
  db.prepare(
    `UPDATE card SET state = 'review', stability = 5.0, difficulty = 5.0, due_day = ?, last_review_day = '2026-11-01',
     introduced_day = '2026-11-01', reps = 2, format = 'sentence'
     WHERE word_id IN (SELECT id FROM word WHERE headword = ?)`,
  ).run(today.study_day, headword);
  db.close();
}

/**
 * Walk through every M4 screen/state, calling `visit` in each.
 *
 * @param {import('@playwright/test').Page} page
 * @param {import('./fixtures.js').App} app
 * @param {(label: string) => Promise<void>} visit
 */
async function walkScreens(page, app, visit, { long = false } = {}) {
  await importStress(app, { long });
  await openToday(page, app.url);
  await visit('today');
  await page.getByLabel(/E2E Stress Source/).check();
  await page.getByTestId('start').click();
  await expect(page.getByTestId('progress')).toHaveText('1 / 2');
  await visit('question (sentence)');
  await page.getByTestId('reveal').click();
  await expect(page.locator('button.rate')).toHaveCount(5);
  await visit('answer with More open (new word)');
  await page.getByRole('button', { name: /^Help/ }).click();
  await expect(page.getByTestId('legend')).toBeVisible();
  await visit('help open');
  await page.getByRole('button', { name: /^Help/ }).click();
  await waitGuard();
  await page.keyboard.press('4');
  await expect(page.getByTestId('next')).toBeVisible();
  await visit('feedback after a 4');
  await page.getByTestId('next').click();
  await expect(page.getByTestId('headword-question')).toBeVisible();
  await visit('question (word only)');
  await page.getByTestId('reveal').click();
  await expect(page.locator('button.rate')).toHaveCount(5);
  await visit('answer (word only)');
  await waitGuard();
  await page.keyboard.press('1');
  // The repeat of the first card.
  await expect(page.getByTestId('progress')).toHaveText('3 / 3');
  await page.getByTestId('reveal').click();
  await waitGuard();
  await page.keyboard.press('1');
  await expect(page.getByRole('heading', { name: 'Block complete' })).toBeVisible();
  await visit('block complete');
  await page.getByTestId('see-summary').click();
  await expect(page).toHaveURL(/#\/summary$/);
  await expect(page.getByTestId('summary-line')).toBeVisible();
  await visit('summary');
  await page.goto(`${app.url}/#/settings`);
  await expect(page.getByTestId('settings-status')).toBeAttached();
  await visit('settings');
}

/** In-page: interactive controls smaller than 44×44 CSS px. */
function undersizedControls(/** @type {{ only?: string, skip?: string }} */ { only, skip } = {}) {
  /** @type {string[]} */
  const small = [];
  const selector = 'button, a[href], input, select, textarea, summary, [role="button"]';
  for (const el of document.querySelectorAll(selector)) {
    if (skip && el.closest(skip)) continue;
    if (only && !el.closest(only)) continue;
    let target = /** @type {HTMLElement} */ (el);
    if (el instanceof HTMLInputElement && (el.type === 'radio' || el.type === 'checkbox')) {
      target = el.closest('label') ?? el;
    }
    const style = getComputedStyle(target);
    if (style.display === 'none' || style.visibility === 'hidden') continue;
    const rect = target.getBoundingClientRect();
    if (rect.width === 0 || rect.height === 0) continue;
    if (rect.width <= 1 && rect.height <= 1) continue;
    // Links inside running text are exempt (WCAG 2.5.8 inline exception).
    if (el.tagName === 'A' && style.display === 'inline' && el.closest('p')) continue;
    if (rect.width < 43.5 || rect.height < 43.5) {
      small.push(`${el.tagName.toLowerCase()} "${(el.getAttribute('aria-label') ?? el.textContent ?? '').trim().slice(0, 30)}" ${Math.round(rect.width)}x${Math.round(rect.height)}`);
    }
  }
  return small;
}

// ── R-60 R-62 R-70: the rating keys ──────────────────────────────────────────

/** Grades in the order they are given: 5 and 4 queue repeats (design §6.7). */
const GRADE_ORDER = [1, 2, 5, 3, 4];
const LABELS = ['Instantly', 'Knew it', 'Shaky', 'Vague', 'No idea'];

test('R-60 R-70 keys 1–5 save the same raw grade as the on-screen buttons', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await startBookBlock(page, app);
  for (const [index, grade] of GRADE_ORDER.entries()) {
    await revealByKey(page);
    await page.keyboard.press(String(grade));
    await expect(page.getByTestId('toast')).toContainText(`Rated ${grade} · ${LABELS[grade - 1]}`);
    expect(await lastRaw(app), `key ${grade} saved raw grade ${grade}`).toBe(grade);
    if (grade >= 4 && index < GRADE_ORDER.length - 1) await page.keyboard.press('Enter');
  }
});

test('R-60 R-62 the on-screen buttons 1–5 save raw grades 1–5 (not mirrored)', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await startBookBlock(page, app);
  for (const [index, grade] of GRADE_ORDER.entries()) {
    await page.getByTestId('reveal').click();
    await waitGuard();
    await page.getByRole('button', { name: `Rate ${grade}, ${LABELS[grade - 1]}` }).click();
    await expect(page.getByTestId('toast')).toContainText(`Rated ${grade} · ${LABELS[grade - 1]}`);
    expect(await lastRaw(app), `button ${grade} saved raw grade ${grade}`).toBe(grade);
    if (grade >= 4 && index < GRADE_ORDER.length - 1) await page.getByTestId('next').click();
  }
});

test('R-70 digits do nothing before the reveal; Space and Enter never rate', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await startBookBlock(page, app);
  // Question side: digits are ignored.
  for (const key of ['1', '2', '3', '4', '5']) await page.keyboard.press(key);
  await expect(page.getByTestId('reveal')).toBeVisible();
  await expect(page.getByTestId('toast')).toHaveCount(0);
  expect((await currentSession(app)).answered).toBe(0);
  // Space reveals.
  await page.keyboard.press('Space');
  await expect(page.locator('button.rate')).toHaveCount(5);
  await waitGuard();
  // Answer side: Space/Enter must not rate (Anki habit: Space = Good).
  await page.keyboard.press('Space');
  await page.keyboard.press('Enter');
  await page.waitForTimeout(200);
  await expect(page.getByTestId('toast')).toHaveCount(0);
  await expect(page.locator('button.rate')).toHaveCount(5);
  expect((await currentSession(app)).answered).toBe(0);
});

test('R-62 a double reveal tap cannot fire a rating (250 ms guard, by touch/click)', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await startBookBlock(page, app);
  await page.getByTestId('reveal').click();
  // A rating fired right away is ignored.
  await page.keyboard.press('3');
  await page.keyboard.press('1');
  await page.waitForTimeout(100);
  expect((await currentSession(app)).answered).toBe(0);
  await waitGuard();
  await page.keyboard.press('3');
  await expect(page.getByTestId('toast')).toContainText('Rated 3');
});

test('R-62 a 1 right after the reveal of a card last failed keeps the toast longer (slip guard)', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await startBookBlock(page, app);
  // g=1, b=5 (queues a repeat of brontle), q=1, s=1; then brontle again.
  for (const grade of [1, 5, 1, 1]) {
    await revealByKey(page);
    await rateByKey(page, grade);
    if (grade === 5) await page.keyboard.press('Enter');
    await page.evaluate(() => document.querySelector('[data-testid="toast"]')?.remove());
  }
  await expect(page.getByTestId('progress')).toHaveText('5 / 5');
  expect(await headwordOnScreen(page)).toBe('brontle');
  await page.keyboard.press('Space');
  await expect(page.locator('button.rate')).toHaveCount(5);
  await page.waitForTimeout(300); // past the 250 ms guard, inside 600 ms
  await page.keyboard.press('1');
  await expect(page.getByTestId('toast')).toContainText('Rated 1');
  // Normal toasts last about 4 s; the slip-guard toast stays up to 8 s.
  await page.waitForTimeout(5500);
  await expect(page.getByTestId('toast')).toBeVisible();
  const animation = await page.getByTestId('toast').evaluate((el) => getComputedStyle(el).animationName);
  expect(animation, 'slip-guard toast pulses').not.toBe('none');
});

test('R-62 the legend stops being always-on after three finished blocks and stays behind "?"', async ({ page, app, pageErrors }) => {
  void pageErrors;
  for (let i = 0; i < 3; i += 1) {
    const started = await app.api('POST', '/api/sessions', { kind: i === 0 ? 'start' : 'continue' });
    expect(started.session, `block ${i + 1} started`).not.toBeNull();
    // Only blocks with at least one rating count (guarantor decision H10).
    await app.api('POST', `/api/sessions/${started.session.id}/reviews`, {
      client_review_id: crypto.randomUUID(),
      item_id: started.session.items[0].item_id,
      raw_grade: 2,
    });
    await app.api('POST', `/api/sessions/${started.session.id}/finish`);
  }
  expect((await app.api('GET', '/api/today')).finished_blocks.total).toBe(3);
  await app.api('PATCH', '/api/settings', { block_size: 10 });
  await page.goto(`${app.url}/#/`);
  await page.getByTestId('start').click();
  await expect(page).toHaveURL(/#\/practice$/);
  await page.getByTestId('reveal').click();
  await expect(page.locator('button.rate')).toHaveCount(5);
  await expect(page.getByTestId('legend')).toHaveCount(0);
  await page.getByRole('button', { name: /^Help/ }).click();
  await expect(page.getByTestId('legend')).toBeVisible();
});

// ── R-54: undo, resume, finish, continue, backlog ────────────────────────────

test('R-54 undo from the "Block complete" step restores the last card on its answer side', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await startBookBlock(page, app);
  for (let i = 0; i < 4; i += 1) {
    await revealByKey(page);
    await rateByKey(page, 1);
    await page.evaluate(() => document.querySelector('[data-testid="toast"]')?.remove());
  }
  await expect(page.getByRole('heading', { name: 'Block complete' })).toBeVisible();
  await page.keyboard.press('u');
  await expect(page.getByTestId('headword')).toHaveText('snorkish');
  await expect(page.locator('button.rate')).toHaveCount(5);
  expect((await currentSession(app)).answered).toBe(3);
});

test('R-54 a reload on the answer side keeps the block, the position and every saved rating', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await startBookBlock(page, app);
  await revealByKey(page);
  await page.keyboard.press('2');
  await expect(page.getByTestId('progress')).toHaveText('2 / 4');
  await revealByKey(page);
  await page.reload();
  // Same card, same position (shown on either side) and nothing rated yet for it.
  await expect(page.getByTestId('progress')).toHaveText('2 / 4');
  expect(await headwordOnScreen(page)).toBe('brontle');
  expect((await currentSession(app)).answered).toBe(1);
  // It can still be revealed (if needed) and rated.
  if (await page.getByTestId('reveal').count()) await page.getByTestId('reveal').click();
  await waitGuard();
  await page.keyboard.press('3');
  await expect(page.getByTestId('toast')).toContainText('Rated 3');
  await expect(page.getByTestId('progress')).toHaveText('3 / 4');
});

test('R-54 ✕ ends the block at once (no confirmation) and the summary says what stays due', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await startBookBlock(page, app);
  await revealByKey(page);
  await page.keyboard.press('1');
  await page.getByRole('button', { name: /^End block/ }).click();
  await expect(page).toHaveURL(/#\/summary$/);
  await expect(page.getByRole('heading', { name: 'Block ended' })).toBeVisible();
  await expect(page.getByText('3 cards not rated — they stay due.')).toBeVisible();
  // The block is closed: Today offers a fresh start rather than a resume.
  await openToday(page, app.url);
  await expect(page.getByTestId('resume')).toHaveCount(0);
});

test('R-54 "Continue +N" after a finished block starts another block of N', async ({ page, app, pageErrors }) => {
  void pageErrors;
  const words = Array.from({ length: 12 }, (_, i) => ({
    headword: `test-cont-${i}`,
    meaning: 'An invented continuation word.',
    czech: ['pokračování'],
    usefulness: 4,
    sentences: [{ en: `A [[test-cont-${i}]] appears.`, cs: `Objeví se [[pokračování]].`, origin: 'example' }],
  }));
  const pack = {
    format: 'vocab-pack',
    format_version: 1,
    pack_id: 'test-e2e-cont--2026-11-01--1',
    exported_at: '2026-11-01',
    source: { id: 'test-e2e-cont', type: 'general', title: 'Continuation Source' },
    words,
  };
  await app.api('POST', '/api/import/commit', { text: JSON.stringify(pack) });
  await app.api('PATCH', '/api/settings', { new_per_day: 15, block_size: 5 });
  await page.goto(`${app.url}/#/`);
  await page.getByLabel(/Continuation Source/).check();
  await expect(page.getByTestId('start')).toHaveText('Start');
  await page.getByTestId('start').click();
  await expect(page.getByTestId('progress')).toHaveText('1 / 5');
  for (let i = 0; i < 5; i += 1) {
    await revealByKey(page);
    await rateByKey(page, 1);
    await page.evaluate(() => document.querySelector('[data-testid="toast"]')?.remove());
  }
  await page.getByTestId('see-summary').click();
  await expect(page.getByTestId('summary-line')).toHaveText('5 cards · 5 new words');
  await expect(page.getByTestId('continue')).toHaveText('Continue +5');
  await page.getByTestId('continue').click();
  await expect(page).toHaveURL(/#\/practice$/);
  await expect(page.getByTestId('progress')).toHaveText('1 / 5');
  // The first block's words are not offered again.
  const session = await currentSession(app);
  expect(session.block_no).toBe(2);
  expect(new Set(session.items.map((/** @type {any} */ i) => i.word.headword)).size).toBe(5);
  // Today now says Continue.
  await page.goto(`${app.url}/#/`);
  await expect(page.getByTestId('resume')).toBeVisible();
});

test('R-54 while a backlog pauses new words, a normal Start brings none; "anyway" does', async ({ page, app, pageErrors }, info) => {
  test.skip(info.project.name !== 'chromium-desktop', 'one viewport is enough');
  void pageErrors;
  const words = Array.from({ length: 41 }, (_, i) => ({
    headword: `test-bl-${i}`,
    meaning: 'An invented backlog word.',
    czech: ['zkouška'],
    usefulness: 3,
    sentences: [{ en: `A [[test-bl-${i}]] here.`, cs: `Tady [[zkouška]].`, origin: 'example' }],
  }));
  const pack = {
    format: 'vocab-pack',
    format_version: 1,
    pack_id: 'test-e2e-bl--2026-11-01--1',
    exported_at: '2026-11-01',
    source: { id: 'test-e2e-bl', type: 'general', title: 'Backlog' },
    words,
  };
  await app.api('POST', '/api/import/commit', { text: JSON.stringify(pack) });
  const today = await app.api('GET', '/api/today');
  const db = new DatabaseSync(app.dbPath);
  db.prepare(
    `UPDATE card SET state = 'review', stability = 5.0, difficulty = 5.0, due_day = ?, last_review_day = '2026-11-01',
     introduced_day = '2026-11-01', reps = 2, format = 'word_only'
     WHERE word_id IN (SELECT id FROM word WHERE headword LIKE 'test-bl-%')`,
  ).run(today.study_day);
  db.close();
  await page.goto(`${app.url}/#/`);
  await expect(page.getByTestId('backlog')).toBeVisible();
  await expect(page.getByTestId('new-left')).toHaveCount(0);
  await page.getByTestId('start').click();
  await expect(page).toHaveURL(/#\/practice$/);
  const session = await currentSession(app);
  expect(session.items.some((/** @type {any} */ i) => i.card.is_new), 'no new word while paused').toBe(false);
});

// ── R-59: scope ──────────────────────────────────────────────────────────────

test('R-59 a block scoped to one source holds only that source and Today names it', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await page.goto(`${app.url}/#/`);
  // The book shows its reading position, the article (no position) does not.
  await expect(page.getByText('reading: Chapter 7')).toBeVisible();
  await expect(page.getByText(/reading:/)).toHaveCount(1);
  await page.getByLabel(/An E2E Test Novel/).check();
  await page.getByTestId('start').click();
  await expect(page).toHaveURL(/#\/practice$/);
  const session = await currentSession(app);
  const heads = session.items.map((/** @type {any} */ i) => i.word.headword).sort();
  expect(heads).toEqual(['brontle', 'glimmerous', 'quabble', 'snorkish']);
  // The caption of each sentence names the book, not the other source.
  await page.goto(`${app.url}/#/`);
  await expect(page.getByTestId('resume')).toHaveText('Resume — 4 of 4 left');
  await expect(page.getByText('Block in progress (An E2E Test Novel).')).toBeVisible();
});

// ── R-52 R-53 R-74: card layout ──────────────────────────────────────────────

test('R-53 R-74 sentence question: minimal cue, highlighted target, criterion line, source caption', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await startBookBlock(page, app);
  const question = page.getByTestId('sentence-en');
  await expect(question).toContainText('The glimmerous lamp swung above the door.');
  // Target = bold + underline (not colour alone).
  const style = await question.locator('strong.target').evaluate((el) => {
    const s = getComputedStyle(el);
    return { weight: Number(s.fontWeight), underline: s.textDecorationLine };
  });
  expect(style.weight).toBeGreaterThanOrEqual(600);
  expect(style.underline).toContain('underline');
  // Sentence size ≥ 19 px, line height ≥ 1.5.
  const metrics = await question.evaluate((el) => {
    const s = getComputedStyle(el);
    return { size: parseFloat(s.fontSize), line: parseFloat(s.lineHeight) / parseFloat(s.fontSize) };
  });
  expect(metrics.size).toBeGreaterThanOrEqual(19);
  expect(metrics.line).toBeGreaterThanOrEqual(1.49);
  // Hidden until the reveal: Czech, gloss, meaning, icon, respelling, note, forms.
  const text = (await page.locator('main, body').first().innerText()).toLowerCase();
  for (const hidden of ['mihotavá', 'třpytivý', 'shining faintly', '✨', 'glim-er-us', 'invented test word', 'glimmerously']) {
    expect(text, `"${hidden}" is hidden on the question side`).not.toContain(hidden.toLowerCase());
  }
  await expect(page.getByTestId('sentence-cs')).toHaveCount(0);
  await expect(page.locator('button.rate')).toHaveCount(0);
  // Criterion line above the reveal button, about the highlighted word.
  await expect(page.getByText('Rate the highlighted word: would you have understood it here, without help?')).toBeVisible();
  // Source caption under the sentence.
  await expect(page.locator('.caption').first()).toContainText(/An E2E Test Novel, Chapter \d/);
});

test('R-52 R-74 word-only answer: Czech gloss, meaning and one example with its translation', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await startBookBlock(page, app);
  await revealByKey(page);
  await page.keyboard.press('1'); // glimmerous → brontle (1 sentence → word-only)
  await expect(page.getByTestId('headword-question')).toHaveText('brontle');
  await expect(page.getByTestId('sentence-en')).toHaveCount(0);
  await expect(page.getByText('Rate the word: would you have known what it means, without help?')).toBeVisible();
  await expect(page.getByTestId('reveal')).toHaveText('Show answer');
  await page.keyboard.press('Space');
  await expect(page.locator('.word-gloss')).toHaveText('bručet si');
  await expect(page.locator('.word-meaning')).toHaveText('To grumble quietly while working.');
  await expect(page.getByTestId('sentence-en')).toContainText('The old cook would brontle over the pots.');
  await expect(page.getByTestId('sentence-cs')).toContainText('Starý kuchař si nad hrnci bručel.');
  // The pronunciation respelling appears on the answer side only.
  await expect(page.getByText('BRON-tul')).toBeVisible();
});

test('R-52 R-53 answer side of a sentence card: Czech translation, strip with icon/respelling/gloss, More', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await startBookBlock(page, app);
  await page.keyboard.press('Space');
  await expect(page.getByTestId('sentence-cs')).toContainText('Nad dveřmi se houpala mihotavá lampa.');
  await expect(page.getByTestId('sentence-en')).toContainText('The glimmerous lamp swung above the door.');
  await expect(page.getByTestId('headword')).toHaveText('glimmerous');
  await expect(page.locator('.word-icon')).toHaveText('✨');
  await expect(page.getByText('GLIM-er-us')).toBeVisible();
  await expect(page.locator('.word-gloss')).toHaveText('třpytivý, mihotavý');
  // A new word opens the full learn view: note, forms, the other sentences.
  const more = page.getByTestId('more');
  await expect(more).toContainText('An invented test word: used for light that comes and goes.');
  await expect(more).toContainText('Think of a candle in a draught.');
  await expect(more).toContainText('glimmerously');
  await expect(more).toContainText('Her eyes turned glimmerous in the dusk.');
  await expect(more).toContainText('A glimmerous path led to the lake.');
  // The sentence on screen is not repeated among the other sentences.
  await expect(more).not.toContainText('The glimmerous lamp swung above the door.');
});

test('R-52 R-74 a reviewed word starts with More closed and a 4 opens it', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await makeReview(app, 'glimmerous');
  await page.goto(`${app.url}/#/`);
  await page.getByLabel(/An E2E Test Novel/).check();
  await page.getByTestId('start').click();
  await expect(page.getByTestId('progress')).toHaveText(/^1 \/ \d$/);
  expect(await headwordOnScreen(page)).toBe('glimmerous');
  await page.keyboard.press('Space');
  await expect(page.locator('button.rate')).toHaveCount(5);
  await expect(page.getByTestId('more')).toHaveCount(0);
  const toggle = page.locator('button.more-toggle');
  await expect(toggle).toHaveAttribute('aria-expanded', 'false');
  // M opens and closes it in place.
  await page.keyboard.press('m');
  await expect(toggle).toHaveAttribute('aria-expanded', 'true');
  await page.keyboard.press('m');
  await expect(page.getByTestId('more')).toHaveCount(0);
  await waitGuard();
  await page.keyboard.press('4');
  // After a 4 the note is open, and there is nothing like "confused with…?" (R-74).
  await expect(page.getByTestId('more')).toBeVisible();
  await expect(page.getByTestId('next')).toBeVisible();
  await expect(page.getByText(/confused with/i)).toHaveCount(0);
  await expect(page.locator('button.rate')).toHaveCount(0);
});

test('R-74 Czech text is marked lang="cs" and English lang="en"', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await startBookBlock(page, app);
  await page.keyboard.press('Space');
  await expect(page.getByTestId('more')).toBeVisible();
  const unmarked = await page.evaluate(() => {
    /** @type {string[]} */
    const bad = [];
    const czech = /[ěščřžýáíéůúďťňĚŠČŘŽÝÁÍÉŮÚĎŤŇ]/;
    const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
    for (let node = walker.nextNode(); node; node = walker.nextNode()) {
      const text = node.textContent ?? '';
      if (!czech.test(text)) continue;
      const parent = node.parentElement;
      if (!parent || parent.closest('[lang="cs"], [aria-live]') === null) bad.push(text.trim().slice(0, 40));
    }
    return bad;
  });
  expect(unmarked, 'Czech text outside lang="cs"').toEqual([]);
  await expect(page.getByTestId('sentence-cs').locator('[lang="cs"]').first()).toBeVisible();
  await expect(page.locator('.word-gloss')).toHaveAttribute('lang', 'cs');
  await expect(page.getByTestId('sentence-en').locator('[lang="en"]').first()).toBeVisible();
  await expect(page.getByTestId('headword')).toHaveAttribute('lang', 'en');
});

test('R-74 the Czech gloss in the screen-reader announcement is marked lang="cs"', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await startBookBlock(page, app);
  await page.keyboard.press('Space');
  const live = page.locator('[aria-live="polite"][role="status"]').filter({ hasText: 'třpytivý' });
  await expect(live.locator('[lang="cs"]')).toContainText('třpytivý');
});

test('R-74 the result of the reveal and the rating are announced in a live region', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await startBookBlock(page, app);
  await page.keyboard.press('Space');
  await expect(page.locator('[aria-live="polite"][role="status"]').filter({ hasText: 'třpytivý' })).toHaveCount(1);
  await waitGuard();
  await page.keyboard.press('2');
  await expect(page.locator('[aria-live]').filter({ hasText: 'Rated 2 · Knew it' }).first()).toBeAttached();
});

// ── R-41 R-70: speech ────────────────────────────────────────────────────────

test('R-41 ▶ buttons speak the pack\'s `speak` text and the plain sentence only when tapped', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await importStress(app);
  await page.goto(`${app.url}/#/`);
  await page.getByLabel(/E2E Stress Source/).check();
  await page.getByTestId('start').click();
  await expect(page.getByTestId('progress')).toHaveText('1 / 2');
  const spoken = () => page.evaluate(() => /** @type {any} */ (window).spoken.map((/** @type {any} */ s) => s.text));
  // Nothing is spoken by itself, nothing plays as recorded audio.
  expect(await spoken()).toEqual([]);
  await page.getByRole('button', { name: 'Play sentence' }).click();
  expect(await spoken()).toEqual(['The antidisestablishmentarianistic mood lasted well past midnight in the hall.']);
  await page.getByTestId('reveal').click();
  await expect(page.locator('button.rate')).toHaveCount(5);
  expect(await spoken(), 'no auto-play on reveal').toHaveLength(1);
  await page.getByRole('button', { name: 'Play word' }).click();
  expect((await spoken()).at(-1), 'the word is spoken from `speak`, not the headword').toBe('kwux');
  // Neither the Czech sentence nor "[[" markers are ever spoken.
  await page.getByRole('button', { name: 'Play sentence' }).last().click();
  for (const text of await spoken()) {
    expect(text).not.toContain('[[');
    expect(text).not.toContain('protistátní');
  }
  expect(await page.locator('audio').count()).toBe(0);
});

test('R-41 without speech support the ▶ buttons say so and are disabled', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await page.addInitScript(() => {
    // @ts-ignore
    delete window.speechSynthesis;
  });
  await startBookBlock(page, app);
  const play = page.locator('button.play-sentence');
  await expect(play).toBeDisabled();
  await expect(play).toHaveAccessibleName(/not available/i);
  await page.keyboard.press('s');
  await expect(page.getByTestId('progress')).toHaveText('1 / 4');
});

// ── R-70: keyboard ───────────────────────────────────────────────────────────

test('R-70 E opens the word detail; Esc ends the block; ? toggles the help with the key list', async ({ page, app, pageErrors }, info) => {
  test.skip(info.project.name !== 'chromium-desktop', 'keyboard only');
  void pageErrors;
  await startBookBlock(page, app);
  await page.keyboard.press('?');
  const keys = page.locator('.shortcuts');
  await expect(keys).toBeVisible();
  for (const label of ['Space', '1 – 5', 'U', 'M', 'S / R', 'E', 'Esc', '?']) {
    await expect(keys.locator('kbd', { hasText: label }).first()).toBeVisible();
  }
  await page.keyboard.press('?');
  await expect(keys).toHaveCount(0);
  await page.keyboard.press('Space');
  await expect(page.locator('button.rate')).toHaveCount(5);
  await page.keyboard.press('e');
  await expect(page).toHaveURL(/#\/words\/\d+$/);
  await expect(page.getByRole('heading', { name: /glimmerous/ })).toBeVisible();
});

test('R-70 Tab reaches the rating buttons with a visible focus ring; Space on one never rates, Enter does', async ({ page, app, pageErrors }, info) => {
  test.skip(info.project.name !== 'chromium-desktop', 'keyboard only');
  void pageErrors;
  await startBookBlock(page, app);
  await revealByKey(page);
  const first = page.locator('button.rate').first();
  for (let i = 0; i < 20; i += 1) {
    await page.keyboard.press('Tab');
    if (await first.evaluate((el) => el === document.activeElement)) break;
  }
  await expect(first).toBeFocused();
  const ring = await first.evaluate((el) => {
    const s = getComputedStyle(el);
    return { outline: s.outlineStyle, width: parseFloat(s.outlineWidth), shadow: s.boxShadow };
  });
  expect(ring.outline !== 'none' && ring.width >= 2 ? true : ring.shadow !== 'none', 'visible focus indicator').toBe(true);
  await page.keyboard.press('Space');
  await page.waitForTimeout(200);
  await expect(page.getByTestId('toast')).toHaveCount(0);
  expect((await currentSession(app)).answered).toBe(0);
  await page.keyboard.press('Enter');
  await expect(page.getByTestId('toast')).toContainText('Rated 1');
});

// ── R-70 R-74: mobile ergonomics and layout ──────────────────────────────────

test('R-70 R-74 on the phone Reveal/Next/rating sit in the bottom third, ✕ ? ↶ at the top, Reveal and Next in the same place', async ({ page, app, pageErrors }, info) => {
  test.skip(info.project.name !== 'webkit-phone', 'phone geometry');
  void pageErrors;
  await startBookBlock(page, app, { touch: true });
  const height = 812;
  const reveal = (await page.getByTestId('reveal').boundingBox())?.valueOf();
  expect(reveal).toBeTruthy();
  const r = /** @type {NonNullable<typeof reveal>} */ (reveal);
  expect(r.y).toBeGreaterThan((height * 2) / 3 - 40);
  expect(r.width, 'Reveal is full width').toBeGreaterThan(375 * 0.8);
  for (const name of [/^End block/, /^Help/, /^Undo/]) {
    const box = await page.getByRole('button', { name }).boundingBox();
    expect(box && box.y + box.height < 100, `${name} is in the top bar`).toBe(true);
  }
  await page.getByTestId('reveal').tap();
  await waitGuard();
  for (const button of await page.locator('button.rate').all()) {
    const box = await button.boundingBox();
    expect(box && box.y > (height * 2) / 3 - 40, 'rating row in the bottom third').toBe(true);
    expect(box && box.width >= 56 && box.height >= 44).toBe(true);
  }
  // A 4 → the Next button takes the Reveal button's place (same x, width and bottom edge).
  await page.getByRole('button', { name: /^Rate 4,/ }).tap();
  const next = await page.getByTestId('next').boundingBox();
  expect(next).toBeTruthy();
  const n = /** @type {NonNullable<typeof next>} */ (next);
  expect(Math.abs(n.x - r.x)).toBeLessThan(2);
  expect(Math.abs(n.width - r.width)).toBeLessThan(2);
  expect(Math.abs(n.y + n.height - (r.y + r.height))).toBeLessThan(10);
});

test('R-70 every interactive target on the practice, Today, summary and settings screens is at least 44×44 px on 375×812', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await page.setViewportSize({ width: 375, height: 812 });
  /** @type {string[]} */
  const failures = [];
  await walkScreens(page, app, async (label) => {
    for (const item of await page.evaluate(undersizedControls, { skip: 'nav, .look-again' })) failures.push(`${label}: ${item}`);
  });
  expect(failures).toEqual([]);
});

test('R-70 the navigation links and the "look at these again" links are at least 44×44 px on 375×812', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await page.setViewportSize({ width: 375, height: 812 });
  /** @type {string[]} */
  const failures = [];
  await walkScreens(page, app, async (label) => {
    for (const item of await page.evaluate(undersizedControls, { only: 'nav, .look-again' })) failures.push(`${label}: ${item}`);
  });
  expect(failures).toEqual([]);
});

for (const width of [320, 375]) {
  test(`R-70 R-74 no horizontal scroll at ${width} px on any M4 screen; rating labels stay visible`, async ({ page, app, pageErrors }) => {
    void pageErrors;
    await page.setViewportSize({ width, height: 640 });
    /** @type {string[]} */
    const failures = [];
    await walkScreens(page, app, async (label) => {
      const wide = await page.evaluate(() => ({
        doc: document.documentElement.scrollWidth,
        body: document.body.scrollWidth,
        inner: window.innerWidth,
      }));
      if (wide.doc > wide.inner || wide.body > wide.inner) failures.push(`${label}: scrollWidth ${wide.doc}/${wide.body} > ${wide.inner}`);
      const buttons = page.locator('button.rate');
      if (await buttons.count()) {
        const clipped = await buttons.evaluateAll((els) =>
          els
            .filter((el) => {
              const label = el.querySelector('.rate-label');
              if (!label) return true;
              const b = el.getBoundingClientRect();
              const l = label.getBoundingClientRect();
              const style = getComputedStyle(label);
              return style.display === 'none' || l.width === 0 || l.left < b.left - 1 || l.right > b.right + 1 || label.scrollWidth > label.clientWidth + 1;
            })
            .map((el) => el.getAttribute('aria-label')),
        );
        if (clipped.length) failures.push(`${label}: rating label clipped or hidden: ${clipped.join('; ')}`);
      }
    });
    expect(failures).toEqual([]);
  });
}

for (const [width, height] of [
  [375, 667],
  [320, 568],
]) {
  test(`R-74 the fixed rating bar never covers the answer: the last line of a long note scrolls into view above it (${width}×${height})`, async ({ page, app, pageErrors }) => {
    void pageErrors;
    await page.setViewportSize({ width, height });
    await importStress(app);
    await page.goto(`${app.url}/#/`);
    await page.getByLabel(/E2E Stress Source/).check();
    await page.getByTestId('start').click();
    await expect(page.getByTestId('progress')).toHaveText('1 / 2');
    await page.getByTestId('reveal').click();
    await expect(page.getByTestId('more')).toBeVisible();
    for (const phase of ['answer', 'feedback']) {
      if (phase === 'feedback') {
        await waitGuard();
        await page.getByRole('button', { name: /^Rate 4,/ }).click();
        await expect(page.getByTestId('next')).toBeVisible();
      }
      await page.evaluate(() => window.scrollTo(0, document.documentElement.scrollHeight));
      await page.waitForTimeout(100);
      const gap = await page.evaluate(() => {
        const more = document.querySelector('[data-testid="more"]');
        const bar = document.querySelector('.practice-bottom');
        if (!more || !bar) return null;
        const last = more.lastElementChild ?? more;
        const barTop = bar.getBoundingClientRect().top;
        const lastBottom = last.getBoundingClientRect().bottom;
        return { barTop, lastBottom, vh: window.innerHeight };
      });
      expect(gap, `${phase}: More and the bar exist`).not.toBeNull();
      const g = /** @type {NonNullable<typeof gap>} */ (gap);
      expect(g.lastBottom, `${phase}: last line of More ends above the bar`).toBeLessThanOrEqual(g.barTop + 1);
    }
  });
}

// ── R-71 R-74: theme, contrast, motion ───────────────────────────────────────

test('R-71 the chosen theme applies on the practice screen and survives a reload', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await page.emulateMedia({ colorScheme: 'dark' });
  await page.goto(`${app.url}/#/settings`);
  await page.getByLabel('Light').check();
  await expect(page.getByTestId('settings-status')).toHaveText('Saved.');
  await startBookBlock(page, app);
  const background = () => page.evaluate(() => getComputedStyle(document.body).backgroundColor);
  expect(await background()).toBe('rgb(247, 247, 245)');
  await page.reload();
  await expect(page.getByTestId('progress')).toBeVisible();
  expect(await background()).toBe('rgb(247, 247, 245)');
  // Light stays light even when the system is dark.
  expect(await page.evaluate(() => document.documentElement.getAttribute('data-theme'))).toBe('light');
});

for (const scheme of /** @type {const} */ (['dark', 'light'])) {
  test(`R-71 R-74 text contrast on the answer side is at least 4.5:1 in the ${scheme} theme`, async ({ page, app, pageErrors }) => {
    void pageErrors;
    await app.api('PATCH', '/api/settings', { theme: scheme });
    await startBookBlock(page, app);
    await page.keyboard.press('Space');
    await expect(page.getByTestId('more')).toBeVisible();
    await page.getByRole('button', { name: /^Help/ }).click();
    const low = await page.evaluate(() => {
      /** @param {string} css @returns {number[]} */
      const parse = (css) => (css.match(/[\d.]+/g) ?? []).map(Number);
      /** @param {number[]} c */
      const lum = (c) => {
        const [r, g, b] = c.slice(0, 3).map((v) => {
          const s = v / 255;
          return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
        });
        return 0.2126 * r + 0.7152 * g + 0.0722 * b;
      };
      /** @param {Element} el @returns {number[]} */
      const background = (el) => {
        for (let e = /** @type {Element | null} */ (el); e; e = e.parentElement) {
          const c = parse(getComputedStyle(e).backgroundColor);
          if (c.length >= 3 && (c.length === 3 || c[3] > 0.9)) return c;
        }
        return parse(getComputedStyle(document.body).backgroundColor);
      };
      /** @type {string[]} */
      const failures = [];
      const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
      for (let node = walker.nextNode(); node; node = walker.nextNode()) {
        if (!(node.textContent ?? '').trim()) continue;
        const el = node.parentElement;
        if (!el) continue;
        const style = getComputedStyle(el);
        if (style.visibility === 'hidden' || style.display === 'none') continue;
        if (el.closest('button:disabled')) continue; // disabled controls are exempt from contrast rules
        const rect = el.getBoundingClientRect();
        if (rect.width <= 1 || rect.height <= 1) continue;
        const fg = parse(style.color);
        const bg = background(el);
        let opacity = 1;
        for (let e = /** @type {Element | null} */ (el); e; e = e.parentElement) opacity *= Number(getComputedStyle(e).opacity);
        const alpha = (fg.length > 3 ? fg[3] : 1) * opacity;
        const mixed = [0, 1, 2].map((i) => fg[i] * alpha + bg[i] * (1 - alpha));
        const l1 = lum(mixed);
        const l2 = lum(bg);
        const ratio = (Math.max(l1, l2) + 0.05) / (Math.min(l1, l2) + 0.05);
        if (ratio < 4.5) failures.push(`${(node.textContent ?? '').trim().slice(0, 30)} (${ratio.toFixed(2)}, .${el.className})`);
      }
      return failures;
    });
    expect(low).toEqual([]);
  });
}

test('R-74 no animation or transition with prefers-reduced-motion; otherwise at most 150 ms', async ({ page, app, pageErrors }) => {
  void pageErrors;
  /** Longest animation/transition (ms) among all elements; flips are never allowed. */
  const motion = () =>
    page.evaluate(() => {
      let longest = 0;
      let flip = false;
      /** @param {string} list */
      const longestOf = (list) => Math.max(0, ...list.split(',').map((t) => parseFloat(t) * (t.trim().endsWith('ms') ? 1 : 1000)));
      for (const el of document.querySelectorAll('*')) {
        const s = getComputedStyle(el);
        if (s.animationName !== 'none') longest = Math.max(longest, longestOf(s.animationDuration) + longestOf(s.animationDelay));
        longest = Math.max(longest, longestOf(s.transitionDuration));
        if (/rotate|perspective|matrix3d/.test(s.transform)) flip = true;
      }
      return { longest, flip };
    });
  await startBookBlock(page, app);
  await page.keyboard.press('Space');
  await waitGuard();
  await page.keyboard.press('4'); // toast on screen
  await expect(page.getByTestId('toast')).toBeVisible();
  const normal = await motion();
  expect(normal.flip, 'no 3-D flip').toBe(false);
  // Slip-guard pulse is allowed to exceed 150 ms only as an attention cue; fades must not.
  const fades = await page.evaluate(() =>
    [...document.querySelectorAll('*')]
      .filter((el) => !el.classList.contains('pulse'))
      .map((el) => getComputedStyle(el).transitionDuration)
      .join(','),
  );
  for (const d of fades.split(',')) {
    const ms = parseFloat(d) * (d.trim().endsWith('ms') ? 1 : 1000);
    expect(ms, `transition ${d}`).toBeLessThanOrEqual(150);
  }

  await page.emulateMedia({ reducedMotion: 'reduce' });
  await page.keyboard.press('Enter'); // Next
  await page.keyboard.press('Space');
  await page.keyboard.press('?');
  const reduced = await motion();
  expect(reduced.longest, 'no motion under reduced motion').toBe(0);
});

// ── R-74: everything stays free of errors on every screen ────────────────────

test('R-74 no console errors, CSP violations or failed requests on any screen', async ({ page, app, pageErrors }) => {
  void pageErrors; // the fixture asserts console/page errors are empty at the end
  /** @type {string[]} */
  const bad = [];
  page.on('response', (response) => {
    if (response.status() >= 400) bad.push(`${response.status()} ${response.url()}`);
  });
  page.on('requestfailed', (request) => bad.push(`failed ${request.url()}`));
  await page.addInitScript(() => {
    document.addEventListener('securitypolicyviolation', (e) => {
      console.error(`CSP violation: ${e.violatedDirective} ${e.blockedURI}`);
    });
  });
  await walkScreens(page, app, async () => {});
  await page.goto(`${app.url}/#/import`);
  await page.waitForLoadState('networkidle');
  expect(bad).toEqual([]);
});

test('R-70 R-74 a 30-letter headword does not cause horizontal scroll at 320 px', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await page.setViewportSize({ width: 320, height: 640 });
  /** @type {string[]} */
  const failures = [];
  await walkScreens(
    page,
    app,
    async (label) => {
      const wide = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
      if (wide > 0) failures.push(`${label}: ${wide} px too wide`);
    },
    { long: true },
  );
  expect(failures).toEqual([]);
});

test('R-74 the Words screen logs no CSP violation', async ({ page, app, pageErrors }, info) => {
  void pageErrors;
  await page.goto(`${app.url}/#/words`);
  await expect(page.getByLabel('Search words')).toBeVisible();
  await page.waitForTimeout(300);
});
