//! The Marlin shader dialect (`docs/design/0004-shader-language.md`,
//! `0015-language-frontend.md` §2.2): which parts of the shared C#-family grammar the shader
//! language accepts, and how to fix the ones it does not.

pub use marlin_front_syntax::*;

/// C# constructs the shader language leaves out (0004 §1).
const REJECTED: ConstructSet = ConstructSet::of(&[
    Construct::New,
    Construct::Class,
    Construct::Record,
    Construct::Null,
    Construct::As,
    Construct::Typeof,
    Construct::Sizeof,
    Construct::Base,
    Construct::Throw,
    Construct::CharLiteral,
    Construct::DecimalLiteral,
    Construct::BlockNamespace,
    Construct::PrimaryConstructor,
    Construct::ProtectedModifier,
    Construct::OverrideModifier,
    Construct::SealedModifier,
    Construct::ExternModifier,
    Construct::UnsafeModifier,
    Construct::VolatileModifier,
    Construct::ParamsModifier,
]);

pub const SHADER: Dialect = Dialect {
    name: "Marlin shader",
    extra_keywords: &[SyntaxKind::HALF_KW],
    allowed: ConstructSet::ALL.without(REJECTED),
    hint,
};

fn hint(construct: Construct) -> Option<&'static str> {
    Some(match construct {
        Construct::New => "construct values with `T(...)`, `T { ... }` or a target-typed `{ ... }`",
        Construct::Class | Construct::Record => {
            "use `struct`: every Marlin shader type is a value type, and `with` works on any struct"
        }
        Construct::Null => "every type is a value type; use `default` for a zero value",
        Construct::As => "use a cast: `(T)x`",
        Construct::Base => "structs have no base type; call the member directly",
        Construct::Throw => "there are no exceptions; use `Debug.Assert` in Development builds",
        Construct::CharLiteral => {
            "strings appear only in attributes, `nameof` and `Debug` messages"
        }
        Construct::DecimalLiteral => "use the `f`, `d` or `h` suffix",
        Construct::BlockNamespace => "declare the namespace for the whole file: `namespace A.B;`",
        Construct::OverrideModifier | Construct::SealedModifier | Construct::ProtectedModifier => {
            "structs cannot be inherited; implement interfaces instead"
        }
        _ => return None,
    })
}

/// Parses one `.mshader` file.
pub fn parse(text: &str) -> Parse {
    marlin_front_syntax::parse(text, &SHADER)
}
