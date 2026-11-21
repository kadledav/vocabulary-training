// @ts-check
/**
 * A stand-in for the TTS service (vocab-tts, design §10.2) for the e2e
 * tests: `GET /v1/voices` and `POST /v1/speak`, the latter answering every
 * text with the same short real MP3 (`hello.mp3`, the synthetic word
 * "Hello." from the real service). Counts the speak requests.
 */

import { readFileSync } from 'node:fs';
import http from 'node:http';
import path from 'node:path';

const here = path.dirname(new URL(import.meta.url).pathname);
const CLIP = readFileSync(path.join(here, 'hello.mp3'));

/**
 * @returns {Promise<{ url: string, speaks: () => number, close: () => Promise<void> }>}
 */
export async function startFakeTts() {
  let speaks = 0;
  const server = http.createServer((req, res) => {
    if (req.method === 'GET' && req.url === '/v1/voices') {
      const body = JSON.stringify({ voices: [], frontend: 'g2p-e2e', engine: 'fake-e2e' });
      res.writeHead(200, { 'content-type': 'application/json', 'content-length': Buffer.byteLength(body) });
      res.end(body);
      return;
    }
    if (req.method === 'POST' && req.url === '/v1/speak') {
      req.resume();
      req.on('end', () => {
        speaks += 1;
        res.writeHead(200, {
          'content-type': 'audio/mpeg',
          'content-length': CLIP.length,
          'x-duration-ms': '700',
          'x-approximate': '0',
        });
        res.end(CLIP);
      });
      return;
    }
    res.writeHead(404, { 'content-length': 0 }).end();
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', () => resolve(undefined)));
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('fake TTS has no port');
  return {
    url: `http://127.0.0.1:${address.port}`,
    speaks: () => speaks,
    close: () =>
      new Promise((resolve) => {
        server.closeAllConnections();
        server.close(() => resolve(undefined));
      }),
  };
}
