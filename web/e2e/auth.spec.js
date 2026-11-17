// @ts-check
/**
 * First-run setup, login screen, the way back after a 401, logout,
 * password change and the connector token (R-82). The browser talks to the
 * app over HTTPS (e2e/server.js), so the Secure `__Host-` session cookie
 * behaves as in production.
 */

import {
  PASSWORD,
  SESSION_COOKIE,
  apiFor,
  expect,
  importPacks,
  loginByApi,
  startBookBlock,
  startServer,
  test,
} from './fixtures.js';

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

test('R-82 first run: the setup code from the log, a new password, then practice', async ({ page, context }) => {
  const errors = collectErrors(page);
  const server = await startServer();
  try {
    expect(server.setupCode).toMatch(/^[0-9A-Z]{4}-[0-9A-Z]{4}-[0-9A-Z]{2}$/);
    await page.goto(`${server.url}/#/`);
    await expect(page).toHaveURL(/#\/login$/);
    await expect(page.getByRole('heading', { name: 'Set up' })).toBeVisible();
    await expect(page.getByLabel('Setup code')).toBeFocused();

    // Too short a password, then a wrong code: clear messages, nothing set up.
    await page.getByLabel('Setup code').fill(server.setupCode);
    await page.getByLabel('New password', { exact: true }).fill('test-short');
    await page.getByLabel('Repeat the new password').fill('test-short');
    await page.getByRole('button', { name: 'Set up' }).click();
    await expect(page.getByTestId('setup-status')).toHaveText(/at least 12 characters/);
    await page.getByLabel('Setup code').fill('0000-0000-00');
    await page.getByLabel('New password', { exact: true }).fill(PASSWORD);
    await page.getByLabel('Repeat the new password').fill(PASSWORD);
    await page.getByRole('button', { name: 'Set up' }).click();
    await expect(page.getByTestId('setup-status')).toHaveText('Wrong setup code. Check the server log and try again.');

    // The right code (typed in lower case) sets the password and logs in.
    await page.getByLabel('Setup code').fill(server.setupCode.toLowerCase());
    await page.getByRole('button', { name: 'Set up' }).click();
    await expect(page).toHaveURL(/#\/$/);
    await expect(page.getByRole('navigation', { name: 'Main' })).toBeVisible();
    const session = (await context.cookies()).find((c) => c.name === SESSION_COOKIE);
    expect(session?.httpOnly).toBe(true);

    // Practice works with that session.
    await importPacks(apiFor(server, session?.value ?? ''));
    await page.reload();
    await startBookBlock(page, { url: server.url, serverUrl: server.serverUrl, dbPath: server.dbPath, cookie: '', api: async () => null });

    // The code is used up: setup is refused from now on.
    const again = await fetch(`${server.serverUrl}/api/auth/setup`, {
      method: 'POST',
      headers: { 'content-type': 'application/json', origin: server.url },
      body: JSON.stringify({ code: server.setupCode, password: 'test-password-of-someone-else' }),
    });
    expect(again.status).toBe(409);
    // The code appears in exactly one log line.
    expect(server.log().split(server.setupCode).length - 1).toBe(1);
  } finally {
    await server.stop();
  }
  expect(errors).toEqual([]);
});

test('R-82 changing the password in Settings logs out every other session', async ({ page, app }) => {
  const errors = collectErrors(page);
  const server = { serverUrl: app.serverUrl, url: app.url };
  const other = await loginByApi(server);
  const newPassword = `${PASSWORD}-changed`;
  await page.goto(`${app.url}/#/settings`);

  await page.getByLabel('Current password').fill('test-password-wrong');
  await page.getByLabel('New password', { exact: true }).fill(newPassword);
  await page.getByLabel('Repeat the new password').fill(newPassword);
  await page.getByRole('button', { name: 'Change password' }).click();
  await expect(page.getByTestId('password-status')).toHaveText('The current password is wrong.');
  await expect(apiFor(server, other)('GET', '/api/today')).resolves.toBeTruthy();

  await page.getByLabel('Current password').fill(PASSWORD);
  await page.getByLabel('New password', { exact: true }).fill(newPassword);
  await page.getByLabel('Repeat the new password').fill(newPassword);
  await page.getByRole('button', { name: 'Change password' }).click();
  await expect(page.getByTestId('password-status')).toHaveText('Password changed. Every other device is logged out.');

  // The other session is gone; this one still works.
  await expect(apiFor(server, other)('GET', '/api/today')).rejects.toThrow(/401/);
  await page.getByRole('link', { name: 'Today' }).click();
  await expect(page.getByTestId('start')).toBeVisible();
  await expect(loginByApi(server)).rejects.toThrow(/401/);
  await expect(loginByApi(server, newPassword)).resolves.toBeTruthy();
  expect(errors).toEqual([]);
});

test('R-82 Settings creates a connector token that opens only the connector routes', async ({ page, app }) => {
  const errors = collectErrors(page);
  await page.goto(`${app.url}/#/settings`);
  await expect(page.getByTestId('connector-status')).toHaveText(/No token yet/);
  // The password is asked for again.
  await page.getByLabel('Your password').fill('test-password-wrong');
  await page.getByTestId('create-token').click();
  await expect(page.getByTestId('token-status')).toHaveText('The password is wrong.');
  await expect(page.getByTestId('connector-token')).toHaveCount(0);
  await page.getByLabel('Your password').fill(PASSWORD);
  await page.getByTestId('create-token').click();
  const field = page.getByTestId('connector-token');
  await expect(field).toHaveValue(/^[0-9a-f]{64}$/);
  const token = await field.inputValue();
  await expect(page.getByTestId('connector-status')).toHaveText(/A token exists/);
  const bearer = { authorization: `Bearer ${token}` };
  expect((await fetch(`${app.serverUrl}/api/connector/v1/sources`, { headers: bearer })).status).toBe(200);
  expect((await fetch(`${app.serverUrl}/api/today`, { headers: bearer })).status).toBe(401);
  // Shown once: after a reload only the status remains.
  await page.reload();
  await expect(page.getByTestId('connector-status')).toHaveText(/A token exists/);
  await expect(page.getByTestId('connector-token')).toHaveCount(0);
  expect(errors).toEqual([]);
});
