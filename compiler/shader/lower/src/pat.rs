//! Patterns (0004 §9.1) and `switch`: a `switch` statement whose labels are all integer or enum
//! constants without guards becomes an IR `switch`; anything else becomes a chain of
//! selections, one region per test (each with a merge block of its own that leads to the
//! next one out), so every region keeps a single exit.

use marlin_front_hir_def::store::{BinaryOp, ExprId, Pat, PatId, StmtId, SwitchArm, SwitchSection};
use marlin_front_hir_def::{ItemTreeQuery, Name};
use marlin_shaderc_hir_ty::{InferLoc, Ty};
use marlin_shaderc_ir::{BinaryOp as IrBin, Block, Op, UnaryOp as IrUn, Value};

use crate::func::{FnCx, Jumps, Term};
use crate::types::Shape;
use crate::{LowerError, R};

impl FnCx<'_, '_, '_> {
    /// Whether `v` (of type `ty`) matches pattern `p`; binds the pattern's variables.
    pub(crate) fn pat_test(&mut self, p: PatId, v: Value, ty: &Ty) -> R<Value> {
        let loc = InferLoc::Pat(p);
        match self.body.store.pats[p].clone() {
            Pat::Missing => Err(self.error("E1305", "broken pattern", loc)),
            Pat::Discard => Ok(self.bool_const(true)),
            Pat::Var(b) => {
                self.write_local(b, v)?;
                Ok(self.bool_const(true))
            }
            Pat::Constant(e) => {
                let c = self.value(e)?;
                self.builtin_binary(BinaryOp::Eq, (v, ty), (c, ty), e)
            }
            Pat::Relational { op, value } => {
                let c = self.value(value)?;
                self.builtin_binary(op, (v, ty), (c, ty), value)
            }
            Pat::And(a, b) => {
                let (x, y) = (self.pat_test(a, v, ty)?, self.pat_test(b, v, ty)?);
                self.op(Op::Binary(IrBin::And), vec![x, y], None)
            }
            Pat::Or(a, b) => {
                let (x, y) = (self.pat_test(a, v, ty)?, self.pat_test(b, v, ty)?);
                self.op(Op::Binary(IrBin::Or), vec![x, y], None)
            }
            Pat::Not(a) => {
                let x = self.pat_test(a, v, ty)?;
                self.op(Op::Unary(IrUn::Not), vec![x], None)
            }
            Pat::Property(items) => {
                let mut all = self.bool_const(true);
                for (names, sub) in items {
                    let (mut part, mut pty) = (v, ty.clone());
                    for name in names.unwrap_or_default() {
                        (part, pty) = self.member_by_name(part, &pty, &name, loc.clone())?;
                    }
                    let t = self.pat_test(sub, part, &pty)?;
                    all = self.op(Op::Binary(IrBin::And), vec![all, t], None)?;
                }
                Ok(all)
            }
            Pat::Positional(items) => {
                let Ty::Tuple(tys) = ty else {
                    return Err(LowerError::unsupported(
                        "a positional pattern on this type",
                        self.owner(),
                        loc,
                    ));
                };
                let tys = tys.clone();
                let mut all = self.bool_const(true);
                for (i, (sub, t)) in items.into_iter().zip(tys.iter()).enumerate() {
                    let part = self.op(Op::ExtractField(i as u32), vec![v], None)?;
                    let x = self.pat_test(sub, part, t)?;
                    all = self.op(Op::Binary(IrBin::And), vec![all, x], None)?;
                }
                Ok(all)
            }
        }
    }

    /// A field of a struct, or a component of a vector, by name.
    fn member_by_name(&mut self, v: Value, ty: &Ty, name: &Name, loc: InferLoc) -> R<(Value, Ty)> {
        if let Some((_, Shape::Vector(_))) = self.l.num_shape(ty)? {
            let lane = match name.as_str() {
                "x" | "r" => 0,
                "y" | "g" => 1,
                "z" | "b" => 2,
                "w" | "a" => 3,
                _ => {
                    return Err(LowerError::unsupported(
                        format!("`.{name}` in a pattern"),
                        self.owner(),
                        loc,
                    ));
                }
            };
            let element = match ty {
                Ty::Adt { args, .. } => match args.first() {
                    Some(marlin_shaderc_hir_ty::GenericArgTy::Type(t)) => t.clone(),
                    _ => Ty::Error,
                },
                _ => Ty::Error,
            };
            return Ok((self.op(Op::Extract(lane), vec![v], None)?, element));
        }
        let (_, fields) = self.l.struct_of(ty)?;
        for (i, f) in fields.iter().enumerate() {
            let tree = self.l.cx.get::<ItemTreeQuery>(&f.file)?;
            if tree.items[f.local].name.as_ref() == Some(name) {
                let types = self.l.cx.get::<marlin_shaderc_hir_ty::ItemTypesQuery>(f)?;
                let fty = match ty {
                    Ty::Adt { item, args } => marlin_shaderc_hir_ty::Subst::new()
                        .with_args(*item, args)
                        .ty(types.ty.as_ref().unwrap_or(&Ty::Error)),
                    _ => Ty::Error,
                };
                return Ok((self.op(Op::ExtractField(i as u32), vec![v], None)?, fty));
            }
        }
        Err(self.error("E1305", format!("no field `{name}`"), loc))
    }

    /// `c && guard`, with the guard evaluated only where `c` holds.
    fn guarded(&mut self, c: Value, guard: Option<ExprId>) -> R<Value> {
        let Some(guard) = guard else {
            return Ok(c);
        };
        let t = self.f.ty(c);
        let var = self.ssa.new_var(t, None);
        self.write_var(var, c);
        let (rest, merge) = (self.new_block(), self.new_block());
        self.terminate(Term::CondBr {
            cond: c,
            then_block: rest,
            else_block: merge,
            merge,
        });
        self.seal(rest);
        self.enter(rest);
        let g = self.value(guard)?;
        self.write_var(var, g);
        self.terminate(Term::Br(merge));
        self.seal(merge);
        self.enter(merge);
        Ok(self.read_var(var))
    }

    /// Runs one test of a chain: a selection on `cond` whose true side `then` runs and whose
    /// false side continues the chain. Returns the selection's merge block.
    fn chain_link(
        &mut self,
        cond: Value,
        then: &mut dyn FnMut(&mut Self, Block) -> R<()>,
    ) -> R<Block> {
        let (yes, no, merge) = (self.new_block(), self.new_block(), self.new_block());
        self.terminate(Term::CondBr {
            cond,
            then_block: yes,
            else_block: no,
            merge,
        });
        self.seal(yes);
        self.seal(no);
        self.enter(yes);
        then(self, merge)?;
        if self.block.is_some() {
            self.terminate(Term::Br(merge));
        }
        self.enter(no);
        Ok(merge)
    }

    /// Leaves a chain: each merge block, innermost first, jumps to the next one out.
    fn unwind(&mut self, merges: Vec<Block>, last: Block) {
        if self.block.is_some() {
            self.terminate(Term::Br(*merges.last().unwrap_or(&last)));
        }
        let mut targets: Vec<Block> = vec![last];
        targets.extend(merges.iter().copied());
        for k in (0..merges.len()).rev() {
            let m = merges[k];
            self.seal(m);
            self.enter(m);
            let preds = self.ssa.preds.get(&m).map_or(0, Vec::len);
            if preds == 0 {
                self.block = None;
                self.terminate_dead();
            } else {
                self.terminate(Term::Br(targets[k]));
            }
        }
        self.seal(last);
        let preds = self.ssa.preds.get(&last).map_or(0, Vec::len);
        self.block = if preds == 0 { None } else { Some(last) };
    }

    /// Nothing to do: a block nothing reaches ends in `unreachable` when the function is done.
    fn terminate_dead(&mut self) {}

    pub(crate) fn switch_expr(
        &mut self,
        e: ExprId,
        scrutinee: ExprId,
        arms: &[SwitchArm],
    ) -> R<Value> {
        let v = self.value(scrutinee)?;
        let ty = self.final_ty(scrutinee);
        let t = self.l.ty(&self.expr_ty(e))?;
        let var = self.ssa.new_var(t, None);
        let last = self.new_block();
        let mut merges = Vec::new();
        for arm in arms {
            let c = self.pat_test(arm.pat, v, &ty)?;
            let c = self.guarded(c, arm.guard)?;
            let value = arm.expr;
            let m = self.chain_link(c, &mut |cx, _| {
                let x = cx.value(value)?;
                cx.write_var(var, x);
                Ok(())
            })?;
            merges.push(m);
        }
        // Exhaustive (5c): nothing gets past the last arm.
        self.terminate(Term::Unreachable);
        self.unwind(merges, last);
        if self.block.is_none() {
            self.enter(last);
        }
        Ok(self.read_var(var))
    }

    pub(crate) fn switch_stmt(
        &mut self,
        s: StmtId,
        scrutinee: ExprId,
        sections: &[SwitchSection],
    ) -> R<()> {
        let v = self.value(scrutinee)?;
        let ty = self.final_ty(scrutinee);
        if let Some(cases) = self.constant_cases(sections, &ty)? {
            return self.ir_switch(v, sections, cases);
        }
        let _ = s;
        // A chain of tests, the `default` section last.
        let last = self.new_block();
        let mut merges = Vec::new();
        let mut default = None;
        for section in sections {
            if section.labels.iter().any(Option::is_none) {
                default = Some(section);
                continue;
            }
            let mut c: Option<Value> = None;
            for (pat, guard) in section.labels.iter().flatten() {
                let t = self.pat_test(*pat, v, &ty)?;
                let t = self.guarded(t, *guard)?;
                c = Some(match c {
                    None => t,
                    Some(prev) => self.op(Op::Binary(IrBin::Or), vec![prev, t], None)?,
                });
            }
            let c = match c {
                Some(c) => c,
                None => self.bool_const(false),
            };
            let body = section.body.clone();
            let m = self.chain_link(c, &mut |cx, merge| {
                cx.jumps.push(Jumps {
                    brk: merge,
                    cont: None,
                });
                let r = body.iter().try_for_each(|&st| cx.stmt(st));
                cx.jumps.pop();
                r
            })?;
            merges.push(m);
        }
        if let Some(section) = default {
            let brk = *merges.last().unwrap_or(&last);
            self.jumps.push(Jumps { brk, cont: None });
            let r = section.body.iter().try_for_each(|&st| self.stmt(st));
            self.jumps.pop();
            r?;
        }
        self.unwind(merges, last);
        Ok(())
    }

    /// The case values when every label is a constant without a guard.
    fn constant_cases(&mut self, sections: &[SwitchSection], ty: &Ty) -> R<Option<Vec<Vec<u64>>>> {
        let integer = self
            .l
            .num_shape(ty)?
            .is_some_and(|(n, s)| s == Shape::Scalar && !n.is_float());
        if !integer {
            return Ok(None);
        }
        let mut out = Vec::new();
        for section in sections {
            let mut values = Vec::new();
            for label in &section.labels {
                match label {
                    None => {}
                    Some((p, None)) => match self.body.store.pats[*p] {
                        Pat::Constant(e) => {
                            let c = self.value(e)?;
                            match self.const_bits(c) {
                                Some(b) => values.push(b),
                                None => return Ok(None),
                            }
                        }
                        _ => return Ok(None),
                    },
                    Some(_) => return Ok(None),
                }
            }
            out.push(values);
        }
        Ok(Some(out))
    }

    fn ir_switch(&mut self, v: Value, sections: &[SwitchSection], cases: Vec<Vec<u64>>) -> R<()> {
        let merge = self.new_block();
        let blocks: Vec<Block> = sections.iter().map(|_| self.new_block()).collect();
        let default = sections
            .iter()
            .position(|s| s.labels.iter().any(Option::is_none))
            .map_or(merge, |i| blocks[i]);
        let mut ir_cases = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for (values, &b) in cases.iter().zip(&blocks) {
            for &value in values {
                if seen.insert(value) {
                    ir_cases.push((value, b));
                }
            }
        }
        self.terminate(Term::Switch {
            value: v,
            cases: ir_cases,
            default,
            merge,
        });
        for (section, &b) in sections.iter().zip(&blocks) {
            self.seal(b);
            self.enter(b);
            self.jumps.push(Jumps {
                brk: merge,
                cont: None,
            });
            let r = section.body.iter().try_for_each(|&st| self.stmt(st));
            self.jumps.pop();
            r?;
            if self.block.is_some() {
                self.terminate(Term::Br(merge));
            }
        }
        self.seal(merge);
        let preds = self.ssa.preds.get(&merge).map_or(0, Vec::len);
        self.block = if preds == 0 { None } else { Some(merge) };
        Ok(())
    }
}
