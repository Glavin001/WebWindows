// AudioWorklet reading the ring of float stereo frames the Wine worker
// writes (./audio-sink.mjs).

class WineAudio extends AudioWorkletProcessor {
  constructor(options) {
    super();
    const buffer = options.processorOptions.buffer;
    this.head = new Int32Array(buffer, 0, 2);
    this.data = new Float32Array(buffer, 8);
    this.frames = this.data.length / 2;
  }

  process(inputs, outputs) {
    const [left, right] = outputs[0];
    const w = Atomics.load(this.head, 0);
    let r = Atomics.load(this.head, 1);
    const n = Math.min(left.length, w - r);
    for (let i = 0; i < n; i++, r++) {
      const at = (r % this.frames) * 2;
      left[i] = this.data[at];
      right[i] = this.data[at + 1];
    }
    // Ran dry: silence for the rest.
    left.fill(0, n);
    right.fill(0, n);
    Atomics.store(this.head, 1, r);
    return true;
  }
}

registerProcessor('wine-audio', WineAudio);
