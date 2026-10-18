//! The text format (0006 §13): what [`crate::parse`] reads back.
//!
//! Blocks print in reverse postorder (from the entry, through jumps, continue and merge
//! blocks), then any the entry does not reach, so every value is defined above its uses.
//! Values print as their source name made unique (`%acc`, `%acc.1`) or a number (`%5`).

use std::collections::{HashMap, HashSet};
use std::fmt::Write;

use crate::func::{Block, FuncId, Function, Inst, Value, ValueDef};
use crate::module::{Binding, Const, ConstValue, Module};
use crate::num::f16_to_f32;
use crate::op::{AccessStep, Op, WaveOp};
use crate::ty::{Scalar, Type, TypeData};

pub fn print_module(m: &Module) -> String {
    let mut out = String::new();
    for (i, format) in m.formats.iter().enumerate() {
        let _ = writeln!(out, "format {i} = {}", quote(format));
    }
    for (_, s) in m.structs.iter() {
        let fields: Vec<String> = s
            .fields
            .iter()
            .map(|(name, ty)| format!("{}: {}", ident_or_quoted(name), type_name(m, *ty)))
            .collect();
        let _ = writeln!(
            out,
            "struct @{} {{ {} }}",
            ident_or_quoted(&s.name),
            fields.join(", ")
        );
    }
    for (_, g) in m.globals.iter() {
        let _ = write!(
            out,
            "global @{} : {}",
            ident_or_quoted(&g.name),
            type_name(m, g.ty)
        );
        match g.binding {
            Binding::None => {}
            Binding::Builtin(b) => {
                let _ = write!(out, " builtin({})", b.name());
            }
            Binding::Location(l) => {
                let _ = write!(out, " location({l})");
            }
            Binding::Target(t) => {
                let _ = write!(out, " target({t})");
            }
        }
        if let Some(interp) = g.interp {
            let _ = write!(out, " interp({})", interp.name());
        }
        if let Some(init) = g.init {
            let _ = write!(out, " = {}", const_lit(m, init));
        }
        out.push('\n');
    }
    for (id, _) in m.funcs.iter() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&print_function(m, id));
    }
    out
}

pub fn print_function(m: &Module, id: FuncId) -> String {
    let f = &m.funcs[id];
    let p = FuncText::new(m, id);
    let (order, names, block_names) = (&p.order, &p.names, &p.blocks);
    let mut out = String::new();
    match m.entry_of(id) {
        Some(entry) => {
            let _ = write!(out, "entry {}", entry.stage.name());
            if let Some([x, y, z]) = entry.stage.group_size() {
                let _ = write!(out, "({x}, {y}, {z})");
            }
            let uses: Vec<String> = entry
                .uses
                .iter()
                .map(|g| format!("@{}", ident_or_quoted(&m.globals[*g].name)))
                .collect();
            let _ = write!(
                out,
                " @{} uses({})",
                ident_or_quoted(&f.name),
                uses.join(", ")
            );
        }
        None => {
            let params: Vec<String> = f
                .params()
                .iter()
                .map(|&v| format!("%{}: {}", names[&v], type_name(m, f.ty(v))))
                .collect();
            let _ = write!(
                out,
                "func @{}({})",
                ident_or_quoted(&f.name),
                params.join(", ")
            );
            match f.results.as_slice() {
                [] => {}
                [one] => {
                    let _ = write!(out, " -> {}", type_name(m, *one));
                }
                many => {
                    let types: Vec<String> = many.iter().map(|t| type_name(m, *t)).collect();
                    let _ = write!(out, " -> ({})", types.join(", "));
                }
            }
        }
    }
    out.push_str(" {\n");
    for (i, &b) in order.iter().enumerate() {
        out.push_str(&block_names[&b]);
        if i > 0 && !f.blocks[b].params.is_empty() {
            let params: Vec<String> = f.blocks[b]
                .params
                .iter()
                .map(|&v| format!("%{}: {}", names[&v], type_name(m, f.ty(v))))
                .collect();
            let _ = write!(out, "({})", params.join(", "));
        }
        out.push_str(":\n");
        for inst in f.insts(b) {
            out.push_str("    ");
            out.push_str(&p.inst(inst));
            out.push('\n');
        }
    }
    out.push_str("}\n");
    out
}

/// Reverse postorder from the entry, then the blocks it does not reach: jump targets in their
/// order, and a region's merge block after the whole region.
pub fn block_order(f: &Function) -> Vec<Block> {
    let mut seen = HashSet::new();
    let mut post = Vec::new();
    if let Some(entry) = f.entry() {
        // Iterative DFS: (block, next successor to visit). A block visited earlier finishes
        // earlier and so comes later: the merge block first, then the jump targets last first.
        // Continue blocks are reached through the loop body's jumps.
        let mut stack = vec![(entry, 0usize)];
        seen.insert(entry);
        while let Some((b, i)) = stack.pop() {
            let next: Vec<Block> = match f.terminator(b) {
                Some(t) => {
                    let op = &f.insts[t].op;
                    let mut all: Vec<Block> = match op {
                        Op::CondBr { merge, .. }
                        | Op::Switch { merge, .. }
                        | Op::Loop { merge, .. } => vec![*merge],
                        _ => Vec::new(),
                    };
                    all.extend(
                        op.successors(f.operands(t).len())
                            .into_iter()
                            .rev()
                            .map(|s| s.0),
                    );
                    all
                }
                None => Vec::new(),
            };
            if let Some(&s) = next.get(i) {
                stack.push((b, i + 1));
                if f.layout.contains(&s) && seen.insert(s) {
                    stack.push((s, 0));
                }
            } else {
                post.push(b);
            }
        }
    }
    post.reverse();
    for &b in &f.layout {
        if !seen.contains(&b) {
            post.push(b);
        }
    }
    post
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '.'
}

/// A name for the text: as it is when it is an identifier (dots allowed), else quoted.
pub fn ident_or_quoted(name: &str) -> String {
    let ok = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(is_name_char);
    if ok { name.to_string() } else { quote(name) }
}

pub fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{{{:x}}}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Unique names for the values defined in `f`, in print order.
fn value_names(f: &Function, order: &[Block]) -> HashMap<Value, String> {
    let mut names = HashMap::new();
    let mut taken = HashSet::new();
    let mut counter = 0;
    let mut name = |v: Value, names: &mut HashMap<Value, String>| {
        let hint = f.values[v].name.as_deref().filter(|h| {
            h.chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && h.chars().all(is_name_char)
        });
        let chosen = match hint {
            Some(h) => {
                let mut candidate = h.to_string();
                let mut k = 1;
                while taken.contains(&candidate) {
                    candidate = format!("{h}.{k}");
                    k += 1;
                }
                candidate
            }
            None => loop {
                let candidate = counter.to_string();
                counter += 1;
                if !taken.contains(&candidate) {
                    break candidate;
                }
            },
        };
        taken.insert(chosen.clone());
        names.insert(v, chosen);
    };
    for &b in order {
        for &p in &f.blocks[b].params {
            name(p, &mut names);
        }
        for inst in f.insts(b) {
            for &r in &f.insts[inst].results {
                name(r, &mut names);
            }
        }
    }
    names
}

pub fn type_name(m: &Module, t: Type) -> String {
    match m.data(t) {
        TypeData::Scalar(s) => s.name().to_string(),
        TypeData::Vector(s, n) => format!("{}x{n}", s.name()),
        TypeData::Matrix {
            element,
            rows,
            cols,
        } => format!("{}x{rows}x{cols}", element.name()),
        TypeData::Array(e, n) => format!("[{}; {n}]", type_name(m, *e)),
        TypeData::Struct(s) => format!("@{}", ident_or_quoted(&m.structs[*s].name)),
        TypeData::Ptr {
            space,
            readonly,
            pointee,
        } => format!(
            "ptr<{}{}, {}>",
            space.name(),
            if *readonly { " ro" } else { "" },
            type_name(m, *pointee)
        ),
        TypeData::Res(k) | TypeData::Handle(k) => {
            let outer = if matches!(m.data(t), TypeData::Res(_)) {
                "res"
            } else {
                "handle"
            };
            match k.element {
                Some(e) => format!("{outer}<{}<{}>>", k.kind.name(), type_name(m, e)),
                None => format!("{outer}<{}>", k.kind.name()),
            }
        }
        TypeData::RayQuery => "rayquery".to_string(),
    }
}

/// A constant's literal, without its type.
pub fn const_lit(m: &Module, c: Const) -> String {
    let data = &m.consts[c];
    match &data.value {
        ConstValue::Zero => "zero".into(),
        ConstValue::Any => "any".into(),
        ConstValue::Composite(items) => {
            let items: Vec<String> = items.iter().map(|&i| const_lit(m, i)).collect();
            format!("({})", items.join(", "))
        }
        ConstValue::Int(bits) => match m.element_scalar(data.ty) {
            Some(Scalar::I1) => if *bits & 1 == 1 { "true" } else { "false" }.into(),
            Some(s) => {
                let shift = 64 - s.bits();
                (((*bits << shift) as i64) >> shift).to_string()
            }
            None => bits.to_string(),
        },
        ConstValue::Float(bits) => {
            let (finite, text) = match m.element_scalar(data.ty) {
                Some(Scalar::F16) => {
                    let x = f16_to_f32(*bits as u16);
                    (x.is_finite(), format!("{x:?}"))
                }
                Some(Scalar::F32) => {
                    let x = f32::from_bits(*bits as u32);
                    (x.is_finite(), format!("{x:?}"))
                }
                _ => {
                    let x = f64::from_bits(*bits);
                    (x.is_finite(), format!("{x:?}"))
                }
            };
            if finite { text } else { format!("0x{bits:X}") }
        }
    }
}

/// One function as the text format names it: its block order, block names and value names, for
/// printing it whole or a piece of it (the verifier quotes instructions).
pub struct FuncText<'a> {
    m: &'a Module,
    f: &'a Function,
    pub order: Vec<Block>,
    pub names: HashMap<Value, String>,
    pub blocks: HashMap<Block, String>,
}

impl<'a> FuncText<'a> {
    pub fn new(m: &'a Module, id: FuncId) -> Self {
        let f = &m.funcs[id];
        let order = block_order(f);
        let blocks = order
            .iter()
            .enumerate()
            .map(|(i, b)| (*b, format!("block{i}")))
            .collect();
        let names = value_names(f, &order);
        FuncText {
            m,
            f,
            order,
            names,
            blocks,
        }
    }

    /// An operand as it prints.
    pub fn value(&self, v: Value) -> String {
        match self.f.values[v].def {
            ValueDef::Const(c) => format!(
                "{} {}",
                type_name(self.m, self.m.consts[c].ty),
                const_lit(self.m, c)
            ),
            ValueDef::Global(g) => format!("@{}", ident_or_quoted(&self.m.globals[g].name)),
            _ => format!("%{}", self.names.get(&v).map_or("?", |s| s.as_str())),
        }
    }

    fn values(&self, vs: &[Value]) -> String {
        vs.iter()
            .map(|&v| self.value(v))
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn target(&self, b: Block, args: &[Value]) -> String {
        let name = self.blocks.get(&b).map_or("block?", |s| s.as_str());
        if args.is_empty() {
            name.to_string()
        } else {
            format!("{name}({})", self.values(args))
        }
    }

    pub fn block(&self, b: Block) -> &str {
        self.blocks.get(&b).map_or("block?", |s| s.as_str())
    }

    /// The name of field `i` of the struct `ty` is, or of the struct `ty` points to.
    fn field(&self, ty: Type, i: u32) -> String {
        let ty = match self.m.data(ty) {
            TypeData::Ptr { pointee, .. } => *pointee,
            _ => ty,
        };
        match self.m.data(ty) {
            TypeData::Struct(s) => self.m.structs[*s]
                .fields
                .get(i as usize)
                .map_or(format!("{i}"), |f| ident_or_quoted(&f.0)),
            _ => format!("{i}"),
        }
    }

    /// The pointee after one access step, for naming the next field.
    fn step(&self, ty: Type, step: AccessStep) -> Option<Type> {
        match (self.m.data(ty), step) {
            (TypeData::Struct(s), AccessStep::Field(i)) => {
                self.m.structs[*s].fields.get(i as usize).map(|f| f.1)
            }
            (TypeData::Array(e, _), AccessStep::Index) => Some(*e),
            _ => None,
        }
    }

    /// One instruction as it prints.
    pub fn inst(&self, inst: Inst) -> String {
        let data = &self.f.insts[inst];
        let ops = self.f.operands(inst);
        let mut out = String::new();
        if !data.results.is_empty() {
            let results: Vec<String> = data
                .results
                .iter()
                .map(|r| format!("%{}", self.names.get(r).map_or("?", |s| s.as_str())))
                .collect();
            let _ = write!(out, "{} = ", results.join(", "));
        }
        let precise = if data.precise { " precise" } else { "" };
        let result_ty = |i: usize| type_name(self.m, self.f.ty(data.results[i]));
        let body = match &data.op {
            Op::Br(b) => format!("br {}", self.target(*b, ops)),
            Op::CondBr {
                then_block,
                then_args,
                else_block,
                merge,
            } => {
                let t = 1 + *then_args as usize;
                format!(
                    "cond_br {}, {}, {} merge {}",
                    self.value(ops[0]),
                    self.target(*then_block, &ops[1..t]),
                    self.target(*else_block, &ops[t..]),
                    self.block(*merge)
                )
            }
            Op::Switch {
                cases,
                default,
                merge,
                ..
            } => {
                let width = self
                    .m
                    .element_scalar(self.f.ty(ops[0]))
                    .map_or(64, |s| s.bits());
                let mut at = 1;
                let mut arms = Vec::new();
                for case in cases {
                    let n = case.args as usize;
                    let shift = 64 - width;
                    let value = ((case.value << shift) as i64) >> shift;
                    arms.push(format!(
                        "{value}: {}",
                        self.target(case.block, &ops[at..at + n])
                    ));
                    at += n;
                }
                format!(
                    "switch {} [{}] default {} merge {}",
                    self.value(ops[0]),
                    arms.join(", "),
                    self.target(*default, &ops[at..]),
                    self.block(*merge)
                )
            }
            Op::Loop {
                header,
                continuing,
                merge,
            } => format!(
                "loop header {} continue {} merge {}",
                self.target(*header, ops),
                self.block(*continuing),
                self.block(*merge)
            ),
            Op::Return if ops.is_empty() => "return".into(),
            Op::Return => format!("return {}", self.values(ops)),
            Op::Unreachable => "unreachable".into(),
            Op::Binary(b) => format!("{}{precise} {}", b.name(), self.values(ops)),
            Op::Unary(u) => format!("{}{precise} {}", u.name(), self.values(ops)),
            Op::Icmp(c) => format!("icmp {} {}", c.name(), self.values(ops)),
            Op::Fcmp(c) => format!("fcmp{precise} {} {}", c.name(), self.values(ops)),
            Op::Select => format!("select {}", self.values(ops)),
            Op::Convert(c) => format!(
                "{}{precise} {} : {}",
                c.name(),
                self.values(ops),
                result_ty(0)
            ),
            Op::Math(op) => format!("{}{precise} {}", op.name(), self.values(ops)),
            Op::Splat => format!("splat {} : {}", self.values(ops), result_ty(0)),
            Op::Extract(i) => format!("extract {}, {i}", self.values(ops)),
            Op::Insert(i) => format!(
                "insert {}, {i}, {}",
                self.value(ops[0]),
                self.values(&ops[1..])
            ),
            Op::Shuffle(lanes) => {
                let lanes: Vec<String> = lanes.iter().map(u32::to_string).collect();
                format!("shuffle {}, [{}]", self.values(ops), lanes.join(", "))
            }
            Op::ExtractDyn => format!("extract.dyn {}", self.values(ops)),
            Op::InsertDyn => format!("insert.dyn {}", self.values(ops)),
            Op::MatMul => format!("matmul{precise} {}", self.values(ops)),
            Op::Transpose => format!("transpose {}", self.values(ops)),
            Op::MatColumn(i) => format!("mat.column {}, {i}", self.values(ops)),
            Op::MatRow(i) => format!("mat.row {}, {i}", self.values(ops)),
            Op::MatExtract(r, c) => format!("mat.extract {}, {r}, {c}", self.values(ops)),
            Op::Construct => format!("construct {}({})", result_ty(0), self.values(ops)),
            Op::ExtractField(i) => format!(
                "extract.field {}, .{}",
                self.values(ops),
                self.field(self.f.ty(ops[0]), *i)
            ),
            Op::InsertField(i) => format!(
                "insert.field {}, .{}, {}",
                self.value(ops[0]),
                self.field(self.f.ty(ops[0]), *i),
                self.values(&ops[1..])
            ),
            Op::ExtractElem(i) => format!("extract.elem {}, {i}", self.values(ops)),
            Op::InsertElem(i) => format!(
                "insert.elem {}, {i}, {}",
                self.value(ops[0]),
                self.values(&ops[1..])
            ),
            Op::Var => {
                let pointee = match self.m.data(self.f.ty(data.results[0])) {
                    TypeData::Ptr { pointee, .. } => type_name(self.m, *pointee),
                    _ => "?".into(),
                };
                format!("var function : {pointee}")
            }
            Op::Load { aligned } => {
                format!("load {}{}", self.values(ops), aligned_suffix(*aligned))
            }
            Op::Store { aligned } => {
                format!("store {}{}", self.values(ops), aligned_suffix(*aligned))
            }
            Op::Access(steps) => {
                let mut parts = vec![self.value(ops[0])];
                let mut ty = match self.m.data(self.f.ty(ops[0])) {
                    TypeData::Ptr { pointee, .. } => Some(*pointee),
                    _ => None,
                };
                let mut next = 1;
                for step in steps {
                    match step {
                        AccessStep::Field(i) => parts.push(format!(
                            ".{}",
                            ty.map_or(i.to_string(), |t| self.field(t, *i))
                        )),
                        AccessStep::Index => {
                            let v = ops.get(next).copied();
                            next += 1;
                            parts.push(format!("[{}]", v.map_or("?".into(), |v| self.index(v))));
                        }
                    }
                    ty = ty.and_then(|t| self.step(t, *step));
                }
                format!("access {}", parts.join(", "))
            }
            Op::BufferElem => format!("buffer.elem {}", self.values(ops)),
            Op::BufferByte => format!("buffer.byte {}", self.values(ops)),
            Op::UniformPtr => format!("uniform.ptr {}", self.values(ops)),
            Op::BufferLength => format!("buffer.length {}", self.values(ops)),
            Op::DescGet { nonuniform } => format!(
                "desc.get {}{}",
                self.values(ops),
                if *nonuniform { " nonuniform" } else { "" }
            ),
            Op::Atomic { op, scope, order } => format!(
                "atomic.{} {} scope({}) order({})",
                op.name(),
                self.values(ops),
                scope.name(),
                order.name()
            ),
            Op::Barrier { exec, mem, spaces } => {
                let spaces: Vec<&str> = spaces.iter().map(|s| s.name()).collect();
                format!(
                    "barrier exec({}) mem({}) spaces({})",
                    exec.map_or("none", |s| s.name()),
                    mem.name(),
                    spaces.join(", ")
                )
            }
            Op::Discard => "discard".into(),
            Op::IsHelper => "is_helper".into(),
            Op::Deriv { axis, fine } => format!(
                "deriv.{} {} {}",
                axis.name(),
                if *fine { "fine" } else { "coarse" },
                self.values(ops)
            ),
            Op::Wave { op, cluster } => {
                let head = match op {
                    WaveOp::Reduce(r) => format!("wave.reduce {}", r.name()),
                    WaveOp::Scan { op, exclusive } => format!(
                        "wave.scan {} {}",
                        op.name(),
                        if *exclusive { "exclusive" } else { "inclusive" }
                    ),
                    WaveOp::Ballot => "wave.ballot".into(),
                    WaveOp::Broadcast => "wave.broadcast".into(),
                    WaveOp::Shuffle => "wave.shuffle".into(),
                    WaveOp::Quad(q) => format!("wave.quad {}", q.name()),
                    WaveOp::Vote(v) => format!("wave.vote {}", v.name()),
                    WaveOp::Match => "wave.match".into(),
                    WaveOp::LaneIndex => "wave.lane_index".into(),
                    WaveOp::LaneCount => "wave.lane_count".into(),
                    WaveOp::IsFirst => "wave.is_first".into(),
                };
                let mut s = head;
                if !ops.is_empty() {
                    let _ = write!(s, " {}", self.values(ops));
                }
                if let Some(n) = cluster {
                    let _ = write!(s, " cluster({n})");
                }
                s
            }
            Op::Call(callee) => {
                let name = self
                    .m
                    .funcs
                    .get(*callee)
                    .map_or("?".to_string(), |f| ident_or_quoted(&f.name));
                format!("call @{name}({})", self.values(ops))
            }
            Op::DebugPrint(fmt) => format!("debug.print {fmt}, ({})", self.values(ops)),
            Op::DebugAssert(fmt) => format!(
                "debug.assert {}, {fmt}, ({})",
                self.value(ops[0]),
                self.values(&ops[1..])
            ),
        };
        out.push_str(&body);
        out
    }

    /// An index: `3` for an `i32` constant, else the operand.
    fn index(&self, v: Value) -> String {
        if let ValueDef::Const(c) = self.f.values[v].def
            && matches!(
                self.m.data(self.m.consts[c].ty),
                TypeData::Scalar(Scalar::I32)
            )
            && let ConstValue::Int(_) = self.m.consts[c].value
        {
            return const_lit(self.m, c);
        }
        self.value(v)
    }
}

fn aligned_suffix(aligned: Option<u32>) -> String {
    aligned.map_or(String::new(), |n| format!(" aligned({n})"))
}
