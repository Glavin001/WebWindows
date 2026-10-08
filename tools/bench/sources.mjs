// The benchmark programs' sources, pinned and checked: SQLite's amalgamation
// and speedtest1, Lua and CoreMark. Shared by the suite (suite.mjs), the
// CoreMark lanes (coremark.mjs) and the site's benchmark apps
// (tools/site/apps.mjs).

import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';

export const SOURCES = {
  sqlite: {
    url: 'https://www.sqlite.org/2025/sqlite-amalgamation-3500400.zip',
    sha256: '1d3049dd0f830a025a53105fc79fd2ab9431aea99e137809d064d8ee8356b032',
    file: 'sqlite.zip',
    dir: 'sqlite-amalgamation-3500400',
    unpack: (f, dir) => execFileSync('unzip', ['-qo', f], { cwd: dir }),
  },
  speedtest1: {
    url: 'https://raw.githubusercontent.com/sqlite/sqlite/version-3.50.4/test/speedtest1.c',
    sha256: 'f495cd1c3f727ebf6270d967b43f11a14304053ae4532d6338dbfea65c1a5a78',
    file: 'speedtest1.c',
  },
  lua: {
    url: 'https://www.lua.org/ftp/lua-5.4.7.tar.gz',
    sha256: '9fbf5e28ef86c69858f6d3d34eccc32e911c1a28b4120ff3e84aaa70cfbf1e30',
    file: 'lua.tar.gz',
    dir: 'lua-5.4.7',
    unpack: (f, dir) => execFileSync('tar', ['xzf', f], { cwd: dir }),
  },
};

const unpacked = new Set();

/** Downloads (once) and checks source `name` into `dir`, unpacking it there. */
export function fetchSource(name, dir) {
  const s = SOURCES[name];
  const f = join(dir, s.file);
  mkdirSync(dir, { recursive: true });
  if (!existsSync(f)) execFileSync('curl', ['-sSfL', '-o', f, s.url], { stdio: 'inherit' });
  const got = createHash('sha256').update(readFileSync(f)).digest('hex');
  if (got !== s.sha256) throw new Error(`${s.file}: SHA-256 ${got}, expected ${s.sha256}`);
  const key = `${name}:${dir}`;
  if (s.unpack && !unpacked.has(key)) {
    s.unpack(f, dir);
    unpacked.add(key);
  }
}

/** CoreMark's commit, and its sources' files. */
export const COREMARK_COMMIT = '1f483d5b8316753a742cbf5590caf5bd0a4e4777';
export const COREMARK_FILES = ['core_list_join.c', 'core_main.c', 'core_matrix.c', 'core_state.c', 'core_util.c', 'simple/core_portme.c'];

/** Checks CoreMark out at `COREMARK_COMMIT` into `dir` (a git repository of its own). */
export function fetchCoremark(dir) {
  let head = '';
  try {
    head = execFileSync('git', ['-C', dir, 'rev-parse', 'HEAD'], { stdio: ['ignore', 'pipe', 'ignore'] }).toString().trim();
  } catch {}
  if (head === COREMARK_COMMIT) return;
  execFileSync('rm', ['-rf', dir]);
  execFileSync('git', ['init', '-q', dir]);
  execFileSync('git', ['-C', dir, 'fetch', '-q', '--depth', '1', 'https://github.com/eembc/coremark', COREMARK_COMMIT], { stdio: 'inherit' });
  execFileSync('git', ['-C', dir, 'checkout', '-q', 'FETCH_HEAD']);
}
