// @ts-check
/**
 * The 1–5 rating row and its legend (design §7.2, research §10c): number
 * plus label on every button, green → red ramp, 1–3 filled and 4–5
 * outlined, an extra gap with a pass ┆ fail divider between 3 and 4.
 */

import { h } from '../dom.js';
import { RATINGS, rating, ratingAriaLabel, ratingGroups } from '../rating.js';

/**
 * @typedef {import('../rating.js').Grade} Grade
 * @typedef {import('../rating.js').Rating} Rating
 */

/**
 * @param {Rating} r
 * @param {(grade: Grade) => void} onRate
 * @param {Grade | null} chosen
 * @param {Grade | null} suggested
 */
function rateButton(r, onRate, chosen, suggested) {
  const isSuggested = suggested === r.grade;
  return h(
    'button',
    {
      type: 'button',
      class: `rate rate-${r.grade} ${r.pass ? 'filled' : 'outlined'}${chosen === r.grade ? ' chosen' : ''}${isSuggested ? ' suggested' : ''}`,
      'data-grade': r.grade,
      'data-suggested': isSuggested,
      'aria-label': `${ratingAriaLabel(r.grade)}${isSuggested ? ' (suggested)' : ''}`,
      onclick: () => onRate(r.grade),
    },
    h('span', { class: 'rate-num', 'aria-hidden': 'true' }, String(r.grade)),
    h('span', { class: 'rate-label', 'aria-hidden': 'true' }, r.label),
  );
}

/**
 * @param {(grade: Grade) => void} onRate
 * @param {Grade | null} [chosen] the grade being saved: shown pressed at once (U3)
 * @param {Grade | null} [suggested] a typed card's suggested grade: ringed (R-56)
 */
export function ratingBar(onRate, chosen = null, suggested = null) {
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
      pass.map((r) => rateButton(r, onRate, chosen, suggested)),
      h('span', { class: 'rating-divider', 'aria-hidden': 'true', 'data-testid': 'rating-divider' }, '┆'),
      fail.map((r) => rateButton(r, onRate, chosen, suggested)),
    ),
  );
}

/**
 * The line above the rating bar of a typed card: what the check suggests
 * and that the owner decides (R-56: the 1–5 rating is his).
 *
 * @param {Grade | null} suggested
 */
export function suggestionLine(suggested) {
  if (suggested === null) return h('p', { class: 'criterion', 'data-testid': 'suggestion' }, 'Rate yourself: how well did you know the word?');
  return h(
    'p',
    { class: 'criterion', 'data-testid': 'suggestion' },
    `Suggested: ${suggested} · ${rating(suggested).label} — Enter or tap it, or choose another.`,
  );
}

/**
 * The legend in one compact line, shown in the flow of the card above the
 * rating bar during the first blocks (never inside the pinned bar, where it
 * would hide the answer on small screens).
 */
export function legendLine() {
  return h(
    'p',
    { class: 'legend-line', 'data-testid': 'legend' },
    RATINGS.flatMap((r, i) => [
      i > 0 ? h('span', { class: 'legend-sep' }, ' · ') : null,
      h('span', { class: `legend-item rate-${r.grade}` }, h('b', {}, String(r.grade)), ` = ${r.short}`),
    ]),
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
