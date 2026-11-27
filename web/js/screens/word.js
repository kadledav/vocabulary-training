// @ts-check
/**
 * One word (design §7.1 screen 7; R-03, R-04, R-25, R-51, R-52, R-72
 * per-word history): everything stored about it, and every change the
 * owner can make:
 *
 * - pack suggestions (conflicts) with *Use new* / *Keep mine*;
 * - usefulness −/+, goal (automatic / recognise / produce), include a
 *   low-priority word, suspend, spelling practice;
 * - every text field (Edit → Save / Cancel), the Czech equivalents,
 *   accepted answers, confusables and forms (add, remove);
 * - sentences: correct (with the `[[…]]` marks) or hide;
 * - per card: state, next due day, recall chance, "Reset card"; the review
 *   history; "Delete word".
 *
 * Every change is saved at once and confirmed by a toast with Undo (which
 * sends the opposite change); a text being edited can be cancelled. A
 * failed save says why in plain words next to the control and keeps what
 * was typed. Resetting a card and deleting the word ask first and say what
 * is lost. Pack text is rendered as text only (R-84).
 */

import { deleteJson, getJson, patchJson, postJson } from '../api.js';
import { forgetClips } from '../clips.js';
import { h, mount } from '../dom.js';
import { displayIcon } from '../icon.js';
import { markedText } from '../marked.js';
import { setNotice } from '../notice.js';
import { isGrade, rating } from '../rating.js';
import { Toast } from '../toast.js';
import {
  FIELDS,
  cardKindLabel,
  cardStateLabel,
  fieldLabel,
  goalChoices,
  goalValue,
  hasOneMark,
  markedSource,
  percent,
  plural,
  showValue,
  statusLabel,
} from '../words-model.js';

/**
 * @typedef {import('../marked.js').TextParts} TextParts
 * @typedef {{ value: string, key: string, mine: boolean }} Item
 * @typedef {{ form: string, key: string, common: boolean }} FormItem
 * @typedef {{ id: number, en: TextParts, cs: TextParts, origin: string, location: string | null,
 *             source_title: string | null, hidden: boolean, edited: boolean }} Sentence
 * @typedef {{ id: number, field: string, current: unknown, now: unknown, incoming: unknown, created_at: string }} Conflict
 * @typedef {{ id: number, kind: string, format: string | null, state: string, stability: number | null,
 *             due_day: string | null, reps: number, lapses: number, leech: string, suspended: boolean,
 *             recall: number | null, desired_retention: number }} CardView
 * @typedef {{ review_id: number, kind: string, review_day: string, raw_grade: number,
 *             typed_answer: string | null, hint_used: boolean, sentence_id: number | null }} HistoryRow
 * @typedef {{ id: number, headword: string, grammar: string | null, pronunciation: string | null,
 *             speak: string | null, meaning: string, usefulness: number, goal: string | null,
 *             effective_goal: string, hint: string | null, spelling: boolean, spelling_note: string | null,
 *             note: string, icon: string | null, low_priority: boolean, included: boolean, suspended: boolean,
 *             overridden: string[], status: string,
 *             items: { czech: Item[], accepted: Item[], confusables: Item[], forms: FormItem[] },
 *             sources: { source_id: number, title: string, seq: number, tag_kind: string | null }[],
 *             sentences: Sentence[], conflicts: Conflict[], cards: CardView[], history: HistoryRow[] }} WordDetail
 * @typedef {import('../api.js').ApiResult<WordDetail>} DetailAnswer
 */

/** How long the "Saved — Undo" toast stays. */
const TOAST_MS = 6000;

/** The list fields: API kind, heading, what is added, the detail's list. */
const LISTS = /** @type {const} */ ([
  ['czech', 'Czech equivalents', 'Czech equivalent', 'czech'],
  ['accepted', 'Accepted answers (Produce)', 'accepted answer', 'accepted'],
  ['confusable', 'Easily confused with', 'confusable word', 'confusables'],
  ['form', 'Forms', 'form', 'forms'],
]);

/**
 * A slot holding a button that, when pressed, asks first: the message says
 * what is lost; *Cancel* puts the button back.
 *
 * @param {{ label: string, testid: string, message: string, confirm: string,
 *           onConfirm: (problem: HTMLElement) => Promise<void> }} spec
 */
function confirmed({ label, testid, message, confirm, onConfirm }) {
  const slot = h('div', { class: 'confirm-slot' });
  const open = h('button', { type: 'button', class: 'danger', 'data-testid': testid }, label);
  const closed = () => {
    mount(slot, open);
    open.focus();
  };
  open.addEventListener('click', () => {
    const problem = h('p', { class: 'field-error', role: 'alert' });
    const yes = h('button', { type: 'button', class: 'danger primary', 'data-testid': `${testid}-confirm` }, confirm);
    const no = h('button', { type: 'button', onclick: closed }, 'Cancel');
    yes.addEventListener('click', async () => {
      yes.disabled = true;
      await onConfirm(problem);
      yes.disabled = false;
    });
    mount(slot, h('div', { class: 'confirm', role: 'group', 'aria-label': label }, h('p', {}, message), h('div', { class: 'actions' }, yes, no), problem));
    no.focus();
  });
  mount(slot, open);
  return slot;
}

/**
 * @param {HTMLElement} root
 * @param {string} id
 * @param {import('../router.js').IsCurrent} isCurrent
 */
export async function renderWord(root, id, isCurrent) {
  const back = () => h('p', {}, h('a', { class: 'back-link', href: '#/words' }, '← Words'));
  mount(root, back(), h('p', { class: 'muted' }, 'Loading…'));
  const first = /** @type {DetailAnswer} */ (await getJson(`/api/words/${id}`));
  if (!isCurrent()) return;
  if (!first.ok) {
    mount(root, back(), h('p', { class: 'error', role: 'alert' }, first.status === 404 ? 'This word does not exist (any more).' : first.message));
    return;
  }
  let word = first.data;
  const body = h('div', { class: 'word-detail' });
  const toastSlot = h('div', { class: 'toast-slot word-toast', 'data-testid': 'word-toast' });
  const toast = new Toast(toastSlot);
  mount(root, body, toastSlot);
  /** One change at a time: a second tap while saving is not sent. */
  let saving = false;

  /**
   * Redraw everything from `word`; then focus the element with `focusId`
   * (the control that was used), keeping the place on the page.
   *
   * @param {string} [focusId]
   */
  const draw = (focusId) => {
    mount(body, ...view());
    if (focusId) document.getElementById(focusId)?.focus({ preventScroll: true });
  };

  /**
   * Send one change. On success the word is redrawn from the answer and a
   * toast confirms it (with Undo when `undo` is given); on failure the
   * reason is returned (and nothing on the page is touched).
   *
   * @param {() => Promise<DetailAnswer>} request
   * @param {{ done: string, focusId?: string, spoken?: boolean,
   *           undo?: ((before: WordDetail, after: WordDetail) => (() => Promise<DetailAnswer>) | null) }} options
   * @returns {Promise<string | null>}
   */
  const change = async (request, { done, focusId, spoken = false, undo }) => {
    if (saving) return 'Another change is being saved. Try again in a moment.';
    saving = true;
    let answer;
    try {
      answer = await request();
    } finally {
      saving = false;
    }
    if (!isCurrent()) return null;
    if (!answer.ok) return answer.message;
    const before = word;
    word = answer.data;
    // New text, new clips: never the old clip for the new text (M6).
    if (spoken) forgetClips(word.id);
    draw(focusId);
    const reverse = undo ? undo(before, word) : null;
    toast.show({
      text: done,
      ms: TOAST_MS,
      onUndo: reverse
        ? async () => {
            const problem = await change(reverse, { done: 'Undone.', focusId, spoken });
            if (problem) toast.show({ text: `Could not undo: ${problem}`, ms: TOAST_MS });
          }
        : undefined,
    });
    return null;
  };

  const url = `/api/words/${word.id}`;
  /** @param {Record<string, unknown>} fields */
  const patch = (fields) => () => /** @type {Promise<DetailAnswer>} */ (patchJson(url, fields));
  /** @param {Record<string, unknown>} body */
  const action = (body) => () => /** @type {Promise<DetailAnswer>} */ (postJson(`${url}/actions`, body));

  /**
   * Save one field; Undo puts the old value back.
   *
   * @param {string} field
   * @param {unknown} value
   * @param {string} focusId
   */
  const saveField = (field, value, focusId) =>
    change(patch({ [field]: value }), {
      done: `Saved: ${fieldLabel(field)}.`,
      focusId,
      spoken: field === 'headword' || field === 'speak',
      undo: (before) => patch({ [field]: /** @type {Record<string, unknown>} */ (before)[field] ?? null }),
    });

  // ── header and suggestions ──────────────────────────────────────────────

  const header = () => {
    const icon = displayIcon(word.icon);
    const badges = [statusLabel(word.status)];
    if (word.suspended) badges.push('suspended');
    if (word.low_priority) badges.push(word.included ? 'low priority, included' : 'low priority');
    return [
      back(),
      h('h1', { lang: 'en', class: 'word-head', 'data-testid': 'word-headword' }, icon ? `${icon} ` : '', word.headword),
      word.pronunciation ? h('p', { class: 'word-pron' }, word.pronunciation) : null,
      h('p', { class: 'meaning', lang: 'en' }, word.meaning),
      h('p', { class: 'small', 'data-testid': 'word-status' }, badges.map((b) => h('span', { class: 'badge' }, b))),
      word.sources.length > 0
        ? h('p', { class: 'muted small' }, 'From ', word.sources.map((s) => `${s.title} #${s.seq}`).join('; '))
        : null,
    ];
  };

  /** @param {unknown} value */
  const valueText = (value) => h('span', { class: 'value-text' }, showValue(value));

  const suggestions = () => {
    if (word.conflicts.length === 0) return null;
    return h(
      'section',
      { class: 'card conflicts', 'data-testid': 'conflicts' },
      h('h2', {}, `Suggestions from later packs (${word.conflicts.length})`),
      h('p', { class: 'muted small' }, 'A later pack brought a different value. Yours stays until you choose.'),
      h(
        'ul',
        { class: 'conflict-list' },
        word.conflicts.map((c) => {
          const problem = h('p', { class: 'field-error', role: 'alert' });
          const answer = (/** @type {'incoming' | 'keep'} */ use) => async () => {
            const failed = await change(() => /** @type {Promise<DetailAnswer>} */ (postJson(`/api/conflicts/${c.id}`, { use })), {
              done: use === 'incoming' ? `Took the pack’s ${fieldLabel(c.field).toLowerCase()}.` : `Kept your ${fieldLabel(c.field).toLowerCase()}.`,
              spoken: use === 'incoming' && c.field === 'speak',
              undo: use === 'incoming' ? () => patch({ [c.field]: c.now ?? null }) : undefined,
            });
            if (failed) problem.textContent = failed;
          };
          return h(
            'li',
            { 'data-testid': `conflict-${c.field}` },
            h('h3', {}, fieldLabel(c.field)),
            h('p', {}, h('span', { class: 'muted' }, 'Now: '), valueText(c.now)),
            h('p', {}, h('span', { class: 'muted' }, 'Pack suggests: '), valueText(c.incoming)),
            h(
              'div',
              { class: 'actions' },
              h('button', { type: 'button', class: 'primary', onclick: answer('incoming') }, 'Use new'),
              h('button', { type: 'button', onclick: answer('keep') }, 'Keep mine'),
            ),
            problem,
          );
        }),
      ),
    );
  };

  // ── practice settings ────────────────────────────────────────────────────

  const practice = () => {
    const problem = h('p', { class: 'field-error', role: 'alert', 'data-testid': 'practice-problem' });
    /**
     * @param {() => Promise<DetailAnswer>} request
     * @param {Parameters<typeof change>[1]} options
     * @param {HTMLInputElement} [toggle] put back when the save fails
     */
    const run = async (request, options, toggle) => {
      problem.textContent = '';
      const failed = await change(request, options);
      if (failed) {
        problem.textContent = failed;
        if (toggle) toggle.checked = !toggle.checked;
      }
    };
    const step = (/** @type {number} */ delta) => {
      const next = word.usefulness + delta;
      return h(
        'button',
        {
          type: 'button',
          class: 'step',
          id: delta < 0 ? 'usefulness-down' : 'usefulness-up',
          'aria-label': delta < 0 ? 'Lower usefulness' : 'Raise usefulness',
          disabled: next < 1 || next > 10,
          onclick: () => run(patch({ usefulness: next }), {
            done: `Usefulness ${next}.`,
            focusId: delta < 0 ? 'usefulness-down' : 'usefulness-up',
            undo: (before) => patch({ usefulness: before.usefulness }),
          }),
        },
        delta < 0 ? '−' : '+',
      );
    };
    /**
     * A switch saved at once.
     *
     * @param {string} idName
     * @param {string} label
     * @param {boolean} checked
     * @param {(on: boolean) => () => Promise<DetailAnswer>} request
     * @param {(on: boolean) => string} done
     * @param {(on: boolean) => () => Promise<DetailAnswer>} reverse
     */
    const toggle = (idName, label, checked, request, done, reverse) => {
      const input = h('input', { type: 'checkbox', id: idName, checked });
      input.addEventListener('change', () =>
        run(request(input.checked), { done: done(input.checked), focusId: idName, undo: () => reverse(input.checked) }, input),
      );
      return h('label', { class: 'setting check' }, input, ' ', label);
    };
    const goal = word.goal ?? 'auto';
    return h(
      'section',
      { class: 'word-section' },
      h('h2', {}, 'Practice'),
      h(
        'div',
        { class: 'stepper', role: 'group', 'aria-label': 'Usefulness' },
        h('span', { class: 'stepper-label' }, 'Usefulness'),
        step(-1),
        h('output', { class: 'stepper-value', 'data-testid': 'usefulness', 'aria-live': 'polite' }, String(word.usefulness)),
        step(1),
      ),
      h('p', { class: 'muted small' }, 'Useful words come first and are kept fresher (1–10).'),
      h(
        'fieldset',
        { class: 'choices' },
        h('legend', {}, 'Goal'),
        goalChoices(word.usefulness).map(([value, label]) =>
          h(
            'label',
            { class: 'choice-option' },
            h('input', {
              type: 'radio',
              name: 'goal',
              id: `goal-${value}`,
              value,
              checked: value === goal,
              onchange: () =>
                run(patch({ goal: goalValue(value) }), {
                  done: 'Goal saved.',
                  focusId: `goal-${value}`,
                  undo: (before) => patch({ goal: before.goal }),
                }),
            }),
            ' ',
            label,
          ),
        ),
      ),
      word.low_priority
        ? toggle(
            'include',
            'Practise this low-priority word (recognise only)',
            word.included,
            (on) => action({ action: on ? 'include' : 'exclude' }),
            (on) => (on ? 'Included in practice.' : 'Not practised any more.'),
            (on) => action({ action: on ? 'exclude' : 'include' }),
          )
        : null,
      toggle(
        'suspend',
        'Suspended (not practised)',
        word.suspended,
        (on) => action({ action: on ? 'suspend' : 'unsuspend' }),
        (on) => (on ? 'Suspended.' : 'Practised again.'),
        (on) => action({ action: on ? 'unsuspend' : 'suspend' }),
      ),
      toggle(
        'spelling',
        'Spelling practice',
        word.spelling,
        (on) => patch({ spelling: on }),
        (on) => (on ? 'Spelling practice on.' : 'Spelling practice off.'),
        (on) => patch({ spelling: !on }),
      ),
      problem,
    );
  };

  // ── text fields ──────────────────────────────────────────────────────────

  /** @param {string} field */
  const fieldRow = (field) => {
    const spec = FIELDS[field];
    const value = /** @type {Record<string, unknown>} */ (word)[field];
    const row = h('div', { class: 'field edit-row', 'data-testid': `field-${field}` });
    const editId = `edit-${field}`;
    const shown = () => {
      mount(
        row,
        h('dt', {}, spec.label, word.overridden.includes(field) ? h('span', { class: 'badge' }, 'edited') : null),
        h(
          'dd',
          {},
          h('span', { class: 'value-text', lang: field === 'note' || field === 'meaning' ? 'en' : null }, showValue(value)),
          h('button', { type: 'button', class: 'link-button', id: editId, 'aria-label': `Edit ${spec.label.toLowerCase()}`, onclick: editor }, 'Edit'),
        ),
      );
    };
    const editor = () => {
      const inputId = `input-${field}`;
      const input =
        spec.input === 'text'
          ? h('textarea', { id: inputId, rows: field === 'note' ? 8 : 3, maxlength: String(spec.max) })
          : h('input', { type: 'text', id: inputId, maxlength: String(spec.max), autocomplete: 'off', spellcheck: field === 'pronunciation' ? 'false' : null });
      input.value = typeof value === 'string' ? value : '';
      const problem = h('p', { class: 'field-error', role: 'alert', 'data-testid': `field-${field}-problem` });
      const save = h('button', { type: 'submit', class: 'primary' }, 'Save');
      const cancel = h('button', { type: 'button', onclick: () => { shown(); document.getElementById(editId)?.focus(); } }, 'Cancel');
      const form = h(
        'form',
        { class: 'field-editor' },
        h('label', { for: inputId }, spec.label),
        input,
        spec.help ? h('p', { class: 'muted small' }, spec.help) : null,
        h('div', { class: 'actions' }, save, cancel),
        problem,
      );
      form.addEventListener('submit', async (event) => {
        event.preventDefault();
        const text = input.value.trim();
        if (spec.required && text === '') {
          problem.textContent = `${spec.label} cannot be empty.`;
          input.setAttribute('aria-invalid', 'true');
          return;
        }
        save.disabled = true;
        problem.textContent = '';
        const failed = await saveField(field, text === '' ? null : text, editId);
        save.disabled = false;
        if (failed) {
          problem.textContent = failed;
          input.setAttribute('aria-invalid', 'true');
        }
      });
      form.addEventListener('keydown', (event) => {
        if (event.key === 'Escape') {
          event.preventDefault();
          cancel.click();
        }
      });
      mount(row, h('dd', { class: 'editing' }, form));
      input.focus();
    };
    shown();
    return row;
  };

  const fields = () =>
    h(
      'section',
      { class: 'word-section' },
      h('h2', {}, 'Details'),
      h('p', { class: 'muted small' }, 'What you change here stays yours: a later pack only suggests other values.'),
      h('dl', { class: 'fields edit-fields' }, Object.keys(FIELDS).map(fieldRow)),
    );

  // ── list fields ──────────────────────────────────────────────────────────

  /**
   * @param {(typeof LISTS)[number]} list
   */
  const listBlock = ([kind, heading, noun, prop]) => {
    const items = /** @type {(Item | FormItem)[]} */ (word.items[prop]);
    const problem = h('p', { class: 'field-error', role: 'alert', 'data-testid': `list-${kind}-problem` });
    const inputId = `add-${kind}`;
    const input = h('input', { type: 'text', id: inputId, maxlength: kind === 'form' ? '100' : '200', autocomplete: 'off', 'aria-label': `Add ${noun}` });
    const rare = kind === 'form' ? h('input', { type: 'checkbox', id: 'add-form-rare' }) : null;
    /** @param {Item | FormItem} item */
    const text = (item) => ('form' in item ? item.form : item.value);
    /** @param {Item | FormItem} item */
    const remove = async (item) => {
      problem.textContent = '';
      const failed = await change(
        () => /** @type {Promise<DetailAnswer>} */ (deleteJson(`${url}/items`, { kind, key: item.key })),
        {
          done: `Removed “${text(item)}”.`,
          focusId: inputId,
          undo: () => () =>
            /** @type {Promise<DetailAnswer>} */ (
              postJson(`${url}/items`, { kind, value: text(item), ...('common' in item ? { common: item.common } : {}) })
            ),
        },
      );
      if (failed) problem.textContent = failed;
    };
    const add = async (/** @type {Event} */ event) => {
      event.preventDefault();
      const value = input.value.trim();
      if (value === '') {
        problem.textContent = `Type the ${noun} first.`;
        return;
      }
      problem.textContent = '';
      const failed = await change(
        () => /** @type {Promise<DetailAnswer>} */ (postJson(`${url}/items`, { kind, value, ...(rare ? { common: !rare.checked } : {}) })),
        {
          done: `Added “${value}”.`,
          focusId: inputId,
          undo: (before, after) => {
            const old = new Set(/** @type {(Item | FormItem)[]} */ (before.items[prop]).map((i) => i.key));
            const added = /** @type {(Item | FormItem)[]} */ (after.items[prop]).find((i) => !old.has(i.key));
            return added ? () => /** @type {Promise<DetailAnswer>} */ (deleteJson(`${url}/items`, { kind, key: added.key })) : null;
          },
        },
      );
      if (failed) problem.textContent = failed;
    };
    return h(
      'div',
      { class: 'list-field', 'data-testid': `list-${kind}` },
      h('h3', {}, heading),
      items.length === 0 ? h('p', { class: 'muted small' }, 'None.') : null,
      h(
        'ul',
        { class: 'chips' },
        items.map((item) =>
          h(
            'li',
            { class: 'chip' },
            h('span', { lang: kind === 'czech' ? 'cs' : 'en' }, text(item)),
            'mine' in item && item.mine && kind === 'accepted' ? h('span', { class: 'muted small' }, ' (mine)') : null,
            'common' in item && !item.common ? h('span', { class: 'muted small' }, ' (rare)') : null,
            h('button', { type: 'button', class: 'chip-remove', 'aria-label': `Remove ${text(item)}`, onclick: () => remove(item) }, '×'),
          ),
        ),
      ),
      h(
        'form',
        { class: 'add-item', onsubmit: add },
        input,
        rare ? h('label', { class: 'rare-check' }, rare, ' rare') : null,
        h('button', { type: 'submit' }, 'Add'),
      ),
      problem,
    );
  };

  const lists = () => h('section', { class: 'word-section' }, h('h2', {}, 'Translations, answers, forms'), LISTS.map(listBlock));

  // ── sentences ────────────────────────────────────────────────────────────

  /** @param {Sentence} s */
  const sentenceItem = (s) => {
    const li = h('li', { class: s.hidden ? 'hidden-sentence' : '', 'data-testid': `sentence-${s.id}` });
    const editId = `edit-sentence-${s.id}`;
    const problem = h('p', { class: 'field-error', role: 'alert' });
    const hide = async () => {
      problem.textContent = '';
      const failed = await change(
        () => /** @type {Promise<DetailAnswer>} */ (postJson(`/api/sentences/${s.id}/hidden`, { hidden: !s.hidden })),
        {
          done: s.hidden ? 'The sentence is shown in practice again.' : 'The sentence is hidden from practice.',
          focusId: `hide-sentence-${s.id}`,
          spoken: true,
          undo: () => () => /** @type {Promise<DetailAnswer>} */ (postJson(`/api/sentences/${s.id}/hidden`, { hidden: s.hidden })),
        },
      );
      if (failed) problem.textContent = failed;
    };
    const shown = () => {
      const meta = [s.origin, s.location, s.source_title, s.edited ? 'edited by you' : null, s.hidden ? 'hidden' : null].filter(Boolean).join(' · ');
      mount(
        li,
        h('p', {}, markedText(s.en, 'en')),
        h('p', { class: 'muted' }, markedText(s.cs, 'cs')),
        h('p', { class: 'muted small' }, meta),
        h(
          'div',
          { class: 'actions' },
          h('button', { type: 'button', class: 'link-button', id: editId, onclick: editor }, 'Edit'),
          h('button', { type: 'button', class: 'link-button', id: `hide-sentence-${s.id}`, onclick: hide }, s.hidden ? 'Show in practice' : 'Hide from practice'),
        ),
        problem,
      );
    };
    const editor = () => {
      const enId = `sentence-${s.id}-en`;
      const csId = `sentence-${s.id}-cs`;
      const en = h('textarea', { id: enId, rows: 3, maxlength: '600', lang: 'en' });
      const cs = h('textarea', { id: csId, rows: 3, maxlength: '600', lang: 'cs' });
      en.value = markedSource(s.en);
      cs.value = markedSource(s.cs);
      const editProblem = h('p', { class: 'field-error', role: 'alert', 'data-testid': 'sentence-problem' });
      const save = h('button', { type: 'submit', class: 'primary' }, 'Save');
      const cancel = h('button', { type: 'button', onclick: () => { shown(); document.getElementById(editId)?.focus(); } }, 'Cancel');
      const form = h(
        'form',
        { class: 'field-editor' },
        h('label', { for: enId }, 'English'),
        en,
        h('label', { for: csId }, 'Czech'),
        cs,
        h('p', { class: 'muted small' }, 'Put [[ and ]] around the word in both, e.g. “The sea [[lulled]] us.”'),
        h('div', { class: 'actions' }, save, cancel),
        editProblem,
      );
      form.addEventListener('submit', async (event) => {
        event.preventDefault();
        const missing = [[en, 'English'], [cs, 'Czech']].filter(([t]) => !hasOneMark(/** @type {HTMLTextAreaElement} */ (t).value));
        if (missing.length > 0) {
          editProblem.textContent = `Mark the word once with [[ and ]] in the ${missing.map(([, n]) => n).join(' and the ')} sentence.`;
          return;
        }
        save.disabled = true;
        editProblem.textContent = '';
        const before = { en: markedSource(s.en), cs: markedSource(s.cs) };
        const failed = await change(
          () => /** @type {Promise<DetailAnswer>} */ (patchJson(`/api/sentences/${s.id}`, { en: en.value, cs: cs.value })),
          {
            done: 'Sentence saved.',
            focusId: editId,
            spoken: true,
            undo: () => () => /** @type {Promise<DetailAnswer>} */ (patchJson(`/api/sentences/${s.id}`, before)),
          },
        );
        save.disabled = false;
        if (failed) editProblem.textContent = failed;
      });
      form.addEventListener('keydown', (event) => {
        if (event.key === 'Escape') {
          event.preventDefault();
          cancel.click();
        }
      });
      mount(li, form);
      en.focus();
    };
    shown();
    return li;
  };

  const sentences = () =>
    h(
      'section',
      { class: 'word-section' },
      h('h2', {}, `Sentences (${word.sentences.length})`),
      word.sentences.length === 0 ? h('p', { class: 'muted' }, 'No sentences yet.') : null,
      h('ol', { class: 'sentences' }, word.sentences.map(sentenceItem)),
    );

  // ── cards, history, delete ───────────────────────────────────────────────

  /** @param {CardView} card */
  const cardItem = (card) => {
    const kind = cardKindLabel(card.kind);
    const facts = [cardStateLabel(card.state)];
    if (card.due_day) facts.push(`next due ${card.due_day}`);
    if (card.recall !== null) facts.push(`recall chance now ${percent(card.recall)} (aim ${percent(card.desired_retention)})`);
    facts.push(plural(card.reps, 'review'), plural(card.lapses, 'lapse'));
    if (card.leech !== 'none') facts.push('problem card');
    if (card.suspended) facts.push('paused');
    return h(
      'li',
      { 'data-testid': `card-${card.kind}` },
      h('p', {}, h('strong', {}, kind), ` — ${facts.join(' · ')}`),
      card.state === 'new' && card.reps === 0
        ? null
        : confirmed({
            label: `Reset ${kind} card`,
            testid: `reset-${card.kind}`,
            message:
              card.kind === 'understand'
                ? `Reset the ${kind} card? Its memory, next due day and lapse count are lost; the word starts again as a new word. The review history stays.`
                : `Reset the ${kind} card? Its memory, next due day and lapse count are lost; it is due again tomorrow. The review history stays.`,
            confirm: 'Reset card',
            onConfirm: async (problem) => {
              const failed = await change(action({ action: 'reset_card', kind: card.kind }), { done: `${kind} card reset.` });
              if (failed) problem.textContent = failed;
            },
          }),
    );
  };

  const history = () => {
    const sentenceText = new Map(word.sentences.map((s) => [s.id, `${s.en.pre}${s.en.target}${s.en.post}`]));
    return h(
      'div',
      { class: 'history' },
      h('h3', {}, `Review history (${word.history.length})`),
      word.history.length === 0
        ? h('p', { class: 'muted small' }, 'Not practised yet.')
        : h(
            'ol',
            { class: 'history-list', reversed: true },
            word.history.map((r) => {
              const grade = isGrade(r.raw_grade) ? `${r.raw_grade} ${rating(r.raw_grade).label}` : String(r.raw_grade);
              const parts = [r.review_day, cardKindLabel(r.kind), grade];
              if (r.typed_answer) parts.push(`typed “${r.typed_answer}”`);
              if (r.hint_used) parts.push('with hint');
              const sentence = r.sentence_id === null ? null : sentenceText.get(r.sentence_id);
              return h(
                'li',
                {},
                h('span', {}, parts.join(' · ')),
                sentence ? h('span', { class: 'muted small history-sentence', lang: 'en' }, sentence) : null,
              );
            }),
          ),
    );
  };

  const learning = () =>
    h(
      'section',
      { class: 'word-section' },
      h('h2', {}, 'Learning'),
      h('ul', { class: 'card-list' }, word.cards.map(cardItem)),
      history(),
    );

  const danger = () =>
    h(
      'section',
      { class: 'word-section danger-zone' },
      h('h2', {}, 'Delete'),
      confirmed({
        label: 'Delete word',
        testid: 'delete-word',
        message: `Delete “${word.headword}” for good? Its sentences, your edits, its cards and the whole review history are deleted; this cannot be undone. A later pack with this word adds it again as a new word.`,
        confirm: 'Delete for good',
        onConfirm: async (problem) => {
          const answer = await deleteJson(url);
          if (!isCurrent()) return;
          if (!answer.ok) {
            problem.textContent = answer.message;
            return;
          }
          forgetClips(word.id);
          setNotice(`Deleted “${word.headword}”.`);
          window.location.hash = '#/words';
        },
      }),
    );

  const view = () => [...header(), suggestions(), practice(), fields(), lists(), sentences(), learning(), danger()];

  draw();
}
