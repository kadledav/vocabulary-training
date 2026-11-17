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
  try {
    localStorage.setItem(STORAGE_KEY, setting);
  } catch {
    // Storage may be unavailable (private mode); the server setting still applies on load.
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
