// @ts-check
/**
 * Starts the real `vocab-server` binary on a temporary data directory for
 * one test, behind a small HTTPS proxy that stands in for Traefik: the
 * session cookie is `Secure` (`__Host-`), which browsers only keep over
 * HTTPS. The proxy uses a throw-away self-signed certificate (the browsers
 * run with `ignoreHTTPSErrors`); `VOCAB_PUBLIC_ORIGIN` is the proxy's origin.
 *
 * Every server starts in setup state with an empty data directory (the
 * server creates it and its cache directory); `setupCode` is the one-time
 * code the server logged. The password is synthetic and made per test run.
 */

import { execFileSync, spawn } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import http from 'node:http';
import https from 'node:https';
import { tmpdir } from 'node:os';
import path from 'node:path';

const here = path.dirname(new URL(import.meta.url).pathname);

/** The server binary: `$VOCAB_SERVER_BIN`, else the workspace's debug build. */
export const SERVER_BIN = process.env.VOCAB_SERVER_BIN ?? path.resolve(here, '../../target/debug/vocab-server');

/** The password the tests set up (synthetic, per test run). */
export const PASSWORD = `test-password-${randomBytes(8).toString('hex')}`;

/** @type {{ key: Buffer, cert: Buffer } | null} */
let tls = null;

function binary() {
  if (!existsSync(SERVER_BIN)) {
    throw new Error(`vocab-server binary not found at ${SERVER_BIN}: run "cargo build -p vocab-server" first`);
  }
  return SERVER_BIN;
}

/** A self-signed certificate for 127.0.0.1, made once per worker with openssl. */
function certificateOnce() {
  if (tls === null) {
    const dir = mkdtempSync(path.join(tmpdir(), 'vocab-e2e-tls-'));
    const key = path.join(dir, 'key.pem');
    const cert = path.join(dir, 'cert.pem');
    execFileSync(
      'openssl',
      ['req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-subj', '/CN=127.0.0.1', '-days', '1', '-keyout', key, '-out', cert],
      { stdio: 'ignore' },
    );
    tls = { key: readFileSync(key), cert: readFileSync(cert) };
    rmSync(dir, { recursive: true, force: true });
  }
  return tls;
}

/**
 * An HTTPS proxy on 127.0.0.1 (any port) to `target` (set later).
 *
 * @returns {Promise<{ origin: string, setTarget: (url: URL) => void, close: () => Promise<void> }>}
 */
async function startProxy() {
  /** @type {URL | null} */
  let target = null;
  const server = https.createServer(certificateOnce(), (req, res) => {
    if (!target) {
      res.writeHead(502).end();
      return;
    }
    const upstream = http.request(
      { host: target.hostname, port: target.port, method: req.method, path: req.url, headers: req.headers },
      (answer) => {
        res.writeHead(answer.statusCode ?? 502, answer.headers);
        answer.pipe(res);
      },
    );
    upstream.on('error', () => res.destroy());
    req.pipe(upstream);
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', () => resolve(undefined)));
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('proxy has no port');
  return {
    origin: `https://127.0.0.1:${address.port}`,
    setTarget: (url) => {
      target = url;
    },
    close: () =>
      new Promise((resolve) => {
        server.closeAllConnections();
        server.close(() => resolve(undefined));
      }),
  };
}

/**
 * @typedef {{ url: string, serverUrl: string, dbPath: string, setupCode: string, log: () => string, stop: () => Promise<void> }} RunningServer
 * `url` is the HTTPS origin the browser uses (= `public_origin`); `serverUrl`
 * is the plain HTTP listener for direct API calls from the tests;
 * `setupCode` comes from the server's "Setup required" log line.
 */

/**
 * @param {{ ttsUrl?: string }} [options] `ttsUrl`: the TTS service (`VOCAB_TTS_URL`; none: browser speech only)
 * @returns {Promise<RunningServer>}
 */
export async function startServer({ ttsUrl } = {}) {
  const bin = binary();
  const proxy = await startProxy();
  const dir = mkdtempSync(path.join(tmpdir(), 'vocab-e2e-'));
  const data = path.join(dir, 'data');
  // The test runner's own VOCAB_* variables (e.g. VOCAB_SERVER_BIN) are not
  // settings of the server, which refuses unknown ones.
  const inherited = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('VOCAB_')));
  const child = spawn(bin, ['serve'], {
    env: {
      ...inherited,
      VOCAB_PUBLIC_ORIGIN: proxy.origin,
      VOCAB_LISTEN: '127.0.0.1:0',
      VOCAB_DATA_DIR: data,
      ...(ttsUrl ? { VOCAB_TTS_URL: ttsUrl } : {}),
    },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  const exited = new Promise((resolve) => child.once('exit', resolve));
  let log = '';
  /** @type {string} */
  const serverUrl = await new Promise((resolve, reject) => {
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
  proxy.setTarget(new URL(serverUrl));
  const code = /Setup required: open the app and enter setup code (\S+)/.exec(log);
  if (!code) throw new Error(`no setup code in the server log:\n${log}`);
  return {
    url: proxy.origin,
    serverUrl,
    dbPath: path.join(data, 'vocabulary.sqlite'),
    setupCode: code[1],
    log: () => log,
    stop: async () => {
      await proxy.close();
      child.kill('SIGTERM');
      await exited;
      rmSync(dir, { recursive: true, force: true });
    },
  };
}
