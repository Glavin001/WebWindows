//! Re-entry at loop headers, so long-running loops reach optimized code.
//!
//! Engines compile a WebAssembly function first with a baseline compiler
//! and switch to optimized code only for later calls: V8 has no on-stack
//! replacement for WebAssembly. A translated function that is entered once
//! and then loops for a long time (an interpreter's dispatch loop, a
//! program's main loop) keeps running baseline code, which for translated
//! code is several times slower than optimized code.
//!
//! Every loop header where only x86 state is live (state can be written
//! back to the CPU struct and reloaded; temporaries cannot) gets a counter
//! on its back edges. When it runs out, the function writes its state back,
//! records the header in `cpu.RESUME` and tail-calls itself; the new
//! activation, optimized once the engine has compiled it, jumps straight
//! back to the header. A new first block reads `cpu.RESUME` on every entry.

use crate::abi::cpu;
use crate::ir::*;
use crate::opt;

/// Back edges between re-entries.
pub const PERIOD: u32 = 1 << 16;

fn native(offset: u32) -> Mem {
    Mem::native(4, offset)
}

/// Adds re-entry points to `f`'s loops (which must be reducible). Returns
/// whether anything changed.
pub fn add_reentry(f: &mut Function) -> bool {
    let before = f.clone();
    let changed = add_reentry_points(f);
    // Never trade structured code for re-entry.
    if changed && !crate::reducible::is_reducible(f) {
        *f = before;
        return false;
    }
    changed
}

fn add_reentry_points(f: &mut Function) -> bool {
    let order = f.rpo();
    let n = f.blocks.len();
    let mut rpo_num = vec![u32::MAX; n];
    for (i, &b) in order.iter().enumerate() {
        rpo_num[b as usize] = i as u32;
    }
    let preds = f.predecessors();
    // Headers: targets of retreating edges.
    let mut headers: Vec<BlockId> = vec![];
    for &b in &order {
        let back = preds[b as usize].iter().any(|&p| {
            rpo_num[p as usize] != u32::MAX && rpo_num[p as usize] >= rpo_num[b as usize]
        });
        if back {
            headers.push(b);
        }
    }
    if headers.is_empty() {
        return false;
    }
    // Only outermost loops: making an inner loop's header reachable from
    // the entry would give its enclosing loop a second entry (irreducible).
    // Natural loop bodies, from each header's retreating edges.
    let mut inner = vec![false; n];
    for &h in &headers {
        let mut body = vec![false; n];
        body[h as usize] = true;
        let mut work: Vec<BlockId> = preds[h as usize]
            .iter()
            .copied()
            .filter(|&p| {
                rpo_num[p as usize] != u32::MAX && rpo_num[p as usize] >= rpo_num[h as usize]
            })
            .collect();
        while let Some(b) = work.pop() {
            if body[b as usize] || rpo_num[b as usize] == u32::MAX {
                continue;
            }
            body[b as usize] = true;
            work.extend(preds[b as usize].iter().copied());
        }
        for &h2 in &headers {
            if h2 != h && body[h2 as usize] {
                inner[h2 as usize] = true;
            }
        }
    }
    headers.retain(|&h| !inner[h as usize]);
    let a = opt::analyze(f);
    headers.retain(|&h| {
        h != 0
            && a.global
                .iter()
                .enumerate()
                .all(|(d, &v)| v < NUM_STATE || !a.live_in[h as usize].contains(d))
    });
    if headers.is_empty() {
        return false;
    }
    // Block 0 must stay the entry: move its code to a new block.
    let old_entry = f.blocks.len() as BlockId;
    let b0 = f.blocks[0].clone();
    let entry_addr = b0.addr;
    f.blocks.push(b0);
    for b in f.blocks.iter_mut() {
        for s in b.term.successors_mut() {
            if *s == 0 {
                *s = old_entry;
            }
        }
    }
    let counter = f.new_vreg(Ty::I32);
    let resume = f.new_vreg(Ty::I32);
    let period = f.new_vreg(Ty::I32);
    f.blocks[0] = Block {
        addr: entry_addr,
        insts: vec![
            Inst {
                dst: Some(period),
                op: Op::Const(PERIOD as u64),
                eip: entry_addr,
            },
            Inst {
                dst: Some(counter),
                op: Op::Copy(period),
                eip: entry_addr,
            },
            Inst {
                dst: Some(resume),
                op: Op::Load {
                    addr: CPU,
                    mem: native(cpu::RESUME),
                },
                eip: entry_addr,
            },
        ],
        term: Term::None,
    };
    // Entry: one test of cpu.RESUME on every call; when it is set (k + 1),
    // clear it and go to header k.
    let chain = f.new_block(entry_addr);
    f.blocks[0].term = Term::Branch {
        cond: resume,
        t: chain,
        f: old_entry,
    };
    let mut cur = chain;
    for (k, &h) in headers.iter().enumerate() {
        let addr = f.blocks[h as usize].addr;
        let go = f.new_block(addr);
        let zero = f.new_vreg(Ty::I32);
        f.blocks[go as usize].insts = vec![
            Inst {
                dst: Some(zero),
                op: Op::Const(0),
                eip: addr,
            },
            Inst {
                dst: None,
                op: Op::Store {
                    addr: CPU,
                    val: zero,
                    mem: native(cpu::RESUME),
                },
                eip: addr,
            },
        ];
        f.blocks[go as usize].term = Term::Jump(h);
        let next = f.new_block(entry_addr);
        let kc = f.new_vreg(Ty::I32);
        let c = f.new_vreg(Ty::I32);
        let blk = &mut f.blocks[cur as usize];
        blk.insts.push(Inst {
            dst: Some(kc),
            op: Op::Const(k as u64 + 1),
            eip: entry_addr,
        });
        blk.insts.push(Inst {
            dst: Some(c),
            op: Op::Bin(BinOp::I32Eq, resume, kc),
            eip: entry_addr,
        });
        blk.term = Term::Branch {
            cond: c,
            t: go,
            f: next,
        };
        cur = next;
    }
    f.blocks[cur as usize].term = Term::Jump(old_entry);
    // Back edges: count, and re-enter when the count runs out.
    let preds = f.predecessors();
    for (k, &h) in headers.iter().enumerate() {
        let addr = f.blocks[h as usize].addr;
        let latch = f.new_block(addr);
        let exit = f.new_block(addr);
        let (one, dec, z, kc) = (
            f.new_vreg(Ty::I32),
            f.new_vreg(Ty::I32),
            f.new_vreg(Ty::I32),
            f.new_vreg(Ty::I32),
        );
        f.blocks[latch as usize].insts = vec![
            Inst {
                dst: Some(one),
                op: Op::Const(1),
                eip: addr,
            },
            Inst {
                dst: Some(dec),
                op: Op::Bin(BinOp::I32Sub, counter, one),
                eip: addr,
            },
            Inst {
                dst: Some(counter),
                op: Op::Copy(dec),
                eip: addr,
            },
            Inst {
                dst: Some(z),
                op: Op::Un(UnOp::I32Eqz, dec),
                eip: addr,
            },
        ];
        f.blocks[latch as usize].term = Term::Branch {
            cond: z,
            t: exit,
            f: h,
        };
        f.blocks[exit as usize].insts = vec![
            Inst {
                dst: Some(kc),
                op: Op::Const(k as u64 + 1),
                eip: addr,
            },
            Inst {
                dst: None,
                op: Op::Store {
                    addr: CPU,
                    val: kc,
                    mem: native(cpu::RESUME),
                },
                eip: addr,
            },
        ];
        f.blocks[exit as usize].term = Term::Exit(f.entry);
        // Retreating edges into h (by the order before this pass).
        for &p in &preds[h as usize] {
            let (pr, hr) = (
                rpo_num.get(p as usize).copied().unwrap_or(u32::MAX),
                rpo_num[h as usize],
            );
            if pr != u32::MAX && pr >= hr {
                for s in f.blocks[p as usize].term.successors_mut() {
                    if *s == h {
                        *s = latch;
                    }
                }
            }
        }
    }
    true
}
