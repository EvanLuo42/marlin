//! `Debug.*` in Shipping builds (0005 §4.4 pass 0.4): `debug.print` and `debug.assert` go, with
//! the module's format strings. Development builds keep them for each backend to lower
//! (SPIR-V's `NonSemantic.DebugPrintf`; a debug buffer where there is nothing like it).

use marlin_shaderc_ir::{Module, Op};

use crate::{Analyses, Pass};

pub struct StripDebug;

impl Pass for StripDebug {
    fn name(&self) -> &'static str {
        "strip-debug"
    }

    fn run(&self, m: &mut Module, _: &mut Analyses) -> bool {
        let mut changed = !m.formats.is_empty();
        for (_, f) in m.funcs.iter_mut() {
            let debug: Vec<_> = f
                .layout
                .iter()
                .flat_map(|&b| f.insts(b).collect::<Vec<_>>())
                .filter(|&i| matches!(f.insts[i].op, Op::DebugPrint(_) | Op::DebugAssert(_)))
                .collect();
            changed |= !debug.is_empty();
            for inst in debug {
                f.remove_inst(inst);
            }
        }
        m.formats.clear();
        changed
    }
}
