//! The core library (`docs/design/0004-shader-language.md` §17, step 9d of 0005 §11): the
//! package `Marlin` that every shader package depends on, as Marlin shader source embedded in
//! the compiler, so a compiler and its core library always match. `marlin-shaderc` adds it to
//! every session.
//!
//! - `Attributes.mshader`: the attributes the compiler knows, `IAttribute`, `Capability`.
//! - `Scalars.mshader`: the numeric interfaces (§8.6) and the scalar types.
//! - `Vectors.mshader`: vectors, matrices and their short names (§3.2).
//! - `Math.mshader`: math functions, with HLSL's names in PascalCase (§17.2).
//! - `Resources.mshader`: uniforms and buffers with their atomics (§10), `Func`/`Action`.
//! - `Gpu.mshader`: `Barrier`, `Wave`, `Debug`, `NonUniform` (§10.3, §12).
//!
//! Textures, samplers, derivatives, ray queries and mesh outputs come with the graphics stages
//! (step 10).

/// The package's name.
pub const PACKAGE: &str = "Marlin";

/// The package's files: (path in the package, text), in a stable order.
pub const FILES: &[(&str, &str)] = &[
    (
        "Attributes.mshader",
        include_str!("../library/Attributes.mshader"),
    ),
    ("Gpu.mshader", include_str!("../library/Gpu.mshader")),
    ("Math.mshader", include_str!("../library/Math.mshader")),
    (
        "Resources.mshader",
        include_str!("../library/Resources.mshader"),
    ),
    (
        "Scalars.mshader",
        include_str!("../library/Scalars.mshader"),
    ),
    (
        "Vectors.mshader",
        include_str!("../library/Vectors.mshader"),
    ),
];
