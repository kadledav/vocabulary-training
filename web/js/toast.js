// @ts-check
/**
 * The confirmation toast after a rating: "Rated 2 · Knew it — Undo" in the
 * rating's colour (design §7.2). One toast at a time; a new one replaces
 * the old. It is a polite live region, so screen readers hear the rating.
 */

import { h, mount } from './dom.js';

/**
 * @typedef {{ text: string, grade?: number, ms: number, pulse?: boolean, onUndo?: () => void }} ToastOptions
 */

export class Toast {
  /** @param {HTMLElement} slot where the toast lives (positioned by CSS) */
  constructor(slot) {
    this.slot = slot;
    /** @type {ReturnType<typeof setTimeout> | null} */
    this.timer = null;
    this.slot.setAttribute('role', 'status');
    this.slot.setAttribute('aria-live', 'polite');
  }

  /** @param {ToastOptions} options */
  show({ text, grade, ms, pulse = false, onUndo }) {
    this.clearTimer();
    const classes = ['toast'];
    if (grade) classes.push(`rate-${grade}`);
    if (pulse) classes.push('pulse');
    const toast = h(
      'div',
      { class: classes.join(' '), 'data-testid': 'toast' },
      h('span', { class: 'toast-text' }, text),
      onUndo
        ? h(
            'button',
            {
              type: 'button',
              class: 'toast-undo',
              onclick: () => {
                this.hide();
                onUndo();
              },
            },
            'Undo',
          )
        : null,
    );
    mount(this.slot, toast);
    this.timer = setTimeout(() => this.hide(), ms);
  }

  hide() {
    this.clearTimer();
    mount(this.slot);
  }

  clearTimer() {
    if (this.timer !== null) clearTimeout(this.timer);
    this.timer = null;
  }
}
