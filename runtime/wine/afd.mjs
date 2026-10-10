// Sockets: the AFD device (\Device\Afd) ws2_32 drives with ioctls
// (include/wine/afd.h), which ntdll's Unix side implements with the host's
// sockets in Wine. A page has no sockets, so this one is in memory: IPv4 UDP
// datagrams travel between the program's own sockets over loopback (and
// broadcast reaches every socket bound to the port), which is what games
// use to talk to their own server in single-player. Datagrams to other
// addresses are dropped, as by a network that loses them; TCP connections
// are refused. One process: every socket is this program's.

const STATUS_SUCCESS = 0;
const STATUS_BUFFER_OVERFLOW = 0x80000005;
const STATUS_INVALID_PARAMETER = 0xc000000d;
const STATUS_NOT_SUPPORTED = 0xc00000bb;
const STATUS_DEVICE_NOT_READY = 0xc00000a3; // WSAEWOULDBLOCK
const STATUS_CONNECTION_REFUSED = 0xc0000236;
const STATUS_ADDRESS_ALREADY_ASSOCIATED = 0xc0000238; // WSAEADDRINUSE
const STATUS_INVALID_CONNECTION = 0xc0000140; // WSAENOTCONN
const STATUS_TIMEOUT = 0x102;

const AF_INET = 2;
const SOCK_STREAM = 1;
const SOCK_DGRAM = 2;

const AFD_POLL_READ = 0x1;
const AFD_POLL_WRITE = 0x4;

const afd = (n) => (0x12 << 16) | (n << 2); // CTL_CODE(FILE_DEVICE_NETWORK, n, METHOD_BUFFERED, FILE_ANY_ACCESS)
const ms = (n) => (1 << 16) | (n << 2) | 3; // CTL_CODE(FILE_DEVICE_BEEP, n, METHOD_NEITHER, ...)
const IOCTL = {
  BIND: ms(0x800),
  LISTEN: ms(0x802),
  RECV: ms(0x805),
  POLL: (1 << 16) | (0x809 << 2), // METHOD_BUFFERED
  GETSOCKNAME: ms(0x80b),
  EVENT_SELECT: ms(0x821),
  GET_EVENTS: ms(0x822),
  CREATE: afd(200),
  CONNECT: afd(203),
  SHUTDOWN: afd(204),
  RECVMSG: afd(205),
  SENDMSG: afd(206),
  FIONBIO: afd(209),
  FIONREAD: afd(211),
  GET_INTERFACE_LIST: afd(213),
  GETPEERNAME: afd(216),
  GET_INFO: afd(218),
  GET_SO_ERROR: afd(222),
};
// Socket options: IOCTL_AFD_WINE_GET_/SET_* (220 to 304). Kept per socket;
// reads return what was set, or these defaults.
const OPTION_DEFAULTS = { 230: 65536, 236: 65536 }; // SO_RCVBUF, SO_SNDBUF
const SET_SO_BROADCAST = afd(221);

let nextPort = 49152;

/** Every bound UDP socket of the process, by port. */
function ports(h) {
  return (h.afdPorts ??= new Map());
}

/** A new socket object for a \Device\Afd handle. */
export function afdOpen() {
  return { type: 'socket', created: false, nonblocking: false, port: 0, peer: null, queue: [], options: new Map() };
}

export function afdClose(h, s) {
  if (s.port && ports(h).get(s.port)?.includes(s)) {
    const list = ports(h).get(s.port).filter((x) => x !== s);
    if (list.length) ports(h).set(s.port, list);
    else ports(h).delete(s.port);
  }
}

function readAddr(h, p) {
  return { family: h.u16(p), port: (h.m.u8[p + 2] << 8) | h.m.u8[p + 3], ip: [...h.m.u8.subarray(p + 4, p + 8)] };
}

function writeAddr(h, p, ip, port) {
  h.m.u8.fill(0, p, p + 16);
  h.w16(p, AF_INET);
  h.m.u8[p + 2] = port >> 8;
  h.m.u8[p + 3] = port & 0xff;
  h.m.u8.set(ip, p + 4);
}

function bindTo(h, s, port) {
  if (!port) {
    do port = nextPort++;
    while (ports(h).has(port));
  } else if (ports(h).has(port) && !s.options.get(afd(234))) {
    // Taken, and this one does not share (SO_REUSEADDR).
    if (!ports(h).get(port).every((o) => o.options.get(afd(234)))) return STATUS_ADDRESS_ALREADY_ASSOCIATED;
  }
  s.port = port;
  ports(h).set(port, [...(ports(h).get(port) ?? []), s]);
  return STATUS_SUCCESS;
}

/** Sends a datagram from socket `s` to ip:port. */
function deliver(h, s, ip, port, data) {
  const loopback = ip[0] === 127 || ip.every((b) => b === 0);
  const broadcast = ip.every((b) => b === 255) || ip[3] === 255;
  if (!loopback && !broadcast) return;
  for (const o of ports(h).get(port) ?? []) {
    if (o.sockType !== SOCK_DGRAM) continue;
    o.queue.push({ data, ip: [127, 0, 0, 1], port: s.port });
    if (!broadcast) break;
  }
}

/** WSABUF[count] at p (32-bit: {ULONG len; char *buf}). */
function buffers(h, p, count) {
  const out = [];
  for (let i = 0; i < count; i++) out.push({ len: h.u32(p + i * 8), ptr: h.u32(p + i * 8 + 4) });
  return out;
}

/** NtDeviceIoControlFile on a socket: the status, or a blocked wait. */
export function afdIoctl(h, s, a, iosb) {
  const [piosb, code, inp, inlen, outp, outlen] = [a(4), a(5), a(6), a(7), a(8), a(9)];
  const done = (status, info = 0) => (iosb(h, piosb, status, info), status);
  switch (code) {
    case IOCTL.CREATE: {
      const [family, type] = [h.u32(inp), h.u32(inp + 4)];
      if (family !== AF_INET || (type !== SOCK_DGRAM && type !== SOCK_STREAM)) {
        h.fixme(`socket family ${family} type ${type} not supported`);
        return done(STATUS_NOT_SUPPORTED);
      }
      Object.assign(s, { created: true, family, sockType: type, protocol: h.u32(inp + 8) });
      return done(STATUS_SUCCESS);
    }
    case IOCTL.BIND: {
      if (s.port) return done(STATUS_INVALID_PARAMETER);
      const addr = readAddr(h, inp + 4);
      const status = bindTo(h, s, addr.port);
      if (status) return done(status);
      s.ip = addr.ip;
      if (outp && outlen >= 16) writeAddr(h, outp, s.ip, s.port);
      return done(STATUS_SUCCESS, 16);
    }
    case IOCTL.GETSOCKNAME:
      if (!s.port) return done(STATUS_INVALID_PARAMETER);
      writeAddr(h, outp, s.ip ?? [0, 0, 0, 0], s.port);
      return done(STATUS_SUCCESS, 16);
    case IOCTL.GETPEERNAME:
      if (!s.peer) return done(STATUS_INVALID_CONNECTION);
      writeAddr(h, outp, s.peer.ip, s.peer.port);
      return done(STATUS_SUCCESS, 16);
    case IOCTL.GET_INFO:
      h.w32(outp, s.family ?? AF_INET);
      h.w32(outp + 4, s.sockType ?? SOCK_DGRAM);
      h.w32(outp + 8, s.protocol ?? 0);
      return done(STATUS_SUCCESS, 12);
    case IOCTL.FIONBIO:
      s.nonblocking = !!h.u32(inp);
      return done(STATUS_SUCCESS);
    case IOCTL.FIONREAD:
      h.w32(outp, s.queue[0]?.data.length ?? 0);
      return done(STATUS_SUCCESS, 4);
    case IOCTL.GET_SO_ERROR:
      h.w32(outp, 0);
      return done(STATUS_SUCCESS, 4);
    case IOCTL.LISTEN:
      if (!s.port) bindTo(h, s, 0);
      return done(STATUS_SUCCESS);
    case IOCTL.SHUTDOWN:
      return done(STATUS_SUCCESS);
    case IOCTL.CONNECT: {
      const addr = readAddr(h, inp + 8);
      if (s.sockType === SOCK_STREAM) return done(STATUS_CONNECTION_REFUSED);
      if (!s.port) bindTo(h, s, 0);
      s.peer = addr;
      return done(STATUS_SUCCESS);
    }
    case IOCTL.SENDMSG: {
      const addrPtr = h.u32(inp);
      const bufs = buffers(h, h.u32(inp + 24), h.u32(inp + 20));
      const to = addrPtr ? readAddr(h, addrPtr) : s.peer;
      if (!to) return done(STATUS_INVALID_CONNECTION);
      if (s.sockType === SOCK_STREAM) return done(STATUS_INVALID_CONNECTION);
      if (!s.port) bindTo(h, s, 0);
      const total = bufs.reduce((n, b) => n + b.len, 0);
      const data = new Uint8Array(total);
      let o = 0;
      for (const b of bufs) (data.set(h.m.u8.subarray(b.ptr, b.ptr + b.len), o), (o += b.len));
      deliver(h, s, to.ip, to.port, data);
      return done(STATUS_SUCCESS, total);
    }
    case IOCTL.RECVMSG:
    case IOCTL.RECV: {
      const msg = code === IOCTL.RECVMSG;
      const bufs = msg ? buffers(h, h.u32(inp + 40), h.u32(inp + 36)) : buffers(h, h.u32(inp), h.u32(inp + 4));
      const receive = () => {
        if (!s.queue.length) return undefined;
        const d = s.queue.shift();
        let o = 0;
        for (const b of bufs) {
          const n = Math.min(b.len, d.data.length - o);
          h.m.u8.set(d.data.subarray(o, o + n), b.ptr);
          o += n;
        }
        if (msg) {
          const [addrPtr, lenPtr, flagsPtr] = [h.u32(inp + 8), h.u32(inp + 16), h.u32(inp + 24)];
          if (addrPtr) writeAddr(h, addrPtr, d.ip, d.port);
          if (lenPtr) h.w32(lenPtr, 16);
          if (flagsPtr) h.w32(flagsPtr, 0);
        }
        return done(o < d.data.length ? STATUS_BUFFER_OVERFLOW : STATUS_SUCCESS, o);
      };
      if (!s.port) return done(STATUS_INVALID_PARAMETER);
      const now = receive();
      if (now !== undefined) return now;
      if (s.nonblocking) return done(STATUS_DEVICE_NOT_READY);
      return h.threads.block('recv', h.sys.ret, h.sys.esp, receive, Infinity);
    }
    case IOCTL.POLL: {
      // {LONGLONG timeout; count; exclusive; padding; {SOCKET; flags; status}[]}
      const timeout = (BigInt(h.u32(inp + 4) | 0) << 32n) | BigInt(h.u32(inp));
      const count = h.u32(inp + 8);
      const wanted = [];
      for (let i = 0; i < count; i++) wanted.push({ handle: h.u32(inp + 16 + i * 12), flags: h.u32(inp + 20 + i * 12) });
      const check = () => {
        const ready = [];
        for (const w of wanted) {
          const o = h.object(w.handle);
          let flags = 0;
          if (o?.type === 'socket') {
            if (o.queue.length) flags |= AFD_POLL_READ;
            flags |= AFD_POLL_WRITE;
          }
          flags &= w.flags;
          if (flags) ready.push({ handle: w.handle, flags });
        }
        if (!ready.length) return undefined;
        return written(ready);
      };
      const written = (ready) => {
        h.w32(outp + 8, ready.length);
        ready.forEach((r, i) => {
          h.w32(outp + 16 + i * 12, r.handle);
          h.w32(outp + 20 + i * 12, r.flags);
          h.w32(outp + 24 + i * 12, 0);
        });
        return done(STATUS_SUCCESS, 16 + ready.length * 12);
      };
      const now = check();
      if (now !== undefined) return now;
      // Relative (negative, in 100 ns), absolute, or infinite.
      const deadline = timeout >= 0x7fffffffffffffffn ? Infinity : timeout < 0n ? performance.now() + Number(-timeout) / 10000 : performance.now();
      if (deadline <= performance.now()) return written([]);
      const r = h.threads.block('select', h.sys.ret, h.sys.esp, check, deadline, STATUS_TIMEOUT);
      if (r.status === STATUS_TIMEOUT) return written([]);
      return r;
    }
    case IOCTL.GET_INTERFACE_LIST: {
      // SIO_GET_INTERFACE_LIST: the loopback interface, as INTERFACE_INFO
      // {iiFlags; address, broadcast, netmask as 24-byte sockaddr_gen}.
      if (outlen < 76) return done(STATUS_BUFFER_OVERFLOW);
      h.m.u8.fill(0, outp, outp + 76);
      h.w32(outp, 0x1 | 0x2 | 0x4); // IFF_UP | IFF_BROADCAST | IFF_LOOPBACK
      writeAddr(h, outp + 4, [127, 0, 0, 1], 0);
      writeAddr(h, outp + 28, [127, 255, 255, 255], 0);
      writeAddr(h, outp + 52, [255, 0, 0, 0], 0);
      return done(STATUS_SUCCESS, 76);
    }
    case IOCTL.EVENT_SELECT:
      s.eventSelect = { event: h.u32(inp), mask: h.u32(inp + 4) };
      return done(STATUS_SUCCESS);
    case IOCTL.GET_EVENTS:
      h.m.u8.fill(0, outp, outp + 56);
      h.w32(outp, (s.queue.length ? AFD_POLL_READ : 0) | AFD_POLL_WRITE);
      return done(STATUS_SUCCESS, 56);
    default: {
      const n = (code >> 2) & 0xfff;
      if (((code >> 16) & 0xffff) === 0x12 && n >= 220 && n <= 304) {
        // An option: odd/even numbering varies, so a call with input sets
        // it and one with output reads it.
        if (inlen >= 4 && inp) {
          s.options.set(code, h.u32(inp));
          if (code === SET_SO_BROADCAST) s.broadcast = !!h.u32(inp);
          return done(STATUS_SUCCESS);
        }
        if (outp && outlen >= 4) {
          const set = [...s.options].find(([k]) => Math.abs(((k >> 2) & 0xfff) - n) === 1)?.[1];
          h.w32(outp, set ?? OPTION_DEFAULTS[n] ?? 0);
          return done(STATUS_SUCCESS, 4);
        }
      }
      h.fixme(`socket ioctl ${code.toString(16)} not implemented`);
      return done(STATUS_NOT_SUPPORTED);
    }
  }
}
