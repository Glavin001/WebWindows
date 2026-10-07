// Synchronous writes to stdout/stderr for the Node hosts. The guest runs in
// one long synchronous call, so process.stdout.write to a pipe would only
// queue the bytes, and the process.exit that follows the run drops whatever
// is still queued.

import { writeSync } from 'node:fs';

export function writeAll(fd, bytes) {
  const buf = typeof bytes === 'string' ? Buffer.from(bytes) : Buffer.from(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  for (let off = 0; off < buf.length; ) {
    try {
      off += writeSync(fd, buf, off, buf.length - off);
    } catch (e) {
      // A non-blocking pipe whose reader is behind: wait for it.
      if (e.code !== 'EAGAIN') throw e;
      Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 1);
    }
  }
}

export const stdout = (b) => writeAll(1, b);
export const stderr = (b) => writeAll(2, b);
