//! One request through the pipeline (E15xx are the session's own errors):
//! - E1501 an unknown target; E1502 a path that names nothing; E1503 not a static parameter,
//!   or a value its type does not fit;
//! - E1504 a capability the entry needs and the target lacks (0004 §12.3);
//! - E1505 the backend cannot write the entry; E1506 a compiler bug (a module that does not
//!   verify, code the backend's validator rejects).

use std::sync::Arc;

use marlin_front_hir_def::{
    FilePackage, ItemId, ItemKind, ItemTree, ItemTreeQuery, LocalItemId, SignatureWithSourceMap,
    Visibility, item_path,
};
use marlin_lang_diag::{Code, Diagnostic, Severity, Span};
use marlin_lang_query::{Cancelled, Cx};
use marlin_lang_span::{FileId, TextRange};
use marlin_shader::reflect::{Attribute, NamedValue, SourceHash, Stage as ReflectStage};
use marlin_shaderc_hir_ty::{
    Builtins, BuiltinsQuery, CapabilitiesQuery, FileTyDiagnostics, GenericArgTy,
    ItemAttributesQuery, ItemTypesQuery, KnownAttr, Stage, Subst, Ty, display_ty,
};
use marlin_shaderc_ir::{Module, Space, TypeData};
use marlin_shaderc_lower::{Evaluated, LowerError, Lowering};
use marlin_shaderc_opt::{Options, PassError, pipeline};
use marlin_shaderc_reflect::Front;

use crate::{CompileError, CompileOutput, EntryPoint, EntryRequest, Session, values};

type Outcome = Result<CompileOutput, CompileError>;

/// Each `[Push]` field's type as written and its user attributes.
type PushFields = Vec<(String, Vec<Attribute>)>;

pub(crate) fn compile(s: &Session<'_>, req: &EntryRequest<'_>) -> Outcome {
    let snapshot = s.db.snapshot();
    match run(s, &snapshot, req) {
        Ok(outcome) => outcome,
        // The session's inputs change only through `&mut Session`, so no read is cancelled.
        Err(Cancelled) => Err(fail("E1506", "the compile was cancelled")),
    }
}

fn fail(code: &'static str, message: impl Into<String>) -> CompileError {
    CompileError {
        diagnostics: vec![Diagnostic::error(Code(code), message)],
    }
}

fn run(s: &Session<'_>, cx: &Cx<'_>, req: &EntryRequest<'_>) -> Result<Outcome, Cancelled> {
    let Some((target, backend)) = s.targets.get(req.target) else {
        let names = s.targets.names().join("`, `");
        return Ok(Err(fail(
            "E1501",
            format!("no target is named `{}` (there are `{names}`)", req.target),
        )));
    };
    let Some(entry) = find_item(s, cx, req.entry)? else {
        return Ok(Err(fail(
            "E1502",
            format!("nothing is named `{}`", req.entry),
        )));
    };
    let package = cx.input::<FilePackage>(&entry.file)?;
    let mut closure = s.closure(package.0 as usize);
    // Type arguments and static parameters may come from packages the entry's own does not see
    // (a game's material type instantiating an engine entry point): what they see is part of
    // the compile too.
    let args = req.consts.iter().map(|(path, _)| *path);
    for path in req.type_args.iter().copied().chain(args) {
        if let Some(item) = find_item(s, cx, path)? {
            let p = cx.input::<FilePackage>(&item.file)?;
            closure.extend(s.closure(p.0 as usize));
        }
    }
    closure.sort_unstable();
    closure.dedup();
    let files: Vec<FileId> = closure
        .iter()
        .flat_map(|&p| s.packages[p].files.iter().copied())
        .collect();

    // The front end's diagnostics for everything the entry sees.
    let (mut errors, mut warnings) = (Vec::new(), Vec::new());
    for &f in &files {
        for d in cx.get::<FileTyDiagnostics>(&f)?.iter() {
            if d.severity == Severity::Error {
                errors.push(d.clone());
            } else {
                warnings.push(d.clone());
            }
        }
    }
    if !errors.is_empty() {
        errors.extend(warnings);
        return Ok(Err(CompileError {
            diagnostics: errors,
        }));
    }
    let builtins = cx.get::<BuiltinsQuery>(&package)?;

    // Lowering, with the request's static parameters and type arguments.
    let lowered = match lower(s, cx, &builtins, req, entry, package)? {
        Ok(l) => l,
        Err(e) => return Ok(Err(e)),
    };
    let module = lowered.module;
    let func = lowered.func;

    // What the entry needs of the target.
    let caps = cx.get::<CapabilitiesQuery>(&entry)?;
    let missing: Vec<Diagnostic> = caps
        .required
        .iter()
        .filter(|(c, _)| !target.has(c))
        .map(|(c, why)| {
            Diagnostic::error(
                Code("E1504"),
                format!(
                    "`{}` needs `{c}` (it {why}), which the target `{}` does not have",
                    req.entry, target.name
                ),
            )
        })
        .collect();
    if !missing.is_empty() {
        return Ok(Err(CompileError {
            diagnostics: missing,
        }));
    }

    // The optimizer, then the backend and its validator.
    let mut optimized = module.clone();
    let options = Options {
        profile: req.profile,
        verify_each: false,
    };
    match pipeline(&options).run(&mut optimized) {
        Ok(_) => {}
        Err(PassError::Rejected { problems, .. }) => {
            let span = |at: Option<u32>| {
                at.and_then(|i| module.locs.get(i as usize)).map(|l| {
                    Span::new(FileId(l.file), TextRange::new(l.start.into(), l.end.into()))
                })
            };
            let diagnostics = problems
                .iter()
                .map(|p| {
                    let mut d = Diagnostic::error(Code(p.code), p.message.clone());
                    if let Some(sp) = span(p.at) {
                        d = d.at(sp, "");
                    }
                    if let Some((why, at)) = &p.because
                        && let Some(sp) = span(*at)
                    {
                        d = d.label(sp, why.clone());
                    }
                    d
                })
                .collect();
            return Ok(Err(CompileError { diagnostics }));
        }
        Err(e) => return Ok(Err(fail("E1506", format!("compiler bug: {e}")))),
    }
    let code = match backend.write(&optimized, func, target) {
        Ok(b) => b,
        Err(e) => {
            return Ok(Err(fail(
                "E1505",
                format!(
                    "the `{}` backend cannot write `{}`: {e}",
                    target.name, req.entry
                ),
            )));
        }
    };
    let problems = backend.validate(&code, target);
    if !problems.is_empty() {
        let shown: Vec<&str> = problems.iter().take(5).map(String::as_str).collect();
        return Ok(Err(fail(
            "E1506",
            format!(
                "compiler bug: the `{}` validator rejects the code written for `{}`:\n  {}",
                target.name,
                req.entry,
                shown.join("\n  ")
            ),
        )));
    }

    // The reflection.
    let mut sources = Vec::new();
    let mut used_sources = Vec::new();
    for &f in &files {
        let file = &s.files[f.0 as usize];
        sources.push(SourceHash {
            package: file.path.package().into(),
            path: file.path.path().into(),
            hash: file.hash,
        });
        used_sources.push(file.path.clone());
    }
    let front = Front {
        entry: module.funcs[func].name.to_string(),
        push_fields: lowered.push_fields,
        static_params: lowered.static_params,
        exports: lowered.exports,
        capabilities: caps.required.keys().map(|c| c.to_string()).collect(),
        target_constants: Vec::new(),
        sources,
    };
    let reflection =
        match marlin_shaderc_reflect::reflect(&module, func, &optimized, code.bytes.len(), front) {
            Ok(r) => r,
            Err(e) => return Ok(Err(fail("E1506", format!("compiler bug: {e}")))),
        };
    Ok(Ok(CompileOutput {
        code,
        reflection,
        diagnostics: warnings,
        used_sources,
    }))
}

/// The entry lowered, with what the reflection needs from the front end.
struct Lowered {
    module: Module,
    func: marlin_shaderc_ir::FuncId,
    push_fields: PushFields,
    static_params: Vec<NamedValue>,
    exports: Vec<NamedValue>,
}

fn lower(
    s: &Session<'_>,
    cx: &Cx<'_>,
    builtins: &Builtins,
    req: &EntryRequest<'_>,
    entry: ItemId,
    package: marlin_front_hir_def::PackageId,
) -> Result<Result<Lowered, CompileError>, Cancelled> {
    let lower_error = |e: LowerError| -> Result<CompileError, Cancelled> {
        Ok(match e.to_diagnostic(cx)? {
            Some(d) => CompileError {
                diagnostics: vec![d],
            },
            None => return Err(Cancelled),
        })
    };
    let mut l = match Lowering::new(cx, package) {
        Ok(l) => l,
        Err(e) => return Ok(Err(lower_error(e)?)),
    };
    for (path, value) in req.consts {
        let Some(item) = find_item(s, cx, path)? else {
            return Ok(Err(fail("E1502", format!("nothing is named `{path}`"))));
        };
        let tree = cx.get::<ItemTreeQuery>(&item.file)?;
        let data = &tree.items[item.local];
        let in_struct = data
            .parent
            .is_some_and(|p| tree.items[p].kind == ItemKind::Struct);
        if data.kind != ItemKind::Field || !data.is_const || !in_struct {
            return Ok(Err(fail(
                "E1503",
                format!("`{path}` is not a static parameter (a `const` field of a struct)"),
            )));
        }
        let ty = cx
            .get::<ItemTypesQuery>(&item)?
            .ty
            .clone()
            .unwrap_or(Ty::Error);
        let Some(val) = values::from_const(cx, builtins, &ty, value)? else {
            return Ok(Err(fail(
                "E1503",
                format!(
                    "`{path}` is a `{}`, which the value given does not fit",
                    display_ty(cx, builtins, &ty)?
                ),
            )));
        };
        if let Err(e) = l.set_static(item, val) {
            return Ok(Err(lower_error(e)?));
        }
    }
    let mut args = Vec::new();
    for path in req.type_args {
        let Some(item) = find_item(s, cx, path)? else {
            return Ok(Err(fail("E1502", format!("nothing is named `{path}`"))));
        };
        args.push(GenericArgTy::Type(Ty::adt(item)));
    }
    let func = match l.entry(entry, &args).and_then(|f| l.run().map(|_| f)) {
        Ok(f) => f,
        Err(e) => return Ok(Err(lower_error(e)?)),
    };

    let push_fields = push_fields(s, cx, builtins, &l)?.unwrap_or_default();
    let named = |list: Vec<Evaluated>| -> Result<Vec<NamedValue>, Cancelled> {
        let mut out = Vec::new();
        for e in list {
            if let Some(value) = values::to_const(cx, builtins, &e.ty, &e.value)? {
                out.push(NamedValue {
                    name: e.name,
                    ty: display_ty(cx, builtins, &e.ty)?,
                    value,
                });
            }
        }
        Ok(out)
    };
    let exports = named(
        l.exports()
            .into_iter()
            .filter(|e| e.item == entry)
            .collect(),
    )?;
    let static_params = match l.static_params() {
        Ok(v) => named(v)?,
        Err(e) => return Ok(Err(lower_error(e)?)),
    };
    let module = match l.finish() {
        Ok(m) => m,
        Err(e) => return Ok(Err(lower_error(e)?)),
    };
    Ok(Ok(Lowered {
        module,
        func,
        push_fields,
        static_params,
        exports,
    }))
}

/// The `[Push]` struct's fields as the source writes them: types and user attributes.
fn push_fields(
    s: &Session<'_>,
    cx: &Cx<'_>,
    builtins: &Builtins,
    l: &Lowering<'_, '_>,
) -> Result<Option<PushFields>, Cancelled> {
    let push = l
        .module
        .globals
        .iter()
        .find_map(|(_, g)| match l.module.data(g.ty) {
            TypeData::Ptr {
                space: Space::Push,
                pointee,
                ..
            } => match l.module.data(*pointee) {
                TypeData::Struct(st) => Some(*st),
                _ => None,
            },
            _ => None,
        });
    let Some((ty, fields)) = push.and_then(|st| l.struct_source(st)) else {
        return Ok(None);
    };
    let subst = match &ty {
        Ty::Adt { item, args } => Subst::new().with_args(*item, args),
        _ => Subst::new(),
    };
    let mut out = Vec::new();
    for field in fields {
        let fty = cx
            .get::<ItemTypesQuery>(&field)?
            .ty
            .clone()
            .unwrap_or(Ty::Error);
        let shown = display_ty(cx, builtins, &subst.ty(&fty))?;
        out.push((shown, attributes(s, cx, field)?));
    }
    Ok(Some(out))
}

/// The user attributes on `item` (0004 §14), as written.
fn attributes(s: &Session<'_>, cx: &Cx<'_>, item: ItemId) -> Result<Vec<Attribute>, Cancelled> {
    let attrs = cx.get::<ItemAttributesQuery>(&item)?;
    let (_, map) = cx.get::<SignatureWithSourceMap>(&item)?;
    let text: Arc<str> = s.files[item.file.0 as usize].text.clone();
    let mut out = Vec::new();
    for a in attrs.item.iter().filter(|a| a.known.is_none()) {
        let tree = cx.get::<ItemTreeQuery>(&a.ty.file)?;
        let name = tree.items[a.ty.local]
            .name
            .as_ref()
            .map(|n| n.to_string())
            .unwrap_or_default();
        let name = name.strip_suffix("Attribute").unwrap_or(&name).to_string();
        let args = a
            .args
            .iter()
            .map(|(arg_name, e)| {
                let written = map
                    .exprs
                    .get(*e)
                    .and_then(|p| text.get(p.range.start().0 as usize..p.range.end().0 as usize))
                    .unwrap_or("?");
                match arg_name {
                    Some(n) => format!("{n}: {written}"),
                    None => written.to_string(),
                }
            })
            .collect();
        out.push(Attribute { name, args });
    }
    Ok(out)
}

/// The declaration whose full path is `path`, in any package of the session.
fn find_item(s: &Session<'_>, cx: &Cx<'_>, path: &str) -> Result<Option<ItemId>, Cancelled> {
    for p in &s.packages {
        for &file in &p.files {
            let tree = cx.get::<ItemTreeQuery>(&file)?;
            for (local, _) in tree.items.iter() {
                let item = ItemId { file, local };
                if item_path(cx, item)? == path {
                    return Ok(Some(item));
                }
            }
        }
    }
    Ok(None)
}

/// The entry points a package declares: public namespace functions with a stage attribute and
/// no type parameters, sorted by full path.
pub(crate) fn entries(s: &Session<'_>, package: usize) -> Vec<EntryPoint> {
    let snapshot = s.db.snapshot();
    // The session's inputs change only through `&mut Session`, so no read is cancelled.
    find_entries(s, &snapshot, package).unwrap_or_default()
}

fn find_entries(
    s: &Session<'_>,
    cx: &Cx<'_>,
    package: usize,
) -> Result<Vec<EntryPoint>, Cancelled> {
    let mut out = Vec::new();
    for &file in &s.packages[package].files {
        let tree = cx.get::<ItemTreeQuery>(&file)?;
        for (local, item) in tree.items.iter() {
            if item.kind != ItemKind::Function || !reachable(&tree, local) {
                continue;
            }
            let id = ItemId { file, local };
            let attrs = cx.get::<ItemAttributesQuery>(&id)?;
            let stage = attrs.item.iter().find_map(|a| match a.known {
                Some(KnownAttr::Stage(stage)) => Some(stage),
                _ => None,
            });
            if let Some(stage) = stage {
                out.push(EntryPoint {
                    path: item_path(cx, id)?,
                    stage: match stage {
                        Stage::Compute => ReflectStage::Compute,
                        Stage::Vertex => ReflectStage::Vertex,
                        Stage::Pixel => ReflectStage::Pixel,
                        Stage::Mesh => ReflectStage::Mesh,
                        Stage::Amplification => ReflectStage::Amplification,
                    },
                });
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// Whether `local` can be an entry point another package requests without type arguments: a
/// public, non-generic function of a namespace (entry points are never type members, E1210).
fn reachable(tree: &ItemTree, local: LocalItemId) -> bool {
    let item = &tree.items[local];
    item.visibility == Visibility::Public
        && item.generic_params.is_empty()
        && item
            .parent
            .is_none_or(|p| tree.items[p].kind == ItemKind::Namespace)
}
