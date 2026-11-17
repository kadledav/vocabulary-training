// @ts-check
/**
 * Requirement-level login flows (R-82), written from the requirements:
 * a real login through the form leads to practice and the session survives
 * a reload, and the way back after login never leaves the app's origin.
 */

import {
  PASSWORD,
  SESSION_COOKIE,
  expect,
  test,
  waitGuard,
} from "./fixtures.js";

/** @param {import('@playwright/test').Page} page */
async function logIn(page) {
  await page.getByLabel("Password").fill(PASSWORD);
  await page.getByRole("button", { name: "Log in" }).click();
}

test("R-82 login through the form, practice, and a reload keeps the session", async ({
  page,
  app,
  context,
}) => {
  await context.clearCookies();
  await page.goto(`${app.url}/#/`);
  await expect(page).toHaveURL(/#\/login$/);
  await logIn(page);
  await expect(page.getByTestId("start")).toBeVisible();

  await page.getByLabel(/An E2E Test Novel/).check();
  await page.getByTestId("start").click();
  await expect(page).toHaveURL(/#\/practice$/);
  await expect(page.getByTestId("progress")).toHaveText("1 / 4");
  await page.getByTestId("reveal").click();
  await waitGuard();

  await page.reload();
  await expect(page).toHaveURL(/#\/practice$/);
  await expect(page.getByTestId("progress")).toBeVisible();
  await expect(page.getByLabel("Password")).toHaveCount(0);
  expect((await context.cookies()).some((c) => c.name === SESSION_COOKIE)).toBe(
    true,
  );
});

test("R-82 a wrong password shows a message and sets no session", async ({
  page,
  app,
  context,
}) => {
  await context.clearCookies();
  await page.goto(`${app.url}/#/`);
  await page.getByLabel("Password").fill("test-password-definitely-wrong");
  await page.getByRole("button", { name: "Log in" }).click();
  await expect(page.getByTestId("login-status")).toHaveText(
    "Wrong password. Try again.",
  );
  await expect(page).toHaveURL(/#\/login$/);
  expect((await context.cookies()).some((c) => c.name === SESSION_COOKIE)).toBe(
    false,
  );
});

test("R-82 a deep link while logged out returns to that page after login", async ({
  page,
  app,
  context,
}) => {
  await context.clearCookies();
  await page.goto(`${app.url}/#/settings`);
  await expect(page).toHaveURL(/#\/login$/);
  await logIn(page);
  await expect(page).toHaveURL(/#\/settings$/);
  await expect(page.getByTestId("logout")).toBeVisible();
});

for (const next of [
  "//evil.example",
  "https://evil.example/",
  "/\\evil.example",
  ["java", "script:alert(1)"].join(""),
]) {
  test(`R-82 login never redirects off-site (next=${next})`, async ({
    page,
    app,
    context,
  }) => {
    await context.clearCookies();
    /** @type {string[]} */
    const foreign = [];
    page.on("framenavigated", (frame) => {
      if (
        frame === page.mainFrame() &&
        new URL(frame.url()).origin !== new URL(app.url).origin
      )
        foreign.push(frame.url());
    });
    await page.route(/evil\.example/, (route) => route.abort());
    await page.goto(`${app.url}/#/login?next=${encodeURIComponent(next)}`);
    // Whatever the app does with the parameter, it stays on its own origin,
    // also through the login that a protected screen then asks for.
    await page.goto(`${app.url}/#/words`);
    await expect(page.getByLabel("Password")).toBeVisible();
    await logIn(page);
    await expect(page).toHaveURL(/#\/words$/);
    await expect(page.getByRole("navigation", { name: "Main" })).toBeVisible();
    expect(new URL(page.url()).origin).toBe(new URL(app.url).origin);
    expect(foreign).toEqual([]);
  });
}
