// @ts-check
/**
 * The only way the frontend creates DOM (R-84): elements via createElement,
 * text via text nodes. Markup strings are never parsed, so pack content
 * (untrusted Claude output) can never become HTML. HTML-injection APIs are
 * banned in web/ by tools/repo-guard.py.
 */

/** @typedef {string | number | boolean | null | undefined | ((event: Event) => void)} AttrValue */
/** @typedef {Node | string | number | null | undefined | false} Child */

/**
 * Create an element.
 * - `attrs.class` sets the class name; `on<event>` keys with a function value
 *   add event listeners; `true` sets a boolean attribute; `false`/null/undefined
 *   skip the attribute; anything else is set as a string attribute.
 * - Children: nodes are appended, strings/numbers become text nodes, falsy
 *   values are skipped.
 *
 * @template {keyof HTMLElementTagNameMap} K
 * @param {K} tag
 * @param {Record<string, AttrValue>} [attrs]
 * @param {...(Child | Child[])} children
 * @returns {HTMLElementTagNameMap[K]}
 */
export function h(tag, attrs = {}, ...children) {
  const el = document.createElement(tag);
  for (const [name, value] of Object.entries(attrs)) {
    if (typeof value === 'function') {
      if (!name.startsWith('on')) throw new Error(`function value for non-event attribute ${name}`);
      el.addEventListener(name.slice(2).toLowerCase(), value);
    } else if (name.toLowerCase().startsWith('on')) {
      throw new Error(`event attribute ${name} needs a function`);
    } else if (value === true) {
      el.setAttribute(name, '');
    } else if (value !== false && value !== null && value !== undefined) {
      el.setAttribute(name, String(value));
    }
  }
  for (const child of children.flat()) {
    if (child === null || child === undefined || child === false) continue;
    el.append(typeof child === 'object' ? child : document.createTextNode(String(child)));
  }
  return el;
}

/**
 * Replace all children of `parent` (empty slots — null, undefined, false —
 * are skipped).
 *
 * @param {Element} parent
 * @param {...(Node | null | undefined | false)} nodes
 */
export function mount(parent, ...nodes) {
  parent.replaceChildren(...nodes.filter((node) => node !== null && node !== undefined && node !== false));
}
