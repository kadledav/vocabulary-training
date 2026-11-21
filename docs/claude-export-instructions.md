# Export instructions for Claude (vocab-pack v1)

How the owner gets words from a claude.ai chat into the training app.

## How to use

1. In claude.ai, open the **Learning english** project → *Project instructions* (or *Set custom instructions*) and paste the whole content of [`claude-project-instructions.txt`](claude-project-instructions.txt). Do this once; when this file changes, replace the block.
2. Put the book PDF into the project knowledge, as you do today.
3. While reading, ask about words as usual. When you want to practise them, say e.g.:
   - *"Export the words for the training app. I'm at chapter 14."*
   - *"Export everything from Book 2 for the training app."*
   - *"Convert this old vocabulary page for the training app."* (attach the old HTML page; the instruction text maps the page's short keys `w`, `m`, `cz`, `forms`, `s`, `say`, `u`, `n`, `ux`, `ic`/`em`, `seq`, `tag`/`tagtext` to pack fields)
4. Copy each JSON block Claude returns and paste it into the app's **Import** page.

### Versioning — two different things

- **`format_version`** is the version of the *format itself*. It changes only when the app changes what it understands. The app rejects packs with a version it does not know, so a stale instruction block in the project is caught immediately rather than silently importing bad data. When it changes, this file is updated and you re-paste the instruction block.
- **Each export is a new pack** with its own `pack_id`. Exporting new words chapter by chapter is the normal way of working. Packs for the same book share the same `source.id`, so the app adds them to the same book. Exporting a word that is already in the app is safe: the app merges it (new sentences are added, nothing is duplicated, learning progress is kept). A corrected translation or marked span of a sentence the app already has replaces the stored one, unless you edited or hid that sentence in the app. A word without `tag` (or with `"tag": null`) keeps the tag the app already has. Importing the same pack twice does nothing.

---

## Instruction text

The paste-ready text is in **[`claude-project-instructions.txt`](claude-project-instructions.txt)** — copy the whole file into the project instructions. That file is the single source of the instruction text; edit it there.

---

## Example: a minimal valid pack

```json
{
  "format": "vocab-pack",
  "format_version": 1,
  "pack_id": "general--2026-11-17--1",
  "exported_at": "2026-11-17",
  "part": { "index": 1, "total": 1 },
  "source": { "id": "general", "type": "general", "title": "General Vocabulary" },
  "words": [
    {
      "headword": "verbatim",
      "grammar": "adverb and adjective",
      "forms": [],
      "pronunciation": "ver-BAY-tim",
      "speak": "verbatim",
      "meaning": "Word for word — exactly as originally said or written, with nothing changed.",
      "czech": ["doslova", "slovo od slova", "doslovný"],
      "usefulness": 7,
      "goal": "active",
      "accepted": ["verbatim"],
      "confusables": ["literally"],
      "hint": "Latin 'verbum' = word → word by word.",
      "spelling": true,
      "spelling_note": "verb-a-tim: ends in -im, not -em",
      "note": "Everyday version: word for word. 'To the letter' is about following instructions exactly.",
      "sentences": [
        { "en": "Don't copy the article [[verbatim]] — put it in your own words.", "cs": "Nekopíruj ten článek [[doslova]] — napiš to vlastními slovy.", "origin": "example" },
        { "en": "The court needs a [[verbatim]] transcript of the hearing.", "cs": "Soud potřebuje [[doslovný]] přepis jednání.", "origin": "example" },
        { "en": "He could recite the speech [[verbatim]].", "cs": "Dokázal ten projev odříkat [[slovo od slova]].", "origin": "example" }
      ]
    }
  ]
}
```

---

## Notes for the design agent

This is the guarantor's **draft of format v1**, written so the owner can start collecting packs now. The design agent formalises it as a JSON Schema (`docs/vocab-pack.schema.json`) and may propose changes; any incompatible change must bump `format_version`, update this file, and keep v1 importable. Validation rules the importer must enforce (R-23): exactly one `[[…]]` span in each `en` and `cs`; required fields present; `usefulness` 1–10; `goal` in the enum; `source.id` pattern `^[a-z0-9]+(-[a-z0-9]+)*$`. The complete rule list is in `docs/design/architecture.md` §5.2; the schema, the importer and the instruction text are kept in agreement by the conformance tests of the `vocab-pack` crate (for every optional field `null` means "not given"; line breaks may be `\n`, `\r\n` or `\r`; `location` may be a number; `source.url` must be `http(s)://`).
