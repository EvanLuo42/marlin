//! Inlining every call (0005 §4.4 pass 1.1; the target description may later keep large
//! callees as calls where calls exist). Callees go first, so what is inlined has no calls left.
//!
//! The callee's body goes inside a loop that runs once, `loop header H continue C merge K`,
//! where `K` takes the results and holds the rest of the caller's block, so that a `return`
//! becomes a jump out of a structured region (0006 §5.1). A `return` that is not inside a loop
//! or `switch` of the callee is a `break` of that loop: `br K(values)`. A `return` inside one
//! breaks only out of the innermost loop or `switch` (the one place a break may go): it stores
//! its values and a flag, jumps to that region's merge block, and there a check on the flag
//! goes on out, region by region, to `K`. mem2reg turns the flag and the values into SSA.

use std::collections::{HashMap, HashSet};

use marlin_shaderc_ir::cfg::{Cfg, RegionKind, Structure};
use marlin_shaderc_ir::{
    Block, FuncId, Function, Inst, Module, Op, Scalar, Space, SwitchCase, Type, TypeData, Val,
    Value, ValueDef,
};

use crate::edit::{constant, zero};
use crate::{Analyses, Pass};

pub struct Inline;

impl Pass for Inline {
    fn name(&self) -> &'static str {
        "inline"
    }

    fn run(&self, m: &mut Module, _: &mut Analyses) -> bool {
        let mut changed = false;
        for id in callees_first(m) {
            while let Some(call) = first_call(&m.funcs[id], id) {
                let Op::Call(callee_id) = m.funcs[id].insts[call].op else {
                    unreachable!()
                };
                let callee = tidy(&m.funcs[callee_id]);
                let mut f = std::mem::replace(&mut m.funcs[id], Function::new("", Vec::new()));
                inline_call(m, &mut f, call, &callee);
                m.funcs[id] = f;
                changed = true;
            }
        }
        changed
    }
}

/// The functions in an order where each comes after the functions it calls. A function in a
/// cycle (runtime recursion, which the verifier rejects) keeps its calls into the cycle.
fn callees_first(m: &Module) -> Vec<FuncId> {
    let mut order = Vec::new();
    let mut state: HashMap<FuncId, bool> = HashMap::new(); // false: on the stack; true: done
    for (root, _) in m.funcs.iter() {
        if state.contains_key(&root) {
            continue;
        }
        let mut stack = vec![(root, calls(m, root), 0usize)];
        state.insert(root, false);
        while let Some((f, cs, i)) = stack.last_mut() {
            if let Some(&c) = cs.get(*i) {
                *i += 1;
                if let std::collections::hash_map::Entry::Vacant(e) = state.entry(c) {
                    e.insert(false);
                    let next = calls(m, c);
                    stack.push((c, next, 0));
                }
            } else {
                let f = *f;
                state.insert(f, true);
                order.push(f);
                stack.pop();
            }
        }
    }
    order
}

fn calls(m: &Module, f: FuncId) -> Vec<FuncId> {
    let func = &m.funcs[f];
    func.layout
        .iter()
        .flat_map(|&b| func.insts(b).collect::<Vec<_>>())
        .filter_map(|i| match func.insts[i].op {
            Op::Call(c) => Some(c),
            _ => None,
        })
        .collect()
}

/// The first call in `f` to something other than itself whose callee has no calls left.
fn first_call(f: &Function, id: FuncId) -> Option<Inst> {
    f.layout
        .iter()
        .flat_map(|&b| f.insts(b).collect::<Vec<_>>())
        .find(|&i| matches!(f.insts[i].op, Op::Call(c) if c != id))
}

/// A copy of `callee` without unreachable code (merge and continue blocks stay, empty).
fn tidy(callee: &Function) -> Function {
    let mut c = callee.clone();
    let cfg = Cfg::new(&c);
    let st = Structure::new(&c, &cfg);
    crate::simplify_cfg::remove_unreachable(&mut c, &cfg, &st);
    c
}

fn remap_op(op: &Op, blocks: &HashMap<Block, Block>) -> Op {
    let b = |x: &Block| blocks[x];
    match op {
        Op::Br(t) => Op::Br(b(t)),
        Op::CondBr {
            then_block,
            then_args,
            else_block,
            merge,
        } => Op::CondBr {
            then_block: b(then_block),
            then_args: *then_args,
            else_block: b(else_block),
            merge: b(merge),
        },
        Op::Switch {
            cases,
            default,
            default_args,
            merge,
        } => Op::Switch {
            cases: cases
                .iter()
                .map(|c| SwitchCase {
                    value: c.value,
                    block: b(&c.block),
                    args: c.args,
                })
                .collect(),
            default: b(default),
            default_args: *default_args,
            merge: b(merge),
        },
        Op::Loop {
            header,
            continuing,
            merge,
        } => Op::Loop {
            header: b(header),
            continuing: b(continuing),
            merge: b(merge),
        },
        other => other.clone(),
    }
}

/// A new block right after `after` in the layout.
fn block_after(f: &mut Function, after: Block) -> Block {
    let b = f.add_block();
    f.layout.pop();
    let at = f
        .layout
        .iter()
        .position(|&x| x == after)
        .map_or(f.layout.len(), |i| i + 1);
    f.layout.insert(at, b);
    b
}

fn append(f: &mut Function, block: Block, op: Op, operands: Vec<Value>, results: &[Type]) -> Inst {
    let inst = f.create_inst(op, operands, results);
    f.append_inst(block, inst);
    inst
}

/// Where a `return` inside the callee must go: its values, the flag, and the regions whose
/// merge blocks check the flag.
struct Exits {
    values: Vec<Value>,
    flag: Value,
}

fn inline_call(m: &mut Module, f: &mut Function, call: Inst, callee: &Function) {
    let b = f.block_of(call).expect("a placed call");
    let args = f.operands(call).to_vec();

    // The block after the call, which takes the results.
    let k = block_after(f, b);
    let results = f.insts[call].results.clone();
    for (&r, &t) in results.iter().zip(&callee.results) {
        let p = f.add_param(k, t, None);
        f.replace_all_uses(r, p);
    }
    let mut after = Vec::new();
    let mut at = f.next_inst(call);
    while let Some(i) = at {
        at = f.next_inst(i);
        after.push(i);
    }
    for i in after {
        f.detach(i);
        f.append_inst(k, i);
    }
    f.remove_inst(call);

    let cfg = Cfg::new(callee);
    let st = Structure::new(callee, &cfg);
    // The innermost loop or switch around each of the callee's blocks.
    let breakable = |bi: usize| {
        st.regions
            .iter()
            .enumerate()
            .filter(|(_, r)| r.kind != RegionKind::Selection && r.blocks[bi])
            .min_by_key(|(_, r)| r.blocks.iter().filter(|x| **x).count())
            .map(|(i, _)| i)
    };

    // The callee's blocks, and the loop around them.
    let header = block_after(f, b);
    let mut blocks: HashMap<Block, Block> = HashMap::new();
    let mut last = header;
    for &cb in &callee.layout {
        let nb = block_after(f, last);
        blocks.insert(cb, nb);
        last = nb;
    }
    let cont = block_after(f, last);
    let mut values: HashMap<Value, Value> = HashMap::new();
    for (&p, &a) in callee.params().iter().zip(&args) {
        values.insert(p, a);
    }
    for &cb in callee.layout.iter().skip(1) {
        for &p in &callee.blocks[cb].params {
            let np = f.add_param(blocks[&cb], callee.ty(p), callee.values[p].name.as_deref());
            values.insert(p, np);
        }
    }
    let entry = blocks[&callee.layout[0]];
    append(f, header, Op::Br(entry), Vec::new(), &[]);
    append(f, cont, Op::Unreachable, Vec::new(), &[]);

    // Values and the flag for returns that break out of nested regions, made before the loop.
    let nested_returns = callee.layout.iter().enumerate().any(|(bi, &cb)| {
        callee
            .terminator(cb)
            .is_some_and(|t| callee.insts[t].op == Op::Return && breakable(bi).is_some())
    });
    let exits = nested_returns.then(|| {
        let mut vals = Vec::new();
        for &t in &callee.results {
            let ptr = m.ptr(Space::Function, false, t);
            vals.push(entry_var(f, ptr));
        }
        let bool_ty = m.scalar(Scalar::I1);
        let ptr = m.ptr(Space::Function, false, bool_ty);
        let flag = entry_var(f, ptr);
        let no = constant(m, f, &Val::Bits(0), bool_ty);
        append(f, b, Op::Store { aligned: None }, vec![flag, no], &[]);
        Exits { values: vals, flag }
    });
    append(
        f,
        b,
        Op::Loop {
            header,
            continuing: cont,
            merge: k,
        },
        Vec::new(),
        &[],
    );

    // The instructions, definitions before uses (reachable blocks in reverse postorder; the
    // rest are empty after `tidy`).
    let mut order: Vec<usize> = Vec::new();
    {
        let mut seen = vec![false; cfg.len()];
        let mut stack = vec![(0usize, 0usize)];
        seen[0] = true;
        let mut post = Vec::new();
        while let Some((bi, i)) = stack.pop() {
            if let Some(&s) = cfg.succs[bi].get(i) {
                stack.push((bi, i + 1));
                if !seen[s] {
                    seen[s] = true;
                    stack.push((s, 0));
                }
            } else {
                post.push(bi);
            }
        }
        post.reverse();
        order.extend(post);
        order.extend((0..cfg.len()).filter(|&i| !seen[i]));
    }
    let mut checks: Vec<usize> = Vec::new();
    for bi in order {
        let cb = callee.layout[bi];
        let nb = blocks[&cb];
        for inst in callee.insts(cb) {
            let data = &callee.insts[inst];
            let operands: Vec<Value> = callee
                .operands(inst)
                .iter()
                .map(|&v| match callee.values[v].def {
                    ValueDef::Const(c) => f.const_value(c, callee.ty(v)),
                    ValueDef::Global(g) => f.global_value(g, callee.ty(v)),
                    _ => values[&v],
                })
                .collect();
            if data.op == Op::Return {
                match (breakable(bi), &exits) {
                    (None, _) | (_, None) => {
                        append(f, nb, Op::Br(k), operands, &[]);
                    }
                    (Some(r), Some(ex)) => {
                        for (&ptr, &v) in ex.values.iter().zip(&operands) {
                            append(f, nb, Op::Store { aligned: None }, vec![ptr, v], &[]);
                        }
                        let bool_ty = m.scalar(Scalar::I1);
                        let yes = constant(m, f, &Val::Bits(1), bool_ty);
                        append(f, nb, Op::Store { aligned: None }, vec![ex.flag, yes], &[]);
                        let merge = blocks[&callee.layout[st.regions[r].merge]];
                        let zeros = zero_args(m, f, merge);
                        append(f, nb, Op::Br(merge), zeros, &[]);
                        if !checks.contains(&r) {
                            checks.push(r);
                        }
                    }
                }
                continue;
            }
            // Variables are made in the entry block; where the callee made one, it starts zeroed.
            if data.op == Op::Var {
                let ptr = callee.ty(data.results[0]);
                let v = entry_var(f, ptr);
                if let TypeData::Ptr { pointee, .. } = *m.data(ptr)
                    && !matches!(m.data(pointee), TypeData::RayQuery)
                {
                    let z = zero(m, f, pointee);
                    append(f, nb, Op::Store { aligned: None }, vec![v, z], &[]);
                }
                values.insert(data.results[0], v);
                continue;
            }
            let types: Vec<Type> = data.results.iter().map(|&r| callee.ty(r)).collect();
            let new = append(f, nb, remap_op(&data.op, &blocks), operands, &types);
            f.insts[new].precise = data.precise;
            f.insts[new].loc = data.loc;
            for (&old, &n) in data.results.iter().zip(&f.insts[new].results.clone()) {
                if let Some(name) = callee.values[old].name.as_deref() {
                    f.set_name(n, Some(name));
                }
                values.insert(old, n);
            }
        }
    }

    // At the merge of each region a return broke out of: go on out if the flag is set.
    let mut done: HashSet<usize> = HashSet::new();
    while let Some(r) = checks.pop() {
        if !done.insert(r) {
            continue;
        }
        let ex = exits.as_ref().expect("returns that break out");
        let merge_index = st.regions[r].merge;
        let mb = blocks[&callee.layout[merge_index]];
        let rest = block_after(f, mb);
        let params = f.blocks[mb].params.clone();
        let mut passed = Vec::new();
        for &p in &params {
            let q = f.add_param(rest, f.ty(p), None);
            f.replace_all_uses(p, q);
            passed.push(p);
        }
        let moved: Vec<_> = f.insts(mb).collect();
        for i in moved {
            f.detach(i);
            f.append_inst(rest, i);
        }
        let bool_ty = m.scalar(Scalar::I1);
        let set = append_value(f, mb, Op::Load { aligned: None }, vec![ex.flag], &[bool_ty]);
        // The region around the merge block, not one it heads (that moves into `rest`).
        let around = st
            .regions
            .iter()
            .enumerate()
            .filter(|(_, x)| {
                x.kind != RegionKind::Selection && x.blocks[merge_index] && x.header != merge_index
            })
            .min_by_key(|(_, x)| x.blocks.iter().filter(|b| **b).count())
            .map(|(i, _)| i);
        let (out, out_args) = match around {
            Some(u) => {
                let target = blocks[&callee.layout[st.regions[u].merge]];
                checks.push(u);
                (target, zero_args(m, f, target))
            }
            None => {
                let leave = block_after(f, mb);
                let mut vals = Vec::new();
                for (&ptr, &t) in ex.values.iter().zip(&callee.results) {
                    vals.push(append_value(
                        f,
                        leave,
                        Op::Load { aligned: None },
                        vec![ptr],
                        &[t],
                    ));
                }
                append(f, leave, Op::Br(k), vals, &[]);
                (leave, Vec::new())
            }
        };
        let then_args = out_args.len() as u32;
        let mut operands = vec![set];
        operands.extend(out_args);
        operands.extend(passed);
        append(
            f,
            mb,
            Op::CondBr {
                then_block: out,
                then_args,
                else_block: rest,
                merge: rest,
            },
            operands,
            &[],
        );
    }
}

/// Zero arguments for each parameter of `target`.
fn zero_args(m: &mut Module, f: &mut Function, target: Block) -> Vec<Value> {
    let types: Vec<Type> = f.blocks[target].params.iter().map(|&p| f.ty(p)).collect();
    types.into_iter().map(|t| zero(m, f, t)).collect()
}

fn append_value(
    f: &mut Function,
    block: Block,
    op: Op,
    operands: Vec<Value>,
    results: &[Type],
) -> Value {
    let inst = append(f, block, op, operands, results);
    f.result(inst)
}

/// A new `var` at the start of `f`'s entry block, where variables are made.
fn entry_var(f: &mut Function, ptr: Type) -> Value {
    let inst = f.create_inst(Op::Var, Vec::new(), &[ptr]);
    let entry = f.layout[0];
    match f.first_inst(entry) {
        Some(first) => f.insert_before(first, inst),
        None => f.append_inst(entry, inst),
    }
    f.result(inst)
}
