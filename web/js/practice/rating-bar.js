// @ts-check
/**
 * The 1–5 rating row and its legend (design §7.2, research §10c): number
 * plus label on every button, green → red ramp, 1–3 filled and 4–5
 * outlined, an extra gap with a pass ┆ fail divider between 3 and 4.
 */

import { h } from '../dom.js';
import { RATINGS, ratingAriaLabel, ratingGroups } from '../rating.js';

/**
 * @typedef {import('../rating.js').Grade} Grade
 * @typedef {import('../rating.js').Rating} Rating
 */

/**
 * @param {Rating} r
 * @param {(grade: Grade) => void} onRate
 */
function rateButton(r, onRate) {
  return h(
    'button',
    {
      type: 'button',
      class: `rate rate-${r.grade} ${r.pass ? 'filled' : 'outlined'}`,
      'data-grade': r.grade,
      'aria-label': ratingAriaLabel(r.grade),
      onclick: () => onRate(r.grade),
    },
    h('span', { class: 'rate-num', 'aria-hidden': 'true' }, String(r.grade)),
    h('span', { class: 'rate-label', 'aria-hidden': 'true' }, r.label),
  );
}

/**
 * @param {(grade: Grade) => void} onRate
 */
export function ratingBar(onRate) {
  const { pass, fail } = ratingGroups();
  return h(
    'div',
    { class: 'rating' },
    h(
      'div',
      { class: 'rating-captions', 'aria-hidden': 'true' },
      h('span', { class: 'caption-pass' }, 'pass · 1 = best'),
      h('span', { class: 'caption-fail' }, 'fail'),
    ),
    h(
      'div',
      { class: 'rating-bar', role: 'group', 'aria-label': 'Rate the word: 1 = best, 5 = no idea' },
      pass.map((r) => rateButton(r, onRate)),
      h('span', { class: 'rating-divider', 'aria-hidden': 'true', 'data-testid': 'rating-divider' }, '┆'),
      fail.map((r) => rateButton(r, onRate)),
    ),
  );
}

/** The legend: what each level means (R-60). */
export function legend() {
  return h(
    'dl',
    { class: 'legend', 'data-testid': 'legend' },
    RATINGS.map((r) =>
      h(
        'div',
        { class: `legend-row rate-${r.grade}${r.grade === 4 ? ' first-fail' : ''}` },
        h('dt', {}, h('span', { class: 'legend-num' }, String(r.grade)), ` ${r.label}`),
        h('dd', {}, r.definition),
      ),
    ),
  );
}
