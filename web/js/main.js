// @ts-check
/** Entry point of the single-page app. M1 renders only the empty shell. */

import { h, mount } from './dom.js';

const root = document.getElementById('app');
if (root) {
  mount(
    root,
    h('h1', {}, 'Vocabulary'),
    h(
      'section',
      { class: 'card' },
      h('p', { class: 'muted' }, 'The app is being set up. Nothing to practise yet.'),
    ),
  );
}
