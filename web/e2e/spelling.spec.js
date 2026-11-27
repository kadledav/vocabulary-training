// @ts-check
/**
 * Practice UI: Spelling (design §17 M10 "done when"; R-58, R-41, R-70;
 * design §6.8, §7.1 screen 3b, §7.4): the word's sound as the question —
 * playing by itself where the browser allows it, ▶ Listen asking for a tap
 * where it does not, the Czech cue when this device has no sound — with
 * nothing on the page that shows the spelling; the answer field without
 * autocorrect, Check / I don't know / I'll just rate myself (no hint); the
 * strict check with the letter diff and the spelling note; the typed text
 * and the check kept through a reload; the bar above the on-screen
 * keyboard. Product time windows run on the page clock.
 */

import { DatabaseSync } from 'node:sqlite';

import { startFakeTts } from './fake-tts.js';
import { advanceClock, expect, freezeClock, installClock, test, waitingSpelling } from './fixtures.js';

const NOTE = 'test-note: the double letters hide in the middle.';

/**
 * Reviews stored so far, with their answer details.
 *
 * @param {import('./fixtures.js').App} app
 */
function storedReviews(app) {
  const db = new DatabaseSync(app.dbPath);
  const rows = db
    .prepare(
      `SELECT w.headword, r.raw_grade, r.suggested_grade, r.typed_answer, r.answer_match, r.hint_used, r.format, r.sentence_id
       FROM review_log r JOIN card c ON c.id = r.card_id JOIN word w ON w.id = c.word_id
       WHERE r.undone = 0 ORDER BY r.id`,
    )
    .all();
  db.close();
  return rows.map((r) => [r.headword, r.raw_grade, r.suggested_grade, r.typed_answer, r.answer_match, r.hint_used, r.format, r.sentence_id]);
}

/**
 * Start the block from Today (all sources), on the page clock.
 *
 * @param {import('@playwright/test').Page} page
 * @param {import('./fixtures.js').App} app
 * @param {{ touch?: boolean }} [options]
 */
async function startSpellingBlock(page, app, { touch = false } = {}) {
  await installClock(page);
  await page.goto(`${app.url}/#/`);
  await expect(page.getByTestId('typed-waiting')).toBeVisible();
  const start = page.getByTestId('start');
  if (touch) await start.tap();
  else await start.click();
  await expect(page).toHaveURL(/#\/practice$/);
  await expect(page.getByTestId('mode')).toHaveText('Spelling');
  await freezeClock(page);
  await advanceClock(page, 300); // past the 250 ms input guard of the first card
}

/** @param {import('@playwright/test').Page} page */
function input(page) {
  return page.getByTestId('answer-input');
}

/** @param {import('@playwright/test').Page} page */
function spoken(page) {
  return page.evaluate(() => /** @type {any} */ (window).spoken.map((/** @type {any} */ s) => s.text));
}

/**
 * The question side of a Spelling card: ▶ Listen, the Czech cue, the
 * answer field — and the spelling nowhere on the page, not in text, not in
 * an attribute.
 *
 * @param {import('@playwright/test').Page} page
 * @param {string} headword
 */
async function expectQuestion(page, headword) {
  await expect(page.getByTestId('mode')).toHaveText('Spelling');
  await expect(page.getByTestId('listen')).toBeVisible();
  await expect(page.getByTestId('replay')).toBeVisible();
  await expect(page.getByTestId('gloss-cue')).toBeVisible();
  await expect(input(page)).toBeVisible();
  await expect(input(page)).toHaveAttribute('placeholder', 'Type the word you hear');
  await expect(page.getByTestId('hint-button')).toHaveCount(0);
  await expect(page.getByTestId('headword')).toHaveCount(0);
  await expect(page.getByTestId('sentence-en')).toHaveCount(0);
  await expect(page.getByTestId('cloze')).toHaveCount(0);
  const html = await page.locator('.practice-body, .practice-bottom, .practice-top').evaluateAll((els) => els.map((e) => e.outerHTML).join(''));
  expect(html.toLowerCase()).not.toContain(headword);
  expect((await page.title()).toLowerCase()).not.toContain(headword);
}

test('R-58 Spelling by touch on the phone: listen, a right spelling, a slip with the letter diff, I don’t know', async ({ page, app, pageErrors }, info) => {
  test.skip(info.project.name !== 'webkit-phone', 'touch flow runs on the phone viewport');
  void pageErrors;
  await waitingSpelling(app, ['glimmerous', 'brontle', 'quabble'], NOTE);
  await page.goto(`${app.url}/#/`);
  await expect(page.getByTestId('typed-waiting')).toHaveText('3 Produce/Spelling cards wait for the first time — all come today.');
  await startSpellingBlock(page, app, { touch: true });

  // 1 · glimmerous. No recorded clips on this server: the sound cannot
  // start by itself, so ▶ Listen asks for a tap; the tap speaks the word.
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
  await expect(page.getByTestId('listen-status')).toHaveText('Tap ▶ Listen to hear the word.');
  await expect(page.getByTestId('listen')).toHaveClass(/needs-tap/);
  expect(await spoken(page)).toEqual([]);
  await page.getByTestId('listen').tap();
  await expect.poll(() => spoken(page)).toEqual(['glimmerous']);
  await expect(page.getByTestId('listen-status')).toHaveText('Type the word you heard. Replay it as often as you like.');
  await expect(page.getByTestId('listen')).not.toHaveClass(/needs-tap/);
  // ▶ next to the field plays it again.
  await page.getByTestId('replay').tap();
  await expect.poll(() => spoken(page)).toEqual(['glimmerous', 'glimmerous']);
  await input(page).tap();
  await input(page).fill(' Glimmerous ');
  await page.getByTestId('check').tap();
  await expect(page.getByTestId('check-result')).toHaveAttribute('data-match', 'exact');
  await expect(page.getByTestId('check-result')).toContainText('Correct spelling');
  await expect(page.getByTestId('spelling-note')).toHaveText(`Trap: ${NOTE}`);
  await expect(page.getByTestId('headword')).toHaveText('glimmerous');
  await expect(page.getByTestId('suggestion')).toContainText('Suggested: 2 · Knew it');
  await advanceClock(page, 650);
  await page.getByRole('button', { name: /^Rate 2,/ }).tap();
  await expect(page.getByTestId('toast')).toContainText('Rated 2');

  // 2 · brontle: a swapped pair → ✗ Not quite with the letter diff,
  // symbols under the differences; 4 suggested; never offered as accepted.
  await expect(page.getByTestId('progress')).toHaveText('2 / 3');
  await expectQuestion(page, 'brontle');
  await advanceClock(page, 300);
  await input(page).fill('brontel');
  await page.getByTestId('check').tap();
  await expect(page.getByTestId('check-result')).toHaveAttribute('data-match', 'none');
  const columns = page.getByTestId('spell-diff').locator('[data-mark]');
  expect(await columns.evaluateAll((els) => els.map((e) => e.getAttribute('data-mark')))).toEqual([
    'same',
    'same',
    'same',
    'same',
    'same',
    'extra',
    'same',
    'missing',
  ]);
  await expect(columns.nth(5).locator('.diff-mark')).toHaveText('−');
  await expect(columns.nth(7).locator('.diff-mark')).toHaveText('+');
  await expect(page.getByTestId('check-result')).toContainText('extra “e” after letter 5; letter 7 missing: “e”');
  await expect(page.getByTestId('check-result')).toContainText('Just a typo? Choose 3.');
  await expect(page.getByTestId('spelling-note')).toBeVisible();
  await expect(page.getByTestId('accept-typed')).toHaveCount(0);
  await expect(page.getByTestId('suggestion')).toContainText('Suggested: 4 · Vague');
  await advanceClock(page, 650);
  await page.getByRole('button', { name: /^Rate 3,/ }).tap();
  await expect(page.getByTestId('toast')).toContainText('Rated 3');

  // 3 · quabble: "I don't know" → the spelling, 5 suggested; the answer
  // stays until Next and the card comes again at the end of the block.
  await expectQuestion(page, 'quabble');
  await advanceClock(page, 300);
  await page.getByTestId('dont-know').tap();
  await expect(page.getByTestId('check-result')).toHaveAttribute('data-match', 'dont_know');
  await expect(page.getByTestId('check-result')).toContainText('The spelling:');
  await expect(page.getByTestId('expected')).toHaveText('quabble');
  await expect(page.getByTestId('suggestion')).toContainText('Suggested: 5 · No idea');
  await advanceClock(page, 650);
  await page.getByRole('button', { name: /^Rate 5,/ }).tap();
  await expect(page.getByTestId('next')).toBeVisible();
  await advanceClock(page, 300);
  await page.getByTestId('next').tap();

  // The repeat: "I'll just rate myself" shows the spelling, no suggestion.
  await expect(page.getByTestId('progress')).toHaveText('4 / 4');
  await expectQuestion(page, 'quabble');
  await advanceClock(page, 300);
  await page.getByTestId('self-rate').tap();
  await expect(page.getByTestId('check-result')).toHaveAttribute('data-match', 'skipped');
  await expect(page.locator('button.rate[data-suggested]')).toHaveCount(0);
  await advanceClock(page, 650);
  await page.getByRole('button', { name: /^Rate 2,/ }).tap();
  await expect(page.getByRole('heading', { name: 'Block complete' })).toBeVisible();

  expect(storedReviews(app)).toEqual([
    ['glimmerous', 2, 2, 'Glimmerous', 'exact', 0, 'spelling', null],
    ['brontle', 3, 4, 'brontel', 'none', 0, 'spelling', null],
    ['quabble', 5, 5, null, 'dont_know', 0, 'spelling', null],
    ['quabble', 2, null, null, 'skipped', 0, 'spelling', null],
  ]);
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth)).toBe(true);
});

test('R-58 R-70 Spelling by keyboard: R replays, Enter checks and accepts; typing and the check survive a reload', async ({ page, app, pageErrors }, info) => {
  test.skip(info.project.name !== 'chromium-desktop', 'keyboard flow runs on the desktop');
  void pageErrors;
  await waitingSpelling(app, ['glimmerous', 'brontle'], NOTE);
  await startSpellingBlock(page, app);
  await expectQuestion(page, 'glimmerous');
  await expect(input(page)).toBeFocused();
  // In the field, R is a letter.
  await page.keyboard.type('r');
  await expect(input(page)).toHaveValue('r');
  expect(await spoken(page)).toEqual([]);
  // Outside it, R plays the word.
  await page.locator('.practice-body').focus();
  await page.keyboard.press('r');
  await expect.poll(() => spoken(page)).toEqual(['glimmerous']);
  await input(page).fill('glimerous');

  // A reload keeps what was typed …
  await page.reload();
  await freezeClock(page);
  await advanceClock(page, 300);
  await expectQuestion(page, 'glimmerous');
  await expect(input(page)).toHaveValue('glimerous');
  await input(page).focus();
  await page.keyboard.press('Enter');
  await expect(page.getByTestId('check-result')).toHaveAttribute('data-match', 'none');
  await expect(page.getByTestId('spell-diff')).toBeVisible();
  // … and, once checked, the checked answer side.
  await page.reload();
  await freezeClock(page);
  await expect(page.getByTestId('check-result')).toHaveAttribute('data-match', 'none');
  await expect(page.getByTestId('check-result')).toContainText('letter 4 missing: “m”');
  await advanceClock(page, 650);
  await page.keyboard.press('Enter');
  await expect(page.getByTestId('toast')).toContainText('Rated 4');
  await advanceClock(page, 300);
  await page.keyboard.press('Enter');

  await expectQuestion(page, 'brontle');
  // Past the input guard: the Enter that brought this card must not check it.
  await advanceClock(page, 300);
  await page.keyboard.type('brontle');
  await page.keyboard.press('Enter');
  await expect(page.getByTestId('check-result')).toHaveAttribute('data-match', 'exact');
  await advanceClock(page, 650);
  await page.keyboard.press('Enter');
  await expect(page.getByTestId('toast')).toContainText('Rated 2');

  expect(storedReviews(app).map((r) => r.slice(0, 5))).toEqual([
    ['glimmerous', 4, 4, 'glimerous', 'none'],
    ['brontle', 2, 2, 'brontle', 'exact'],
  ]);
});

test('R-58 R-70 the answer field, ▶ and the buttons stay above the on-screen keyboard, also in landscape', async ({ page, app, pageErrors }, info) => {
  test.skip(info.project.name !== 'webkit-phone', 'the on-screen keyboard is a phone matter');
  void pageErrors;
  await waitingSpelling(app, ['glimmerous']);
  await startSpellingBlock(page, app, { touch: true });
  await input(page).tap();
  /**
   * The keyboard leaves `height` px of the screen.
   *
   * @param {number} height
   */
  const keyboard = (height) =>
    page.evaluate((h) => {
      const vv = /** @type {VisualViewport} */ (window.visualViewport);
      Object.defineProperty(vv, 'height', { configurable: true, get: () => h });
      Object.defineProperty(vv, 'offsetTop', { configurable: true, get: () => 0 });
      vv.dispatchEvent(new Event('resize'));
    }, height);
  /**
   * @param {string[]} ids
   * @param {number} bottom
   * @param {number} width
   */
  const allAbove = async (ids, bottom, width) => {
    for (const id of ids) {
      const box = await page.getByTestId(id).boundingBox();
      expect(box, id).not.toBeNull();
      const b = /** @type {{ x: number, y: number, width: number, height: number }} */ (box);
      expect(b.y, `${id} below the top`).toBeGreaterThanOrEqual(0);
      expect(b.y + b.height, `${id} above the keyboard`).toBeLessThanOrEqual(bottom);
      expect(b.x + b.width, `${id} inside the screen width`).toBeLessThanOrEqual(width);
    }
  };
  await keyboard(450);
  await allAbove(['replay', 'answer-input', 'check', 'dont-know', 'self-rate'], 450, 375);
  // Landscape with the keyboard open: the compact bar keeps ▶, the field and Check.
  await page.setViewportSize({ width: 812, height: 375 });
  await keyboard(200);
  await expect(page.locator('.practice')).toHaveClass(/keyboard-tight/);
  await allAbove(['replay', 'answer-input', 'check'], 200, 812);
});

test('R-58 R-41 without any sound on this device the Czech cue is the question; no dead end', async ({ page, app, pageErrors }) => {
  void pageErrors;
  // No speech synthesis at all, and no recorded clips on this server.
  await page.addInitScript(() => {
    delete (/** @type {any} */ (window)).speechSynthesis;
    delete (/** @type {any} */ (window)).SpeechSynthesisUtterance;
  });
  await waitingSpelling(app, ['glimmerous']);
  await startSpellingBlock(page, app);
  await expectQuestion(page, 'glimmerous');
  await expect(page.getByTestId('listen-status')).toHaveText('No sound for this word on this device: spell it from the Czech below.');
  await expect(page.getByTestId('gloss-cue')).toContainText('třpytivý');
  await input(page).fill('glimmerous');
  await page.getByTestId('check').click();
  await expect(page.getByTestId('check-result')).toHaveAttribute('data-match', 'exact');
  await advanceClock(page, 650);
  await page.getByRole('button', { name: /^Rate 2,/ }).click();
  await expect(page.getByRole('heading', { name: 'Block complete' })).toBeVisible();
});

/**
 * Media that "plays" and ends at once, recording the clip addresses; while
 * `window.blockAutoplay` is set, a play not started by a tap on ▶ is
 * refused as the browser's autoplay rule refuses it.
 */
function installAutoplayRule() {
  /** @type {string[]} */
  const played = [];
  let blocked = false;
  let tapped = false;
  Object.defineProperty(window, 'blockAutoplay', { set: (v) => (blocked = v), get: () => blocked, configurable: true });
  Object.defineProperty(window, 'played', { value: played, configurable: true });
  document.addEventListener(
    'click',
    (event) => {
      const target = /** @type {Element | null} */ (event.target);
      if (target?.closest?.('[data-testid="listen"], [data-testid="replay"]')) tapped = true;
    },
    true,
  );
  HTMLMediaElement.prototype.play = function play() {
    if (this.muted) return Promise.resolve();
    if (blocked && !tapped) return Promise.reject(new DOMException('autoplay refused', 'NotAllowedError'));
    tapped = false;
    played.push(this.src);
    this.dispatchEvent(new Event('playing'));
    setTimeout(() => this.dispatchEvent(new Event('ended')), 20);
    return Promise.resolve();
  };
  HTMLMediaElement.prototype.pause = function pause() {};
}

test.describe('with a TTS service', () => {
  /** @type {Awaited<ReturnType<typeof startFakeTts>>} */
  let tts;
  test.beforeAll(async () => {
    tts = await startFakeTts();
  });
  test.afterAll(async () => {
    await tts.close();
  });
  test.use({ ttsUrl: async ({}, use) => use(tts.url) });

  test('R-58 R-40 the word’s clip plays by itself; where autoplay is refused ▶ Listen asks for a tap', async ({ page, app, pageErrors }) => {
    void pageErrors;
    await expect
      .poll(async () => {
        const status = await app.api('GET', '/api/audio/status');
        return status.enabled && status.connected && status.pending === 0 ? status.ready : 0;
      }, { timeout: 30_000 })
      .toBeGreaterThan(10);
    await page.addInitScript(installAutoplayRule);
    await waitingSpelling(app, ['glimmerous', 'brontle']);
    const played = () => page.evaluate(() => /** @type {any} */ (window).played.slice());

    // The first card plays its recorded clip by itself (not speech); its
    // address names a hash, never the word.
    await startSpellingBlock(page, app);
    await expect.poll(played).toHaveLength(1);
    const [clip] = await played();
    expect(new URL(clip).pathname).toMatch(/^\/api\/audio\/clips\/[0-9a-f]+\.mp3$/);
    expect(clip).not.toContain('glimmerous');
    expect(await spoken(page)).toEqual([]);
    await expect(page.getByTestId('listen-status')).toHaveText('Type the word you heard. Replay it as often as you like.');
    await expectQuestion(page, 'glimmerous');

    // The browser refuses to start the next card's sound by itself: ▶
    // Listen is marked and says so; a tap plays it.
    await page.evaluate(() => {
      /** @type {any} */ (window).blockAutoplay = true;
    });
    await input(page).fill('glimmerous');
    await page.getByTestId('check').click();
    await advanceClock(page, 650);
    await page.getByRole('button', { name: /^Rate 2,/ }).click();
    await expectQuestion(page, 'brontle');
    await expect(page.getByTestId('listen-status')).toHaveText('Tap ▶ Listen to hear the word.');
    await expect(page.getByTestId('listen')).toHaveClass(/needs-tap/);
    expect(await played()).toHaveLength(1);
    await page.getByTestId('listen').click();
    await expect.poll(played).toHaveLength(2);
    await expect(page.getByTestId('listen-status')).toHaveText('Type the word you heard. Replay it as often as you like.');
    expect(await spoken(page)).toEqual([]);
  });
});
