//! Building functions: a cursor at the end of a block, instructions whose result types are
//! inferred (0006 §2: MIR lowering, monomorphization and passes all use it).

use crate::func::{Block, FuncId, Function, Inst, Value};
use crate::infer::result_types;
use crate::module::{Const, GlobalId, Module};
use crate::op::{BinaryOp, Op};
use crate::ty::{Scalar, Type};

pub struct FuncBuilder<'m> {
    pub module: &'m mut Module,
    pub func: FuncId,
    block: Option<Block>,
}

impl<'m> FuncBuilder<'m> {
    /// Builds into a new function of `module`.
    pub fn new(module: &'m mut Module, name: &str, results: Vec<Type>) -> Self {
        let func = module.add_function(Function::new(name, results));
        Self {
            module,
            func,
            block: None,
        }
    }

    /// Builds into an existing function.
    pub fn edit(module: &'m mut Module, func: FuncId) -> Self {
        Self {
            module,
            func,
            block: None,
        }
    }

    pub fn f(&mut self) -> &mut Function {
        &mut self.module.funcs[self.func]
    }

    pub fn add_block(&mut self) -> Block {
        self.f().add_block()
    }

    /// Instructions go to the end of `block` from now on.
    pub fn switch_to(&mut self, block: Block) {
        self.block = Some(block);
    }

    pub fn current_block(&self) -> Option<Block> {
        self.block
    }

    pub fn param(&mut self, block: Block, ty: Type, name: Option<&str>) -> Value {
        self.f().add_param(block, ty, name)
    }

    pub fn konst(&mut self, c: Const) -> Value {
        let ty = self.module.consts[c].ty;
        self.f().const_value(c, ty)
    }

    pub fn int(&mut self, s: Scalar, value: i64) -> Value {
        let ty = self.module.scalar(s);
        let c = self.module.int(ty, value);
        self.konst(c)
    }

    pub fn float(&mut self, s: Scalar, value: f64) -> Value {
        let ty = self.module.scalar(s);
        let c = self.module.float(ty, value);
        self.konst(c)
    }

    pub fn bool(&mut self, value: bool) -> Value {
        let c = self.module.bool(value);
        self.konst(c)
    }

    pub fn global(&mut self, g: GlobalId) -> Value {
        let ty = self.module.globals[g].ty;
        self.f().global_value(g, ty)
    }

    /// Appends an instruction at the cursor; its result types are inferred from the operands
    /// (or `explicit`, for the operations that need it).
    pub fn ins_with(
        &mut self,
        op: Op,
        operands: Vec<Value>,
        explicit: Option<Type>,
    ) -> Result<Inst, String> {
        let block = self.block.ok_or("no current block")?;
        let types: Vec<Type> = {
            let f = &self.module.funcs[self.func];
            operands.iter().map(|&v| f.ty(v)).collect()
        };
        let results = result_types(self.module, &op, &types, explicit)?;
        let f = self.f();
        let inst = f.create_inst(op, operands, &results);
        f.append_inst(block, inst);
        Ok(inst)
    }

    /// [`ins_with`](Self::ins_with) for operations whose result types are inferred; panics on
    /// operands the operation cannot take, which is a bug in the caller.
    pub fn ins(&mut self, op: Op, operands: Vec<Value>) -> Inst {
        self.ins_with(op, operands, None)
            .unwrap_or_else(|e| panic!("building IR: {e}"))
    }

    /// The single result of a new instruction.
    pub fn value(&mut self, op: Op, operands: Vec<Value>) -> Value {
        let inst = self.ins(op, operands);
        self.module.funcs[self.func].result(inst)
    }

    pub fn binary(&mut self, op: BinaryOp, a: Value, b: Value) -> Value {
        self.value(Op::Binary(op), vec![a, b])
    }

    pub fn br(&mut self, target: Block, args: Vec<Value>) -> Inst {
        self.ins(Op::Br(target), args)
    }

    pub fn ret(&mut self, values: Vec<Value>) -> Inst {
        self.ins(Op::Return, values)
    }
}
