//! An entry point's reflection (`docs/design/0005-shader-compiler.md` §6, step 9c of §11): the
//! data types are the runtime's (`marlin_shader::reflect`); this crate fills them.
//!
//! From the IR: the stage and thread-group size, the `[Push]` struct's layout (from the module
//! as lowered, so a push struct the code never reads is still described), and the numbers of
//! the optimized module. From the front end ([`Front`]): what the IR does not keep, namely
//! types as the source writes them, attributes, compile-time values, capabilities and files.

use marlin_shader::reflect::{
    Attribute, EntryInfo, FieldLayout, NamedValue, ShaderReflection, SourceHash, Stage, Stats,
    StructLayout,
};
use marlin_shaderc_ir::{
    FuncId, Module, Space, Stage as IrStage, TypeData, scalar_field_offset, scalar_layout,
};

/// What the front end knows about an entry point.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Front {
    /// The entry function's path with its type arguments.
    pub entry: String,
    /// For each field of the `[Push]` struct, in its order: the type as written, and the
    /// user attributes.
    pub push_fields: Vec<(String, Vec<Attribute>)>,
    pub static_params: Vec<NamedValue>,
    pub exports: Vec<NamedValue>,
    pub capabilities: Vec<String>,
    pub target_constants: Vec<String>,
    pub sources: Vec<SourceHash>,
}

/// The reflection of entry point `entry`: `lowered` is its module as lowered, `optimized` the
/// module the backend wrote (`code_bytes` of code) for the entry with the same id.
pub fn reflect(
    lowered: &Module,
    entry: FuncId,
    optimized: &Module,
    code_bytes: usize,
    front: Front,
) -> Result<ShaderReflection, String> {
    let e = lowered
        .entry_of(entry)
        .ok_or_else(|| format!("`{}` is not an entry point", lowered.funcs[entry].name))?;
    let (stage, group_size) = match e.stage {
        IrStage::Compute(s) => (Stage::Compute, s),
        IrStage::Vertex => (Stage::Vertex, [1; 3]),
        IrStage::Pixel => (Stage::Pixel, [1; 3]),
        IrStage::Mesh(s) => (Stage::Mesh, s),
        IrStage::Amplification(s) => (Stage::Amplification, s),
    };
    let push = push_layout(lowered, entry, &front.push_fields)?;
    let mut capabilities = front.capabilities;
    capabilities.sort();
    capabilities.dedup();
    let mut target_constants = front.target_constants;
    target_constants.sort();
    target_constants.dedup();
    Ok(ShaderReflection {
        entry: EntryInfo {
            name: front.entry,
            stage,
            group_size,
        },
        push,
        static_params: front.static_params,
        exports: front.exports,
        capabilities,
        target_constants,
        sources: front.sources,
        stats: stats(optimized, entry, code_bytes),
        extensions: Vec::new(),
    })
}

/// The layout of the entry's `[Push]` struct, with the front end's field types and attributes.
fn push_layout(
    m: &Module,
    entry: FuncId,
    fields: &[(String, Vec<Attribute>)],
) -> Result<Option<StructLayout>, String> {
    let Some(e) = m.entry_of(entry) else {
        return Ok(None);
    };
    let push = e.uses.iter().find_map(|&g| match m.data(m.globals[g].ty) {
        TypeData::Ptr {
            space: Space::Push,
            pointee,
            ..
        } => Some(*pointee),
        _ => None,
    });
    let Some(t) = push else { return Ok(None) };
    let TypeData::Struct(s) = m.data(t) else {
        return Err("the push constants are not a struct".into());
    };
    let data = &m.structs[*s];
    if fields.len() != data.fields.len() {
        return Err(format!(
            "the front end describes {} push field(s), the IR has {}",
            fields.len(),
            data.fields.len()
        ));
    }
    let (size, _) = scalar_layout(m, t).ok_or("the push struct has no layout")?;
    let mut out = Vec::new();
    for (i, ((name, ft), (ty, attributes))) in data.fields.iter().zip(fields).enumerate() {
        let offset = scalar_field_offset(m, t, i as u32).ok_or("a push field has no offset")?;
        let (fsize, _) = scalar_layout(m, *ft).ok_or("a push field has no layout")?;
        out.push(FieldLayout {
            name: name.to_string(),
            ty: ty.clone(),
            offset: offset as u32,
            size: fsize as u32,
            attributes: attributes.clone(),
        });
    }
    Ok(Some(StructLayout {
        name: data.name.to_string(),
        size: size as u32,
        fields: out,
    }))
}

/// Instructions of the optimized module and the groupshared memory the entry uses.
fn stats(m: &Module, entry: FuncId, code_bytes: usize) -> Stats {
    let instructions = m
        .funcs
        .iter()
        .map(|(_, f)| f.layout.iter().map(|&b| f.insts(b).count()).sum::<usize>())
        .sum::<usize>();
    let groupshared_bytes = m.entry_of(entry).map_or(0, |e| {
        e.uses
            .iter()
            .filter_map(|&g| match m.data(m.globals[g].ty) {
                TypeData::Ptr {
                    space: Space::GroupShared,
                    pointee,
                    ..
                } => scalar_layout(m, *pointee).map(|(s, _)| s),
                _ => None,
            })
            .sum::<u64>()
    });
    Stats {
        instructions: instructions as u32,
        groupshared_bytes: groupshared_bytes as u32,
        code_bytes: code_bytes as u32,
    }
}
