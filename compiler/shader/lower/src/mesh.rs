//! Mesh outputs and `Mesh.Dispatch` (0004 §11.6, 0006 §8.9). A `MeshOutput` value and its
//! lists (`o.Vertices`, `o.Triangles`, ...) are constants that name what they write; a write
//! `o.Vertices[i] = v` stores each field of `v` at index `i` of that field's output array.

use marlin_front_hir_def::store::{Arg, ExprId};
use marlin_front_hir_def::{ItemId, ItemTreeQuery};
use marlin_shaderc_hir_ty::{InferLoc, Subst};
use marlin_shaderc_ir::{AccessStep, Op, Value};

use crate::R;
use crate::entry::{
    MESH_LINES, MESH_POINTS, MESH_PRIMITIVES, MESH_TRIANGLES, MESH_VERTICES, MeshTopology,
};
use crate::func::FnCx;

impl FnCx<'_, '_, '_> {
    /// `mesh.set_counts`, `mesh.dispatch`, and the lists of a `MeshOutput`.
    pub(crate) fn mesh_op(
        &mut self,
        e: ExprId,
        name: &str,
        item: ItemId,
        subst: &Subst,
        args: &[Arg],
    ) -> R<Option<Value>> {
        let loc = InferLoc::Expr(e);
        let list = match name {
            "mesh.vertices" => Some(MESH_VERTICES),
            "mesh.primitives" => Some(MESH_PRIMITIVES),
            "mesh.indices" => {
                let tree = self.l.cx.get::<ItemTreeQuery>(&item.file)?;
                Some(
                    match tree.items[item.local].name.as_ref().map(|n| n.as_str()) {
                        Some("Lines") => MESH_LINES,
                        Some("Points") => MESH_POINTS,
                        _ => MESH_TRIANGLES,
                    },
                )
            }
            _ => None,
        };
        if let Some(list) = list {
            let ty = self.member_result_ty(item, subst)?;
            let t = self.l.ty(&ty)?;
            return Ok(Some(self.int_const(t, list)));
        }
        let (op, n) = match name {
            "mesh.set_counts" => (Op::MeshSetCounts, 2),
            _ => (Op::MeshDispatch, 3),
        };
        let mut operands = Vec::new();
        for a in args.iter().take(n) {
            operands.push(self.value(a.expr)?);
        }
        if operands.len() != n {
            return Err(self.error("E1305", "a mesh operation without its counts", loc));
        }
        self.emit(op, operands, None)?;
        Ok(None)
    }

    /// `o.Vertices[i] = v` and the like: `list` is the value of `o.Vertices`.
    pub(crate) fn mesh_write(
        &mut self,
        e: ExprId,
        list: Value,
        args: &[Arg],
        value: Value,
    ) -> R<()> {
        let loc = InferLoc::Expr(e);
        let Some(outputs) = self.l.mesh.clone() else {
            return Err(self.error("E1308", "mesh outputs are written by a mesh entry", loc));
        };
        let Some(kind) = self.const_bits(list) else {
            return Err(self.error(
                "E1308",
                "write to a mesh output list directly (`o.Vertices[i] = v`)",
                loc,
            ));
        };
        let [Arg { expr: index, .. }] = args else {
            return Err(self.error("E1305", "a mesh output index", loc));
        };
        let i = self.value(*index)?;
        let at = |cx: &mut Self, g, v: Value| -> R<()> {
            let t = cx.l.module.globals[g].ty;
            let base = cx.f.global_value(g, t);
            let p = cx.op(Op::Access(vec![AccessStep::Index]), vec![base, i], None)?;
            cx.emit(Op::Store { aligned: None }, vec![p, v], None)?;
            Ok(())
        };
        let fields = match kind as i64 {
            MESH_VERTICES => outputs.vertices.clone(),
            MESH_PRIMITIVES => outputs.primitives.clone(),
            k => {
                let want = match outputs.topology {
                    MeshTopology::Triangle => MESH_TRIANGLES,
                    MeshTopology::Line => MESH_LINES,
                    MeshTopology::Point => MESH_POINTS,
                };
                if k != want {
                    let name = match outputs.topology {
                        MeshTopology::Triangle => "Triangles",
                        MeshTopology::Line => "Lines",
                        MeshTopology::Point => "Points",
                    };
                    return Err(self.error(
                        "E1308",
                        format!("this mesh entry's topology writes `{name}`"),
                        loc,
                    ));
                }
                return at(self, outputs.indices, value);
            }
        };
        for (k, g) in fields.into_iter().enumerate() {
            let part = self.op(Op::ExtractField(k as u32), vec![value], None)?;
            at(self, g, part)?;
        }
        Ok(())
    }
}
