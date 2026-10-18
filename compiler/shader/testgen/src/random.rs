//! Random structured functions for differential testing (0005 §4.6): `(int, int, int) -> int`
//! with three variables carried through ifs, loops with bounded trip counts, switches,
//! `break`, `continue` and early `return`s, over integer and float arithmetic. What it builds
//! verifies; every loop ends, so every input runs to a result.

use marlin_math::Rng;
use marlin_shaderc_ir::{
    AccessStep, BinaryOp, Block, ConstValue, ConvertOp, FloatCmp, FuncBuilder, FuncId, IntCmp,
    Module, Op, Scalar, Space, SwitchCase, Type, TypeData, UnaryOp, Value,
};

const VARS: usize = 3;

#[derive(Clone, Copy)]
enum Target {
    /// A loop: its continue block (which takes the variables and the counter) and merge.
    Loop {
        cont: Block,
        merge: Block,
        counter: Value,
    },
    Switch {
        merge: Block,
    },
}

struct Gen<'m> {
    b: FuncBuilder<'m>,
    rng: Rng,
    int: Type,
    float: Type,
    /// Statements left to make.
    budget: usize,
    /// Enclosing loops and switches, innermost last.
    targets: Vec<Target>,
    /// Functions this one may call (`(int, int, int) -> int`).
    callees: Vec<FuncId>,
    /// Variables made at the start: a `{ int x, y }` and an `int[4]`.
    record: Value,
    array: Value,
    array_ty: Type,
}

/// Whether the block ended (a jump out, or `return`) or code may follow.
#[derive(PartialEq, Eq)]
enum Flow {
    Open,
    Closed,
}

const INTERESTING: &[i32] = &[0, 1, -1, 2, 3, 7, 31, 32, 255, -128, i32::MIN, i32::MAX];

/// An argument for a generated function.
pub fn arg(rng: &mut Rng) -> i32 {
    if rng.bool() {
        INTERESTING[rng.below(INTERESTING.len())]
    } else {
        rng.next_u32() as i32 >> rng.below(31)
    }
}

/// A module of generated functions: helpers, each of which may call the ones before it, and
/// `@f`, which may call them all.
pub fn function(seed: u64, size: usize) -> (Module, FuncId) {
    let mut m = Module::new();
    let mut rng = Rng::new(seed);
    let helpers = rng.below(3);
    let mut callees = Vec::new();
    for k in 0..helpers {
        let s = rng.next_u64();
        let id = one(&mut m, &format!("h{k}"), s, 4 + size / 3, callees.clone());
        callees.push(id);
    }
    let id = one(&mut m, "f", rng.next_u64(), size, callees);
    (m, id)
}

fn one(m: &mut Module, name: &str, seed: u64, size: usize, callees: Vec<FuncId>) -> FuncId {
    let int = m.scalar(Scalar::I32);
    let float = m.scalar(Scalar::F32);
    let found = m
        .structs
        .iter()
        .find(|(_, s)| &*s.name == "R")
        .map(|(id, _)| id);
    let record =
        found.unwrap_or_else(|| m.add_struct("R", vec![("x".into(), int), ("y".into(), int)]));
    let record_ty = m.struct_ty(record);
    let array_ty = m.ty(TypeData::Array(int, 4));
    let record_ptr = m.ptr(Space::Function, false, record_ty);
    let array_ptr = m.ptr(Space::Function, false, array_ty);
    let mut b = FuncBuilder::new(m, name, vec![int]);
    let entry = b.add_block();
    let mut vars = Vec::new();
    for i in 0..VARS {
        let name = format!("a{i}");
        vars.push(b.param(entry, int, Some(&name)));
    }
    b.switch_to(entry);
    let record = var(&mut b, record_ptr);
    let array = var(&mut b, array_ptr);
    let mut g = Gen {
        b,
        rng: Rng::new(seed),
        int,
        float,
        budget: size,
        targets: Vec::new(),
        callees,
        record,
        array,
        array_ty,
    };
    if g.stmts(&mut vars, 0) == Flow::Open {
        let v = g.combine(&vars);
        g.b.ret(vec![v]);
    }
    g.b.func
}

fn var(b: &mut FuncBuilder<'_>, ptr: Type) -> Value {
    let inst = b.ins_with(Op::Var, Vec::new(), Some(ptr)).expect("a var");
    b.f().result(inst)
}

impl Gen<'_> {
    fn int(&mut self, v: i32) -> Value {
        self.b.int(Scalar::I32, v as i64)
    }

    fn combine(&mut self, vars: &[Value]) -> Value {
        let x = self.b.binary(BinaryOp::Xor, vars[0], vars[1]);
        self.b.binary(BinaryOp::Add, x, vars[2])
    }

    fn expr(&mut self, vars: &[Value], depth: u32) -> Value {
        if depth == 0 || self.rng.chance(0.3) {
            return if self.rng.chance(0.7) {
                vars[self.rng.below(vars.len())]
            } else {
                let v = INTERESTING[self.rng.below(INTERESTING.len())];
                self.int(v)
            };
        }
        if !self.callees.is_empty() && self.rng.chance(0.15) {
            let callee = self.callees[self.rng.below(self.callees.len())];
            let args = (0..VARS).map(|_| self.expr(vars, depth - 1)).collect();
            return self.b.value(Op::Call(callee), args);
        }
        match self.rng.below(10) {
            0..=5 => {
                use BinaryOp::*;
                let ops = [
                    Add, Sub, Mul, And, Or, Xor, Shl, LShr, AShr, SDiv, UDiv, SRem, URem,
                ];
                let op = ops[self.rng.below(ops.len())];
                let a = self.expr(vars, depth - 1);
                let b = self.expr(vars, depth - 1);
                self.b.binary(op, a, b)
            }
            6 => {
                let op = if self.rng.bool() {
                    UnaryOp::Neg
                } else {
                    UnaryOp::Not
                };
                let a = self.expr(vars, depth - 1);
                self.b.value(Op::Unary(op), vec![a])
            }
            7 => {
                let c = self.cond(vars, depth - 1);
                let a = self.expr(vars, depth - 1);
                let b = self.expr(vars, depth - 1);
                self.b.value(Op::Select, vec![c, a, b])
            }
            _ => {
                // Through floats and back.
                let a = self.expr(vars, depth - 1);
                let b = self.expr(vars, depth - 1);
                let fa = self.convert(ConvertOp::SiToFp, a, self.float);
                let fb = self.convert(ConvertOp::SiToFp, b, self.float);
                let op = [BinaryOp::FAdd, BinaryOp::FSub, BinaryOp::FMul][self.rng.below(3)];
                let r = self.b.binary(op, fa, fb);
                if self.rng.bool() {
                    let c = self.b.value(Op::Fcmp(FloatCmp::Olt), vec![fa, r]);
                    let one = self.int(1);
                    let zero = self.int(0);
                    self.b.value(Op::Select, vec![c, one, zero])
                } else {
                    self.convert(ConvertOp::FpToSi, r, self.int)
                }
            }
        }
    }

    fn convert(&mut self, op: ConvertOp, v: Value, to: Type) -> Value {
        let inst = self
            .b
            .ins_with(Op::Convert(op), vec![v], Some(to))
            .expect("a conversion");
        self.b.f().result(inst)
    }

    fn cond(&mut self, vars: &[Value], depth: u32) -> Value {
        use IntCmp::*;
        let ops = [Eq, Ne, Slt, Sle, Sgt, Sge, Ult, Ule, Ugt, Uge];
        let op = ops[self.rng.below(ops.len())];
        let a = self.expr(vars, depth);
        let b = self.expr(vars, depth);
        self.b.value(Op::Icmp(op), vec![a, b])
    }

    /// A block taking the variables (and `extra` more integers).
    fn join(&mut self, extra: usize) -> (Block, Vec<Value>) {
        let block = self.b.add_block();
        let params = (0..VARS + extra)
            .map(|_| self.b.param(block, self.int, None))
            .collect();
        (block, params)
    }

    fn stmts(&mut self, vars: &mut Vec<Value>, depth: u32) -> Flow {
        let n = 1 + self.rng.below(4);
        for _ in 0..n {
            if self.budget == 0 {
                break;
            }
            self.budget -= 1;
            if self.stmt(vars, depth) == Flow::Closed {
                return Flow::Closed;
            }
        }
        Flow::Open
    }

    fn stmt(&mut self, vars: &mut Vec<Value>, depth: u32) -> Flow {
        let nested = depth < 4;
        match self.rng.below(16) {
            11..=14 => {
                self.memory(vars);
                Flow::Open
            }
            0..=4 => {
                let i = self.rng.below(VARS);
                vars[i] = self.expr(vars, 3);
                Flow::Open
            }
            5 | 6 if nested => self.if_stmt(vars, depth),
            7 if nested => self.loop_stmt(vars, depth),
            8 if nested => self.switch_stmt(vars, depth),
            9 | 10 if depth > 0 => self.jump_out(vars),
            _ => {
                let i = self.rng.below(VARS);
                vars[i] = self.expr(vars, 1);
                Flow::Open
            }
        }
    }

    /// `break`, `continue` or `return`.
    fn jump_out(&mut self, vars: &[Value]) -> Flow {
        match (self.rng.below(3), self.targets.last().copied()) {
            (0, Some(Target::Loop { merge, .. } | Target::Switch { merge })) => {
                self.b.br(merge, vars.to_vec());
                Flow::Closed
            }
            (1, Some(_)) => {
                let Some(Target::Loop { cont, counter, .. }) = self
                    .targets
                    .iter()
                    .rev()
                    .find(|t| matches!(t, Target::Loop { .. }))
                    .copied()
                else {
                    return Flow::Open;
                };
                let mut args = vars.to_vec();
                args.push(counter);
                self.b.br(cont, args);
                Flow::Closed
            }
            (2, _) => {
                let v = self.expr(vars, 2);
                self.b.ret(vec![v]);
                Flow::Closed
            }
            _ => Flow::Open,
        }
    }

    fn if_stmt(&mut self, vars: &mut Vec<Value>, depth: u32) -> Flow {
        let c = self.cond(vars, 2);
        let (merge, params) = self.join(0);
        let then = self.b.add_block();
        let with_else = self.rng.bool();
        let other = if with_else { self.b.add_block() } else { merge };
        let mut operands = vec![c];
        let else_args = if with_else { Vec::new() } else { vars.clone() };
        operands.extend(&else_args);
        self.b.ins(
            Op::CondBr {
                then_block: then,
                then_args: 0,
                else_block: other,
                merge,
            },
            operands,
        );
        for arm in [Some(then), with_else.then_some(other)]
            .into_iter()
            .flatten()
        {
            self.b.switch_to(arm);
            let mut local = vars.clone();
            if self.stmts(&mut local, depth + 1) == Flow::Open {
                self.b.br(merge, local);
            }
        }
        self.b.switch_to(merge);
        *vars = params;
        Flow::Open
    }

    fn loop_stmt(&mut self, vars: &mut Vec<Value>, depth: u32) -> Flow {
        let trips = self.rng.below(4) as i32;
        let (header, hparams) = self.join(1);
        let (cont, cparams) = self.join(1);
        let (merge, mparams) = self.join(0);
        let body = self.b.add_block();
        let zero = self.int(0);
        let mut args = vars.clone();
        args.push(zero);
        self.b.ins(
            Op::Loop {
                header,
                continuing: cont,
                merge,
            },
            args,
        );
        // header: leave once the counter reaches the trip count.
        self.b.switch_to(header);
        let counter = hparams[VARS];
        let limit = self.int(trips);
        let done = self.b.value(Op::Icmp(IntCmp::Sge), vec![counter, limit]);
        let mut operands = vec![done];
        operands.extend(&hparams[..VARS]);
        self.b.ins(
            Op::CondBr {
                then_block: merge,
                then_args: VARS as u32,
                else_block: body,
                merge: body,
            },
            operands,
        );
        // body
        self.b.switch_to(body);
        self.targets.push(Target::Loop {
            cont,
            merge,
            counter,
        });
        let mut local = hparams[..VARS].to_vec();
        if self.stmts(&mut local, depth + 1) == Flow::Open {
            local.push(counter);
            self.b.br(cont, local);
        }
        self.targets.pop();
        // continue: count, go round.
        self.b.switch_to(cont);
        let one = self.int(1);
        let next = self.b.binary(BinaryOp::Add, cparams[VARS], one);
        let mut back = cparams[..VARS].to_vec();
        back.push(next);
        self.b.br(header, back);
        self.b.switch_to(merge);
        *vars = mparams;
        Flow::Open
    }

    fn switch_stmt(&mut self, vars: &mut Vec<Value>, depth: u32) -> Flow {
        let e = self.expr(vars, 2);
        let three = self.int(3);
        let scrutinee = self.b.binary(BinaryOp::And, e, three);
        let (merge, params) = self.join(0);
        let mut values: Vec<u64> = (0..4).collect();
        self.rng.shuffle(&mut values);
        let count = 1 + self.rng.below(3);
        let mut cases = Vec::new();
        let mut blocks = Vec::new();
        for &v in &values[..count] {
            let block = self.b.add_block();
            cases.push(SwitchCase {
                value: v,
                block,
                args: 0,
            });
            blocks.push(block);
        }
        let with_default = self.rng.bool();
        let default = if with_default {
            self.b.add_block()
        } else {
            merge
        };
        let mut operands = vec![scrutinee];
        let default_args = if with_default {
            Vec::new()
        } else {
            vars.clone()
        };
        operands.extend(&default_args);
        self.b.ins(
            Op::Switch {
                cases,
                default,
                default_args: default_args.len() as u32,
                merge,
            },
            operands,
        );
        if with_default {
            blocks.push(default);
        }
        self.targets.push(Target::Switch { merge });
        for block in blocks {
            self.b.switch_to(block);
            let mut local = vars.clone();
            if self.stmts(&mut local, depth + 1) == Flow::Open {
                self.b.br(merge, local);
            }
        }
        self.targets.pop();
        self.b.switch_to(merge);
        *vars = params;
        Flow::Open
    }
}

impl Gen<'_> {
    /// An index into the array: a constant, or a value masked into range.
    fn index(&mut self, vars: &[Value]) -> Value {
        if self.rng.bool() {
            let k = self.rng.below(4) as i32;
            self.int(k)
        } else {
            let e = self.expr(vars, 1);
            let three = self.int(3);
            self.b.binary(BinaryOp::And, e, three)
        }
    }

    fn load(&mut self, ptr: Value) -> Value {
        self.b.value(Op::Load { aligned: None }, vec![ptr])
    }

    fn store(&mut self, ptr: Value, v: Value) {
        self.b.ins(Op::Store { aligned: None }, vec![ptr, v]);
    }

    /// Reads and writes of the record and the array, and a lookup in a constant table.
    fn memory(&mut self, vars: &mut [Value]) {
        let i = self.rng.below(VARS);
        match self.rng.below(6) {
            0 => {
                let field = self.rng.below(2) as u32;
                let p = self.b.value(
                    Op::Access(vec![AccessStep::Field(field)]),
                    vec![self.record],
                );
                let v = self.expr(vars, 2);
                self.store(p, v);
            }
            1 => {
                let field = self.rng.below(2) as u32;
                let p = self.b.value(
                    Op::Access(vec![AccessStep::Field(field)]),
                    vec![self.record],
                );
                vars[i] = self.load(p);
            }
            2 => {
                let k = self.index(vars);
                let p = self
                    .b
                    .value(Op::Access(vec![AccessStep::Index]), vec![self.array, k]);
                let v = self.expr(vars, 2);
                self.store(p, v);
            }
            3 => {
                let k = self.index(vars);
                let p = self
                    .b
                    .value(Op::Access(vec![AccessStep::Index]), vec![self.array, k]);
                vars[i] = self.load(p);
            }
            4 => {
                let whole = self.load(self.record);
                let field = self.rng.below(2) as u32;
                vars[i] = self.b.value(Op::ExtractField(field), vec![whole]);
            }
            _ => {
                let int = self.int;
                let items: Vec<_> = (0..4)
                    .map(|_| {
                        let v = INTERESTING[self.rng.below(INTERESTING.len())];
                        self.b.module.int(int, v as i64)
                    })
                    .collect();
                let table = self
                    .b
                    .module
                    .constant(self.array_ty, ConstValue::Composite(items));
                let ptr_ty = self.b.module.ptr(Space::Function, false, self.array_ty);
                // Variables are made in the entry block.
                let f = self.b.f();
                let inst = f.create_inst(Op::Var, Vec::new(), &[ptr_ty]);
                let entry = f.layout[0];
                let first = f
                    .first_inst(entry)
                    .expect("the record and array come first");
                f.insert_before(first, inst);
                let local = f.result(inst);
                let table = self.b.konst(table);
                self.store(local, table);
                let k = self.index(vars);
                let p = self
                    .b
                    .value(Op::Access(vec![AccessStep::Index]), vec![local, k]);
                vars[i] = self.load(p);
            }
        }
    }
}
