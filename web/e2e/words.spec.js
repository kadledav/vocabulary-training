// @ts-check
/**
 * M7 Words: browse, detail, edit (design §7.1 screens 6 and 7; R-03, R-04,
 * R-25, R-51, R-52, R-73). One test per owner action, on the phone (WebKit,
 * touch) and the desktop (Chromium). Synthetic words only (e2e/pack.js).
 */

import { BOOK_PACK } from './pack.js';
import { expect, test } from './fixtures.js';

/**
 * The id of a word of the synthetic packs.
 *
 * @param {import('./fixtures.js').App} app
 * @param {string} headword
 * @returns {Promise<number>}
 */
async function wordId(app, headword) {
  const rows = await app.api('GET', `/api/words?q=${encodeURIComponent(headword)}`);
  const row = rows.find((/** @type {{ headword: string }} */ r) => r.headword === headword);
  if (!row) throw new Error(`${headword} not found`);
  return row.id;
}

/**
 * Open a word's screen.
 *
 * @param {import('@playwright/test').Page} page
 * @param {import('./fixtures.js').App} app
 * @param {string} headword
 */
async function openWord(page, app, headword) {
  const id = await wordId(app, headword);
  await page.goto(`${app.url}/#/words/${id}`);
  await expect(page.getByTestId('word-headword')).toContainText(headword);
  return id;
}

/** The book pack imported again under a new pack id (same content). */
function bookAgain(/** @type {Record<string, unknown>} */ changes = {}) {
  return { ...BOOK_PACK, pack_id: 'test-e2e-novel--2026-11-02--1', ...changes };
}

/**
 * Accept the browser's console line for an expected refusal.
 *
 * @param {string[]} pageErrors
 * @param {number} status
 */
function allowStatus(pageErrors, status) {
  const line = new RegExp(`^console: Failed to load resource: the server responded with a status of ${status}`);
  for (let i = pageErrors.length - 1; i >= 0; i -= 1) if (line.test(pageErrors[i] ?? '')) pageErrors.splice(i, 1);
}

/**
 * The page never scrolls sideways (phone first).
 *
 * @param {import('@playwright/test').Page} page
 */
async function expectNoSidewaysScroll(page) {
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(0);
}

test('R-73 the list searches, sorts and filters, and a row opens the word', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await page.goto(`${app.url}/#/words`);
  await expect(page.getByTestId('word-count')).toHaveText('5 words');
  await page.getByLabel('Search words').fill('třpyt');
  await expect(page.getByTestId('word-count')).toHaveText('1 word');
  await page.getByLabel('Search words').fill('');
  await expect(page.getByTestId('word-count')).toHaveText('5 words');

  await page.getByTestId('filters-summary').click();
  await page.getByRole('radio', { name: 'Most useful' }).check();
  await expect(page.locator('a.word-row .headword')).toHaveText(['glimmerous', 'brontle', 'quabble', 'snorkish', 'flumpet']);
  await page.getByLabel('Usefulness from').fill('7');
  await page.getByLabel('Usefulness from').blur();
  await expect(page.getByTestId('word-count')).toHaveText('3 words');
  await expect(page.getByTestId('filters-summary')).toContainText('1 filter on');
  await page.getByRole('radio', { name: /An E2E Test Article/ }).check();
  await expect(page.getByTestId('word-count')).toHaveText('0 words');
  await expect(page.getByText('No word matches.')).toBeVisible();
  await page.getByRole('button', { name: 'Clear sort and filters' }).click();
  await expect(page.getByTestId('word-count')).toHaveText('5 words');
  await expect(page.locator('a.word-row .headword').first()).toHaveText('brontle');

  await page.getByRole('checkbox', { name: 'Suspended' }).check();
  await expect(page.getByTestId('word-count')).toHaveText('0 words');
  await page.getByRole('checkbox', { name: 'Suspended' }).uncheck();
  await page.getByRole('radio', { name: 'New', exact: true }).check();
  await expect(page.getByTestId('word-count')).toHaveText('5 words');
  await expectNoSidewaysScroll(page);

  await page.locator('a.word-row', { hasText: 'quabble' }).click();
  await expect(page).toHaveURL(/#\/words\/\d+$/);
  await expect(page.getByTestId('word-headword')).toHaveText('quabble');
  await expect(page.getByTestId('word-status')).toContainText('New');
  await expectNoSidewaysScroll(page);
  await page.getByRole('link', { name: '← Words' }).click();
  await expect(page.getByRole('radio', { name: 'New', exact: true })).toBeChecked();
});

test('R-04 a field is edited, saved with a toast, undone, and an edit can be cancelled', async ({ page, app, pageErrors }) => {
  void pageErrors;
  const id = await openWord(page, app, 'brontle');
  const meaning = page.getByTestId('field-meaning');
  await page.getByRole('button', { name: 'Edit meaning' }).click();
  await page.getByLabel('Meaning', { exact: true }).fill('To mutter to oneself while busy (owner’s words).');
  await page.getByRole('button', { name: 'Save' }).click();
  await expect(page.getByTestId('toast')).toContainText('Saved: Meaning.');
  await expect(meaning).toContainText('To mutter to oneself while busy (owner’s words).');
  await expect(meaning).toContainText('edited');
  // The header shows the new meaning too, and the server has it.
  expect((await app.api('GET', `/api/words/${id}`)).meaning).toBe('To mutter to oneself while busy (owner’s words).');

  await page.getByTestId('toast').getByRole('button', { name: 'Undo' }).click();
  await expect(page.getByTestId('toast')).toContainText('Undone.');
  await expect(meaning).toContainText('To grumble quietly while working.');

  // Cancel leaves the value as it was.
  await page.getByRole('button', { name: 'Edit grammar' }).click();
  await page.getByLabel('Grammar', { exact: true }).fill('something else');
  await page.getByRole('button', { name: 'Cancel' }).click();
  await expect(page.getByTestId('field-grammar')).toContainText('(empty)');
  expect((await app.api('GET', `/api/words/${id}`)).grammar).toBeNull();

  // A required field cannot be emptied: said in place, nothing sent.
  await page.getByRole('button', { name: 'Edit headword' }).click();
  await page.getByLabel('Headword', { exact: true }).fill('   ');
  await page.getByRole('button', { name: 'Save' }).click();
  await expect(page.getByTestId('field-headword-problem')).toHaveText('Headword cannot be empty.');
});

test('R-04 a refused save says why and keeps what was typed', async ({ page, app, pageErrors }) => {
  await openWord(page, app, 'brontle');
  await page.getByRole('button', { name: 'Edit headword' }).click();
  const input = page.getByLabel('Headword', { exact: true });
  await input.fill('Quabble');
  await page.getByRole('button', { name: 'Save' }).click();
  await expect(page.getByTestId('field-headword-problem')).toHaveText('Another word already has the headword "quabble".');
  await expect(input).toHaveValue('Quabble');
  allowStatus(pageErrors, 409);
  // A lost connection: the same, nothing lost.
  await page.route('**/api/words/*', (route) => (route.request().method() === 'PATCH' ? route.abort() : route.continue()));
  await input.fill('brontle (to grumble)');
  await page.getByRole('button', { name: 'Save' }).click();
  await expect(page.getByTestId('field-headword-problem')).toHaveText('The server cannot be reached.');
  await expect(input).toHaveValue('brontle (to grumble)');
  for (let i = pageErrors.length - 1; i >= 0; i -= 1) if (/ERR_FAILED|cancelled|Load failed|network connection/i.test(pageErrors[i] ?? '')) pageErrors.splice(i, 1);
});

test('R-04 edits survive a re-import of the same pack; the pack only suggests (R-03)', async ({ page, app, pageErrors }) => {
  void pageErrors;
  const id = await openWord(page, app, 'glimmerous');
  await page.getByRole('button', { name: 'Edit meaning' }).click();
  await page.getByLabel('Meaning', { exact: true }).fill('Glowing on and off (my words).');
  await page.getByRole('button', { name: 'Save' }).click();
  await expect(page.getByTestId('field-meaning')).toContainText('my words');
  await page.getByRole('button', { name: 'Raise usefulness' }).click();
  await expect(page.getByTestId('usefulness')).toHaveText('10');

  const report = await app.api('POST', '/api/import/commit', { text: JSON.stringify(bookAgain()) });
  expect(report.packs[0].status).toBe('imported');
  await page.reload();
  await expect(page.getByTestId('field-meaning')).toContainText('Glowing on and off (my words).');
  await expect(page.getByTestId('usefulness')).toHaveText('10');
  const meaningConflict = page.getByTestId('conflict-meaning');
  await expect(meaningConflict).toContainText('Shining faintly and unsteadily.');
  await expect(page.getByTestId('conflict-usefulness')).toContainText('9');

  // Keep mine: the suggestion goes; Use new: the pack's value comes.
  await page.getByTestId('conflict-usefulness').getByRole('button', { name: 'Keep mine' }).click();
  await expect(page.getByTestId('conflict-usefulness')).toHaveCount(0);
  await expect(page.getByTestId('usefulness')).toHaveText('10');
  await meaningConflict.getByRole('button', { name: 'Use new' }).click();
  await expect(page.getByTestId('conflicts')).toHaveCount(0);
  await expect(page.getByTestId('field-meaning')).toContainText('Shining faintly and unsteadily.');
  await expect(page.getByTestId('toast')).toContainText('Took the pack’s meaning.');
  await page.getByTestId('toast').getByRole('button', { name: 'Undo' }).click();
  await expect(page.getByTestId('field-meaning')).toContainText('Glowing on and off (my words).');
  expect((await app.api('GET', `/api/words/${id}`)).conflicts).toEqual([]);
});

test('R-52 the goal switch and usefulness are saved at once', async ({ page, app, pageErrors }) => {
  void pageErrors;
  const id = await openWord(page, app, 'snorkish');
  await expect(page.getByRole('radio', { name: /Automatic \(produce/ })).toBeChecked();
  await page.getByRole('radio', { name: 'Recognise only' }).check();
  await expect(page.getByTestId('toast')).toContainText('Goal saved.');
  expect((await app.api('GET', `/api/words/${id}`)).effective_goal).toBe('recognise');
  await page.getByRole('button', { name: 'Lower usefulness' }).click();
  await expect(page.getByTestId('usefulness')).toHaveText('5');
  await expect(page.getByRole('radio', { name: /Automatic \(recognise/ })).toBeVisible();
  await page.getByRole('radio', { name: /Automatic/ }).check();
  await expect(page.getByTestId('toast')).toContainText('Goal saved.');
  const word = await app.api('GET', `/api/words/${id}`);
  expect([word.goal, word.usefulness, word.effective_goal]).toEqual([null, 5, 'recognise']);
});

test('R-51 a low-priority word can be included; R-04 a word can be suspended', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await app.api('POST', '/api/import/commit', {
    text: JSON.stringify({
      ...bookAgain(),
      pack_id: 'test-e2e-novel--2026-11-03--1',
      words: [{ ...BOOK_PACK.words[3], headword: 'drowsle', tag: { kind: 'low-priority' } }],
    }),
  });
  const before = (await app.api('GET', '/api/today')).new_waiting;
  await openWord(page, app, 'drowsle');
  await expect(page.getByTestId('word-status')).toContainText('low priority');
  await page.getByLabel('Practise this low-priority word (recognise only)').check();
  await expect(page.getByTestId('toast')).toContainText('Included in practice.');
  await expect(page.getByTestId('word-status')).toContainText('low priority, included');
  expect((await app.api('GET', '/api/today')).new_waiting).toBe(before + 1);

  await page.getByLabel('Suspended (not practised)').check();
  await expect(page.getByTestId('toast')).toContainText('Suspended.');
  await expect(page.getByTestId('word-status')).toContainText('suspended');
  expect((await app.api('GET', '/api/today')).new_waiting).toBe(before);
  await page.getByTestId('toast').getByRole('button', { name: 'Undo' }).click();
  await expect(page.getByLabel('Suspended (not practised)')).not.toBeChecked();
  expect((await app.api('GET', '/api/today')).new_waiting).toBe(before + 1);
});

test('R-04 Czech equivalents, accepted answers and forms are added and removed', async ({ page, app, pageErrors }) => {
  void pageErrors;
  const id = await openWord(page, app, 'glimmerous');
  const czech = page.getByTestId('list-czech');
  await page.getByLabel('Add Czech equivalent').fill('zářivý');
  await czech.getByRole('button', { name: 'Add' }).click();
  await expect(czech.locator('.chip')).toHaveText(['třpytivý×', 'mihotavý×', 'zářivý×']);
  await czech.getByRole('button', { name: 'Remove třpytivý' }).click();
  await expect(page.getByTestId('toast')).toContainText('Removed “třpytivý”.');
  await expect(czech.locator('.chip')).toHaveCount(2);
  await page.getByTestId('toast').getByRole('button', { name: 'Undo' }).click();
  await expect(czech.locator('.chip')).toHaveCount(3);

  const accepted = page.getByTestId('list-accepted');
  await page.getByLabel('Add accepted answer').fill('glimmery');
  await accepted.getByRole('button', { name: 'Add' }).click();
  await expect(accepted).toContainText('glimmery (mine)');
  await page.getByTestId('list-form').getByRole('button', { name: 'Remove glimmerously' }).click();
  await expect(page.getByTestId('list-form').locator('.chip')).toHaveCount(1);
  // Removed stays removed when the pack comes again.
  await app.api('POST', '/api/import/commit', { text: JSON.stringify(bookAgain()) });
  const word = await app.api('GET', `/api/words/${id}`);
  expect(word.items.forms.map((/** @type {{ form: string }} */ f) => f.form)).toEqual(['glimmerous']);
  // An empty add is refused in place.
  await page.getByTestId('list-confusable').getByRole('button', { name: 'Add' }).click();
  await expect(page.getByTestId('list-confusable-problem')).toHaveText('Type the confusable word first.');
});

test('R-25 a sentence is corrected (re-validated) and hidden', async ({ page, app, pageErrors }) => {
  const id = await openWord(page, app, 'quabble');
  const first = (await app.api('GET', `/api/words/${id}`)).sentences[0];
  const item = page.getByTestId(`sentence-${first.id}`);
  await item.getByRole('button', { name: 'Edit' }).click();
  await expect(page.getByLabel('English', { exact: true })).toHaveValue('They had a [[quabble]] about the map.');
  // Without marks: said before anything is sent.
  await page.getByLabel('English', { exact: true }).fill('They had a quabble about the old map.');
  await item.getByRole('button', { name: 'Save' }).click();
  await expect(page.getByTestId('sentence-problem')).toHaveText('Mark the word once with [[ and ]] in the English sentence.');
  // The server's rule: a Czech text with letters.
  await page.getByLabel('English', { exact: true }).fill('They had a [[quabble]] about the old map.');
  await page.getByLabel('Czech', { exact: true }).fill('[[123]]');
  await item.getByRole('button', { name: 'Save' }).click();
  await expect(page.getByTestId('sentence-problem')).toContainText('no letters');
  await expect(page.getByLabel('English', { exact: true })).toHaveValue('They had a [[quabble]] about the old map.');
  allowStatus(pageErrors, 422);
  await page.getByLabel('Czech', { exact: true }).fill('Měli kvůli staré mapě [[šarvátku]].');
  await item.getByRole('button', { name: 'Save' }).click();
  await expect(page.getByTestId('toast')).toContainText('Sentence saved.');
  await expect(item).toContainText('They had a quabble about the old map.');
  await expect(item).toContainText('edited by you');
  await expect(item.locator('strong.target').first()).toHaveText('quabble');

  await item.getByRole('button', { name: 'Hide from practice' }).click();
  await expect(page.getByTestId('toast')).toContainText('hidden from practice');
  await expect(item).toContainText('hidden');
  await expect(item.getByRole('button', { name: 'Show in practice' })).toBeVisible();
  const stored = (await app.api('GET', `/api/words/${id}`)).sentences[0];
  expect([stored.edited, stored.hidden]).toEqual([true, true]);
});

test('R-04 Reset card asks first, says what is lost, and keeps the history', async ({ page, app, pageErrors }) => {
  void pageErrors;
  const id = await wordId(app, 'glimmerous');
  // One review of the word (it is first in a new block).
  const start = await app.api('POST', '/api/sessions', { kind: 'start' });
  const item = start.session.items.find((/** @type {any} */ i) => i.word.headword === 'glimmerous');
  await app.api('POST', `/api/sessions/${start.session.id}/reviews`, {
    client_review_id: '00000000-0000-4000-8000-0000000000e7',
    item_id: item.item_id,
    raw_grade: 2,
    hint_used: false,
  });
  await page.goto(`${app.url}/#/words/${id}`);
  const card = page.getByTestId('card-understand');
  await expect(card).toContainText('in review');
  await expect(card).toContainText('recall chance now');
  await expect(page.getByText('Review history (1)')).toBeVisible();
  await expect(page.locator('.history-list li')).toContainText('2 Knew it');

  await page.getByTestId('reset-understand').click();
  await expect(card).toContainText('Its memory, next due day and lapse count are lost');
  await card.getByRole('button', { name: 'Cancel' }).click();
  await expect(card).toContainText('in review');
  await page.getByTestId('reset-understand').click();
  await page.getByTestId('reset-understand-confirm').click();
  await expect(page.getByTestId('toast')).toContainText('Understand card reset.');
  await expect(card).toContainText('not practised yet');
  await expect(page.getByText('Review history (1)')).toBeVisible();
});

test('R-04 Delete word asks first, then the list says it is gone', async ({ page, app, pageErrors }) => {
  void pageErrors;
  await openWord(page, app, 'flumpet');
  await page.getByTestId('delete-word').click();
  await expect(page.getByRole('group', { name: 'Delete word' })).toContainText('cannot be undone');
  await page.getByRole('group', { name: 'Delete word' }).getByRole('button', { name: 'Cancel' }).click();
  await expect(page.getByTestId('delete-word')).toBeFocused();
  await page.getByTestId('delete-word').click();
  await page.getByTestId('delete-word-confirm').click();
  await expect(page).toHaveURL(/#\/words$/);
  await expect(page.getByTestId('notice')).toHaveText('Deleted “flumpet”.');
  await expect(page.getByTestId('word-count')).toHaveText('4 words');
  const rows = await app.api('GET', '/api/words?q=flumpet');
  expect(rows).toEqual([]);
});

test('R-74 a long word and its long texts wrap on the phone', async ({ page, app, pageErrors }) => {
  void pageErrors;
  const long = 'supercalifragilisticexpialidociouslyunbreakablewordwithoutanyspaces';
  await app.api('POST', '/api/import/commit', {
    text: JSON.stringify({
      ...bookAgain(),
      pack_id: 'test-e2e-novel--2026-11-04--1',
      words: [
        {
          headword: long,
          meaning: `${'Averyveryverylongmeaningwithoutanyspaceatall'.repeat(3)}.`,
          czech: ['dlouhéslovobezmezerdlouhéslovobezmezerdlouhéslovobezmezer'],
          usefulness: 3,
          sentences: [{ en: `A [[${long}]] here.`, cs: `Tady [[${long}]].`, origin: 'example' }],
        },
      ],
    }),
  });
  await page.goto(`${app.url}/#/words`);
  await expect(page.locator('a.word-row', { hasText: long })).toBeVisible();
  await expectNoSidewaysScroll(page);
  await openWord(page, app, long);
  await expectNoSidewaysScroll(page);
  await page.getByRole('button', { name: 'Edit meaning' }).click();
  await expectNoSidewaysScroll(page);
});
