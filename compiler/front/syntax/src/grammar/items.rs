//! Files, namespaces, `using`, and declarations.

use super::{arg_list, attribute_lists, exprs, modifiers, name, param_list, stmts, types};
use crate::SyntaxKind::*;
use crate::T;
use crate::parser::{ITEM_RECOVERY, Marker, Parser, TokenSet};

pub(crate) fn source_file(p: &mut Parser<'_>) {
    let m = p.start();
    while !p.at(EOF) {
        if p.at(T![using]) || (p.at(T![global]) && p.nth_at(1, T![using])) {
            using_directive(p);
        } else if p.at(T![namespace]) {
            namespace_decl(p);
        } else if p.at(T!['}']) {
            p.err_and_bump("unmatched `}`");
        } else {
            member(p);
        }
    }
    m.complete(p, SOURCE_FILE);
}

/// `global using static A.B;`, `using X = A.B<C>;`
fn using_directive(p: &mut Parser<'_>) {
    let m = p.start();
    p.eat(T![global]);
    p.bump(T![using]);
    p.eat(T![static]);
    if p.at(IDENT) && p.nth_at(1, T![=]) {
        let a = p.start();
        name(p);
        p.bump(T![=]);
        a.complete(p, USING_ALIAS);
        types::type_ref(p);
    } else {
        types::path(p);
    }
    p.expect(T![;]);
    m.complete(p, USING_DIRECTIVE);
}

/// `namespace A.B;` (file-scoped), or the block form `namespace A.B { ... }`, which a dialect
/// may reject.
fn namespace_decl(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(T![namespace]);
    types::path(p);
    if p.at(T!['{']) {
        member_list(p);
    } else {
        p.expect(T![;]);
    }
    m.complete(p, NAMESPACE_DECL);
}

/// `{ members }`
fn member_list(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(T!['{']);
    while !p.at(T!['}']) && !p.at(EOF) {
        if p.at(T![using]) {
            using_directive(p);
        } else if p.at(T![namespace]) {
            namespace_decl(p);
        } else {
            member(p);
        }
    }
    p.expect(T!['}']);
    m.complete(p, MEMBER_LIST);
}

/// One declaration at namespace level or inside a type.
fn member(p: &mut Parser<'_>) {
    if !p.enter() {
        return;
    }
    member_inner(p);
    p.leave();
}

fn member_inner(p: &mut Parser<'_>) {
    let m = p.start();
    let start = p.pos();
    attribute_lists(p);
    modifiers(p);
    let attrs_or_mods = p.pos() > start;

    match p.current_contextual() {
        T![struct] => type_decl(p, m, T![struct], STRUCT_DECL),
        T![class] => type_decl(p, m, T![class], CLASS_DECL),
        T![interface] => type_decl(p, m, T![interface], INTERFACE_DECL),
        T![record] if matches!(p.nth(1), IDENT | STRUCT_KW | CLASS_KW) => {
            p.bump(T![record]);
            if p.at(T![struct]) || p.at(T![class]) {
                p.bump_any();
            }
            type_decl_rest(p, m, RECORD_DECL);
        }
        T![enum] => enum_decl(p, m),
        T![extension] if matches!(p.nth(1), L_PAREN | LT) => extension_decl(p, m),
        T![type] if p.nth(1) == IDENT => assoc_type_decl(p, m),
        T![implicit] | T![explicit] => conversion_decl(p, m),
        // Constructor: `Name(...)` with no return type.
        IDENT if p.nth(1) == L_PAREN => constructor_decl(p, m),
        _ if types::at_type_start(p) => {
            types::type_ref(p);
            member_after_type(p, m);
        }
        _ => {
            if attrs_or_mods {
                p.error_expected("a declaration");
                m.complete(p, ERROR);
            } else {
                m.abandon(p);
                p.err_recover("expected a declaration", ITEM_RECOVERY);
                // `err_recover` leaves recovery tokens alone; skip one so the loop advances.
                if p.at_ts(ITEM_RECOVERY) || p.at(T!['{']) {
                    let e = p.start();
                    p.bump_any();
                    e.complete(p, ERROR);
                }
            }
        }
    }
}

/// After `Type`: an operator, an indexer, or a named method, property or field.
fn member_after_type(p: &mut Parser<'_>, m: Marker) {
    match p.current() {
        T![operator] => {
            p.bump(T![operator]);
            operator_token(p);
            param_list(p, T!['('], T![')']);
            body(p);
            m.complete(p, OPERATOR_DECL);
        }
        T![this] if p.nth_at(1, T!['[']) => {
            p.bump(T![this]);
            param_list(p, T!['['], T![']']);
            property_body(p);
            m.complete(p, INDEXER_DECL);
        }
        IDENT => {
            let declarator = p.start();
            name(p);
            match p.current() {
                T!['('] | T![<] => {
                    declarator.abandon(p);
                    function_tail(p);
                    m.complete(p, FUNCTION_DECL);
                }
                T!['{'] | T![=>] => {
                    declarator.abandon(p);
                    property_body(p);
                    m.complete(p, PROPERTY_DECL);
                }
                _ => {
                    if p.at(T!['[']) {
                        p.error(
                            "the array length belongs to the type: `float[4] x`, not `float x[4]`",
                        );
                        let e = p.start();
                        while !matches!(p.current(), R_BRACK | SEMICOLON | EOF | R_CURLY) {
                            p.bump_any();
                        }
                        p.eat(T![']']);
                        e.complete(p, ERROR);
                    }
                    if p.eat(T![=]) {
                        exprs::expr(p);
                    }
                    declarator.complete(p, VARIABLE_DECLARATOR);
                    if p.eat(T![,]) {
                        stmts::variable_declarators(p);
                    }
                    p.expect(T![;]);
                    m.complete(p, FIELD_DECL);
                }
            }
        }
        _ => {
            p.error_expected("a member name");
            m.complete(p, ERROR);
        }
    }
}

/// A local function after its attributes and modifiers: `Type Name<T>(params) where ... body`.
pub(super) fn function_rest(p: &mut Parser<'_>, m: Marker) {
    types::type_ref(p);
    name(p);
    function_tail(p);
    m.complete(p, FUNCTION_DECL);
}

/// `<T>(params) where ... body` after a function's name.
fn function_tail(p: &mut Parser<'_>) {
    if p.at(T![<]) {
        type_param_list(p);
    }
    if p.at(T!['(']) {
        param_list(p, T!['('], T![')']);
    } else {
        p.error_expected("`(`");
    }
    where_clauses(p);
    body(p);
}

/// `{ ... }`, `=> expr;`, or `;` (abstract interface members).
fn body(p: &mut Parser<'_>) {
    match p.current() {
        T!['{'] => stmts::block(p),
        T![=>] => {
            expr_body(p);
            p.expect(T![;]);
        }
        T![;] => p.bump(T![;]),
        _ => p.error_expected("`{`, `=>` or `;`"),
    }
}

fn expr_body(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(T![=>]);
    exprs::expr(p);
    m.complete(p, EXPR_BODY);
}

/// Accessors `{ get; set => ...; }` with an optional initializer, or `=> expr;`.
fn property_body(p: &mut Parser<'_>) {
    if p.at(T![=>]) {
        expr_body(p);
        p.expect(T![;]);
        return;
    }
    if !p.at(T!['{']) {
        p.error_expected("`{` or `=>`");
        return;
    }
    let list = p.start();
    p.bump(T!['{']);
    while !p.at(T!['}']) && !p.at(EOF) {
        let before = p.pos();
        let a = p.start();
        attribute_lists(p);
        modifiers(p);
        if p.at(T![get]) || p.at(T![set]) || p.at(T![init]) {
            let kind = p.current_contextual();
            p.bump(kind);
            body(p);
            a.complete(p, ACCESSOR);
        } else {
            a.abandon(p);
            p.err_recover("expected `get` or `set`", TokenSet::new(&[T![;]]));
            p.eat(T![;]);
            if p.pos() == before {
                break;
            }
        }
    }
    p.expect(T!['}']);
    list.complete(p, ACCESSOR_LIST);
    if p.eat(T![=]) {
        exprs::expr(p);
        p.expect(T![;]);
    }
}

/// The operator of an `operator` declaration.
fn operator_token(p: &mut Parser<'_>) {
    const COMPOSITE: &[crate::SyntaxKind] = &[T![>>>], T![>>]];
    if let Some(&op) = COMPOSITE.iter().find(|&&op| p.at(op)) {
        p.bump(op);
        return;
    }
    const OPERATORS: TokenSet = TokenSet::new(&[
        T![+],
        T![-],
        T![*],
        T![/],
        T![%],
        T![&],
        T![|],
        T![^],
        T![<<],
        T![==],
        T![!=],
        T![<],
        T![>],
        T![<=],
        T![>=],
        T![!],
        T![~],
        T![++],
        T![--],
        TRUE_KW,
        FALSE_KW,
    ]);
    if p.at_ts(OPERATORS) {
        p.bump_any();
    } else {
        p.error_expected("an overloadable operator");
    }
}

/// `struct Name<T> : IFoo where T : IBar { ... }` and the other type declarations.
fn type_decl(p: &mut Parser<'_>, m: Marker, keyword: crate::SyntaxKind, kind: crate::SyntaxKind) {
    p.bump(keyword);
    type_decl_rest(p, m, kind);
}

fn type_decl_rest(p: &mut Parser<'_>, m: Marker, kind: crate::SyntaxKind) {
    name(p);
    if p.at(T![<]) {
        type_param_list(p);
    }
    if p.at(T!['(']) {
        // Primary constructor (records); a dialect may reject it.
        param_list(p, T!['('], T![')']);
    }
    if p.at(T![:]) {
        base_list(p);
    }
    where_clauses(p);
    if p.at(T!['{']) {
        member_list(p);
        p.eat(T![;]);
    } else {
        p.expect(T![;]);
    }
    m.complete(p, kind);
}

/// `enum Lobe : uint { Diffuse, Specular = 2 }`
fn enum_decl(p: &mut Parser<'_>, m: Marker) {
    p.bump(T![enum]);
    name(p);
    if p.at(T![:]) {
        base_list(p);
    }
    if p.at(T!['{']) {
        let list = p.start();
        p.bump(T!['{']);
        while !p.at(T!['}']) && !p.at(EOF) {
            let e = p.start();
            attribute_lists(p);
            if !p.at(IDENT) {
                e.abandon(p);
                p.err_recover("expected an enum member", TokenSet::new(&[T![,]]));
                if !p.eat(T![,]) {
                    break;
                }
                continue;
            }
            name(p);
            if p.eat(T![=]) {
                exprs::expr(p);
            }
            e.complete(p, ENUM_MEMBER);
            if !p.at(T!['}']) && !p.expect(T![,]) {
                break;
            }
        }
        p.expect(T!['}']);
        list.complete(p, ENUM_MEMBER_LIST);
        p.eat(T![;]);
    } else {
        p.expect(T!['{']);
    }
    m.complete(p, ENUM_DECL);
}

/// `extension<T>(Vector<T, 3> v) : IFoo where T : IBar { ... }`
fn extension_decl(p: &mut Parser<'_>, m: Marker) {
    p.bump(T![extension]);
    if p.at(T![<]) {
        type_param_list(p);
    }
    if p.at(T!['(']) {
        param_list(p, T!['('], T![')']);
    } else {
        p.error_expected("`(`");
    }
    if p.at(T![:]) {
        base_list(p);
    }
    where_clauses(p);
    if p.at(T!['{']) {
        member_list(p);
    } else {
        p.expect(T!['{']);
    }
    m.complete(p, EXTENSION_DECL);
}

/// `type Params : IParams;` in an interface, `type Params = RockParams;` in an implementation.
fn assoc_type_decl(p: &mut Parser<'_>, m: Marker) {
    p.bump(T![type]);
    name(p);
    if p.at(T![:]) {
        base_list(p);
    }
    if p.eat(T![=]) {
        types::type_ref(p);
    }
    p.expect(T![;]);
    m.complete(p, ASSOC_TYPE_DECL);
}

/// `static implicit operator float3(Color c) => ...;`
fn conversion_decl(p: &mut Parser<'_>, m: Marker) {
    p.bump_any();
    p.expect(T![operator]);
    types::type_ref(p);
    if p.at(T!['(']) {
        param_list(p, T!['('], T![')']);
    } else {
        p.error_expected("`(`");
    }
    body(p);
    m.complete(p, CONVERSION_DECL);
}

/// `Name(params) : this(args) { ... }`
fn constructor_decl(p: &mut Parser<'_>, m: Marker) {
    name(p);
    param_list(p, T!['('], T![')']);
    if p.at(T![:]) {
        let i = p.start();
        p.bump(T![:]);
        if p.at(T![this]) || p.at(T![base]) {
            p.bump_any();
        } else {
            p.error_expected("`this`");
        }
        if p.at(T!['(']) {
            arg_list(p);
        } else {
            p.error_expected("`(`");
        }
        i.complete(p, CONSTRUCTOR_INITIALIZER);
    }
    body(p);
    m.complete(p, CONSTRUCTOR_DECL);
}

/// `<T, const int N>`
fn type_param_list(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(T![<]);
    while !p.at(T![>]) && !p.at(EOF) {
        let t = p.start();
        attribute_lists(p);
        if p.eat(T![const]) {
            types::type_ref(p);
        }
        if !p.at(IDENT) {
            t.abandon(p);
            p.err_recover(
                "expected a type parameter",
                TokenSet::new(&[T![>], T![,], T!['(']]),
            );
            if !p.eat(T![,]) {
                break;
            }
            continue;
        }
        name(p);
        t.complete(p, TYPE_PARAM);
        if !p.at(T![>]) && !p.expect(T![,]) {
            break;
        }
    }
    p.expect(T![>]);
    m.complete(p, TYPE_PARAM_LIST);
}

/// `: IFoo, IBar<T>`
fn base_list(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(T![:]);
    loop {
        types::type_ref(p);
        if !p.eat(T![,]) {
            break;
        }
    }
    m.complete(p, BASE_LIST);
}

/// `where T : IFoo, IBar where M.Params : IParams`
fn where_clauses(p: &mut Parser<'_>) {
    while p.at(T![where]) {
        let m = p.start();
        p.bump(T![where]);
        types::type_ref(p);
        p.expect(T![:]);
        loop {
            if p.at(T![struct]) || p.at(T![class]) {
                // C#'s `struct`/`class` constraints.
                p.bump_any();
            } else {
                types::type_ref(p);
            }
            if !p.eat(T![,]) {
                break;
            }
        }
        m.complete(p, WHERE_CLAUSE);
    }
}
