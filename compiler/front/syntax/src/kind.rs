//! Token and node kinds.

macro_rules! syntax_kinds {
    (
        punct: { $($p_name:ident = $p_text:literal,)* }
        composite: { $($x_name:ident = $x_text:literal,)* }
        keywords: { $($k_name:ident = $k_text:literal,)* }
        extra_keywords: { $($e_name:ident = $e_text:literal,)* }
        contextual: { $($c_name:ident = $c_text:literal,)* }
        tokens: { $($(#[$t_meta:meta])* $t_name:ident,)* }
        nodes: { $($n_name:ident,)* }
    ) => {
        /// Every token and node kind of the concrete syntax tree.
        #[allow(non_camel_case_types)]
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[repr(u16)]
        pub enum SyntaxKind {
            $($p_name,)*
            $($x_name,)*
            $($k_name,)*
            $($e_name,)*
            $($c_name,)*
            $($(#[$t_meta])* $t_name,)*
            $($n_name,)*
        }

        impl SyntaxKind {
            /// The C# reserved keyword spelled `text`.
            pub fn from_keyword(text: &str) -> Option<Self> {
                match text {
                    $($k_text => Some(Self::$k_name),)*
                    _ => None,
                }
            }

            /// The dialect-specific keyword spelled `text`, reserved only where a dialect lists it.
            pub fn from_extra_keyword(text: &str) -> Option<Self> {
                match text {
                    $($e_text => Some(Self::$e_name),)*
                    _ => None,
                }
            }

            /// The contextual keyword spelled `text`. The lexer produces `IDENT` for these; the
            /// parser gives them their keyword kind where they act as one.
            pub fn from_contextual_keyword(text: &str) -> Option<Self> {
                match text {
                    $($c_text => Some(Self::$c_name),)*
                    _ => None,
                }
            }

            /// Reserved or contextual keyword.
            pub fn is_keyword(self) -> bool {
                matches!(self, $(Self::$k_name)|* | $(Self::$e_name)|* | $(Self::$c_name)|*)
            }

            pub fn is_contextual_keyword(self) -> bool {
                matches!(self, $(Self::$c_name)|*)
            }

            pub fn is_punct(self) -> bool {
                matches!(self, $(Self::$p_name)|* | $(Self::$x_name)|*)
            }

            /// Whether this is a node kind (as opposed to a token kind).
            pub fn is_node(self) -> bool {
                self as u16 >= Self::SOURCE_FILE as u16
            }

            /// The fixed spelling of a punctuation or keyword token.
            pub fn token_text(self) -> Option<&'static str> {
                match self {
                    $(Self::$p_name => Some($p_text),)*
                    $(Self::$x_name => Some($x_text),)*
                    $(Self::$k_name => Some($k_text),)*
                    $(Self::$e_name => Some($e_text),)*
                    $(Self::$c_name => Some($c_text),)*
                    _ => None,
                }
            }
        }
    };
}

syntax_kinds! {
    punct: {
        L_CURLY = "{",
        R_CURLY = "}",
        L_PAREN = "(",
        R_PAREN = ")",
        L_BRACK = "[",
        R_BRACK = "]",
        SEMICOLON = ";",
        COMMA = ",",
        DOT = ".",
        DOT2 = "..",
        COLON = ":",
        COLON2 = "::",
        QUESTION = "?",
        TILDE = "~",
        BANG = "!",
        EQ = "=",
        EQ2 = "==",
        NEQ = "!=",
        LT = "<",
        LTEQ = "<=",
        SHL = "<<",
        SHL_EQ = "<<=",
        GT = ">",
        GTEQ = ">=",
        PLUS = "+",
        PLUS2 = "++",
        PLUS_EQ = "+=",
        MINUS = "-",
        MINUS2 = "--",
        MINUS_EQ = "-=",
        STAR = "*",
        STAR_EQ = "*=",
        SLASH = "/",
        SLASH_EQ = "/=",
        PERCENT = "%",
        PERCENT_EQ = "%=",
        AMP = "&",
        AMP2 = "&&",
        AMP_EQ = "&=",
        PIPE = "|",
        PIPE2 = "||",
        PIPE_EQ = "|=",
        CARET = "^",
        CARET_EQ = "^=",
        FAT_ARROW = "=>",
    }
    // Glued by the parser from adjacent `>` / `>=` tokens, so that `A<B<C>>` lexes as two `>`.
    composite: {
        SHR = ">>",
        SHR_EQ = ">>=",
        USHR = ">>>",
        USHR_EQ = ">>>=",
    }
    // C#'s reserved keywords. Constructs a dialect lacks are rejected by validation.
    keywords: {
        ABSTRACT_KW = "abstract",
        AS_KW = "as",
        BASE_KW = "base",
        BOOL_KW = "bool",
        BREAK_KW = "break",
        BYTE_KW = "byte",
        CASE_KW = "case",
        CATCH_KW = "catch",
        CHAR_KW = "char",
        CHECKED_KW = "checked",
        CLASS_KW = "class",
        CONST_KW = "const",
        CONTINUE_KW = "continue",
        DECIMAL_KW = "decimal",
        DEFAULT_KW = "default",
        DELEGATE_KW = "delegate",
        DO_KW = "do",
        DOUBLE_KW = "double",
        ELSE_KW = "else",
        ENUM_KW = "enum",
        EVENT_KW = "event",
        EXPLICIT_KW = "explicit",
        EXTERN_KW = "extern",
        FALSE_KW = "false",
        FINALLY_KW = "finally",
        FIXED_KW = "fixed",
        FLOAT_KW = "float",
        FOR_KW = "for",
        FOREACH_KW = "foreach",
        GOTO_KW = "goto",
        IF_KW = "if",
        IMPLICIT_KW = "implicit",
        IN_KW = "in",
        INT_KW = "int",
        INTERFACE_KW = "interface",
        INTERNAL_KW = "internal",
        IS_KW = "is",
        LOCK_KW = "lock",
        LONG_KW = "long",
        NAMESPACE_KW = "namespace",
        NEW_KW = "new",
        NULL_KW = "null",
        OBJECT_KW = "object",
        OPERATOR_KW = "operator",
        OUT_KW = "out",
        OVERRIDE_KW = "override",
        PARAMS_KW = "params",
        PRIVATE_KW = "private",
        PROTECTED_KW = "protected",
        PUBLIC_KW = "public",
        READONLY_KW = "readonly",
        REF_KW = "ref",
        RETURN_KW = "return",
        SBYTE_KW = "sbyte",
        SEALED_KW = "sealed",
        SHORT_KW = "short",
        SIZEOF_KW = "sizeof",
        STACKALLOC_KW = "stackalloc",
        STATIC_KW = "static",
        STRING_KW = "string",
        STRUCT_KW = "struct",
        SWITCH_KW = "switch",
        THIS_KW = "this",
        THROW_KW = "throw",
        TRUE_KW = "true",
        TRY_KW = "try",
        TYPEOF_KW = "typeof",
        UINT_KW = "uint",
        ULONG_KW = "ulong",
        UNCHECKED_KW = "unchecked",
        UNSAFE_KW = "unsafe",
        USHORT_KW = "ushort",
        USING_KW = "using",
        VIRTUAL_KW = "virtual",
        VOID_KW = "void",
        VOLATILE_KW = "volatile",
        WHILE_KW = "while",
    }
    // Reserved only by the dialects that list them in `Dialect::extra_keywords`.
    extra_keywords: {
        HALF_KW = "half",
    }
    contextual: {
        AND_KW = "and",
        EXTENSION_KW = "extension",
        GET_KW = "get",
        GLOBAL_KW = "global",
        INIT_KW = "init",
        NOT_KW = "not",
        OR_KW = "or",
        RECORD_KW = "record",
        SET_KW = "set",
        TYPE_KW = "type",
        VAR_KW = "var",
        WHEN_KW = "when",
        WHERE_KW = "where",
        WITH_KW = "with",
    }
    tokens: {
        INT_NUMBER,
        FLOAT_NUMBER,
        STRING,
        CHAR,
        /// `$"` (or `$@"` / `@$"`), opening an interpolated string.
        INTERP_START,
        /// Literal text inside an interpolated string.
        INTERP_TEXT,
        /// The format text after `:` in an interpolation hole.
        INTERP_FORMAT,
        /// The closing `"` of an interpolated string.
        INTERP_END,
        IDENT,
        WHITESPACE,
        LINE_COMMENT,
        BLOCK_COMMENT,
        /// A character the lexer does not accept, or a preprocessor line.
        ERROR_TOKEN,
        EOF,
    }
    nodes: {
        SOURCE_FILE,
        ERROR,

        // Items and members.
        NAMESPACE_DECL,
        USING_DIRECTIVE,
        USING_ALIAS,
        ATTRIBUTE_LIST,
        ATTRIBUTE,
        MODIFIERS,
        STRUCT_DECL,
        CLASS_DECL,
        RECORD_DECL,
        INTERFACE_DECL,
        ENUM_DECL,
        ENUM_MEMBER_LIST,
        ENUM_MEMBER,
        EXTENSION_DECL,
        MEMBER_LIST,
        FUNCTION_DECL,
        CONSTRUCTOR_DECL,
        CONSTRUCTOR_INITIALIZER,
        OPERATOR_DECL,
        CONVERSION_DECL,
        PROPERTY_DECL,
        INDEXER_DECL,
        ACCESSOR_LIST,
        ACCESSOR,
        FIELD_DECL,
        VARIABLE_DECLARATOR,
        ASSOC_TYPE_DECL,
        TYPE_PARAM_LIST,
        TYPE_PARAM,
        BASE_LIST,
        WHERE_CLAUSE,
        PARAM_LIST,
        PARAM,
        EXPR_BODY,
        NAME,
        NAME_REF,

        // Types.
        PREDEFINED_TYPE,
        PATH_TYPE,
        PATH,
        PATH_SEGMENT,
        GENERIC_ARG_LIST,
        TYPE_ARG,
        CONST_ARG,
        ARRAY_TYPE,
        TUPLE_TYPE,
        TUPLE_TYPE_ELEMENT,

        // Statements.
        BLOCK,
        LOCAL_DECL_STMT,
        EXPR_STMT,
        IF_STMT,
        ELSE_CLAUSE,
        FOR_STMT,
        FOR_INIT,
        FOR_UPDATE,
        FOREACH_STMT,
        WHILE_STMT,
        DO_STMT,
        SWITCH_STMT,
        SWITCH_SECTION,
        CASE_LABEL,
        DEFAULT_LABEL,
        BREAK_STMT,
        CONTINUE_STMT,
        RETURN_STMT,
        EMPTY_STMT,

        // Expressions.
        LITERAL,
        INTERPOLATED_STRING,
        INTERPOLATION,
        NAME_EXPR,
        TYPE_EXPR,
        MEMBER_EXPR,
        CALL_EXPR,
        INDEX_EXPR,
        ARG_LIST,
        BRACKET_ARG_LIST,
        ARG,
        NAME_COLON,
        PAREN_EXPR,
        TUPLE_EXPR,
        PREFIX_EXPR,
        POSTFIX_EXPR,
        BIN_EXPR,
        CONDITIONAL_EXPR,
        CAST_EXPR,
        RANGE_EXPR,
        IS_EXPR,
        SWITCH_EXPR,
        SWITCH_ARM,
        WHEN_CLAUSE,
        WITH_EXPR,
        OBJECT_INIT_EXPR,
        INITIALIZER,
        FIELD_INIT,
        DEFAULT_EXPR,
        THIS_EXPR,
        BASE_EXPR,
        NEW_EXPR,
        AS_EXPR,
        TYPEOF_EXPR,
        SIZEOF_EXPR,
        THROW_EXPR,
        COLLECTION_EXPR,
        LAMBDA_EXPR,
        LAMBDA_PARAM_LIST,
        DECL_EXPR,
        TUPLE_DESIGNATION,

        // Patterns.
        CONSTANT_PATTERN,
        RELATIONAL_PATTERN,
        DISCARD_PATTERN,
        VAR_PATTERN,
        BINARY_PATTERN,
        NOT_PATTERN,
        PAREN_PATTERN,
        PROPERTY_PATTERN,
        POSITIONAL_PATTERN,
        SUBPATTERN,
    }
}

impl SyntaxKind {
    pub fn is_trivia(self) -> bool {
        matches!(
            self,
            Self::WHITESPACE | Self::LINE_COMMENT | Self::BLOCK_COMMENT
        )
    }

    /// Keywords that name built-in scalar types.
    pub fn is_predefined_type(self) -> bool {
        use SyntaxKind::*;
        matches!(
            self,
            BOOL_KW
                | BYTE_KW
                | CHAR_KW
                | DECIMAL_KW
                | DOUBLE_KW
                | FLOAT_KW
                | HALF_KW
                | INT_KW
                | LONG_KW
                | OBJECT_KW
                | SBYTE_KW
                | SHORT_KW
                | STRING_KW
                | UINT_KW
                | ULONG_KW
                | USHORT_KW
                | VOID_KW
        )
    }

    pub fn is_literal(self) -> bool {
        use SyntaxKind::*;
        matches!(
            self,
            INT_NUMBER | FLOAT_NUMBER | STRING | CHAR | TRUE_KW | FALSE_KW | NULL_KW
        )
    }

    /// How to show this kind in "expected X" messages.
    pub fn describe(self) -> String {
        match self.token_text() {
            Some(text) => format!("`{text}`"),
            None => match self {
                Self::IDENT => "identifier".into(),
                Self::INT_NUMBER | Self::FLOAT_NUMBER => "number".into(),
                Self::STRING => "string".into(),
                Self::EOF => "end of file".into(),
                _ => format!("{self:?}").to_lowercase().replace('_', " "),
            },
        }
    }
}

/// `T![;]`, `T![struct]`, `T!['{']`: the kind of a punctuation or keyword token.
#[macro_export]
macro_rules! T {
    ['{'] => { $crate::SyntaxKind::L_CURLY };
    ['}'] => { $crate::SyntaxKind::R_CURLY };
    ['('] => { $crate::SyntaxKind::L_PAREN };
    [')'] => { $crate::SyntaxKind::R_PAREN };
    ['['] => { $crate::SyntaxKind::L_BRACK };
    [']'] => { $crate::SyntaxKind::R_BRACK };
    [;] => { $crate::SyntaxKind::SEMICOLON };
    [,] => { $crate::SyntaxKind::COMMA };
    [.] => { $crate::SyntaxKind::DOT };
    [..] => { $crate::SyntaxKind::DOT2 };
    [:] => { $crate::SyntaxKind::COLON };
    [::] => { $crate::SyntaxKind::COLON2 };
    [?] => { $crate::SyntaxKind::QUESTION };
    [~] => { $crate::SyntaxKind::TILDE };
    [!] => { $crate::SyntaxKind::BANG };
    [=] => { $crate::SyntaxKind::EQ };
    [==] => { $crate::SyntaxKind::EQ2 };
    [!=] => { $crate::SyntaxKind::NEQ };
    [<] => { $crate::SyntaxKind::LT };
    [<=] => { $crate::SyntaxKind::LTEQ };
    [<<] => { $crate::SyntaxKind::SHL };
    [<<=] => { $crate::SyntaxKind::SHL_EQ };
    [>] => { $crate::SyntaxKind::GT };
    [>=] => { $crate::SyntaxKind::GTEQ };
    [>>] => { $crate::SyntaxKind::SHR };
    [>>=] => { $crate::SyntaxKind::SHR_EQ };
    [>>>] => { $crate::SyntaxKind::USHR };
    [>>>=] => { $crate::SyntaxKind::USHR_EQ };
    [+] => { $crate::SyntaxKind::PLUS };
    [++] => { $crate::SyntaxKind::PLUS2 };
    [+=] => { $crate::SyntaxKind::PLUS_EQ };
    [-] => { $crate::SyntaxKind::MINUS };
    [--] => { $crate::SyntaxKind::MINUS2 };
    [-=] => { $crate::SyntaxKind::MINUS_EQ };
    [*] => { $crate::SyntaxKind::STAR };
    [*=] => { $crate::SyntaxKind::STAR_EQ };
    [/] => { $crate::SyntaxKind::SLASH };
    [/=] => { $crate::SyntaxKind::SLASH_EQ };
    [%] => { $crate::SyntaxKind::PERCENT };
    [%=] => { $crate::SyntaxKind::PERCENT_EQ };
    [&] => { $crate::SyntaxKind::AMP };
    [&&] => { $crate::SyntaxKind::AMP2 };
    [&=] => { $crate::SyntaxKind::AMP_EQ };
    [|] => { $crate::SyntaxKind::PIPE };
    [||] => { $crate::SyntaxKind::PIPE2 };
    [|=] => { $crate::SyntaxKind::PIPE_EQ };
    [^] => { $crate::SyntaxKind::CARET };
    [^=] => { $crate::SyntaxKind::CARET_EQ };
    [=>] => { $crate::SyntaxKind::FAT_ARROW };
    [and] => { $crate::SyntaxKind::AND_KW };
    [base] => { $crate::SyntaxKind::BASE_KW };
    [break] => { $crate::SyntaxKind::BREAK_KW };
    [case] => { $crate::SyntaxKind::CASE_KW };
    [class] => { $crate::SyntaxKind::CLASS_KW };
    [const] => { $crate::SyntaxKind::CONST_KW };
    [continue] => { $crate::SyntaxKind::CONTINUE_KW };
    [default] => { $crate::SyntaxKind::DEFAULT_KW };
    [do] => { $crate::SyntaxKind::DO_KW };
    [else] => { $crate::SyntaxKind::ELSE_KW };
    [enum] => { $crate::SyntaxKind::ENUM_KW };
    [explicit] => { $crate::SyntaxKind::EXPLICIT_KW };
    [extension] => { $crate::SyntaxKind::EXTENSION_KW };
    [false] => { $crate::SyntaxKind::FALSE_KW };
    [for] => { $crate::SyntaxKind::FOR_KW };
    [foreach] => { $crate::SyntaxKind::FOREACH_KW };
    [get] => { $crate::SyntaxKind::GET_KW };
    [global] => { $crate::SyntaxKind::GLOBAL_KW };
    [if] => { $crate::SyntaxKind::IF_KW };
    [implicit] => { $crate::SyntaxKind::IMPLICIT_KW };
    [in] => { $crate::SyntaxKind::IN_KW };
    [init] => { $crate::SyntaxKind::INIT_KW };
    [interface] => { $crate::SyntaxKind::INTERFACE_KW };
    [is] => { $crate::SyntaxKind::IS_KW };
    [namespace] => { $crate::SyntaxKind::NAMESPACE_KW };
    [new] => { $crate::SyntaxKind::NEW_KW };
    [not] => { $crate::SyntaxKind::NOT_KW };
    [null] => { $crate::SyntaxKind::NULL_KW };
    [operator] => { $crate::SyntaxKind::OPERATOR_KW };
    [or] => { $crate::SyntaxKind::OR_KW };
    [out] => { $crate::SyntaxKind::OUT_KW };
    [readonly] => { $crate::SyntaxKind::READONLY_KW };
    [record] => { $crate::SyntaxKind::RECORD_KW };
    [ref] => { $crate::SyntaxKind::REF_KW };
    [return] => { $crate::SyntaxKind::RETURN_KW };
    [set] => { $crate::SyntaxKind::SET_KW };
    [static] => { $crate::SyntaxKind::STATIC_KW };
    [struct] => { $crate::SyntaxKind::STRUCT_KW };
    [switch] => { $crate::SyntaxKind::SWITCH_KW };
    [this] => { $crate::SyntaxKind::THIS_KW };
    [true] => { $crate::SyntaxKind::TRUE_KW };
    [type] => { $crate::SyntaxKind::TYPE_KW };
    [using] => { $crate::SyntaxKind::USING_KW };
    [var] => { $crate::SyntaxKind::VAR_KW };
    [void] => { $crate::SyntaxKind::VOID_KW };
    [when] => { $crate::SyntaxKind::WHEN_KW };
    [where] => { $crate::SyntaxKind::WHERE_KW };
    [while] => { $crate::SyntaxKind::WHILE_KW };
    [with] => { $crate::SyntaxKind::WITH_KW };
}
