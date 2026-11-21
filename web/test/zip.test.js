// @ts-check
/**
 * Import from files (owner feedback F3): the in-browser zip reader and the
 * file-to-paste conversion, with small synthetic archives built here.
 */

import assert from 'node:assert/strict';
import { test } from 'node:test';
import { crc32 as nodeCrc32, deflateRawSync } from 'node:zlib';

import { byteLength, pieceText, readChosen, splitIntoParts } from '../js/import-files.js';
import { MAX_ENTRY_BYTES, ZipError, crc32, readZip } from '../js/zip.js';

const enc = new TextEncoder();

/**
 * A zip archive of `entries` (stored or deflated), built by hand.
 *
 * @param {{ name: string, text?: string, bytes?: Uint8Array, method?: 0 | 8, flags?: number, size?: number }[]} entries
 * @param {{ zip64Locator?: boolean }} [options]
 */
function makeZip(entries, options = {}) {
  /** @type {Buffer[]} */
  const locals = [];
  /** @type {Buffer[]} */
  const centrals = [];
  let offset = 0;
  for (const e of entries) {
    const name = Buffer.from(e.name, 'utf8');
    const raw = Buffer.from(e.bytes ?? enc.encode(e.text ?? ''));
    const method = e.method ?? 0;
    const data = method === 8 ? deflateRawSync(raw) : raw;
    const crc = nodeCrc32(raw) >>> 0;
    const size = e.size ?? raw.length;
    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0);
    local.writeUInt16LE(20, 4);
    local.writeUInt16LE((e.flags ?? 0) | 0x800, 6);
    local.writeUInt16LE(method, 8);
    local.writeUInt32LE(crc, 14);
    local.writeUInt32LE(data.length, 18);
    local.writeUInt32LE(size, 22);
    local.writeUInt16LE(name.length, 26);
    locals.push(local, name, data);
    const central = Buffer.alloc(46);
    central.writeUInt32LE(0x02014b50, 0);
    central.writeUInt16LE(20, 4);
    central.writeUInt16LE(20, 6);
    central.writeUInt16LE((e.flags ?? 0) | 0x800, 8);
    central.writeUInt16LE(method, 10);
    central.writeUInt32LE(crc, 16);
    central.writeUInt32LE(data.length, 20);
    central.writeUInt32LE(size, 24);
    central.writeUInt16LE(name.length, 28);
    central.writeUInt32LE(offset, 42);
    centrals.push(central, name);
    offset += 30 + name.length + data.length;
  }
  const centralBytes = Buffer.concat(centrals);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(entries.length, 8);
  end.writeUInt16LE(entries.length, 10);
  end.writeUInt32LE(centralBytes.length, 12);
  end.writeUInt32LE(offset, 16);
  const parts = [...locals, centralBytes];
  if (options.zip64Locator) {
    const locator = Buffer.alloc(20);
    locator.writeUInt32LE(0x07064b50, 0);
    parts.push(locator);
  }
  parts.push(end);
  return new Uint8Array(Buffer.concat(parts));
}

const PACK_A = '{"format":"vocab-pack","pack_id":"a"}';
const PACK_B = '{"format":"vocab-pack","pack_id":"b"}';

test('F3 crc32 matches the zip polynomial', () => {
  const bytes = enc.encode('The quick brown fox jumps over the lazy dog');
  assert.equal(crc32(bytes), 0x414fa339);
  assert.equal(crc32(bytes), nodeCrc32(bytes) >>> 0);
});

test('F3 a zip with stored and deflated entries in a nested folder is read', async () => {
  const zip = makeZip([
    { name: 'packs/', text: '' },
    { name: 'packs/a.json', text: PACK_A, method: 0 },
    { name: 'packs/deep/b.json', text: PACK_B.repeat(1), method: 8 },
    { name: 'packs/answer.md', text: 'Here:\n```json\n{}\n```\n', method: 8 },
  ]);
  const contents = await readZip(zip);
  assert.deepEqual(
    contents.files.map((f) => f.name),
    ['packs/a.json', 'packs/deep/b.json', 'packs/answer.md'],
  );
  assert.equal(contents.files[1]?.text, PACK_B);
  assert.equal(contents.problems.length, 0);
  assert.equal(contents.skipped, 1);
});

test('F3 macOS metadata, .DS_Store and other file types are skipped', async () => {
  const zip = makeZip([
    { name: '__MACOSX/packs/._a.json', text: 'binary junk', method: 0 },
    { name: 'packs/.DS_Store', text: 'junk', method: 0 },
    { name: 'packs/._a.json', text: 'junk', method: 0 },
    { name: 'packs/picture.png', text: 'png', method: 0 },
    { name: 'packs/a.JSON', text: PACK_A, method: 8 },
  ]);
  const contents = await readZip(zip);
  assert.deepEqual(contents.files.map((f) => f.name), ['packs/a.JSON']);
  assert.equal(contents.skipped, 4);
});

test('F3 an entry over 2 MiB is reported and the others are still read', async () => {
  const big = 'x'.repeat(MAX_ENTRY_BYTES + 1);
  const zip = makeZip([
    { name: 'big.json', text: big, method: 8 },
    { name: 'a.json', text: PACK_A, method: 8 },
  ]);
  const contents = await readZip(zip);
  assert.deepEqual(contents.files.map((f) => f.name), ['a.json']);
  assert.equal(contents.problems[0]?.name, 'big.json');
  assert.match(contents.problems[0]?.reason ?? '', /2 MiB/);
});

test('F3 an entry that unpacks to more than it claims is never unpacked in full', async () => {
  // Claims 10 bytes, holds a megabyte of zeros (a small zip bomb).
  const zip = makeZip([{ name: 'bomb.json', bytes: new Uint8Array(1024 * 1024), method: 8, size: 10 }]);
  const contents = await readZip(zip);
  assert.equal(contents.files.length, 0);
  assert.match(contents.problems[0]?.reason ?? '', /damaged/);
});

test('F3 a truncated archive is refused with a clear message', async () => {
  const zip = makeZip([{ name: 'a.json', text: PACK_A, method: 8 }]);
  await assert.rejects(readZip(zip.subarray(0, zip.length - 10)), (err) => err instanceof ZipError && /damaged|incomplete/.test(err.message));
  await assert.rejects(readZip(zip.subarray(0, 12)), ZipError);
  await assert.rejects(readZip(enc.encode('not a zip at all, just some text that is long enough')), ZipError);
});

test('F3 a cut-off central directory or entry data is refused', async () => {
  const zip = makeZip([{ name: 'a.json', text: PACK_A.repeat(50), method: 0 }]);
  // Keep the end record but drop bytes in the middle: offsets point nowhere.
  const broken = new Uint8Array([...zip.subarray(0, 40), ...zip.subarray(zip.length - 120)]);
  await assert.rejects(readZip(broken), ZipError);
});

test('F3 encrypted and ZIP64 archives are refused', async () => {
  await assert.rejects(readZip(makeZip([{ name: 'a.json', text: PACK_A, flags: 1 }])), /encrypted/);
  await assert.rejects(readZip(makeZip([{ name: 'a.json', text: PACK_A }], { zip64Locator: true })), /ZIP64/);
});

test('F3 more than 100 files in one archive are refused', async () => {
  const many = Array.from({ length: 101 }, (_, i) => ({ name: `p${i}.json`, text: '{}' }));
  await assert.rejects(readZip(makeZip(many)), /more than 100/);
});

test('F3 files become fenced JSON blocks; a selection over one paste goes in parts', async () => {
  assert.equal(pieceText('a.json', PACK_A), `\`\`\`json\n${PACK_A}\n\`\`\``);
  assert.equal(pieceText('a.txt', ' {"x":1} '), '```json\n{"x":1}\n```');
  assert.equal(pieceText('answer.md', 'Text\n```json\n{}\n```'), 'Text\n```json\n{}\n```');

  const read = await readChosen([
    { name: 'a.json', bytes: enc.encode(PACK_A) },
    { name: 'b.json', bytes: enc.encode(`﻿${PACK_B}`) },
    { name: 'photo.jpg', bytes: enc.encode('jpg') },
  ]);
  assert.equal(read.fatal, null);
  assert.deepEqual(read.pieces.map((p) => p.name), ['a.json', 'b.json']);
  assert.equal(read.problems[0]?.name, 'photo.jpg');
  const { parts } = splitIntoParts(read.pieces);
  assert.equal(parts.length, 1);
  assert.ok(parts[0]?.includes('"pack_id":"a"') && parts[0]?.includes('"pack_id":"b"'));

  const pieces = [
    { name: 'one', text: 'a'.repeat(60) },
    { name: 'two', text: 'b'.repeat(60) },
    { name: 'huge', text: 'c'.repeat(200) },
    { name: 'three', text: 'd'.repeat(30) },
  ];
  const split = splitIntoParts(pieces, 100);
  assert.deepEqual(split.tooLarge, ['huge']);
  assert.deepEqual(split.parts.map((p) => byteLength(p)), [60, 92]);
});

test('F3 a zip among the chosen files is unpacked; entry names are shown under the archive name', async () => {
  const zip = makeZip([
    { name: 'x/a.json', text: PACK_A, method: 8 },
    { name: '__MACOSX/x/._a.json', text: 'junk' },
  ]);
  const read = await readChosen([{ name: 'packs.zip', bytes: zip }]);
  assert.deepEqual(read.pieces.map((p) => p.name), ['packs.zip › x/a.json']);
  const empty = await readChosen([{ name: 'empty.zip', bytes: makeZip([{ name: 'readme.pdf', text: 'x' }]) }]);
  assert.match(empty.problems[0]?.reason ?? '', /no \.json/);
});
