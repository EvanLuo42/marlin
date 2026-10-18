//! Places in memory (0006 §6): `[GroupShared]` variables (0004 §12.1) and buffer elements
//! (§10). Locals and parameters are SSA values, read and rebuilt as a whole; memory is shared
//! with other lanes, so `Tile[gi] = x` and `b[i].Field = x` store only the part they name,
//! through a pointer (`access`, `buffer.elem`). Atomic operations are methods of buffers and
//! groupshared arrays that take the element's index (§12.2).

use marlin_front_hir_def::store::{Arg, Expr, ExprId};
use marlin_front_hir_def::{ItemId, ItemKind, ItemTreeQuery, item_path};
use marlin_shaderc_hir_ty::{
    Callee, InferLoc, ItemAttributesQuery, ItemTypesQuery, KnownAttr, Root, Step, Ty, ValuePath,
};
use marlin_shaderc_ir::{
    AccessStep, AtomicOp, GlobalData, GlobalId, MemOrder, Op, Scope, Space, TypeData, Value,
};

use crate::expr::Receiver;
use crate::func::FnCx;
use crate::{LowerError, Lowering, R};

impl Lowering<'_, '_> {
    /// The module variable of `item` if it is a `[GroupShared]` field.
    pub(crate) fn shared_global(&mut self, item: ItemId) -> R<Option<GlobalId>> {
        if let Some(&g) = self.shared.get(&item) {
            return Ok(Some(g));
        }
        let tree = self.cx.get::<ItemTreeQuery>(&item.file)?;
        if tree.items[item.local].kind != ItemKind::Field {
            return Ok(None);
        }
        let attrs = self.cx.get::<ItemAttributesQuery>(&item)?;
        if !attrs.has(KnownAttr::GroupShared) {
            return Ok(None);
        }
        let path = item_path(self.cx, item)?;
        if self.ctfe {
            return Err(LowerError::new(
                "E1306",
                format!("compile-time evaluation cannot use `[GroupShared]` `{path}`"),
            ));
        }
        let types = self.cx.get::<ItemTypesQuery>(&item)?;
        let t = self.ty(types.ty.as_ref().unwrap_or(&Ty::Error))?;
        // The field an amplification entry dispatches is its payload (0004 §11.6).
        let space = if self.payload_field == Some(item) {
            Space::Payload
        } else {
            Space::GroupShared
        };
        let ptr = self.module.ptr(space, false, t);
        let g = self.module.add_global(GlobalData {
            name: path.to_string().into(),
            ty: ptr,
            binding: marlin_shaderc_ir::Binding::None,
            interp: None,
            init: None,
        });
        self.shared.insert(item, g);
        Ok(Some(g))
    }

    pub(crate) fn is_shared(&self, item: ItemId) -> R<bool> {
        let tree = self.cx.get::<ItemTreeQuery>(&item.file)?;
        if tree.items[item.local].kind != ItemKind::Field {
            return Ok(false);
        }
        Ok(self
            .cx
            .get::<ItemAttributesQuery>(&item)?
            .has(KnownAttr::GroupShared))
    }
}

impl FnCx<'_, '_, '_> {
    /// The intrinsic of the indexer an index expression calls, if any.
    fn indexer_intrinsic(&self, e: ExprId) -> R<Option<Box<str>>> {
        Ok(match self.infer.callees.get(e) {
            Some(Callee::Item { item, .. }) => {
                self.l.cx.get::<ItemTypesQuery>(item)?.intrinsic.clone()
            }
            _ => None,
        })
    }

    fn step_in_memory(step: &Step) -> bool {
        match step {
            Step::Field(_) => true,
            Step::Swizzle(lanes) => lanes.len() == 1,
            Step::Property(_) | Step::Length => false,
        }
    }

    /// Whether `e` names a place in memory. Emits nothing.
    pub(crate) fn is_mem_place(&self, e: ExprId) -> R<bool> {
        match &self.body.store.exprs[e] {
            Expr::Path(_) | Expr::Member { .. } if self.infer.paths.get(e).is_some() => {
                let path = &self.infer.paths[e];
                let Root::Item(item) = path.root else {
                    return Ok(false);
                };
                Ok(self.l.is_shared(item)? && path.steps.iter().all(Self::step_in_memory))
            }
            Expr::Member { receiver, .. } => Ok(self
                .infer
                .member_steps
                .get(e)
                .is_some_and(Self::step_in_memory)
                && self.is_mem_place(*receiver)?),
            Expr::Index { base, .. } => match self.indexer_intrinsic(e)? {
                Some(op) => Ok(&*op == "buffer.elem"),
                None if self.infer.callees.get(e).is_some() => Ok(false),
                None => self.is_mem_place(*base),
            },
            _ => Ok(false),
        }
    }

    /// `v.yz` with `v` a vector in memory: `v`'s expression and the lanes. Such a swizzle is
    /// not one place; it is stored lane by lane ([`Self::store_lanes`]).
    pub(crate) fn lanes_in_memory(&self, e: ExprId) -> R<Option<(ExprId, Vec<u8>)>> {
        let Expr::Member { receiver, .. } = &self.body.store.exprs[e] else {
            return Ok(None);
        };
        match self.infer.member_steps.get(e) {
            Some(Step::Swizzle(lanes)) if lanes.len() > 1 && self.is_mem_place(*receiver)? => {
                Ok(Some((*receiver, lanes.clone())))
            }
            _ => Ok(None),
        }
    }

    /// Stores the components of `value` to `lanes` of the vector at `p`, each on its own, so
    /// the other lanes are not written.
    pub(crate) fn store_lanes(&mut self, p: Value, lanes: &[u8], value: Value) -> R<()> {
        let i32_ty = self.l.module.scalar(marlin_shaderc_ir::Scalar::I32);
        for (k, &lane) in lanes.iter().enumerate() {
            let i = self.int_const(i32_ty, lane as i64);
            let lp = self.op(Op::Access(vec![AccessStep::Index]), vec![p, i], None)?;
            let x = self.op(Op::Extract(k as u32), vec![value], None)?;
            self.emit(Op::Store { aligned: None }, vec![lp, x], None)?;
        }
        Ok(())
    }

    /// The pointer to the place in memory `e` names (see [`Self::is_mem_place`]), and the
    /// type there.
    pub(crate) fn mem_place(&mut self, e: ExprId) -> R<(Value, Ty)> {
        let loc = InferLoc::Expr(e);
        match self.body.store.exprs[e].clone() {
            Expr::Path(_) | Expr::Member { .. } if self.infer.paths.get(e).is_some() => {
                let path = self.infer.paths[e].clone();
                self.path_place(&path, e)
            }
            Expr::Member { receiver, .. } => {
                let (p, ty) = self.mem_place(receiver)?;
                let step = self.infer.member_steps[e].clone();
                self.step_place(p, &ty, &step)
            }
            Expr::Index { base, args } => {
                if self.indexer_intrinsic(e)?.is_some() {
                    let b = self.value(base)?;
                    let [Arg { expr: i, .. }] = args.as_slice() else {
                        return Err(self.error(
                            "E1305",
                            "a buffer index that is not one value",
                            loc,
                        ));
                    };
                    let i = self.value(*i)?;
                    let h = self.op(Op::DescGet { nonuniform: false }, vec![b], None)?;
                    let p = self.op(Op::BufferElem, vec![h, i], None)?;
                    return Ok((p, self.expr_ty(e)));
                }
                let (p, bty) = self.mem_place(base)?;
                let idx = self.indices(&bty, &args)?;
                let mut operands = vec![p];
                operands.extend(&idx);
                let p = self.op(
                    Op::Access(vec![AccessStep::Index; idx.len()]),
                    operands,
                    None,
                )?;
                Ok((p, self.expr_ty(e)))
            }
            _ => Err(self.error("E1305", "not a place in memory", loc)),
        }
    }

    /// The pointer for a path whose root is a `[GroupShared]` variable.
    pub(crate) fn path_place(&mut self, path: &ValuePath, e: ExprId) -> R<(Value, Ty)> {
        let Root::Item(item) = path.root else {
            return Err(self.error("E1305", "not a place in memory", InferLoc::Expr(e)));
        };
        let (mut p, mut ty) = self.shared_root(item, e)?;
        for step in &path.steps {
            (p, ty) = self.step_place(p, &ty, step)?;
        }
        Ok((p, ty))
    }

    /// The pointer to a `[GroupShared]` variable, and its type.
    pub(crate) fn shared_root(&mut self, item: ItemId, e: ExprId) -> R<(Value, Ty)> {
        let g = self
            .l
            .shared_global(item)
            .map_err(|err| err.located(self.owner(), InferLoc::Expr(e)))?
            .ok_or_else(|| {
                self.error("E1305", "not a `[GroupShared]` variable", InferLoc::Expr(e))
            })?;
        let types = self.l.cx.get::<ItemTypesQuery>(&item)?;
        let ty = self.concrete(types.ty.as_ref().unwrap_or(&Ty::Error));
        let t = self.l.module.globals[g].ty;
        Ok((self.f.global_value(g, t), ty))
    }

    fn step_place(&mut self, p: Value, ty: &Ty, step: &Step) -> R<(Value, Ty)> {
        match step {
            Step::Field(f) => {
                let i = self.l.field_index(ty, *f)?;
                let fty = self.field_ty(ty, *f)?;
                Ok((
                    self.op(Op::Access(vec![AccessStep::Field(i)]), vec![p], None)?,
                    fty,
                ))
            }
            Step::Swizzle(lanes) => {
                let i32_ty = self.l.module.scalar(marlin_shaderc_ir::Scalar::I32);
                let i = self.int_const(i32_ty, lanes[0] as i64);
                let element = match ty {
                    Ty::Adt { args, .. } => match args.first() {
                        Some(marlin_shaderc_hir_ty::GenericArgTy::Type(t)) => t.clone(),
                        _ => Ty::Error,
                    },
                    _ => Ty::Error,
                };
                Ok((
                    self.op(Op::Access(vec![AccessStep::Index]), vec![p, i], None)?,
                    element,
                ))
            }
            Step::Property(_) | Step::Length => {
                Err(LowerError::new("E1305", "not a place in memory"))
            }
        }
    }

    /// `b.AtomicAdd(i, v)` and the like: `atomic.<op>` on the element `i` of a buffer or a
    /// `[GroupShared]` array.
    pub(crate) fn atomic(
        &mut self,
        e: ExprId,
        op: &str,
        receiver: Option<Receiver>,
        mut operands: Vec<Value>,
    ) -> R<Option<Value>> {
        let loc = InferLoc::Expr(e);
        let atomic = AtomicOp::from_name(op).ok_or_else(|| {
            LowerError::unsupported(
                format!("intrinsic `atomic.{op}`"),
                self.owner(),
                loc.clone(),
            )
        })?;
        if operands.is_empty() {
            return Err(self.error("E1305", "an atomic operation without an index", loc));
        }
        let index = operands.remove(0);
        let (ptr, scope) = match receiver {
            Some(Receiver::Place(path)) if matches!(path.root, Root::Item(item) if self.l.is_shared(item)?) =>
            {
                let (p, _) = self.path_place(&path, e)?;
                let p = self.op(Op::Access(vec![AccessStep::Index]), vec![p, index], None)?;
                (p, Scope::Group)
            }
            Some(r) => {
                let b = match r {
                    Receiver::Place(path) => self.read_path(&path, e)?,
                    Receiver::Value(v) => v,
                };
                if !matches!(self.l.module.data(self.f.ty(b)), TypeData::Res(_)) {
                    return Err(LowerError::unsupported(
                        "an atomic operation on this",
                        self.owner(),
                        loc,
                    ));
                }
                let h = self.op(Op::DescGet { nonuniform: false }, vec![b], None)?;
                (
                    self.op(Op::BufferElem, vec![h, index], None)?,
                    Scope::Device,
                )
            }
            None => return Err(self.error("E1305", "an atomic operation without a receiver", loc)),
        };
        let mut all = vec![ptr];
        all.extend(operands);
        let inst = self.emit(
            Op::Atomic {
                op: atomic,
                scope,
                order: MemOrder::Relaxed,
            },
            all,
            None,
        )?;
        Ok(self.f.insts[inst].results.first().copied())
    }
}
