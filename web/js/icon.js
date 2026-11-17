// @ts-check
/**
 * Pack icons are emoji or icon-set names (e.g. Lucide `book-open`); only
 * emoji are shown in phase 1, no icon set is shipped (design §7.5).
 *
 * @param {string | null} icon
 * @returns {string | null}
 */
export function displayIcon(icon) {
  if (!icon || /^[a-z0-9-]+$/.test(icon)) return null;
  return icon;
}
