// @ts-check
/**
 * Import page (design §7.1 screen 8, R-11; usability U2; owner feedback
 * F3): paste — or choose files (one .zip, or .json/.txt/.md files; drag and
 * drop on a desktop) — → Preview → Import, then the final report and
 * "Start practising"; recent imports below. The preview writes nothing;
 * Import sends the same text again and the server re-validates it. A file
 * selection larger than one paste goes in parts, one request per part.
 */

import { getJson, postJson } from '../api.js';
import { h, mount } from '../dom.js';
import { describePointer } from '../import-issues.js';
import { MAX_PASTE_BYTES, byteLength, readChosen, sizeLabel, splitIntoParts } from '../import-files.js';

/** Longer than the server's own import timeout (120 s), so its answer arrives. */
const IMPORT_TIMEOUT_MS = 130_000;

const INSTRUCTIONS_URL =
  'https://github.com/kadledav/vocabulary-training/blob/main/docs/vocabulary-export-instructions.txt';

/** @typedef {{ pointer: string, code: string, message: string }} Issue */
/**
 * @typedef {{ field: string, current: unknown, incoming: unknown, already_open: boolean }} Conflict
 * @typedef {{ czech: number, accepted: number, confusables: number, forms: number }} ItemsAdded
 * @typedef {{ outcome: string, filled: string[], conflicts: Conflict[], items_added: ItemsAdded,
 *             sentences_added: number, sentences_known: number, sentences_updated?: number,
 *             source_linked: boolean, tag_changed: boolean }} Merge
 * @typedef {{ index: number, headword: string | null, outcome: string, errors: Issue[], warnings: Issue[],
 *             sentence_errors: Issue[], merge: Merge | null, note: string | null }} WordReport
 * @typedef {{ words: number, new_words: number, merged_words: number, unchanged_words: number,
 *             invalid_words: number, new_sentences: number, words_with_new_sentences: number,
 *             updated_sentences?: number, conflicts: number, sentence_errors: number, warnings: number }} Summary
 * @typedef {{ position: number, pack_id: string | null, source: { id: string, type: string, title: string | null } | null,
 *             part: { index: number, total: number } | null, exported_at: string | null, status: string,
 *             already_imported_at: string | null, notes: string[], errors: Issue[], warnings: Issue[],
 *             reading_position: { value: string, updated: boolean, reason?: string | null } | null, summary: Summary,
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
  failed: 'failed — nothing of it imported',
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
  if ((s.updated_sentences ?? 0) > 0) parts.push(`${plural(s.updated_sentences ?? 0, 'sentence')} updated`);
  if (s.conflicts > 0) parts.push(plural(s.conflicts, 'conflict'));
  if (s.invalid_words > 0) parts.push(plural(s.invalid_words, 'word error'));
  if (s.sentence_errors > 0) parts.push(plural(s.sentence_errors, 'sentence error'));
  if (s.warnings > 0) parts.push(plural(s.warnings, 'warning'));
  return parts.join(' · ');
}

/**
 * Issues with their place in plain words (the headword, "sentence 1
 * (Czech)") instead of JSON pointers.
 *
 * @param {Issue[]} issues
 * @param {string} cls `error` (problems) or `warning` (advice)
 * @param {(index: number) => string | null} headwordAt
 * @param {boolean} withinWord listed under its word already
 */
function issueList(issues, cls, headwordAt, withinWord) {
  if (issues.length === 0) return null;
  return h(
    'ul',
    { class: `issues ${cls}` },
    issues.map((issue) => {
      const where = describePointer(issue.pointer, headwordAt, { withinWord });
      return h('li', {}, where ? h('span', { class: 'issue-where' }, `${where}: `) : null, issue.message);
    }),
  );
}

/** @param {unknown} value */
function show(value) {
  if (value === null || value === undefined) return '(empty)';
  return typeof value === 'string' ? value : JSON.stringify(value);
}

/**
 * @param {WordReport} word
 * @param {(index: number) => string | null} headwordAt
 */
function wordItem(word, headwordAt) {
  const merge = word.merge;
  const facts = [];
  if (merge) {
    if (merge.sentences_added > 0) facts.push(`+${plural(merge.sentences_added, 'sentence')}`);
    if ((merge.sentences_updated ?? 0) > 0) facts.push(`${plural(merge.sentences_updated ?? 0, 'sentence')} updated`);
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
    issueList(word.errors, 'error', headwordAt, true),
    issueList(word.sentence_errors, 'error', headwordAt, true),
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
      ? h(
          'details',
          {},
          h('summary', {}, plural(word.warnings.length, 'piece', 'pieces'), ' of advice'),
          issueList(word.warnings, 'warning', headwordAt, true),
        )
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
  /** @param {number} index */
  const headwordAt = (index) => pack.words.find((w) => w.index === index)?.headword ?? null;
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
          reading.updated ? '' : ` (not applied: ${reading.reason ?? 'an import with a newer export date already set it'})`,
        )
      : null,
    pack.already_imported_at ? h('p', { class: 'muted' }, `Imported on ${pack.already_imported_at}.`) : null,
    pack.notes.map((note) => h('p', { class: 'note' }, note)),
    pack.errors.length > 0 ? h('p', { class: 'issues-title error' }, 'Problems') : null,
    issueList(pack.errors, 'error', headwordAt, false),
    pack.words.length > 0 ? h('p', {}, summaryLine(pack.summary)) : null,
    pack.warnings.length > 0
      ? h(
          'details',
          {},
          h('summary', {}, `Advice for this pack (${pack.warnings.length})`),
          issueList(pack.warnings, 'warning', headwordAt, false),
        )
      : null,
    pack.words.length > 0 ? h('ul', { class: 'words' }, pack.words.map((w) => wordItem(w, headwordAt))) : null,
  );
}

/**
 * One report for several parts (a large file selection): the packs one
 * after the other, the totals added up.
 *
 * @param {ImportReport[]} reports
 * @returns {ImportReport}
 */
export function combineReports(reports) {
  const first = reports[0];
  if (reports.length === 1 && first) return first;
  /** @type {Summary} */
  const totals = {
    words: 0,
    new_words: 0,
    merged_words: 0,
    unchanged_words: 0,
    invalid_words: 0,
    new_sentences: 0,
    words_with_new_sentences: 0,
    updated_sentences: 0,
    conflicts: 0,
    sentence_errors: 0,
    warnings: 0,
  };
  /** @type {PackReport[]} */
  const packs = [];
  for (const report of reports) {
    for (const pack of report.packs) packs.push({ ...pack, position: packs.length + 1 });
    for (const key of /** @type {(keyof Summary)[]} */ (Object.keys(totals))) {
      totals[key] = (totals[key] ?? 0) + (report.totals[key] ?? 0);
    }
  }
  return { mode: first ? first.mode : 'preview', packs, totals };
}

/**
 * @param {ImportReport} report
 * @param {Node | null} afterReport the repeated Import button below a long preview
 */
function reportView(report, afterReport) {
  const heading = report.mode === 'preview' ? 'Preview — nothing imported yet' : 'Import finished';
  return h(
    'div',
    { class: 'report' },
    h('h2', {}, heading),
    h('p', { class: 'totals' }, summaryLine(report.totals)),
    report.packs.map(packCard),
    afterReport,
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
    rows: 10,
    spellcheck: 'false',
    autocomplete: 'off',
    autocapitalize: 'none',
    placeholder: 'Paste Claude’s answer here: one or more ```json blocks; text around them is ignored.',
    'aria-label': 'Packs to import',
  });
  const fileInput = h('input', {
    type: 'file',
    multiple: true,
    accept: '.zip,.json,.txt,.md,application/zip,application/json,text/plain,text/markdown',
    class: 'sr-only',
    tabindex: '-1',
    'aria-hidden': 'true',
    'data-testid': 'file-input',
  });
  const chooseButton = h('button', { type: 'button', 'data-testid': 'choose-files' }, 'Choose files');
  const filesLine = h('div', { class: 'files-line', 'data-testid': 'files' });
  const previewButton = h('button', { type: 'button', class: 'primary', 'data-testid': 'preview' }, 'Preview');
  const importButton = h('button', { type: 'button', disabled: true, 'data-testid': 'import' }, 'Import');
  const importAgain = h('button', { type: 'button', class: 'primary', 'data-testid': 'import-bottom' }, 'Import');
  const status = h('p', { class: 'status', role: 'status', 'data-testid': 'import-status' });
  const result = h('div', { class: 'result' });
  const recent = h('div', { class: 'recent-list' }, h('p', { class: 'muted' }, 'Loading…'));
  const dropZone = h('div', { class: 'drop-zone' }, textarea, h('p', { class: 'muted small drop-hint' }, 'You can also drop files here.'));

  /**
   * The text to send, in parts that each fit one paste: the paste box, or
   * the chosen files.
   *
   * @type {{ kind: 'paste' } | { kind: 'files', parts: string[], label: string }}
   */
  let source = { kind: 'paste' };
  /** What the last preview was made for (`null`: nothing previewable). */
  /** @type {string[] | null} */
  let previewed = null;
  let working = false;

  /** @returns {string[]} */
  const currentParts = () => (source.kind === 'files' ? source.parts : textarea.value.trim() === '' ? [] : [textarea.value]);
  const samePreview = () => {
    const parts = currentParts();
    return previewed !== null && previewed.length === parts.length && previewed.every((p, i) => p === parts[i]);
  };
  const refresh = () => {
    previewButton.disabled = working || currentParts().length === 0;
    importButton.disabled = working || !samePreview();
    importAgain.disabled = importButton.disabled;
    chooseButton.disabled = working;
    textarea.disabled = working;
  };

  const loadRecent = async () => {
    const answer = /** @type {import('../api.js').ApiResult<ImportLogRow[]>} */ (
      await getJson('/api/import/recent')
    );
    if (!isCurrent()) return;
    mount(recent, answer.ok ? recentView(answer.data) : h('p', { class: 'error' }, answer.message));
  };

  /** After a successful import: the box is cleared and the next step offered. */
  /** @param {ImportReport} report */
  const imported = (report) => {
    const words = report.totals.new_words + report.totals.merged_words;
    textarea.value = '';
    useFiles(null);
    mount(
      status,
      h(
        'span',
        { class: 'import-done', 'data-testid': 'import-done' },
        h('span', { class: 'done-line' }, words > 0 ? `${plural(words, 'word')} imported.` : 'Nothing new was imported.'),
        words > 0 ? h('a', { class: 'button primary big', href: '#/', 'data-testid': 'start-practising' }, 'Start practising') : null,
      ),
    );
  };

  /** @param {'preview' | 'commit'} mode */
  const run = async (mode) => {
    const parts = currentParts();
    if (parts.length === 0) return;
    if (source.kind === 'paste' && byteLength(parts[0] ?? '') > MAX_PASTE_BYTES) {
      status.textContent = 'The paste is larger than 2 MiB. Paste fewer packs at a time, or save them as files and choose those.';
      return;
    }
    working = true;
    if (mode === 'preview') previewed = null;
    refresh();
    /** @type {ImportReport[]} */
    const reports = [];
    /** @type {string | null} */
    let failure = null;
    for (const [i, text] of parts.entries()) {
      const where = parts.length > 1 ? ` part ${i + 1} of ${parts.length}` : '';
      status.textContent = mode === 'preview' ? `Checking${where}…` : `Importing${where}…`;
      const answer = /** @type {import('../api.js').ApiResult<ImportReport>} */ (
        // The server gives an import up to 120 s before it answers itself.
        await postJson(`/api/import/${mode}`, { text }, { timeoutMs: IMPORT_TIMEOUT_MS })
      );
      if (!isCurrent()) return;
      if (!answer.ok) {
        failure =
          parts.length > 1
            ? `Part ${i + 1} of ${parts.length} failed: ${answer.message}${mode === 'commit' && i > 0 ? ' The parts before it were imported; importing again is safe (packs already imported are skipped).' : ''}`
            : answer.message;
        break;
      }
      reports.push(answer.data);
    }
    working = false;
    if (reports.length > 0) {
      const report = combineReports(reports);
      const importable = report.packs.some((p) => p.status === 'ready');
      const long = mode === 'preview' && importable && report.packs.reduce((n, p) => n + p.words.length, 0) > 5;
      mount(result, reportView(report, long ? h('div', { class: 'actions' }, importAgain) : null), failure ? h('p', { class: 'error' }, failure) : null);
      if (mode === 'preview') {
        previewed = importable && !failure ? parts : null;
        status.textContent = failure ? '' : importable ? 'Check the preview, then press Import.' : 'Nothing to import here.';
      } else {
        previewed = null;
        if (failure) status.textContent = '';
        else imported(report);
        void loadRecent();
      }
    } else {
      status.textContent = '';
      mount(result, h('p', { class: 'error' }, failure ?? 'Nothing was checked.'));
    }
    refresh();
  };

  /**
   * Use the chosen files (or the paste box again: `null`).
   *
   * @param {{ parts: string[], label: string } | null} files
   */
  const useFiles = (files) => {
    source = files ? { kind: 'files', ...files } : { kind: 'paste' };
    previewed = null;
    if (!files) {
      mount(filesLine);
    } else {
      const clear = h('button', { type: 'button', class: 'link-button', 'data-testid': 'clear-files' }, 'Clear');
      clear.addEventListener('click', () => {
        useFiles(null);
        mount(result);
        status.textContent = '';
      });
      mount(filesLine, h('p', {}, files.label), clear);
    }
    refresh();
  };

  /** @param {File[]} files */
  const choose = async (files) => {
    if (files.length === 0) return;
    working = true;
    refresh();
    status.textContent = 'Reading the files…';
    mount(result);
    const chosen = await Promise.all(files.map(async (f) => ({ name: f.name, bytes: new Uint8Array(await f.arrayBuffer()) })));
    const read = await readChosen(chosen);
    if (!isCurrent()) return;
    working = false;
    const problems = read.problems.map((p) => h('li', {}, h('span', { class: 'issue-where' }, `${p.name}: `), p.reason));
    if (read.fatal) {
      useFiles(null);
      status.textContent = read.fatal;
      return;
    }
    const { parts, tooLarge } = splitIntoParts(read.pieces);
    for (const name of tooLarge) problems.push(h('li', {}, h('span', { class: 'issue-where' }, `${name}: `), 'larger than one import (2 MiB) — split it'));
    if (parts.length === 0) {
      useFiles(null);
      status.textContent = 'Nothing in the chosen files can be imported.';
      mount(result, problems.length > 0 ? h('ul', { class: 'issues error' }, problems) : null);
      return;
    }
    const bytes = parts.reduce((n, p) => n + byteLength(p), 0);
    const names = read.pieces.map((p) => p.name);
    const label =
      `${plural(read.pieces.length, 'file')} chosen (${sizeLabel(bytes)}): ${names.slice(0, 5).join(', ')}` +
      (names.length > 5 ? ` and ${names.length - 5} more` : '') +
      (parts.length > 1 ? `. Larger than one import, so it goes in ${parts.length} parts.` : '.');
    textarea.value = '';
    useFiles({ parts, label });
    status.textContent = 'Press Preview to check the files.';
    if (problems.length > 0) mount(result, h('p', { class: 'issues-title error' }, 'Not read'), h('ul', { class: 'issues error' }, problems));
  };

  textarea.addEventListener('input', () => {
    if (source.kind === 'files' && textarea.value !== '') useFiles(null);
    refresh();
  });
  chooseButton.addEventListener('click', () => fileInput.click());
  fileInput.addEventListener('change', () => {
    const files = [...(fileInput.files ?? [])];
    fileInput.value = '';
    void choose(files);
  });
  dropZone.addEventListener('dragover', (event) => {
    if (!event.dataTransfer || ![...event.dataTransfer.types].includes('Files')) return;
    event.preventDefault();
    dropZone.classList.add('dragging');
  });
  dropZone.addEventListener('dragleave', () => dropZone.classList.remove('dragging'));
  dropZone.addEventListener('drop', (event) => {
    dropZone.classList.remove('dragging');
    const files = [...(event.dataTransfer?.files ?? [])];
    if (files.length === 0) return;
    event.preventDefault();
    if (!working) void choose(files);
  });
  previewButton.addEventListener('click', () => void run('preview'));
  importButton.addEventListener('click', () => void run('commit'));
  importAgain.addEventListener('click', () => void run('commit'));

  mount(
    root,
    h('h1', {}, 'Import'),
    h(
      'p',
      { class: 'muted' },
      'Paste the packs Claude exported, or choose the files: one .zip, or .json, .txt or .md files. The preview shows what would change; nothing is stored until you press Import. Importing the same pack twice does nothing.',
    ),
    h('p', {}, h('a', { class: 'tap-link', href: INSTRUCTIONS_URL, target: '_blank', rel: 'noopener noreferrer' }, 'Export instructions for Claude')),
    dropZone,
    h('div', { class: 'actions wrap' }, chooseButton, fileInput),
    filesLine,
    h('div', { class: 'actions' }, previewButton, importButton),
    status,
    result,
    h('h2', {}, 'Recent imports'),
    recent,
  );
  refresh();
  void loadRecent();
}
