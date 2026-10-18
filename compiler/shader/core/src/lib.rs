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
//! - `Textures.mshader`: textures, storage textures and samplers (§10.4).
//! - `Gpu.mshader`: `Barrier`, `Wave`, `Debug`, `NonUniform` (§10.3, §12).
//! - `Mesh.mshader`: mesh outputs and `Mesh.Dispatch` (§11.6).
//! - `RayTracing.mshader`: rays, inline ray queries and acceleration structures (§12.4).

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
    ("Mesh.mshader", include_str!("../library/Mesh.mshader")),
    (
        "Resources.mshader",
        include_str!("../library/Resources.mshader"),
    ),
    (
        "RayTracing.mshader",
        include_str!("../library/RayTracing.mshader"),
    ),
    (
        "Scalars.mshader",
        include_str!("../library/Scalars.mshader"),
    ),
    (
        "Textures.mshader",
        include_str!("../library/Textures.mshader"),
    ),
    (
        "Vectors.mshader",
        include_str!("../library/Vectors.mshader"),
    ),
];
