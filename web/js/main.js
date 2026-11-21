// @ts-check
/**
 * Entry point of the single-page app: theme, navigation and hash routing.
 * Practice is a focused screen with its own top bar (no navigation); the
 * login screen has no navigation either; the other screens share the
 * navigation bar. Any "not logged in" answer of the server leads to the
 * login screen, which returns to the screen afterwards.
 */

import { onBusy, onUnauthenticated } from './api.js';
import { goToLogin } from './auth.js';
import { h, mount } from './dom.js';
import { startRouter } from './router.js';
import { renderImport } from './screens/import.js';
import { renderLogin } from './screens/login.js';
import { renderPractice } from './screens/practice.js';
import { renderSettings } from './screens/settings.js';
import { renderSummary } from './screens/summary.js';
import { renderToday } from './screens/today.js';
import { renderWord, renderWords } from './screens/words.js';
import { loadSettings } from './settings.js';
import { applyRememberedTheme } from './theme.js';

applyRememberedTheme();
onUnauthenticated(goToLogin);
void loadSettings();

const root = document.getElementById('app');

/** @param {string} name the route whose link is current */
function nav(name) {
  const link = (/** @type {string} */ href, /** @type {string} */ label, /** @type {string} */ route) =>
    h('a', { href, 'aria-current': route === name ? 'page' : false }, label);
  return h(
    'nav',
    { class: 'top', 'aria-label': 'Main' },
    link('#/', 'Today', 'home'),
    link('#/words', 'Words', 'words'),
    link('#/import', 'Import', 'import'),
    link('#/settings', 'Settings', 'settings'),
  );
}

// The screens set the scroll position themselves (top on a new screen, the
// old place when Words is shown again): the browser's own restoring on
// Back would fight with that.
if ('scrollRestoration' in history) history.scrollRestoration = 'manual';

if (root) {
  const header = h('header', {});
  const content = h('div', { class: 'content' });
  const busy = h('p', { class: 'busy-banner', role: 'status', 'data-testid': 'busy' });
  onBusy((on) => {
    busy.textContent = on ? 'The server is busy, trying again…' : '';
  });
  root.before(header, busy);
  mount(root, content);
  /** @type {(() => void) | null} */
  let leave = null;
  startRouter((route, isCurrent) => {
    if (leave) leave();
    leave = null;
    const practice = route.name === 'practice';
    const bare = practice || route.name === 'login';
    document.body.classList.toggle('in-practice', practice);
    mount(header, ...(bare ? [] : [nav(route.name === 'word' ? 'words' : route.name)]));
    switch (route.name) {
      case 'home':
        renderToday(content, isCurrent);
        break;
      case 'practice':
        leave = renderPractice(content, isCurrent);
        break;
      case 'summary':
        renderSummary(content, isCurrent);
        break;
      case 'settings':
        renderSettings(content, isCurrent);
        break;
      case 'login':
        renderLogin(content, isCurrent);
        break;
      case 'import':
        renderImport(content, isCurrent);
        break;
      case 'words':
        renderWords(content, isCurrent);
        break;
      case 'word':
        void renderWord(content, route.params.id, isCurrent);
        break;
      default:
        mount(content, h('h1', {}, 'Not found'), h('p', {}, h('a', { href: '#/' }, 'Today')));
    }
    window.scrollTo(0, 0);
  });
}
