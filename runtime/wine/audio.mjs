// The audio driver for translated Wine (Milestone 5): the Unix side of the
// "winepulse.drv" that Wine's mmdevapi loads (native/audio builds a PE stub
// by that name). winmm's waveOut and DirectSound both play through
// mmdevapi, which calls these functions with __wine_unix_call
// (dlls/mmdevapi/unixlib.h).
//
// One render endpoint. A stream's buffer lives in guest memory; the
// program writes frames into it (get_render_buffer / release_render_buffer)
// and a clock running at the stream's rate consumes them, converting them to
// float stereo for a sink: an AudioWorklet in the browser (./audio-sink.mjs),
// or, in Node, nothing or a WAV file. The timer loop mmdevapi runs on its own
// thread signals the stream's event every period, as Wine's PulseAudio driver
// does; it blocks in the scheduler between periods.

import L from './audio-layout.json' with { type: 'json' };

export const AUDIO_UNIXLIB = 0x3000;

const S_OK = 0;
const S_FALSE = 1;
const E_NOTIMPL = 0x80004001;
const AUDCLNT_E_NOT_STOPPED = 0x88890005;
const AUDCLNT_E_BUFFER_TOO_LARGE = 0x88890006;
const AUDCLNT_E_OUT_OF_ORDER = 0x88890007;
const AUDCLNT_E_UNSUPPORTED_FORMAT = 0x88890008;
const AUDCLNT_E_INVALID_SIZE = 0x88890009;
const AUDCLNT_E_EVENTHANDLE_NOT_EXPECTED = 0x88890011;
const AUDCLNT_E_NOT_INITIALIZED = 0x88890001;
const AUDCLNT_E_BUFFER_OPERATION_PENDING = 0x8889000b;
const ERROR_INSUFFICIENT_BUFFER_HR = 0x8007007a;
const AUDCLNT_STREAMFLAGS_EVENTCALLBACK = 0x40000;
const AUDCLNT_BUFFERFLAGS_SILENT = 2;
const AUDCLNT_SHAREMODE_SHARED = 0;
const MMSYSERR_NOTSUPPORTED = 8;
const MMSYSERR_NODRIVER = 6;
const PRIORITY_PREFERRED = 3;

const WAVE_FORMAT_PCM = 1;
const WAVE_FORMAT_IEEE_FLOAT = 3;
const WAVE_FORMAT_EXTENSIBLE = 0xfffe;

const DEFAULT_PERIOD = 100000n; // 10 ms in 100 ns units
const MIN_PERIOD = 30000n;

// The unix_funcs enum.
const F = [
  'process_attach', 'process_detach', 'main_loop', 'get_endpoint_ids', 'create_stream', 'release_stream',
  'start', 'stop', 'reset', 'timer_loop', 'get_render_buffer', 'release_render_buffer', 'get_capture_buffer',
  'release_capture_buffer', 'is_format_supported', 'get_loopback_capture_device', 'get_mix_format',
  'get_device_period', 'get_buffer_size', 'get_latency', 'get_current_padding', 'get_next_packet_size',
  'get_frequency', 'get_position', 'set_volumes', 'set_event_handle', 'set_sample_rate', 'test_connect',
  'is_started', 'get_prop_value', 'midi_get_driver', 'midi_init', 'midi_release', 'midi_out_message',
  'midi_in_message', 'midi_notify_wait', 'aux_message',
];

/** A WAVEFORMATEX in guest memory, or null when the driver cannot play it. */
function readFormat(h, at) {
  const dv = h.m.dv;
  let tag = dv.getUint16(at, true);
  const channels = dv.getUint16(at + 2, true);
  const rate = dv.getUint32(at + 4, true);
  const blockAlign = dv.getUint16(at + 12, true);
  const bits = dv.getUint16(at + 14, true);
  if (tag === WAVE_FORMAT_EXTENSIBLE && dv.getUint16(at + 16, true) >= 22) {
    tag = dv.getUint32(at + 24, true); // SubFormat's first field
  }
  const float = tag === WAVE_FORMAT_IEEE_FLOAT;
  if (!(tag === WAVE_FORMAT_PCM || (float && bits === 32))) return null;
  if (!float && ![8, 16, 24, 32].includes(bits)) return null;
  if (channels < 1 || channels > 8 || rate < 1000 || rate > 384000) return null;
  if (blockAlign !== (channels * bits) / 8) return null;
  return { channels, rate, blockAlign, bits, float };
}

export class BrowserAudio {
  /**
   * @param {import('./host.mjs').WineHost} h
   * @param {object} [sink] {rate, write(Float32Array of interleaved stereo frames)}
   */
  constructor(h, sink = null) {
    this.h = h;
    this.m = h.m;
    this.sink = sink;
    this.mixRate = sink?.rate ?? 48000;
    this.streams = new Map();
    this.nextStream = 1;
  }

  u32(a) {
    return this.h.u32(a);
  }

  w32(a, v) {
    this.h.w32(a, v);
  }

  /** A unix call; returns an NTSTATUS, or {yield} when the calling thread blocks. */
  call(code, p) {
    const name = F[code];
    const fn = this[name];
    if (!fn) {
      this.h.log(`audio: ${name ?? code} not implemented`);
      return 0xc0000002;
    }
    return fn.call(this, p) ?? 0;
  }

  stream(p) {
    return this.streams.get(this.u32(p));
  }

  // -- the driver -----------------------------------------------------------------

  process_attach() {}
  process_detach() {}

  /** Runs mmdevapi's main loop thread: tells it the driver is ready, then blocks for good. */
  main_loop(p) {
    this.setEvent(this.u32(p));
    return this.h.threads.block('audio main loop', this.h.sys.ret, this.h.sys.esp, () => undefined, Infinity);
  }

  test_connect(p) {
    this.w32(p + L.test_connect_params.priority, PRIORITY_PREFERRED);
  }

  get_endpoint_ids(p) {
    const P = L.get_endpoint_ids_params;
    const flow = this.u32(p + P.flow);
    const buf = this.u32(p + P.endpoints);
    const size = this.u32(p + P.size);
    if (flow !== 0) {
      // No capture devices.
      this.w32(p + P.num, 0);
      this.w32(p + P.default_idx, 0);
      this.w32(p + P.result, S_OK);
      return;
    }
    const name = 'Speakers';
    const device = 'browser';
    const nameBytes = (name.length + 1) * 2;
    const needed = 8 + nameBytes + device.length + 1;
    this.w32(p + P.num, 1);
    this.w32(p + P.default_idx, 0);
    if (needed > size) {
      this.w32(p + P.size, needed);
      this.w32(p + P.result, ERROR_INSUFFICIENT_BUFFER_HR);
      return;
    }
    this.w32(buf, 8);
    this.m.writeWString(buf + 8, name);
    this.w32(buf + 4, 8 + nameBytes);
    this.m.writeCString(buf + 8 + nameBytes, device);
    this.w32(p + P.result, S_OK);
  }

  get_mix_format(p) {
    const P = L.get_mix_format_params;
    const fmt = this.u32(p + P.fmt);
    const dv = this.m.dv;
    // WAVEFORMATEXTENSIBLE: float stereo at the sink's rate.
    dv.setUint16(fmt, WAVE_FORMAT_EXTENSIBLE, true);
    dv.setUint16(fmt + 2, 2, true);
    dv.setUint32(fmt + 4, this.mixRate, true);
    dv.setUint32(fmt + 8, this.mixRate * 8, true);
    dv.setUint16(fmt + 12, 8, true);
    dv.setUint16(fmt + 14, 32, true);
    dv.setUint16(fmt + 16, 22, true);
    dv.setUint16(fmt + 18, 32, true); // valid bits
    dv.setUint32(fmt + 20, 3, true); // SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT
    // KSDATAFORMAT_SUBTYPE_IEEE_FLOAT {00000003-0000-0010-8000-00aa00389b71}
    const guid = [3, 0, 0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71];
    this.m.u8.set(guid, fmt + 24);
    this.w32(p + P.result, this.u32(p + P.flow) === 0 ? S_OK : E_NOTIMPL);
  }

  is_format_supported(p) {
    const P = L.is_format_supported_params;
    const fmt = this.u32(p + P.fmt_in);
    this.w32(p + P.result, fmt && readFormat(this.h, fmt) ? S_OK : AUDCLNT_E_UNSUPPORTED_FORMAT);
  }

  get_device_period(p) {
    const P = L.get_device_period_params;
    const def = this.u32(p + P.def_period);
    const min = this.u32(p + P.min_period);
    if (def) this.m.dv.setBigInt64(def, DEFAULT_PERIOD, true);
    if (min) this.m.dv.setBigInt64(min, MIN_PERIOD, true);
    this.w32(p + P.result, S_OK);
  }

  get_prop_value(p) {
    this.w32(p + L.get_prop_value_params.result, E_NOTIMPL);
  }

  create_stream(p) {
    const P = L.create_stream_params;
    const dv = this.m.dv;
    const flow = this.u32(p + P.flow);
    const fmt = readFormat(this.h, this.u32(p + P.fmt));
    if (flow !== 0) return this.w32(p + P.result, E_NOTIMPL);
    if (!fmt) return this.w32(p + P.result, AUDCLNT_E_UNSUPPORTED_FORMAT);
    const share = this.u32(p + P.share);
    const flags = this.u32(p + P.flags);
    let duration = dv.getBigInt64(p + P.duration, true);
    let period = dv.getBigInt64(p + P.period, true);
    if (share === AUDCLNT_SHAREMODE_SHARED || period < MIN_PERIOD) period = DEFAULT_PERIOD;
    if (duration < 3n * period) duration = 3n * period;
    const periodFrames = Math.max(1, Math.round((Number(period) * fmt.rate) / 1e7));
    const bufFrames = Math.ceil((Number(duration) * fmt.rate) / 1e7);
    if (bufFrames > fmt.rate * 10) return this.w32(p + P.result, AUDCLNT_E_INVALID_SIZE);
    const bytes = bufFrames * fmt.blockAlign;
    // The ring and a second area for writes that wrap, in guest memory.
    const ring = this.h.alloc(Math.ceil((bytes * 2) / 0x1000) * 0x1000, 4, 'audio buffer');
    const id = this.nextStream++;
    this.streams.set(id, {
      id, fmt, share, flags, bufFrames, periodFrames, period, ring, tmp: ring + bytes,
      held: 0, write: 0, read: 0, locked: 0, lockedTmp: false, started: false, event: 0,
      written: 0, lastPos: 0, clock: 0, volume: 1, released: false,
    });
    this.w32(this.u32(p + P.channel_count), fmt.channels);
    dv.setBigUint64(this.u32(p + P.stream), BigInt(id), true);
    this.w32(p + P.result, S_OK);
  }

  release_stream(p) {
    const s = this.stream(p);
    if (s) {
      s.released = true;
      this.streams.delete(s.id);
      this.h.vm.release(s.ring);
    }
    this.w32(p + L.release_stream_params.result, S_OK);
  }

  start(p) {
    const s = this.stream(p);
    if (!s) return this.w32(p + 8, AUDCLNT_E_NOT_INITIALIZED);
    if ((s.flags & AUDCLNT_STREAMFLAGS_EVENTCALLBACK) && !s.event) return this.w32(p + 8, 0x88890015); // AUDCLNT_E_EVENTHANDLE_NOT_SET
    if (s.started) return this.w32(p + 8, AUDCLNT_E_NOT_STOPPED);
    s.started = true;
    s.clock = performance.now();
    this.w32(p + 8, S_OK);
  }

  stop(p) {
    const s = this.stream(p);
    if (!s) return this.w32(p + 8, AUDCLNT_E_NOT_INITIALIZED);
    this.advance(s);
    const was = s.started;
    s.started = false;
    this.w32(p + 8, was ? S_OK : S_FALSE);
  }

  reset(p) {
    const s = this.stream(p);
    if (!s) return this.w32(p + 8, AUDCLNT_E_NOT_INITIALIZED);
    if (s.started) return this.w32(p + 8, AUDCLNT_E_NOT_STOPPED);
    if (s.locked) return this.w32(p + 8, AUDCLNT_E_BUFFER_OPERATION_PENDING);
    s.written -= s.held;
    s.held = 0;
    s.read = s.write;
    this.w32(p + 8, S_OK);
  }

  is_started(p) {
    const s = this.stream(p);
    this.w32(p + 8, s?.started ? S_OK : S_FALSE);
  }

  set_event_handle(p) {
    const P = L.set_event_handle_params;
    const s = this.stream(p);
    if (!s) return this.w32(p + P.result, AUDCLNT_E_NOT_INITIALIZED);
    if (!(s.flags & AUDCLNT_STREAMFLAGS_EVENTCALLBACK)) return this.w32(p + P.result, AUDCLNT_E_EVENTHANDLE_NOT_EXPECTED);
    s.event = this.u32(p + P.event);
    this.w32(p + P.result, S_OK);
  }

  set_volumes(p) {
    const P = L.set_volumes_params;
    const s = this.stream(p);
    if (!s) return;
    let v = this.m.dv.getFloat32(p + P.master_volume, true);
    const vols = this.u32(p + P.volumes);
    const sess = this.u32(p + P.session_volumes);
    if (vols) v *= this.m.dv.getFloat32(vols, true);
    if (sess) v *= this.m.dv.getFloat32(sess, true);
    s.volume = v;
  }

  set_sample_rate(p) {
    this.w32(p + 12, E_NOTIMPL);
  }

  get_buffer_size(p) {
    const P = L.get_buffer_size_params;
    const s = this.stream(p);
    if (!s) return this.w32(p + P.result, AUDCLNT_E_NOT_INITIALIZED);
    this.w32(this.u32(p + P.frames), s.bufFrames);
    this.w32(p + P.result, S_OK);
  }

  get_latency(p) {
    const P = L.get_latency_params;
    const s = this.stream(p);
    if (!s) return this.w32(p + P.result, AUDCLNT_E_NOT_INITIALIZED);
    this.m.dv.setBigInt64(this.u32(p + P.latency), s.period * 2n, true);
    this.w32(p + P.result, S_OK);
  }

  get_current_padding(p) {
    const P = L.get_current_padding_params;
    const s = this.stream(p);
    if (!s) return this.w32(p + P.result, AUDCLNT_E_NOT_INITIALIZED);
    this.advance(s);
    this.w32(this.u32(p + P.padding), s.held);
    this.w32(p + P.result, S_OK);
  }

  get_next_packet_size(p) {
    this.w32(p + 8, E_NOTIMPL);
  }

  get_frequency(p) {
    const P = L.get_frequency_params;
    const s = this.stream(p);
    if (!s) return this.w32(p + P.result, AUDCLNT_E_NOT_INITIALIZED);
    let f = BigInt(s.fmt.rate);
    if (s.share === AUDCLNT_SHAREMODE_SHARED) f *= BigInt(s.fmt.blockAlign);
    this.m.dv.setBigUint64(this.u32(p + P.freq), f, true);
    this.w32(p + P.result, S_OK);
  }

  get_position(p) {
    const P = L.get_position_params;
    const s = this.stream(p);
    if (!s) return this.w32(p + P.result, AUDCLNT_E_NOT_INITIALIZED);
    this.advance(s);
    // Frames played; bytes in shared mode unless the device position is asked.
    let pos = s.written - s.held;
    if (s.share === AUDCLNT_SHAREMODE_SHARED && !this.u32(p + P.device)) pos *= s.fmt.blockAlign;
    if (pos < s.lastPos) pos = s.lastPos;
    s.lastPos = pos;
    this.m.dv.setBigUint64(this.u32(p + P.pos), BigInt(pos), true);
    const qpc = this.u32(p + P.qpctime);
    if (qpc) this.m.dv.setBigUint64(qpc, BigInt(Math.floor(performance.now() * 10000)), true);
    this.w32(p + P.result, S_OK);
  }

  get_render_buffer(p) {
    const P = L.get_render_buffer_params;
    const s = this.stream(p);
    const dataPtr = this.u32(p + P.data);
    if (!s) return this.w32(p + P.result, AUDCLNT_E_NOT_INITIALIZED);
    if (s.locked) return this.w32(p + P.result, AUDCLNT_E_OUT_OF_ORDER);
    const frames = this.u32(p + P.frames);
    if (!frames) {
      this.w32(dataPtr, 0);
      return this.w32(p + P.result, S_OK);
    }
    this.advance(s);
    if (s.held + frames > s.bufFrames) return this.w32(p + P.result, AUDCLNT_E_BUFFER_TOO_LARGE);
    // Contiguous room in the ring, or the second area when the write wraps.
    const wraps = s.write + frames > s.bufFrames;
    s.locked = frames;
    s.lockedTmp = wraps;
    this.w32(dataPtr, wraps ? s.tmp : s.ring + s.write * s.fmt.blockAlign);
    this.w32(p + P.result, S_OK);
  }

  release_render_buffer(p) {
    const P = L.release_render_buffer_params;
    const s = this.stream(p);
    if (!s) return this.w32(p + P.result, AUDCLNT_E_NOT_INITIALIZED);
    const written = this.u32(p + P.written_frames);
    if (!s.locked) return this.w32(p + P.result, written ? AUDCLNT_E_OUT_OF_ORDER : S_OK);
    if (written > s.locked) return this.w32(p + P.result, AUDCLNT_E_INVALID_SIZE);
    const ba = s.fmt.blockAlign;
    const src = s.lockedTmp ? s.tmp : s.ring + s.write * ba;
    if (this.u32(p + P.flags) & AUDCLNT_BUFFERFLAGS_SILENT) {
      this.m.u8.fill(s.fmt.bits === 8 && !s.fmt.float ? 128 : 0, src, src + written * ba);
    }
    if (s.lockedTmp) {
      const first = s.bufFrames - s.write;
      const n1 = Math.min(first, written);
      this.m.u8.copyWithin(s.ring + s.write * ba, s.tmp, s.tmp + n1 * ba);
      if (written > n1) this.m.u8.copyWithin(s.ring, s.tmp + n1 * ba, s.tmp + written * ba);
    }
    s.write = (s.write + written) % s.bufFrames;
    s.held += written;
    s.written += written;
    s.locked = 0;
    this.w32(p + P.result, S_OK);
  }

  /** The stream's clock: frames due since it last ran are played (sent to the sink). */
  advance(s) {
    const now = performance.now();
    if (!s.started) {
      s.clock = now;
      return;
    }
    const due = Math.floor(((now - s.clock) * s.fmt.rate) / 1000);
    if (due <= 0) return;
    s.clock += (due * 1000) / s.fmt.rate;
    const n = Math.min(due, s.held);
    if (n) this.play(s, n);
    s.held -= n;
    s.read = (s.read + n) % s.bufFrames;
  }

  /** Converts `n` frames at the read position to float stereo at the mix rate and sends them to the sink. */
  play(s, n) {
    if (!this.sink) return;
    const { channels, bits, float, blockAlign, rate } = s.fmt;
    const dv = this.m.dv;
    const sample = (at) => {
      if (float) return dv.getFloat32(at, true);
      switch (bits) {
        case 8: return (this.m.u8[at] - 128) / 128;
        case 16: return dv.getInt16(at, true) / 32768;
        case 24: return ((dv.getUint8(at) | (dv.getUint8(at + 1) << 8) | (dv.getInt8(at + 2) << 16)) / 8388608);
        default: return dv.getInt32(at, true) / 2147483648;
      }
    };
    const bytesPerSample = bits / 8;
    const src = new Float32Array(n * 2);
    for (let i = 0; i < n; i++) {
      const at = s.ring + ((s.read + i) % s.bufFrames) * blockAlign;
      const l = sample(at);
      const r = channels > 1 ? sample(at + bytesPerSample) : l;
      src[i * 2] = l * s.volume;
      src[i * 2 + 1] = r * s.volume;
    }
    this.sink.write(rate === this.mixRate ? src : resample(src, rate, this.mixRate, s));
  }

  /** mmdevapi's timer thread: signals the stream's event every period until the stream goes. */
  timer_loop(p) {
    const s = this.stream(p);
    if (!s) return 0;
    const periodMs = (s.periodFrames * 1000) / s.fmt.rate;
    let next = performance.now() + periodMs;
    const check = () => {
      if (s.released) return 0;
      const now = performance.now();
      if (now >= next) {
        this.advance(s);
        if (s.event) this.setEvent(s.event);
        next = Math.max(next + periodMs, now - periodMs);
      }
      return undefined;
    };
    return this.h.threads.block('audio timer', this.h.sys.ret, this.h.sys.esp, check, Infinity, 0, () => next);
  }

  setEvent(handle) {
    this.h.unix?.syscalls.get('NtSetEvent')?.(handle, 0);
  }

  // -- no MIDI, aux or capture ----------------------------------------------------------

  midi_get_driver(p) {
    this.m.u16[p >>> 1] = 0;
  }
  midi_init(p) {
    const err = this.u32(p + L.midi_init_params.err);
    if (err) this.w32(err, 0);
  }
  midi_release() {}
  midi_out_message(p) {
    const err = this.u32(p + L.midi_out_message_params.err);
    if (err) this.w32(err, MMSYSERR_NODRIVER);
    const notify = this.u32(p + L.midi_out_message_params.notify);
    if (notify) this.w32(notify, 0);
  }
  midi_in_message(p) {
    this.midi_out_message(p);
  }
  aux_message(p) {
    const err = this.u32(p + L.aux_message_params.err);
    if (err) this.w32(err, MMSYSERR_NOTSUPPORTED);
  }
  get_capture_buffer(p) {
    this.w32(p + 8, E_NOTIMPL);
  }
  release_capture_buffer(p) {
    this.w32(p + 12, E_NOTIMPL);
  }
  get_loopback_capture_device() {
    return 0xc0000002;
  }
}

/** Linear interpolation from `from` to `to` Hz, carrying the phase across calls. */
function resample(src, from, to, s) {
  const n = src.length / 2;
  const step = from / to;
  s.phase ??= 0;
  s.prevL ??= 0;
  s.prevR ??= 0;
  const out = [];
  let t = s.phase;
  while (t < n) {
    const i = Math.floor(t);
    const f = t - i;
    const l0 = i === 0 ? s.prevL : src[(i - 1) * 2];
    const r0 = i === 0 ? s.prevR : src[(i - 1) * 2 + 1];
    // Interpolate between sample i-1 (or the last one before) and i.
    out.push(l0 + (src[i * 2] - l0) * f, r0 + (src[i * 2 + 1] - r0) * f);
    t += step;
  }
  s.phase = t - n;
  s.prevL = src[(n - 1) * 2];
  s.prevR = src[(n - 1) * 2 + 1];
  return Float32Array.from(out);
}

