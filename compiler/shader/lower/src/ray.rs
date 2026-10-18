//! Inline ray queries (0004 §12.4, 0006 §8.9). A `RayQuery` value is a pointer to a `rayquery`
//! variable of its own, which its constructor makes and initializes; its methods are `rq.*`
//! operations on it, and `Committed` and `Candidate` read every field of a `RayHit`.

use marlin_front_hir_def::ItemId;
use marlin_front_hir_def::store::ExprId;
use marlin_shaderc_hir_ty::{InferLoc, Subst};
use marlin_shaderc_ir::{Op, RqField, TypeData, Value};

use crate::R;
use crate::func::FnCx;

impl FnCx<'_, '_, '_> {
    /// `rq.init` (the constructor: the acceleration structure, the ray, the flags, the mask),
    /// `rq.proceed`, `rq.commit_triangle`, `rq.committed` and `rq.candidate` (on the query).
    pub(crate) fn ray_query(
        &mut self,
        e: ExprId,
        name: &str,
        item: ItemId,
        subst: &Subst,
        operands: Vec<Value>,
    ) -> R<Option<Value>> {
        let loc = InferLoc::Expr(e);
        let want = if name == "rq.init" { 4 } else { 1 };
        if operands.len() != want {
            return Err(self.error("E1305", "a ray query operation's operands", loc));
        }
        match name {
            "rq.init" => {
                let rq = self.l.module.ty(TypeData::RayQuery);
                let q = self.temp_var(rq)?;
                let accel = self.op(Op::DescGet { nonuniform: false }, vec![operands[0]], None)?;
                // `Ray { Origin, TMin, Direction, TMax }`.
                let mut ops = vec![q, accel];
                for k in 0..4 {
                    ops.push(self.op(Op::ExtractField(k), vec![operands[1]], None)?);
                }
                ops.extend([operands[2], operands[3]]);
                self.emit(Op::RqInit, ops, None)?;
                Ok(Some(q))
            }
            "rq.proceed" => self.op(Op::RqProceed, operands, None).map(Some),
            "rq.commit_triangle" => {
                self.emit(Op::RqCommitTriangle, operands, None)?;
                Ok(None)
            }
            "rq.committed" | "rq.candidate" => {
                use RqField::*;
                let committed = name == "rq.committed";
                // `RayHit`'s fields, in order.
                let mut parts = Vec::new();
                for field in [
                    Kind,
                    T,
                    Barycentrics,
                    InstanceIndex,
                    InstanceId,
                    PrimitiveIndex,
                    GeometryIndex,
                    FrontFace,
                ] {
                    parts.push(self.op(Op::RqGet { committed, field }, vec![operands[0]], None)?);
                }
                let ty = self.member_result_ty(item, subst)?;
                let t = self.l.ty(&ty)?;
                self.op(Op::Construct, parts, Some(t)).map(Some)
            }
            _ => Err(crate::LowerError::unsupported(
                format!("intrinsic `{name}`"),
                self.owner(),
                loc,
            )),
        }
    }
}
