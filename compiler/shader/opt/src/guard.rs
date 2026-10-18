//! Integer division by zero is 0 (0004 §9.6, 0005 §4.4 pass 0.2): SPIR-V leaves it undefined
//! and D3D12 gives all ones. Each `sdiv`, `udiv`, `srem` and `urem` whose divisor is not a
//! known safe constant divides by 1 instead where the divisor is 0 (and, signed, where
//! `MIN / -1` would overflow: `MIN / 1` and `MIN % 1` are the wrapped results), then picks 0
//! where it was 0. Per component, for vectors. It runs before optimization, so value ranges
//! and constant folding can take the checks away again.

use marlin_shaderc_ir::{BinaryOp, FuncId, Function, IntCmp, Module, Op, Val, ValueDef};

use crate::edit::{constant, value_before};
use crate::{Analyses, Changed, FunctionPass};

pub struct DivGuard;

impl FunctionPass for DivGuard {
    fn name(&self) -> &'static str {
        "div-guard"
    }

    fn run_function(
        &self,
        m: &mut Module,
        _: FuncId,
        f: &mut Function,
        _: &mut Analyses,
    ) -> Changed {
        let mut changed = Changed::No;
        let insts: Vec<_> = f
            .layout
            .iter()
            .flat_map(|&b| f.insts(b).collect::<Vec<_>>())
            .collect();
        for inst in insts {
            let Op::Binary(
                op @ (BinaryOp::SDiv | BinaryOp::UDiv | BinaryOp::SRem | BinaryOp::URem),
            ) = f.insts[inst].op
            else {
                continue;
            };
            let signed = matches!(op, BinaryOp::SDiv | BinaryOp::SRem);
            let (n, d) = (f.operands(inst)[0], f.operands(inst)[1]);
            let t = f.ty(d);
            let Some(s) = m.element_scalar(t) else {
                continue;
            };
            let width = m.width(t).unwrap_or(1) as usize;
            let mask = if s.bits() >= 64 {
                u64::MAX
            } else {
                (1u64 << s.bits()) - 1
            };
            // A constant divisor that is neither 0 nor (signed) -1 needs nothing.
            if let ValueDef::Const(c) = f.values[d].def
                && let Ok(v) = marlin_shaderc_ir::constant_value(m, c)
            {
                let parts = match &v {
                    Val::Agg(p) => p.clone(),
                    one => vec![one.clone()],
                };
                let safe = parts.iter().all(|p| {
                    p.bits()
                        .is_some_and(|b| b & mask != 0 && !(signed && b & mask == mask))
                });
                if safe {
                    continue;
                }
            }
            let splat = |bits: u64| {
                if width == 1 {
                    Val::Bits(bits & mask)
                } else {
                    Val::Agg(vec![Val::Bits(bits & mask); width])
                }
            };
            let zero = constant(m, f, &splat(0), t);
            let one = constant(m, f, &splat(1), t);
            let is_zero = value_before(m, f, inst, Op::Icmp(IntCmp::Eq), vec![d, zero], None);
            let bad = if signed {
                let min = constant(m, f, &splat(1 << (s.bits() - 1)), t);
                let minus_one = constant(m, f, &splat(u64::MAX), t);
                let is_min = value_before(m, f, inst, Op::Icmp(IntCmp::Eq), vec![n, min], None);
                let is_m1 =
                    value_before(m, f, inst, Op::Icmp(IntCmp::Eq), vec![d, minus_one], None);
                let overflow = value_before(
                    m,
                    f,
                    inst,
                    Op::Binary(BinaryOp::And),
                    vec![is_min, is_m1],
                    None,
                );
                value_before(
                    m,
                    f,
                    inst,
                    Op::Binary(BinaryOp::Or),
                    vec![is_zero, overflow],
                    None,
                )
            } else {
                is_zero
            };
            let safe_d = value_before(m, f, inst, Op::Select, vec![bad, one, d], None);
            let q = value_before(m, f, inst, Op::Binary(op), vec![n, safe_d], None);
            let r = value_before(m, f, inst, Op::Select, vec![is_zero, zero, q], None);
            let old = f.result(inst);
            f.replace_all_uses(old, r);
            f.remove_inst(inst);
            changed = Changed::Values;
        }
        changed
    }
}
