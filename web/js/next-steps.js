// @ts-check
/**
 * What the owner can do next, shared by Today and the block summary
 * (usability U1, owner feedback F1): Start/Continue with what the block
 * will hold, or — when the day's work is done — "Done for today" with
 * "Practise more" and "Learn N more new words". Never a dead end.
 */

import { blockLabel, dayDone, dueTomorrow, moreNewCount, nextBlock, plural } from './day-plan.js';
import { h } from './dom.js';
import { startBlock } from './session.js';

/**
 * @typedef {import('./session.js').Today} Today
 * @typedef {import('./session.js').StartKind} StartKind
 * @typedef {{ today: Today, scope: number | null, blockSize: number, perDay: number,
 *             isCurrent: () => boolean, message: HTMLElement, startTestId: string }} NextStepsOptions
 */

/**
 * Start a block of `kind` and open it; says so when there is nothing to
 * practise.
 *
 * @param {NextStepsOptions} options
 * @param {{ kind: StartKind, count?: number, addNewAnyway?: boolean }} request
 * @param {HTMLButtonElement} button
 */
export async function begin(options, request, button) {
  button.disabled = true;
  options.message.textContent = '';
  const answer = await startBlock({
    ...request,
    scope: options.scope,
    finishedBlocks: options.today.finished_blocks.total,
  });
  if (!options.isCurrent()) return;
  button.disabled = false;
  if (!answer.ok) {
    options.message.textContent = answer.message;
    return;
  }
  if (!answer.data.session) {
    options.message.textContent =
      request.kind === 'extra' ? 'Nothing to practise again right now.' : 'Nothing to practise in this scope right now.';
    return;
  }
  window.location.hash = '#/practice';
}

/**
 * "Done for today" with the two ways to go on.
 *
 * @param {NextStepsOptions} options
 */
function endOfDay(options) {
  const { today, perDay } = options;
  const extra = today.extra_available;
  const canExtra = extra === undefined || extra > 0;
  const more = h(
    'button',
    { type: 'button', class: 'primary big', 'data-testid': 'practise-more', disabled: !canExtra, 'aria-describedby': canExtra ? false : 'extra-why' },
    'Practise more',
  );
  more.addEventListener('click', () => void begin(options, { kind: 'extra' }, more));
  const count = moreNewCount(today, perDay);
  const learn =
    count > 0
      ? h('button', { type: 'button', class: 'big', 'data-testid': 'learn-more' }, `Learn ${plural(count, 'more new word')}`)
      : null;
  learn?.addEventListener('click', () => void begin(options, { kind: 'more_new', count }, learn));
  return h(
    'section',
    { class: 'card end-of-day', 'data-testid': 'end-of-day' },
    h('p', { class: 'end-title' }, `Done for today — ${dueTomorrow(today)} due tomorrow`),
    h(
      'div',
      { class: 'stack' },
      more,
      canExtra
        ? h('p', { class: 'muted small' }, 'Extra practice of words you have seen, weakest first.')
        : h(
            'p',
            { class: 'muted small', id: 'extra-why', 'data-testid': 'extra-why' },
            'Nothing needs extra practice yet: words you knew well today rest until they are due.',
          ),
      learn,
    ),
  );
}

/**
 * The Start/Continue button, or the end-of-day panel.
 *
 * @param {NextStepsOptions} options
 * @returns {HTMLElement}
 */
export function nextSteps(options) {
  const { today, blockSize } = options;
  if (dayDone(today, blockSize)) return endOfDay(options);
  const kind = today.finished_blocks.today > 0 ? 'continue' : 'start';
  const button = h(
    'button',
    { type: 'button', class: 'primary big', 'data-testid': options.startTestId },
    `${kind === 'continue' ? 'Continue' : 'Start'} · ${blockLabel(nextBlock(today, blockSize))}`,
  );
  button.addEventListener('click', () => void begin(options, { kind }, button));
  return button;
}
