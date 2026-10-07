#!/usr/bin/env node
// Development server with the headers shared WebAssembly memory needs
// (cross-origin isolation). Serves the repository root.
//
//   node runtime/web/serve.mjs [port] [root]
//   open http://localhost:8080/runtime/web/

import { createServer } from 'node:http';
import { readFile, stat } from 'node:fs/promises';
import { extname, join, resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const port = Number(process.argv[2] ?? 8080);
const root = resolve(process.argv[3] ?? join(dirname(fileURLToPath(import.meta.url)), '../..'));
const types = {
  '.html': 'text/html; charset=utf-8',
  '.mjs': 'text/javascript',
  '.js': 'text/javascript',
  '.wasm': 'application/wasm',
  '.json': 'application/json',
  '.exe': 'application/octet-stream',
};

createServer(async (req, res) => {
  const url = new URL(req.url, 'http://x');
  let path = join(root, decodeURIComponent(url.pathname));
  if (!path.startsWith(root)) {
    res.writeHead(403).end();
    return;
  }
  try {
    if ((await stat(path)).isDirectory()) path = join(path, 'index.html');
    const body = await readFile(path);
    res.writeHead(200, {
      'Content-Type': types[extname(path)] ?? 'application/octet-stream',
      'Cross-Origin-Opener-Policy': 'same-origin',
      'Cross-Origin-Embedder-Policy': 'require-corp',
      'Cache-Control': 'no-cache',
    });
    res.end(body);
  } catch {
    res.writeHead(404).end('not found');
  }
}).listen(port, () => console.log(`serving ${root} on http://localhost:${port}/runtime/web/`));
