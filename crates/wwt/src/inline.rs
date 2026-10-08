//! Inlining of small leaf functions into their callers.
//!
//! A translated call costs far more than an x86 one: the caller writes its
//! dirty registers back to the CPU struct, the callee loads what it reads
//! and writes back what it changed, and the caller reloads everything live
//! after the call. For small callees that is most of their time. Inlining
//! splices the callee's blocks into the caller instead, where the x86 state
//! simply stays in the caller's locals.
//!
//! x86 semantics are kept: the `call` still pushes its return address
//! (the block ending in `Term::Call` did that), and the callee's `ret` pops
//! it and continues inline only when it is the expected address, leaving
//! the function for that address otherwise, which is what the caller's
//! check after a real call does. Faults inside the inlined code report the
//! callee's instruction addresses and the full register state.
//!
//! Only leaf callees are inlined: no calls, tail jumps or indirect jumps,
//! whose continuations would otherwise go through the dispatcher.

use std::collections::HashMap;

use crate::ir::*;

/// Callees up to this many IR instructions are inlined at every call site.
const SMALL: usize = 150;
/// Larger callees are inlined only when they have at most `FEW_SITES` direct
/// call sites in the module, up to this size.
const MEDIUM: usize = 1500;
const FEW_SITES: usize = 2;
/// A caller stops growing past this many instructions.
const MAX_CALLER: usize = 6000;

fn size(f: &Function) -> usize {
    f.blocks.iter().map(|b| b.insts.len() + 1).sum()
}

/// Whether `f` can be inlined: every way out of it is a return or a fault.
fn is_leaf(f: &Function) -> bool {
    f.blocks.iter().all(|b| {
        matches!(
            b.term,
            Term::Jump(_) | Term::Branch { .. } | Term::Ret(_) | Term::Fault { .. } | Term::None
        )
    })
}

/// Inlines leaf callees into their callers across `funcs`. Returns the
/// indexes of the functions that changed (they need optimizing again).
pub fn inline_leaves(funcs: &mut [Function]) -> Vec<usize> {
    let mut sites: HashMap<u64, usize> = HashMap::new();
    for f in funcs.iter() {
        for b in &f.blocks {
            if let Term::Call {
                target: CallTarget::Direct(t),
                ..
            } = b.term
            {
                *sites.entry(t).or_default() += 1;
            }
        }
    }
    let inlinable: HashMap<u64, Function> = funcs
        .iter()
        .filter(|g| {
            let n = size(g);
            is_leaf(g)
                && (n <= SMALL
                    || (n <= MEDIUM && sites.get(&g.entry).copied().unwrap_or(0) <= FEW_SITES))
        })
        .map(|g| (g.entry, g.clone()))
        .collect();
    let mut changed = vec![];
    for (i, f) in funcs.iter_mut().enumerate() {
        let mut any = false;
        // Call sites present before inlining (inlined code has no calls).
        let calls: Vec<BlockId> = (0..f.blocks.len() as BlockId)
            .filter(|&b| matches!(f.blocks[b as usize].term, Term::Call { target: CallTarget::Direct(t), .. } if inlinable.contains_key(&t) && t != f.entry))
            .collect();
        for b in calls {
            let Term::Call {
                target: CallTarget::Direct(t),
                ret,
                cont,
            } = f.blocks[b as usize].term.clone()
            else {
                unreachable!()
            };
            let g = &inlinable[&t];
            if size(f) + size(g) > MAX_CALLER {
                continue;
            }
            let entry = splice(f, g, ret, cont);
            f.blocks[b as usize].term = Term::Jump(entry);
            any = true;
        }
        if any {
            changed.push(i);
        }
    }
    changed
}

/// Copies `g` into `f`, returning the block that starts it. Returns of `g`
/// continue at `cont` when they pop `ret`, and leave the function for the
/// popped address otherwise.
fn splice(f: &mut Function, g: &Function, ret: u64, cont: BlockId) -> BlockId {
    let base = f.blocks.len() as BlockId;
    // State vregs are the machine's own; temporaries get fresh numbers.
    let mut vmap: Vec<V> = Vec::with_capacity(g.vtypes.len());
    for (v, &ty) in g.vtypes.iter().enumerate() {
        vmap.push(if (v as u32) < NUM_STATE {
            v as V
        } else {
            f.new_vreg(ty)
        });
    }
    let map = |v: &mut V| *v = vmap[*v as usize];
    for gb in &g.blocks {
        let mut insts = gb.insts.clone();
        for inst in &mut insts {
            if let Some(d) = &mut inst.dst {
                map(d);
            }
            for u in inst.op.uses_mut() {
                map(u);
            }
        }
        let mut term = gb.term.clone();
        for u in term.uses_mut() {
            map(u);
        }
        for s in term.successors_mut() {
            *s += base;
        }
        f.blocks.push(Block {
            addr: gb.addr,
            insts,
            term,
        });
    }
    // Returns: continue inline at the expected address, else leave.
    for k in base..f.blocks.len() as BlockId {
        let Term::Ret(v) = f.blocks[k as usize].term else {
            continue;
        };
        let addr = f.blocks[k as usize].addr;
        let eip = f.blocks[k as usize].insts.last().map_or(addr, |i| i.eip);
        // The popped address is i64 in 64-bit code.
        let rty = f.ty(v);
        let kc = f.new_vreg(rty);
        let c = f.new_vreg(Ty::I32);
        let leave = f.new_block(addr);
        f.blocks[leave as usize].term = Term::JmpInd(v);
        let blk = &mut f.blocks[k as usize];
        blk.insts.push(Inst {
            dst: Some(kc),
            op: Op::Const(ret),
            eip,
        });
        blk.insts.push(Inst {
            dst: Some(c),
            op: Op::Bin(
                if rty == Ty::I64 {
                    BinOp::I64Eq
                } else {
                    BinOp::I32Eq
                },
                v,
                kc,
            ),
            eip,
        });
        blk.term = Term::Branch {
            cond: c,
            t: cont,
            f: leave,
        };
    }
    base
}

#[cfg(test)]
mod tests {
    use super::*;

    /// f: push ret; call g; (cont) ret eax. g: eax = ecx + 1; ret.
    fn caller_and_callee() -> (Function, Function) {
        let mut g = Function::new(0x2000);
        g.new_block(0x2000);
        let one = g.new_vreg(Ty::I32);
        let ra = g.new_vreg(Ty::I32);
        g.blocks[0].insts = vec![
            Inst {
                dst: Some(one),
                op: Op::Const(1),
                eip: 0x2000,
            },
            Inst {
                dst: Some(EAX),
                op: Op::Bin(BinOp::I32Add, ECX, one),
                eip: 0x2000,
            },
            Inst {
                dst: Some(ra),
                op: Op::Load {
                    addr: ESP,
                    mem: Mem::guest(4),
                },
                eip: 0x2003,
            },
        ];
        g.blocks[0].term = Term::Ret(ra);
        let mut f = Function::new(0x1000);
        f.new_block(0x1000);
        f.new_block(0x1005);
        f.blocks[0].term = Term::Call {
            target: CallTarget::Direct(0x2000),
            ret: 0x1005,
            cont: 1,
        };
        f.blocks[1].term = Term::Ret(EAX);
        (f, g)
    }

    #[test]
    fn inlines_a_leaf() {
        let (f, g) = caller_and_callee();
        let mut funcs = vec![f, g];
        let changed = inline_leaves(&mut funcs);
        assert_eq!(changed, vec![0]);
        let f = &funcs[0];
        assert!(matches!(f.blocks[0].term, Term::Jump(2)));
        // The inlined return checks the address and continues at block 1.
        match &f.blocks[2].term {
            Term::Branch { t, f: leave, .. } => {
                assert_eq!(*t, 1);
                assert!(matches!(
                    funcs[0].blocks[*leave as usize].term,
                    Term::JmpInd(_)
                ));
            }
            t => panic!("{t:?}"),
        }
        // The callee itself is unchanged.
        assert!(matches!(funcs[1].blocks[0].term, Term::Ret(_)));
    }

    #[test]
    fn leaves_non_leaves_alone() {
        let (f, mut g) = caller_and_callee();
        g.blocks[0].term = Term::Exit(0x3000);
        let mut funcs = vec![f, g];
        assert!(inline_leaves(&mut funcs).is_empty());
    }
}
