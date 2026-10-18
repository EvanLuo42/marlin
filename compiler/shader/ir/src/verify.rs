//! The IR verifier (0006 §3–§8, 0005 §4.6): checks a module and changes nothing.
//!
//! - Types and constants are well formed: vectors of 2–4, no pointers or handles in memory,
//!   no struct that contains itself, constants that fit their types.
//! - Module-level variables, entry points and the call graph: bindings fit their address
//!   space and stage, an entry lists every variable it reaches, no runtime recursion, stage-only
//!   operations (`discard`, derivatives, group barriers) only where the stage allows them.
//! - Each function: blocks end in exactly one terminator, use lists match operands, every
//!   instruction's operands have the types its operation takes and its results the types
//!   inference gives, jumps pass what the target's parameters take, definitions dominate uses,
//!   and pointers and handles stay where 0006 §6–§7 allows.
//! - Structured control flow (0006 §5.1): each selection or loop region is entered only through
//!   its header and left only through its merge block, a `break` (the innermost loop's or
//!   switch's merge) or a `continue`; regions nest; back edges come from the continue block.
//!
//! Regions are found as in SPIR-V: on the CFG plus an edge from each header to its merge (and
//! from a loop's header to its continue block), a region is what its header dominates and its
//! merge does not, inside the region around it. Whether convergent operations stay where
//! uniformity allows (0006 §5.3) needs the uniformity analysis (0005 §11 step 8).
//!
//! Messages name blocks and values as [`crate::print_module`] prints the function.

use std::collections::{HashMap, HashSet};
use std::fmt;

use crate::func::{Block, FuncId, Function, Inst, Value, ValueDef};
use crate::infer::{Existing, result_types_in};
use crate::module::{Binding, Builtin, Const, ConstValue, Module, Stage};
use crate::op::{
    AccessStep, AtomicOp, BinaryOp, ConvertOp, IntCmp, MathOp, MemOrder, Op, QuadOp, ReduceOp,
    Scope, UnaryOp, VoteOp, WaveOp,
};
use crate::print::{FuncText, type_name};
use crate::ty::{ResourceKind, Scalar, Space, Type, TypeData};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifyError {
    /// The function's name, for errors inside one.
    pub func: Option<Box<str>>,
    /// The instruction (or block) as it prints.
    pub at: Option<String>,
    pub message: String,
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(func) = &self.func {
            write!(f, "@{func}: ")?;
        }
        if let Some(at) = &self.at {
            write!(f, "`{at}`: ")?;
        }
        f.write_str(&self.message)
    }
}

/// Every problem in `m`; empty when the module is valid.
pub fn verify_module(m: &Module) -> Vec<VerifyError> {
    let mut v = Verifier {
        m,
        errors: Vec::new(),
        cyclic: HashSet::new(),
    };
    v.module();
    v.errors
}

struct Verifier<'a> {
    m: &'a Module,
    errors: Vec<VerifyError>,
    /// Structs that contain themselves, reported once and otherwise taken as data.
    cyclic: HashSet<crate::ty::StructId>,
}

/// Up to this many errors per function; after that, the rest are noise.
const MAX_FUNCTION_ERRORS: usize = 64;

impl<'a> Verifier<'a> {
    fn error(&mut self, message: impl Into<String>) {
        self.errors.push(VerifyError {
            func: None,
            at: None,
            message: message.into(),
        });
    }

    fn v_const(&self, c: Const) -> Option<&'a crate::module::ConstData> {
        (c.index() < self.m.consts.len()).then(|| &self.m.consts[c])
    }

    fn ty_ok(&self, t: Type) -> bool {
        t.index() < self.m.types.len()
    }

    fn data(&self, t: Type) -> &'a TypeData {
        &self.m.types[t]
    }

    fn show(&self, t: Type) -> String {
        if self.ty_ok(t) {
            type_name(self.m, t)
        } else {
            "?".into()
        }
    }

    // ---- types ----

    /// Whether values of `t` can live in memory and pass between blocks: no pointers, handles
    /// or ray queries anywhere inside.
    fn is_data(&self, t: Type) -> bool {
        self.is_data_at(t, 0)
    }

    fn is_data_at(&self, t: Type, depth: u32) -> bool {
        if depth > 64 || !self.ty_ok(t) {
            return false;
        }
        match self.data(t) {
            TypeData::Scalar(_) | TypeData::Vector(..) | TypeData::Matrix { .. } => true,
            TypeData::Res(_) => true,
            TypeData::Array(e, _) => self.is_data_at(*e, depth + 1),
            TypeData::Struct(s) if self.cyclic.contains(s) => true,
            TypeData::Struct(s) => self
                .m
                .structs
                .get(*s)
                .is_some_and(|s| s.fields.iter().all(|(_, f)| self.is_data_at(*f, depth + 1))),
            TypeData::Ptr { .. } | TypeData::Handle(_) | TypeData::RayQuery => false,
        }
    }

    /// `(scalar, width)` of a scalar or vector.
    fn shape(&self, t: Type) -> Option<(Scalar, u8)> {
        if !self.ty_ok(t) {
            return None;
        }
        match self.data(t) {
            TypeData::Scalar(s) => Some((*s, 1)),
            TypeData::Vector(s, n) => Some((*s, *n)),
            _ => None,
        }
    }

    /// Integer scalars or vectors; `bool` (i1) only if `with_bool`.
    fn is_int(&self, t: Type, with_bool: bool) -> bool {
        self.shape(t)
            .is_some_and(|(s, _)| s.is_int() && (with_bool || s != Scalar::I1))
    }

    /// Float scalars or vectors, and float matrices if `with_matrix`.
    fn is_float(&self, t: Type, with_matrix: bool) -> bool {
        if self.shape(t).is_some_and(|(s, _)| s.is_float()) {
            return true;
        }
        with_matrix
            && self.ty_ok(t)
            && matches!(self.data(t), TypeData::Matrix { element, .. } if element.is_float())
    }

    fn is_scalar(&self, t: Type, s: Scalar) -> bool {
        self.shape(t) == Some((s, 1))
    }

    /// A struct that contains itself by value has no size.
    fn struct_cycles(&mut self) {
        for (id, s) in self.m.structs.iter() {
            let mut stack: Vec<Type> = s.fields.iter().map(|f| f.1).collect();
            let mut seen = HashSet::new();
            while let Some(t) = stack.pop() {
                if !self.ty_ok(t) || !seen.insert(t) {
                    continue;
                }
                match self.data(t) {
                    TypeData::Struct(inner) if *inner == id => {
                        let name = s.name.clone();
                        self.error(format!("struct `{name}` contains itself"));
                        self.cyclic.insert(id);
                        break;
                    }
                    TypeData::Struct(inner) => {
                        if let Some(inner) = self.m.structs.get(*inner) {
                            stack.extend(inner.fields.iter().map(|f| f.1));
                        }
                    }
                    TypeData::Array(e, _) => stack.push(*e),
                    _ => {}
                }
            }
        }
    }

    fn types(&mut self) {
        self.struct_cycles();
        for (_, s) in self.m.structs.iter() {
            if s.fields.iter().any(|f| !self.is_data(f.1)) {
                let name = s.name.clone();
                self.error(format!(
                    "struct `{name}`: a struct holds data: no pointers, handles or ray queries"
                ));
            }
        }
        for (t, data) in self.m.types.iter() {
            let problem = match data {
                TypeData::Vector(_, n) if !(2..=4).contains(n) => {
                    Some("a vector has 2 to 4 elements")
                }
                TypeData::Matrix {
                    element,
                    rows,
                    cols,
                } => {
                    if !element.is_float() {
                        Some("a matrix holds floats")
                    } else if !(2..=4).contains(rows) || !(2..=4).contains(cols) {
                        Some("a matrix has 2 to 4 rows and columns")
                    } else {
                        None
                    }
                }
                TypeData::Array(e, n) => {
                    if *n == 0 {
                        Some("an array has at least one element")
                    } else if !self.is_data(*e) {
                        Some("an array holds data: no pointers, handles or ray queries")
                    } else {
                        None
                    }
                }
                TypeData::Struct(s) => match self.m.structs.get(*s) {
                    None => Some("no such struct"),
                    Some(_) => None,
                },
                TypeData::Ptr { space, pointee, .. } => {
                    if !self.ty_ok(*pointee) {
                        Some("no such type")
                    } else if matches!(self.data(*pointee), TypeData::RayQuery) {
                        (*space != Space::Function)
                            .then_some("a ray query lives in a `function` variable")
                    } else if !self.is_data(*pointee) {
                        Some("a pointer points at data: no pointers, handles or ray queries")
                    } else {
                        None
                    }
                }
                TypeData::Res(k) | TypeData::Handle(k) => {
                    if k.kind.has_element() != k.element.is_some() {
                        Some("this resource kind's element type is missing or extra")
                    } else {
                        match k.element {
                            Some(e) if !self.is_data(e) => {
                                Some("a resource holds data: no pointers, handles or ray queries")
                            }
                            Some(e)
                                if matches!(
                                    k.kind,
                                    ResourceKind::Texture2D
                                        | ResourceKind::Texture2DArray
                                        | ResourceKind::TextureCube
                                        | ResourceKind::StorageTexture2D
                                        | ResourceKind::StorageTexture2DArray
                                ) && !self.is_int(e, false)
                                    && !self.is_float(e, false) =>
                            {
                                Some("a texture's texels are numeric scalars or vectors")
                            }
                            _ => None,
                        }
                    }
                }
                _ => None,
            };
            if let Some(p) = problem {
                let shown = self.show(t);
                self.error(format!("type `{shown}`: {p}"));
            }
        }
    }

    fn consts(&mut self) {
        for (c, _) in self.m.consts.iter() {
            if let Some(p) = self.const_problem(c) {
                let ty = self.show(self.m.consts[c].ty);
                self.error(format!("constant of type `{ty}`: {p}"));
            }
        }
    }

    fn const_problem(&self, c: Const) -> Option<String> {
        let data = self.v_const(c)?;
        let ty = data.ty;
        if !self.ty_ok(ty) {
            return Some("no such type".into());
        }
        let fits = |bits: u64, width: u32| width >= 64 || bits >> width == 0;
        match (&data.value, self.data(ty)) {
            (ConstValue::Int(bits), TypeData::Scalar(s)) if s.is_int() => {
                (!fits(*bits, s.bits())).then(|| "the value is wider than the type".into())
            }
            (ConstValue::Float(bits), TypeData::Scalar(s)) if s.is_float() => {
                (!fits(*bits, s.bits())).then(|| "the value is wider than the type".into())
            }
            (ConstValue::Int(_) | ConstValue::Float(_), _) => {
                Some("a number constant has a scalar type of its kind".into())
            }
            (ConstValue::Zero | ConstValue::Any, _) => {
                (!self.is_data(ty)).then(|| "only data has constants".into())
            }
            (ConstValue::Composite(items), shape) => {
                let parts: Vec<Option<Type>> = match shape {
                    TypeData::Vector(s, n) => {
                        let e = self.m.types.lookup(&TypeData::Scalar(*s));
                        vec![e; *n as usize]
                    }
                    TypeData::Matrix {
                        element,
                        rows,
                        cols,
                    } => {
                        let col = self.m.types.lookup(&TypeData::Vector(*element, *rows));
                        vec![col; *cols as usize]
                    }
                    TypeData::Array(e, n) => {
                        if *n as usize != items.len() {
                            return Some(format!("{} elements for an array of {n}", items.len()));
                        }
                        vec![Some(*e); *n as usize]
                    }
                    TypeData::Struct(s) => self
                        .m
                        .structs
                        .get(*s)?
                        .fields
                        .iter()
                        .map(|f| Some(f.1))
                        .collect(),
                    _ => return Some("this type has no composite constants".into()),
                };
                if parts.len() != items.len() {
                    return Some(format!(
                        "{} parts where the type has {}",
                        items.len(),
                        parts.len()
                    ));
                }
                for (item, part) in items.iter().zip(parts) {
                    let item_ty = self.v_const(*item).map(|d| d.ty);
                    if item_ty.is_none() || item_ty != part {
                        return Some("a part does not have the type its place takes".into());
                    }
                }
                None
            }
        }
    }

    // ---- module ----

    fn module(&mut self) {
        self.types();
        self.consts();
        let mut names = HashSet::new();
        for (_, s) in self.m.structs.iter() {
            if !names.insert(s.name.clone()) {
                let n = s.name.clone();
                self.error(format!("two structs are named `{n}`"));
            }
        }
        self.globals();
        let mut names = HashSet::new();
        for (_, f) in self.m.funcs.iter() {
            if !names.insert(f.name.clone()) {
                let n = f.name.clone();
                self.error(format!("two functions are named `{n}`"));
            }
        }
        for (id, _) in self.m.funcs.iter() {
            self.function(id);
        }
        self.recursion();
        self.entries();
    }

    fn globals(&mut self) {
        let mut names = HashSet::new();
        for (_, g) in self.m.globals.iter() {
            let name = g.name.clone();
            if !names.insert(name.clone()) {
                self.error(format!("two globals are named `{name}`"));
            }
            let mut problems = Vec::new();
            let (space, pointee) = match self.ty_ok(g.ty).then(|| self.data(g.ty)) {
                Some(TypeData::Ptr { space, pointee, .. }) => (*space, *pointee),
                _ => {
                    self.error(format!("global `@{name}`: its type is a pointer"));
                    continue;
                }
            };
            if space == Space::Function {
                problems.push("a module-level variable is not in the `function` space".into());
            }
            let io = matches!(space, Space::Input | Space::Output);
            match g.binding {
                Binding::None => {}
                Binding::Builtin(b) => {
                    if !io {
                        problems.push("a builtin is an `input` or `output`".to_string());
                    } else if !self.ty_ok(pointee) || !builtin_type_ok(self, b, pointee) {
                        let shown = self.show(pointee);
                        problems.push(format!(
                            "`builtin({})` must be {}, not `{shown}`",
                            b.name(),
                            builtin_expected(b)
                        ));
                    }
                }
                Binding::Location(_) if !io => {
                    problems.push("a location is for `input` and `output`".into());
                }
                Binding::Target(_) if space != Space::Output => {
                    problems.push("a render target is an `output`".into());
                }
                _ => {}
            }
            if g.interp.is_some() && !io {
                problems.push("interpolation is for `input` and `output`".into());
            }
            match (space, g.init) {
                (Space::Constant, None) => problems.push("constant data needs `= ...`".into()),
                (Space::Constant, Some(c)) => {
                    let ty = self.v_const(c).map(|d| d.ty);
                    if ty != Some(pointee) {
                        problems.push("the initial value has another type".into());
                    }
                }
                (_, Some(_)) => {
                    problems.push("only `constant` variables have initial values".into());
                }
                _ => {}
            }
            for p in problems {
                self.error(format!("global `@{name}`: {p}"));
            }
        }
    }

    /// The functions `entry` reaches through calls, itself included.
    fn reached(&self, entry: FuncId) -> Vec<FuncId> {
        let mut seen = HashSet::new();
        let mut stack = vec![entry];
        let mut out = Vec::new();
        while let Some(f) = stack.pop() {
            if !seen.insert(f) {
                continue;
            }
            out.push(f);
            let Some(func) = self.m.funcs.get(f) else {
                continue;
            };
            for (_, inst) in func.insts.iter() {
                if let Op::Call(callee) = inst.op
                    && self.m.funcs.get(callee).is_some()
                {
                    stack.push(callee);
                }
            }
        }
        out
    }

    fn placed_insts(f: &Function) -> Vec<Inst> {
        f.layout
            .iter()
            .filter(|b| f.blocks.get(**b).is_some())
            .flat_map(|&b| f.insts(b).take(f.insts.len()).collect::<Vec<_>>())
            .collect()
    }

    fn entries(&mut self) {
        let mut seen = HashSet::new();
        for entry in &self.m.entries {
            let Some(f) = self.m.funcs.get(entry.func) else {
                self.error("an entry point names no function");
                continue;
            };
            let name = f.name.clone();
            let mut problems: Vec<String> = Vec::new();
            if !seen.insert(entry.func) {
                problems.push("it is an entry point twice".into());
            }
            if !f.params().is_empty() || !f.results.is_empty() {
                problems.push("an entry point has no parameters or results".into());
            }
            if let Some(size) = entry.stage.group_size()
                && size.contains(&0)
            {
                problems.push("a thread group has at least one thread in each direction".into());
            }
            let uses: HashSet<_> = entry.uses.iter().copied().collect();
            if uses.len() != entry.uses.len() {
                problems.push("`uses` names a variable twice".into());
            }
            let compute_like = entry.stage.group_size().is_some();
            for id in self.reached(entry.func) {
                let Some(func) = self.m.funcs.get(id) else {
                    continue;
                };
                for (vid, value) in func.values.iter() {
                    if let ValueDef::Global(g) = value.def {
                        let Some(global) = self.m.globals.get(g) else {
                            continue;
                        };
                        if func.uses(vid).is_empty() {
                            continue;
                        }
                        let gname = &global.name;
                        if !uses.contains(&g) {
                            problems
                                .push(format!("it uses `@{gname}`, which `uses` does not list"));
                        }
                        let space = match self.data(global.ty) {
                            TypeData::Ptr { space, .. } => *space,
                            _ => continue,
                        };
                        if matches!(space, Space::GroupShared) && !compute_like {
                            problems.push(format!(
                                "`@{gname}` is groupshared, which a {} entry point has none of",
                                entry.stage.name()
                            ));
                        }
                        if let Binding::Builtin(b) = global.binding
                            && !builtin_allowed(b, entry.stage, space)
                        {
                            problems.push(format!(
                                "`builtin({})` is not an {} of a {} entry point",
                                b.name(),
                                space.name(),
                                entry.stage.name()
                            ));
                        }
                    }
                }
                for inst in Self::placed_insts(func) {
                    let op = &func.insts[inst].op;
                    let pixel_only = matches!(op, Op::Discard | Op::IsHelper | Op::Deriv { .. });
                    if pixel_only && entry.stage != Stage::Pixel {
                        problems.push(format!(
                            "`@{}` uses a pixel-only operation, reached from a {} entry point",
                            func.name,
                            entry.stage.name()
                        ));
                    }
                    if let Op::Barrier {
                        exec: Some(Scope::Group),
                        ..
                    } = op
                        && !compute_like
                    {
                        problems.push(format!(
                            "`@{}` has a group barrier, reached from a {} entry point",
                            func.name,
                            entry.stage.name()
                        ));
                    }
                    if let Op::Call(callee) = op
                        && self.m.entry_of(*callee).is_some()
                    {
                        problems.push("an entry point is not called".into());
                    }
                }
            }
            problems.dedup();
            for p in problems {
                self.errors.push(VerifyError {
                    func: Some(name.clone()),
                    at: None,
                    message: p,
                });
            }
        }
    }

    /// Calls must not form a cycle: recursion only exists at compile time (0004 §6).
    fn recursion(&mut self) {
        let mut state: HashMap<FuncId, u8> = HashMap::new(); // 1: on the path, 2: done
        let ids: Vec<FuncId> = self.m.funcs.iter().map(|(id, _)| id).collect();
        for start in ids {
            if state.contains_key(&start) {
                continue;
            }
            // Iterative DFS with an explicit path.
            let mut path: Vec<(FuncId, Vec<FuncId>)> = Vec::new();
            state.insert(start, 1);
            path.push((start, self.callees(start)));
            while let Some((f, callees)) = path.last_mut() {
                let f = *f;
                match callees.pop() {
                    Some(c) => match state.get(&c) {
                        Some(1) => {
                            let at = path.iter().position(|(p, _)| *p == c).unwrap_or(0);
                            let mut names: Vec<String> = path[at..]
                                .iter()
                                .map(|(p, _)| format!("@{}", self.m.funcs[*p].name))
                                .collect();
                            names.push(format!("@{}", self.m.funcs[c].name));
                            self.error(format!(
                                "runtime recursion: {} (recursion must unroll at compile time)",
                                names.join(" -> ")
                            ));
                        }
                        Some(_) => {}
                        None => {
                            state.insert(c, 1);
                            let next = self.callees(c);
                            path.push((c, next));
                        }
                    },
                    None => {
                        state.insert(f, 2);
                        path.pop();
                    }
                }
            }
        }
    }

    fn callees(&self, f: FuncId) -> Vec<FuncId> {
        let Some(func) = self.m.funcs.get(f) else {
            return Vec::new();
        };
        let mut out: Vec<FuncId> = Self::placed_insts(func)
            .into_iter()
            .filter_map(|i| match func.insts[i].op {
                Op::Call(c) if self.m.funcs.get(c).is_some() => Some(c),
                _ => None,
            })
            .collect();
        out.sort();
        out.dedup();
        out
    }

    // ---- functions ----

    fn function(&mut self, id: FuncId) {
        let start = self.errors.len();
        FunctionVerifier::run(self, id);
        let name = self.m.funcs[id].name.clone();
        for e in &mut self.errors[start..] {
            e.func.get_or_insert_with(|| name.clone());
        }
        if self.errors.len() - start > MAX_FUNCTION_ERRORS {
            self.errors.truncate(start + MAX_FUNCTION_ERRORS);
            self.errors.push(VerifyError {
                func: Some(name),
                at: None,
                message: "more errors follow".into(),
            });
        }
    }
}

fn builtin_expected(b: Builtin) -> &'static str {
    use Builtin::*;
    match b {
        DispatchThreadId | GroupId | GroupThreadId => "`i32x3`",
        Position => "`f32x4`",
        ClipDistance => "`[f32; N]`",
        CullPrimitive | IsFrontFace => "`i1`",
        Depth => "`f32`",
        _ => "`i32`",
    }
}

fn builtin_type_ok(v: &Verifier<'_>, b: Builtin, t: Type) -> bool {
    use Builtin::*;
    match b {
        DispatchThreadId | GroupId | GroupThreadId => v.shape(t) == Some((Scalar::I32, 3)),
        Position => v.shape(t) == Some((Scalar::F32, 4)),
        ClipDistance => matches!(v.data(t), TypeData::Array(e, _) if v.is_scalar(*e, Scalar::F32)),
        CullPrimitive | IsFrontFace => v.is_scalar(t, Scalar::I1),
        Depth => v.is_scalar(t, Scalar::F32),
        GroupIndex | VertexId | InstanceId | PrimitiveId | SampleIndex | Coverage => {
            v.is_scalar(t, Scalar::I32)
        }
    }
}

/// 0004 §11.2's table: where a system value exists.
fn builtin_allowed(b: Builtin, stage: Stage, space: Space) -> bool {
    use Builtin::*;
    let input = space == Space::Input;
    let compute_like = stage.group_size().is_some();
    match b {
        DispatchThreadId | GroupId | GroupThreadId | GroupIndex => input && compute_like,
        VertexId | InstanceId => input && stage == Stage::Vertex,
        Position | ClipDistance => {
            if input {
                stage == Stage::Pixel
            } else {
                matches!(stage, Stage::Vertex | Stage::Mesh(_))
            }
        }
        PrimitiveId => {
            if input {
                stage == Stage::Pixel
            } else {
                matches!(stage, Stage::Mesh(_))
            }
        }
        CullPrimitive => !input && matches!(stage, Stage::Mesh(_)),
        IsFrontFace | SampleIndex => input && stage == Stage::Pixel,
        Depth | Coverage => !input && stage == Stage::Pixel,
    }
}

// ---- one function ----

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Selection,
    Switch,
    Loop,
}

/// A structured region (0006 §5.1), by block positions in the layout.
struct Region {
    kind: Kind,
    header: usize,
    merge: usize,
    /// Loops: the continue block, and the block whose `loop` instruction names the region.
    cont: Option<usize>,
    owner: usize,
    blocks: Vec<bool>,
}

/// Dominators of a graph over `0..n` from `entry`: `idom[b]` (the entry is its own), `None`
/// for blocks the entry does not reach.
fn dominators(n: usize, entry: usize, succs: &[Vec<usize>]) -> Vec<Option<usize>> {
    // Reverse postorder.
    let mut order = Vec::new();
    let mut seen = vec![false; n];
    let mut stack = vec![(entry, 0usize)];
    seen[entry] = true;
    while let Some((b, i)) = stack.pop() {
        if let Some(&s) = succs[b].get(i) {
            stack.push((b, i + 1));
            if !seen[s] {
                seen[s] = true;
                stack.push((s, 0));
            }
        } else {
            order.push(b);
        }
    }
    order.reverse();
    let mut rpo = vec![usize::MAX; n];
    for (i, &b) in order.iter().enumerate() {
        rpo[b] = i;
    }
    let mut preds = vec![Vec::new(); n];
    for &b in &order {
        for &s in &succs[b] {
            preds[s].push(b);
        }
    }
    // Cooper, Harvey and Kennedy's iteration.
    let mut idom: Vec<Option<usize>> = vec![None; n];
    idom[entry] = Some(entry);
    let mut changed = true;
    while changed {
        changed = false;
        for &b in order.iter().skip(1) {
            let mut new: Option<usize> = None;
            for &p in &preds[b] {
                if idom[p].is_none() {
                    continue;
                }
                new = Some(match new {
                    None => p,
                    Some(mut a) => {
                        let mut c = p;
                        while a != c {
                            while rpo[a] > rpo[c] {
                                a = idom[a].unwrap_or(entry);
                            }
                            while rpo[c] > rpo[a] {
                                c = idom[c].unwrap_or(entry);
                            }
                        }
                        a
                    }
                });
            }
            if new.is_some() && idom[b] != new {
                idom[b] = new;
                changed = true;
            }
        }
    }
    idom
}

fn dominates(idom: &[Option<usize>], a: usize, mut b: usize) -> bool {
    if idom[b].is_none() {
        return false;
    }
    loop {
        if a == b {
            return true;
        }
        match idom[b] {
            Some(p) if p != b => b = p,
            _ => return false,
        }
    }
}

struct FunctionVerifier<'v, 'a> {
    v: &'v mut Verifier<'a>,
    m: &'a Module,
    f: &'a Function,
    /// Layout position of each block.
    index: HashMap<Block, usize>,
    /// Where each placed instruction is: block position, position in the block.
    position: HashMap<Inst, (usize, usize)>,
    text: Option<FuncText<'a>>,
}

impl<'v, 'a> FunctionVerifier<'v, 'a> {
    fn run(v: &'v mut Verifier<'a>, id: FuncId) {
        let m = v.m;
        let f = &m.funcs[id];
        let mut fv = FunctionVerifier {
            v,
            m,
            f,
            index: HashMap::new(),
            position: HashMap::new(),
            text: None,
        };
        if fv.layout() {
            fv.text = Some(FuncText::new(m, id));
            fv.body();
        }
    }

    fn error(&mut self, message: impl Into<String>) {
        self.v.errors.push(VerifyError {
            func: None,
            at: None,
            message: message.into(),
        });
    }

    fn error_at(&mut self, inst: Inst, message: impl Into<String>) {
        let at = self.text.as_ref().map(|t| t.inst(inst));
        self.v.errors.push(VerifyError {
            func: None,
            at,
            message: message.into(),
        });
    }

    fn block_name(&self, b: Block) -> String {
        self.text
            .as_ref()
            .map_or_else(|| format!("{b:?}"), |t| t.block(b).to_string())
    }

    /// Blocks, instruction lists and operands are sound enough to walk; reports what is not.
    fn layout(&mut self) -> bool {
        let f = self.f;
        if f.layout.is_empty() {
            self.error("a function has at least one block");
            return false;
        }
        for (i, &b) in f.layout.iter().enumerate() {
            if f.blocks.get(b).is_none() {
                self.error("the layout names a block that does not exist");
                return false;
            }
            if self.index.insert(b, i).is_some() {
                self.error("the layout names a block twice");
                return false;
            }
        }
        let mut ok = true;
        for (bi, &b) in f.layout.iter().enumerate() {
            let mut prev = None;
            for (k, inst) in f.insts(b).enumerate() {
                if k >= f.insts.len() || self.position.insert(inst, (bi, k)).is_some() {
                    self.error("an instruction list loops");
                    return false;
                }
                if f.block_of(inst) != Some(b) || f.prev_inst(inst) != prev {
                    self.error("an instruction list's links disagree");
                    return false;
                }
                prev = Some(inst);
            }
            if f.last_inst(b) != prev {
                self.error("a block's last instruction is not the end of its list");
                return false;
            }
        }
        // Every value an instruction names must exist, with the type it says.
        let placed: Vec<Inst> = self.position.keys().copied().collect();
        for inst in placed {
            let data = &f.insts[inst];
            for &v in f.operands(inst).iter().chain(&data.results) {
                if f.values.get(v).is_none() || !self.v.ty_ok(f.values[v].ty) {
                    self.error("an instruction names a value that does not exist");
                    ok = false;
                }
            }
            let blocks = data
                .op
                .successors(f.operands(inst).len())
                .into_iter()
                .map(|s| s.0)
                .chain(data.op.structure());
            for b in blocks {
                if !self.index.contains_key(&b) {
                    self.error("an instruction names a block that is not in the layout");
                    ok = false;
                }
            }
            if let Op::Call(c) = data.op
                && self.m.funcs.get(c).is_none()
            {
                self.error("a call names a function that does not exist");
                ok = false;
            }
        }
        for &b in &f.layout {
            for &p in &f.blocks[b].params {
                if f.values.get(p).is_none() || !self.v.ty_ok(f.values[p].ty) {
                    self.error("a block parameter does not exist");
                    ok = false;
                }
            }
        }
        ok
    }

    fn body(&mut self) {
        let f = self.f;
        self.definitions();
        // Terminators.
        for (bi, &b) in f.layout.iter().enumerate() {
            let insts: Vec<Inst> = f.insts(b).collect();
            match insts.last() {
                Some(&last) if f.insts[last].op.is_terminator() => {}
                _ => {
                    let name = self.block_name(b);
                    self.error(format!("{name} does not end in a terminator"));
                }
            }
            for &inst in insts.iter().rev().skip(1) {
                if f.insts[inst].op.is_terminator() {
                    self.error_at(inst, "a terminator ends its block");
                }
            }
            if bi > 0 {
                for &p in &f.blocks[b].params {
                    if !self.v.is_data(f.ty(p)) {
                        self.error(format!(
                            "{}: a block parameter cannot be a pointer or handle",
                            self.block_name(b)
                        ));
                    }
                }
            }
        }
        for &t in &f.results {
            if !self.v.ty_ok(t) || !self.v.is_data(t) {
                self.error("a function returns data, not pointers or handles");
            }
        }
        let insts: Vec<Inst> = {
            let mut v: Vec<_> = self.position.iter().map(|(i, p)| (*p, *i)).collect();
            v.sort();
            v.into_iter().map(|(_, i)| i).collect()
        };
        for &inst in &insts {
            self.inst(inst);
        }
        self.dominance(&insts);
        self.structure();
    }

    /// Every value's definition and uses agree with the instructions that hold them.
    fn definitions(&mut self) {
        let f = self.f;
        for (v, data) in f.values.iter() {
            let ok = match data.def {
                ValueDef::Result(inst, i) => {
                    self.position.contains_key(&inst)
                        && f.insts[inst].results.get(i as usize) == Some(&v)
                        || f.uses(v).is_empty()
                }
                ValueDef::Param(b, i) => {
                    f.blocks.get(b).and_then(|b| b.params.get(i as usize)) == Some(&v)
                }
                ValueDef::Const(c) => self.v.v_const(c).is_some_and(|d| d.ty == data.ty),
                ValueDef::Global(g) => self.m.globals.get(g).is_some_and(|g| g.ty == data.ty),
            };
            if !ok {
                self.error("a value's definition does not match what defines it");
            }
            for u in f.uses(v) {
                let placed = self.position.contains_key(&u.inst);
                if !placed || f.operands(u.inst).get(u.index as usize) != Some(&v) {
                    self.error(
                        "a use list names an operand that is not in a block or does not read the value",
                    );
                }
            }
        }
        let mut ok = true;
        for &inst in self.position.keys() {
            for (i, &v) in f.operands(inst).iter().enumerate() {
                let listed = f
                    .uses(v)
                    .iter()
                    .any(|u| u.inst == inst && u.index == i as u32);
                ok &= listed;
            }
        }
        if !ok {
            self.error("an operand is missing from its value's use list");
        }
    }

    // ---- instructions ----

    fn inst(&mut self, inst: Inst) {
        let f = self.f;
        let data = &f.insts[inst];
        let ops: Vec<Value> = f.operands(inst).to_vec();
        let args: Vec<Type> = ops.iter().map(|&v| f.ty(v)).collect();
        let results: Vec<Type> = data.results.iter().map(|&r| f.ty(r)).collect();
        let explicit = match data.op {
            Op::Convert(_) | Op::Splat | Op::Construct | Op::Var => results.first().copied(),
            _ => None,
        };
        match result_types_in(&mut Existing(self.m), &data.op, &args, explicit) {
            Ok(expected) if expected != results => {
                let want: Vec<String> = expected.iter().map(|t| self.v.show(*t)).collect();
                self.error_at(inst, format!("the results should be ({})", want.join(", ")));
            }
            Ok(_) => {}
            Err(e) => {
                self.error_at(inst, e);
                return;
            }
        }
        if data.precise
            && !matches!(
                data.op,
                Op::Binary(b) if b.is_float()
            )
            && !matches!(
                data.op,
                Op::Unary(UnaryOp::FNeg)
                    | Op::Fcmp(_)
                    | Op::Convert(
                        ConvertOp::FpExt
                            | ConvertOp::FpTrunc
                            | ConvertOp::SiToFp
                            | ConvertOp::UiToFp
                            | ConvertOp::FpToSi
                            | ConvertOp::FpToUi
                    )
                    | Op::Math(_)
                    | Op::MatMul
            )
        {
            self.error_at(inst, "`precise` is for floating-point operations");
        }
        let problem = self.operands(inst, &data.op, &ops, &args, &results);
        if let Some(p) = problem {
            self.error_at(inst, p);
        }
    }

    /// What is wrong with the operands, if anything.
    fn operands(
        &mut self,
        inst: Inst,
        op: &Op,
        ops: &[Value],
        args: &[Type],
        results: &[Type],
    ) -> Option<String> {
        let v = &*self.v;
        let f = self.f;
        let count = |n: usize| (args.len() != n).then(|| format!("takes {n} operand(s)"));
        let same = |ts: &[Type]| ts.windows(2).all(|w| w[0] == w[1]);
        let index_ok = |t: Type| v.is_int(t, false) && v.shape(t).is_some_and(|s| s.1 == 1);
        match op {
            Op::Binary(b) => {
                if let Some(p) = count(2) {
                    return Some(p);
                }
                if !same(args) {
                    return Some("the operands have different types".into());
                }
                let ok = match b {
                    _ if b.is_float() => v.is_float(args[0], true),
                    BinaryOp::And | BinaryOp::Or | BinaryOp::Xor => v.is_int(args[0], true),
                    _ => v.is_int(args[0], false),
                };
                (!ok).then(|| format!("`{}` does not take `{}`", b.name(), v.show(args[0])))
            }
            Op::Unary(u) => {
                if let Some(p) = count(1) {
                    return Some(p);
                }
                let ok = match u {
                    UnaryOp::Neg => v.is_int(args[0], false),
                    UnaryOp::Not => v.is_int(args[0], true),
                    UnaryOp::FNeg => v.is_float(args[0], true),
                };
                (!ok).then(|| format!("`{}` does not take `{}`", u.name(), v.show(args[0])))
            }
            Op::Icmp(c) => {
                if let Some(p) = count(2) {
                    return Some(p);
                }
                let bool_ok = matches!(c, IntCmp::Eq | IntCmp::Ne);
                if !same(args) || !v.is_int(args[0], bool_ok) {
                    return Some("compares two integers of one type".into());
                }
                None
            }
            Op::Fcmp(_) => {
                if let Some(p) = count(2) {
                    return Some(p);
                }
                (!same(args) || !v.is_float(args[0], false))
                    .then(|| "compares two floats of one type".into())
            }
            Op::Select => {
                if let Some(p) = count(3) {
                    return Some(p);
                }
                if args[1] != args[2] {
                    return Some("both choices have one type".into());
                }
                if !v.is_data(args[1]) {
                    return Some("pointers and handles cannot be selected".into());
                }
                match v.shape(args[0]) {
                    Some((Scalar::I1, 1)) => None,
                    Some((Scalar::I1, n)) if v.shape(args[1]).map(|s| s.1) == Some(n) => None,
                    _ => Some(
                        "the condition is `i1`, or an `i1` vector as wide as the choices".into(),
                    ),
                }
            }
            Op::Convert(c) => {
                if let Some(p) = count(1) {
                    return Some(p);
                }
                let (Some((from, n)), Some((to, m))) = (v.shape(args[0]), v.shape(results[0]))
                else {
                    return Some("converts scalars and vectors".into());
                };
                if n != m {
                    return Some("a conversion keeps the number of components".into());
                }
                let ok = match c {
                    ConvertOp::SExt | ConvertOp::ZExt => {
                        from.is_int() && to.is_int() && to.bits() > from.bits()
                    }
                    ConvertOp::Trunc => from.is_int() && to.is_int() && to.bits() < from.bits(),
                    ConvertOp::FpExt => from.is_float() && to.is_float() && to.bits() > from.bits(),
                    ConvertOp::FpTrunc => {
                        from.is_float() && to.is_float() && to.bits() < from.bits()
                    }
                    ConvertOp::SiToFp | ConvertOp::UiToFp => {
                        from.is_int() && from != Scalar::I1 && to.is_float()
                    }
                    ConvertOp::FpToSi | ConvertOp::FpToUi => {
                        from.is_float() && to.is_int() && to != Scalar::I1
                    }
                    ConvertOp::Bitcast => {
                        from.bits() == to.bits() && from != Scalar::I1 && from != to
                    }
                };
                (!ok).then(|| {
                    format!(
                        "`{}` does not convert `{}` to `{}`",
                        c.name(),
                        v.show(args[0]),
                        v.show(results[0])
                    )
                })
            }
            Op::Math(op) => {
                if let Some(p) = count(op.arity()) {
                    return Some(p);
                }
                self.math(*op, args)
            }
            Op::Splat => {
                if let Some(p) = count(1) {
                    return Some(p);
                }
                match (v.shape(args[0]), v.shape(results[0])) {
                    (Some((s, 1)), Some((t, n))) if s == t && n > 1 => None,
                    _ => Some("makes a vector from a scalar of its element type".into()),
                }
            }
            Op::Extract(i) | Op::Insert(i) => {
                let n = if matches!(op, Op::Extract(_)) { 1 } else { 2 };
                if let Some(p) = count(n) {
                    return Some(p);
                }
                let len = match v.data(args[0]) {
                    TypeData::Vector(_, n) => *n as u32,
                    TypeData::Matrix { cols, .. } => *cols as u32,
                    _ => return Some("takes a vector or matrix".into()),
                };
                if *i >= len {
                    return Some(format!("index {i} is out of range"));
                }
                if let Op::Insert(_) = op
                    && result_types_in(&mut Existing(self.m), &Op::Extract(*i), &args[..1], None)
                        .ok()
                        .as_deref()
                        != Some(&args[1..2])
                {
                    return Some("the inserted value has another type".into());
                }
                None
            }
            Op::Shuffle(lanes) => {
                if let Some(p) = count(2) {
                    return Some(p);
                }
                let Some((_, n)) = v.shape(args[0]).filter(|s| s.1 > 1) else {
                    return Some("shuffles vectors".into());
                };
                if args[0] != args[1] {
                    return Some("shuffles two vectors of one type".into());
                }
                if lanes.is_empty() || lanes.len() > 4 {
                    return Some("picks 1 to 4 lanes".into());
                }
                lanes
                    .iter()
                    .any(|&l| l >= 2 * n as u32)
                    .then(|| "a lane is out of range".into())
            }
            Op::ExtractDyn | Op::InsertDyn => {
                let n = if matches!(op, Op::ExtractDyn) { 2 } else { 3 };
                if let Some(p) = count(n) {
                    return Some(p);
                }
                if !matches!(v.data(args[0]), TypeData::Vector(..)) {
                    return Some("takes a vector".into());
                }
                if !index_ok(args[1]) {
                    return Some("the index is an integer scalar".into());
                }
                if n == 3 && v.shape(args[2]).map(|s| s.0) != v.shape(args[0]).map(|s| s.0) {
                    return Some("the inserted value is the vector's element type".into());
                }
                (n == 3 && !v.shape(args[2]).is_some_and(|s| s.1 == 1))
                    .then(|| "the inserted value is a scalar".into())
            }
            Op::MatMul => {
                if let Some(p) = count(2) {
                    return Some(p);
                }
                let dims = |t: Type| match v.data(t) {
                    TypeData::Matrix {
                        element,
                        rows,
                        cols,
                    } => Some((*element, Some((*rows, *cols)), 0)),
                    TypeData::Vector(s, n) => Some((*s, None, *n)),
                    _ => None,
                };
                let ok = match (dims(args[0]), dims(args[1])) {
                    (Some((a, Some((_, k1)), _)), Some((b, Some((k2, _)), _))) => {
                        a == b && k1 == k2
                    }
                    (Some((a, Some((_, c)), _)), Some((b, None, n))) => a == b && c == n,
                    (Some((a, None, n)), Some((b, Some((r, _)), _))) => a == b && n == r,
                    _ => false,
                };
                (!ok).then(|| "the shapes do not multiply".into())
            }
            Op::Transpose => count(1),
            Op::MatColumn(i) | Op::MatRow(i) | Op::MatExtract(i, _) => {
                if let Some(p) = count(1) {
                    return Some(p);
                }
                let TypeData::Matrix { rows, cols, .. } = v.data(args[0]) else {
                    return Some("takes a matrix".into());
                };
                let (r, c) = (*rows as u32, *cols as u32);
                let ok = match op {
                    Op::MatColumn(_) => *i < c,
                    Op::MatRow(_) => *i < r,
                    Op::MatExtract(row, col) => *row < r && *col < c,
                    _ => true,
                };
                (!ok).then(|| "an index is out of range".into())
            }
            Op::Construct => self.construct(results[0], args),
            Op::ExtractField(i) | Op::InsertField(i) => {
                let n = if matches!(op, Op::ExtractField(_)) {
                    1
                } else {
                    2
                };
                if let Some(p) = count(n) {
                    return Some(p);
                }
                let TypeData::Struct(s) = v.data(args[0]) else {
                    return Some("takes a struct".into());
                };
                let field = self.m.structs.get(*s)?.fields.get(*i as usize)?.1;
                (n == 2 && args[1] != field).then(|| "the inserted value has another type".into())
            }
            Op::ExtractElem(i) | Op::InsertElem(i) => {
                let n = if matches!(op, Op::ExtractElem(_)) {
                    1
                } else {
                    2
                };
                if let Some(p) = count(n) {
                    return Some(p);
                }
                let TypeData::Array(e, len) = v.data(args[0]) else {
                    return Some("takes an array".into());
                };
                if *i >= *len {
                    return Some(format!("index {i} is out of range"));
                }
                (n == 2 && args[1] != *e).then(|| "the inserted value has another type".into())
            }
            Op::Var => {
                if let Some(p) = count(0) {
                    return Some(p);
                }
                if !matches!(
                    v.data(results[0]),
                    TypeData::Ptr {
                        space: Space::Function,
                        readonly: false,
                        ..
                    }
                ) {
                    return Some("a variable is a writable `function` pointer".into());
                }
                let entry = f.entry();
                (f.block_of(inst) != entry).then(|| "variables are made in the entry block".into())
            }
            Op::Load { aligned } | Op::Store { aligned } => {
                let store = matches!(op, Op::Store { .. });
                if let Some(p) = count(if store { 2 } else { 1 }) {
                    return Some(p);
                }
                if aligned.is_some_and(|a| !a.is_power_of_two()) {
                    return Some("an alignment is a power of two".into());
                }
                let TypeData::Ptr {
                    space,
                    readonly,
                    pointee,
                } = v.data(args[0])
                else {
                    return Some("takes a pointer".into());
                };
                if !v.is_data(*pointee) {
                    return Some("only data is loaded and stored".into());
                }
                if store {
                    if *readonly || space.is_read_only() {
                        return Some(format!("`{}` memory here is read-only", space.name()));
                    }
                    if args[1] != *pointee {
                        return Some("the stored value has another type".into());
                    }
                } else if matches!(
                    space,
                    Space::Output | Space::MeshVertex | Space::MeshPrimitive | Space::MeshIndex
                ) {
                    return Some(format!("`{}` memory is write-only", space.name()));
                }
                None
            }
            Op::Access(steps) => {
                let indices = steps.iter().filter(|s| **s == AccessStep::Index).count();
                if args.len() != 1 + indices {
                    return Some("takes one operand per index step".into());
                }
                let TypeData::Ptr { pointee, .. } = v.data(args[0]) else {
                    return Some("takes a pointer".into());
                };
                let mut ty = *pointee;
                let mut next = 1;
                for step in steps {
                    if *step == AccessStep::Index {
                        if !index_ok(args[next]) {
                            return Some("an index is an integer scalar".into());
                        }
                        let len = match v.data(ty) {
                            TypeData::Array(_, n) => Some(*n as u64),
                            TypeData::Vector(_, n) => Some(*n as u64),
                            TypeData::Matrix { cols, .. } => Some(*cols as u64),
                            _ => None,
                        };
                        if let (Some(len), ValueDef::Const(c)) = (len, f.values[ops[next]].def)
                            && let Some(data) = v.v_const(c)
                            && let ConstValue::Int(bits) = data.value
                        {
                            let width = v.shape(data.ty).map_or(64, |s| s.0.bits());
                            let shift = 64 - width;
                            let value = ((bits << shift) as i64) >> shift;
                            if value < 0 || value as u64 >= len {
                                return Some(format!("index {value} is out of range"));
                            }
                        }
                        next += 1;
                    }
                    ty = crate::infer::step_type_in(&mut Existing(self.m), ty, *step).ok()?;
                }
                None
            }
            Op::BufferElem | Op::BufferByte => {
                if let Some(p) = count(2) {
                    return Some(p);
                }
                (!index_ok(args[1])).then(|| "the index is an integer scalar".into())
            }
            Op::UniformPtr | Op::BufferLength | Op::DescGet { .. } => count(1),
            Op::Atomic { op, scope, order } => {
                let n = match op {
                    AtomicOp::Load => 1,
                    AtomicOp::CmpXchg => 3,
                    _ => 2,
                };
                if let Some(p) = count(n) {
                    return Some(p);
                }
                let TypeData::Ptr {
                    space,
                    readonly,
                    pointee,
                } = v.data(args[0])
                else {
                    return Some("takes a pointer".into());
                };
                if !matches!(space, Space::GroupShared | Space::Buffer) {
                    return Some("atomics work on `groupshared` and `buffer` memory".into());
                }
                if *readonly && *op != AtomicOp::Load {
                    return Some("this memory is read-only".into());
                }
                let int = v.is_scalar(*pointee, Scalar::I32) || v.is_scalar(*pointee, Scalar::I64);
                let float = v.is_scalar(*pointee, Scalar::F32);
                let ok = match op {
                    AtomicOp::Add
                    | AtomicOp::Sub
                    | AtomicOp::Xchg
                    | AtomicOp::Load
                    | AtomicOp::Store => int || float,
                    _ => int,
                };
                if !ok {
                    return Some(format!(
                        "`atomic.{}` does not take `{}`",
                        op.name(),
                        v.show(*pointee)
                    ));
                }
                if args[1..].iter().any(|a| a != pointee) {
                    return Some("the values have the memory's type".into());
                }
                if *scope == Scope::Lane {
                    return Some("an atomic's scope is wave, group or device".into());
                }
                let bad_order = match op {
                    AtomicOp::Load => matches!(order, MemOrder::Release | MemOrder::AcqRel),
                    AtomicOp::Store => matches!(order, MemOrder::Acquire | MemOrder::AcqRel),
                    _ => false,
                };
                bad_order.then(|| format!("`{}` is not an ordering for this", order.name()))
            }
            Op::Barrier { exec, mem, spaces } => {
                if let Some(p) = count(0) {
                    return Some(p);
                }
                if matches!(exec, Some(Scope::Lane | Scope::Device)) {
                    return Some("an execution barrier is for a wave or a group".into());
                }
                if *mem == Scope::Lane {
                    return Some("a memory barrier's scope is wave, group or device".into());
                }
                if spaces
                    .iter()
                    .any(|s| !matches!(s, Space::GroupShared | Space::Buffer))
                {
                    return Some("a barrier orders `groupshared` and `buffer` memory".into());
                }
                (spaces.is_empty() && exec.is_none())
                    .then(|| "a barrier waits for something or orders something".into())
            }
            Op::Discard | Op::IsHelper => count(0),
            Op::Deriv { .. } => {
                if let Some(p) = count(1) {
                    return Some(p);
                }
                (!v.is_float(args[0], false)).then(|| "takes floats".into())
            }
            Op::Wave { op, cluster } => {
                if cluster.is_some_and(|n| !n.is_power_of_two() || n > 128) {
                    return Some("a cluster is a power of two up to 128".into());
                }
                self.wave(*op, args)
            }
            Op::Call(callee) => {
                let callee = &self.m.funcs[*callee];
                let params: Vec<Type> = callee.params().iter().map(|&p| callee.ty(p)).collect();
                if callee.layout.is_empty() {
                    return Some("calls a function without a body".into());
                }
                (params != args).then(|| "the arguments do not match the parameters".into())
            }
            Op::DebugPrint(fmt) | Op::DebugAssert(fmt) => {
                if *fmt as usize >= self.m.formats.len() {
                    return Some(format!("no format {fmt}"));
                }
                if let Op::DebugAssert(_) = op
                    && !args.first().is_some_and(|&a| v.is_scalar(a, Scalar::I1))
                {
                    return Some("the condition is `i1`".into());
                }
                args.iter()
                    .any(|&a| !v.is_data(a))
                    .then(|| "prints data, not pointers or handles".into())
            }
            Op::Return => (args != f.results.as_slice())
                .then(|| "returns other types than the function's".into()),
            Op::Unreachable => count(0),
            Op::Br(_) | Op::CondBr { .. } | Op::Switch { .. } | Op::Loop { .. } => {
                self.terminator(inst, op, ops, args)
            }
        }
    }

    fn math(&self, op: MathOp, args: &[Type]) -> Option<String> {
        use MathOp::*;
        let v = &*self.v;
        let same = args.windows(2).all(|w| w[0] == w[1]);
        let float = |t: Type| v.is_float(t, false);
        let fvec = |t: Type| v.shape(t).is_some_and(|(s, n)| s.is_float() && n > 1);
        let int = |t: Type| v.is_int(t, false);
        let i32_scalar = |t: Type| v.is_scalar(t, Scalar::I32);
        let ok = match op {
            Abs | Sign => (float(args[0]) || int(args[0])) && same,
            SMin | SMax | UMin | UMax | Popcount | FirstBitHiS | FirstBitHiU | FirstBitLo
            | BitReverse => int(args[0]) && same,
            BitfieldExtractS | BitfieldExtractU => {
                int(args[0]) && i32_scalar(args[1]) && i32_scalar(args[2])
            }
            BitfieldInsert => {
                int(args[0]) && args[0] == args[1] && i32_scalar(args[2]) && i32_scalar(args[3])
            }
            Dot => {
                same && v
                    .shape(args[0])
                    .is_some_and(|(s, n)| s != Scalar::I1 && n > 1)
            }
            Cross => {
                same && v
                    .shape(args[0])
                    .is_some_and(|(s, n)| s.is_float() && n == 3)
            }
            Length | Normalize => fvec(args[0]),
            Distance | Reflect | FaceForward => same && fvec(args[0]),
            Refract => {
                fvec(args[0])
                    && args[0] == args[1]
                    && v.shape(args[2]) == v.shape(args[0]).map(|(s, _)| (s, 1))
            }
            IsNan | IsInf => float(args[0]),
            _ => same && float(args[0]),
        };
        (!ok).then(|| format!("`{}` does not take these operand types", op.name()))
    }

    fn wave(&self, op: WaveOp, args: &[Type]) -> Option<String> {
        let v = &*self.v;
        let numeric = |t: Type| v.shape(t).is_some_and(|(s, _)| s != Scalar::I1);
        let lane = |t: Type| v.is_scalar(t, Scalar::I32);
        let n = match op {
            WaveOp::LaneIndex | WaveOp::LaneCount | WaveOp::IsFirst => 0,
            WaveOp::Broadcast | WaveOp::Shuffle | WaveOp::Quad(QuadOp::ReadLane) => 2,
            _ => 1,
        };
        if args.len() != n {
            return Some(format!("takes {n} operand(s)"));
        }
        let ok = match op {
            WaveOp::Reduce(r) | WaveOp::Scan { op: r, .. } => match r {
                ReduceOp::FAdd | ReduceOp::FMul | ReduceOp::FMin | ReduceOp::FMax => {
                    v.is_float(args[0], false)
                }
                ReduceOp::And | ReduceOp::Or | ReduceOp::Xor => v.is_int(args[0], true),
                _ => v.is_int(args[0], false),
            },
            WaveOp::Ballot | WaveOp::Vote(VoteOp::Any | VoteOp::All) => {
                v.is_scalar(args[0], Scalar::I1)
            }
            WaveOp::Broadcast | WaveOp::Shuffle | WaveOp::Quad(QuadOp::ReadLane) => {
                numeric(args[0]) && lane(args[1])
            }
            WaveOp::Quad(_) | WaveOp::Vote(VoteOp::Equal) | WaveOp::Match => numeric(args[0]),
            WaveOp::LaneIndex | WaveOp::LaneCount | WaveOp::IsFirst => true,
        };
        (!ok).then(|| "this wave operation does not take these operand types".into())
    }

    fn construct(&self, result: Type, args: &[Type]) -> Option<String> {
        let v = &*self.v;
        let ok = match v.data(result) {
            TypeData::Vector(s, n) => {
                let mut total = 0u32;
                let mut ok = true;
                for &a in args {
                    match v.shape(a) {
                        Some((t, k)) if t == *s => total += k as u32,
                        _ => ok = false,
                    }
                }
                ok && total == *n as u32
            }
            TypeData::Matrix {
                element,
                rows,
                cols,
            } => {
                args.len() == *cols as usize
                    && args.iter().all(|&a| v.shape(a) == Some((*element, *rows)))
            }
            TypeData::Array(e, n) => args.len() == *n as usize && args.iter().all(|a| a == e),
            TypeData::Struct(s) => {
                let fields = &self.m.structs.get(*s)?.fields;
                fields.len() == args.len() && fields.iter().zip(args).all(|(f, a)| f.1 == *a)
            }
            _ => return Some("builds vectors, matrices, arrays and structs".into()),
        };
        (!ok).then(|| "the parts do not make this type".into())
    }

    fn terminator(&self, inst: Inst, op: &Op, ops: &[Value], args: &[Type]) -> Option<String> {
        let f = self.f;
        let v = &*self.v;
        match op {
            Op::CondBr { then_args, .. } => {
                if !args.first().is_some_and(|&a| v.is_scalar(a, Scalar::I1)) {
                    return Some("the condition is `i1`".into());
                }
                if 1 + *then_args as usize > ops.len() {
                    return Some("the branch arguments do not add up".into());
                }
            }
            Op::Switch { cases, .. } => {
                if !args
                    .first()
                    .is_some_and(|&a| v.is_int(a, false) && v.shape(a).is_some_and(|s| s.1 == 1))
                {
                    return Some("switches on an integer scalar".into());
                }
                let mut values = HashSet::new();
                if cases.iter().any(|c| !values.insert(c.value)) {
                    return Some("two cases have one value".into());
                }
                let total = 1
                    + cases.iter().map(|c| c.args as usize).sum::<usize>()
                    + match op {
                        Op::Switch { default_args, .. } => *default_args as usize,
                        _ => 0,
                    };
                if total != ops.len() {
                    return Some("the case arguments do not add up".into());
                }
            }
            _ => {}
        }
        let entry = f.entry();
        for (target, range) in op.successors(ops.len()) {
            if Some(target) == entry {
                return Some("nothing jumps to the entry block".into());
            }
            let params: Vec<Type> = f.blocks[target].params.iter().map(|&p| f.ty(p)).collect();
            let passed = args.get(range).unwrap_or(&[]);
            if params.as_slice() != passed {
                return Some(format!(
                    "{} takes ({}), not ({})",
                    self.block_name(target),
                    params
                        .iter()
                        .map(|t| v.show(*t))
                        .collect::<Vec<_>>()
                        .join(", "),
                    passed
                        .iter()
                        .map(|t| v.show(*t))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        if let Op::Loop { header, .. } = op
            && f.block_of(inst) == Some(*header)
        {
            return Some("a loop's header is not the block that enters it".into());
        }
        None
    }

    // ---- dominance ----

    fn real_succs(&self) -> Vec<Vec<usize>> {
        let f = self.f;
        f.layout
            .iter()
            .map(|&b| match f.terminator(b) {
                Some(t) => f.insts[t]
                    .op
                    .successors(f.operands(t).len())
                    .into_iter()
                    .filter_map(|(s, _)| self.index.get(&s).copied())
                    .collect(),
                None => Vec::new(),
            })
            .collect()
    }

    /// Definitions dominate uses (on the CFG; uses in unreachable blocks are not checked).
    fn dominance(&mut self, insts: &[Inst]) {
        let f = self.f;
        let n = f.layout.len();
        let idom = dominators(n, 0, &self.real_succs());
        for &inst in insts {
            let (b, k) = self.position[&inst];
            if idom[b].is_none() {
                continue;
            }
            for &v in f.operands(inst) {
                let ok = match f.values[v].def {
                    ValueDef::Result(d, _) => match self.position.get(&d) {
                        Some(&(db, dk)) if db == b => dk < k,
                        Some(&(db, _)) => dominates(&idom, db, b),
                        None => false,
                    },
                    ValueDef::Param(pb, _) => self
                        .index
                        .get(&pb)
                        .is_some_and(|&pb| dominates(&idom, pb, b)),
                    ValueDef::Const(_) | ValueDef::Global(_) => true,
                };
                if !ok {
                    let name = self
                        .text
                        .as_ref()
                        .map_or_else(|| "a value".into(), |t| t.value(v));
                    self.error_at(
                        inst,
                        format!("`{name}` is used where its definition does not dominate"),
                    );
                }
            }
        }
    }

    // ---- structure ----

    fn structure(&mut self) {
        let f = self.f;
        let n = f.layout.len();
        let real = self.real_succs();
        // Regions, and the structural edges they add.
        let mut regions = Vec::new();
        let mut structural = real.clone();
        for (bi, &b) in f.layout.iter().enumerate() {
            let Some(t) = f.terminator(b) else { continue };
            match &f.insts[t].op {
                Op::CondBr { merge, .. } | Op::Switch { merge, .. } => {
                    let kind = if matches!(f.insts[t].op, Op::Switch { .. }) {
                        Kind::Switch
                    } else {
                        Kind::Selection
                    };
                    let m = self.index[merge];
                    if m == bi {
                        self.error_at(t, "a region's merge block is not its header");
                        continue;
                    }
                    structural[bi].push(m);
                    regions.push(Region {
                        kind,
                        header: bi,
                        merge: m,
                        cont: None,
                        owner: bi,
                        blocks: Vec::new(),
                    });
                }
                Op::Loop {
                    header,
                    continuing,
                    merge,
                } => {
                    let (h, c, m) = (
                        self.index[header],
                        self.index[continuing],
                        self.index[merge],
                    );
                    if m == h || c == m {
                        self.error_at(t, "a loop's header, continue and merge blocks differ (the continue block may be the header)");
                        continue;
                    }
                    structural[h].push(m);
                    structural[h].push(c);
                    regions.push(Region {
                        kind: Kind::Loop,
                        header: h,
                        merge: m,
                        cont: Some(c),
                        owner: bi,
                        blocks: Vec::new(),
                    });
                }
                _ => {}
            }
        }
        if regions.is_empty() {
            return;
        }
        let sdom = dominators(n, 0, &structural);
        let depth = |mut b: usize| {
            let mut d = 0;
            while let Some(p) = sdom[b].filter(|&p| p != b) {
                b = p;
                d += 1;
            }
            d
        };

        // A block is the merge of one region, the continue block of one loop, not both.
        let mut merges = vec![0u32; n];
        let mut conts = vec![0u32; n];
        for r in &regions {
            merges[r.merge] += 1;
            if let Some(c) = r.cont {
                conts[c] += 1;
            }
        }
        for b in 0..n {
            if merges[b] > 1 || conts[b] > 1 || (merges[b] > 0 && conts[b] > 0) {
                self.error(format!(
                    "{} is the merge or continue block of more than one region",
                    self.block_name(f.layout[b])
                ));
            }
        }
        let mut headers = HashSet::new();
        for r in regions.iter().filter(|r| r.kind == Kind::Loop) {
            if !headers.insert(r.header) {
                self.error(format!(
                    "{} is the header of two loops",
                    self.block_name(f.layout[r.header])
                ));
            }
        }

        // Region blocks, outermost first; a loop before a selection with the same header.
        // A region inside a loop also stops at the loop's continue block (unless it is in the
        // continue block's part of the loop), as SPIR-V's constructs do.
        regions.sort_by_key(|r| (depth(r.header), r.kind != Kind::Loop));
        let mut innermost_loop: Vec<Option<usize>> = Vec::with_capacity(regions.len());
        for i in 0..regions.len() {
            let (header, merge) = (regions[i].header, regions[i].merge);
            let parent = (0..i)
                .filter(|&j| regions[j].blocks[header])
                .min_by_key(|&j| regions[j].blocks.iter().filter(|x| **x).count());
            let around = parent.and_then(|p| innermost_loop[p]);
            let stop = around
                .and_then(|l| regions[l].cont)
                .filter(|&c| !dominates(&sdom, c, header));
            let blocks: Vec<bool> = (0..n)
                .map(|b| {
                    sdom[b].is_some()
                        && dominates(&sdom, header, b)
                        && !dominates(&sdom, merge, b)
                        && parent.is_none_or(|p| regions[p].blocks[b])
                        && stop.is_none_or(|c| !dominates(&sdom, c, b))
                })
                .collect();
            regions[i].blocks = blocks;
            innermost_loop.push(if regions[i].kind == Kind::Loop {
                Some(i)
            } else {
                around
            });
        }

        let mut preds = vec![Vec::new(); n];
        for (b, ss) in real.iter().enumerate() {
            if sdom[b].is_none() {
                continue;
            }
            for &s in ss {
                preds[s].push(b);
            }
        }
        let name = |s: &Self, b: usize| s.block_name(f.layout[b]);
        let mut problems = Vec::new();
        for (i, r) in regions.iter().enumerate() {
            let what = match r.kind {
                Kind::Loop => "loop",
                Kind::Selection => "selection",
                Kind::Switch => "switch",
            };
            let header = name(self, r.header);
            for b in (0..n).filter(|&b| r.blocks[b]) {
                // Entered only through the header.
                if b != r.header {
                    for &p in &preds[b] {
                        if !r.blocks[p] {
                            problems.push(format!(
                                "{} jumps into the middle of the {what} at {header} (to {})",
                                name(self, p),
                                name(self, b)
                            ));
                        }
                    }
                }
                // Left only through the merge, a break or a continue.
                for &s in &real[b] {
                    if r.blocks[s] || s == r.merge {
                        continue;
                    }
                    let escape = regions.iter().any(|o| {
                        o.blocks[b]
                            && ((o.kind == Kind::Loop && (s == o.merge || Some(s) == o.cont))
                                || (o.kind == Kind::Switch && s == o.merge))
                    });
                    if !escape {
                        problems.push(format!(
                            "{} leaves the {what} at {header} for {}, which is not its merge, a \
                             break or a continue",
                            name(self, b),
                            name(self, s)
                        ));
                    }
                }
            }
            if r.kind == Kind::Loop {
                let c = r.cont.unwrap_or(r.header);
                for &p in &preds[r.header] {
                    if !r.blocks[p] && p != r.owner {
                        problems.push(format!(
                            "{} enters the loop at {header} without its `loop` instruction",
                            name(self, p)
                        ));
                    }
                    if r.blocks[p] && !dominates(&sdom, c, p) {
                        problems.push(format!(
                            "{} jumps back to {header} from outside the continue block",
                            name(self, p)
                        ));
                    }
                }
                if sdom[c].is_some() && !r.blocks[c] {
                    problems.push(format!(
                        "the continue block {} is not inside the loop at {header}",
                        name(self, c)
                    ));
                }
            }
            // Regions nest.
            for o in &regions[i + 1..] {
                let both = (0..n).any(|b| r.blocks[b] && o.blocks[b]);
                let r_in_o = (0..n).all(|b| !r.blocks[b] || o.blocks[b]);
                let o_in_r = (0..n).all(|b| !o.blocks[b] || r.blocks[b]);
                if both && !r_in_o && !o_in_r {
                    problems.push(format!(
                        "the regions at {header} and {} overlap without nesting",
                        name(self, o.header)
                    ));
                }
            }
        }
        problems.sort();
        problems.dedup();
        for p in problems {
            self.error(p);
        }
    }
}
