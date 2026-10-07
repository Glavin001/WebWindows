// Sound from the Wine worker to the page (Milestone 5): a ring of float
// stereo frames in shared memory. The audio driver (./audio.mjs) writes
// what the program plays into it; an AudioWorklet on the page reads it at
// the AudioContext's rate, playing silence when it runs dry.
//
// Layout: Int32 [write, read] (frame counts), then Float32 frames (L, R).

const HEADER = 2;

/** A new ring for `frames` stereo frames (on the page); pass `.buffer` to the worker and the worklet. */
export function createAudioRing(frames = 16384) {
  return new SharedArrayBuffer(HEADER * 4 + frames * 2 * 4);
}

/** The worker's end: a sink for BrowserAudio at `rate` Hz. */
export function ringWriter(buffer, rate) {
  const head = new Int32Array(buffer, 0, HEADER);
  const data = new Float32Array(buffer, HEADER * 4);
  const frames = data.length / 2;
  return {
    rate,
    write(src) {
      const n = src.length / 2;
      let w = Atomics.load(head, 0);
      const r = Atomics.load(head, 1);
      // A full ring drops the oldest frames' worth of new ones.
      const room = frames - (w - r);
      const k = Math.min(n, room);
      for (let i = 0; i < k; i++) {
        const at = ((w + i) % frames) * 2;
        data[at] = src[i * 2];
        data[at + 1] = src[i * 2 + 1];
      }
      Atomics.store(head, 0, w + k);
    },
  };
}

/**
 * The page's end: starts an AudioContext playing the ring. Call from a user
 * gesture (browsers start audio only then). Returns the context.
 */
export async function playAudioRing(buffer) {
  const ctx = new AudioContext();
  await ctx.audioWorklet.addModule(new URL('./audio-worklet.js', import.meta.url));
  const node = new AudioWorkletNode(ctx, 'wine-audio', { outputChannelCount: [2], processorOptions: { buffer } });
  node.connect(ctx.destination);
  // Without a user gesture (or headless) resume() may never settle.
  await Promise.race([ctx.resume(), new Promise((ok) => setTimeout(ok, 500))]);
  return ctx;
}
