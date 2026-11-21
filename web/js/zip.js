// @ts-check
/**
 * Reading `.zip` archives in the browser for the Import page (owner
 * feedback F3), without any library: the central directory is parsed by
 * hand, entries are either stored or deflated (`DecompressionStream`
 * `deflate-raw`). Only `.json`, `.txt` and `.md` entries are read; folders
 * and macOS metadata are skipped. Limits keep a hostile or broken archive
 * harmless: entry count, size per entry and in total, no encryption, no
 * ZIP64, every size checked against what was really decompressed and the
 * CRC-32 checked. An entry's path is only ever shown as text.
 */

/** At most this many readable entries in one archive. */
export const MAX_ZIP_ENTRIES = 100;
/** At most this many bytes per entry once unpacked. */
export const MAX_ENTRY_BYTES = 2 * 1024 * 1024;
/** At most this many bytes in all entries once unpacked. */
export const MAX_ZIP_TOTAL_BYTES = 10 * 1024 * 1024;
/** Central directory records looked at, readable or not (bounds the work). */
const MAX_RECORDS = 1000;

const EOCD_SIGNATURE = 0x06054b50;
const ZIP64_LOCATOR_SIGNATURE = 0x07064b50;
const CENTRAL_SIGNATURE = 0x02014b50;
const LOCAL_SIGNATURE = 0x04034b50;
const EOCD_SIZE = 22;
const MAX_COMMENT = 0xffff;

/** File types read from an archive (and accepted as single files). */
export const READABLE_EXTENSIONS = ['.json', '.txt', '.md'];

/**
 * A problem with the archive as a whole: nothing in it is read.
 */
export class ZipError extends Error {
  /** @param {string} message a complete sentence for the owner */
  constructor(message) {
    super(message);
    this.name = 'ZipError';
  }
}

/**
 * @typedef {{ name: string, text: string }} ZipText
 * @typedef {{ name: string, reason: string }} ZipProblem
 * @typedef {{ files: ZipText[], problems: ZipProblem[], skipped: number }} ZipContents
 */

/** Can this browser unpack deflated entries? */
export function canUnzip() {
  return typeof DecompressionStream === 'function';
}

/**
 * Is `name` a file type the Import page reads?
 *
 * @param {string} name
 */
export function readableName(name) {
  const lower = name.toLowerCase();
  return READABLE_EXTENSIONS.some((ext) => lower.endsWith(ext));
}

/**
 * Folders, macOS metadata (`__MACOSX/`, `.DS_Store`, `._name`) and other
 * file types are skipped without a word.
 *
 * @param {string} name
 */
function ignoredEntry(name) {
  if (name.endsWith('/') || name.endsWith('\\')) return true;
  const parts = name.split(/[\\/]/);
  if (parts.includes('__MACOSX')) return true;
  const base = parts[parts.length - 1] ?? '';
  if (base === '.DS_Store' || base.startsWith('._')) return true;
  return !readableName(base);
}

/** @type {Uint32Array | null} */
let crcTable = null;

/**
 * CRC-32 (the zip polynomial) of `bytes`.
 *
 * @param {Uint8Array} bytes
 */
export function crc32(bytes) {
  if (!crcTable) {
    crcTable = new Uint32Array(256);
    for (let n = 0; n < 256; n++) {
      let c = n;
      for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
      crcTable[n] = c >>> 0;
    }
  }
  let crc = 0xffffffff;
  for (let i = 0; i < bytes.length; i++) crc = crcTable[(crc ^ bytes[i]) & 0xff] ^ (crc >>> 8);
  return (crc ^ 0xffffffff) >>> 0;
}

/**
 * Find the end-of-central-directory record (it sits at the very end, after
 * an optional comment of at most 64 KiB).
 *
 * @param {DataView} view
 */
function findEndRecord(view) {
  const last = view.byteLength - EOCD_SIZE;
  const first = Math.max(0, last - MAX_COMMENT);
  for (let at = last; at >= first; at--) {
    if (view.getUint32(at, true) === EOCD_SIGNATURE && at + EOCD_SIZE + view.getUint16(at + 20, true) === view.byteLength) {
      return at;
    }
  }
  return -1;
}

/**
 * Unpack a raw-deflate stream, stopping as soon as it grows beyond `limit`
 * bytes (a zip bomb is never unpacked in full).
 *
 * @param {Uint8Array} data
 * @param {number} limit
 * @returns {Promise<Uint8Array | null>} null when it is larger than `limit`
 */
async function inflate(data, limit) {
  const stream = new Blob([/** @type {BlobPart} */ (data)]).stream().pipeThrough(new DecompressionStream('deflate-raw'));
  const reader = stream.getReader();
  /** @type {Uint8Array[]} */
  const chunks = [];
  let size = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    size += value.byteLength;
    if (size > limit) {
      await reader.cancel().catch(() => {});
      return null;
    }
    chunks.push(value);
  }
  const out = new Uint8Array(size);
  let at = 0;
  for (const chunk of chunks) {
    out.set(chunk, at);
    at += chunk.byteLength;
  }
  return out;
}

/** @param {Uint8Array} bytes */
function utf8(bytes) {
  return new TextDecoder('utf-8').decode(bytes).replace(/^﻿/, '');
}

/**
 * Read the `.json`, `.txt` and `.md` entries of a zip archive. Throws a
 * `ZipError` when the archive as a whole cannot be read; problems of
 * single entries are listed and the other entries are still read.
 *
 * @param {Uint8Array} bytes the whole archive
 * @returns {Promise<ZipContents>}
 */
export async function readZip(bytes) {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  if (bytes.byteLength < EOCD_SIZE) throw new ZipError('This is not a zip file, or it is incomplete.');
  const end = findEndRecord(view);
  if (end < 0) throw new ZipError('This is not a zip file, or it is damaged or incomplete.');
  if (end >= 20 && view.getUint32(end - 20, true) === ZIP64_LOCATOR_SIGNATURE) {
    throw new ZipError('This zip file uses the ZIP64 format, which is not supported. Zip fewer or smaller files.');
  }
  const disk = view.getUint16(end + 4, true);
  const centralDisk = view.getUint16(end + 6, true);
  const records = view.getUint16(end + 10, true);
  const centralSize = view.getUint32(end + 12, true);
  const centralOffset = view.getUint32(end + 16, true);
  if (records === 0xffff || centralSize === 0xffffffff || centralOffset === 0xffffffff) {
    throw new ZipError('This zip file uses the ZIP64 format, which is not supported. Zip fewer or smaller files.');
  }
  if (disk !== 0 || centralDisk !== 0) throw new ZipError('Zip files split into several parts are not supported.');
  if (centralOffset + centralSize > end) throw new ZipError('This zip file is damaged or incomplete.');
  if (records > MAX_RECORDS) throw new ZipError(`This zip file holds more than ${MAX_RECORDS} entries.`);

  /** @type {ZipText[]} */
  const files = [];
  /** @type {ZipProblem[]} */
  const problems = [];
  let skipped = 0;
  let total = 0;
  let at = centralOffset;
  for (let i = 0; i < records; i++) {
    if (at + 46 > end || view.getUint32(at, true) !== CENTRAL_SIGNATURE) {
      throw new ZipError('This zip file is damaged or incomplete.');
    }
    const flags = view.getUint16(at + 8, true);
    const method = view.getUint16(at + 10, true);
    const crc = view.getUint32(at + 16, true);
    const packedSize = view.getUint32(at + 20, true);
    const size = view.getUint32(at + 24, true);
    const nameLength = view.getUint16(at + 28, true);
    const extraLength = view.getUint16(at + 30, true);
    const commentLength = view.getUint16(at + 32, true);
    const localOffset = view.getUint32(at + 42, true);
    if (at + 46 + nameLength > end) throw new ZipError('This zip file is damaged or incomplete.');
    // Shown as text only: never used as a path.
    const name = utf8(bytes.subarray(at + 46, at + 46 + nameLength));
    at += 46 + nameLength + extraLength + commentLength;

    if (ignoredEntry(name)) {
      skipped++;
      continue;
    }
    if (flags & 0x1) throw new ZipError('This zip file is encrypted (password-protected), which is not supported.');
    if (size === 0xffffffff || packedSize === 0xffffffff || localOffset === 0xffffffff) {
      throw new ZipError('This zip file uses the ZIP64 format, which is not supported. Zip fewer or smaller files.');
    }
    if (files.length + problems.length >= MAX_ZIP_ENTRIES) {
      throw new ZipError(`This zip file holds more than ${MAX_ZIP_ENTRIES} files to import. Split it into smaller ones.`);
    }
    if (size > MAX_ENTRY_BYTES) {
      problems.push({ name, reason: 'larger than 2 MiB once unpacked' });
      continue;
    }
    if (total + size > MAX_ZIP_TOTAL_BYTES) {
      throw new ZipError('This zip file holds more than 10 MiB of text. Split it into smaller ones.');
    }
    if (method !== 0 && method !== 8) {
      problems.push({ name, reason: 'packed in a way that is not supported (only stored and deflated files are)' });
      continue;
    }
    if (localOffset + 30 > bytes.byteLength || view.getUint32(localOffset, true) !== LOCAL_SIGNATURE) {
      throw new ZipError('This zip file is damaged or incomplete.');
    }
    const dataStart = localOffset + 30 + view.getUint16(localOffset + 26, true) + view.getUint16(localOffset + 28, true);
    if (dataStart + packedSize > bytes.byteLength) throw new ZipError('This zip file is damaged or incomplete.');
    const packed = bytes.subarray(dataStart, dataStart + packedSize);
    /** @type {Uint8Array | null} */
    let data;
    if (method === 0) {
      data = packedSize === size ? packed : null;
    } else {
      if (!canUnzip()) throw new ZipError('This browser cannot open zip files — choose the .json files instead.');
      try {
        data = await inflate(packed, size);
      } catch {
        data = null;
      }
    }
    if (!data || data.byteLength !== size || crc32(data) !== crc) {
      problems.push({ name, reason: 'damaged (its content does not match the archive)' });
      continue;
    }
    total += size;
    files.push({ name, text: utf8(data) });
  }
  return { files, problems, skipped };
}
