//! Layers 2 and 3 — Discover code and decode it.
//!
//! Recursive descent from a set of seeds (entry point, exports, TLS
//! callbacks, relocation targets, run-time profile) decodes reachable
//! instructions with `iced-x86`, records basic-block leaders and function
//! entries, and recovers jump tables behind indirect jumps.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use iced_x86::{
    Code, ConditionCode, Decoder, DecoderOptions, FlowControl, Instruction, Mnemonic, OpKind,
    Register,
};

use crate::ir::Mode;
use crate::lift::branch_target;
use crate::pe::Image;

/// Where code bytes come from: a PE image on disk or a snapshot of guest
/// memory at run time.
pub trait CodeSource {
    /// Bytes starting at `va`, as many as are available.
    fn bytes(&self, va: u64) -> &[u8];
    /// Whether `va` is plausibly executable code.
    fn is_code(&self, va: u64) -> bool;
    fn read_u32(&self, va: u64) -> Option<u32> {
        let b = self.bytes(va);
        (b.len() >= 4).then(|| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn read_u64(&self, va: u64) -> Option<u64> {
        let b = self.bytes(va);
        (b.len() >= 8).then(|| u64::from_le_bytes(b[..8].try_into().unwrap()))
    }
    /// Whether a value stored at `va` can be assumed to stay constant, so a
    /// jump table there can be trusted (read-only data or code).
    fn is_readonly(&self, va: u64) -> bool;
}

impl CodeSource for Image {
    fn bytes(&self, va: u64) -> &[u8] {
        self.bytes_from(va)
    }
    fn is_code(&self, va: u64) -> bool {
        Image::is_code(self, va)
    }
    fn is_readonly(&self, va: u64) -> bool {
        self.section_of(va).is_some_and(|s| !s.is_writable())
    }
}

/// A flat region of memory with every byte treated as code, used for
/// snippets in tests and for run-time (fast mode) translation.
pub struct FlatCode {
    pub base: u64,
    pub bytes: Vec<u8>,
}

impl CodeSource for FlatCode {
    fn bytes(&self, va: u64) -> &[u8] {
        if va < self.base || (va - self.base) as usize >= self.bytes.len() {
            return &[];
        }
        &self.bytes[(va - self.base) as usize..]
    }
    fn is_code(&self, va: u64) -> bool {
        va >= self.base && ((va - self.base) as usize) < self.bytes.len()
    }
    fn is_readonly(&self, _va: u64) -> bool {
        true
    }
}

#[derive(Debug, Clone)]
pub struct JumpTable {
    /// Address of the first table entry.
    pub base: u64,
    /// Targets in table order.
    pub targets: Vec<u64>,
    /// Bytes per entry: 4, or 8 for x86-64 tables of absolute addresses.
    /// The jump's table load reads entry `(load address - base) / size`.
    pub entry_size: u32,
}

#[derive(Debug, Default)]
pub struct Discovery {
    /// 32-bit or 64-bit code (selects the decoder).
    pub mode: Mode,
    pub insts: HashMap<u64, Instruction>,
    /// Addresses where a basic block must start.
    pub leaders: BTreeSet<u64>,
    /// Function entry points.
    pub functions: BTreeSet<u64>,
    /// Jump tables keyed by the address of the indirect `jmp`.
    pub jump_tables: HashMap<u64, JumpTable>,
    /// Addresses where decoding failed.
    pub invalid: BTreeSet<u64>,
    /// Where each seed came from, for reporting.
    pub seed_kinds: BTreeMap<u64, SeedKind>,
    /// Function entries translated elsewhere (fast mode): treated as
    /// functions but not decoded or translated again.
    pub external: BTreeSet<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SeedKind {
    Entry,
    Export,
    TlsCallback,
    Relocation,
    DataScan,
    Profile,
    Call,
    /// x86-64 `.pdata` function table.
    Pdata,
}

pub fn decode_one(src: &dyn CodeSource, va: u64) -> Option<Instruction> {
    decode_one_in(src, va, Mode::X86)
}

pub fn decode_one_in(src: &dyn CodeSource, va: u64, mode: Mode) -> Option<Instruction> {
    let bytes = src.bytes(va);
    if bytes.is_empty() {
        return None;
    }
    let n = bytes.len().min(15);
    let mut d = Decoder::with_ip(mode.bitness(), &bytes[..n], va, DecoderOptions::NONE);
    let i = d.decode();
    (i.code() != Code::INVALID).then_some(i)
}

/// True for instructions after which execution does not fall through.
pub fn ends_block(i: &Instruction) -> bool {
    !matches!(i.flow_control(), FlowControl::Next)
}

/// Instructions that never return or fall through.
fn no_fallthrough(i: &Instruction) -> bool {
    match i.flow_control() {
        FlowControl::UnconditionalBranch | FlowControl::IndirectBranch | FlowControl::Return => {
            true
        }
        FlowControl::Exception => true,
        FlowControl::Interrupt => matches!(i.mnemonic(), Mnemonic::Int3 | Mnemonic::Hlt),
        _ => matches!(i.mnemonic(), Mnemonic::Ud2 | Mnemonic::Hlt),
    }
}

impl Discovery {
    pub fn new() -> Discovery {
        Discovery::default()
    }

    pub fn new_in(mode: Mode) -> Discovery {
        Discovery {
            mode,
            ..Discovery::default()
        }
    }

    pub fn add_function_seed(&mut self, va: u64, kind: SeedKind) {
        self.functions.insert(va);
        self.leaders.insert(va);
        self.seed_kinds.entry(va).or_insert(kind);
    }

    /// Explores everything reachable from the current functions and leaders.
    /// `region` restricts decoding to `[start, end)` when given.
    pub fn explore(&mut self, src: &dyn CodeSource) {
        let mut work: VecDeque<u64> = self.leaders.iter().copied().collect();
        let mut done: BTreeSet<u64> = BTreeSet::new();
        while let Some(start) = work.pop_front() {
            if !done.insert(start) || self.external.contains(&start) {
                continue;
            }
            let mut va = start;
            loop {
                if !src.is_code(va) {
                    self.invalid.insert(va);
                    break;
                }
                let inst = match self.insts.get(&va) {
                    Some(i) => *i,
                    None => match decode_one_in(src, va, self.mode) {
                        Some(i) => {
                            self.insts.insert(va, i);
                            i
                        }
                        None => {
                            self.invalid.insert(va);
                            break;
                        }
                    },
                };
                let next = inst.next_ip();
                let mut push = |d: &mut Discovery, t: u64, func: bool| {
                    if func {
                        d.functions.insert(t);
                        d.seed_kinds.entry(t).or_insert(SeedKind::Call);
                    }
                    d.leaders.insert(t);
                    work.push_back(t);
                };
                match inst.flow_control() {
                    FlowControl::Next => {
                        va = next;
                        if self.leaders.contains(&va) {
                            work.push_back(va);
                            break;
                        }
                        continue;
                    }
                    FlowControl::ConditionalBranch => {
                        if let Some(t) = branch_target(&inst, 0) {
                            push(self, t, false);
                        }
                        push(self, next, false);
                    }
                    FlowControl::UnconditionalBranch => {
                        if let Some(t) = branch_target(&inst, 0) {
                            push(self, t, false);
                        }
                    }
                    FlowControl::Call => {
                        if let Some(t) = branch_target(&inst, 0) {
                            // `call $+5` is a get-PC idiom, not a call.
                            push(self, t, t != next);
                        }
                        push(self, next, false);
                    }
                    FlowControl::IndirectCall => push(self, next, false),
                    FlowControl::IndirectBranch => {
                        let jt = match self.mode {
                            Mode::X86 => find_jump_table(self, src, va, &inst),
                            Mode::X64 => find_jump_table64(self, src, va, &inst),
                        };
                        if let Some(jt) = jt {
                            for &t in &jt.targets {
                                push(self, t, false);
                            }
                            self.jump_tables.insert(va, jt);
                        }
                    }
                    FlowControl::Return => {}
                    _ => {
                        if !no_fallthrough(&inst) {
                            push(self, next, false);
                        }
                    }
                }
                break;
            }
        }
    }

    /// Instructions of the basic block starting at `start`, ending at a
    /// control-flow instruction, a leader or undecodable bytes. Returns the
    /// instructions and, when the block ran into a leader or ended in an
    /// instruction that falls through, the fall-through address.
    pub fn block(&self, start: u64) -> (Vec<Instruction>, Option<u64>) {
        let mut out = vec![];
        let mut va = start;
        loop {
            let Some(inst) = self.insts.get(&va) else {
                // Falling through into bytes that were not decoded (outside
                // the region): leave the function there; decoding is retried
                // when execution actually reaches them.
                return (out, if va == start { None } else { Some(va) });
            };
            out.push(*inst);
            let next = inst.next_ip();
            if ends_block(inst) {
                return (out, (!no_fallthrough(inst)).then_some(next));
            }
            if self.leaders.contains(&next) {
                return (out, Some(next));
            }
            va = next;
        }
    }

    /// Finds the instruction that ends just before `va` in linear order.
    fn prev_inst(&self, va: u64) -> Option<Instruction> {
        (1..=15u64).find_map(|back| {
            let a = va.checked_sub(back)?;
            self.insts.get(&a).filter(|i| i.next_ip() == va).copied()
        })
    }
}

// ---- Jump tables ---------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Sym {
    Const(u32),
    Reg(Register),
    Add(Box<Sym>, Box<Sym>),
    Mul(Box<Sym>, u32),
    Load(Box<Sym>),
    Unknown,
}

impl Sym {
    fn add(a: Sym, b: Sym) -> Sym {
        match (a, b) {
            (Sym::Const(x), Sym::Const(y)) => Sym::Const(x.wrapping_add(y)),
            (Sym::Unknown, _) | (_, Sym::Unknown) => Sym::Unknown,
            (a, b) => Sym::Add(Box::new(a), Box::new(b)),
        }
    }
    /// Splits `const + rest`.
    fn split_const(&self) -> (u32, Vec<Sym>) {
        match self {
            Sym::Const(c) => (*c, vec![]),
            Sym::Add(a, b) => {
                let (ca, mut ra) = a.split_const();
                let (cb, rb) = b.split_const();
                ra.extend(rb);
                (ca.wrapping_add(cb), ra)
            }
            other => (0, vec![other.clone()]),
        }
    }
}

/// Symbolic value of `reg` just before instruction `idx` of `insts`.
fn sym_reg(insts: &[Instruction], idx: usize, reg: Register, depth: u32) -> Sym {
    if depth > 8 {
        return Sym::Unknown;
    }
    let reg = reg.full_register32();
    for j in (0..idx).rev() {
        let i = &insts[j];
        if i.op_count() == 0 || i.op0_kind() != OpKind::Register {
            continue;
        }
        if i.op0_register().full_register32() != reg {
            // Instructions that write registers implicitly would break the
            // slice; they rarely appear between a table load and its jump.
            continue;
        }
        if i.op0_register().size() != 4 {
            return Sym::Unknown;
        }
        let src = |n: u32| sym_operand(insts, j, i, n, depth + 1);
        return match i.mnemonic() {
            Mnemonic::Mov => src(1),
            Mnemonic::Lea => sym_mem_addr(insts, j, i, depth + 1),
            Mnemonic::Add => Sym::add(src(0), src(1)),
            Mnemonic::Shl | Mnemonic::Sal => match i.op1_kind() {
                OpKind::Immediate8 => {
                    let s = i.immediate8() as u32 & 31;
                    Sym::Mul(Box::new(src(0)), 1 << s)
                }
                _ => Sym::Unknown,
            },
            Mnemonic::Imul if i.op_count() == 3 => match src(1) {
                Sym::Unknown => Sym::Unknown,
                s => Sym::Mul(Box::new(s), i.immediate32()),
            },
            Mnemonic::Movzx => Sym::Unknown,
            _ => Sym::Unknown,
        };
    }
    Sym::Reg(reg)
}

fn sym_mem_addr(insts: &[Instruction], idx: usize, i: &Instruction, depth: u32) -> Sym {
    if i.memory_segment() == Register::FS || i.memory_segment() == Register::GS {
        return Sym::Unknown;
    }
    let mut s = Sym::Const(i.memory_displacement32());
    if i.memory_base() != Register::None {
        s = Sym::add(s, sym_reg(insts, idx, i.memory_base(), depth));
    }
    if i.memory_index() != Register::None {
        let ix = sym_reg(insts, idx, i.memory_index(), depth);
        let sc = i.memory_index_scale();
        s = Sym::add(
            s,
            if sc == 1 {
                ix
            } else {
                Sym::Mul(Box::new(ix), sc)
            },
        );
    }
    s
}

fn sym_operand(insts: &[Instruction], idx: usize, i: &Instruction, n: u32, depth: u32) -> Sym {
    match i.op_kind(n) {
        OpKind::Register => sym_reg(insts, idx, i.op_register(n), depth),
        OpKind::Immediate32 | OpKind::Immediate8to32 => Sym::Const(i.immediate(n) as u32),
        OpKind::Memory => {
            if i.memory_size().size() != 4 {
                return Sym::Unknown;
            }
            Sym::Load(Box::new(sym_mem_addr(insts, idx, i, depth)))
        }
        _ => Sym::Unknown,
    }
}

/// Recognizes `jmp [table + index*4]` (directly or through a register
/// loaded in the same block) and reads the table's entries.
fn find_jump_table(
    d: &Discovery,
    src: &dyn CodeSource,
    jmp_va: u64,
    jmp: &Instruction,
) -> Option<JumpTable> {
    // Collect the block's instructions leading up to the jump, walking back
    // through fall-through predecessors.
    let mut insts = vec![*jmp];
    let mut va = jmp_va;
    while insts.len() < 12 {
        let Some(p) = d.prev_inst(va) else { break };
        if matches!(
            p.flow_control(),
            FlowControl::UnconditionalBranch | FlowControl::Return | FlowControl::IndirectBranch
        ) {
            break;
        }
        insts.push(p);
        va = p.ip();
    }
    insts.reverse();
    let jidx = insts.len() - 1;
    let target = sym_operand(&insts, jidx, jmp, 0, 0);
    let Sym::Load(addr) = target else {
        return None;
    };
    let (base, rest) = addr.split_const();
    // Expect exactly one scaled index term with scale 4.
    if rest.len() != 1 || !matches!(&rest[0], Sym::Mul(_, 4)) {
        return None;
    }
    let base = base as u64;
    if !src.is_readonly(base) && !src.is_code(base) {
        return None;
    }
    let bound = find_bound(&insts);
    let limit = bound.unwrap_or(1024).min(4096) as u64;
    let mut targets = vec![];
    let jmp_section_code = src.is_code(jmp_va);
    for k in 0..limit {
        let Some(t) = src.read_u32(base + k * 4) else {
            break;
        };
        let t = t as u64;
        if !src.is_code(t) || !jmp_section_code {
            break;
        }
        // Without a known bound, stop where another table or code starts.
        if bound.is_none() && k > 0 && (base + k * 4 == t || d.functions.contains(&(base + k * 4)))
        {
            break;
        }
        targets.push(t);
    }
    if targets.is_empty() {
        return None;
    }
    Some(JumpTable {
        base,
        targets,
        entry_size: 4,
    })
}

/// The instructions of the block before `jmp_va` (walking back through
/// fall-through predecessors), ending with the jump.
fn block_before(d: &Discovery, jmp_va: u64, jmp: &Instruction) -> Vec<Instruction> {
    let mut insts = vec![*jmp];
    let mut va = jmp_va;
    while insts.len() < 12 {
        let Some(p) = d.prev_inst(va) else { break };
        if matches!(
            p.flow_control(),
            FlowControl::UnconditionalBranch | FlowControl::Return | FlowControl::IndirectBranch
        ) {
            break;
        }
        insts.push(p);
        va = p.ip();
    }
    insts.reverse();
    insts
}

/// The last instruction before `idx` whose first operand is register `r`
/// (any size), and its index.
fn last_write(insts: &[Instruction], idx: usize, r: Register) -> Option<(usize, Instruction)> {
    let full = r.full_register();
    (0..idx).rev().find_map(|j| {
        let i = &insts[j];
        (i.op_count() > 0
            && i.op0_kind() == OpKind::Register
            && i.op0_register().full_register() == full)
            .then_some((j, *i))
    })
}

/// A constant address loaded into `r` before `idx` (`lea r, [rip+X]` or
/// `mov r, imm`).
fn const_reg(insts: &[Instruction], idx: usize, r: Register) -> Option<u64> {
    let (_, i) = last_write(insts, idx, r)?;
    match i.mnemonic() {
        Mnemonic::Lea if i.memory_index() == Register::None => match i.memory_base() {
            Register::RIP | Register::None => Some(i.memory_displacement64()),
            _ => None,
        },
        Mnemonic::Mov if matches!(i.op1_kind(), OpKind::Immediate64 | OpKind::Immediate32to64) => {
            Some(i.immediate(1))
        }
        _ => None,
    }
}

/// x86-64 jump tables:
/// * `jmp [table + index*8]`: absolute 8-byte entries;
/// * `lea b, [rip+X]; mov e, [b + index*4 + D]; add r, b; jmp r`: 4-byte
///   entries relative to X, the image base for MSVC (D is the table's RVA)
///   and the table itself for GCC and Clang (`movsxd`, D = 0).
fn find_jump_table64(
    d: &Discovery,
    src: &dyn CodeSource,
    jmp_va: u64,
    jmp: &Instruction,
) -> Option<JumpTable> {
    let insts = block_before(d, jmp_va, jmp);
    let jidx = insts.len() - 1;
    let bound = find_bound(&insts);
    let limit = bound.unwrap_or(1024).min(4096) as u64;
    let jmp_section_code = src.is_code(jmp_va);
    if !jmp_section_code {
        return None;
    }
    let mut targets = vec![];
    if jmp.op0_kind() == OpKind::Memory {
        if jmp.memory_base() != Register::None
            || jmp.memory_index() == Register::None
            || jmp.memory_index_scale() != 8
        {
            return None;
        }
        let base = jmp.memory_displacement64();
        if !src.is_readonly(base) && !src.is_code(base) {
            return None;
        }
        for k in 0..limit {
            let Some(t) = src.read_u64(base + k * 8) else {
                break;
            };
            if !src.is_code(t) {
                break;
            }
            if bound.is_none() && k > 0 && d.functions.contains(&(base + k * 8)) {
                break;
            }
            targets.push(t);
        }
        return (!targets.is_empty()).then_some(JumpTable {
            base,
            targets,
            entry_size: 8,
        });
    }
    if jmp.op0_kind() != OpKind::Register {
        return None;
    }
    // add r, b (either order)
    let (aidx, add) = last_write(&insts, jidx, jmp.op0_register())?;
    if add.mnemonic() != Mnemonic::Add || add.op1_kind() != OpKind::Register {
        return None;
    }
    let (r1, r2) = (add.op0_register(), add.op1_register());
    // The load of one operand from [b + index*4 + D].
    for (loaded, other) in [(r1, r2), (r2, r1)] {
        let Some((lidx, load)) = last_write(&insts, aidx, loaded) else {
            continue;
        };
        if !matches!(load.mnemonic(), Mnemonic::Mov | Mnemonic::Movsxd)
            || load.op1_kind() != OpKind::Memory
            || load.memory_size().size() != 4
            || load.memory_index() == Register::None
            || load.memory_index_scale() != 4
        {
            continue;
        }
        let b = load.memory_base();
        if b == Register::None || b.full_register() != other.full_register() {
            continue;
        }
        let Some(x) = const_reg(&insts, lidx, b) else {
            continue;
        };
        let base = x.wrapping_add(load.memory_displacement64());
        if !src.is_readonly(base) && !src.is_code(base) {
            return None;
        }
        for k in 0..limit {
            let Some(e) = src.read_u32(base + k * 4) else {
                break;
            };
            // GCC's entries are signed offsets from the table.
            let t = x.wrapping_add(e as i32 as i64 as u64);
            if !src.is_code(t) {
                break;
            }
            if bound.is_none() && k > 0 && d.functions.contains(&(base + k * 4)) {
                break;
            }
            targets.push(t);
        }
        return (!targets.is_empty()).then_some(JumpTable {
            base,
            targets,
            entry_size: 4,
        });
    }
    None
}

/// Looks for `cmp x, imm` followed by `ja`/`jae` guarding the jump.
fn find_bound(insts: &[Instruction]) -> Option<u32> {
    for w in insts.windows(2).rev() {
        let (c, j) = (&w[0], &w[1]);
        if c.mnemonic() == Mnemonic::Cmp
            && matches!(
                c.op1_kind(),
                OpKind::Immediate32 | OpKind::Immediate8to32 | OpKind::Immediate8
            )
        {
            let imm = c.immediate(1) as u32;
            return match j.condition_code() {
                ConditionCode::a => imm.checked_add(1),
                ConditionCode::ae => Some(imm),
                _ => None,
            };
        }
    }
    None
}

/// Scans a data range for 32-bit values that point at valid instruction
/// starts in code, returning plausible code pointers (vtables, callback
/// tables). Each candidate is confirmed by decoding a few instructions.
pub fn scan_data_for_code_pointers(src: &dyn CodeSource, start: u64, end: u64) -> Vec<u64> {
    scan_data_for_code_pointers_in(src, start, end, Mode::X86)
}

/// [`scan_data_for_code_pointers`] for `mode`: x86-64 pointers are 8-byte
/// values at 8-byte alignment.
pub fn scan_data_for_code_pointers_in(
    src: &dyn CodeSource,
    start: u64,
    end: u64,
    mode: Mode,
) -> Vec<u64> {
    let mut out = vec![];
    if mode == Mode::X64 {
        let mut va = (start + 7) & !7;
        while va + 8 <= end {
            if let Some(v) = src.read_u64(va) {
                if src.is_code(v) && plausible_function_start(src, v, mode) {
                    out.push(v);
                }
            }
            va += 8;
        }
        out.sort_unstable();
        out.dedup();
        return out;
    }
    let mut va = start;
    while va + 4 <= end {
        if let Some(v) = src.read_u32(va) {
            let v = v as u64;
            if src.is_code(v) && plausible_function_start(src, v, mode) {
                out.push(v);
            }
        }
        va += 4;
    }
    out.sort_unstable();
    out.dedup();
    out
}

fn plausible_function_start(src: &dyn CodeSource, va: u64, mode: Mode) -> bool {
    let mut a = va;
    for _ in 0..4 {
        let Some(i) = decode_one_in(src, a, mode) else {
            return false;
        };
        if matches!(
            i.mnemonic(),
            Mnemonic::Int3 | Mnemonic::Hlt | Mnemonic::In | Mnemonic::Out
        ) || i.is_privileged()
        {
            return false;
        }
        // Real code rarely starts with these patterns that zero bytes decode to.
        if i.code() == Code::Add_rm8_r8 && i.op0_kind() == OpKind::Memory {
            return false;
        }
        if ends_block(&i) {
            return true;
        }
        a = i.next_ip();
    }
    true
}

pub fn is_conditional(i: &Instruction) -> bool {
    i.flow_control() == FlowControl::ConditionalBranch
}
