// @ts-check
/**
 * Theme (R-71; design §7.5): dark by default, light, or follow the system.
 * The setting lives on the server; the last applied value is also kept on
 * this device so the next page load starts in the right theme.
 */

/** @typedef {'dark' | 'light' | 'system'} ThemeSetting */

const STORAGE_KEY = 'vocab.theme';

/**
 * @param {unknown} value
 * @returns {value is ThemeSetting}
 */
export function isThemeSetting(value) {
  return value === 'dark' || value === 'light' || value === 'system';
}

/**
 * The `data-theme` attribute value for a setting: `null` = follow the
 * system (`prefers-color-scheme` in the stylesheet decides).
 *
 * @param {ThemeSetting} setting
 * @returns {'dark' | 'light' | null}
 */
export function themeAttribute(setting) {
  return setting === 'system' ? null : setting;
}

/**
 * Apply a theme setting to the document and remember it on this device.
 *
 * @param {ThemeSetting} setting
 */
export function applyTheme(setting) {
  const value = themeAttribute(setting);
  const root = document.documentElement;
  if (value) root.setAttribute('data-theme', value);
  else root.removeAttribute('data-theme');
  applyThemeColor(value);
  try {
    localStorage.setItem(STORAGE_KEY, setting);
  } catch {
    // Storage may be unavailable (private mode); the server setting still applies on load.
  }
}

/** Browser bar colours: the page background of each theme. */
const THEME_COLORS = { dark: '#15171c', light: '#f7f7f5' };

/**
 * Colour the browser's bar like the page: a forced theme overrides both
 * `theme-color` entries of the shell; "system" leaves each its own scheme.
 *
 * @param {'dark' | 'light' | null} forced
 */
function applyThemeColor(forced) {
  for (const meta of document.querySelectorAll('meta[name="theme-color"]')) {
    const scheme = (meta.getAttribute('media') ?? '').includes('light') ? 'light' : 'dark';
    meta.setAttribute('content', THEME_COLORS[forced ?? scheme]);
  }
}

/** Apply the theme remembered on this device (before the settings arrive). */
export function applyRememberedTheme() {
  let stored = null;
  try {
    stored = localStorage.getItem(STORAGE_KEY);
  } catch {
    stored = null;
  }
  applyTheme(isThemeSetting(stored) ? stored : 'dark');
}
