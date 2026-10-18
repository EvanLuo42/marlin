//! Barriers the whole group reaches (0004 §12.2; decided at step 8c): a `barrier` that waits
//! for the thread group must not be in control flow that only some of the group's threads may
//! run, which D3D12 and Vulkan leave undefined. It runs on entry points after inlining, since a
//! helper's barrier is wrong only where it is called. `[Uniform]` (`assume.uniform`) vouches
//! for a value the analysis cannot prove the same.
//!
//! Derivatives in control flow that differs within a quad are allowed, as in HLSL.

use marlin_shaderc_ir::{Module, Op, ValueDef};

use crate::uniformity::{Level, analyze};
use crate::{Analyses, Pass, Problem};

pub struct CheckBarriers;

impl Pass for CheckBarriers {
    fn name(&self) -> &'static str {
        "check-barriers"
    }

    fn run(&self, _: &mut Module, _: &mut Analyses) -> bool {
        false
    }

    fn problems(&self, m: &Module) -> Vec<Problem> {
        let mut out = Vec::new();
        for entry in &m.entries {
            let f = &m.funcs[entry.func];
            let u = analyze(m, f, Level::Group);
            for &b in &f.layout {
                for i in f.insts(b) {
                    if !matches!(f.insts[i].op, Op::Barrier { exec: Some(_), .. }) {
                        continue;
                    }
                    let Some(&branch) = u.divergent_blocks.get(&b) else {
                        continue;
                    };
                    let cond = f.operands(branch)[0];
                    let at = match f.values[cond].def {
                        ValueDef::Result(d, _) => f.insts[d].loc.or(f.insts[branch].loc),
                        _ => f.insts[branch].loc,
                    };
                    out.push(Problem {
                        code: "E1401",
                        message: "this barrier waits for the whole thread group, but only some of \
                                  its threads may get here"
                            .into(),
                        at: f.insts[i].loc,
                        because: Some((
                            "the threads may part here: this may differ between them \
                             (`[Uniform]` asserts it does not)"
                                .into(),
                            at,
                        )),
                    });
                }
            }
        }
        out
    }
}
