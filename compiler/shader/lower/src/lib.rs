//! Lowering type-checked HIR to IR (`docs/design/0005-shader-compiler.md` step 7, 0006 §2):
//! each instance (a function with concrete type and constant arguments) goes straight from its
//! HIR body and `InferenceResult` to a concrete IR function in SSA form.
//!
//! Step 7a, here so far: non-generic code.
//! - Types: scalars (signless, the operations carry the sign), vectors, matrices, arrays,
//!   structs (one IR struct per instantiated type), enums (their underlying type), tuples,
//!   resource indices.
//! - Functions: SSA built directly with Braun et al.'s algorithm (variables become block
//!   parameters where paths meet); structured terminators; `in` by value, `out` and `ref` as
//!   pointers to a caller's variable (copy-in, copy-out); instance methods take `this` by value
//!   (`readonly`) or by pointer; constructors return the value they build.
//! - Expressions and statements, with C#'s evaluation order, short-circuit `&&`/`||`, `?:`,
//!   patterns, `switch`, loops with `break` and `continue`.
//! - Calls to declared functions, `[Intrinsic("op")]` functions as IR operations (0004 §17.2).
//! - Compile-time evaluation of `const` fields, enum members and field defaults: their
//!   initializers are lowered and run in the IR interpreter (0006 §11).
//!
//! Step 7b: instances.
//! - Generic functions and types, with type and constant arguments, one IR function or struct
//!   per instance; compile-time recursion (`Fact<N - 1>`) up to a bound on instances.
//! - Interface members and operators called on concrete types resolve to the implementing
//!   member, or to the built-in operator for scalars and vectors (0004 §8.5).
//! - Properties, indexers and extension members call their accessor bodies.
//! - Lambdas, local functions and method groups passed to `Func`/`Action` parameters, with
//!   captures by value in an environment struct (0004 §9.2, 0006 §2).
//!
//! Step 7c: entry points and compile-time evaluation.
//! - [`Lowering::entry`]: a compute entry (generic ones with type arguments, 0004 §11.4) as an
//!   IR entry that loads its system values and `[Push]` struct from module variables and calls
//!   the entry function's instance (0006 §6.3).
//! - `[GroupShared]` fields as module variables; they and buffer elements are places in
//!   memory, read and written through pointers (`access`, `buffer.elem`) one part at a time;
//!   buffer and groupshared atomics; `b.Length`.
//! - [`Lowering::set_static`]: static parameters, `const` fields of structs a request
//!   overrides (0004 §13.3); constants that depend on them follow.
//! - Compile-time evaluation calls only `const` functions (0004 §13.4); thread-group sizes and
//!   array lengths may be constants.
//!
//! Step 8c: instructions carry the source location of what they were lowered from, for checks
//! after inlining; `[Uniform]` locals assert what is written to them (`assume.uniform`).
//!
//! Step 9c: local `const`s are evaluated at compile time like `const` fields (their uses are
//! constants; an initializer that reads a runtime value is E1306); an entry's `[Export]`
//! constants ([`Lowering::exports`]) and the static parameters used
//! ([`Lowering::static_params`]) are kept for the reflection.
//!
//! Step 9d, for the core library: static properties read through a type (`Wave.LaneIndex`,
//! and an interface's `T.Zero` as the concrete type's member, also on built-in types);
//! intrinsics that pick their operation by the operand's kind (`min` is `fmin`, `smin` or
//! `umin`; E1309 when a kind has none); `const.zero`/`const.one`; `Uniform.Value` and byte
//! buffers' `Load`/`Store`.
//!
//! Step 10a: vertex and pixel entries. Each field of a stage I/O struct parameter or result
//! becomes a module variable (a system value, a render target with its blend source, or the
//! next location in declaration order, integers `[Flat]`); a pixel entry returning a scalar or
//! vector writes target 0.
//!
//! Step 10b: textures (`texture.rs`): `Sample`, `Load`, indexers and storage writes as
//! `tex.*` operations, their optional arguments passed by name only when given; implicit
//! levels only in pixel entries (E1309); bitcasts; byte buffers' `Load2`-`Store4`.
//!
//! Step 10c: mesh and amplification entries (`mesh.rs`). A `MeshOutput` parameter becomes
//! output arrays (one per field of the vertex and primitive types, and the index list); the
//! output and its lists are constant markers that writes resolve to those arrays. The field an
//! amplification entry dispatches lives in `payload` memory; `Mesh.Dispatch` must end the
//! entry's body (the first version's rule). Writes before `SetCounts` are E1308.
//!
//! Not lowered yet: auto-properties.

mod closures;
mod ctfe;
mod dispatch;
mod entry;
mod error;
mod expr;
mod func;
mod intrinsic;
mod members;
mod mesh;
mod pat;
mod place;
mod ssa;
mod stmt;
mod texture;
mod types;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use marlin_front_hir_def::store::BindingId;
use marlin_front_hir_def::{
    BodyOwner, BodyPart, FilePackage, ItemId, ItemKind, ItemTreeQuery, item_path,
};
use marlin_lang_query::Cx;
use marlin_shaderc_hir_ty::{Builtins, BuiltinsQuery, Subst, Ty};
use marlin_shaderc_ir::{FuncId, GlobalId, Module, StructId, Type};

pub use error::LowerError;

pub(crate) type R<T> = Result<T, LowerError>;

/// A value computed at compile time: an `[Export]` constant (`item` is its entry point,
/// `name` the local's name) or a static parameter (`item` is the field, `name` its path).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Evaluated {
    pub item: ItemId,
    pub name: String,
    pub value: marlin_shaderc_ir::Val,
    pub ty: Ty,
}

/// A function body with concrete arguments for every generic parameter in scope, and the
/// lambdas its `Func`/`Action` parameters are bound to (0004 §9.2: one instance per lambda).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Instance {
    pub owner: BodyOwner,
    pub subst: Subst,
    /// By parameter index: the lambda or local function instance each `Func`/`Action`
    /// parameter calls.
    pub closures: Vec<(u32, Instance)>,
    /// A lambda or local function inside `owner`'s body, rather than the body itself.
    pub inner: Option<closures::Inner>,
}

/// Lowers functions into one module. Functions are declared when first referenced and their
/// bodies lowered by [`Lowering::finish`] (or [`Lowering::run`]).
pub struct Lowering<'a, 'b> {
    pub(crate) cx: &'a Cx<'b>,
    pub(crate) builtins: Arc<Builtins>,
    pub module: Module,
    pub(crate) types: HashMap<Ty, Type>,
    /// Instance struct types: their IR struct, and the field items in IR field order.
    pub(crate) structs: HashMap<Ty, (StructId, Vec<ItemId>)>,
    pub(crate) funcs: HashMap<Instance, FuncId>,
    pub(crate) queue: Vec<(Instance, FuncId)>,
    pub(crate) names: HashMap<String, FuncId>,
    /// Compile-time values of constants, enum members and field defaults.
    pub(crate) consts: HashMap<ItemId, ctfe::ConstValue>,
    /// Constants being evaluated, to report cycles.
    pub(crate) evaluating: Vec<ItemId>,
    /// The values of local `const`s, by the body (with its generic arguments) and binding.
    pub(crate) local_consts: HashMap<(BodyOwner, Subst, BindingId), ctfe::ConstValue>,
    /// `[Export]` constants of entry points lowered, in order: entry item, name, value.
    pub(crate) exports: Vec<(ItemId, String, ctfe::ConstValue)>,
    /// Local `const`s being evaluated, to report cycles.
    pub(crate) evaluating_locals: Vec<(BodyOwner, Subst, BindingId)>,
    /// The environment struct of each lambda and local function instance.
    pub(crate) envs: HashMap<Instance, Type>,
    /// Static parameters set by the request (0004 §13.3).
    pub(crate) statics: HashSet<ItemId>,
    /// The module variable of each `[GroupShared]` field used.
    pub(crate) shared: HashMap<ItemId, GlobalId>,
    /// Lowering for compile-time evaluation: only `const` functions may be called.
    pub(crate) ctfe: bool,
    /// The index of each source location in the module's table.
    pub(crate) locs: HashMap<marlin_shaderc_ir::SourceLoc, u32>,
    /// The output variables of the mesh entry being lowered.
    pub(crate) mesh: Option<entry::MeshOutputs>,
    /// The `[GroupShared]` field an amplification entry dispatches, which lives in `payload`
    /// memory.
    pub(crate) payload_field: Option<ItemId>,
}

impl<'a, 'b> Lowering<'a, 'b> {
    /// A lowering for code of `package` (whose core library the built-in types come from).
    pub fn new(cx: &'a Cx<'b>, package: marlin_front_hir_def::PackageId) -> R<Self> {
        Ok(Lowering {
            cx,
            builtins: cx.get::<BuiltinsQuery>(&package)?,
            module: Module::new(),
            types: HashMap::new(),
            structs: HashMap::new(),
            funcs: HashMap::new(),
            queue: Vec::new(),
            names: HashMap::new(),
            consts: HashMap::new(),
            evaluating: Vec::new(),
            local_consts: HashMap::new(),
            exports: Vec::new(),
            evaluating_locals: Vec::new(),
            envs: HashMap::new(),
            statics: HashSet::new(),
            shared: HashMap::new(),
            ctfe: false,
            locs: HashMap::new(),
            mesh: None,
            payload_field: None,
        })
    }

    /// A lowering for the package of `item`'s file.
    pub fn for_item(cx: &'a Cx<'b>, item: ItemId) -> R<Self> {
        let package = cx.input::<FilePackage>(&item.file)?;
        Self::new(cx, package)
    }

    /// The IR function for the non-generic function `item`, declared now, lowered by `run`.
    pub fn function(&mut self, item: ItemId) -> R<FuncId> {
        self.instance(Instance {
            owner: BodyOwner {
                item,
                part: BodyPart::Main,
            },
            subst: Subst::new(),
            closures: Vec::new(),
            inner: None,
        })
    }

    /// Lowers every declared function that has no body yet (including the ones they call).
    pub fn run(&mut self) -> R<()> {
        while let Some((instance, id)) = self.queue.pop() {
            func::lower_body(self, &instance, id)?;
        }
        Ok(())
    }

    /// The `[Export]` constants of the entry points lowered so far, in source order (0004
    /// §13.4).
    pub fn exports(&self) -> Vec<Evaluated> {
        self.exports
            .iter()
            .map(|(entry, name, v)| Evaluated {
                item: *entry,
                name: name.clone(),
                value: v.val.clone(),
                ty: v.ty.clone(),
            })
            .collect()
    }

    /// The static parameters (`const` fields of structs, 0004 §13.3) whose values the code
    /// lowered so far used, set by the request or not, by path.
    pub fn static_params(&self) -> R<Vec<Evaluated>> {
        let mut out = Vec::new();
        for (&item, v) in &self.consts {
            let tree = self.cx.get::<ItemTreeQuery>(&item.file)?;
            let data = &tree.items[item.local];
            let in_struct = data
                .parent
                .is_some_and(|p| tree.items[p].kind == ItemKind::Struct);
            if data.kind == ItemKind::Field && data.is_const && in_struct {
                out.push(Evaluated {
                    item,
                    name: item_path(self.cx, item)?.to_string(),
                    value: v.val.clone(),
                    ty: v.ty.clone(),
                });
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// The front-end type an IR struct was made from, and its field declarations in IR field
    /// order.
    pub fn struct_source(&self, s: StructId) -> Option<(Ty, Vec<ItemId>)> {
        self.structs
            .iter()
            .find(|(_, (id, _))| *id == s)
            .map(|(ty, (_, fields))| (ty.clone(), fields.clone()))
    }

    /// Lowers what is left and gives the module.
    pub fn finish(mut self) -> R<Module> {
        self.run()?;
        self.entry_uses();
        self.implicit_levels()?;
        if let Some(f) = marlin_shaderc_ir::early_mesh_writes(&self.module).first() {
            return Err(LowerError::new(
                "E1308",
                format!("`{f}` writes a mesh output before `SetCounts`"),
            ));
        }
        Ok(self.module)
    }

    /// A sample without `lod:` or `grad:` takes its level from derivatives, which only pixel
    /// entries have (0004 §10.4): an error in code a module's other entries run.
    fn implicit_levels(&self) -> R<()> {
        use marlin_shaderc_ir::{Op, Stage as IrStage, TexArg};
        if self
            .module
            .entries
            .iter()
            .all(|e| e.stage == IrStage::Pixel)
        {
            return Ok(());
        }
        for (_, f) in self.module.funcs.iter() {
            for &b in &f.layout {
                for inst in f.insts(b) {
                    if let Op::TexSample(a) = &f.insts[inst].op
                        && !a.contains(&TexArg::Lod)
                        && !a.contains(&TexArg::Grad)
                    {
                        return Err(LowerError::new(
                            "E1309",
                            format!(
                                "`{}` samples without `lod:` or `grad:`, which only pixel \
                                 entries can (their level comes from derivatives)",
                                f.name
                            ),
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    /// The IR function of an instance, declared (and queued) the first time.
    pub(crate) fn instance(&mut self, instance: Instance) -> R<FuncId> {
        let instance = self.normalize(instance)?;
        if let Some(&id) = self.funcs.get(&instance) {
            return Ok(id);
        }
        if self.funcs.len() >= MAX_INSTANCES {
            return Err(LowerError::new(
                "E1307",
                format!(
                    "more than {MAX_INSTANCES} function instances: compile-time recursion that does not end?"
                ),
            ));
        }
        let id = func::declare(self, &instance)?;
        self.funcs.insert(instance.clone(), id);
        self.queue.push((instance, id));
        Ok(id)
    }

    /// A unique function name: the item's path, with the parameter types when overloads share
    /// it.
    pub(crate) fn function_name(
        &mut self,
        instance: &Instance,
        params: &[Type],
        id: FuncId,
    ) -> R<String> {
        let base = self.instance_base(instance)?;
        let mut name = base.clone();
        if self.names.contains_key(&name) {
            let shown: Vec<String> = params
                .iter()
                .map(|&t| marlin_shaderc_ir::type_name(&self.module, t))
                .collect();
            name = format!("{base}({})", shown.join(", "));
            let mut k = 1;
            while self.names.contains_key(&name) {
                name = format!("{base}#{k}");
                k += 1;
            }
        }
        self.names.insert(name.clone(), id);
        Ok(name)
    }

    /// An instance's name before overloads are told apart: the item's path, its type
    /// arguments, then the lambda or local function (`Game.Outer<float>.lambda`).
    pub(crate) fn instance_base(&self, instance: &Instance) -> R<String> {
        let item = instance.owner.item;
        let tree = self.cx.get::<ItemTreeQuery>(&item.file)?;
        let mut base = match tree.items[item.local].kind {
            ItemKind::Constructor => format!("{}.ctor", item_path(self.cx, item)?),
            _ => item_path(self.cx, item)?.to_string(),
        };
        let args: Vec<String> = instance
            .subst
            .entries
            .iter()
            .flat_map(|(_, args)| args.iter().flatten())
            .map(|a| match a {
                marlin_shaderc_hir_ty::GenericArgTy::Type(t) => self.show(t),
                marlin_shaderc_hir_ty::GenericArgTy::Const(c) => {
                    marlin_shaderc_hir_ty::display_const(self.cx, c).unwrap_or_else(|_| "?".into())
                }
            })
            .collect();
        if !args.is_empty() {
            base = format!("{base}<{}>", args.join(", "));
        }
        if let Some(inner) = &instance.inner {
            base = format!("{base}.{}", self.inner_name(instance, inner)?);
        }
        Ok(base)
    }
}

/// More function instances than this in one module means compile-time recursion that does
/// not end (0004 §6).
const MAX_INSTANCES: usize = 4096;

impl Lowering<'_, '_> {
    /// The instance with arguments for exactly the generic parameters in scope of its item
    /// (its own and those of the types around it), outermost first; an error when one is
    /// missing.
    fn normalize(&self, mut instance: Instance) -> R<Instance> {
        let item = instance.owner.item;
        let tree = self.cx.get::<ItemTreeQuery>(&item.file)?;
        let mut chain = Vec::new();
        let mut at = Some(item.local);
        while let Some(local) = at {
            chain.push(ItemId {
                file: item.file,
                local,
            });
            at = tree.items[local].parent;
        }
        chain.reverse();
        let mut entries = Vec::new();
        for owner in chain {
            if tree.items[owner.local].generic_params.is_empty() {
                continue;
            }
            let args = instance
                .subst
                .entries
                .iter()
                .find(|(o, _)| *o == owner)
                .map(|(_, a)| a.clone())
                .filter(|a| a.iter().all(Option::is_some));
            let Some(args) = args else {
                let path = item_path(self.cx, owner)?;
                return Err(LowerError::new(
                    "E1307",
                    format!("the generic arguments of `{path}` are not known here"),
                ));
            };
            entries.push((owner, args));
        }
        instance.subst = Subst { entries };
        Ok(instance)
    }
}

impl Lowering<'_, '_> {
    /// The module's index for the source of `loc` in `owner`'s body, if it has one.
    pub(crate) fn source_loc(
        &mut self,
        owner: BodyOwner,
        loc: &marlin_shaderc_hir_ty::InferLoc,
    ) -> R<Option<u32>> {
        use marlin_shaderc_hir_ty::InferLoc;
        let (_, map) = self
            .cx
            .get::<marlin_front_hir_def::BodyWithSourceMap>(&owner)?;
        let ptr = match loc {
            InferLoc::Expr(e) => map.exprs.get(*e).copied(),
            InferLoc::Stmt(s) => map.stmts.get(*s).copied(),
            _ => None,
        };
        let Some(ptr) = ptr else { return Ok(None) };
        let source = marlin_shaderc_ir::SourceLoc {
            file: owner.item.file.0,
            start: ptr.range.start().0,
            end: ptr.range.end().0,
        };
        if let Some(&i) = self.locs.get(&source) {
            return Ok(Some(i));
        }
        let i = self.module.locs.len() as u32;
        self.module.locs.push(source);
        self.locs.insert(source, i);
        Ok(Some(i))
    }
}
