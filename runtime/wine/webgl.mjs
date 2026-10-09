// OpenGL for translated Wine: the host side of opengl32 on WebGL 2
// (native/opengl32-webgl).
//
// The guest's opengl32 is gl4es, which turns desktop OpenGL into OpenGL ES 2;
// each ES call it makes is a unix call that this bridge runs on a WebGL 2
// context in the same worker (the program's threads all run here, so the
// calls are synchronous like a driver's). The calls are numbered by
// ./gles-table.mjs (GLES_CALL_BASE + index) and take 32-bit words.
//
// What OpenGL ES has and WebGL 2 does not is done here:
//  - object names: GL's integers map to WebGL objects (one table per kind);
//  - uniform locations: integers per program, consecutive for the elements
//    of an array, as gl4es computes them;
//  - client-side arrays: vertex attributes and indices in guest memory are
//    copied into scratch buffers at each draw;
//  - strings, which the guest caches (GLES_STRING), and buffer reads for its
//    mappings (GLES_GET_BUFFER).
//
// A swap reads the drawing buffer back into guest memory (GLES_PRESENT) and
// the guest draws it into the window with GDI.

import { GLES_CALL_BASE, GLES_FUNCTIONS } from './gles-table.mjs';

const GLES_OPEN = 0x1000;
const GLES_MAKE_CURRENT = 0x1001;
const GLES_PRESENT = 0x1002;
const GLES_STRING = 0x1003;
const GLES_GET_BUFFER = 0x1004;

const STATUS_SUCCESS = 0;
const STATUS_UNSUCCESSFUL = 0xc0000001;
const STATUS_NOT_SUPPORTED = 0xc00000bb;

// GL enums used here.
const GL = {
  BYTE: 0x1400, UNSIGNED_BYTE: 0x1401, SHORT: 0x1402, UNSIGNED_SHORT: 0x1403, INT: 0x1404, UNSIGNED_INT: 0x1405,
  FLOAT: 0x1406, HALF_FLOAT: 0x140b, FIXED: 0x140c, HALF_FLOAT_OES: 0x8d61,
  UNSIGNED_SHORT_4_4_4_4: 0x8033, UNSIGNED_SHORT_5_5_5_1: 0x8034, UNSIGNED_SHORT_5_6_5: 0x8363,
  UNSIGNED_INT_2_10_10_10_REV: 0x8368, UNSIGNED_INT_24_8: 0x84fa, UNSIGNED_INT_10F_11F_11F_REV: 0x8c3b,
  UNSIGNED_INT_5_9_9_9_REV: 0x8c3e, FLOAT_32_UNSIGNED_INT_24_8_REV: 0x8dad, INT_2_10_10_10_REV: 0x8d9f,
  ALPHA: 0x1906, RGB: 0x1907, RGBA: 0x1908, LUMINANCE: 0x1909, LUMINANCE_ALPHA: 0x190a, RED: 0x1903,
  RG: 0x8227, RED_INTEGER: 0x8d94, RG_INTEGER: 0x8228, RGB_INTEGER: 0x8d98, RGBA_INTEGER: 0x8d99,
  DEPTH_COMPONENT: 0x1902, DEPTH_STENCIL: 0x84f9,
  ARRAY_BUFFER: 0x8892, ELEMENT_ARRAY_BUFFER: 0x8893, PIXEL_PACK_BUFFER: 0x88eb, PIXEL_UNPACK_BUFFER: 0x88ec,
  STREAM_DRAW: 0x88e0, READ_FRAMEBUFFER: 0x8ca8,
  UNPACK_ROW_LENGTH: 0x0cf2, UNPACK_SKIP_ROWS: 0x0cf3, UNPACK_SKIP_PIXELS: 0x0cf4, UNPACK_ALIGNMENT: 0x0cf5,
  PACK_ROW_LENGTH: 0x0d02, PACK_SKIP_ROWS: 0x0d03, PACK_SKIP_PIXELS: 0x0d04, PACK_ALIGNMENT: 0x0d05,
  UNPACK_IMAGE_HEIGHT: 0x806e, UNPACK_SKIP_IMAGES: 0x806d,
  EXTENSIONS: 0x1f03, VENDOR: 0x1f00, RENDERER: 0x1f01, VERSION: 0x1f02, SHADING_LANGUAGE_VERSION: 0x8b8c,
  NUM_EXTENSIONS: 0x821d, MAJOR_VERSION: 0x821b, MINOR_VERSION: 0x821c,
  NUM_COMPRESSED_TEXTURE_FORMATS: 0x86a2, COMPRESSED_TEXTURE_FORMATS: 0x86a3,
  NUM_SHADER_BINARY_FORMATS: 0x8df9, SHADER_BINARY_FORMATS: 0x8df8, NUM_PROGRAM_BINARY_FORMATS: 0x87fe,
  PROGRAM_BINARY_FORMATS: 0x87ff, SHADER_COMPILER: 0x8dfa, IMPLEMENTATION_COLOR_READ_FORMAT: 0x8b9b,
  IMPLEMENTATION_COLOR_READ_TYPE: 0x8b9a, MAX_TEXTURE_MAX_ANISOTROPY: 0x84ff,
  INFO_LOG_LENGTH: 0x8b84, SHADER_SOURCE_LENGTH: 0x8b88, ACTIVE_UNIFORMS: 0x8b86,
  ACTIVE_UNIFORM_MAX_LENGTH: 0x8b87, ACTIVE_ATTRIBUTES: 0x8b89, ACTIVE_ATTRIBUTE_MAX_LENGTH: 0x8b8a,
  ACTIVE_UNIFORM_BLOCKS: 0x8a36, ACTIVE_UNIFORM_BLOCK_MAX_NAME_LENGTH: 0x8a35, LINK_STATUS: 0x8b82,
  TRANSFORM_FEEDBACK_VARYINGS: 0x8c83, TRANSFORM_FEEDBACK_VARYING_MAX_LENGTH: 0x8c76,
  PROGRAM_BINARY_LENGTH: 0x8741, UNIFORM_BLOCK_NAME_LENGTH: 0x8a41,
  VERTEX_ATTRIB_ARRAY_BUFFER_BINDING: 0x889f, VERTEX_ATTRIB_ARRAY_POINTER: 0x8645,
  VERTEX_ATTRIB_ARRAY_ENABLED: 0x8622, VERTEX_ATTRIB_ARRAY_SIZE: 0x8623, VERTEX_ATTRIB_ARRAY_STRIDE: 0x8624,
  VERTEX_ATTRIB_ARRAY_TYPE: 0x8625, VERTEX_ATTRIB_ARRAY_NORMALIZED: 0x886a, VERTEX_ATTRIB_ARRAY_INTEGER: 0x88fd,
  CURRENT_VERTEX_ATTRIB: 0x8626, ARRAY_BUFFER_BINDING: 0x8894, ELEMENT_ARRAY_BUFFER_BINDING: 0x8895,
  NUM_SAMPLE_COUNTS: 0x9380, SAMPLES: 0x80a9, TIMEOUT_IGNORED_LO: 0xffffffff,
  INVALID_ENUM: 0x0500, INVALID_VALUE: 0x0501, INVALID_OPERATION: 0x0502,
  TEXTURE_2D: 0x0de1,
};

/** Bytes per element of a pixel or vertex type. */
const TYPE_BYTES = {
  [GL.BYTE]: 1, [GL.UNSIGNED_BYTE]: 1, [GL.SHORT]: 2, [GL.UNSIGNED_SHORT]: 2, [GL.INT]: 4, [GL.UNSIGNED_INT]: 4,
  [GL.FLOAT]: 4, [GL.HALF_FLOAT]: 2, [GL.HALF_FLOAT_OES]: 2, [GL.FIXED]: 4,
  [GL.UNSIGNED_SHORT_4_4_4_4]: 2, [GL.UNSIGNED_SHORT_5_5_5_1]: 2, [GL.UNSIGNED_SHORT_5_6_5]: 2,
  [GL.UNSIGNED_INT_2_10_10_10_REV]: 4, [GL.INT_2_10_10_10_REV]: 4, [GL.UNSIGNED_INT_24_8]: 4,
  [GL.UNSIGNED_INT_10F_11F_11F_REV]: 4, [GL.UNSIGNED_INT_5_9_9_9_REV]: 4, [GL.FLOAT_32_UNSIGNED_INT_24_8_REV]: 8,
};
/** Types whose one element holds a whole pixel. */
const PACKED = new Set([
  GL.UNSIGNED_SHORT_4_4_4_4, GL.UNSIGNED_SHORT_5_5_5_1, GL.UNSIGNED_SHORT_5_6_5, GL.UNSIGNED_INT_2_10_10_10_REV,
  GL.UNSIGNED_INT_24_8, GL.UNSIGNED_INT_10F_11F_11F_REV, GL.UNSIGNED_INT_5_9_9_9_REV,
  GL.FLOAT_32_UNSIGNED_INT_24_8_REV,
]);
const COMPONENTS = {
  [GL.ALPHA]: 1, [GL.LUMINANCE]: 1, [GL.RED]: 1, [GL.RED_INTEGER]: 1, [GL.DEPTH_COMPONENT]: 1,
  [GL.LUMINANCE_ALPHA]: 2, [GL.RG]: 2, [GL.RG_INTEGER]: 2, [GL.DEPTH_STENCIL]: 2,
  [GL.RGB]: 3, [GL.RGB_INTEGER]: 3, [GL.RGBA]: 4, [GL.RGBA_INTEGER]: 4,
};

/** The typed array WebGL takes for pixel data of a type. */
function arrayFor(type) {
  switch (type) {
    case GL.BYTE: return Int8Array;
    case GL.SHORT: return Int16Array;
    case GL.INT: return Int32Array;
    case GL.FLOAT: case GL.FLOAT_32_UNSIGNED_INT_24_8_REV: return Float32Array;
    case GL.UNSIGNED_SHORT: case GL.HALF_FLOAT: case GL.HALF_FLOAT_OES: case GL.UNSIGNED_SHORT_4_4_4_4:
    case GL.UNSIGNED_SHORT_5_5_5_1: case GL.UNSIGNED_SHORT_5_6_5: return Uint16Array;
    case GL.UNSIGNED_INT: case GL.UNSIGNED_INT_2_10_10_10_REV: case GL.UNSIGNED_INT_24_8:
    case GL.UNSIGNED_INT_10F_11F_11F_REV: case GL.UNSIGNED_INT_5_9_9_9_REV: return Uint32Array;
    default: return Uint8Array;
  }
}

/** The kinds of named objects, by what creates them. */
const KINDS = ['buffer', 'texture', 'framebuffer', 'renderbuffer', 'program', 'shader', 'vao', 'query', 'sampler',
  'sync', 'transformFeedback'];

export class WebGLBridge {
  /**
   * @param {(s: string) => void} log
   * @param {() => OffscreenCanvas} [makeCanvas]  where the context lives
   */
  constructor(log = () => {}, makeCanvas = () => new OffscreenCanvas(1, 1)) {
    this.log = log;
    this.makeCanvas = makeCanvas;
    this.gl = null;
    this.calls = 0;
    this.failed = new Set();
    this.decoders = GLES_FUNCTIONS.map(([name, rkind, kinds]) => ({ name, rkind, kinds: [...kinds] }));
  }

  /** Makes the WebGL 2 context (the first GLES_OPEN). */
  open() {
    if (this.gl) return true;
    let canvas;
    try {
      canvas = this.makeCanvas();
    } catch {
      return false;
    }
    const gl = canvas.getContext('webgl2', {
      alpha: false, depth: true, stencil: true, antialias: false, premultipliedAlpha: false,
      preserveDrawingBuffer: true, powerPreference: 'high-performance',
    });
    if (!gl) return false;
    this.canvas = canvas;
    this.gl = gl;
    this.names = Object.fromEntries(KINDS.map((k) => [k, new Map()]));
    this.next = Object.fromEntries(KINDS.map((k) => [k, 1]));
    this.vaos = new Map([[0, this.vaoState()]]);
    this.vao = this.vaos.get(0);
    this.arrayBuffer = 0;
    this.unpackBuffer = 0;
    this.packBuffer = 0;
    this.program = null;
    this.store = new Map([
      [GL.UNPACK_ALIGNMENT, 4], [GL.PACK_ALIGNMENT, 4], [GL.UNPACK_ROW_LENGTH, 0], [GL.UNPACK_SKIP_ROWS, 0],
      [GL.UNPACK_SKIP_PIXELS, 0], [GL.UNPACK_IMAGE_HEIGHT, 0], [GL.UNPACK_SKIP_IMAGES, 0], [GL.PACK_ROW_LENGTH, 0],
      [GL.PACK_SKIP_ROWS, 0], [GL.PACK_SKIP_PIXELS, 0],
    ]);
    this.scratch = [];
    this.scratchIndices = gl.createBuffer();
    // Extensions WebGL 2 has for what gl4es can use, by their ES names.
    const ext = ['GL_OES_texture_npot', 'GL_EXT_blend_minmax', 'GL_OES_element_index_uint',
      'GL_OES_packed_depth_stencil', 'GL_OES_depth24', 'GL_OES_rgb8_rgba8', 'GL_OES_depth_texture',
      'GL_OES_fragment_precision_high', 'GL_EXT_texture_lod_bias'];
    if (gl.getExtension('EXT_texture_filter_anisotropic')) ext.push('GL_EXT_texture_filter_anisotropic');
    if (gl.getExtension('WEBGL_compressed_texture_s3tc')) {
      ext.push('GL_EXT_texture_compression_s3tc', 'GL_EXT_texture_compression_dxt1');
    }
    this.extensions = ext;
    const info = gl.getExtension('WEBGL_debug_renderer_info');
    this.renderer = (info && gl.getParameter(info.UNMASKED_RENDERER_WEBGL)) || gl.getParameter(gl.RENDERER);
    this.log(`opengl32: WebGL 2 on ${this.renderer}`);
    return true;
  }

  vaoState() {
    return { attribs: [], element: 0 };
  }

  /** A unix call from opengl32.dll: `args` is its array of words. */
  unixCall(machine, code, args) {
    const u8 = machine.u8;
    const dv = new DataView(u8.buffer);
    this.u8 = u8;
    this.dv = dv;
    if (code >= GLES_CALL_BASE) return this.call(code - GLES_CALL_BASE, args);
    switch (code) {
      case GLES_OPEN:
        return this.open() ? STATUS_SUCCESS : STATUS_NOT_SUPPORTED;
      case GLES_MAKE_CURRENT: {
        if (!this.gl) return STATUS_UNSUCCESSFUL;
        const w = Math.max(1, dv.getUint32(args + 8, true));
        const h = Math.max(1, dv.getUint32(args + 12, true));
        if (this.canvas.width !== w || this.canvas.height !== h) {
          this.canvas.width = w;
          this.canvas.height = h;
        }
        return STATUS_SUCCESS;
      }
      case GLES_PRESENT:
        return this.present(dv.getUint32(args + 4, true), dv.getUint32(args + 8, true), dv.getUint32(args + 12, true));
      case GLES_STRING: {
        const s = this.string(dv.getUint32(args + 4, true), dv.getUint32(args + 8, true));
        if (s == null) return STATUS_UNSUCCESSFUL;
        const bytes = new TextEncoder().encode(s);
        dv.setUint32(args, bytes.length, true);
        const buf = dv.getUint32(args + 12, true);
        const size = dv.getUint32(args + 16, true);
        if (buf && size) {
          const n = Math.min(bytes.length, size - 1);
          u8.set(bytes.subarray(0, n), buf);
          u8[buf + n] = 0;
        }
        return STATUS_SUCCESS;
      }
      case GLES_GET_BUFFER: {
        const [target, offset, size, dst] = [4, 8, 12, 16].map((o) => dv.getUint32(args + o, true));
        const tmp = new Uint8Array(size);
        try {
          this.gl.getBufferSubData(target, offset, tmp);
        } catch (e) {
          this.warn('getBufferSubData', e);
        }
        u8.set(tmp, dst);
        return STATUS_SUCCESS;
      }
      default:
        return STATUS_NOT_SUPPORTED;
    }
  }

  warn(name, e) {
    if (this.failed.has(name)) return;
    this.failed.add(name);
    this.log(`opengl32: ${name}: ${e?.message ?? e}`);
  }

  /** Runs ES function `index` with the words at `args`. */
  call(index, args) {
    const f = this.decoders[index];
    const fn = f && this[f.name];
    if (!this.gl || !fn) {
      if (f) this.warn(f.name, 'not implemented');
      return STATUS_NOT_SUPPORTED;
    }
    const dv = this.dv;
    const a = [];
    let p = args + 4;
    for (const k of f.kinds) {
      if (k === 'f') a.push(dv.getFloat32(p, true));
      else if (k === 'p') a.push(dv.getUint32(p, true));
      else if (k === 'q') {
        a.push(dv.getUint32(p, true) + dv.getUint32(p + 4, true) * 2 ** 32);
        p += 4;
      } else a.push(dv.getInt32(p, true));
      p += 4;
    }
    this.calls++;
    let r;
    try {
      r = fn.apply(this, a);
    } catch (e) {
      this.warn(f.name, e);
      r = 0;
    }
    if (f.rkind !== 'v') dv.setUint32(args, typeof r === 'boolean' ? +r : r >>> 0, true);
    return STATUS_SUCCESS;
  }

  // ---- Guest memory ----------------------------------------------------------

  cstr(p) {
    if (!p) return '';
    const u8 = this.u8;
    let e = p;
    while (u8[e]) e++;
    return new TextDecoder().decode(u8.slice(p, e));
  }

  /** Writes `s` as a C string of at most `size` bytes; returns its length. */
  putStr(s, size, lengthPtr, ptr) {
    const bytes = new TextEncoder().encode(s);
    const n = size > 0 ? Math.min(bytes.length, size - 1) : 0;
    if (ptr && size > 0) {
      this.u8.set(bytes.subarray(0, n), ptr);
      this.u8[ptr + n] = 0;
    }
    if (lengthPtr) this.dv.setInt32(lengthPtr, n, true);
    return n;
  }

  i32(p, n) {
    return p % 4 ? new Int32Array(this.u8.slice(p, p + 4 * n).buffer) : new Int32Array(this.u8.buffer, p, n);
  }

  u32(p, n) {
    return p % 4 ? new Uint32Array(this.u8.slice(p, p + 4 * n).buffer) : new Uint32Array(this.u8.buffer, p, n);
  }

  f32(p, n) {
    return p % 4 ? new Float32Array(this.u8.slice(p, p + 4 * n).buffer) : new Float32Array(this.u8.buffer, p, n);
  }

  /** Writes one value of a query result (number, boolean, object, array). */
  putValues(p, v, kind = 'i', max = Infinity) {
    if (!p) return;
    const dv = this.dv;
    const vals = v == null ? [0]
      : typeof v === 'object' && typeof v.length === 'number' ? Array.from(v).slice(0, max)
        : [v];
    vals.forEach((x, i) => {
      const n = typeof x === 'boolean' ? +x : typeof x === 'number' ? x : x && typeof x === 'object' ? this.nameOf(x) : 0;
      if (kind === 'f') dv.setFloat32(p + 4 * i, n, true);
      else if (kind === 'b') dv.setUint8(p + i, n ? 1 : 0);
      else if (kind === 'q') dv.setBigInt64(p + 8 * i, BigInt(Math.trunc(n)), true);
      else dv.setInt32(p + 4 * i, kind === 'i' && !Number.isInteger(n) ? Math.round(n) : n, true);
    });
  }

  /** The bytes of an image of w x h x d pixels in client memory (unpack). */
  imageBytes(w, h, d, format, type, pack = false) {
    const st = this.store;
    const bpp = PACKED.has(type) ? TYPE_BYTES[type] : (COMPONENTS[format] ?? 4) * (TYPE_BYTES[type] ?? 1);
    const rowPixels = st.get(pack ? GL.PACK_ROW_LENGTH : GL.UNPACK_ROW_LENGTH) || w;
    const align = st.get(pack ? GL.PACK_ALIGNMENT : GL.UNPACK_ALIGNMENT);
    const row = Math.ceil((rowPixels * bpp) / align) * align;
    const skip = pack
      ? st.get(GL.PACK_SKIP_ROWS) * row + st.get(GL.PACK_SKIP_PIXELS) * bpp
      : st.get(GL.UNPACK_SKIP_ROWS) * row + st.get(GL.UNPACK_SKIP_PIXELS) * bpp;
    const imageRows = (!pack && st.get(GL.UNPACK_IMAGE_HEIGHT)) || h;
    return skip + row * (imageRows * (d - 1) + h - 1) + w * bpp + (pack ? 0 : st.get(GL.UNPACK_SKIP_IMAGES) * row * imageRows);
  }

  /** Pixel data at guest `p` as [view, offset] for WebGL, or null. */
  pixels(p, w, h, d, format, type) {
    const T = arrayFor(type);
    if (p % T.BYTES_PER_ELEMENT === 0) return [new T(this.u8.buffer), p / T.BYTES_PER_ELEMENT];
    const n = this.imageBytes(w, h, d, format, type);
    const copy = new Uint8Array(Math.ceil(n / T.BYTES_PER_ELEMENT) * T.BYTES_PER_ELEMENT);
    copy.set(this.u8.subarray(p, p + n));
    return [new T(copy.buffer), 0];
  }

  // ---- Names ---------------------------------------------------------------

  obj(kind, name) {
    return name ? this.names[kind].get(name >>> 0) ?? null : null;
  }

  nameOf(o) {
    return o?.__glName ?? 0;
  }

  add(kind, o, name = 0) {
    if (!o) return 0;
    if (!name) {
      while (this.names[kind].has(this.next[kind])) this.next[kind]++;
      name = this.next[kind]++;
    }
    o.__glName = name;
    this.names[kind].set(name, o);
    return name;
  }

  gen(kind, create, n, p) {
    for (let i = 0; i < n; i++) this.dv.setUint32(p + 4 * i, this.add(kind, create()), true);
  }

  del(kind, destroy, n, p) {
    for (let i = 0; i < n; i++) {
      const name = this.dv.getUint32(p + 4 * i, true);
      const o = this.obj(kind, name);
      if (!o) continue;
      destroy(o);
      this.names[kind].delete(name);
    }
  }

  /** The object of a name, made on first bind as GL ES does. */
  bound(kind, name, create) {
    if (!name) return null;
    return this.obj(kind, name) ?? this.names[kind].get(this.add(kind, create(), name >>> 0));
  }

  // ---- Strings and presenting ------------------------------------------------

  string(name, index) {
    const gl = this.gl;
    if (!gl) return null;
    if (index !== 0xffffffff) return name === GL.EXTENSIONS ? this.extensions[index] ?? null : null;
    switch (name) {
      case GL.VENDOR: return 'WebWindows';
      case GL.RENDERER: return `WebGL 2 (${this.renderer})`;
      case GL.VERSION: return 'OpenGL ES 2.0 (WebGL 2.0)';
      case GL.SHADING_LANGUAGE_VERSION: return 'OpenGL ES GLSL ES 1.00 (WebGL GLSL ES 3.00)';
      // gl4es looks for each name followed by a space.
      case GL.EXTENSIONS: return `${this.extensions.join(' ')} `;
      default: return null;
    }
  }

  /** Reads the drawing buffer into guest memory as bottom-up BGRA rows. */
  present(width, height, dst) {
    const gl = this.gl;
    if (!gl || !dst) return STATUS_UNSUCCESSFUL;
    const w = Math.min(width, gl.drawingBufferWidth);
    const h = Math.min(height, gl.drawingBufferHeight);
    const read = gl.getParameter(gl.READ_FRAMEBUFFER_BINDING);
    gl.bindFramebuffer(gl.READ_FRAMEBUFFER, null);
    if (this.packBuffer) gl.bindBuffer(gl.PIXEL_PACK_BUFFER, null);
    const saved = [GL.PACK_ALIGNMENT, GL.PACK_ROW_LENGTH, GL.PACK_SKIP_ROWS, GL.PACK_SKIP_PIXELS];
    gl.pixelStorei(GL.PACK_ALIGNMENT, 4);
    for (const s of saved.slice(1)) gl.pixelStorei(s, 0);
    const n = w * h * 4;
    if (!this.frame || this.frame.length < n) this.frame = new Uint8Array(n);
    const px = this.frame.subarray(0, n);
    gl.readPixels(0, 0, w, h, gl.RGBA, gl.UNSIGNED_BYTE, px);
    for (const s of saved) gl.pixelStorei(s, this.store.get(s));
    if (this.packBuffer) gl.bindBuffer(gl.PIXEL_PACK_BUFFER, this.obj('buffer', this.packBuffer));
    gl.bindFramebuffer(gl.READ_FRAMEBUFFER, read);
    // RGBA to BGRA, row by row into the guest's (possibly wider) frame.
    const out = this.u8;
    for (let y = 0; y < h; y++) {
      let s = y * w * 4;
      let d = dst + y * width * 4;
      for (let x = 0; x < w; x++, s += 4, d += 4) {
        out[d] = px[s + 2];
        out[d + 1] = px[s + 1];
        out[d + 2] = px[s];
        out[d + 3] = 255;
      }
    }
    return STATUS_SUCCESS;
  }

  // ---- Draws with client-side arrays -----------------------------------------

  /** Copies the enabled client-side attributes for vertices [0, count). */
  clientArrays(count) {
    const gl = this.gl;
    let any = false;
    this.vao.attribs.forEach((a, i) => {
      if (!a?.enabled || !a.client) return;
      any = true;
      const elem = a.size * (TYPE_BYTES[a.type] ?? 4);
      const stride = a.stride || elem;
      const bytes = count > 0 ? (count - 1) * stride + elem : 0;
      const buf = (this.scratch[i] ??= gl.createBuffer());
      gl.bindBuffer(gl.ARRAY_BUFFER, buf);
      gl.bufferData(gl.ARRAY_BUFFER, Math.max(bytes, 4), gl.STREAM_DRAW);
      if (bytes) gl.bufferSubData(gl.ARRAY_BUFFER, 0, this.u8, a.ptr, bytes);
      if (a.integer) gl.vertexAttribIPointer(i, a.size, a.type, a.stride, 0);
      else gl.vertexAttribPointer(i, a.size, a.type, a.norm, a.stride, 0);
    });
    if (any) gl.bindBuffer(gl.ARRAY_BUFFER, this.obj('buffer', this.arrayBuffer));
    return any;
  }

  hasClientArrays() {
    return this.vao.attribs.some((a) => a?.enabled && a.client);
  }

  /** The largest index of `count` indices of `type` at `bytes[offset]`. */
  static maxIndex(bytes, offset, count, type) {
    const size = TYPE_BYTES[type] ?? 1;
    const end = Math.min(offset + count * size, bytes.byteLength);
    let max = 0;
    const dv = new DataView(bytes.buffer, bytes.byteOffset);
    for (let p = offset; p + size <= end; p += size) {
      const v = size === 1 ? dv.getUint8(p) : size === 2 ? dv.getUint16(p, true) : dv.getUint32(p, true);
      if (v > max && !(size === 4 && v === 0xffffffff) && !(size === 2 && v === 0xffff) && !(size === 1 && v === 0xff)) max = v;
    }
    return max;
  }

  /** drawElements and its kin, with indices in a buffer or in guest memory. */
  drawIndexed(mode, count, type, ptr, draw, vertices = -1) {
    const gl = this.gl;
    const element = this.obj('buffer', this.vao.element);
    if (this.hasClientArrays()) {
      if (vertices < 0) {
        const src = element ? element.__shadow : this.u8;
        vertices = src ? WebGLBridge.maxIndex(src, ptr, count, type) + 1 : 0;
      }
      this.clientArrays(vertices);
    }
    if (element) {
      draw(ptr);
      return;
    }
    const bytes = count * (TYPE_BYTES[type] ?? 1);
    gl.bindBuffer(gl.ELEMENT_ARRAY_BUFFER, this.scratchIndices);
    gl.bufferData(gl.ELEMENT_ARRAY_BUFFER, Math.max(bytes, 4), gl.STREAM_DRAW);
    if (bytes) gl.bufferSubData(gl.ELEMENT_ARRAY_BUFFER, 0, this.u8, ptr, bytes);
    draw(0);
    gl.bindBuffer(gl.ELEMENT_ARRAY_BUFFER, null);
  }

  // ---- Uniform locations ---------------------------------------------------

  /** After a link: numbers for the program's uniforms, consecutive within arrays. */
  mapUniforms(prog) {
    const gl = this.gl;
    prog.__locs = [];
    prog.__uniforms = new Map();
    if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) return;
    const n = gl.getProgramParameter(prog, gl.ACTIVE_UNIFORMS);
    for (let i = 0; i < n; i++) {
      const u = gl.getActiveUniform(prog, i);
      if (!u) continue;
      const base = u.name.replace(/\[0\]$/, '');
      const first = prog.__locs.length;
      const array = u.name.endsWith('[0]');
      for (let j = 0; j < u.size; j++) {
        prog.__locs.push(gl.getUniformLocation(prog, array ? `${base}[${j}]` : base));
      }
      prog.__uniforms.set(base, { first, size: u.size });
    }
  }

  loc(location) {
    return location >= 0 ? this.program?.__locs?.[location] ?? null : null;
  }

  // ---- The ES functions ------------------------------------------------------
  // Named as in gles-table.mjs; arguments decoded by kind.

  glActiveTexture(t) { this.gl.activeTexture(t); }
  glAttachShader(p, s) { this.gl.attachShader(this.obj('program', p), this.obj('shader', s)); }
  glBindAttribLocation(p, i, name) { this.gl.bindAttribLocation(this.obj('program', p), i, this.cstr(name)); }
  glBindBuffer(target, name) {
    const gl = this.gl;
    const b = this.bound('buffer', name, () => gl.createBuffer());
    if (target === GL.ARRAY_BUFFER) this.arrayBuffer = name;
    else if (target === GL.ELEMENT_ARRAY_BUFFER) this.vao.element = name;
    else if (target === GL.PIXEL_PACK_BUFFER) this.packBuffer = name;
    else if (target === GL.PIXEL_UNPACK_BUFFER) this.unpackBuffer = name;
    if (b && target === GL.ELEMENT_ARRAY_BUFFER) b.__element = true;
    gl.bindBuffer(target, b);
  }
  glBindFramebuffer(t, name) { const gl = this.gl; gl.bindFramebuffer(t, this.bound('framebuffer', name, () => gl.createFramebuffer())); }
  glBindRenderbuffer(t, name) { const gl = this.gl; gl.bindRenderbuffer(t, this.bound('renderbuffer', name, () => gl.createRenderbuffer())); }
  glBindTexture(t, name) { const gl = this.gl; gl.bindTexture(t, this.bound('texture', name, () => gl.createTexture())); }
  glBlendColor(r, g, b, a) { this.gl.blendColor(r, g, b, a); }
  glBlendEquation(m) { this.gl.blendEquation(m); }
  glBlendEquationSeparate(a, b) { this.gl.blendEquationSeparate(a, b); }
  glBlendFunc(s, d) { this.gl.blendFunc(s, d); }
  glBlendFuncSeparate(a, b, c, d) { this.gl.blendFuncSeparate(a, b, c, d); }

  /** The buffer bound to `target`, for keeping a copy of index data. */
  targetBuffer(target) {
    const name = target === GL.ELEMENT_ARRAY_BUFFER ? this.vao.element : target === GL.ARRAY_BUFFER ? this.arrayBuffer : 0;
    return this.obj('buffer', name);
  }

  glBufferData(target, size, data, usage) {
    const gl = this.gl;
    if (data) gl.bufferData(target, this.u8, usage, data, size);
    else gl.bufferData(target, size, usage);
    const b = this.targetBuffer(target);
    if (b && target === GL.ELEMENT_ARRAY_BUFFER) {
      b.__shadow = new Uint8Array(size);
      if (data) b.__shadow.set(this.u8.subarray(data, data + size));
    }
  }
  glBufferSubData(target, offset, size, data) {
    this.gl.bufferSubData(target, offset, this.u8, data, size);
    const b = this.targetBuffer(target);
    if (b?.__shadow && target === GL.ELEMENT_ARRAY_BUFFER) b.__shadow.set(this.u8.subarray(data, data + size), offset);
  }
  glCheckFramebufferStatus(t) { return this.gl.checkFramebufferStatus(t); }
  glClear(m) { this.gl.clear(m); }
  glClearColor(r, g, b, a) { this.gl.clearColor(r, g, b, a); }
  glClearDepthf(d) { this.gl.clearDepth(d); }
  glClearStencil(s) { this.gl.clearStencil(s); }
  glColorMask(r, g, b, a) { this.gl.colorMask(!!r, !!g, !!b, !!a); }
  glCompileShader(s) { this.gl.compileShader(this.obj('shader', s)); }
  glCompressedTexImage2D(t, level, fmt, w, h, border, size, data) {
    if (this.unpackBuffer) this.gl.compressedTexImage2D(t, level, fmt, w, h, border, size, data);
    else this.gl.compressedTexImage2D(t, level, fmt, w, h, border, this.u8, data, size);
  }
  glCompressedTexSubImage2D(t, level, x, y, w, h, fmt, size, data) {
    if (this.unpackBuffer) this.gl.compressedTexSubImage2D(t, level, x, y, w, h, fmt, size, data);
    else this.gl.compressedTexSubImage2D(t, level, x, y, w, h, fmt, this.u8, data, size);
  }
  glCopyTexImage2D(t, level, fmt, x, y, w, h, border) { this.gl.copyTexImage2D(t, level, fmt, x, y, w, h, border); }
  glCopyTexSubImage2D(t, level, xo, yo, x, y, w, h) { this.gl.copyTexSubImage2D(t, level, xo, yo, x, y, w, h); }
  glCreateProgram() { return this.add('program', this.gl.createProgram()); }
  glCreateShader(type) { return this.add('shader', this.gl.createShader(type)); }
  glCullFace(m) { this.gl.cullFace(m); }
  glDeleteBuffers(n, p) {
    this.del('buffer', (b) => {
      const name = b.__glName;
      if (this.arrayBuffer === name) this.arrayBuffer = 0;
      for (const v of this.vaos.values()) if (v.element === name) v.element = 0;
      this.gl.deleteBuffer(b);
    }, n, p);
  }
  glDeleteFramebuffers(n, p) { this.del('framebuffer', (o) => this.gl.deleteFramebuffer(o), n, p); }
  glDeleteProgram(name) {
    const o = this.obj('program', name);
    if (!o) return;
    this.gl.deleteProgram(o);
    this.names.program.delete(name);
  }
  glDeleteRenderbuffers(n, p) { this.del('renderbuffer', (o) => this.gl.deleteRenderbuffer(o), n, p); }
  glDeleteShader(name) {
    const o = this.obj('shader', name);
    if (!o) return;
    this.gl.deleteShader(o);
    this.names.shader.delete(name);
  }
  glDeleteTextures(n, p) { this.del('texture', (o) => this.gl.deleteTexture(o), n, p); }
  glDepthFunc(f) { this.gl.depthFunc(f); }
  glDepthMask(m) { this.gl.depthMask(!!m); }
  glDepthRangef(n, f) { this.gl.depthRange(n, f); }
  glDetachShader(p, s) { this.gl.detachShader(this.obj('program', p), this.obj('shader', s)); }
  glDisable(c) { this.gl.disable(c); }
  glDisableVertexAttribArray(i) {
    (this.vao.attribs[i] ??= {}).enabled = false;
    this.gl.disableVertexAttribArray(i);
  }
  glDrawArrays(mode, first, count) {
    if (this.hasClientArrays()) this.clientArrays(first + count);
    this.gl.drawArrays(mode, first, count);
  }
  glDrawElements(mode, count, type, ptr) {
    this.drawIndexed(mode, count, type, ptr, (o) => this.gl.drawElements(mode, count, type, o));
  }
  glEnable(c) { this.gl.enable(c); }
  glEnableVertexAttribArray(i) {
    (this.vao.attribs[i] ??= {}).enabled = true;
    this.gl.enableVertexAttribArray(i);
  }
  glFinish() { this.gl.finish(); }
  glFlush() { this.gl.flush(); }
  glFramebufferRenderbuffer(t, a, rt, rb) { this.gl.framebufferRenderbuffer(t, a, rt, this.obj('renderbuffer', rb)); }
  glFramebufferTexture2D(t, a, tt, tex, level) { this.gl.framebufferTexture2D(t, a, tt, this.obj('texture', tex), level); }
  glFrontFace(m) { this.gl.frontFace(m); }
  glGenBuffers(n, p) { this.gen('buffer', () => this.gl.createBuffer(), n, p); }
  glGenerateMipmap(t) { this.gl.generateMipmap(t); }
  glGenFramebuffers(n, p) { this.gen('framebuffer', () => this.gl.createFramebuffer(), n, p); }
  glGenRenderbuffers(n, p) { this.gen('renderbuffer', () => this.gl.createRenderbuffer(), n, p); }
  glGenTextures(n, p) { this.gen('texture', () => this.gl.createTexture(), n, p); }

  activeInfo(info, bufSize, length, size, type, name) {
    if (!info) {
      if (length) this.dv.setInt32(length, 0, true);
      return;
    }
    if (size) this.dv.setInt32(size, info.size, true);
    if (type) this.dv.setUint32(type, info.type, true);
    this.putStr(info.name, bufSize, length, name);
  }
  glGetActiveAttrib(p, i, bufSize, length, size, type, name) {
    this.activeInfo(this.gl.getActiveAttrib(this.obj('program', p), i), bufSize, length, size, type, name);
  }
  glGetActiveUniform(p, i, bufSize, length, size, type, name) {
    this.activeInfo(this.gl.getActiveUniform(this.obj('program', p), i), bufSize, length, size, type, name);
  }
  glGetAttachedShaders(p, max, count, shaders) {
    const list = (this.gl.getAttachedShaders(this.obj('program', p)) ?? []).slice(0, Math.max(0, max));
    if (count) this.dv.setInt32(count, list.length, true);
    list.forEach((s, i) => this.dv.setUint32(shaders + 4 * i, this.nameOf(s), true));
  }
  glGetAttribLocation(p, name) { return this.gl.getAttribLocation(this.obj('program', p), this.cstr(name)); }

  /** glGet*v: WebGL's getParameter, with what it does not answer. */
  parameter(pname) {
    const gl = this.gl;
    switch (pname) {
      case GL.NUM_EXTENSIONS: return this.extensions.length;
      case GL.MAJOR_VERSION: return 2;
      case GL.MINOR_VERSION: return 0;
      case GL.NUM_COMPRESSED_TEXTURE_FORMATS: return gl.getParameter(gl.COMPRESSED_TEXTURE_FORMATS)?.length ?? 0;
      case GL.NUM_SHADER_BINARY_FORMATS: case GL.NUM_PROGRAM_BINARY_FORMATS: return 0;
      case GL.SHADER_BINARY_FORMATS: case GL.PROGRAM_BINARY_FORMATS: return [];
      case GL.SHADER_COMPILER: return 1;
      case GL.ARRAY_BUFFER_BINDING: return this.arrayBuffer;
      case GL.ELEMENT_ARRAY_BUFFER_BINDING: return this.vao.element;
      default: return gl.getParameter(pname);
    }
  }
  glGetBooleanv(pname, p) { this.putValues(p, this.parameter(pname), 'b'); }
  glGetIntegerv(pname, p) { this.putValues(p, this.parameter(pname), 'i'); }
  glGetFloatv(pname, p) { this.putValues(p, this.parameter(pname), 'f'); }
  glGetInteger64v(pname, p) { this.putValues(p, this.parameter(pname), 'q'); }
  glGetIntegeri_v(pname, i, p) { this.putValues(p, this.gl.getIndexedParameter(pname, i), 'i'); }
  glGetInteger64i_v(pname, i, p) { this.putValues(p, this.gl.getIndexedParameter(pname, i), 'q'); }
  glGetBufferParameteriv(t, pname, p) { this.putValues(p, this.gl.getBufferParameter(t, pname), 'i'); }
  glGetBufferParameteri64v(t, pname, p) { this.putValues(p, this.gl.getBufferParameter(t, pname), 'q'); }
  glGetError() { return this.gl.getError(); }
  glGetFramebufferAttachmentParameteriv(t, a, pname, p) {
    this.putValues(p, this.gl.getFramebufferAttachmentParameter(t, a, pname), 'i');
  }
  glGetProgramiv(name, pname, p) {
    const gl = this.gl;
    const prog = this.obj('program', name);
    let v;
    const longest = (n, get) => {
      let m = 0;
      for (let i = 0; i < n; i++) m = Math.max(m, (get(i)?.length ?? -1) + 1);
      return m;
    };
    switch (pname) {
      case GL.INFO_LOG_LENGTH: {
        const log = gl.getProgramInfoLog(prog);
        v = log ? log.length + 1 : 0;
        break;
      }
      case GL.ACTIVE_UNIFORM_MAX_LENGTH:
        v = longest(gl.getProgramParameter(prog, GL.ACTIVE_UNIFORMS), (i) => gl.getActiveUniform(prog, i)?.name);
        break;
      case GL.ACTIVE_ATTRIBUTE_MAX_LENGTH:
        v = longest(gl.getProgramParameter(prog, GL.ACTIVE_ATTRIBUTES), (i) => gl.getActiveAttrib(prog, i)?.name);
        break;
      case GL.ACTIVE_UNIFORM_BLOCK_MAX_NAME_LENGTH:
        v = longest(gl.getProgramParameter(prog, GL.ACTIVE_UNIFORM_BLOCKS), (i) => gl.getActiveUniformBlockName(prog, i));
        break;
      case GL.TRANSFORM_FEEDBACK_VARYING_MAX_LENGTH:
        v = longest(gl.getProgramParameter(prog, GL.TRANSFORM_FEEDBACK_VARYINGS),
          (i) => gl.getTransformFeedbackVarying(prog, i)?.name);
        break;
      case GL.PROGRAM_BINARY_LENGTH:
        v = 0;
        break;
      default:
        v = gl.getProgramParameter(prog, pname);
    }
    this.putValues(p, v, 'i');
  }
  glGetProgramInfoLog(name, bufSize, length, log) {
    this.putStr(this.gl.getProgramInfoLog(this.obj('program', name)) ?? '', bufSize, length, log);
  }
  glGetRenderbufferParameteriv(t, pname, p) { this.putValues(p, this.gl.getRenderbufferParameter(t, pname), 'i'); }
  glGetShaderiv(name, pname, p) {
    const gl = this.gl;
    const s = this.obj('shader', name);
    let v;
    if (pname === GL.INFO_LOG_LENGTH) {
      const log = gl.getShaderInfoLog(s);
      v = log ? log.length + 1 : 0;
    } else if (pname === GL.SHADER_SOURCE_LENGTH) {
      const src = gl.getShaderSource(s);
      v = src ? src.length + 1 : 0;
    } else v = gl.getShaderParameter(s, pname);
    this.putValues(p, v, 'i');
  }
  glGetShaderInfoLog(name, bufSize, length, log) {
    this.putStr(this.gl.getShaderInfoLog(this.obj('shader', name)) ?? '', bufSize, length, log);
  }
  glGetShaderPrecisionFormat(shader, precision, range, out) {
    const f = this.gl.getShaderPrecisionFormat(shader, precision);
    this.putValues(range, f ? [f.rangeMin, f.rangeMax] : [0, 0], 'i');
    this.putValues(out, f ? f.precision : 0, 'i');
  }
  glGetShaderSource(name, bufSize, length, src) {
    this.putStr(this.gl.getShaderSource(this.obj('shader', name)) ?? '', bufSize, length, src);
  }
  glGetTexParameterfv(t, pname, p) { this.putValues(p, this.gl.getTexParameter(t, pname), 'f'); }
  glGetTexParameteriv(t, pname, p) { this.putValues(p, this.gl.getTexParameter(t, pname), 'i'); }
  glGetUniformfv(prog, location, p) {
    const o = this.obj('program', prog);
    const l = o?.__locs?.[location];
    if (l) this.putValues(p, this.gl.getUniform(o, l), 'f');
  }
  glGetUniformiv(prog, location, p) {
    const o = this.obj('program', prog);
    const l = o?.__locs?.[location];
    if (l) this.putValues(p, this.gl.getUniform(o, l), 'i');
  }
  glGetUniformuiv(prog, location, p) { this.glGetUniformiv(prog, location, p); }
  glGetUniformLocation(prog, namePtr) {
    const o = this.obj('program', prog);
    if (!o?.__uniforms) return -1;
    const name = this.cstr(namePtr);
    let u = o.__uniforms.get(name);
    if (u) return u.first;
    const m = /^(.*)\[(\d+)\]$/.exec(name);
    if (m && (u = o.__uniforms.get(m[1])) && +m[2] < u.size) return u.first + +m[2];
    return -1;
  }
  vertexAttrib(i, pname) {
    const a = this.vao.attribs[i];
    if (a?.client) {
      switch (pname) {
        case GL.VERTEX_ATTRIB_ARRAY_BUFFER_BINDING: return 0;
        case GL.VERTEX_ATTRIB_ARRAY_SIZE: return a.size;
        case GL.VERTEX_ATTRIB_ARRAY_TYPE: return a.type;
        case GL.VERTEX_ATTRIB_ARRAY_STRIDE: return a.stride;
        case GL.VERTEX_ATTRIB_ARRAY_NORMALIZED: return a.norm;
        default:
      }
    }
    return this.gl.getVertexAttrib(i, pname);
  }
  glGetVertexAttribfv(i, pname, p) { this.putValues(p, this.vertexAttrib(i, pname), 'f'); }
  glGetVertexAttribiv(i, pname, p) { this.putValues(p, this.vertexAttrib(i, pname), 'i'); }
  glGetVertexAttribIiv(i, pname, p) { this.putValues(p, this.vertexAttrib(i, pname), 'i'); }
  glGetVertexAttribIuiv(i, pname, p) { this.putValues(p, this.vertexAttrib(i, pname), 'i'); }
  glGetVertexAttribPointerv(i, pname, p) {
    const a = this.vao.attribs[i];
    this.dv.setUint32(p, a?.client ? a.ptr : this.gl.getVertexAttribOffset(i, pname), true);
  }
  glHint(t, m) { this.gl.hint(t, m); }
  glIsBuffer(n) { return this.gl.isBuffer(this.obj('buffer', n)); }
  glIsEnabled(c) { return this.gl.isEnabled(c); }
  glIsFramebuffer(n) { return this.gl.isFramebuffer(this.obj('framebuffer', n)); }
  glIsProgram(n) { return this.gl.isProgram(this.obj('program', n)); }
  glIsRenderbuffer(n) { return this.gl.isRenderbuffer(this.obj('renderbuffer', n)); }
  glIsShader(n) { return this.gl.isShader(this.obj('shader', n)); }
  glIsTexture(n) { return this.gl.isTexture(this.obj('texture', n)); }
  glLineWidth(w) { this.gl.lineWidth(w); }
  glLinkProgram(name) {
    const o = this.obj('program', name);
    this.gl.linkProgram(o);
    if (o) this.mapUniforms(o);
  }
  glPixelStorei(pname, v) {
    if (this.store.has(pname)) this.store.set(pname, v);
    this.gl.pixelStorei(pname, v);
  }
  glPolygonOffset(f, u) { this.gl.polygonOffset(f, u); }
  glReadPixels(x, y, w, h, format, type, p) {
    const gl = this.gl;
    if (this.packBuffer) {
      gl.readPixels(x, y, w, h, format, type, p);
      return;
    }
    const n = this.imageBytes(w, h, 1, format, type, true);
    const T = arrayFor(type);
    const tmp = new T(Math.ceil(n / T.BYTES_PER_ELEMENT));
    gl.readPixels(x, y, w, h, format, type, tmp);
    this.u8.set(new Uint8Array(tmp.buffer, 0, n), p);
  }
  glReleaseShaderCompiler() {}
  glRenderbufferStorage(t, fmt, w, h) { this.gl.renderbufferStorage(t, fmt, w, h); }
  glSampleCoverage(v, invert) { this.gl.sampleCoverage(v, !!invert); }
  glScissor(x, y, w, h) { this.gl.scissor(x, y, w, h); }
  glShaderBinary() {}
  glShaderSource(name, count, strings, lengths) {
    let src = '';
    for (let i = 0; i < count; i++) {
      const s = this.dv.getUint32(strings + 4 * i, true);
      const len = lengths ? this.dv.getInt32(lengths + 4 * i, true) : -1;
      src += len >= 0 ? new TextDecoder().decode(this.u8.slice(s, s + len)) : this.cstr(s);
    }
    this.gl.shaderSource(this.obj('shader', name), src);
  }
  glStencilFunc(f, r, m) { this.gl.stencilFunc(f, r, m); }
  glStencilFuncSeparate(face, f, r, m) { this.gl.stencilFuncSeparate(face, f, r, m); }
  glStencilMask(m) { this.gl.stencilMask(m); }
  glStencilMaskSeparate(face, m) { this.gl.stencilMaskSeparate(face, m); }
  glStencilOp(a, b, c) { this.gl.stencilOp(a, b, c); }
  glStencilOpSeparate(face, a, b, c) { this.gl.stencilOpSeparate(face, a, b, c); }
  glTexImage2D(t, level, ifmt, w, h, border, format, type, p) {
    const gl = this.gl;
    if (this.unpackBuffer) gl.texImage2D(t, level, ifmt, w, h, border, format, type, p);
    else if (!p) gl.texImage2D(t, level, ifmt, w, h, border, format, type, null);
    else gl.texImage2D(t, level, ifmt, w, h, border, format, type, ...this.pixels(p, w, h, 1, format, type));
  }
  glTexParameterf(t, pname, v) { this.gl.texParameterf(t, pname, v); }
  glTexParameterfv(t, pname, p) { this.gl.texParameterf(t, pname, this.dv.getFloat32(p, true)); }
  glTexParameteri(t, pname, v) { this.gl.texParameteri(t, pname, v); }
  glTexParameteriv(t, pname, p) { this.gl.texParameteri(t, pname, this.dv.getInt32(p, true)); }
  glTexSubImage2D(t, level, x, y, w, h, format, type, p) {
    const gl = this.gl;
    if (this.unpackBuffer) gl.texSubImage2D(t, level, x, y, w, h, format, type, p);
    else gl.texSubImage2D(t, level, x, y, w, h, format, type, ...this.pixels(p, w, h, 1, format, type));
  }
  glUniform1f(l, x) { this.gl.uniform1f(this.loc(l), x); }
  glUniform2f(l, x, y) { this.gl.uniform2f(this.loc(l), x, y); }
  glUniform3f(l, x, y, z) { this.gl.uniform3f(this.loc(l), x, y, z); }
  glUniform4f(l, x, y, z, w) { this.gl.uniform4f(this.loc(l), x, y, z, w); }
  glUniform1i(l, x) { this.gl.uniform1i(this.loc(l), x); }
  glUniform2i(l, x, y) { this.gl.uniform2i(this.loc(l), x, y); }
  glUniform3i(l, x, y, z) { this.gl.uniform3i(this.loc(l), x, y, z); }
  glUniform4i(l, x, y, z, w) { this.gl.uniform4i(this.loc(l), x, y, z, w); }
  glUniform1ui(l, x) { this.gl.uniform1ui(this.loc(l), x >>> 0); }
  glUniform2ui(l, x, y) { this.gl.uniform2ui(this.loc(l), x >>> 0, y >>> 0); }
  glUniform3ui(l, x, y, z) { this.gl.uniform3ui(this.loc(l), x >>> 0, y >>> 0, z >>> 0); }
  glUniform4ui(l, x, y, z, w) { this.gl.uniform4ui(this.loc(l), x >>> 0, y >>> 0, z >>> 0, w >>> 0); }
  glUniform1fv(l, n, p) { if (n > 0) this.gl.uniform1fv(this.loc(l), this.f32(p, n)); }
  glUniform2fv(l, n, p) { if (n > 0) this.gl.uniform2fv(this.loc(l), this.f32(p, 2 * n)); }
  glUniform3fv(l, n, p) { if (n > 0) this.gl.uniform3fv(this.loc(l), this.f32(p, 3 * n)); }
  glUniform4fv(l, n, p) { if (n > 0) this.gl.uniform4fv(this.loc(l), this.f32(p, 4 * n)); }
  glUniform1iv(l, n, p) { if (n > 0) this.gl.uniform1iv(this.loc(l), this.i32(p, n)); }
  glUniform2iv(l, n, p) { if (n > 0) this.gl.uniform2iv(this.loc(l), this.i32(p, 2 * n)); }
  glUniform3iv(l, n, p) { if (n > 0) this.gl.uniform3iv(this.loc(l), this.i32(p, 3 * n)); }
  glUniform4iv(l, n, p) { if (n > 0) this.gl.uniform4iv(this.loc(l), this.i32(p, 4 * n)); }
  glUniform1uiv(l, n, p) { if (n > 0) this.gl.uniform1uiv(this.loc(l), this.u32(p, n)); }
  glUniform2uiv(l, n, p) { if (n > 0) this.gl.uniform2uiv(this.loc(l), this.u32(p, 2 * n)); }
  glUniform3uiv(l, n, p) { if (n > 0) this.gl.uniform3uiv(this.loc(l), this.u32(p, 3 * n)); }
  glUniform4uiv(l, n, p) { if (n > 0) this.gl.uniform4uiv(this.loc(l), this.u32(p, 4 * n)); }
  glUniformMatrix2fv(l, n, t, p) { if (n > 0) this.gl.uniformMatrix2fv(this.loc(l), !!t, this.f32(p, 4 * n)); }
  glUniformMatrix3fv(l, n, t, p) { if (n > 0) this.gl.uniformMatrix3fv(this.loc(l), !!t, this.f32(p, 9 * n)); }
  glUniformMatrix4fv(l, n, t, p) { if (n > 0) this.gl.uniformMatrix4fv(this.loc(l), !!t, this.f32(p, 16 * n)); }
  glUniformMatrix2x3fv(l, n, t, p) { if (n > 0) this.gl.uniformMatrix2x3fv(this.loc(l), !!t, this.f32(p, 6 * n)); }
  glUniformMatrix3x2fv(l, n, t, p) { if (n > 0) this.gl.uniformMatrix3x2fv(this.loc(l), !!t, this.f32(p, 6 * n)); }
  glUniformMatrix2x4fv(l, n, t, p) { if (n > 0) this.gl.uniformMatrix2x4fv(this.loc(l), !!t, this.f32(p, 8 * n)); }
  glUniformMatrix4x2fv(l, n, t, p) { if (n > 0) this.gl.uniformMatrix4x2fv(this.loc(l), !!t, this.f32(p, 8 * n)); }
  glUniformMatrix3x4fv(l, n, t, p) { if (n > 0) this.gl.uniformMatrix3x4fv(this.loc(l), !!t, this.f32(p, 12 * n)); }
  glUniformMatrix4x3fv(l, n, t, p) { if (n > 0) this.gl.uniformMatrix4x3fv(this.loc(l), !!t, this.f32(p, 12 * n)); }
  glUseProgram(name) {
    this.program = this.obj('program', name);
    this.gl.useProgram(this.program);
  }
  glValidateProgram(name) { this.gl.validateProgram(this.obj('program', name)); }
  glVertexAttrib1f(i, x) { this.gl.vertexAttrib1f(i, x); }
  glVertexAttrib2f(i, x, y) { this.gl.vertexAttrib2f(i, x, y); }
  glVertexAttrib3f(i, x, y, z) { this.gl.vertexAttrib3f(i, x, y, z); }
  glVertexAttrib4f(i, x, y, z, w) { this.gl.vertexAttrib4f(i, x, y, z, w); }
  glVertexAttrib1fv(i, p) { this.gl.vertexAttrib1fv(i, this.f32(p, 1)); }
  glVertexAttrib2fv(i, p) { this.gl.vertexAttrib2fv(i, this.f32(p, 2)); }
  glVertexAttrib3fv(i, p) { this.gl.vertexAttrib3fv(i, this.f32(p, 3)); }
  glVertexAttrib4fv(i, p) { this.gl.vertexAttrib4fv(i, this.f32(p, 4)); }
  glVertexAttribI4i(i, x, y, z, w) { this.gl.vertexAttribI4i(i, x, y, z, w); }
  glVertexAttribI4ui(i, x, y, z, w) { this.gl.vertexAttribI4ui(i, x >>> 0, y >>> 0, z >>> 0, w >>> 0); }
  glVertexAttribI4iv(i, p) { this.gl.vertexAttribI4iv(i, this.i32(p, 4)); }
  glVertexAttribI4uiv(i, p) { this.gl.vertexAttribI4uiv(i, this.u32(p, 4)); }

  /** glVertexAttribPointer: with no array buffer bound, `ptr` is guest memory. */
  attribPointer(i, size, type, norm, stride, ptr, integer) {
    const a = (this.vao.attribs[i] ??= {});
    Object.assign(a, { size, type, norm: !!norm, stride, ptr, integer, client: !this.arrayBuffer });
    if (a.client) return;
    if (integer) this.gl.vertexAttribIPointer(i, size, type, stride, ptr);
    else this.gl.vertexAttribPointer(i, size, type, !!norm, stride, ptr);
  }
  glVertexAttribPointer(i, size, type, norm, stride, ptr) { this.attribPointer(i, size, type, norm, stride, ptr, false); }
  glVertexAttribIPointer(i, size, type, stride, ptr) { this.attribPointer(i, size, type, false, stride, ptr, true); }
  glViewport(x, y, w, h) { this.gl.viewport(x, y, w, h); }

  // OpenGL ES 3.0.
  glReadBuffer(m) { this.gl.readBuffer(m); }
  glDrawRangeElements(mode, start, end, count, type, ptr) {
    this.drawIndexed(mode, count, type, ptr, (o) => this.gl.drawRangeElements(mode, start, end, count, type, o), end + 1);
  }
  glTexImage3D(t, level, ifmt, w, h, d, border, format, type, p) {
    const gl = this.gl;
    if (this.unpackBuffer) gl.texImage3D(t, level, ifmt, w, h, d, border, format, type, p);
    else if (!p) gl.texImage3D(t, level, ifmt, w, h, d, border, format, type, null);
    else gl.texImage3D(t, level, ifmt, w, h, d, border, format, type, ...this.pixels(p, w, h, d, format, type));
  }
  glTexSubImage3D(t, level, x, y, z, w, h, d, format, type, p) {
    const gl = this.gl;
    if (this.unpackBuffer) gl.texSubImage3D(t, level, x, y, z, w, h, d, format, type, p);
    else gl.texSubImage3D(t, level, x, y, z, w, h, d, format, type, ...this.pixels(p, w, h, d, format, type));
  }
  glCopyTexSubImage3D(t, level, xo, yo, zo, x, y, w, h) { this.gl.copyTexSubImage3D(t, level, xo, yo, zo, x, y, w, h); }
  glCompressedTexImage3D(t, level, fmt, w, h, d, border, size, data) {
    if (this.unpackBuffer) this.gl.compressedTexImage3D(t, level, fmt, w, h, d, border, size, data);
    else this.gl.compressedTexImage3D(t, level, fmt, w, h, d, border, this.u8, data, size);
  }
  glCompressedTexSubImage3D(t, level, x, y, z, w, h, d, fmt, size, data) {
    if (this.unpackBuffer) this.gl.compressedTexSubImage3D(t, level, x, y, z, w, h, d, fmt, size, data);
    else this.gl.compressedTexSubImage3D(t, level, x, y, z, w, h, d, fmt, this.u8, data, size);
  }
  glGenQueries(n, p) { this.gen('query', () => this.gl.createQuery(), n, p); }
  glDeleteQueries(n, p) { this.del('query', (o) => this.gl.deleteQuery(o), n, p); }
  glIsQuery(n) { return this.gl.isQuery(this.obj('query', n)); }
  glBeginQuery(t, n) { this.gl.beginQuery(t, this.obj('query', n)); }
  glEndQuery(t) { this.gl.endQuery(t); }
  glGetQueryiv(t, pname, p) { this.putValues(p, this.gl.getQuery(t, pname), 'i'); }
  glGetQueryObjectuiv(n, pname, p) { this.putValues(p, this.gl.getQueryParameter(this.obj('query', n), pname), 'i'); }
  glDrawBuffers(n, p) { this.gl.drawBuffers(Array.from(this.u32(p, n))); }
  glBlitFramebuffer(a, b, c, d, e, f, g, h, mask, filter) { this.gl.blitFramebuffer(a, b, c, d, e, f, g, h, mask, filter); }
  glRenderbufferStorageMultisample(t, s, fmt, w, h) { this.gl.renderbufferStorageMultisample(t, s, fmt, w, h); }
  glFramebufferTextureLayer(t, a, tex, level, layer) {
    this.gl.framebufferTextureLayer(t, a, this.obj('texture', tex), level, layer);
  }
  glBindVertexArray(name) {
    const gl = this.gl;
    const o = this.bound('vao', name, () => gl.createVertexArray());
    if (!this.vaos.has(name)) this.vaos.set(name, this.vaoState());
    this.vao = this.vaos.get(name);
    gl.bindVertexArray(o);
  }
  glDeleteVertexArrays(n, p) {
    for (let i = 0; i < n; i++) {
      const name = this.dv.getUint32(p + 4 * i, true);
      if (this.vao === this.vaos.get(name)) this.vao = this.vaos.get(0);
      this.vaos.delete(name);
    }
    this.del('vao', (o) => this.gl.deleteVertexArray(o), n, p);
  }
  glGenVertexArrays(n, p) { this.gen('vao', () => this.gl.createVertexArray(), n, p); }
  glIsVertexArray(n) { return this.gl.isVertexArray(this.obj('vao', n)); }
  glBeginTransformFeedback(m) { this.gl.beginTransformFeedback(m); }
  glEndTransformFeedback() { this.gl.endTransformFeedback(); }
  glBindBufferRange(t, i, b, offset, size) {
    const gl = this.gl;
    gl.bindBufferRange(t, i, this.bound('buffer', b, () => gl.createBuffer()), offset, size);
  }
  glBindBufferBase(t, i, b) {
    const gl = this.gl;
    gl.bindBufferBase(t, i, this.bound('buffer', b, () => gl.createBuffer()));
  }
  glTransformFeedbackVaryings(prog, n, p, mode) {
    const names = [];
    for (let i = 0; i < n; i++) names.push(this.cstr(this.dv.getUint32(p + 4 * i, true)));
    this.gl.transformFeedbackVaryings(this.obj('program', prog), names, mode);
  }
  glGetTransformFeedbackVarying(prog, i, bufSize, length, size, type, name) {
    this.activeInfo(this.gl.getTransformFeedbackVarying(this.obj('program', prog), i), bufSize, length, size, type, name);
  }
  glGetFragDataLocation(prog, name) { return this.gl.getFragDataLocation(this.obj('program', prog), this.cstr(name)); }
  glClearBufferiv(b, i, p) { this.gl.clearBufferiv(b, i, this.i32(p, 4)); }
  glClearBufferuiv(b, i, p) { this.gl.clearBufferuiv(b, i, this.u32(p, 4)); }
  glClearBufferfv(b, i, p) { this.gl.clearBufferfv(b, i, this.f32(p, 4)); }
  glClearBufferfi(b, i, depth, stencil) { this.gl.clearBufferfi(b, i, depth, stencil); }
  glCopyBufferSubData(r, w, ro, wo, size) { this.gl.copyBufferSubData(r, w, ro, wo, size); }
  glGetUniformIndices(prog, n, names, out) {
    const list = [];
    for (let i = 0; i < n; i++) list.push(this.cstr(this.dv.getUint32(names + 4 * i, true)));
    this.putValues(out, this.gl.getUniformIndices(this.obj('program', prog), list), 'i');
  }
  glGetActiveUniformsiv(prog, n, indices, pname, out) {
    this.putValues(out, this.gl.getActiveUniforms(this.obj('program', prog), Array.from(this.u32(indices, n)), pname), 'i');
  }
  glGetUniformBlockIndex(prog, name) { return this.gl.getUniformBlockIndex(this.obj('program', prog), this.cstr(name)); }
  glGetActiveUniformBlockiv(prog, i, pname, out) {
    const o = this.obj('program', prog);
    const v = pname === GL.UNIFORM_BLOCK_NAME_LENGTH
      ? (this.gl.getActiveUniformBlockName(o, i)?.length ?? -1) + 1
      : this.gl.getActiveUniformBlockParameter(o, i, pname);
    this.putValues(out, v, 'i');
  }
  glGetActiveUniformBlockName(prog, i, bufSize, length, name) {
    this.putStr(this.gl.getActiveUniformBlockName(this.obj('program', prog), i) ?? '', bufSize, length, name);
  }
  glUniformBlockBinding(prog, i, b) { this.gl.uniformBlockBinding(this.obj('program', prog), i, b); }
  glDrawArraysInstanced(mode, first, count, n) {
    if (this.hasClientArrays()) this.clientArrays(first + count);
    this.gl.drawArraysInstanced(mode, first, count, n);
  }
  glDrawElementsInstanced(mode, count, type, ptr, n) {
    this.drawIndexed(mode, count, type, ptr, (o) => this.gl.drawElementsInstanced(mode, count, type, o, n));
  }
  glFenceSync(cond, flags) { return this.add('sync', this.gl.fenceSync(cond, flags)); }
  glIsSync(s) { return this.gl.isSync(this.obj('sync', s)); }
  glDeleteSync(s) {
    const o = this.obj('sync', s);
    if (!o) return;
    this.gl.deleteSync(o);
    this.names.sync.delete(s);
  }
  // WebGL never blocks for the GPU: the wait is 0 (its MAX_CLIENT_WAIT_TIMEOUT).
  glClientWaitSync(s, flags) { return this.gl.clientWaitSync(this.obj('sync', s), flags, 0); }
  glWaitSync(s, flags) { this.gl.waitSync(this.obj('sync', s), flags, -1); }
  glGetSynciv(s, pname, bufSize, length, values) {
    if (bufSize < 1) return;
    this.putValues(values, this.gl.getSyncParameter(this.obj('sync', s), pname), 'i');
    if (length) this.dv.setInt32(length, 1, true);
  }
  glGenSamplers(n, p) { this.gen('sampler', () => this.gl.createSampler(), n, p); }
  glDeleteSamplers(n, p) { this.del('sampler', (o) => this.gl.deleteSampler(o), n, p); }
  glIsSampler(n) { return this.gl.isSampler(this.obj('sampler', n)); }
  glBindSampler(unit, n) { this.gl.bindSampler(unit, this.obj('sampler', n)); }
  glSamplerParameteri(s, pname, v) { this.gl.samplerParameteri(this.obj('sampler', s), pname, v); }
  glSamplerParameteriv(s, pname, p) { this.gl.samplerParameteri(this.obj('sampler', s), pname, this.dv.getInt32(p, true)); }
  glSamplerParameterf(s, pname, v) { this.gl.samplerParameterf(this.obj('sampler', s), pname, v); }
  glSamplerParameterfv(s, pname, p) { this.gl.samplerParameterf(this.obj('sampler', s), pname, this.dv.getFloat32(p, true)); }
  glGetSamplerParameteriv(s, pname, p) { this.putValues(p, this.gl.getSamplerParameter(this.obj('sampler', s), pname), 'i'); }
  glGetSamplerParameterfv(s, pname, p) { this.putValues(p, this.gl.getSamplerParameter(this.obj('sampler', s), pname), 'f'); }
  glVertexAttribDivisor(i, d) { this.gl.vertexAttribDivisor(i, d); }
  glBindTransformFeedback(t, n) {
    const gl = this.gl;
    gl.bindTransformFeedback(t, this.bound('transformFeedback', n, () => gl.createTransformFeedback()));
  }
  glDeleteTransformFeedbacks(n, p) { this.del('transformFeedback', (o) => this.gl.deleteTransformFeedback(o), n, p); }
  glGenTransformFeedbacks(n, p) { this.gen('transformFeedback', () => this.gl.createTransformFeedback(), n, p); }
  glIsTransformFeedback(n) { return this.gl.isTransformFeedback(this.obj('transformFeedback', n)); }
  glPauseTransformFeedback() { this.gl.pauseTransformFeedback(); }
  glResumeTransformFeedback() { this.gl.resumeTransformFeedback(); }
  glGetProgramBinary(prog, bufSize, length) { if (length) this.dv.setInt32(length, 0, true); }
  glProgramBinary() {}
  glProgramParameteri() {}
  glInvalidateFramebuffer(t, n, p) { this.gl.invalidateFramebuffer(t, Array.from(this.u32(p, n))); }
  glInvalidateSubFramebuffer(t, n, p, x, y, w, h) {
    this.gl.invalidateSubFramebuffer(t, Array.from(this.u32(p, n)), x, y, w, h);
  }
  glTexStorage2D(t, levels, fmt, w, h) { this.gl.texStorage2D(t, levels, fmt, w, h); }
  glTexStorage3D(t, levels, fmt, w, h, d) { this.gl.texStorage3D(t, levels, fmt, w, h, d); }
  glGetInternalformativ(t, fmt, pname, bufSize, p) {
    const v = this.gl.getInternalformatParameter(t, fmt, GL.SAMPLES);
    if (pname === GL.NUM_SAMPLE_COUNTS) this.putValues(p, v?.length ?? 0, 'i');
    else this.putValues(p, v ?? [], 'i', bufSize);
  }
}
