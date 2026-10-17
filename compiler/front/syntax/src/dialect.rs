//! Dialects: what each C#-family language accepts of the superset grammar
//! (`docs/design/0015-language-frontend.md` §2.2).

use crate::SyntaxKind;

/// A construct that some dialects have and others lack. The parser builds all of them;
/// validation rejects those missing from [`Dialect::allowed`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Construct {
    // From C#.
    New,
    Class,
    Record,
    Null,
    As,
    Typeof,
    Sizeof,
    Base,
    Throw,
    CharLiteral,
    DecimalLiteral,
    BlockNamespace,
    PrimaryConstructor,
    ProtectedModifier,
    OverrideModifier,
    SealedModifier,
    ExternModifier,
    UnsafeModifier,
    VolatileModifier,
    ParamsModifier,
    // From the Marlin shader language.
    ConstIf,
    AssocType,
    TypeAlias,
    ConstGenericParam,
    Extension,
    ExtensionConformance,
    HalfLiteral,
    NamespaceFunction,
    NamespaceField,
}

impl Construct {
    pub const ALL: &'static [Construct] = &[
        Self::New,
        Self::Class,
        Self::Record,
        Self::Null,
        Self::As,
        Self::Typeof,
        Self::Sizeof,
        Self::Base,
        Self::Throw,
        Self::CharLiteral,
        Self::DecimalLiteral,
        Self::BlockNamespace,
        Self::PrimaryConstructor,
        Self::ProtectedModifier,
        Self::OverrideModifier,
        Self::SealedModifier,
        Self::ExternModifier,
        Self::UnsafeModifier,
        Self::VolatileModifier,
        Self::ParamsModifier,
        Self::ConstIf,
        Self::AssocType,
        Self::TypeAlias,
        Self::ConstGenericParam,
        Self::Extension,
        Self::ExtensionConformance,
        Self::HalfLiteral,
        Self::NamespaceFunction,
        Self::NamespaceField,
    ];

    /// How the construct is named in "X has no ..." messages.
    pub fn describe(self) -> &'static str {
        match self {
            Self::New => "`new` expressions",
            Self::Class => "classes",
            Self::Record => "records",
            Self::Null => "`null`",
            Self::As => "`as` conversions",
            Self::Typeof => "`typeof`",
            Self::Sizeof => "`sizeof`",
            Self::Base => "`base`",
            Self::Throw => "`throw`",
            Self::CharLiteral => "character literals",
            Self::DecimalLiteral => "`decimal` literals",
            Self::BlockNamespace => "block-scoped namespaces",
            Self::PrimaryConstructor => "primary constructors",
            Self::ProtectedModifier => "`protected`",
            Self::OverrideModifier => "`override`",
            Self::SealedModifier => "`sealed`",
            Self::ExternModifier => "`extern`",
            Self::UnsafeModifier => "`unsafe`",
            Self::VolatileModifier => "`volatile`",
            Self::ParamsModifier => "`params` parameters",
            Self::ConstIf => "`const if`",
            Self::AssocType => "associated types",
            Self::TypeAlias => "type aliases in namespaces",
            Self::ConstGenericParam => "constant generic parameters",
            Self::Extension => "extension blocks",
            Self::ExtensionConformance => "interface conformance on extension blocks",
            Self::HalfLiteral => "`half` literals",
            Self::NamespaceFunction => "functions outside a type",
            Self::NamespaceField => "fields outside a type",
        }
    }

    /// The modifier keyword this construct stands for, if it is one.
    pub fn modifier(kind: SyntaxKind) -> Option<Construct> {
        Some(match kind {
            SyntaxKind::PROTECTED_KW => Self::ProtectedModifier,
            SyntaxKind::OVERRIDE_KW => Self::OverrideModifier,
            SyntaxKind::SEALED_KW => Self::SealedModifier,
            SyntaxKind::EXTERN_KW => Self::ExternModifier,
            SyntaxKind::UNSAFE_KW => Self::UnsafeModifier,
            SyntaxKind::VOLATILE_KW => Self::VolatileModifier,
            SyntaxKind::PARAMS_KW => Self::ParamsModifier,
            _ => return None,
        })
    }
}

/// A set of [`Construct`]s.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ConstructSet(u64);

impl ConstructSet {
    pub const NONE: ConstructSet = ConstructSet(0);
    pub const ALL: ConstructSet = ConstructSet((1 << Construct::ALL.len()) - 1);

    pub const fn of(constructs: &[Construct]) -> ConstructSet {
        let mut bits = 0;
        let mut i = 0;
        while i < constructs.len() {
            bits |= 1 << constructs[i] as u8;
            i += 1;
        }
        ConstructSet(bits)
    }

    pub const fn union(self, other: ConstructSet) -> ConstructSet {
        ConstructSet(self.0 | other.0)
    }

    pub const fn without(self, other: ConstructSet) -> ConstructSet {
        ConstructSet(self.0 & !other.0)
    }

    pub const fn contains(self, construct: Construct) -> bool {
        self.0 & (1 << construct as u8) != 0
    }
}

/// One language of the C# family, as far as syntax is concerned.
#[derive(Clone, Copy)]
pub struct Dialect {
    /// The language's name in diagnostics: "Marlin shader has no `new` expressions".
    pub name: &'static str,
    /// Keywords reserved beyond C#'s, such as `half`.
    pub extra_keywords: &'static [SyntaxKind],
    pub allowed: ConstructSet,
    /// How to fix a construct the dialect rejects, shown as a note.
    pub hint: fn(Construct) -> Option<&'static str>,
}

impl Dialect {
    /// Accepts everything the grammar parses. For tests and tools that only need the tree.
    pub const SUPERSET: Dialect = Dialect {
        name: "the superset grammar",
        extra_keywords: &[SyntaxKind::HALF_KW],
        allowed: ConstructSet::ALL,
        hint: |_| None,
    };
}

impl std::fmt::Debug for Dialect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dialect")
            .field("name", &self.name)
            .field("extra_keywords", &self.extra_keywords)
            .field("allowed", &self.allowed)
            .finish_non_exhaustive()
    }
}
