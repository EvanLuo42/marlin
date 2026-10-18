//! Statements and structured control flow (0006 §5.1).
//!
//! A loop is `loop header H continue C merge M` in the block before it. `while` and `for` test
//! in the header (`cond_br c, body, M merge body`); `do` tests in the continue block, through a
//! latch block that holds the back edge. `break` jumps to M, `continue` to C. Code after a
//! jump or `return` is dead and not lowered.

use marlin_front_hir_def::store::{Expr, ExprId, Stmt, StmtId};
use marlin_shaderc_hir_ty::{InferLoc, Ty};
use marlin_shaderc_ir::{AccessStep, BinaryOp as IrBin, Block, IntCmp, Op, TypeData};

use crate::func::{FnCx, Jumps, Term};
use crate::types::Num;
use crate::{LowerError, R};

impl FnCx<'_, '_, '_> {
    pub(crate) fn stmt(&mut self, s: StmtId) -> R<()> {
        if self.block.is_none() {
            return Ok(());
        }
        let loc = InferLoc::Stmt(s);
        match self.body.store.stmts[s].clone() {
            Stmt::Missing => return Err(self.error("E1305", "broken statement", loc)),
            Stmt::Empty | Stmt::LocalFunction(_) => {}
            Stmt::Expr(e) => {
                self.expr(e)?;
            }
            Stmt::Local { declarators, .. } => {
                for (b, init) in declarators {
                    if let Some(init) = init {
                        let v = self.value(init)?;
                        self.write_local(b, v)?;
                    }
                }
            }
            Stmt::Block(stmts) => {
                for st in stmts {
                    self.stmt(st)?;
                }
            }
            Stmt::If {
                cond,
                then,
                otherwise,
                ..
            } => self.if_stmt(cond, then, otherwise)?,
            Stmt::While { cond, body } => self.loop_stmt(Some(cond), body, &[], false)?,
            Stmt::Do { body, cond } => self.loop_stmt(Some(cond), body, &[], true)?,
            Stmt::For {
                init,
                cond,
                update,
                body,
            } => {
                for st in init {
                    self.stmt(st)?;
                }
                self.loop_stmt(cond, body, &update, false)?;
            }
            Stmt::Foreach {
                variable,
                iterable,
                body,
            } => self.foreach(s, variable, iterable, body)?,
            Stmt::Switch {
                scrutinee,
                sections,
            } => self.switch_stmt(s, scrutinee, &sections)?,
            Stmt::Break => {
                let target =
                    self.jumps.last().map(|j| j.brk).ok_or_else(|| {
                        self.error("E1305", "`break` outside a loop or switch", loc)
                    })?;
                self.terminate(Term::Br(target));
            }
            Stmt::Continue => {
                let target = self
                    .jumps
                    .iter()
                    .rev()
                    .find_map(|j| j.cont)
                    .ok_or_else(|| self.error("E1305", "`continue` outside a loop", loc))?;
                self.terminate(Term::Br(target));
            }
            Stmt::Return(value) => {
                let values = match value {
                    Some(e) => self.expr(e)?.into_iter().collect(),
                    None => Vec::new(),
                };
                self.ret(values)?;
            }
        }
        Ok(())
    }

    /// Continues in `b` if anything jumps there; otherwise the code after is dead.
    fn continue_in(&mut self, b: Block) {
        self.seal(b);
        let preds = self.ssa.preds.get(&b).map_or(0, Vec::len);
        self.block = if preds == 0 { None } else { Some(b) };
    }

    fn if_stmt(&mut self, cond: ExprId, then: StmtId, otherwise: Option<StmtId>) -> R<()> {
        let c = self.value(cond)?;
        // A constant condition takes one side only (0004 §13.2): this is also what ends
        // compile-time recursion over constant parameters.
        if let Some(bits) = self.const_bits(c) {
            if bits & 1 == 1 {
                self.stmt(then)?;
            } else if let Some(o) = otherwise {
                self.stmt(o)?;
            }
            return Ok(());
        }
        let (tb, merge) = (self.new_block(), self.new_block());
        let eb = match otherwise {
            Some(_) => self.new_block(),
            None => merge,
        };
        self.terminate(Term::CondBr {
            cond: c,
            then_block: tb,
            else_block: eb,
            merge,
        });
        self.seal(tb);
        self.enter(tb);
        self.stmt(then)?;
        if self.block.is_some() {
            self.terminate(Term::Br(merge));
        }
        if let Some(o) = otherwise {
            self.seal(eb);
            self.enter(eb);
            self.stmt(o)?;
            if self.block.is_some() {
                self.terminate(Term::Br(merge));
            }
        }
        self.continue_in(merge);
        Ok(())
    }

    /// `while`, `for` (with `update`), and `do` (`test_last`).
    fn loop_stmt(
        &mut self,
        cond: Option<ExprId>,
        body: StmtId,
        update: &[ExprId],
        test_last: bool,
    ) -> R<()> {
        let (header, cont, merge) = (self.new_block(), self.new_block(), self.new_block());
        self.terminate(Term::Loop {
            header,
            continuing: cont,
            merge,
        });
        self.enter(header);
        let jumps = Jumps {
            brk: merge,
            cont: Some(cont),
        };
        if !test_last && let Some(c) = cond {
            let c = self.value(c)?;
            let start = self.new_block();
            self.terminate(Term::CondBr {
                cond: c,
                then_block: start,
                else_block: merge,
                merge: start,
            });
            self.seal(start);
            self.enter(start);
        }
        self.jumps.push(jumps);
        let r = self.stmt(body);
        self.jumps.pop();
        r?;
        if self.block.is_some() {
            self.terminate(Term::Br(cont));
        }
        self.seal(cont);
        self.enter(cont);
        for &u in update {
            self.expr(u)?;
        }
        match (test_last, cond) {
            (true, Some(c)) => {
                let c = self.value(c)?;
                let latch = self.new_block();
                self.terminate(Term::CondBr {
                    cond: c,
                    then_block: latch,
                    else_block: merge,
                    merge: latch,
                });
                self.seal(latch);
                self.enter(latch);
                self.terminate(Term::Br(header));
            }
            _ => self.terminate(Term::Br(header)),
        }
        self.seal(header);
        self.continue_in(merge);
        Ok(())
    }

    /// `foreach` over a range `a..b` or an array.
    fn foreach(&mut self, s: StmtId, variable: ExprId, iterable: ExprId, body: StmtId) -> R<()> {
        let loc = InferLoc::Stmt(s);
        let binding = match self.body.store.exprs[variable] {
            Expr::DeclVar { binding, .. } => binding,
            _ => {
                return Err(LowerError::unsupported(
                    "this `foreach` variable",
                    self.owner(),
                    loc,
                ));
            }
        };
        // What to count from and to, and how to get each element.
        let (start, end, count_ty, array) = match self.body.store.exprs[iterable].clone() {
            Expr::Range {
                start: Some(a),
                end: Some(b),
            } => {
                let (a, b) = (self.value(a)?, self.value(b)?);
                (a, b, self.binding_ty(binding), None)
            }
            _ => {
                let arr = self.value(iterable)?;
                let len = match self.l.module.data(self.f.ty(arr)).clone() {
                    TypeData::Array(_, n) => n,
                    _ => {
                        return Err(LowerError::unsupported(
                            "`foreach` over this",
                            self.owner(),
                            loc,
                        ));
                    }
                };
                let int = self
                    .l
                    .builtins
                    .item(marlin_shaderc_hir_ty::BuiltinKind::Scalar(
                        marlin_shaderc_hir_ty::Scalar::I32,
                    ))
                    .map_or(Ty::Error, Ty::adt);
                let i32_ty = self.l.ty(&int)?;
                let (zero, n) = (
                    self.int_const(i32_ty, 0),
                    self.int_const(i32_ty, len as i64),
                );
                let t = self.f.ty(arr);
                let var = self.temp_var(t)?;
                self.emit(Op::Store { aligned: None }, vec![var, arr], None)?;
                (zero, n, int, Some(var))
            }
        };
        let t = self.l.ty(&count_ty)?;
        let signed = self.l.num(&count_ty)?.is_some_and(Num::is_signed);
        let counter = self.ssa.new_var(t, None);
        self.write_var(counter, start);

        let (header, cont, merge) = (self.new_block(), self.new_block(), self.new_block());
        self.terminate(Term::Loop {
            header,
            continuing: cont,
            merge,
        });
        self.enter(header);
        let k = self.read_var(counter);
        let cmp = if signed { IntCmp::Slt } else { IntCmp::Ult };
        let more = self.op(Op::Icmp(cmp), vec![k, end], None)?;
        let start_body = self.new_block();
        self.terminate(Term::CondBr {
            cond: more,
            then_block: start_body,
            else_block: merge,
            merge: start_body,
        });
        self.seal(start_body);
        self.enter(start_body);
        let element = match array {
            Some(var) => {
                let p = self.op(Op::Access(vec![AccessStep::Index]), vec![var, k], None)?;
                self.op(Op::Load { aligned: None }, vec![p], None)?
            }
            None => k,
        };
        self.write_local(binding, element)?;
        self.jumps.push(Jumps {
            brk: merge,
            cont: Some(cont),
        });
        let r = self.stmt(body);
        self.jumps.pop();
        r?;
        if self.block.is_some() {
            self.terminate(Term::Br(cont));
        }
        self.seal(cont);
        self.enter(cont);
        let k = self.read_var(counter);
        let one = self.int_const(t, 1);
        let next = self.op(Op::Binary(IrBin::Add), vec![k, one], None)?;
        self.write_var(counter, next);
        self.terminate(Term::Br(header));
        self.seal(header);
        self.continue_in(merge);
        Ok(())
    }
}
