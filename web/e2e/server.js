// @ts-check
/**
 * Starts the real `vocab-server` binary on a temporary data directory for
 * one test (listen port chosen by the OS, read from the "serving" log line)
 * and imports synthetic packs through the API.
 */

import { spawn } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';

const here = path.dirname(new URL(import.meta.url).pathname);

/** The server binary: `$VOCAB_SERVER_BIN`, else the workspace's debug build. */
export const SERVER_BIN = process.env.VOCAB_SERVER_BIN ?? path.resolve(here, '../../target/debug/vocab-server');

/**
 * @typedef {{ url: string, dbPath: string, stop: () => Promise<void> }} RunningServer
 */

/** @returns {Promise<RunningServer>} */
export async function startServer() {
  if (!existsSync(SERVER_BIN)) {
    throw new Error(`vocab-server binary not found at ${SERVER_BIN}: run "cargo build -p vocab-server" first`);
  }
  const dir = mkdtempSync(path.join(tmpdir(), 'vocab-e2e-'));
  const data = path.join(dir, 'data');
  const cache = path.join(dir, 'cache');
  mkdirSync(data);
  mkdirSync(cache);
  const config = path.join(dir, 'config.json');
  writeFileSync(config, JSON.stringify({ public_origin: 'http://127.0.0.1', listen: '127.0.0.1:0' }));
  const child = spawn(SERVER_BIN, ['serve'], {
    env: { ...process.env, VOCAB_CONFIG: config, VOCAB_DATA_DIR: data, VOCAB_CACHE_DIR: cache },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  const exited = new Promise((resolve) => child.once('exit', resolve));
  let log = '';
  const url = await new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`vocab-server did not start:\n${log}`)), 15_000);
    /** @param {Buffer} chunk */
    const onData = (chunk) => {
      log += chunk.toString();
      const match = /serving listen=(\S+)/.exec(log);
      if (match) {
        clearTimeout(timer);
        resolve(`http://${match[1]}`);
      }
    };
    child.stdout.on('data', onData);
    child.stderr.on('data', onData);
    child.once('exit', (code) => {
      clearTimeout(timer);
      reject(new Error(`vocab-server exited with ${code}:\n${log}`));
    });
  });
  return {
    url,
    dbPath: path.join(data, 'vocabulary.sqlite'),
    stop: async () => {
      child.kill('SIGTERM');
      await exited;
      rmSync(dir, { recursive: true, force: true });
    },
  };
}
