//! Makes irreducible control flow reducible before code generation.
//!
//! WebAssembly only has structured control flow, so a loop with more than
//! one entry block cannot be expressed directly. GCC's jump threading makes
//! such loops out of ordinary state machines (CoreMark's
//! `core_state_transition` is one). Instead of falling back to a dispatch
//! loop over the whole function, each multi-entry loop gets a new header
//! that dispatches on a label variable to the entry that was meant; every
//! edge into an entry sets the label and goes to the header. The rest of
//! the function keeps its structure, and the cost is a constant and a
//! compare on edges into those entries. This is the scheme of LLVM's
//! WebAssemblyFixIrreducibleControlFlow, applied to strongly connected
//! regions recursively.
//!
//! The label vreg has several definitions, unlike other temporaries, so
//! this runs on a copy of the function inside code generation, after the
//! optimizer (which assumes single definitions) is done with it.

use crate::ir::*;

/// Rewrites `f` so that every loop has a single entry. Returns whether
/// anything changed.
pub fn make_reducible(f: &mut Function) -> bool {
    if is_reducible(f) {
        return false;
    }
    let order = f.rpo();
    // Block 0 must stay the function entry, so it cannot also be a loop
    // entry that gets redirected: move its code to a new block first.
    if f.predecessors()[0].iter().any(|&p| order.contains(&p)) {
        let moved = f.blocks.len() as BlockId;
        let mut b0 = f.blocks[0].clone();
        retarget(&mut b0.term, 0, moved);
        for b in f.blocks.iter_mut() {
            retarget(&mut b.term, 0, moved);
        }
        let addr = b0.addr;
        f.blocks.push(b0);
        f.blocks[0] = Block {
            addr,
            insts: vec![],
            term: Term::Jump(moved),
        };
    }
    let label = f.new_vreg(Ty::I32);
    let region: Vec<BlockId> = f.rpo();
    fix_region(f, label, &region, None);
    true
}

/// Merges jump tables that have the same targets into one dispatch block.
///
/// Compilers duplicate an interpreter's dispatch (`jmp *table(,%eax,4)`)
/// into the end of every handler, which makes each handler a loop entry and
/// the function irreducible. With one shared dispatch block (each former
/// table jump sets the shared index and jumps to it), that block is the
/// loop's only entry, as for a `switch` in a loop. Returns whether
/// anything changed.
pub fn merge_switches(f: &mut Function) -> bool {
    let mut groups: Vec<(Vec<BlockId>, Vec<BlockId>)> = vec![];
    for (b, blk) in f.blocks.iter().enumerate() {
        if let Term::Switch { targets, .. } = &blk.term {
            match groups.iter_mut().find(|(t, _)| t == targets) {
                Some((_, members)) => members.push(b as BlockId),
                None => groups.push((targets.clone(), vec![b as BlockId])),
            }
        }
    }
    let mut changed = false;
    for (targets, members) in groups {
        if members.len() < 2 {
            continue;
        }
        let addr = f.blocks[members[0] as usize].addr;
        let index = f.new_vreg(Ty::I32);
        let fallback = f.new_vreg(Ty::I32);
        let shared = f.new_block(addr);
        f.blocks[shared as usize].term = Term::Switch {
            index,
            targets,
            fallback,
        };
        for b in members {
            let blk = &mut f.blocks[b as usize];
            let Term::Switch {
                index: i,
                fallback: fb,
                ..
            } = blk.term.clone()
            else {
                unreachable!()
            };
            let eip = blk.insts.last().map_or(blk.addr, |x| x.eip);
            blk.insts.push(Inst {
                dst: Some(index),
                op: Op::Copy(i),
                eip,
            });
            blk.insts.push(Inst {
                dst: Some(fallback),
                op: Op::Copy(fb),
                eip,
            });
            blk.term = Term::Jump(shared);
        }
        changed = true;
    }
    changed
}

/// The test code generation uses: every retreating edge goes to a block
/// that dominates its source.
pub fn is_reducible(f: &Function) -> bool {
    let order = f.rpo();
    let n = f.blocks.len();
    let mut rpo_num = vec![u32::MAX; n];
    for (i, &b) in order.iter().enumerate() {
        rpo_num[b as usize] = i as u32;
    }
    let preds = f.predecessors();
    let idom = dominators(&order, &preds, &rpo_num);
    let dominates = |a: BlockId, mut b: BlockId| loop {
        if a == b {
            return true;
        }
        let d = idom[b as usize];
        if b == 0 || d == u32::MAX || d == b {
            return false;
        }
        b = d;
    };
    for &b in &order {
        for &p in &preds[b as usize] {
            let rp = rpo_num[p as usize];
            if rp != u32::MAX && rp >= rpo_num[b as usize] && !dominates(b, p) {
                return false;
            }
        }
    }
    true
}

fn dominators(order: &[BlockId], preds: &[Vec<BlockId>], rpo_num: &[u32]) -> Vec<u32> {
    let mut idom = vec![u32::MAX; preds.len()];
    idom[0] = 0;
    let intersect = |idom: &[u32], mut a: u32, mut b: u32| {
        while a != b {
            while rpo_num[a as usize] > rpo_num[b as usize] {
                a = idom[a as usize];
            }
            while rpo_num[b as usize] > rpo_num[a as usize] {
                b = idom[b as usize];
            }
        }
        a
    };
    let mut changed = true;
    while changed {
        changed = false;
        for &b in order.iter().skip(1) {
            let mut new = u32::MAX;
            for &p in &preds[b as usize] {
                if idom[p as usize] == u32::MAX {
                    continue;
                }
                new = if new == u32::MAX {
                    p
                } else {
                    intersect(&idom, p, new)
                };
            }
            if new != idom[b as usize] {
                idom[b as usize] = new;
                changed = true;
            }
        }
    }
    idom
}

fn retarget(t: &mut Term, from: BlockId, to: BlockId) {
    for s in t.successors_mut() {
        if *s == from {
            *s = to;
        }
    }
}

/// Fixes the strongly connected components of `region` (with edges into
/// `header` removed, which breaks the region's own loop), then recurses
/// into each of them.
fn fix_region(f: &mut Function, label: V, region: &[BlockId], header: Option<BlockId>) {
    let mut in_region = vec![false; f.blocks.len()];
    for &b in region {
        in_region[b as usize] = true;
    }
    let sccs = sccs(f, region, &in_region, header);
    for scc in sccs {
        let n = f.blocks.len();
        let mut in_scc = vec![false; n];
        for &b in &scc {
            in_scc[b as usize] = true;
        }
        let cyclic = scc.len() > 1 || {
            let b = scc[0];
            Some(b) != header && f.blocks[b as usize].term.successors().contains(&b)
        };
        if !cyclic {
            continue;
        }
        // Entries: blocks with a predecessor outside the component.
        let preds = f.predecessors();
        let reach = f.rpo();
        let mut reachable = vec![false; n];
        for &b in &reach {
            reachable[b as usize] = true;
        }
        let mut entries: Vec<BlockId> = scc
            .iter()
            .copied()
            .filter(|&b| {
                preds[b as usize]
                    .iter()
                    .any(|&p| reachable[p as usize] && !in_scc[p as usize])
            })
            .collect();
        entries.sort_by_key(|&b| reach.iter().position(|&x| x == b));
        if entries.is_empty() {
            continue;
        }
        if entries.len() == 1 {
            fix_region(f, label, &scc, Some(entries[0]));
            continue;
        }
        // New header dispatching on the label, and blocks that set the label
        // for each entry; every edge into an entry goes through one of them.
        // Edges from outside and from inside the component get separate
        // setters, so that only the header is entered from outside.
        let addr = f.blocks[entries[0] as usize].addr;
        let dispatch = f.new_block(addr);
        let mut body = scc.clone();
        body.push(dispatch);
        for (k, &e) in entries.iter().enumerate() {
            let ea = f.blocks[e as usize].addr;
            for inner in [false, true] {
                let from: Vec<BlockId> = preds[e as usize]
                    .iter()
                    .copied()
                    .filter(|&p| reachable[p as usize] && in_scc[p as usize] == inner)
                    .collect();
                if from.is_empty() {
                    continue;
                }
                let s = f.new_block(ea);
                f.blocks[s as usize].insts.push(Inst {
                    dst: Some(label),
                    op: Op::Const(k as u64),
                    eip: ea,
                });
                f.blocks[s as usize].term = Term::Jump(dispatch);
                for p in from {
                    retarget(&mut f.blocks[p as usize].term, e, s);
                }
                if inner {
                    body.push(s);
                }
            }
        }
        build_dispatch(f, label, dispatch, &entries, &mut body);
        fix_region(f, label, &body, Some(dispatch));
    }
}

/// Fills `dispatch` with a chain of compares on `label` (or a table for
/// many entries). Blocks it adds are appended to `body`.
fn build_dispatch(
    f: &mut Function,
    label: V,
    dispatch: BlockId,
    entries: &[BlockId],
    body: &mut Vec<BlockId>,
) {
    let addr = f.blocks[dispatch as usize].addr;
    if entries.len() > 4 {
        f.blocks[dispatch as usize].term = Term::Switch {
            index: label,
            targets: entries.to_vec(),
            fallback: label,
        };
        return;
    }
    let n = entries.len();
    if n == 2 {
        // The label is 0 or 1: branch on it directly.
        f.blocks[dispatch as usize].term = Term::Branch {
            cond: label,
            t: entries[1],
            f: entries[0],
        };
        return;
    }
    // label == 0 ? e0 : label == 1 ? e1 : ... : e(n-1)
    let mut cur = dispatch;
    for (k, &e) in entries[..n - 1].iter().enumerate() {
        let next = if k == n - 2 {
            entries[n - 1]
        } else {
            let b = f.new_block(addr);
            body.push(b);
            b
        };
        let kc = f.new_vreg(Ty::I32);
        let c = f.new_vreg(Ty::I32);
        let blk = &mut f.blocks[cur as usize];
        blk.insts.push(Inst {
            dst: Some(kc),
            op: Op::Const(k as u64),
            eip: addr,
        });
        blk.insts.push(Inst {
            dst: Some(c),
            op: Op::Bin(BinOp::I32Eq, label, kc),
            eip: addr,
        });
        blk.term = Term::Branch {
            cond: c,
            t: e,
            f: next,
        };
        cur = next;
    }
}

/// Tarjan's algorithm over the blocks of `region`, ignoring edges that
/// leave the region or go to `header`.
fn sccs(
    f: &Function,
    region: &[BlockId],
    in_region: &[bool],
    header: Option<BlockId>,
) -> Vec<Vec<BlockId>> {
    let n = f.blocks.len();
    let succs = |b: BlockId| -> Vec<BlockId> {
        f.blocks[b as usize]
            .term
            .successors()
            .into_iter()
            .filter(|&s| in_region[s as usize] && Some(s) != header)
            .collect()
    };
    let mut index = vec![u32::MAX; n];
    let mut low = vec![0u32; n];
    let mut on_stack = vec![false; n];
    let mut stack: Vec<BlockId> = vec![];
    let mut next = 0u32;
    let mut out = vec![];
    for &root in region {
        if index[root as usize] != u32::MAX {
            continue;
        }
        let mut work: Vec<(BlockId, usize)> = vec![(root, 0)];
        index[root as usize] = next;
        low[root as usize] = next;
        next += 1;
        stack.push(root);
        on_stack[root as usize] = true;
        while let Some(&mut (v, ref mut i)) = work.last_mut() {
            let ss = succs(v);
            if *i < ss.len() {
                let w = ss[*i];
                *i += 1;
                if index[w as usize] == u32::MAX {
                    index[w as usize] = next;
                    low[w as usize] = next;
                    next += 1;
                    stack.push(w);
                    on_stack[w as usize] = true;
                    work.push((w, 0));
                } else if on_stack[w as usize] {
                    low[v as usize] = low[v as usize].min(index[w as usize]);
                }
            } else {
                work.pop();
                if let Some(&(p, _)) = work.last() {
                    low[p as usize] = low[p as usize].min(low[v as usize]);
                }
                if low[v as usize] == index[v as usize] {
                    let mut c = vec![];
                    loop {
                        let w = stack.pop().unwrap();
                        on_stack[w as usize] = false;
                        c.push(w);
                        if w == v {
                            break;
                        }
                    }
                    out.push(c);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 0 -> 1, 0 -> 2, 1 <-> 2, 2 -> 3: a loop with two entries.
    fn two_entry_loop() -> Function {
        let mut f = Function::new(0x1000);
        for i in 0..4 {
            f.new_block(0x1000 + i * 0x10);
        }
        let c = f.new_vreg(Ty::I32);
        f.blocks[0].insts.push(Inst {
            dst: Some(c),
            op: Op::Copy(EAX),
            eip: 0x1000,
        });
        f.blocks[0].term = Term::Branch {
            cond: c,
            t: 1,
            f: 2,
        };
        f.blocks[1].term = Term::Jump(2);
        f.blocks[2].term = Term::Branch {
            cond: ECX,
            t: 1,
            f: 3,
        };
        f.blocks[3].term = Term::Ret(EAX);
        f
    }

    #[test]
    fn fixes_two_entry_loop() {
        let mut f = two_entry_loop();
        assert!(!is_reducible(&f));
        assert!(make_reducible(&mut f));
        assert!(is_reducible(&f));
    }

    #[test]
    fn fixes_loop_through_entry_block() {
        // 0 <-> 1 and 2 -> 0, 2 -> 1 after an entry branch: block 0 is a
        // loop entry and the function entry.
        let mut f = two_entry_loop();
        f.blocks[3].term = Term::Branch {
            cond: EDX,
            t: 0,
            f: 3,
        };
        assert!(make_reducible(&mut f));
        assert!(is_reducible(&f));
    }

    /// Entries 1, 2, 3 all reachable from 0 and each other in a ring.
    fn three_entry_loop() -> Function {
        let mut f = Function::new(0x1000);
        for i in 0..6 {
            f.new_block(0x1000 + i * 0x10);
        }
        f.blocks[0].term = Term::Branch {
            cond: EAX,
            t: 1,
            f: 4,
        };
        f.blocks[4].term = Term::Branch {
            cond: ECX,
            t: 2,
            f: 3,
        };
        f.blocks[1].term = Term::Branch {
            cond: EDX,
            t: 2,
            f: 5,
        };
        f.blocks[2].term = Term::Jump(3);
        f.blocks[3].term = Term::Jump(1);
        f.blocks[5].term = Term::Ret(EAX);
        f
    }

    /// Follows the graph from block 0 with the given branch decisions
    /// (`EAX`, `ECX`, `EDX` as booleans), evaluating dispatch compares, and
    /// returns the original blocks visited, up to `steps` of them.
    fn walk(f: &Function, eax: bool, ecx: bool, edx: bool, steps: usize) -> Vec<BlockId> {
        use std::collections::HashMap;
        let mut vals: HashMap<V, u64> = HashMap::new();
        vals.insert(EAX, eax as u64);
        vals.insert(ECX, ecx as u64);
        vals.insert(EDX, edx as u64);
        let mut b = 0;
        let mut seen = vec![];
        for _ in 0..1000 {
            if b < 6 {
                seen.push(b);
                if seen.len() == steps {
                    break;
                }
            }
            for i in &f.blocks[b as usize].insts {
                let v = match i.op {
                    Op::Const(c) => c,
                    Op::Bin(BinOp::I32Eq, x, y) => (vals[&x] == vals[&y]) as u64,
                    _ => unreachable!(),
                };
                vals.insert(i.dst.unwrap(), v);
            }
            b = match &f.blocks[b as usize].term {
                Term::Jump(t) => *t,
                Term::Branch { cond, t, f } => {
                    if vals[cond] != 0 {
                        *t
                    } else {
                        *f
                    }
                }
                _ => break,
            };
        }
        seen
    }

    #[test]
    fn three_entries_keep_their_paths() {
        let orig = three_entry_loop();
        let mut f = orig.clone();
        assert!(make_reducible(&mut f));
        assert!(is_reducible(&f));
        // Original blocks keep their code and terminators except for
        // retargeted edges; every path visits the same original blocks.
        for &(a, c, d) in &[
            (true, false, false),
            (false, true, true),
            (false, false, true),
            (true, true, false),
        ] {
            assert_eq!(
                walk(&orig, a, c, d, 12),
                walk(&f, a, c, d, 12),
                "eax={a} ecx={c} edx={d}"
            );
        }
    }

    /// An interpreter shape: two table jumps over the same handlers, each
    /// handler ending in one of them.
    #[test]
    fn merges_duplicated_dispatch() {
        let mut f = Function::new(0x1000);
        for i in 0..5 {
            f.new_block(0x1000 + i * 0x10);
        }
        let fb = f.new_vreg(Ty::I32);
        f.blocks[0].term = Term::Switch {
            index: EAX,
            targets: vec![1, 2],
            fallback: fb,
        };
        f.blocks[1].term = Term::Jump(3);
        f.blocks[2].term = Term::Branch {
            cond: ECX,
            t: 4,
            f: 3,
        };
        f.blocks[3].term = Term::Switch {
            index: EDX,
            targets: vec![1, 2],
            fallback: fb,
        };
        f.blocks[4].term = Term::Ret(EAX);
        assert!(!is_reducible(&f));
        assert!(merge_switches(&mut f));
        let switches = f
            .blocks
            .iter()
            .filter(|b| matches!(b.term, Term::Switch { .. }))
            .count();
        assert_eq!(switches, 1);
        make_reducible(&mut f);
        assert!(is_reducible(&f));
    }

    #[test]
    fn leaves_reducible_alone() {
        let mut f = two_entry_loop();
        f.blocks[0].term = Term::Jump(1);
        assert!(is_reducible(&f));
        assert!(!make_reducible(&mut f));
    }
}
