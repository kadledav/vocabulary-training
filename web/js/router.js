// @ts-check
/** Hash routes (`#/import`, `#/words`, `#/words/12`). */

/**
 * @typedef {{ name: string, params: Record<string, string> }} Route
 */

/**
 * Parse `location.hash` into a route.
 *
 * @param {string} hash
 * @returns {Route}
 */
export function parseRoute(hash) {
  const path = hash.replace(/^#/, '') || '/';
  const parts = path.split('/').filter((part) => part !== '');
  if (parts.length === 0) return { name: 'home', params: {} };
  if (parts[0] === 'import' && parts.length === 1) return { name: 'import', params: {} };
  if (parts[0] === 'words' && parts.length === 1) return { name: 'words', params: {} };
  if (parts[0] === 'words' && parts.length === 2 && /^[0-9]+$/.test(parts[1])) {
    return { name: 'word', params: { id: parts[1] } };
  }
  return { name: 'not-found', params: {} };
}

/**
 * Tells a screen whether it is still the one on display. Screens call it
 * after every `await` and stop when it returns false, so a slow answer for
 * a screen the owner already left never replaces the current one.
 *
 * @typedef {() => boolean} IsCurrent
 */

let generation = 0;

/**
 * Call `render` with the current route now and on every hash change.
 *
 * @param {(route: Route, isCurrent: IsCurrent) => void} render
 */
export function startRouter(render) {
  const run = () => {
    const mine = ++generation;
    render(parseRoute(window.location.hash), () => mine === generation);
  };
  window.addEventListener('hashchange', run);
  run();
}
