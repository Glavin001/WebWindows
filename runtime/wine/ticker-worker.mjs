// The ticker's worker (see ./ticker.mjs): writes the clock into the shared
// memory every millisecond.

import { writeClock } from './ticker.mjs';

function loop({ buffer, boot }) {
  const dv = new DataView(buffer);
  const sleeper = new Int32Array(new SharedArrayBuffer(4));
  for (;;) {
    writeClock(dv, boot);
    Atomics.wait(sleeper, 0, 0, 1);
  }
}

if (globalThis.process?.versions?.node) {
  const { workerData, parentPort } = await import('node:worker_threads');
  parentPort.postMessage('ready');
  loop(workerData);
} else {
  self.onmessage = (e) => {
    self.postMessage('ready');
    loop(e.data);
  };
}
