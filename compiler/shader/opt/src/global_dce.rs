//! Dead functions and module variables (0005 §4.4 pass 1.8). In a module with entry points,
//! what no entry reaches through calls goes; a module without entry points (a library of
//! functions, as tests build) keeps every function. Module variables that no kept function
//! uses go either way.

use std::collections::HashSet;

use marlin_shaderc_ir::{FuncId, GlobalId, Module, Op, ValueDef};

use crate::{Analyses, Pass};

pub struct GlobalDce;

impl Pass for GlobalDce {
    fn name(&self) -> &'static str {
        "global-dce"
    }

    fn run(&self, m: &mut Module, _: &mut Analyses) -> bool {
        let mut keep: HashSet<FuncId> = HashSet::new();
        if m.entries.is_empty() {
            keep.extend(m.funcs.iter().map(|(id, _)| id));
        } else {
            let mut work: Vec<FuncId> = m.entries.iter().map(|e| e.func).collect();
            while let Some(id) = work.pop() {
                if !keep.insert(id) {
                    continue;
                }
                let f = &m.funcs[id];
                for &b in &f.layout {
                    for i in f.insts(b) {
                        if let Op::Call(c) = f.insts[i].op {
                            work.push(c);
                        }
                    }
                }
            }
        }
        let mut used: HashSet<GlobalId> = HashSet::new();
        for &id in &keep {
            let f = &m.funcs[id];
            for (v, data) in f.values.iter() {
                if let ValueDef::Global(g) = data.def
                    && !f.uses(v).is_empty()
                {
                    used.insert(g);
                }
            }
        }
        let changed = keep.len() < m.funcs.len() || used.len() < m.globals.len();
        if changed {
            m.retain(|f| keep.contains(&f), |g| used.contains(&g));
        }
        changed
    }
}
