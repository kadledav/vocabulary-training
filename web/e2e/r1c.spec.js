// @ts-check
/**
 * Fix round R1-C (frontend review and owner feedback after the first
 * deployment): the answer side on small screens, the toast, the double-tap
 * guard, the end of the day (F1), import from files (F3), log out
 * everywhere (F4), Words returning to its place, keyboard focus, settings
 * ranges and the study-day rollover. Synthetic data only.
 */

import { randomUUID } from 'node:crypto';
import { deflateRawSync, crc32 } from 'node:zlib';

import { advanceClock, expect, freezeClock, installClock, startBookBlock, test, waitGuard } from './fixtures.js';

// ── helpers ──────────────────────────────────────────────────────────────────

/**
 * Practise one whole block through the API, every card rated `grade`, and
 * finish it.
 *
 * @param {import('./fixtures.js').App} app
 * @param {number} grade
 * @param {Record<string, unknown>} [start]
 */
async function practiseByApi(app, grade, start = { kind: 'start' }) {
  const started = await app.api('POST', '/api/sessions', start);
  let session = started.session;
  if (!session) throw new Error('nothing to practise');
  for (;;) {
    const item = session.items.find((/** @type {any} */ i) => !i.answered);
    if (!item) break;
    const rated = await app.api('POST', `/api/sessions/${session.id}/reviews`, {
      client_review_id: randomUUID(),
      item_id: item.item_id,
      raw_grade: grade,
      hint_used: false,
      duration_ms: 2000,
    });
    session = rated.session;
  }
  await app.api('POST', `/api/sessions/${session.id}/finish`, {});
}

/**
 * A pack of `n` invented words.
 *
 * @param {string} id
 * @param {number} n
 */
function wordsPack(id, n) {
  return {
    format: 'vocab-pack',
    format_version: 1,
    pack_id: `test-e2e-${id}--2026-11-01--1`,
    exported_at: '2026-11-01',
    source: { id: `test-e2e-${id}`, type: 'general', title: `Source ${id}` },
    words: Array.from({ length: n }, (_, i) => ({
      headword: `test-${id}-${i}`,
      meaning: 'An invented word.',
      czech: ['slovo'],
      usefulness: 3,
      sentences: [{ en: `A [[test-${id}-${i}]] here.`, cs: `Tady [[slovo]].`, origin: 'example' }],
    })),
  };
}

/**
 * A zip archive (deflated entries) built by hand.
 *
 * @param {{ name: string, text: string }[]} entries
 */
function zipOf(entries) {
  /** @type {Buffer[]} */
  const locals = [];
  /** @type {Buffer[]} */
  const centrals = [];
  let offset = 0;
  for (const e of entries) {
    const name = Buffer.from(e.name);
    const raw = Buffer.from(e.text);
    const data = deflateRawSync(raw);
    const crc = crc32(raw) >>> 0;
    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0);
    local.writeUInt16LE(20, 4);
    local.writeUInt16LE(8, 8);
    local.writeUInt32LE(crc, 14);
    local.writeUInt32LE(data.length, 18);
    local.writeUInt32LE(raw.length, 22);
    local.writeUInt16LE(name.length, 26);
    locals.push(local, name, data);
    const central = Buffer.alloc(46);
    central.writeUInt32LE(0x02014b50, 0);
    central.writeUInt16LE(20, 6);
    central.writeUInt16LE(8, 10);
    central.writeUInt32LE(crc, 16);
    central.writeUInt32LE(data.length, 20);
    central.writeUInt32LE(raw.length, 24);
    central.writeUInt16LE(name.length, 28);
    central.writeUInt32LE(offset, 42);
    centrals.push(central, name);
    offset += 30 + name.length + data.length;
  }
  const dir = Buffer.concat(centrals);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(entries.length, 8);
  end.writeUInt16LE(entries.length, 10);
  end.writeUInt32LE(dir.length, 12);
  end.writeUInt32LE(offset, 16);
  return Buffer.concat([...locals, dir, end]);
}

/** @param {import('@playwright/test').Page} page */
function noHorizontalScroll(page) {
  return page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth);
}

// ── bug 1, 2: the answer side on small screens ───────────────────────────────

for (const [width, height] of [
  [390, 664],
  [320, 568],
  [664, 390],
]) {
  test(`R-74 bug 1 the legend never hides the answer; the bar stays under 40 % (${width}×${height})`, async ({ page, app, pageErrors }) => {
    void pageErrors;
    await page.setViewportSize({ width, height });
    await startBookBlock(page, app);
    await page.getByTestId('reveal').click();
    await expect(page.getByTestId('legend')).toBeVisible();
    const layout = await page.evaluate(() => {
      const bar = /** @type {HTMLElement} */ (document.querySelector('.practice-bottom'));
      const head = /** @type {HTMLElement} */ (document.querySelector('[data-testid="headword"]'));
      // Landscape: the card scrolls; the headword must be reachable above the bar.
      head.scrollIntoView({ block: 'center' });
      return {
        barTop: bar.getBoundingClientRect().top,
        barHeight: bar.getBoundingClientRect().height,
        headBottom: head.getBoundingClientRect().bottom,
        legendInBar: bar.querySelector('[data-testid="legend"]') !== null,
        vh: window.innerHeight,
      };
    });
    expect(layout.legendInBar, 'legend is not in the pinned bar').toBe(false);
    expect(layout.barHeight).toBeLessThanOrEqual(layout.vh * 0.4 + 1);
    expect(layout.headBottom, 'the headword shows above the bar').toBeLessThanOrEqual(layout.barTop);
    for (const grade of [1, 5]) await expect(page.getByRole('button', { name: new RegExp(`^Rate ${grade},`) })).toBeInViewport();
    expect(await noHorizontalScroll(page)).toBe(true);
  });
}

test('R-74 bug 1 at 200 % text the rating buttons stay reachable and nothing scrolls sideways', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await page.setViewportSize({ width: 320, height: 568 });
  await page.goto(`${app.url}/#/`);
  await page.evaluate(() => {
    document.documentElement.style.fontSize = '200%';
  });
  await expect(page.getByTestId('start')).toBeVisible();
  expect(await noHorizontalScroll(page), 'Today at 200 % text').toBe(true);
  await page.getByLabel(/An E2E Test Novel/).check();
  await page.getByTestId('start').click();
  await page.evaluate(() => {
    document.documentElement.style.fontSize = '200%';
  });
  await page.getByTestId('reveal').click();
  const button = page.getByRole('button', { name: /^Rate 2,/ });
  await button.scrollIntoViewIfNeeded();
  await expect(button).toBeInViewport();
  const bar = await page.locator('.practice-bottom').boundingBox();
  expect(bar && bar.height <= 568 * 0.4 + 1).toBe(true);
});

test('R-62 bug 2 the "Rated … Undo" toast is a row of the bottom bar and never covers the card', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await page.setViewportSize({ width: 390, height: 664 });
  await startBookBlock(page, app);
  await page.getByTestId('reveal').click();
  await waitGuard();
  await page.getByRole('button', { name: /^Rate 2,/ }).click();
  const toast = page.getByTestId('toast');
  await expect(toast).toContainText('Rated 2');
  expect(await toast.evaluate((el) => el.closest('.practice-bottom') !== null)).toBe(true);
  const card = await page.locator('.practice-body').evaluate((el) => {
    const r = el.getBoundingClientRect();
    return { top: r.top, bottom: r.bottom };
  });
  const box = await toast.boundingBox();
  const word = await page.getByTestId('headword-question').boundingBox();
  expect(box && word).toBeTruthy();
  const t = /** @type {NonNullable<typeof box>} */ (box);
  const w = /** @type {NonNullable<typeof word>} */ (word);
  const overlaps = t.y < w.y + w.height && w.y < t.y + t.height && t.x < w.x + w.width && w.x < t.x + t.width;
  expect(overlaps, 'the toast does not cover the word').toBe(false);
  expect(t.y).toBeGreaterThanOrEqual(Math.min(card.bottom, w.y + w.height));
});

// ── bug 4: a slow double tap on the reveal button ───────────────────────────

test('R-62 bug 4 a slow double tap on "Show translation" never rates the card', async ({ page, app, pageErrors }, info) => {
  void pageErrors;
  // The page's clock is frozen between the two taps and moved by exact
  // amounts, so the timing never depends on how fast this machine is.
  await installClock(page);
  await startBookBlock(page, app);
  const reveal = await page.getByTestId('reveal').boundingBox();
  if (!reveal) throw new Error('no reveal button');
  const x = reveal.x + reveal.width / 2;
  const y = reveal.y + reveal.height / 2;
  const tap = (/** @type {number} */ px, /** @type {number} */ py) =>
    info.project.name === 'webkit-phone' ? page.touchscreen.tap(px, py) : page.mouse.click(px, py);
  const reviewPosts = /** @type {string[]} */ ([]);
  page.on('request', (request) => {
    if (request.method() === 'POST' && request.url().endsWith('/reviews')) reviewPosts.push(request.url());
  });
  // 300 ms: early in the 500 ms rating guard; 480 ms: just inside its end.
  for (const pause of [300, 480]) {
    await freezeClock(page);
    await advanceClock(page, 250); // past the 250 ms input guard that disables Reveal
    await expect(page.getByTestId('reveal')).toBeEnabled();
    await tap(x, y);
    await expect(page.locator('button.rate')).toHaveCount(5);
    await advanceClock(page, pause);
    // Whatever button is under the finger now, the second tap is ignored.
    await tap(x, y);
    // Rating, if it happened, is a request sent from the tap itself; give
    // it real time to show up (a longer wait cannot make this flaky).
    await page.waitForTimeout(300);
    expect(reviewPosts, `second tap after ${pause} ms sent no rating`).toEqual([]);
    const current = await app.api('GET', '/api/sessions/current');
    expect(current.answered, `second tap after ${pause} ms did not rate`).toBe(0);
    await expect(page.getByTestId('toast')).toHaveCount(0);
    // Past the guard (and the 600 ms same-spot window): a deliberate rating works.
    await advanceClock(page, 700);
    await page.getByRole('button', { name: /^Rate 3,/ }).click();
    await expect(page.getByTestId('toast')).toContainText('Rated 3');
    expect(reviewPosts).toHaveLength(1);
    await page.getByTestId('undo').click();
    await expect(page.getByTestId('toast')).toContainText('Rating undone');
    // Undo leaves the answer side: start over from the question side.
    await page.evaluate(() => document.querySelector('[data-testid="toast"]')?.remove());
    reviewPosts.length = 0;
    await page.reload();
    await expect(page.getByTestId('reveal')).toBeVisible();
  }
});

test('U3 the tapped rating shows pressed at once while it is saved', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await startBookBlock(page, app);
  await page.route('**/reviews', async (route) => {
    await new Promise((resolve) => setTimeout(() => resolve(undefined), 600));
    await route.continue();
  });
  await page.getByTestId('reveal').click();
  await waitGuard();
  await page.getByRole('button', { name: /^Rate 2,/ }).click();
  await expect(page.locator('button.rate.chosen')).toHaveAttribute('data-grade', '2');
  await expect(page.getByTestId('toast')).toContainText('Rated 2');
});

// ── F1: the end of the daily work ────────────────────────────────────────────

test('F1 after the day is done: "Done for today", Practise more starts an extra block of seen words', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await practiseByApi(app, 3);
  await page.goto(`${app.url}/#/`);
  const end = page.getByTestId('end-of-day');
  await expect(end).toContainText(/Done for today — \d+ due tomorrow/);
  await expect(page.getByTestId('start')).toHaveCount(0);
  await expect(page.getByTestId('new-waiting')).toHaveText('1 new word waiting.');
  await expect(page.getByTestId('learn-more')).toHaveText('Learn 1 more new word');
  await expect(page.getByTestId('practise-more')).toBeEnabled();
  await page.getByTestId('practise-more').click();
  await expect(page).toHaveURL(/#\/practice$/);
  const session = await app.api('GET', '/api/sessions/current');
  expect(session.kind).toBe('extra');
  expect(session.items.every((/** @type {any} */ i) => !i.card.is_new)).toBe(true);
  await expect(page.getByTestId('progress')).toHaveText(`1 / ${session.items.length}`);
});

test('F1 "Learn N more new words" starts a block of new words', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await practiseByApi(app, 2);
  await page.goto(`${app.url}/#/`);
  await expect(page.getByTestId('end-of-day')).toBeVisible();
  // Day one, everything rated 1–2: nothing needs extra practice, and it says why.
  await expect(page.getByTestId('practise-more')).toBeDisabled();
  await expect(page.getByTestId('extra-why')).toBeVisible();
  await page.getByTestId('learn-more').click();
  await expect(page).toHaveURL(/#\/practice$/);
  const session = await app.api('GET', '/api/sessions/current');
  expect(session.kind).toBe('more_new');
  expect(session.items.map((/** @type {any} */ i) => i.word.headword)).toEqual(['flumpet']);
});

test('F1 the block summary offers the same two ways on when the day is done', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await app.api('PATCH', '/api/settings', { new_per_day: 4, block_size: 5 });
  await startBookBlock(page, app);
  for (let i = 0; i < 4; i++) {
    await page.getByTestId('reveal').click();
    await waitGuard();
    await page.getByRole('button', { name: /^Rate 3,/ }).click();
    await expect(page.getByTestId('toast')).toContainText('Rated 3');
  }
  await page.getByTestId('see-summary').click();
  await expect(page).toHaveURL(/#\/summary$/);
  await expect(page.getByTestId('end-of-day')).toContainText('Done for today');
  await expect(page.getByTestId('continue')).toHaveCount(0);
  await expect(page.getByTestId('practise-more')).toBeEnabled();
});

test('F1 Today says when new words are paused by the backlog, in plain words', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await page.route('**/api/today*', async (route) => {
    const response = await route.fetch();
    const body = await response.json();
    body.new_paused_reason = 'backlog';
    body.new_words = { ...body.new_words, paused: true, backlog: true, quota: 0, due_now: 60 };
    body.due_now = 60;
    await route.fulfill({ response, json: body });
  });
  await page.goto(`${app.url}/#/`);
  await expect(page.getByTestId('backlog')).toContainText('New words are paused until you catch up on reviews');
  await expect(page.getByTestId('backlog')).toContainText('you can still add some now');
  await expect(page.getByTestId('add-new-anyway')).toBeVisible();
});

test('U1 Start says what the block will hold', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await page.goto(`${app.url}/#/`);
  await page.getByLabel(/An E2E Test Novel/).check();
  await expect(page.getByTestId('start')).toHaveText('Start · 4 cards (4 new)');
});

// ── bug 12: study-day rollover, login history ────────────────────────────────

test('bug 12 a rating refused because the study day rolled over is reported on Today', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await startBookBlock(page, app);
  await page.route('**/reviews', (route) =>
    route.fulfill({
      status: 409,
      json: { error: { code: 'session_expired', message: 'The block belongs to an earlier study day.' } },
    }),
  );
  await page.getByTestId('reveal').click();
  await waitGuard();
  await page.getByRole('button', { name: /^Rate 2,/ }).click();
  await expect(page).toHaveURL(/#\/$/);
  await expect(page.getByTestId('notice')).toHaveText('A new study day has started — your last rating was not saved.');
  const conflict = /^console: Failed to load resource: the server responded with a status of 409/;
  for (let i = pageErrors.length - 1; i >= 0; i -= 1) if (conflict.test(pageErrors[i] ?? '')) pageErrors.splice(i, 1);
});

// ── F3: import from files ────────────────────────────────────────────────────

test('F3 import from a zip: preview, import, the box is cleared and "Start practising" offered', async ({ page, app, pageErrors }) => {
  void pageErrors;
  const zip = zipOf([
    { name: 'packs/one.json', text: JSON.stringify(wordsPack('zipa', 2)) },
    { name: '__MACOSX/packs/._one.json', text: 'junk' },
    { name: 'packs/answer.md', text: `Here you are:\n\`\`\`json\n${JSON.stringify(wordsPack('zipb', 1))}\n\`\`\`\n` },
  ]);
  await page.goto(`${app.url}/#/import`);
  await page.getByTestId('file-input').setInputFiles({ name: 'packs.zip', mimeType: 'application/zip', buffer: zip });
  await expect(page.getByTestId('files')).toContainText('2 files chosen');
  await expect(page.getByTestId('files')).toContainText('packs.zip › packs/one.json');
  await page.getByTestId('preview').click();
  await expect(page.getByRole('heading', { name: 'Preview — nothing imported yet' })).toBeVisible();
  await page.getByTestId('import').click();
  await expect(page.getByTestId('import-done')).toContainText('3 words imported.');
  await expect(page.locator('#paste')).toHaveValue('');
  await expect(page.getByTestId('files')).toBeHidden();
  await page.getByTestId('start-practising').click();
  await expect(page).toHaveURL(/#\/$/);
  await expect(page.getByLabel(/Source zipa/)).toBeVisible();
});

test('F3 import from two JSON files', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await page.goto(`${app.url}/#/import`);
  await page.getByTestId('file-input').setInputFiles([
    { name: 'a.json', mimeType: 'application/json', buffer: Buffer.from(JSON.stringify(wordsPack('jsa', 3))) },
    { name: 'b.json', mimeType: 'application/json', buffer: Buffer.from(JSON.stringify(wordsPack('jsb', 4))) },
  ]);
  await expect(page.getByTestId('files')).toContainText('2 files chosen');
  await page.getByTestId('preview').click();
  await expect(page.locator('section.pack')).toHaveCount(2);
  // A long preview repeats Import at its end.
  await page.getByTestId('import-bottom').click();
  await expect(page.getByTestId('import-done')).toContainText('7 words imported.');
  const words = await app.api('GET', '/api/words?q=test-js');
  expect(words.length).toBe(7);
});

test('U2 import problems name the headword instead of a JSON pointer', async ({ page, app, pageErrors }) => {
  void pageErrors;
  const pack = wordsPack('bad', 2);
  /** @type {any} */ (pack.words[1]).sentences[0].cs = 'Bez označení.';
  await page.goto(`${app.url}/#/import`);
  await page.locator('#paste').fill(JSON.stringify(pack));
  await page.getByTestId('preview').click();
  await expect(page.getByRole('heading', { name: 'Preview — nothing imported yet' })).toBeVisible();
  await expect(page.locator('.report')).not.toContainText('/words/');
});

// ── F4: log out everywhere ───────────────────────────────────────────────────

test('F4 "Log out everywhere" ends every session and shows the login screen', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await page.goto(`${app.url}/#/settings`);
  await page.getByTestId('logout-all').click();
  await expect(page.getByRole('button', { name: 'Log in' })).toBeVisible();
  await expect(page.getByLabel('Password', { exact: true })).toBeVisible();
  await expect(app.api('GET', '/api/today')).rejects.toThrow(/401/);
});

// ── bug 5, 7, 11 ─────────────────────────────────────────────────────────────

test('bug 5 Back from a word returns to the same place and search in the Words list', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await app.api('POST', '/api/import/commit', { text: JSON.stringify(wordsPack('many', 40)) });
  await page.setViewportSize({ width: 375, height: 600 });
  await page.goto(`${app.url}/#/words`);
  await page.getByLabel('Search words').fill('test-many');
  await expect(page.getByText('40 words')).toBeVisible();
  const row = page.locator('a.word-row').nth(30);
  await row.scrollIntoViewIfNeeded();
  const before = await page.evaluate(() => window.scrollY);
  expect(before).toBeGreaterThan(200);
  const box = await row.boundingBox();
  expect(box && box.height >= 44, 'the whole row is a 44 px link').toBe(true);
  await row.click();
  await expect(page).toHaveURL(/#\/words\/\d+$/);
  await page.goBack();
  await expect(page.getByLabel('Search words')).toHaveValue('test-many');
  await expect.poll(() => page.evaluate(() => window.scrollY)).toBeGreaterThan(before - 5);
  expect(await page.evaluate(() => window.scrollY)).toBeLessThan(before + 5);
});

test('bug 7 changing a radio with the arrow keys keeps the focus', async ({ page, app, pageErrors }, info) => {
  test.skip(info.project.name !== 'chromium-desktop', 'keyboard runs on the desktop');
  void pageErrors;
  await page.goto(`${app.url}/#/`);
  await expect(page.getByTestId('start')).toHaveText('Start · 4 cards (4 new)');
  await page.getByLabel('All sources').focus();
  await page.keyboard.press('ArrowDown');
  // Wait for the redraw of the newly chosen scope (its block differs).
  await expect(page.getByTestId('start')).not.toHaveText('Start · 4 cards (4 new)');
  expect(await page.evaluate(() => /** @type {HTMLInputElement} */ (document.activeElement).name)).toBe('scope');
  await page.goto(`${app.url}/#/settings`);
  await page.getByRole('radio', { name: 'Dark' }).focus();
  await page.keyboard.press('ArrowDown');
  await expect(page.getByTestId('settings-status')).toHaveText('Saved.');
  expect(await page.evaluate(() => /** @type {HTMLInputElement} */ (document.activeElement).name)).toBe('theme');
});

test('bug 11 F2 a number out of range says what is allowed; new words per day goes up to 100', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await page.goto(`${app.url}/#/settings`);
  const field = page.getByLabel(/New words per day/);
  await expect(page.getByText('New words per day (0–100)')).toBeVisible();
  await expect(page.getByText('Many new words today mean many reviews over the next days.', { exact: false })).toBeVisible();
  await field.fill('101');
  await field.blur();
  await expect(page.getByTestId('new-per-day-problem')).toHaveText('Enter a whole number from 0 to 100. Not saved.');
  await expect(field).toHaveValue('101');
  await field.fill('60');
  await field.blur();
  await expect(page.getByTestId('settings-status')).toHaveText('Saved.');
  const settings = await app.api('GET', '/api/settings');
  expect(settings.new_per_day).toBe(60);
});

test('bug 10 the shell has an icon, a manifest and theme colours', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await page.goto(`${app.url}/#/`);
  const icon = await page.locator('link[rel="apple-touch-icon"]').getAttribute('href');
  const manifest = await page.locator('link[rel="manifest"]').getAttribute('href');
  expect(icon && manifest).toBeTruthy();
  const png = await page.request.get(`${app.url}${icon}`);
  expect(png.headers()['content-type']).toBe('image/png');
  const json = await (await page.request.get(`${app.url}${manifest}`)).json();
  expect(json.icons[0].sizes).toBe('180x180');
  await expect(page.locator('meta[name="theme-color"]')).toHaveCount(2);
  expect(await page.locator('meta[name="viewport"]').getAttribute('content')).toContain('viewport-fit=cover');
});

test('R-74 a cut-off Today answer shows "Try again", never a page error; Try again loads it', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await page.route('**/api/today*', (route) => route.fulfill({ status: 200, contentType: 'application/json', body: '' }));
  await page.goto(`${app.url}/#/`);
  await expect(page.getByTestId('try-again')).toBeVisible();
  await page.unroute('**/api/today*');
  await page.getByTestId('try-again').click();
  await expect(page.getByTestId('start')).toBeVisible();
});

test('R-74 leaving Today while its answer is cut off does not touch the next screen', async ({ page, app, pageErrors }) => {
  void pageErrors;
  /** @type {(() => void) | null} */
  let release = null;
  await page.route('**/api/today*', async (route) => {
    await new Promise((resolve) => {
      release = () => resolve(undefined);
    });
    await route.fulfill({ status: 200, contentType: 'application/json', body: '{"study_day":' });
  });
  await page.goto(`${app.url}/#/`);
  await expect.poll(() => release !== null).toBe(true);
  await page.getByRole('link', { name: 'Words' }).click();
  await expect(page.getByRole('heading', { name: 'Words' })).toBeVisible();
  /** @type {() => void} */ (/** @type {unknown} */ (release))();
  await page.waitForTimeout(300);
  await expect(page.getByRole('heading', { name: 'Words' })).toBeVisible();
  await expect(page.getByTestId('try-again')).toHaveCount(0);
});
