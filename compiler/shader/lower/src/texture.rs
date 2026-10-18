//! Texture calls (0004 §10.4, 0006 §8.6): `t.Sample(s, uv, lod: 0)`, `t.Load(px, layer: l)`,
//! `t[px]`, and a storage texture's `t[px] = v`. The optional arguments are the parameters
//! named `lod`, `bias`, `grad`, `layer`, `mip` and `sample`, passed to the operation only when
//! the call gives them; `grad` is a pair of derivatives.

use marlin_front_hir_def::ItemId;
use marlin_front_hir_def::store::{Arg, ExprId};
use marlin_shaderc_hir_ty::{InferLoc, ItemTypesQuery};
use marlin_shaderc_ir::{Op, TexArg, Value};

use crate::R;
use crate::expr::Receiver;
use crate::func::FnCx;

impl FnCx<'_, '_, '_> {
    /// `tex.sample`, `tex.load` and `tex.texel` (a storage texture's indexer: a load, or with
    /// `store` the value written).
    pub(crate) fn texture_op(
        &mut self,
        e: ExprId,
        name: &str,
        item: ItemId,
        receiver: Option<Receiver>,
        args: &[Arg],
        store: Option<Value>,
    ) -> R<Option<Value>> {
        let loc = InferLoc::Expr(e);
        let r = match receiver {
            Some(Receiver::Place(p)) => self.read_path(&p, e)?,
            Some(Receiver::Value(v)) => v,
            None => return Err(self.error("E1305", "a texture call without a texture", loc)),
        };
        let texture = self.op(Op::DescGet { nonuniform: false }, vec![r], None)?;
        // The given arguments, by parameter name, in parameter order.
        let types = self.l.cx.get::<ItemTypesQuery>(&item)?;
        let slots = self
            .infer
            .call_args
            .get(e)
            .cloned()
            .unwrap_or_else(|| (0..args.len() as u32).map(Some).collect());
        let mut given: Vec<(String, Value)> = Vec::new();
        for (p, slot) in slots.iter().enumerate() {
            let Some(a) = slot else { continue };
            let name = types.params.get(p).and_then(|p| p.name.as_ref());
            let v = self.value(args[*a as usize].expr)?;
            given.push((name.map_or(String::new(), |n| n.to_string()), v));
        }
        let lead = match name {
            "tex.sample" => 2,
            _ => 1,
        };
        if given.len() < lead {
            return Err(self.error("E1305", "a texture call without its coordinate", loc));
        }
        let mut operands = vec![texture];
        for (k, (_, v)) in given.iter().take(lead).enumerate() {
            // The sampler is materialized like the texture.
            let v = if lead == 2 && k == 0 {
                self.op(Op::DescGet { nonuniform: false }, vec![*v], None)?
            } else {
                *v
            };
            operands.push(v);
        }
        if let Some(v) = store {
            operands.push(v);
        }
        let mut tex_args = Vec::new();
        for &arg in TexArg::ALL {
            let Some((_, v)) = given[lead..].iter().find(|(n, _)| n == arg.name()) else {
                continue;
            };
            if arg == TexArg::Grad {
                let dx = self.op(Op::ExtractField(0), vec![*v], None)?;
                let dy = self.op(Op::ExtractField(1), vec![*v], None)?;
                operands.extend([dx, dy]);
            } else {
                operands.push(*v);
            }
            tex_args.push(arg);
        }
        let levels = [TexArg::Lod, TexArg::Bias, TexArg::Grad];
        if levels.iter().filter(|a| tex_args.contains(a)).count() > 1 {
            return Err(self.error("E1309", "`lod`, `bias` and `grad` exclude each other", loc));
        }
        let op = match (name, store) {
            ("tex.sample", _) => Op::TexSample(tex_args),
            (_, Some(_)) => Op::TexStore(tex_args),
            _ => Op::TexLoad(tex_args),
        };
        let inst = self
            .emit(op, operands, None)
            .map_err(|err| err.located(self.owner(), loc))?;
        Ok(self.f.insts[inst].results.first().copied())
    }
}
