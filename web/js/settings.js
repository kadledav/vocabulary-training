// @ts-check
/**
 * The owner's preferences (`GET/PATCH /api/settings`), loaded once and
 * shared by the screens. Applying the theme is done here, so every change
 * of the setting takes effect at once.
 */

import { getJson, patchJson } from './api.js';
import { applyTheme } from './theme.js';

/**
 * @typedef {{ new_per_day: number, block_size: number, voice: 'us' | 'uk', autoplay: boolean,
 *             speed: '1' | '0.75', theme: 'dark' | 'light' | 'system', legend: 'auto' | 'always' | 'off' }} Settings
 * @typedef {Partial<Settings>} SettingsPatch
 */

/** Used until the server answers (and if it cannot be reached): the server defaults. */
/** @type {Settings} */
export const DEFAULT_SETTINGS = Object.freeze({
  new_per_day: 5,
  block_size: 20,
  voice: 'us',
  autoplay: true,
  speed: '1',
  theme: 'dark',
  legend: 'auto',
});

/** @type {Settings | null} */
let cached = null;
/** @type {Promise<Settings> | null} */
let loading = null;

/**
 * The current settings (fetched on first use).
 *
 * @returns {Promise<Settings>}
 */
export function loadSettings() {
  if (cached) return Promise.resolve(cached);
  if (!loading) {
    loading = (async () => {
      const answer = /** @type {import('./api.js').ApiResult<Settings>} */ (await getJson('/api/settings'));
      loading = null;
      if (!answer.ok) return DEFAULT_SETTINGS;
      cached = answer.data;
      applyTheme(cached.theme);
      return cached;
    })();
  }
  return loading;
}

/**
 * Change some settings; resolves to the saved settings or an error message.
 *
 * @param {SettingsPatch} patch
 * @returns {Promise<{ ok: true, settings: Settings } | { ok: false, message: string }>}
 */
export async function saveSettings(patch) {
  const answer = /** @type {import('./api.js').ApiResult<Settings>} */ (await patchJson('/api/settings', patch));
  if (!answer.ok) return { ok: false, message: answer.message };
  cached = answer.data;
  applyTheme(cached.theme);
  return { ok: true, settings: cached };
}
