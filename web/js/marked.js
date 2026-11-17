// @ts-check
/**
 * Rendering of a marked sentence that the server already split into
 * `{pre, target, post}` (design §8). Everything is text; the target is only
 * wrapped in an element for its highlight (bold + underline, not colour alone).
 */

import { h } from './dom.js';

/** @typedef {{ pre: string, target: string, post: string }} TextParts */

/**
 * @param {TextParts} parts
 * @param {string} lang
 */
export function markedText(parts, lang) {
  return h(
    'span',
    { lang },
    parts.pre,
    parts.target ? h('strong', { class: 'target' }, parts.target) : null,
    parts.post,
  );
}
