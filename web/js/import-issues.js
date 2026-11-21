// @ts-check
/**
 * Import report wording (usability U2): an issue's JSON pointer
 * (`/words/3/sentences/0/cs`) becomes the place in plain words — the
 * headword and "sentence 1 (Czech)" — and pack totals become one line.
 */

/**
 * @typedef {{ pointer: string, code: string, message: string }} Issue
 */

const FIELD_NAMES = /** @type {Record<string, string>} */ ({
  en: 'English',
  cs: 'Czech',
  czech: 'Czech',
  headword: 'headword',
  meaning: 'meaning',
  note: 'note',
  forms: 'forms',
  accepted: 'accepted answers',
  confusables: 'confusables',
  pronunciation: 'pronunciation',
  source: 'source',
  reading_position: 'reading position',
  exported_at: 'export date',
  pack_id: 'pack id',
});

/** @param {string} segment */
function decodeSegment(segment) {
  return segment.replace(/~1/g, '/').replace(/~0/g, '~');
}

/** @param {string} name */
function fieldName(name) {
  return FIELD_NAMES[name] ?? name.replace(/_/g, ' ');
}

/**
 * Where an issue is, in plain words; empty when it concerns the whole pack.
 *
 * @param {string} pointer JSON pointer into the pack
 * @param {(index: number) => string | null} headwordAt headword of word `index` in the pack
 * @param {{ withinWord?: boolean }} [options] the issue is listed under its word already
 */
export function describePointer(pointer, headwordAt, { withinWord = false } = {}) {
  if (!pointer || pointer === '/') return '';
  const segments = pointer.split('/').slice(1).map(decodeSegment);
  /** @type {string[]} */
  const parts = [];
  let i = 0;
  if (segments[0] === 'words' && segments.length > 1 && /^[0-9]+$/.test(segments[1])) {
    const index = Number(segments[1]);
    if (!withinWord) parts.push(headwordAt(index) ?? `word ${index + 1}`);
    i = 2;
  }
  while (i < segments.length) {
    const name = segments[i];
    const next = segments[i + 1];
    if (next !== undefined && /^[0-9]+$/.test(next)) {
      const singular = name === 'sentences' ? 'sentence' : name === 'forms' ? 'form' : fieldName(name);
      parts.push(`${singular} ${Number(next) + 1}`);
      i += 2;
    } else if (/^[0-9]+$/.test(name)) {
      parts.push(`item ${Number(name) + 1}`);
      i += 1;
    } else {
      parts.push(fieldName(name));
      i += 1;
    }
  }
  if (parts.length === 0) return '';
  // "glimmerous, sentence 1, Czech" reads better as "glimmerous, sentence 1 (Czech)".
  const last = parts[parts.length - 1];
  if (parts.length >= 2 && (last === 'English' || last === 'Czech') && parts[parts.length - 2].startsWith('sentence ')) {
    parts.splice(-2, 2, `${parts[parts.length - 2]} (${last})`);
  }
  return parts.join(', ');
}
