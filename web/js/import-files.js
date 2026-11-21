// @ts-check
/**
 * Import from files (owner feedback F3): one `.zip`, or several `.json` /
 * `.txt` / `.md` files, read in the browser and turned into the same text a
 * paste would be (every JSON file as a fenced ```json block). A selection
 * larger than one paste is cut into parts at file boundaries, each within
 * the server's paste limit, so it can be checked and imported part by part.
 */

import { MAX_ENTRY_BYTES, MAX_ZIP_TOTAL_BYTES, ZipError, canUnzip, readZip, readableName } from './zip.js';

/** The server's limit for one paste (`vocab-pack` MAX_PASTE_BYTES). */
export const MAX_PASTE_BYTES = 2 * 1024 * 1024;
/** One part is kept a little under the limit (the separators between files). */
const PART_BYTES = MAX_PASTE_BYTES - 1024;

/**
 * @typedef {{ name: string, bytes: Uint8Array }} ChosenFile
 * @typedef {{ name: string, text: string }} Piece one file's text, ready to paste
 * @typedef {{ name: string, reason: string }} FileProblem
 * @typedef {{ pieces: Piece[], problems: FileProblem[], fatal: string | null }} ReadResult
 */

const encoder = new TextEncoder();

/** @param {string} text */
export function byteLength(text) {
  return encoder.encode(text).byteLength;
}

/**
 * The paste text of one file: JSON (a `.json` file, or a `.txt`/`.md` file
 * that holds bare JSON) is fenced, anything else (Claude's answer with its
 * own fenced blocks) is taken as it is.
 *
 * @param {string} name
 * @param {string} text
 */
export function pieceText(name, text) {
  const trimmed = text.trim();
  const json = name.toLowerCase().endsWith('.json') || trimmed.startsWith('{') || trimmed.startsWith('[');
  return json && !trimmed.startsWith('```') ? `\`\`\`json\n${trimmed}\n\`\`\`` : trimmed;
}

/** @param {Uint8Array} bytes */
function decode(bytes) {
  return new TextDecoder('utf-8').decode(bytes).replace(/^﻿/, '');
}

/**
 * Read the chosen files into pieces of paste text.
 *
 * @param {ChosenFile[]} chosen
 * @returns {Promise<ReadResult>}
 */
export async function readChosen(chosen) {
  /** @type {Piece[]} */
  const pieces = [];
  /** @type {FileProblem[]} */
  const problems = [];
  let total = 0;
  for (const file of chosen) {
    const lower = file.name.toLowerCase();
    if (lower.endsWith('.zip')) {
      if (!canUnzip()) {
        return { pieces: [], problems, fatal: 'This browser cannot open zip files — choose the .json files instead.' };
      }
      try {
        const contents = await readZip(file.bytes);
        for (const p of contents.problems) problems.push({ name: `${file.name} › ${p.name}`, reason: p.reason });
        for (const f of contents.files) {
          total += f.text.length;
          pieces.push({ name: `${file.name} › ${f.name}`, text: pieceText(f.name, f.text) });
        }
        if (contents.files.length === 0 && contents.problems.length === 0) {
          problems.push({ name: file.name, reason: 'holds no .json, .txt or .md files' });
        }
      } catch (err) {
        if (err instanceof ZipError) problems.push({ name: file.name, reason: err.message });
        else problems.push({ name: file.name, reason: 'could not be read' });
      }
    } else if (readableName(file.name)) {
      if (file.bytes.byteLength > MAX_ENTRY_BYTES) {
        problems.push({ name: file.name, reason: 'larger than 2 MiB — split it into smaller files' });
        continue;
      }
      total += file.bytes.byteLength;
      pieces.push({ name: file.name, text: pieceText(file.name, decode(file.bytes)) });
    } else {
      problems.push({ name: file.name, reason: 'not a .zip, .json, .txt or .md file' });
    }
    if (total > MAX_ZIP_TOTAL_BYTES) {
      return { pieces: [], problems, fatal: 'The chosen files hold more than 10 MiB of text. Choose fewer at a time.' };
    }
  }
  return { pieces, problems, fatal: null };
}

/**
 * Cut the pieces into parts that each fit one paste, keeping files whole.
 * A single piece larger than a paste cannot be imported (its name is
 * returned in `tooLarge`).
 *
 * @param {Piece[]} pieces
 * @param {number} [limit]
 * @returns {{ parts: string[], tooLarge: string[] }}
 */
export function splitIntoParts(pieces, limit = PART_BYTES) {
  /** @type {string[]} */
  const parts = [];
  /** @type {string[]} */
  const tooLarge = [];
  let current = '';
  let currentBytes = 0;
  for (const piece of pieces) {
    const size = byteLength(piece.text);
    if (size > limit) {
      tooLarge.push(piece.name);
      continue;
    }
    const joined = current === '' ? size : currentBytes + 2 + size;
    if (joined > limit) {
      parts.push(current);
      current = piece.text;
      currentBytes = size;
    } else {
      current = current === '' ? piece.text : `${current}\n\n${piece.text}`;
      currentBytes = joined;
    }
  }
  if (current !== '') parts.push(current);
  return { parts, tooLarge };
}

/**
 * "12 KB", "1.4 MB" for a byte count.
 *
 * @param {number} bytes
 */
export function sizeLabel(bytes) {
  if (bytes < 1024) return `${bytes} bytes`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}
