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

mod ctfe;
mod error;
mod expr;
mod func;
mod intrinsic;
mod pat;
mod ssa;
mod stmt;
mod types;

use std::collections::HashMap;
use std::sync::Arc;

use marlin_front_hir_def::{
    BodyOwner, BodyPart, FilePackage, ItemId, ItemKind, ItemTreeQuery, item_path,
};
use marlin_lang_query::Cx;
use marlin_shaderc_hir_ty::{Builtins, BuiltinsQuery, Subst, Ty};
use marlin_shaderc_ir::{FuncId, Module, StructId, Type};

pub use error::LowerError;

pub(crate) type R<T> = Result<T, LowerError>;

/// A function body with concrete arguments for every generic parameter in scope.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Instance {
    pub owner: BodyOwner,
    pub subst: Subst,
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
        })
    }

    /// Lowers every declared function that has no body yet (including the ones they call).
    pub fn run(&mut self) -> R<()> {
        while let Some((instance, id)) = self.queue.pop() {
            func::lower_body(self, &instance, id)?;
        }
        Ok(())
    }

    /// Lowers what is left and gives the module.
    pub fn finish(mut self) -> R<Module> {
        self.run()?;
        Ok(self.module)
    }

    /// The IR function of an instance, declared (and queued) the first time.
    pub(crate) fn instance(&mut self, instance: Instance) -> R<FuncId> {
        if let Some(&id) = self.funcs.get(&instance) {
            return Ok(id);
        }
        let id = func::declare(self, &instance)?;
        self.funcs.insert(instance.clone(), id);
        self.queue.push((instance, id));
        Ok(id)
    }

    /// A unique function name: the item's path, with the parameter types when overloads share
    /// it.
    pub(crate) fn function_name(&mut self, item: ItemId, params: &[Type], id: FuncId) -> R<String> {
        let tree = self.cx.get::<ItemTreeQuery>(&item.file)?;
        let base = match tree.items[item.local].kind {
            ItemKind::Constructor => format!("{}.ctor", item_path(self.cx, item)?),
            _ => item_path(self.cx, item)?.to_string(),
        };
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
}
