// @ts-check
/**
 * Recorded audio (M6; R-40, R-41, R-42): with a TTS service the ▶ buttons
 * play the server's clips through a media element (not speech); without a
 * ready clip they fall back to the browser's speech; Settings shows the
 * state of the clips.
 */

import { startFakeTts } from './fake-tts.js';
import { expect, startBookBlock, test } from './fixtures.js';

/**
 * Wait until the server's queue is empty and `ready` clips exist.
 *
 * @param {import('./fixtures.js').App} app
 */
async function waitForClips(app) {
  await expect
    .poll(async () => {
      const status = await app.api('GET', '/api/audio/status');
      return status.enabled && status.connected && status.pending === 0 ? status.ready : 0;
    }, { timeout: 30_000 })
    .toBeGreaterThan(10);
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

  test('R-40 a ready clip is played from the server, not by speech synthesis; ranges and preload work', async ({
    page,
    app,
    pageErrors,
  }) => {
    void pageErrors;
    await waitForClips(app);
    /** @type {{ url: string, status: number }[]} */
    const clipResponses = [];
    page.on('response', (response) => {
      if (response.url().includes('/api/audio/clips/')) clipResponses.push({ url: response.url(), status: response.status() });
    });
    const known = page.waitForResponse((r) => r.url().includes('/api/audio/words/'));
    await startBookBlock(page, app);
    // The clips of the card on screen are known.
    await known;
    await page.waitForTimeout(300);
    await page.getByTestId('reveal').click();
    await expect(page.getByTestId('headword')).toBeVisible();
    const before = clipResponses.length;
    await page.locator('button.play-word').click();
    await expect.poll(() => clipResponses.length).toBeGreaterThan(before);
    expect(clipResponses.every((r) => r.status === 200 || r.status === 206)).toBe(true);
    // Played by the media element: the browser's speech was not used.
    await expect(page.locator('button.play-word')).not.toHaveClass(/failed/);
    const spoken = await page.evaluate(() => /** @type {any} */ (window).spoken.length);
    expect(spoken).toBe(0);

    // The clip serves byte ranges (iOS Safari needs them).
    const url = new URL(clipResponses[clipResponses.length - 1].url);
    const ranged = await page.evaluate(async (path) => {
      const r = await fetch(path, { headers: { Range: 'bytes=0-3' } });
      return { status: r.status, type: r.headers.get('content-type'), length: (await r.arrayBuffer()).byteLength };
    }, url.pathname);
    expect(ranged).toEqual({ status: 206, type: 'audio/mpeg', length: 4 });

    // Settings shows the clips.
    await page.goto(`${app.url}/#/settings`);
    await expect(page.getByTestId('audio-status')).toContainText('clips ready');
    expect(tts.speaks()).toBeGreaterThan(10);
  });
});

/**
 * Media that "plays" until paused (no real playback, so a clip never ends
 * on its own during the test), and speech that is "speaking" until
 * cancelled; both record when they are stopped.
 */
function installEndlessSound() {
  /** @type {string[]} */
  const events = [];
  /** @type {WeakSet<HTMLMediaElement>} */
  const sounding = new WeakSet();
  HTMLMediaElement.prototype.play = function play() {
    // The muted play of the first tap only unlocks the element (iOS): no sound.
    if (this.muted) return Promise.resolve();
    sounding.add(this);
    events.push('clip');
    this.dispatchEvent(new Event('playing'));
    return Promise.resolve();
  };
  HTMLMediaElement.prototype.pause = function pause() {
    if (!sounding.delete(this)) return;
    events.push('clip stopped');
  };
  const synth = /** @type {any} */ (window).speechSynthesis;
  const record = synth.speak;
  let speaking = false;
  Object.defineProperty(synth, 'speaking', { get: () => speaking, configurable: true });
  synth.pending = false;
  synth.speak = (/** @type {any} */ u) => {
    speaking = true;
    events.push('speech');
    record(u);
  };
  synth.cancel = () => {
    if (speaking) events.push('speech stopped');
    speaking = false;
  };
  Object.defineProperty(window, 'soundEvents', { value: events, configurable: true });
}

test.describe('with a TTS service: never two sounds at once', () => {
  /** @type {Awaited<ReturnType<typeof startFakeTts>>} */
  let tts;
  test.beforeAll(async () => {
    tts = await startFakeTts();
  });
  test.afterAll(async () => {
    await tts.close();
  });
  test.use({ ttsUrl: async ({}, use) => use(tts.url) });

  test('R-41 a clip stops before the browser speaks, and speech stops before a clip plays', async ({
    page,
    app,
    pageErrors,
  }) => {
    void pageErrors;
    await waitForClips(app);
    await page.addInitScript(installEndlessSound);
    // The sentences have no clip yet: ▶ sentence falls back to speech.
    await page.route('**/api/audio/words/*', async (route) => {
      const response = await route.fetch();
      const info = await response.json();
      for (const s of info.sentences ?? []) Object.assign(s, { ready: false, url: null });
      await route.fulfill({ response, json: info });
    });
    const known = page.waitForResponse((r) => r.url().includes('/api/audio/words/'));
    await startBookBlock(page, app);
    await known;
    await page.waitForTimeout(300);
    await page.getByTestId('reveal').click();
    await expect(page.getByTestId('headword')).toBeVisible();
    const events = () => page.evaluate(() => /** @type {any} */ (window).soundEvents.slice());

    await page.locator('button.play-word').click();
    await expect.poll(events).toEqual(['clip']);
    await page.locator('button.play-sentence').click();
    await expect.poll(events).toEqual(['clip', 'clip stopped', 'speech']);
    await page.locator('button.play-word').click();
    await expect.poll(events).toEqual(['clip', 'clip stopped', 'speech', 'speech stopped', 'clip']);
  });
});

test.describe('with a TTS service that is down', () => {
  // A port nothing listens on: the clips never become ready.
  test.use({ ttsUrl: 'http://127.0.0.1:9' });

  test('R-41 no ready clip: ▶ falls back to the browser speech; Settings says so', async ({ page, app, pageErrors }) => {
    void pageErrors;
    await startBookBlock(page, app);
    await page.getByTestId('reveal').click();
    await page.locator('button.play-word').click();
    await expect.poll(() => page.evaluate(() => /** @type {any} */ (window).spoken.map((/** @type {any} */ s) => s.text))).toEqual([
      'glimmerous',
    ]);
    await page.goto(`${app.url}/#/settings`);
    await expect(page.getByTestId('audio-status')).toContainText('has not answered yet');
  });
});

test('R-41 without a TTS service Settings says the browser speaks', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await page.goto(`${app.url}/#/settings`);
  await expect(page.getByTestId('audio-status')).toContainText('not set up on this server');
});
