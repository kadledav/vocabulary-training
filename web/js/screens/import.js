// @ts-check
/**
 * Import page (design §7.1 screen 8, R-11): paste → Preview → Import, then
 * the final report; recent imports below. The preview writes nothing; Import
 * sends the same text again and the server re-validates it.
 */

import { getJson, postJson } from '../api.js';
import { h, mount } from '../dom.js';

const INSTRUCTIONS_URL =
  'https://github.com/kadledav/vocabulary-training/blob/main/docs/claude-project-instructions.txt';

/** @typedef {{ pointer: string, code: string, message: string }} Issue */
/**
 * @typedef {{ field: string, current: unknown, incoming: unknown, already_open: boolean }} Conflict
 * @typedef {{ czech: number, accepted: number, confusables: number, forms: number }} ItemsAdded
 * @typedef {{ outcome: string, filled: string[], conflicts: Conflict[], items_added: ItemsAdded,
 *             sentences_added: number, sentences_known: number, source_linked: boolean, tag_changed: boolean }} Merge
 * @typedef {{ index: number, headword: string | null, outcome: string, errors: Issue[], warnings: Issue[],
 *             sentence_errors: Issue[], merge: Merge | null, note: string | null }} WordReport
 * @typedef {{ words: number, new_words: number, merged_words: number, unchanged_words: number,
 *             invalid_words: number, new_sentences: number, words_with_new_sentences: number,
 *             conflicts: number, sentence_errors: number, warnings: number }} Summary
 * @typedef {{ position: number, pack_id: string | null, source: { id: string, type: string, title: string | null } | null,
 *             part: { index: number, total: number } | null, exported_at: string | null, status: string,
 *             already_imported_at: string | null, notes: string[], errors: Issue[], warnings: Issue[],
 *             reading_position: { value: string, updated: boolean } | null, summary: Summary,
 *             words: WordReport[] }} PackReport
 * @typedef {{ mode: 'preview' | 'commit', packs: PackReport[], totals: Summary }} ImportReport
 * @typedef {{ id: number, pack_id: string, source_slug: string, source_title: string | null,
 *             exported_at: string | null, part: { index: number, total: number }, via: string,
 *             imported_at: string, summary: Summary | null }} ImportLogRow
 */

/** @param {{ id: string, type: string, title: string | null }} source */
function sourceLabel(source) {
  return `${source.title ?? source.id} (${source.type})`;
}

const STATUS_LABELS = /** @type {Record<string, string>} */ ({
  ready: 'ready to import',
  imported: 'imported',
  already_imported: 'already imported — nothing to do',
  rejected: 'rejected',
  unreadable: 'not readable',
});

const OUTCOME_LABELS = /** @type {Record<string, string>} */ ({
  new: 'new',
  merged: 'merged',
  unchanged: 'unchanged',
  invalid: 'error — skipped',
  not_imported: 'not imported',
});

/**
 * @param {number} n
 * @param {string} one
 * @param {string} [many]
 */
function plural(n, one, many) {
  return `${n} ${n === 1 ? one : many ?? `${one}s`}`;
}

/** @param {Summary} s */
function summaryLine(s) {
  const parts = [];
  if (s.words_with_new_sentences > 0 && s.new_words === 0) {
    parts.push(
      `${plural(s.new_sentences, 'new sentence')} for ${plural(s.words_with_new_sentences, 'word')}`,
    );
  }
  parts.push(plural(s.new_words, 'new word'));
  parts.push(`${s.merged_words} merged`);
  parts.push(`${s.unchanged_words} unchanged`);
  if (!(s.words_with_new_sentences > 0 && s.new_words === 0)) {
    parts.push(plural(s.new_sentences, 'new sentence'));
  }
  if (s.conflicts > 0) parts.push(plural(s.conflicts, 'conflict'));
  if (s.invalid_words > 0) parts.push(plural(s.invalid_words, 'word error'));
  if (s.sentence_errors > 0) parts.push(plural(s.sentence_errors, 'sentence error'));
  if (s.warnings > 0) parts.push(plural(s.warnings, 'warning'));
  return parts.join(' · ');
}

/**
 * @param {Issue[]} issues
 * @param {string} cls
 */
function issueList(issues, cls) {
  if (issues.length === 0) return null;
  return h(
    'ul',
    { class: `issues ${cls}` },
    issues.map((issue) =>
      h('li', {}, issue.pointer ? h('code', {}, issue.pointer) : null, issue.pointer ? ' ' : null, issue.message),
    ),
  );
}

/** @param {unknown} value */
function show(value) {
  if (value === null || value === undefined) return '(empty)';
  return typeof value === 'string' ? value : JSON.stringify(value);
}

/** @param {WordReport} word */
function wordItem(word) {
  const merge = word.merge;
  const facts = [];
  if (merge) {
    if (merge.sentences_added > 0) facts.push(`+${plural(merge.sentences_added, 'sentence')}`);
    if (merge.sentences_known > 0) facts.push(`${merge.sentences_known} already known`);
    const items = merge.items_added;
    const added = items.czech + items.accepted + items.confusables + items.forms;
    if (word.outcome !== 'new' && added > 0) facts.push(`+${plural(added, 'list item')}`);
    if (merge.filled.length > 0) facts.push(`filled: ${merge.filled.join(', ')}`);
    if (merge.source_linked && word.outcome !== 'new') facts.push('linked to this source');
    if (merge.tag_changed) facts.push('tag changed');
  }
  const conflicts = merge ? merge.conflicts : [];
  return h(
    'li',
    { class: `word outcome-${word.outcome}` },
    h(
      'div',
      { class: 'word-head' },
      h('span', { class: 'headword', lang: 'en' }, word.headword ?? `word ${word.index + 1}`),
      ' ',
      h('span', { class: `badge outcome-${word.outcome}` }, OUTCOME_LABELS[word.outcome] ?? word.outcome),
      facts.length > 0 ? h('span', { class: 'muted' }, ` ${facts.join(' · ')}`) : null,
    ),
    word.note ? h('p', { class: 'muted small' }, word.note) : null,
    issueList(word.errors, 'error'),
    issueList(word.sentence_errors, 'error'),
    conflicts.length > 0
      ? h(
          'details',
          { class: 'conflicts' },
          h('summary', {}, `${plural(conflicts.length, 'field')} differ — existing values kept`),
          h(
            'ul',
            {},
            conflicts.map((c) =>
              h(
                'li',
                {},
                h('code', {}, c.field),
                `: kept “${show(c.current)}”; the pack suggests “${show(c.incoming)}”`,
                c.already_open ? ' (already waiting for review)' : '',
              ),
            ),
          ),
        )
      : null,
    word.warnings.length > 0
      ? h('details', {}, h('summary', {}, plural(word.warnings.length, 'warning')), issueList(word.warnings, 'warning'))
      : null,
  );
}

/** @param {PackReport} pack */
function packCard(pack) {
  const title = pack.pack_id ?? `Pack ${pack.position}`;
  const source = pack.source;
  const meta = [];
  if (source) meta.push(sourceLabel(source));
  if (pack.part && pack.part.total > 1) meta.push(`part ${pack.part.index} of ${pack.part.total}`);
  if (pack.exported_at) meta.push(`exported ${pack.exported_at}`);
  const reading = pack.reading_position;
  return h(
    'section',
    { class: `card pack status-${pack.status}` },
    h(
      'h3',
      {},
      `${pack.position}. `,
      h('span', { class: 'pack-id' }, title),
      ' ',
      h('span', { class: `badge status-${pack.status}` }, STATUS_LABELS[pack.status] ?? pack.status),
    ),
    meta.length > 0 ? h('p', { class: 'muted' }, meta.join(' · ')) : null,
    reading
      ? h(
          'p',
          { class: 'muted' },
          `Reading position: ${reading.value}`,
          reading.updated ? '' : ' (an import with a newer export date already set it)',
        )
      : null,
    pack.already_imported_at ? h('p', { class: 'muted' }, `Imported on ${pack.already_imported_at}.`) : null,
    pack.notes.map((note) => h('p', { class: 'note' }, note)),
    issueList(pack.errors, 'error'),
    pack.words.length > 0 ? h('p', {}, summaryLine(pack.summary)) : null,
    pack.warnings.length > 0
      ? h('details', {}, h('summary', {}, plural(pack.warnings.length, 'pack warning')), issueList(pack.warnings, 'warning'))
      : null,
    pack.words.length > 0 ? h('ul', { class: 'words' }, pack.words.map(wordItem)) : null,
  );
}

/** @param {ImportReport} report */
function reportView(report) {
  const heading = report.mode === 'preview' ? 'Preview — nothing imported yet' : 'Import finished';
  return h(
    'div',
    { class: 'report' },
    h('h2', {}, heading),
    h('p', { class: 'totals' }, summaryLine(report.totals)),
    report.packs.map(packCard),
  );
}

/** @param {ImportLogRow[]} rows */
function recentView(rows) {
  if (rows.length === 0) return h('p', { class: 'muted' }, 'Nothing imported yet.');
  return h(
    'ul',
    { class: 'recent' },
    rows.map((row) =>
      h(
        'li',
        {},
        h('span', { class: 'pack-id' }, row.pack_id),
        row.part.total > 1 ? ` (part ${row.part.index}/${row.part.total})` : '',
        ` — ${row.source_title ?? row.source_slug} · ${row.imported_at}`,
        row.summary ? h('span', { class: 'muted' }, ` · ${summaryLine(row.summary)}`) : null,
      ),
    ),
  );
}

/**
 * @param {HTMLElement} root
 * @param {import('../router.js').IsCurrent} isCurrent
 */
export function renderImport(root, isCurrent) {
  const textarea = h('textarea', {
    id: 'paste',
    rows: 14,
    spellcheck: 'false',
    autocomplete: 'off',
    autocapitalize: 'none',
    placeholder: 'Paste Claude’s answer here: one or more ```json blocks; text around them is ignored.',
    'aria-label': 'Packs to import',
  });
  const previewButton = h('button', { type: 'button', class: 'primary' }, 'Preview');
  const importButton = h('button', { type: 'button', disabled: true }, 'Import');
  const status = h('p', { class: 'status', role: 'status' });
  const result = h('div', { class: 'result' });
  const recent = h('div', { class: 'recent-list' }, h('p', { class: 'muted' }, 'Loading…'));
  /** The text the last preview was made for. */
  let previewed = '';

  const busy = (/** @type {boolean} */ on) => {
    previewButton.disabled = on;
    importButton.disabled = on || previewed === '' || previewed !== textarea.value;
  };

  const loadRecent = async () => {
    const answer = /** @type {import('../api.js').ApiResult<ImportLogRow[]>} */ (
      await getJson('/api/import/recent')
    );
    if (!isCurrent()) return;
    mount(recent, answer.ok ? recentView(answer.data) : h('p', { class: 'error' }, answer.message));
  };

  /** @param {'preview' | 'commit'} mode */
  const run = async (mode) => {
    const text = textarea.value;
    status.textContent = mode === 'preview' ? 'Checking…' : 'Importing…';
    previewed = mode === 'preview' ? '' : previewed;
    busy(true);
    const answer = /** @type {import('../api.js').ApiResult<ImportReport>} */ (
      await postJson(`/api/import/${mode}`, { text })
    );
    if (!isCurrent()) return;
    if (answer.ok) {
      const report = answer.data;
      mount(result, reportView(report));
      const importable = report.packs.some((p) => p.status === 'ready');
      if (mode === 'preview') {
        previewed = importable ? text : '';
        status.textContent = importable
          ? 'Check the preview, then press Import.'
          : 'Nothing to import in this paste.';
      } else {
        previewed = '';
        status.textContent = 'Done.';
        void loadRecent();
      }
    } else {
      status.textContent = '';
      mount(result, h('p', { class: 'error' }, answer.message));
    }
    busy(false);
  };

  textarea.addEventListener('input', () => busy(false));
  previewButton.addEventListener('click', () => void run('preview'));
  importButton.addEventListener('click', () => void run('commit'));

  mount(
    root,
    h('h1', {}, 'Import'),
    h(
      'p',
      { class: 'muted' },
      'Paste the packs Claude exported for the training app. The preview shows what would change; nothing is stored until you press Import. Importing the same pack twice does nothing. ',
      h('a', { href: INSTRUCTIONS_URL, target: '_blank', rel: 'noopener noreferrer' }, 'Export instructions for Claude'),
      '.',
    ),
    textarea,
    h('div', { class: 'actions' }, previewButton, importButton),
    status,
    result,
    h('h2', {}, 'Recent imports'),
    recent,
  );
  void loadRecent();
}
