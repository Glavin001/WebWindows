//! Stack frame slots in WebAssembly locals.
//!
//! 32-bit compilers keep locals and spilled registers in the stack frame
//! (`[esp+N]`, `[ebp-N]`): x86 has eight registers. Translated, every such
//! access is a WebAssembly load or store, and a value carried around a loop
//! in a frame slot goes through memory on every iteration even when V8 has
//! registers free. An Emscripten build of the same source keeps those
//! values in locals. This pass does the same for translated code: a frame
//! slot lives in a local, and goes to memory only where something other
//! than the function's own code could look at it.
//!
//! **Which accesses are slots.** Addresses are tracked as the function's
//! entry `esp` plus a constant (`Sym::Frame`), through `esp`, `ebp` and any
//! other register or temporary computed from them. A slot is a range below
//! the entry `esp` (the function's own frame, not its arguments or return
//! address) that every access with a known address reaches with the same
//! offset and size.
//!
//! **Where the frame must be current** (dirty slots are written back):
//! calls (the callee sees the frame through `esp`) and every other way out
//! of the function; explicit faults (`FaultIf`); loop back edges, where a
//! preempted thread resumes or `osr` re-enters from memory; accesses to the
//! frame at an unknown offset (`[esp+eax*4+N]`, `rep movs` into the frame);
//! and before the last register that locates the frame loses track of it.
//! **Where it may have changed** (slots are reloaded before their next
//! use): after calls and after stores to the frame at an unknown offset.
//!
//! **Where the frame's address escapes** (stored to memory, or held in a
//! register other than `esp` and `ebp` where code outside the function can
//! see it, at calls and exits), code the pass cannot see could reach the
//! frame between write-back points: another thread, or this function
//! through a pointer it loaded. The objects whose addresses escape, from the
//! lowest such address up, stay in memory; slots below it do not (C code
//! reaches only the object a pointer points into, which starts at the
//! address taken of it). An address escaping at an unknown offset keeps the
//! whole frame in memory.
//!
//! **Calls.** A direct call to a function in the module whose every return
//! pops the same number of bytes (`stack_pops`) leaves `esp` known after
//! the call; any other call leaves it unknown, and accesses through it are
//! then at unknown offsets. `ebp`, `ebx`, `esi` and `edi` keep what they
//! held (see `after_call`).
//!
//! **Faults.** An access violation sees the frame's slots as last written
//! back, as memory traps already have it for the registers
//! (docs/memory-traps.md). Faithful mode promises exact state at every
//! fault, which this pass does not keep.
//!
//! **Status: experimental, off by default** (`wwt translate --frame-slots`,
//! `Config::frame_slots`): correct on the test suites, but no measurable
//! gain on CoreMark or Lua yet (docs/performance.md, "What didn't help").
//!
//! **Memory below `esp` is dead:** a slot that `esp` moves above (`pop`,
//! `ret`, `add esp`) is dropped without being written back. Windows may
//! overwrite memory below the stack pointer at any time (exception
//! dispatch, APCs), so code does not rely on it.

use std::collections::HashMap;
use std::sync::Arc;

use crate::ir::*;

const NS: usize = NUM_STATE as usize;

/// What a value is, relative to the function's entry `esp`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sym {
    /// Not yet known (optimistic start of the analysis).
    Top,
    /// Entry `esp` plus this offset (32-bit wrapping).
    Frame(i32),
    /// Either entry `esp` plus this offset, or not derived from the frame's
    /// address (a register holding a frame address on some paths and other
    /// values on others).
    Maybe(i32),
    /// Not derived from the frame's address.
    Other,
    /// Possibly derived from the frame's address, at an unknown offset.
    Mixed,
}

impl Sym {
    fn meet(self, o: Sym) -> Sym {
        use Sym::*;
        match (self, o) {
            (Top, x) | (x, Top) => x,
            (Frame(a), Frame(b)) if a == b => self,
            (Other, Other) => Other,
            (Frame(a) | Maybe(a), Other) | (Other, Frame(a) | Maybe(a)) => Maybe(a),
            (Frame(a) | Maybe(a), Frame(b) | Maybe(b)) if a == b => Maybe(a),
            _ => Mixed,
        }
    }
    fn framey(self) -> bool {
        matches!(self, Sym::Frame(_) | Sym::Maybe(_) | Sym::Mixed)
    }
}

type State = [Sym; NS];

/// A slot kept in a local: bytes `[entry esp + offset, + size)`.
#[derive(Clone, Debug)]
pub struct SlotHome {
    pub v: V,
    pub offset: i32,
    pub size: u8,
}

/// What the pass leaves for code generation: `osr` reloads the slots live
/// at a loop header when it re-enters there.
#[derive(Clone, Debug, Default)]
pub struct FrameInfo {
    pub slots: Vec<SlotHome>,
    /// Bytes popped by each function of the module whose returns all pop
    /// the same number (`stack_pops`).
    pub pops: Arc<HashMap<u64, u32>>,
    /// Calls (by block) after which a check confirms the callee popped
    /// this many bytes, leaving the function when it did not (`guard_calls`).
    pub guarded: HashMap<BlockId, u32>,
    /// Whether loop back edges leave slots in their locals, so that code
    /// generation writes them back where a preempted thread yields
    /// (`codegen`'s preemption check) and `osr` where it re-enters
    /// (`flush_at`). Otherwise (irreducible functions) every back edge
    /// writes them back.
    pub live_across_back_edges: bool,
    /// On entry to each block: the register locating the frame (with entry
    /// esp's offset from it), and the slots held in their locals.
    pub base_at: Vec<Option<(V, i32)>>,
    pub valid_at: Vec<Vec<V>>,
}

/// A slot write-back: store `slot` at `base` + `offset`.
pub struct Flush {
    pub base: V,
    pub offset: i32,
    pub slot: V,
    pub size: u8,
}

impl FrameInfo {
    /// What makes the frame current on entry to block `b`: the slots held
    /// in their locals there, written back.
    pub fn flush_at(&self, b: BlockId) -> Vec<Flush> {
        if !self.live_across_back_edges {
            return vec![];
        }
        let (Some(Some((base, k))), Some(valid)) =
            (self.base_at.get(b as usize), self.valid_at.get(b as usize))
        else {
            return vec![];
        };
        valid
            .iter()
            .map(|&v| {
                let h = self.slots.iter().find(|h| h.v == v).unwrap();
                Flush {
                    base: *base,
                    offset: h.offset.wrapping_sub(*k),
                    slot: v,
                    size: h.size,
                }
            })
            .collect()
    }
}

/// Values of single-definition temporaries defined by a constant.
fn consts(f: &Function) -> Vec<Option<u64>> {
    let n = f.vtypes.len();
    let mut defs = vec![0u8; n];
    let mut c = vec![None; n];
    for b in &f.blocks {
        for i in &b.insts {
            if let Some(d) = i.dst {
                defs[d as usize] = defs[d as usize].saturating_add(1);
                if let Op::Const(k) = i.op {
                    c[d as usize] = Some(k);
                }
            }
        }
    }
    for (v, c) in c.iter_mut().enumerate() {
        if v < NS || defs[v] != 1 {
            *c = None;
        }
    }
    c
}

fn op_sym(op: &Op, get: &dyn Fn(V) -> Sym, consts: &[Option<u64>]) -> Sym {
    use Sym::*;
    let k = |v: V| consts.get(v as usize).copied().flatten();
    match op {
        Op::Copy(a) => get(*a),
        Op::Select { t, f, .. } => get(*t).meet(get(*f)),
        Op::Un(_, a) => match get(*a) {
            Top => Top,
            s if s.framey() => Mixed,
            _ => Other,
        },
        Op::Vec(_, args) => {
            let mut r = Other;
            for a in args {
                match get(*a) {
                    Top => return Top,
                    s if s.framey() => r = Mixed,
                    _ => {}
                }
            }
            r
        }
        Op::Bin(op, a, b) => {
            let (sa, sb) = (get(*a), get(*b));
            if sa == Top || sb == Top {
                return Top;
            }
            if !sa.framey() && !sb.framey() {
                return Other;
            }
            use BinOp::*;
            match op {
                I32Add => match (sa, sb, k(*a), k(*b)) {
                    (Frame(x), _, _, Some(c)) => Frame(x.wrapping_add(c as i32)),
                    (_, Frame(y), Some(c), _) => Frame(y.wrapping_add(c as i32)),
                    (Maybe(x), _, _, Some(c)) => Maybe(x.wrapping_add(c as i32)),
                    (_, Maybe(y), Some(c), _) => Maybe(y.wrapping_add(c as i32)),
                    _ => Mixed,
                },
                I32Sub => match (sa, sb, k(*b)) {
                    (Frame(x), _, Some(c)) => Frame(x.wrapping_sub(c as i32)),
                    (Maybe(x), _, Some(c)) => Maybe(x.wrapping_sub(c as i32)),
                    // The distance between two frame addresses is a number.
                    (Frame(_), Frame(_), _) => Other,
                    _ => Mixed,
                },
                I32Eq | I32Ne | I32LtS | I32LtU | I32GtS | I32GtU | I32LeS | I32LeU | I32GeS
                | I32GeU | I64Eq | I64Ne | I64LtS | I64LtU | I64GtS | I64GtU | I64LeS | I64LeU
                | I64GeS | I64GeU => Other,
                _ => Mixed,
            }
        }
        // Loaded values are never frame addresses while the frame's address
        // does not escape (which the pass checks before relying on it).
        _ => Other,
    }
}

/// The state after a call returns. `esp` is known after calls to functions
/// whose returns all pop the same amount (`stack_pops`). The other
/// callee-saved registers are taken to keep what they held, as the C
/// calling convention has them do. The callees that break it set up frames
/// (`_EH_prolog`, `__SEH_prolog4`) or allocate stack (`_chkstk`): they move
/// `esp` without a matching return, so `esp` is unknown after them, and
/// a register a callee points into this frame can only reach it through
/// accesses this pass sees as unknown.
fn after_call(
    cur: &mut State,
    target: &CallTarget,
    pops: &HashMap<u64, u32>,
    guarded: Option<u32>,
) {
    let esp = match (cur[ESP as usize], target, guarded) {
        (Sym::Top, ..) => Sym::Top,
        (Sym::Frame(k), _, Some(n)) => Sym::Frame(k.wrapping_add(4).wrapping_add(n as i32)),
        (Sym::Frame(k), CallTarget::Direct(t), None) => match pops.get(t) {
            Some(&n) => Sym::Frame(k.wrapping_add(4).wrapping_add(n as i32)),
            None => Sym::Mixed,
        },
        _ => Sym::Mixed,
    };
    let saved = [EBX, EBP, ESI, EDI].map(|r| cur[r as usize]);
    for s in cur.iter_mut() {
        if *s != Sym::Top {
            *s = Sym::Other;
        }
    }
    cur[ESP as usize] = esp;
    for (r, s) in [EBX, EBP, ESI, EDI].into_iter().zip(saved) {
        cur[r as usize] = s;
    }
}

/// Symbolic values: per block on entry for state vregs, and per temporary
/// over the whole function.
struct Syms {
    state_in: Vec<State>,
    reached: Vec<bool>,
    temps: Vec<Sym>,
}

impl Syms {
    fn get(&self, cur: &State, v: V) -> Sym {
        if (v as usize) < NS {
            cur[v as usize]
        } else {
            self.temps[v as usize]
        }
    }
}

fn syms(
    f: &Function,
    consts: &[Option<u64>],
    pops: &HashMap<u64, u32>,
    guarded: &HashMap<BlockId, u32>,
) -> Syms {
    let n = f.blocks.len();
    let mut s = Syms {
        state_in: vec![[Sym::Top; NS]; n],
        reached: vec![false; n],
        temps: vec![Sym::Top; f.vtypes.len()],
    };
    if n == 0 {
        return s;
    }
    let mut entry = [Sym::Other; NS];
    entry[ESP as usize] = Sym::Frame(0);
    s.state_in[0] = entry;
    s.reached[0] = true;
    let order = f.rpo();
    loop {
        let mut changed = false;
        for &b in &order {
            if !s.reached[b as usize] {
                continue;
            }
            let mut cur = s.state_in[b as usize];
            for inst in &f.blocks[b as usize].insts {
                let Some(d) = inst.dst else { continue };
                let v = op_sym(&inst.op, &|v| s.get(&cur, v), consts);
                if (d as usize) < NS {
                    cur[d as usize] = v;
                } else {
                    let m = s.temps[d as usize].meet(v);
                    if m != s.temps[d as usize] {
                        s.temps[d as usize] = m;
                        changed = true;
                    }
                }
            }
            let term = &f.blocks[b as usize].term;
            if let Term::Call { target, .. } = term {
                after_call(&mut cur, target, pops, guarded.get(&b).copied());
            }
            for succ in term.successors() {
                let si = succ as usize;
                if !s.reached[si] {
                    s.reached[si] = true;
                    s.state_in[si] = cur;
                    changed = true;
                    continue;
                }
                for (x, &c) in s.state_in[si].iter_mut().zip(cur.iter()) {
                    let m = x.meet(c);
                    if m != *x {
                        *x = m;
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
    s
}

/// The state at the end of block `b`'s instructions (before its terminator).
fn block_end(f: &Function, b: usize, s: &Syms, consts: &[Option<u64>]) -> State {
    let mut cur = s.state_in[b];
    for inst in &f.blocks[b].insts {
        if let Some(d) = inst.dst {
            if (d as usize) < NS {
                cur[d as usize] = op_sym(&inst.op, &|v| s.get(&cur, v), consts);
            }
        }
    }
    cur
}

/// Bytes each function pops on return (beyond its return address), for the
/// functions where every way out is a return popping the same amount (or a
/// fault). Calls to them leave `esp` known in the caller.
pub fn stack_pops(funcs: &[Function]) -> HashMap<u64, u32> {
    let mut pops: HashMap<u64, u32> = HashMap::new();
    // Callers of each function, to revisit when it is resolved.
    let mut callers: HashMap<u64, Vec<usize>> = HashMap::new();
    for (i, f) in funcs.iter().enumerate() {
        for b in &f.blocks {
            if let Term::Call {
                target: CallTarget::Direct(t),
                ..
            } = b.term
            {
                callers.entry(t).or_default().push(i);
            }
        }
    }
    let mut work: Vec<usize> = (0..funcs.len()).collect();
    let mut queued = vec![true; funcs.len()];
    while let Some(i) = work.pop() {
        queued[i] = false;
        let f = &funcs[i];
        if pops.contains_key(&f.entry) {
            continue;
        }
        if let Some(n) = pops_of(f, &pops) {
            pops.insert(f.entry, n);
            for &c in callers.get(&f.entry).map(|v| v.as_slice()).unwrap_or(&[]) {
                if !queued[c] {
                    queued[c] = true;
                    work.push(c);
                }
            }
        }
    }
    pops
}

fn pops_of(f: &Function, pops: &HashMap<u64, u32>) -> Option<u32> {
    if f.mode != Mode::X86 || f.mem64 || f.blocks.is_empty() {
        return None;
    }
    let c = consts(f);
    let s = syms(f, &c, pops, &HashMap::new());
    let mut n: Option<u32> = None;
    for b in 0..f.blocks.len() {
        if !s.reached[b] {
            continue;
        }
        match f.blocks[b].term {
            Term::Jump(_) | Term::Branch { .. } | Term::Call { .. } | Term::Fault { .. } => {}
            Term::Ret(_) => {
                let Sym::Frame(m) = block_end(f, b, &s, &c)[ESP as usize] else {
                    return None;
                };
                let p = u32::try_from(m.checked_sub(4)?).ok()?;
                if n.is_some_and(|n| n != p) {
                    return None;
                }
                n = Some(p);
            }
            _ => return None,
        }
    }
    n
}

// ---- Slots ----------------------------------------------------------------

#[derive(Clone, Debug)]
struct Slot {
    o: i32,
    size: u8,
    ty: Ty,
    v: V,
}

struct Slots {
    /// Sorted by offset, not overlapping.
    list: Vec<Slot>,
    exact: HashMap<(i32, u8), u32>,
}

impl Slots {
    fn exact(&self, o: i32, size: u8) -> Option<u32> {
        self.exact.get(&(o, size)).copied()
    }
    fn overlaps(&self, o: i32, size: u8) -> bool {
        let end = o as i64 + size as i64;
        let i = self.list.partition_point(|s| (s.o as i64) < end);
        i > 0 && {
            let s = &self.list[i - 1];
            s.o as i64 + s.size as i64 > o as i64
        }
    }
}

/// A small bit set over slots.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Bits(Vec<u64>);

impl Bits {
    fn new(n: usize) -> Bits {
        Bits(vec![0; n.div_ceil(64)])
    }
    fn set(&mut self, i: u32) {
        self.0[i as usize / 64] |= 1 << (i % 64);
    }
    fn unset(&mut self, i: u32) {
        self.0[i as usize / 64] &= !(1 << (i % 64));
    }
    fn has(&self, i: u32) -> bool {
        self.0[i as usize / 64] >> (i % 64) & 1 != 0
    }
    fn clear(&mut self) {
        self.0.iter_mut().for_each(|w| *w = 0);
    }
    fn is_empty(&self) -> bool {
        self.0.iter().all(|&w| w == 0)
    }
    fn or(&mut self, o: &Bits) -> bool {
        let mut ch = false;
        for (a, b) in self.0.iter_mut().zip(&o.0) {
            let n = *a | *b;
            ch |= n != *a;
            *a = n;
        }
        ch
    }
    fn and(&mut self, o: &Bits) {
        for (a, b) in self.0.iter_mut().zip(&o.0) {
            *a &= *b;
        }
    }
    fn minus(&mut self, o: &Bits) {
        for (a, b) in self.0.iter_mut().zip(&o.0) {
            *a &= !*b;
        }
    }
    fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        self.0.iter().enumerate().flat_map(|(i, &w)| {
            (0..64)
                .filter(move |b| w >> b & 1 != 0)
                .map(move |b| (i * 64 + b) as u32)
        })
    }
}

/// A step of a block, in order: its instructions with what they do to the
/// slots, and the writes and reloads around them.
#[derive(Clone, Copy, Debug)]
enum Item {
    /// Instruction `i`, unchanged.
    Keep(usize),
    /// Instruction `i` loads slot `s` (from byte `d` of it).
    Load(usize, u32, u8),
    /// Instruction `i` stores slot `s` (at byte `d` of it).
    Store(usize, u32, u8),
    /// Write back the dirty slots, addressed from a register holding entry
    /// esp plus a known offset.
    Flush(Option<(V, i32)>, u64),
    /// Memory may have changed: every slot must be reloaded.
    Clobber,
    /// Slots below entry esp plus this offset are dead.
    Drop(i32),
}

struct Plan {
    items: Vec<Item>,
    /// The register locating the frame at the end of the block.
    end_base: Option<(V, i32)>,
}

fn base_of(cur: &State) -> Option<(V, i32)> {
    if let Sym::Frame(k) = cur[ESP as usize] {
        Some((ESP, k))
    } else if let Sym::Frame(k) = cur[EBP as usize] {
        Some((EBP, k))
    } else {
        None
    }
}

/// How an instruction touches the frame.
enum Acc {
    None,
    /// A load or store of a slot (write or not), at this byte within it.
    Slot(u32, bool, u8),
    /// An access that may reach slots at an unknown place (write or not).
    Unknown(bool),
    /// Memory must be current (a fault may be raised).
    Observe,
}

/// Where the frame's address escapes: stored to memory, or held in a
/// register where code outside the function sees it.
enum Escape {
    /// Nowhere.
    No,
    /// Addresses of the frame from this offset up: the objects they point
    /// to. Slots below the lowest stay the function's own: C code reaches
    /// only the object a pointer points into, and a frame object starts at
    /// the address taken of it (`&local`, `&array[0]`, `&struct`).
    From(i32),
    /// At an unknown place: the whole frame.
    All(String),
}

fn escapes(f: &Function, s: &Syms, consts: &[Option<u64>]) -> Escape {
    let mut low: Option<i32> = None;
    let mut note = |sym: Sym, what: &dyn Fn() -> String| -> Option<String> {
        match sym {
            Sym::Frame(x) | Sym::Maybe(x) => {
                low = Some(low.map_or(x, |l: i32| l.min(x)));
                None
            }
            Sym::Mixed => Some(what()),
            _ => None,
        }
    };
    for b in 0..f.blocks.len() {
        if !s.reached[b] {
            continue;
        }
        let mut cur = s.state_in[b];
        for inst in &f.blocks[b].insts {
            let vals: Vec<V> = match &inst.op {
                Op::Store { val, .. } | Op::AtomicRmw { val, .. } => vec![*val],
                Op::AtomicCmpxchg { expected, new, .. } => vec![*expected, *new],
                Op::MemFill { val, len, .. } => vec![*val, *len],
                Op::MemCopy { len, .. } => vec![*len],
                _ => vec![],
            };
            for v in vals {
                if let Some(why) = note(s.get(&cur, v), &|| {
                    format!("{:#x}: {} (esp {:?})", inst.eip, inst.op, cur[ESP as usize])
                }) {
                    return Escape::All(why);
                }
            }
            if let Some(d) = inst.dst {
                if (d as usize) < NS {
                    cur[d as usize] = op_sym(&inst.op, &|v| s.get(&cur, v), consts);
                }
            }
        }
        let term = &f.blocks[b].term;
        if term.uses().iter().any(|&v| s.get(&cur, v).framey())
            && !matches!(term, Term::Branch { .. })
        {
            // Jumps or calls to the stack, or tables indexed by its address.
            return Escape::All(format!("block {:#x}: terminator", f.blocks[b].addr));
        }
        if term.syncs() && !matches!(term, Term::Switch { .. }) {
            // Code outside the function sees the registers.
            for v in 0..NS as V {
                if matches!(v, ESP | EBP | FK | FR | FA | FB | FC | CPU) {
                    continue;
                }
                let what = || {
                    format!(
                        "block {:#x}: {} at a call or exit",
                        f.blocks[b].addr,
                        VName(v)
                    )
                };
                if let Some(why) = note(cur[v as usize], &what) {
                    return Escape::All(why);
                }
            }
        }
    }
    match low {
        Some(l) => Escape::From(l),
        None => Escape::No,
    }
}

/// The slots of a function, below `floor` (where the frame's address
/// escapes, `Escape::From`).
fn find_slots(f: &mut Function, s: &Syms, consts: &[Option<u64>], floor: i32) -> Slots {
    // Every access at a known frame offset: (offset, size, type), and the
    // ranges atomic accesses reach.
    let mut acc: Vec<(i32, u8, Ty)> = vec![];
    let mut blockers: Vec<(i32, u8)> = vec![];
    for b in 0..f.blocks.len() {
        if !s.reached[b] {
            continue;
        }
        let mut cur = s.state_in[b];
        for inst in &f.blocks[b].insts {
            let addr_sym = |a: V| s.get(&cur, a);
            match &inst.op {
                Op::Load { addr, mem } | Op::Store { addr, mem, .. }
                    if mem.space != Space::Native =>
                {
                    if let Sym::Frame(x) = addr_sym(*addr) {
                        let o = x.wrapping_add(mem.offset as i32);
                        let ty = match &inst.op {
                            Op::Store { val, .. } => f.ty(*val),
                            _ => f.ty(inst.dst.unwrap()),
                        };
                        if mem.atomic {
                            blockers.push((o, mem.size));
                        } else {
                            acc.push((o, mem.size, ty));
                        }
                    }
                }
                Op::AtomicRmw { addr, mem, .. } | Op::AtomicCmpxchg { addr, mem, .. }
                    if mem.space != Space::Native =>
                {
                    if let Sym::Frame(x) = addr_sym(*addr) {
                        blockers.push((x.wrapping_add(mem.offset as i32), mem.size));
                    }
                }
                _ => {}
            }
            if let Some(d) = inst.dst {
                if (d as usize) < NS {
                    cur[d as usize] = op_sym(&inst.op, &|v| s.get(&cur, v), consts);
                }
            }
        }
    }
    // Candidate ranges, with the types they are accessed as.
    let mut cands: HashMap<(i32, u8), Vec<Ty>> = HashMap::new();
    for &(o, size, ty) in &acc {
        cands.entry((o, size)).or_default().push(ty);
    }
    let mut ranges: Vec<(i32, u8)> = cands.keys().copied().collect();
    ranges.sort_unstable();
    // Clusters of overlapping ranges. A cluster is a slot when it is one
    // range, or a dword that narrower accesses reach parts of.
    let mut list = vec![];
    let mut exact = HashMap::new();
    let mut i = 0;
    while i < ranges.len() {
        let lo = ranges[i].0 as i64;
        let mut hi = lo + ranges[i].1 as i64;
        let mut j = i + 1;
        while j < ranges.len() && (ranges[j].0 as i64) < hi {
            hi = hi.max(ranges[j].0 as i64 + ranges[j].1 as i64);
            j += 1;
        }
        let members = &ranges[i..j];
        i = j;
        // The function's own frame only, below the return address; nothing
        // atomic.
        if hi > 0
            || hi > floor as i64
            || blockers
                .iter()
                .any(|&(bo, bs)| (bo as i64) < hi && bo as i64 + bs as i64 > lo)
        {
            continue;
        }
        let tys = |r: &(i32, u8)| cands[r].iter().copied();
        let ty = if members.len() == 1 {
            let r = members[0];
            match r.1 {
                1 | 2 if tys(&r).all(|t| t == Ty::I32) => Ty::I32,
                4 if tys(&r).all(|t| matches!(t, Ty::I32 | Ty::F32)) => {
                    if tys(&r).any(|t| t == Ty::I32) {
                        Ty::I32
                    } else {
                        Ty::F32
                    }
                }
                8 if tys(&r).all(|t| matches!(t, Ty::I64 | Ty::F64)) => {
                    if tys(&r).any(|t| t == Ty::I64) {
                        Ty::I64
                    } else {
                        Ty::F64
                    }
                }
                _ => continue,
            }
        } else if hi - lo == 4
            && members.iter().all(|r| match r.1 {
                1 | 2 => tys(r).all(|t| t == Ty::I32),
                4 => tys(r).all(|t| matches!(t, Ty::I32 | Ty::F32)),
                _ => false,
            })
        {
            Ty::I32
        } else {
            if std::env::var_os("WWT_FRAME_DEBUG").is_some() {
                eprintln!("frame {:#x}: overlapping {members:?}", f.entry);
            }
            continue;
        };
        let size = (hi - lo) as u8;
        let v = f.new_vreg(ty);
        let idx = list.len() as u32;
        for &(o, sz) in members {
            exact.insert((o, sz), idx);
        }
        list.push(Slot {
            o: lo as i32,
            size,
            ty,
            v,
        });
    }
    Slots { list, exact }
}

fn classify(inst: &Inst, cur: &State, s: &Syms, slots: &Slots) -> Acc {
    let sym = |v: V| s.get(cur, v);
    let at = |a: V, mem: &Mem, write: bool| match sym(a) {
        Sym::Frame(x) => {
            let o = x.wrapping_add(mem.offset as i32);
            if !mem.atomic {
                if let Some(i) = slots.exact(o, mem.size) {
                    let d = o.wrapping_sub(slots.list[i as usize].o) as u8;
                    return Acc::Slot(i, write, d);
                }
            }
            if slots.overlaps(o, mem.size) {
                Acc::Unknown(write)
            } else {
                Acc::None
            }
        }
        // A frame address on some paths: the access is an unknown one if it
        // may reach a slot.
        Sym::Maybe(x) if slots.overlaps(x.wrapping_add(mem.offset as i32), mem.size) => {
            Acc::Unknown(write)
        }
        Sym::Mixed => Acc::Unknown(write),
        _ => Acc::None,
    };
    match &inst.op {
        Op::Load { addr, mem } if mem.space != Space::Native => at(*addr, mem, false),
        Op::Store { addr, mem, .. } if mem.space != Space::Native => at(*addr, mem, true),
        Op::AtomicRmw { addr, mem, .. } | Op::AtomicCmpxchg { addr, mem, .. }
            if mem.space != Space::Native =>
        {
            match at(*addr, mem, true) {
                Acc::Slot(..) => Acc::Unknown(true),
                a => a,
            }
        }
        Op::MemCopy { dst, src, .. } => {
            if sym(*dst).framey() {
                Acc::Unknown(true)
            } else if sym(*src).framey() {
                Acc::Unknown(false)
            } else {
                Acc::None
            }
        }
        Op::MemFill { dst, .. } if sym(*dst).framey() => Acc::Unknown(true),
        Op::FaultIf { .. } => Acc::Observe,
        _ => Acc::None,
    }
}

fn plan_block(
    f: &Function,
    b: usize,
    s: &Syms,
    consts: &[Option<u64>],
    slots: &Slots,
    preempt: bool,
) -> Plan {
    let mut items = vec![];
    let mut cur = s.state_in[b];
    for (i, inst) in f.blocks[b].insts.iter().enumerate() {
        let mut next = cur;
        let mut esp_moves = false;
        if let Some(d) = inst.dst {
            if (d as usize) < NS {
                next[d as usize] = op_sym(&inst.op, &|v| s.get(&cur, v), consts);
                esp_moves = d == ESP || d == EBP;
            }
        }
        let base = base_of(&cur);
        let lost = esp_moves && base.is_some() && base_of(&next).is_none();
        if lost {
            items.push(Item::Flush(base, inst.eip));
        }
        match classify(inst, &cur, s, slots) {
            Acc::Slot(sl, w, d) if base.is_some() => items.push(if w {
                Item::Store(i, sl, d)
            } else {
                Item::Load(i, sl, d)
            }),
            Acc::Slot(_, w, _) | Acc::Unknown(w) => {
                items.push(Item::Flush(base, inst.eip));
                items.push(Item::Keep(i));
                if w {
                    items.push(Item::Clobber);
                }
            }
            Acc::Observe => {
                items.push(Item::Flush(base, inst.eip));
                items.push(Item::Keep(i));
            }
            Acc::None => items.push(Item::Keep(i)),
        }
        if lost {
            items.push(Item::Clobber);
        }
        // esp moving up (or found again) leaves the slots below it dead.
        if let Sym::Frame(k1) = next[ESP as usize] {
            let up = match cur[ESP as usize] {
                Sym::Frame(k0) => k1.wrapping_sub(k0) > 0,
                _ => true,
            };
            if up {
                items.push(Item::Drop(k1));
            }
        }
        cur = next;
    }
    let term = &f.blocks[b].term;
    let end_base = base_of(&cur);
    // A jump table's index is in range (`split_switches`).
    let syncs = term.syncs() && !matches!(term, Term::Switch { .. });
    if syncs || preempt {
        let eip = f.blocks[b].insts.last().map_or(f.blocks[b].addr, |i| i.eip);
        items.push(Item::Flush(end_base, eip));
    }
    if matches!(term, Term::Call { .. }) {
        items.push(Item::Clobber);
    }
    Plan { items, end_base }
}

/// Applies a plan's effect on (valid, dirty).
fn forward(p: &Plan, slots: &Slots, valid: &mut Bits, dirty: &mut Bits) {
    for it in &p.items {
        match *it {
            Item::Load(_, s, _) => valid.set(s),
            Item::Store(_, s, _) => {
                valid.set(s);
                dirty.set(s);
            }
            Item::Flush(..) => dirty.clear(),
            Item::Clobber => {
                debug_assert!(dirty.is_empty());
                valid.clear();
                dirty.clear();
            }
            Item::Drop(k) => drop_below(slots, k, valid, dirty),
            Item::Keep(_) => {}
        }
    }
}

fn drop_below(slots: &Slots, k: i32, valid: &mut Bits, dirty: &mut Bits) {
    for (i, sl) in slots.list.iter().enumerate() {
        if sl.o.wrapping_sub(k) < 0 {
            valid.unset(i as u32);
            dirty.unset(i as u32);
        }
    }
}

/// How many bytes the callee of the call ending block `b` likely pops, from
/// how the caller moves `esp` before and after the call:
///
/// * arguments pushed in the call's block, then `add esp` after it: the
///   caller cleans up (cdecl), nothing popped;
/// * no arguments pushed (moved into place, as GCC does), then `sub esp, n`
///   after the call: GCC restoring its outgoing area, which a `stdcall`
///   callee popped (`n` bytes); or `esp` used with no adjustment: nothing
///   popped.
///
/// Anything else is left unknown: after pushed arguments, a `stdcall`
/// return and deferred cdecl cleanup look alike, and with none pushed, an
/// `add esp` may fold a pop into the epilogue's.
fn likely_pops(
    f: &Function,
    b: usize,
    consts: &[Option<u64>],
    live: &crate::opt::Analysis,
) -> Option<u32> {
    let Term::Call { cont, .. } = f.blocks[b].term else {
        return None;
    };
    // Bytes pushed in the call's block, besides the return address: esp's
    // change over the block, followed through constant adjustments.
    let k = |v: V| consts.get(v as usize).copied().flatten();
    let step = |rel: &HashMap<V, i64>, op: &Op| match *op {
        Op::Copy(a) => rel.get(&a).copied(),
        Op::Bin(BinOp::I32Add, a, c) => rel
            .get(&a)
            .zip(k(c))
            .map(|(r, c)| r + c as u32 as i32 as i64),
        Op::Bin(BinOp::I32Sub, a, c) => rel
            .get(&a)
            .zip(k(c))
            .map(|(r, c)| r - c as u32 as i32 as i64),
        _ => None,
    };
    let mut rel: HashMap<V, i64> = HashMap::new();
    rel.insert(ESP, 0);
    for inst in &f.blocks[b].insts {
        if let Some(d) = inst.dst {
            match step(&rel, &inst.op) {
                Some(r) => rel.insert(d, r),
                None => rel.remove(&d),
            };
        }
    }
    let pushed = -rel.get(&ESP).copied()? - 4;
    // The first change to esp in the continuation, relative to its entry.
    let mut rel: HashMap<V, i64> = HashMap::new();
    rel.insert(ESP, 0);
    let mut moved = None;
    let insts = &f.blocks[cont as usize].insts;
    for inst in insts {
        let Some(d) = inst.dst else { continue };
        let r = step(&rel, &inst.op);
        if d == ESP {
            // A push (or a call pushing its return address) stores in the
            // same x86 instruction; `sub esp, n` does not.
            let push = insts
                .iter()
                .find(|i| i.eip == inst.eip && matches!(i.op, Op::Store { .. }));
            if let Some(p) = push {
                // The next call's return address: no adjustment before it.
                let next_ret = match (&f.blocks[cont as usize].term, &p.op) {
                    (Term::Call { ret, .. }, Op::Store { val, .. }) => k(*val) == Some(*ret),
                    _ => false,
                };
                if next_ret {
                    break;
                }
                return None;
            }
            moved = Some(r?);
            break;
        }
        match r {
            Some(r) => rel.insert(d, r),
            None => rel.remove(&d),
        };
    }
    match (pushed, moved) {
        (p, Some(r)) if p > 0 && r > 0 => Some(0),
        (0, Some(r)) if r < 0 => u32::try_from(-r).ok(),
        (0, None) if live.is_live_in(cont, ESP) => Some(0),
        _ => None,
    }
}

/// Jump tables leave the function for an index out of the table's range,
/// which needs the frame current; compiled code checks the range before.
/// That exit becomes its own block, so the table itself is not a write-back
/// point.
fn split_switches(f: &mut Function) {
    for b in 0..f.blocks.len() {
        let Term::Switch {
            index,
            targets,
            fallback,
        } = f.blocks[b].term.clone()
        else {
            continue;
        };
        let addr = f.blocks[b].addr;
        let eip = f.blocks[b].insts.last().map_or(addr, |i| i.eip);
        let (n, inr) = (f.new_vreg(Ty::I32), f.new_vreg(Ty::I32));
        let table = f.new_block(addr);
        let out = f.new_block(addr);
        f.blocks[table as usize].term = Term::Switch {
            index,
            targets: targets.clone(),
            fallback,
        };
        f.blocks[out as usize].term = Term::JmpInd(fallback);
        f.blocks[b].insts.push(Inst {
            dst: Some(n),
            op: Op::Const(targets.len() as u64),
            eip,
        });
        f.blocks[b].insts.push(Inst {
            dst: Some(inr),
            op: Op::Bin(BinOp::I32LtU, index, n),
            eip,
        });
        f.blocks[b].term = Term::Branch {
            cond: inr,
            t: table,
            f: out,
        };
    }
}

/// Checks after calls whose callee's stack effect is not known
/// (`stack_pops`) but likely (`likely_pops`): the call continues only when
/// `esp` is where that puts it, and otherwise leaves the function for the
/// dispatcher at the return address. Returns the guarded calls.
fn guard_calls(f: &mut Function, pops: &HashMap<u64, u32>) -> HashMap<BlockId, u32> {
    let c = consts(f);
    let live = crate::opt::analyze(f);
    let mut guess: HashMap<BlockId, u32> = HashMap::new();
    for b in f.rpo() {
        let b = b as usize;
        let Term::Call { target, .. } = &f.blocks[b].term else {
            continue;
        };
        if matches!(target, CallTarget::Direct(t) if pops.contains_key(t)) {
            continue;
        }
        if let Some(n) = likely_pops(f, b, &c, &live) {
            guess.insert(b as BlockId, n);
        }
    }
    let mut todo: Vec<(usize, u32)> = guess.into_iter().map(|(b, n)| (b as usize, n)).collect();
    todo.sort_unstable();
    let mut guarded = HashMap::new();
    for (b, n) in todo {
        let Term::Call { ret, cont, .. } = f.blocks[b].term else {
            unreachable!()
        };
        let eip = f.blocks[b].insts.last().map_or(f.blocks[b].addr, |i| i.eip);
        let save = f.new_vreg(Ty::I32);
        f.blocks[b].insts.push(Inst {
            dst: Some(save),
            op: Op::Copy(ESP),
            eip,
        });
        let check = f.new_block(ret);
        let leave = f.new_block(ret);
        let (k, want, ne) = (
            f.new_vreg(Ty::I32),
            f.new_vreg(Ty::I32),
            f.new_vreg(Ty::I32),
        );
        f.blocks[check as usize].insts = vec![
            Inst {
                dst: Some(k),
                op: Op::Const(4 + n as u64),
                eip: ret,
            },
            Inst {
                dst: Some(want),
                op: Op::Bin(BinOp::I32Add, save, k),
                eip: ret,
            },
            Inst {
                dst: Some(ne),
                op: Op::Bin(BinOp::I32Ne, ESP, want),
                eip: ret,
            },
        ];
        f.blocks[check as usize].term = Term::Branch {
            cond: ne,
            t: leave,
            f: cont,
        };
        f.blocks[leave as usize].term = Term::Exit(ret);
        if let Term::Call { cont: c, .. } = &mut f.blocks[b].term {
            *c = check;
        }
        guarded.insert(b as BlockId, n);
    }
    guarded
}

/// Keeps stack frame slots of `f` in locals. Returns how many.
pub fn promote(f: &mut Function, pops: &Arc<HashMap<u64, u32>>) -> usize {
    if f.mode != Mode::X86 || f.mem64 || f.blocks.is_empty() {
        return 0;
    }
    let before = (f.blocks.clone(), f.vtypes.len());
    split_switches(f);
    let guarded = guard_calls(f, pops);
    let reducible = crate::reducible::is_reducible(f);
    let c = consts(f);
    let s = syms(f, &c, pops, &guarded);
    let floor = match escapes(f, &s, &c) {
        Escape::All(why) => {
            if std::env::var_os("WWT_FRAME_DEBUG").is_some() {
                eprintln!("frame {:#x}: escapes: {why}", f.entry);
            }
            f.blocks = before.0;
            f.vtypes.truncate(before.1);
            return 0;
        }
        Escape::From(o) => o,
        Escape::No => i32::MAX,
    };
    let ntypes = f.vtypes.len();
    let slots = find_slots(f, &s, &c, floor);
    let ns = slots.list.len();
    if ns == 0 {
        f.blocks = before.0;
        f.vtypes.truncate(before.1);
        let _ = ntypes;
        return 0;
    }
    let n = f.blocks.len();
    let preempt = crate::opt::preempt_points(f);
    // In a reducible function, code generation writes the slots back on
    // the rare paths that leave a loop at its back edge (preemption, `osr`);
    // otherwise every back edge does.
    let plans: Vec<Plan> = (0..n)
        .map(|b| plan_block(f, b, &s, &c, &slots, preempt[b] && !reducible))
        .collect();
    let succs: Vec<Vec<BlockId>> = f.blocks.iter().map(|b| b.term.successors()).collect();
    let mut is_cont = vec![false; n];
    for blk in &f.blocks {
        if let Term::Call { cont, .. } = blk.term {
            is_cont[cont as usize] = true;
        }
    }
    let order = f.rpo();
    // Loop headers: their back edges write every dirty slot back (they are
    // preemption points), so slots enter loops clean, written back once
    // before the loop rather than on every iteration.
    let mut rpo_num = vec![usize::MAX; n];
    for (i, &b) in order.iter().enumerate() {
        rpo_num[b as usize] = i;
    }
    let mut header = vec![false; n];
    for &b in &order {
        for &t in &succs[b as usize] {
            if !reducible && (rpo_num[t as usize] <= rpo_num[b as usize] || preempt[b as usize]) {
                header[t as usize] = true;
            }
        }
    }
    // Liveness: slots read before being written or invalidated.
    let mut live_in = vec![Bits::new(ns); n];
    loop {
        let mut changed = false;
        for &b in order.iter().rev() {
            let b = b as usize;
            let mut live = Bits::new(ns);
            for &t in &succs[b] {
                live.or(&live_in[t as usize]);
            }
            for it in plans[b].items.iter().rev() {
                match *it {
                    Item::Load(_, sl, _) => live.set(sl),
                    // A store to part of a slot keeps the rest.
                    Item::Store(i, sl, _) => {
                        let Op::Store { mem, .. } = f.blocks[b].insts[i].op else {
                            unreachable!()
                        };
                        if mem.size == slots.list[sl as usize].size {
                            live.unset(sl);
                        } else {
                            live.set(sl);
                        }
                    }
                    Item::Clobber => live.clear(),
                    Item::Drop(k) => {
                        let mut d = Bits::new(ns);
                        drop_below(&slots, k, &mut live, &mut d);
                    }
                    Item::Flush(..) | Item::Keep(_) => {}
                }
            }
            if live != live_in[b] {
                live_in[b] = live;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    // Slots held in their locals on entry to each block: those live there,
    // where the frame is located on entry. Not the entry (memory holds the
    // frame there) nor after calls (it may have changed).
    let mut valid_in = vec![Bits::new(ns); n];
    for b in 0..n {
        if b == 0 || is_cont[b] || !s.reached[b] || base_of(&s.state_in[b]).is_none() {
            continue;
        }
        let mut v = live_in[b].clone();
        if let Sym::Frame(k) = s.state_in[b][ESP as usize] {
            let mut d = Bits::new(ns);
            drop_below(&slots, k, &mut v, &mut d);
        }
        valid_in[b] = v;
    }
    // What an edge into each block may leave dirty.
    let keep_in: Vec<Bits> = (0..n)
        .map(|t| {
            if header[t] {
                Bits::new(ns)
            } else {
                valid_in[t].clone()
            }
        })
        .collect();
    // Slots whose memory may be stale on entry to each block.
    let mut dirty_in = vec![Bits::new(ns); n];
    loop {
        let mut changed = false;
        for &b in &order {
            let b = b as usize;
            if matches!(f.blocks[b].term, Term::Call { .. }) {
                continue;
            }
            let mut valid = valid_in[b].clone();
            let mut dirty = dirty_in[b].clone();
            forward(&plans[b], &slots, &mut valid, &mut dirty);
            for &t in &succs[b] {
                dirty.and(&keep_in[t as usize]);
            }
            for &t in &succs[b] {
                changed |= dirty_in[t as usize].or(&dirty);
            }
        }
        if !changed {
            break;
        }
    }
    // Rewrite.
    let mut homes: Vec<SlotHome> = slots
        .list
        .iter()
        .map(|sl| SlotHome {
            v: sl.v,
            offset: sl.o,
            size: sl.size,
        })
        .collect();
    homes.sort_by_key(|h| h.offset);
    for b in 0..n {
        if !s.reached[b] {
            continue;
        }
        let old = std::mem::take(&mut f.blocks[b].insts);
        let mut out: Vec<Inst> = Vec::with_capacity(old.len());
        let mut valid = valid_in[b].clone();
        let mut dirty = dirty_in[b].clone();
        for it in &plans[b].items {
            match *it {
                Item::Keep(i) => out.push(old[i].clone()),
                Item::Load(i, sl, d) => {
                    let inst = &old[i];
                    let Op::Load { addr, mem } = inst.op else {
                        unreachable!()
                    };
                    let slot = &slots.list[sl as usize];
                    if !valid.has(sl) {
                        reload_at(f, &mut out, slot, addr, mem.offset, d, inst.eip);
                        valid.set(sl);
                    }
                    let dst = inst.dst.unwrap();
                    let ty = f.ty(dst);
                    let op = if d == 0 && mem.size == slot.size {
                        load_conv(f, &mut out, slot, ty, mem.signed, inst.eip)
                    } else {
                        let mut src = slot.v;
                        if d > 0 {
                            src = bin_c(f, &mut out, BinOp::I32ShrU, src, 8 * d as u32, inst.eip);
                        }
                        narrow(f, &mut out, src, mem.size, mem.signed, inst.eip)
                    };
                    out.push(Inst {
                        dst: Some(dst),
                        op,
                        eip: inst.eip,
                    });
                }
                Item::Store(i, sl, d) => {
                    let inst = &old[i];
                    let Op::Store { addr, val, mem } = inst.op else {
                        unreachable!()
                    };
                    let slot = &slots.list[sl as usize];
                    let op = if d == 0 && mem.size == slot.size {
                        let ty = f.ty(val);
                        store_conv(slot, ty, val)
                    } else {
                        // Part of a dword: merge into the rest of it.
                        if !valid.has(sl) {
                            reload_at(f, &mut out, slot, addr, mem.offset, d, inst.eip);
                        }
                        let mask = if mem.size == 1 { 0xffu32 } else { 0xffff };
                        let shift = 8 * d as u32;
                        let rest = bin_c(
                            f,
                            &mut out,
                            BinOp::I32And,
                            slot.v,
                            !(mask << shift),
                            inst.eip,
                        );
                        let mut part = bin_c(f, &mut out, BinOp::I32And, val, mask, inst.eip);
                        if shift > 0 {
                            part = bin_c(f, &mut out, BinOp::I32Shl, part, shift, inst.eip);
                        }
                        Op::Bin(BinOp::I32Or, rest, part)
                    };
                    out.push(Inst {
                        dst: Some(slot.v),
                        op,
                        eip: inst.eip,
                    });
                    valid.set(sl);
                    dirty.set(sl);
                }
                Item::Flush(base, eip) => {
                    for sl in dirty.iter().collect::<Vec<_>>() {
                        let base = base.expect("dirty slots with no frame base");
                        write_back(f, &mut out, &slots.list[sl as usize], base, eip);
                    }
                    dirty.clear();
                }
                Item::Clobber => {
                    valid.clear();
                    dirty.clear();
                }
                Item::Drop(k) => drop_below(&slots, k, &mut valid, &mut dirty),
            }
        }
        // Edges: write back what a successor does not keep, and load what
        // one expects in its local.
        if !matches!(f.blocks[b].term, Term::Call { .. }) && !succs[b].is_empty() {
            let eip = out.last().map_or(f.blocks[b].addr, |i| i.eip);
            let mut keep = Bits(vec![!0u64; ns.div_ceil(64)]);
            let mut want = Bits::new(ns);
            for &t in &succs[b] {
                keep.and(&keep_in[t as usize]);
                want.or(&valid_in[t as usize]);
            }
            let mut flush = dirty.clone();
            flush.minus(&keep);
            want.minus(&valid);
            if !flush.is_empty() || !want.is_empty() {
                let base = plans[b].end_base.expect("edge actions with no frame base");
                for sl in flush.iter().collect::<Vec<_>>() {
                    write_back(f, &mut out, &slots.list[sl as usize], base, eip);
                }
                for sl in want.iter().collect::<Vec<_>>() {
                    let slot = &slots.list[sl as usize];
                    let (a, off) = address(f, &mut out, base, slot.o, eip);
                    out.push(Inst {
                        dst: Some(slot.v),
                        op: Op::Load {
                            addr: a,
                            mem: trusted(slot.size, off),
                        },
                        eip,
                    });
                }
            }
        }
        f.blocks[b].insts = out;
    }
    let base_at = (0..n)
        .map(|b| {
            if s.reached[b] {
                base_of(&s.state_in[b])
            } else {
                None
            }
        })
        .collect();
    let valid_at = valid_in
        .iter()
        .map(|v| v.iter().map(|i| slots.list[i as usize].v).collect())
        .collect();
    f.frame = Some(Arc::new(FrameInfo {
        slots: homes,
        pops: pops.clone(),
        guarded,
        live_across_back_edges: reducible,
        base_at,
        valid_at,
    }));
    crate::opt::dce(f);
    ns
}

fn trusted(size: u8, offset: u32) -> Mem {
    Mem {
        size,
        signed: false,
        space: Space::Trusted,
        offset,
        atomic: false,
    }
}

/// The address of entry esp + `o`, from `base` holding entry esp + k: a
/// vreg and a memory offset.
fn address(f: &mut Function, out: &mut Vec<Inst>, base: (V, i32), o: i32, eip: u64) -> (V, u32) {
    let d = o.wrapping_sub(base.1);
    if d >= 0 {
        return (base.0, d as u32);
    }
    let c = f.new_vreg(Ty::I32);
    out.push(Inst {
        dst: Some(c),
        op: Op::Const(d as u32 as u64),
        eip,
    });
    let a = f.new_vreg(Ty::I32);
    out.push(Inst {
        dst: Some(a),
        op: Op::Bin(BinOp::I32Add, base.0, c),
        eip,
    });
    (a, 0)
}

fn write_back(f: &mut Function, out: &mut Vec<Inst>, slot: &Slot, base: (V, i32), eip: u64) {
    let (a, off) = address(f, out, base, slot.o, eip);
    out.push(Inst {
        dst: None,
        op: Op::Store {
            addr: a,
            val: slot.v,
            mem: trusted(slot.size, off),
        },
        eip,
    });
}

/// Loads `slot` from memory, given an access to its byte `d` at `addr` +
/// `offset`.
fn reload_at(
    f: &mut Function,
    out: &mut Vec<Inst>,
    slot: &Slot,
    addr: V,
    offset: u32,
    d: u8,
    eip: u64,
) {
    let (a, off) = match offset.checked_sub(d as u32) {
        Some(off) => (addr, off),
        None => {
            let a = bin_c(
                f,
                out,
                BinOp::I32Add,
                addr,
                offset.wrapping_sub(d as u32),
                eip,
            );
            (a, 0)
        }
    };
    out.push(Inst {
        dst: Some(slot.v),
        op: Op::Load {
            addr: a,
            mem: trusted(slot.size, off),
        },
        eip,
    });
}

/// `a op c` for a constant `c`, in a new temporary.
fn bin_c(f: &mut Function, out: &mut Vec<Inst>, op: BinOp, a: V, c: u32, eip: u64) -> V {
    let k = f.new_vreg(Ty::I32);
    out.push(Inst {
        dst: Some(k),
        op: Op::Const(c as u64),
        eip,
    });
    let r = f.new_vreg(Ty::I32);
    out.push(Inst {
        dst: Some(r),
        op: Op::Bin(op, a, k),
        eip,
    });
    r
}

/// The low `size` bytes of `v`, extended.
fn narrow(f: &mut Function, out: &mut Vec<Inst>, v: V, size: u8, signed: bool, eip: u64) -> Op {
    match (size, signed) {
        (1, true) => Op::Un(UnOp::I32Extend8S, v),
        (2, true) => Op::Un(UnOp::I32Extend16S, v),
        (4, _) => Op::Copy(v),
        _ => {
            let m = f.new_vreg(Ty::I32);
            out.push(Inst {
                dst: Some(m),
                op: Op::Const(if size == 1 { 0xff } else { 0xffff }),
                eip,
            });
            Op::Bin(BinOp::I32And, v, m)
        }
    }
}

/// The value a load of `slot` as `ty` reads.
fn load_conv(
    f: &mut Function,
    out: &mut Vec<Inst>,
    slot: &Slot,
    ty: Ty,
    signed: bool,
    eip: u64,
) -> Op {
    let v = slot.v;
    match (slot.size, slot.ty, ty) {
        (1 | 2, _, _) => narrow(f, out, v, slot.size, signed, eip),
        (_, a, b) if a == b => Op::Copy(v),
        (_, Ty::I32, Ty::F32) => Op::Un(UnOp::F32ReinterpretI32, v),
        (_, Ty::F32, Ty::I32) => Op::Un(UnOp::I32ReinterpretF32, v),
        (_, Ty::I64, Ty::F64) => Op::Un(UnOp::F64ReinterpretI64, v),
        (_, Ty::F64, Ty::I64) => Op::Un(UnOp::I64ReinterpretF64, v),
        _ => unreachable!("slot type"),
    }
}

/// The slot's new value after a store of `val` (of type `ty`).
fn store_conv(slot: &Slot, ty: Ty, val: V) -> Op {
    match (slot.ty, ty) {
        (a, b) if a == b => Op::Copy(val),
        (Ty::I32, Ty::F32) => Op::Un(UnOp::I32ReinterpretF32, val),
        (Ty::F32, Ty::I32) => Op::Un(UnOp::F32ReinterpretI32, val),
        (Ty::I64, Ty::F64) => Op::Un(UnOp::I64ReinterpretF64, val),
        (Ty::F64, Ty::I64) => Op::Un(UnOp::F64ReinterpretI64, val),
        _ => unreachable!("slot type"),
    }
}

/// The offset of each block's entry `esp` from the function's entry `esp`,
/// where known (for `osr`, which reloads slots at re-entry).
pub fn entry_esp(f: &Function) -> Vec<Option<i32>> {
    let (pops, guarded) = match &f.frame {
        Some(i) => (i.pops.clone(), i.guarded.clone()),
        None => Default::default(),
    };
    let c = consts(f);
    let s = syms(f, &c, &pops, &guarded);
    (0..f.blocks.len())
        .map(|b| match s.state_in[b][ESP as usize] {
            Sym::Frame(k) if s.reached[b] => Some(k),
            _ => None,
        })
        .collect()
}
