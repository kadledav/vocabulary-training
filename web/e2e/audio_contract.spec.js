// @ts-check
/**
 * Contract tests of the audio in the UI, from the requirements (R-40
 * recorded server audio, R-41 browser speech only as a tap-triggered
 * fallback, usable on a phone, R-42 voice and speed settings, R-83 hardening)
 * and design §7.4. Observed from outside: the media element is watched
 * through a wrapper of the media prototype's play method (its `src`, speed, whether the
 * call happened inside a user gesture), the browser's speech through the
 * fake `window.spoken`, and every test fails on a console error or a CSP
 * violation. Runs in both projects (WebKit phone, Chromium desktop).
 */

import { startFakeTts } from './fake-tts.js';
import { expect, headwordOnScreen, startBookBlock, test, waitGuard } from './fixtures.js';

/** What the page observed. */
const CLIP_PATH = /^\/api\/audio\/clips\/[0-9a-f]{32}\.mp3$/;

/**
 * Record every `play()` of a media element and every CSP violation.
 */
function installMediaSpy() {
  /** @type {{ src: string, rate: number, pitch: boolean, gesture: boolean | null }[]} */
  const plays = [];
  /** @type {string[]} */
  const violations = [];
  const original = HTMLMediaElement.prototype.play;
  HTMLMediaElement.prototype.play = function play() {
    const activation = /** @type {any} */ (navigator).userActivation;
    plays.push({
      src: this.src,
      rate: this.playbackRate,
      pitch: /** @type {any} */ (this).preservesPitch !== false,
      gesture: activation ? Boolean(activation.isActive) : null,
    });
    return original.call(this);
  };
  document.addEventListener('securitypolicyviolation', (e) => {
    violations.push(`${e.violatedDirective} ${e.blockedURI}`);
  });
  Object.defineProperty(window, 'plays', { value: plays, configurable: true });
  Object.defineProperty(window, 'violations', { value: violations, configurable: true });
}

/** @param {import('@playwright/test').Page} page */
const plays = (page) => page.evaluate(() => /** @type {any} */ (window).plays.slice());
/** @param {import('@playwright/test').Page} page */
const spoken = (page) => page.evaluate(() => /** @type {any} */ (window).spoken.map((/** @type {any} */ s) => s.text));
/** @param {import('@playwright/test').Page} page */
const violations = (page) => page.evaluate(() => /** @type {any} */ (window).violations.slice());

/** @param {import('./fixtures.js').App} app */
async function waitForClips(app) {
  await expect
    .poll(
      async () => {
        const status = await app.api('GET', '/api/audio/status');
        return status.enabled && status.connected && status.pending === 0 ? status.ready : 0;
      },
      { timeout: 30_000 },
    )
    .toBeGreaterThan(10);
}

/**
 * Start the book block and reveal the first card by a click on the reveal
 * control; waits until the page knows the clips of the card.
 *
 * @param {import('@playwright/test').Page} page
 * @param {import('./fixtures.js').App} app
 */
async function startAndReveal(page, app) {
  const known = page.waitForResponse((r) => r.url().includes('/api/audio/words/'));
  await startBookBlock(page, app);
  await known;
  await page.waitForTimeout(300);
  await page.getByTestId('reveal').click();
  await expect(page.getByTestId('headword')).toBeVisible();
}

test.beforeEach(async ({ page }) => {
  await page.addInitScript(installMediaSpy);
});

test.describe('R-40 with a TTS service whose clips are ready', () => {
  /** @type {Awaited<ReturnType<typeof startFakeTts>>} */
  let tts;
  test.beforeAll(async () => {
    tts = await startFakeTts();
  });
  test.afterAll(async () => {
    await tts.close();
  });
  test.use({ ttsUrl: async ({}, use) => use(tts.url) });

  test('R-40 ▶ word and ▶ sentence play the server clip through a media element, not browser speech', async ({
    page,
    app,
    pageErrors,
  }) => {
    void pageErrors;
    await waitForClips(app);
    /** @type {string[]} */
    const hosts = [];
    page.on('request', (r) => hosts.push(new URL(r.url()).origin));
    await startAndReveal(page, app);
    // Tap on a phone, click on the desktop: a real user gesture either way.
    await page.locator('button.play-word').click();
    await expect.poll(async () => (await plays(page)).length).toBe(1);
    const [word] = await plays(page);
    expect(new URL(word.src).pathname).toMatch(CLIP_PATH);
    expect(new URL(word.src).origin).toBe(new URL(app.url).origin);
    expect(word.gesture, 'play() runs inside the tap').not.toBe(false);
    await page.locator('button.play-sentence').click();
    await expect.poll(async () => (await plays(page)).length).toBe(2);
    const [, sentence] = await plays(page);
    expect(new URL(sentence.src).pathname).toMatch(CLIP_PATH);
    expect(sentence.src).not.toBe(word.src);
    // Browser speech was not used for either.
    expect(await spoken(page)).toEqual([]);
    // The clips were really fetched from the app (200 or 206), nothing from elsewhere.
    expect(new Set(hosts)).toEqual(new Set([new URL(app.url).origin]));
    expect(await violations(page)).toEqual([]);
  });

  test('R-41 the keyboard replay keys play the clip too', async ({ page, app, pageErrors }) => {
    void pageErrors;
    await waitForClips(app);
    await startAndReveal(page, app);
    await page.keyboard.press('r');
    await expect.poll(async () => (await plays(page)).length).toBeGreaterThanOrEqual(1);
    expect(new URL((await plays(page))[0].src).pathname).toMatch(CLIP_PATH);
    expect(await spoken(page)).toEqual([]);
  });

  test('R-42 the speed setting applies to the clip (pitch preserved); 1× is the default', async ({
    page,
    app,
    pageErrors,
  }) => {
    void pageErrors;
    await waitForClips(app);
    const settings = await app.api('GET', '/api/settings');
    expect(settings.speed).toBe('1');
    await startAndReveal(page, app);
    await page.locator('button.play-word').click();
    await expect.poll(async () => (await plays(page)).length).toBe(1);
    expect((await plays(page))[0].rate).toBe(1);

    await app.api('PATCH', '/api/settings', { speed: '0.75' });
    await page.reload();
    await expect(page.getByTestId('progress')).toBeVisible();
    await page.waitForTimeout(500);
    const headword = await headwordOnScreen(page);
    expect(headword).not.toBe('');
    if (!(await page.getByTestId('headword').count())) {
      await page.getByTestId('reveal').click();
      await expect(page.getByTestId('headword')).toBeVisible();
    }
    await page.locator('button.play-word').click();
    await expect.poll(async () => (await plays(page)).length).toBe(1);
    const [slow] = await plays(page);
    expect(slow.rate).toBe(0.75);
    expect(slow.pitch, 'preservesPitch').toBe(true);
    expect(await spoken(page)).toEqual([]);
  });

  test('R-42 auto-play is off by default: revealing an answer plays nothing', async ({ page, app, pageErrors }) => {
    void pageErrors;
    await waitForClips(app);
    expect((await app.api('GET', '/api/settings')).autoplay).toBe(false);
    await startAndReveal(page, app);
    await waitGuard();
    await page.waitForTimeout(500);
    expect(await plays(page)).toEqual([]);
    expect(await spoken(page)).toEqual([]);
  });

  test('R-41 auto-play on: nothing plays on the question side or on load, the word plays once on reveal by a gesture', async ({
    page,
    app,
    pageErrors,
  }) => {
    void pageErrors;
    await waitForClips(app);
    await app.api('PATCH', '/api/settings', { autoplay: true });
    const known = page.waitForResponse((r) => r.url().includes('/api/audio/words/'));
    await startBookBlock(page, app);
    await known;
    await page.waitForTimeout(800);
    // Question side, no gesture since the start button: nothing played.
    expect(await plays(page)).toEqual([]);
    expect(await spoken(page)).toEqual([]);
    await page.getByTestId('reveal').click();
    await expect(page.getByTestId('headword')).toBeVisible();
    await expect.poll(async () => (await plays(page)).length).toBe(1);
    const [auto] = await plays(page);
    expect(new URL(auto.src).pathname).toMatch(CLIP_PATH);
    expect(auto.gesture, 'auto-play happens inside the reveal gesture').not.toBe(false);
    await page.waitForTimeout(500);
    expect((await plays(page)).length, 'once, not repeatedly').toBe(1);
    expect(await spoken(page)).toEqual([]);
    // A reload in the middle of the block plays nothing by itself.
    await page.reload();
    await expect(page.getByTestId('progress')).toBeVisible();
    await page.waitForTimeout(800);
    expect(await plays(page)).toEqual([]);
    expect(await spoken(page)).toEqual([]);
  });

  test('R-40 Settings offers "Retry failed clips" only when clips failed', async ({ page, app, pageErrors }) => {
    void pageErrors;
    await waitForClips(app);
    await page.goto(`${app.url}/#/settings`);
    await expect(page.getByTestId('audio-status')).toContainText('clips ready');
    expect((await app.api('GET', '/api/audio/status')).failed).toBe(0);
    await expect(page.getByTestId('audio-retry')).toBeHidden();
  });

  test('R-42 Settings shows the audio status, voice, speed and auto-play', async ({ page, app, pageErrors }) => {
    void pageErrors;
    await waitForClips(app);
    const status = await app.api('GET', '/api/audio/status');
    await page.goto(`${app.url}/#/settings`);
    await expect(page.getByTestId('audio-status')).toContainText(`${status.ready} clips ready`);
    await expect(page.locator('input[name="voice"][value="us"]')).toBeChecked();
    await expect(page.locator('input[name="speed"][value="1"]')).toBeChecked();
    await expect(page.getByLabel(/Play the word when the answer is shown/)).not.toBeChecked();
    // Choosing UK saves, keeps the status line, and queues the other voice.
    await page.locator('input[name="voice"][value="uk"]').check();
    await expect.poll(async () => (await app.api('GET', '/api/settings')).voice).toBe('uk');
    await expect(page.getByTestId('audio-status')).toContainText('clips ready');
    await expect
      .poll(async () => {
        const s = await app.api('GET', '/api/audio/status');
        return s.pending === 0 ? s.ready : -1;
      }, { timeout: 30_000 })
      .toBe(status.ready);
    expect(tts.speaks()).toBeGreaterThanOrEqual(status.ready * 2);
    expect(await violations(page)).toEqual([]);
  });
});

test.describe('R-41 with a TTS service that is down', () => {
  test.use({ ttsUrl: 'http://127.0.0.1:9' });

  test('R-41 not ready: the tap speaks with the browser voice, and only the tap does', async ({
    page,
    app,
    pageErrors,
  }) => {
    void pageErrors;
    await startBookBlock(page, app);
    await page.waitForTimeout(500);
    expect(await spoken(page), 'nothing speaks by itself').toEqual([]);
    await page.getByTestId('reveal').click();
    await expect(page.getByTestId('headword')).toBeVisible();
    await page.waitForTimeout(500);
    expect(await spoken(page)).toEqual([]);
    await page.locator('button.play-word').click();
    await expect.poll(() => spoken(page)).toEqual(['glimmerous']);
    // No media element was started for a clip that does not exist.
    expect(await plays(page)).toEqual([]);
    expect(await violations(page)).toEqual([]);
  });

  test('R-41 auto-play never falls back to speech, and speed/voice reach the speech', async ({
    page,
    app,
    pageErrors,
  }) => {
    void pageErrors;
    await app.api('PATCH', '/api/settings', { autoplay: true, speed: '0.75', voice: 'uk' });
    await startBookBlock(page, app);
    await page.getByTestId('reveal').click();
    await expect(page.getByTestId('headword')).toBeVisible();
    await page.waitForTimeout(800);
    expect(await spoken(page), 'auto-play is for recorded clips only').toEqual([]);
    await page.locator('button.play-word').click();
    await expect
      .poll(() => page.evaluate(() => /** @type {any} */ (window).spoken))
      .toEqual([{ text: 'glimmerous', lang: 'en-GB', rate: 0.75 }]);
    expect(await plays(page)).toEqual([]);
  });

  test('R-41 Settings says the service has not answered and ▶ uses the browser speech', async ({
    page,
    app,
    pageErrors,
  }) => {
    void pageErrors;
    await page.goto(`${app.url}/#/settings`);
    await expect(page.getByTestId('audio-status')).toContainText('browser');
  });
});

test.describe('R-41 without a TTS service', () => {
  test('R-41 ▶ speaks with the browser voice and Settings says audio is not set up', async ({
    page,
    app,
    pageErrors,
  }) => {
    void pageErrors;
    await startBookBlock(page, app);
    await page.getByTestId('reveal').click();
    await page.locator('button.play-word').click();
    await expect.poll(() => spoken(page)).toEqual(['glimmerous']);
    expect(await plays(page)).toEqual([]);
    const status = await app.api('GET', '/api/audio/status');
    expect(status).toEqual({ enabled: false });
    await page.goto(`${app.url}/#/settings`);
    await expect(page.getByTestId('audio-status')).toContainText('not set up');
    expect(await violations(page)).toEqual([]);
  });
});

test.describe('R-41 a clip that fails to load', () => {
  /** @type {Awaited<ReturnType<typeof startFakeTts>>} */
  let tts;
  test.beforeAll(async () => {
    tts = await startFakeTts();
  });
  test.afterAll(async () => {
    await tts.close();
  });
  test.use({ ttsUrl: async ({}, use) => use(tts.url) });

  test('R-41 a clip request that fails falls back to the browser speech on the same tap', async ({
    page,
    app,
    pageErrors,
  }) => {
    await waitForClips(app);
    await page.route('**/api/audio/clips/**', (route) => route.fulfill({ status: 404, body: 'gone' }));
    await startAndReveal(page, app);
    await page.locator('button.play-word').click();
    await expect.poll(() => spoken(page)).toEqual(['glimmerous']);
    // The failed request is a deliberate 404 of this test; nothing else is wrong.
    const allowed = /Failed to load resource|404|media/i;
    expect(pageErrors.filter((e) => !allowed.test(e))).toEqual([]);
    pageErrors.length = 0;
  });
});
