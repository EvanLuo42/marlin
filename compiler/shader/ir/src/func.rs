//! A function (0006 §4, §12): blocks, instructions and values in arenas of their own, `u32`
//! indices between them, the instructions of a block in a doubly linked list, and a use list
//! per value. The function's parameters are the entry block's parameters.

use std::collections::HashMap;

use marlin_lang_arena::{Arena, Idx};

use crate::module::{Const, GlobalId};
use crate::op::Op;
use crate::ty::Type;

pub type Block = Idx<BlockData>;
pub type Inst = Idx<InstData>;
pub type Value = Idx<ValueData>;
pub type FuncId = Idx<Function>;

#[derive(Clone, Debug, Default)]
pub struct BlockData {
    pub params: Vec<Value>,
    first: Option<Inst>,
    last: Option<Inst>,
}

#[derive(Clone, Debug)]
pub struct InstData {
    pub op: Op,
    operands: Vec<Value>,
    pub results: Vec<Value>,
    /// `None` while the instruction is not in a block.
    block: Option<Block>,
    prev: Option<Inst>,
    next: Option<Inst>,
    /// Floating-point operations: no reassociation, contraction or approximation (0006 §8.1).
    pub precise: bool,
    /// An index into the module's source locations, once there are any.
    pub loc: Option<u32>,
}

/// Where a value comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ValueDef {
    Result(Inst, u32),
    Param(Block, u32),
    Const(Const),
    /// A module-level variable's address.
    Global(GlobalId),
}

/// One operand slot that reads a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Use {
    pub inst: Inst,
    pub index: u32,
}

#[derive(Clone, Debug)]
pub struct ValueData {
    pub ty: Type,
    pub def: ValueDef,
    /// The source name, a hint for the text format only.
    pub name: Option<Box<str>>,
    uses: Vec<Use>,
}

#[derive(Clone, Debug)]
pub struct Function {
    /// The full name (`Marlin.Post.Blur`).
    pub name: Box<str>,
    pub results: Vec<Type>,
    pub blocks: Arena<BlockData>,
    pub insts: Arena<InstData>,
    pub values: Arena<ValueData>,
    /// The blocks in order; the first is the entry block.
    pub layout: Vec<Block>,
    consts: HashMap<Const, Value>,
    globals: HashMap<GlobalId, Value>,
}

impl Function {
    pub fn new(name: &str, results: Vec<Type>) -> Self {
        Self {
            name: name.into(),
            results,
            blocks: Arena::new(),
            insts: Arena::new(),
            values: Arena::new(),
            layout: Vec::new(),
            consts: HashMap::new(),
            globals: HashMap::new(),
        }
    }

    // ---- blocks ----

    /// A new block at the end of the layout.
    pub fn add_block(&mut self) -> Block {
        let b = self.blocks.alloc(BlockData::default());
        self.layout.push(b);
        b
    }

    pub fn entry(&self) -> Option<Block> {
        self.layout.first().copied()
    }

    /// The function's parameters: the entry block's.
    pub fn params(&self) -> &[Value] {
        match self.entry() {
            Some(b) => &self.blocks[b].params,
            None => &[],
        }
    }

    pub fn add_param(&mut self, block: Block, ty: Type, name: Option<&str>) -> Value {
        let index = self.blocks[block].params.len() as u32;
        let v = self.values.alloc(ValueData {
            ty,
            def: ValueDef::Param(block, index),
            name: name.map(Into::into),
            uses: Vec::new(),
        });
        self.blocks[block].params.push(v);
        v
    }

    /// The instructions of `block`, in order.
    pub fn insts(&self, block: Block) -> InstIter<'_> {
        InstIter {
            func: self,
            next: self.blocks[block].first,
        }
    }

    pub fn first_inst(&self, block: Block) -> Option<Inst> {
        self.blocks[block].first
    }

    pub fn last_inst(&self, block: Block) -> Option<Inst> {
        self.blocks[block].last
    }

    /// The block's last instruction if it is a terminator.
    pub fn terminator(&self, block: Block) -> Option<Inst> {
        self.blocks[block]
            .last
            .filter(|&i| self.insts[i].op.is_terminator())
    }

    // ---- values ----

    /// The value standing for module constant `c` in this function.
    pub fn const_value(&mut self, c: Const, ty: Type) -> Value {
        if let Some(&v) = self.consts.get(&c) {
            return v;
        }
        let v = self.values.alloc(ValueData {
            ty,
            def: ValueDef::Const(c),
            name: None,
            uses: Vec::new(),
        });
        self.consts.insert(c, v);
        v
    }

    /// Renumbers called functions and module variables (see [`crate::Module::retain`]).
    pub(crate) fn remap(
        &mut self,
        funcs: &HashMap<FuncId, FuncId>,
        globals: &HashMap<GlobalId, GlobalId>,
    ) {
        for (_, inst) in self.insts.iter_mut() {
            if let Op::Call(c) = &mut inst.op
                && let Some(&n) = funcs.get(c)
            {
                *c = n;
            }
        }
        // A removed variable's address value stays, unused, naming nothing.
        let old = std::mem::take(&mut self.globals);
        for (g, v) in old {
            if let Some(&n) = globals.get(&g) {
                self.values[v].def = ValueDef::Global(n);
                self.globals.insert(n, v);
            } else {
                debug_assert!(
                    self.values[v].uses.is_empty(),
                    "a removed module variable is used"
                );
            }
        }
    }

    /// The value standing for the address of module variable `g` in this function.
    pub fn global_value(&mut self, g: GlobalId, ty: Type) -> Value {
        if let Some(&v) = self.globals.get(&g) {
            return v;
        }
        let v = self.values.alloc(ValueData {
            ty,
            def: ValueDef::Global(g),
            name: None,
            uses: Vec::new(),
        });
        self.globals.insert(g, v);
        v
    }

    pub fn ty(&self, v: Value) -> Type {
        self.values[v].ty
    }

    pub fn uses(&self, v: Value) -> &[Use] {
        &self.values[v].uses
    }

    pub fn set_name(&mut self, v: Value, name: Option<&str>) {
        self.values[v].name = name.map(Into::into);
    }

    // ---- instructions ----

    /// A new instruction, not yet in a block, with one result per type.
    pub fn create_inst(&mut self, op: Op, operands: Vec<Value>, results: &[Type]) -> Inst {
        let inst = self.insts.alloc(InstData {
            op,
            operands: Vec::new(),
            results: Vec::new(),
            block: None,
            prev: None,
            next: None,
            precise: false,
            loc: None,
        });
        for (i, &ty) in results.iter().enumerate() {
            let v = self.values.alloc(ValueData {
                ty,
                def: ValueDef::Result(inst, i as u32),
                name: None,
                uses: Vec::new(),
            });
            self.insts[inst].results.push(v);
        }
        for (i, &v) in operands.iter().enumerate() {
            self.values[v].uses.push(Use {
                inst,
                index: i as u32,
            });
        }
        self.insts[inst].operands = operands;
        inst
    }

    pub fn operands(&self, inst: Inst) -> &[Value] {
        &self.insts[inst].operands
    }

    pub fn block_of(&self, inst: Inst) -> Option<Block> {
        self.insts[inst].block
    }

    pub fn next_inst(&self, inst: Inst) -> Option<Inst> {
        self.insts[inst].next
    }

    pub fn prev_inst(&self, inst: Inst) -> Option<Inst> {
        self.insts[inst].prev
    }

    /// The single result of `inst`.
    pub fn result(&self, inst: Inst) -> Value {
        self.insts[inst].results[0]
    }

    pub fn set_operand(&mut self, inst: Inst, index: usize, value: Value) {
        let old = self.insts[inst].operands[index];
        let u = Use {
            inst,
            index: index as u32,
        };
        self.values[old].uses.retain(|x| *x != u);
        self.values[value].uses.push(u);
        self.insts[inst].operands[index] = value;
    }

    /// Makes every use of `old` read `new` instead.
    pub fn replace_all_uses(&mut self, old: Value, new: Value) {
        if old == new {
            return;
        }
        let uses = std::mem::take(&mut self.values[old].uses);
        for u in &uses {
            self.insts[u.inst].operands[u.index as usize] = new;
        }
        self.values[new].uses.extend(uses);
    }

    pub fn append_inst(&mut self, block: Block, inst: Inst) {
        debug_assert!(
            self.insts[inst].block.is_none(),
            "instruction already placed"
        );
        let last = self.blocks[block].last;
        let data = &mut self.insts[inst];
        data.block = Some(block);
        data.prev = last;
        data.next = None;
        match last {
            Some(l) => self.insts[l].next = Some(inst),
            None => self.blocks[block].first = Some(inst),
        }
        self.blocks[block].last = Some(inst);
    }

    /// Places `inst` right before `before`, in its block.
    pub fn insert_before(&mut self, before: Inst, inst: Inst) {
        debug_assert!(
            self.insts[inst].block.is_none(),
            "instruction already placed"
        );
        let block = self.insts[before]
            .block
            .expect("insert before a placed instruction");
        let prev = self.insts[before].prev;
        let data = &mut self.insts[inst];
        data.block = Some(block);
        data.prev = prev;
        data.next = Some(before);
        self.insts[before].prev = Some(inst);
        match prev {
            Some(p) => self.insts[p].next = Some(inst),
            None => self.blocks[block].first = Some(inst),
        }
    }

    /// Takes `inst` out of its block; it keeps its operands.
    pub fn detach(&mut self, inst: Inst) {
        let Some(block) = self.insts[inst].block else {
            return;
        };
        let (prev, next) = (self.insts[inst].prev, self.insts[inst].next);
        match prev {
            Some(p) => self.insts[p].next = next,
            None => self.blocks[block].first = next,
        }
        match next {
            Some(n) => self.insts[n].prev = prev,
            None => self.blocks[block].last = prev,
        }
        let data = &mut self.insts[inst];
        data.block = None;
        data.prev = None;
        data.next = None;
    }

    /// Deletes `inst`: out of its block, and its operands no longer use their values. Its
    /// results must be unused.
    pub fn remove_inst(&mut self, inst: Inst) {
        debug_assert!(
            self.insts[inst]
                .results
                .iter()
                .all(|&r| self.values[r].uses.is_empty()),
            "removing an instruction whose results are used"
        );
        self.detach(inst);
        self.drop_operands(inst);
    }

    fn drop_operands(&mut self, inst: Inst) {
        let operands = std::mem::take(&mut self.insts[inst].operands);
        for (i, v) in operands.into_iter().enumerate() {
            self.values[v]
                .uses
                .retain(|u| !(u.inst == inst && u.index == i as u32));
        }
    }

    /// Deletes several instructions at once: their results may be used only by each other.
    pub fn remove_insts(&mut self, insts: &[Inst]) {
        for &inst in insts {
            self.detach(inst);
            self.drop_operands(inst);
        }
        debug_assert!(
            insts.iter().all(|&i| self.insts[i]
                .results
                .iter()
                .all(|&r| self.values[r].uses.is_empty())),
            "removing instructions whose results are used elsewhere"
        );
    }

    /// Deletes every instruction of `block`.
    pub fn clear_block(&mut self, block: Block) {
        let insts: Vec<Inst> = self.insts(block).collect();
        self.remove_insts(&insts);
    }

    /// Replaces `inst` (which has no results) by a new instruction in its place.
    pub fn replace_inst(&mut self, inst: Inst, op: Op, operands: Vec<Value>) -> Inst {
        debug_assert!(
            self.insts[inst].results.is_empty(),
            "replacing an instruction with results"
        );
        let new = self.create_inst(op, operands, &[]);
        self.insts[new].loc = self.insts[inst].loc;
        self.insert_before(inst, new);
        self.remove_inst(inst);
        new
    }

    /// Removes parameter `index` of `block`, which must be unused; later parameters move down.
    /// Jumps to the block must stop passing it.
    pub fn remove_param(&mut self, block: Block, index: usize) {
        let v = self.blocks[block].params.remove(index);
        debug_assert!(self.values[v].uses.is_empty(), "removing a used parameter");
        for (i, &p) in self.blocks[block].params.iter().enumerate().skip(index) {
            self.values[p].def = ValueDef::Param(block, i as u32);
        }
    }
}

pub struct InstIter<'a> {
    func: &'a Function,
    next: Option<Inst>,
}

impl Iterator for InstIter<'_> {
    type Item = Inst;
    fn next(&mut self) -> Option<Inst> {
        let inst = self.next?;
        self.next = self.func.insts[inst].next;
        Some(inst)
    }
}
