// @ts-check
/**
 * Login screen, the way back after a 401, and logout (R-82). The browser
 * talks to the app over HTTPS (e2e/server.js), so the Secure `__Host-`
 * session cookie behaves as in production.
 */

import { PASSWORD, SESSION_COOKIE, expect, test } from './fixtures.js';

/**
 * Page errors and console errors, except the browsers' own "failed to load
 * resource: 401" lines, which these tests cause on purpose.
 *
 * @param {import('@playwright/test').Page} page
 */
function collectErrors(page) {
  /** @type {string[]} */
  const errors = [];
  page.on('pageerror', (err) => errors.push(`pageerror: ${err.message}`));
  page.on('console', (msg) => {
    if (msg.type() === 'error' && !/\b401\b|Unauthorized/i.test(msg.text())) errors.push(`console: ${msg.text()}`);
  });
  return errors;
}

/** @param {import('@playwright/test').Page} page */
async function expectLoginScreen(page) {
  await expect(page).toHaveURL(/#\/login$/);
  await expect(page.getByLabel('Password')).toBeVisible();
  await expect(page.getByRole('navigation', { name: 'Main' })).toHaveCount(0);
}

test('R-82 without a session the app shows the login screen and returns to the screen afterwards', async ({
  page,
  app,
  context,
}) => {
  const errors = collectErrors(page);
  await context.clearCookies();
  await page.goto(`${app.url}/#/words`);
  await expectLoginScreen(page);

  // Wrong password: a clear message, still on the login screen.
  await page.getByLabel('Password').fill('test-password-wrong');
  await page.getByRole('button', { name: 'Log in' }).click();
  await expect(page.getByTestId('login-status')).toHaveText('Wrong password. Try again.');
  await expect(page).toHaveURL(/#\/login$/);

  // The right password (Enter submits) goes back to Words.
  await page.getByLabel('Password').fill(PASSWORD);
  await page.getByLabel('Password').press('Enter');
  await expect(page).toHaveURL(/#\/words$/);
  await expect(page.getByRole('navigation', { name: 'Main' })).toBeVisible();
  await expect(page.getByRole('link', { name: 'Words' })).toHaveAttribute('aria-current', 'page');

  // The browser now holds a Secure, HttpOnly session cookie.
  const cookies = await context.cookies();
  const session = cookies.find((c) => c.name === SESSION_COOKIE);
  expect(session?.secure).toBe(true);
  expect(session?.httpOnly).toBe(true);
  expect(session?.sameSite).toBe('Strict');
  expect(await page.evaluate(() => document.cookie)).toBe('');
  expect(errors).toEqual([]);
});

test('R-82 a session that ends while the app is open leads to the login screen and back', async ({
  page,
  app,
}) => {
  const errors = collectErrors(page);
  await page.goto(`${app.url}/#/`);
  await expect(page.getByTestId('start')).toBeVisible();

  // The session ends on the server (logged out from elsewhere).
  await app.api('POST', '/api/auth/logout');

  await page.getByRole('link', { name: 'Import' }).click();
  await expectLoginScreen(page);
  await page.getByLabel('Password').fill(PASSWORD);
  await page.getByRole('button', { name: 'Log in' }).click();
  await expect(page).toHaveURL(/#\/import$/);
  await expect(page.getByRole('heading', { name: 'Import', exact: true })).toBeVisible();
  expect(errors).toEqual([]);
});

test('R-82 Log out in Settings ends the session on this device', async ({ page, app }) => {
  const errors = collectErrors(page);
  await page.goto(`${app.url}/#/settings`);
  await page.getByTestId('logout').click();
  await expectLoginScreen(page);

  // Reloading does not bring the session back...
  await page.goto(`${app.url}/#/`);
  await expectLoginScreen(page);
  // ...and the old cookie no longer opens the API.
  await expect(app.api('GET', '/api/today')).rejects.toThrow(/401/);
  expect(errors).toEqual([]);
});

test('R-82 R-70 the login screen is phone-sized with large targets', async ({ page, app, context }, info) => {
  test.skip(info.project.name !== 'webkit-phone', 'phone layout');
  const errors = collectErrors(page);
  await context.clearCookies();
  await page.goto(`${app.url}/#/`);
  await expectLoginScreen(page);
  await expect(page.getByLabel('Password')).toBeFocused();
  for (const target of [page.getByLabel('Password'), page.getByRole('button', { name: 'Log in' })]) {
    const box = await target.boundingBox();
    expect(box?.height ?? 0).toBeGreaterThanOrEqual(44);
    expect((box?.x ?? 0) + (box?.width ?? 0)).toBeLessThanOrEqual(375);
  }
  await expect(page.getByLabel('Password')).toHaveAttribute('type', 'password');
  await expect(page.getByLabel('Password')).toHaveAttribute('autocomplete', 'current-password');
  expect(errors).toEqual([]);
});
