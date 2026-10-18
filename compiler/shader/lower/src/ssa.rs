//! SSA construction as in Braun et al., "Simple and Efficient Construction of Static Single
//! Assignment Form" (2013), with block parameters for phis: a variable read in a block with
//! several predecessors becomes a parameter of that block, and each predecessor's jump passes
//! its own definition. Jumps get their arguments when the function is finished, after every
//! block is sealed.

use std::collections::{HashMap, HashSet};

use marlin_shaderc_ir::{Block, ConstValue, Function, Module, Type, Value};

pub(crate) type Var = u32;

#[derive(Default)]
pub(crate) struct Ssa {
    vars: Vec<(Type, Option<Box<str>>)>,
    defs: HashMap<(Var, Block), Value>,
    sealed: HashSet<Block>,
    incomplete: HashMap<Block, Vec<(Var, Value)>>,
    pub(crate) preds: HashMap<Block, Vec<Block>>,
    /// For each block parameter made for a variable: what each predecessor passes.
    pub(crate) phi_args: HashMap<Value, HashMap<Block, Value>>,
}

impl Ssa {
    pub(crate) fn new_var(&mut self, ty: Type, name: Option<&str>) -> Var {
        self.vars.push((ty, name.map(Into::into)));
        (self.vars.len() - 1) as Var
    }

    pub(crate) fn write(&mut self, var: Var, block: Block, value: Value) {
        self.defs.insert((var, block), value);
    }

    pub(crate) fn add_pred(&mut self, block: Block, pred: Block) {
        self.preds.entry(block).or_default().push(pred);
    }

    /// The value of `var` at the end of what `block` has so far.
    pub(crate) fn read(
        &mut self,
        f: &mut Function,
        m: &mut Module,
        var: Var,
        block: Block,
    ) -> Value {
        if let Some(&v) = self.defs.get(&(var, block)) {
            return v;
        }
        self.read_recursive(f, m, var, block)
    }

    fn read_recursive(
        &mut self,
        f: &mut Function,
        m: &mut Module,
        var: Var,
        block: Block,
    ) -> Value {
        let (ty, name) = self.vars[var as usize].clone();
        let preds = self.preds.get(&block).cloned().unwrap_or_default();
        let value = if !self.sealed.contains(&block) {
            let p = f.add_param(block, ty, name.as_deref());
            self.incomplete.entry(block).or_default().push((var, p));
            p
        } else if preds.len() == 1 {
            self.read(f, m, var, preds[0])
        } else if preds.is_empty() {
            // Read where it was never written (the entry, or code nothing reaches).
            let c = m.constant(ty, ConstValue::Any);
            f.const_value(c, ty)
        } else {
            let p = f.add_param(block, ty, name.as_deref());
            self.write(var, block, p);
            self.add_operands(f, m, var, block, p);
            p
        };
        self.write(var, block, value);
        value
    }

    fn add_operands(
        &mut self,
        f: &mut Function,
        m: &mut Module,
        var: Var,
        block: Block,
        param: Value,
    ) {
        let preds = self.preds.get(&block).cloned().unwrap_or_default();
        for pred in preds {
            let v = self.read(f, m, var, pred);
            self.phi_args.entry(param).or_default().insert(pred, v);
        }
    }

    /// No more predecessors will be added to `block`.
    pub(crate) fn seal(&mut self, f: &mut Function, m: &mut Module, block: Block) {
        if !self.sealed.insert(block) {
            return;
        }
        for (var, p) in self.incomplete.remove(&block).unwrap_or_default() {
            self.add_operands(f, m, var, block, p);
        }
    }
}
