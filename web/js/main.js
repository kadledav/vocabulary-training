// @ts-check
/**
 * Entry point of the single-page app: navigation and hash routing.
 * M2 has the Import page and a read-only word list; practice arrives in M4.
 */

import { h, mount } from './dom.js';
import { startRouter } from './router.js';
import { renderImport } from './screens/import.js';
import { renderWord, renderWords } from './screens/words.js';

const root = document.getElementById('app');

/** @param {string} name */
function nav(name) {
  const link = (/** @type {string} */ href, /** @type {string} */ label, /** @type {string} */ route) =>
    h('a', { href, 'aria-current': route === name ? 'page' : false }, label);
  return h(
    'nav',
    { class: 'top' },
    link('#/', 'Vocabulary', 'home'),
    link('#/import', 'Import', 'import'),
    link('#/words', 'Words', 'words'),
  );
}

if (root) {
  const header = h('header', {});
  const content = h('div', { class: 'content' });
  root.before(header);
  mount(root, content);
  startRouter((route, isCurrent) => {
    mount(header, nav(route.name === 'word' ? 'words' : route.name));
    switch (route.name) {
      case 'import':
        renderImport(content, isCurrent);
        break;
      case 'words':
        renderWords(content, isCurrent);
        break;
      case 'word':
        void renderWord(content, route.params.id, isCurrent);
        break;
      case 'home':
        mount(
          content,
          h('h1', {}, 'Vocabulary'),
          h(
            'section',
            { class: 'card' },
            h('p', {}, 'Practice is not available yet.'),
            h('p', {}, h('a', { href: '#/import' }, 'Import packs'), ' or ', h('a', { href: '#/words' }, 'browse the words'), '.'),
          ),
        );
        break;
      default:
        mount(content, h('h1', {}, 'Not found'), h('p', {}, h('a', { href: '#/' }, 'Home')));
    }
    window.scrollTo(0, 0);
  });
}
