//! Expressions, with C#'s precedence (lowest first):
//! assignment and lambda, `?:`, `||`, `&&`, `|`, `^`, `&`, `== !=`, `< > <= >= is`,
//! `<< >> >>>`, `+ -`, `* / %`, `switch` and `with`, `..`, unary, primary.

use super::{arg_list, bracket_arg_list, name, name_ref, patterns, scan, stmts, types};
use crate::SyntaxKind::{self, *};
use crate::T;
use crate::parser::{CompletedMarker, Parser, TokenSet};

pub(crate) fn can_start_expr(kind: SyntaxKind) -> bool {
    kind.is_literal()
        || kind.is_predefined_type()
        || matches!(
            kind,
            IDENT
                | L_PAREN
                | L_BRACK
                | L_CURLY
                | MINUS
                | PLUS
                | BANG
                | TILDE
                | PLUS2
                | MINUS2
                | DOT2
                | THIS_KW
                | DEFAULT_KW
                | NEW_KW
                | INTERP_START
        )
}

/// A full expression, including assignment and lambdas. Reports an error and returns `None`
/// (consuming nothing) when there is no expression here.
pub(crate) fn expr(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    if !p.enter() {
        return None;
    }
    let result = assignment(p);
    p.leave();
    result
}

const ASSIGN_OPS: &[SyntaxKind] = &[
    T![=],
    T![+=],
    T![-=],
    T![*=],
    T![/=],
    T![%=],
    T![&=],
    T![|=],
    T![^=],
    T![<<=],
    T![>>=],
    T![>>>=],
];

fn assignment(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    if is_lambda_start(p) {
        return Some(lambda(p));
    }
    let lhs = conditional(p)?;
    if let Some(&op) = ASSIGN_OPS.iter().find(|&&op| p.at(op)) {
        // `a >= b` lexes as `>=`, which is not an assignment; `>>=` is glued from `>` `>=`.
        let m = lhs.precede(p);
        p.bump(op);
        expr(p);
        return Some(m.complete(p, BIN_EXPR));
    }
    Some(lhs)
}

fn conditional(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    let cond = binary(p, 1)?;
    if !p.at(T![?]) {
        return Some(cond);
    }
    let m = cond.precede(p);
    p.bump(T![?]);
    expr(p);
    p.expect(T![:]);
    expr(p);
    Some(m.complete(p, CONDITIONAL_EXPR))
}

/// Precedence of the binary operator at the cursor (higher binds tighter).
fn binary_op(p: &Parser<'_>) -> Option<(SyntaxKind, u8)> {
    const TABLE: &[(SyntaxKind, u8)] = &[
        (T![||], 1),
        (T![&&], 2),
        (T![|], 3),
        (T![^], 4),
        (T![&], 5),
        (T![==], 6),
        (T![!=], 6),
        (T![<=], 7),
        (T![>=], 7),
        (T![<], 7),
        (T![<<], 8),
        (T![>>>], 8),
        (T![>>], 8),
        (T![+], 9),
        (T![-], 9),
        (T![*], 10),
        (T![/], 10),
        (T![%], 10),
    ];
    if p.at(T![>]) {
        // Glued forms first; `>>=` and `>>>=` are assignments, not binary operators here.
        if p.at(T![>>>=]) || p.at(T![>>=]) {
            return None;
        }
        if p.at(T![>>>]) {
            return Some((T![>>>], 8));
        }
        if p.at(T![>>]) {
            return Some((T![>>], 8));
        }
        if p.no_gt {
            return None;
        }
        return Some((T![>], 7));
    }
    if p.no_gt && p.at(T![>=]) {
        return None;
    }
    TABLE.iter().copied().find(|&(op, _)| p.at(op))
}

const IS_PRECEDENCE: u8 = 7;
/// Constant patterns are parsed at shift precedence, so `x is < 3` and `a or b` stay patterns.
pub(super) const SHIFT_PRECEDENCE: u8 = 8;

pub(super) fn binary(p: &mut Parser<'_>, min: u8) -> Option<CompletedMarker> {
    let mut lhs = switch_with(p)?;
    loop {
        if p.at(T![is]) && IS_PRECEDENCE >= min {
            let m = lhs.precede(p);
            p.bump(T![is]);
            patterns::pattern(p);
            lhs = m.complete(p, IS_EXPR);
            continue;
        }
        if p.at(AS_KW) && IS_PRECEDENCE >= min {
            let m = lhs.precede(p);
            p.bump(AS_KW);
            types::type_ref(p);
            lhs = m.complete(p, AS_EXPR);
            continue;
        }
        let Some((op, prec)) = binary_op(p) else {
            break;
        };
        if prec < min {
            break;
        }
        let m = lhs.precede(p);
        p.bump(op);
        binary(p, prec + 1);
        lhs = m.complete(p, BIN_EXPR);
    }
    Some(lhs)
}

/// `x switch { ... }` and `x with { ... }` bind tighter than `*`.
fn switch_with(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    let mut lhs = range(p)?;
    loop {
        if p.at(T![switch]) {
            let m = lhs.precede(p);
            p.bump(T![switch]);
            switch_arms(p);
            lhs = m.complete(p, SWITCH_EXPR);
        } else if p.at(T![with]) && p.nth_at(1, T!['{']) {
            let m = lhs.precede(p);
            p.bump(T![with]);
            initializer(p);
            lhs = m.complete(p, WITH_EXPR);
        } else {
            return Some(lhs);
        }
    }
}

/// `a..b`, `..b`, `a..`, `..`
fn range(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    if p.at(T![..]) {
        let m = p.start();
        p.bump(T![..]);
        if can_start_expr(p.current()) && !p.at(T!['{']) {
            unary(p);
        }
        return Some(m.complete(p, RANGE_EXPR));
    }
    let lhs = unary(p)?;
    if !p.at(T![..]) {
        return Some(lhs);
    }
    let m = lhs.precede(p);
    p.bump(T![..]);
    if can_start_expr(p.current()) && !p.at(T!['{']) {
        unary(p);
    }
    Some(m.complete(p, RANGE_EXPR))
}

fn unary(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    if !p.enter() {
        return None;
    }
    let result = unary_inner(p);
    p.leave();
    result
}

fn unary_inner(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    const PREFIX: &[SyntaxKind] = &[T![-], T![+], T![!], T![~], T![++], T![--]];
    if let Some(&op) = PREFIX.iter().find(|&&op| p.at(op)) {
        let m = p.start();
        p.bump(op);
        unary(p);
        return Some(m.complete(p, PREFIX_EXPR));
    }
    if p.at(T!['(']) && scan::is_cast(p) {
        let m = p.start();
        p.bump(T!['(']);
        types::type_ref(p);
        p.expect(T![')']);
        unary(p);
        return Some(m.complete(p, CAST_EXPR));
    }
    let lhs = primary(p)?;
    Some(postfix(p, lhs))
}

fn postfix(p: &mut Parser<'_>, mut lhs: CompletedMarker) -> CompletedMarker {
    loop {
        lhs = match p.current() {
            T![.] => {
                let m = lhs.precede(p);
                p.bump(T![.]);
                name_ref(p);
                if p.at(T![<]) && scan::is_generic_args_in_expr(p) {
                    types::generic_arg_list(p);
                }
                m.complete(p, MEMBER_EXPR)
            }
            T!['('] => {
                let m = lhs.precede(p);
                arg_list(p);
                m.complete(p, CALL_EXPR)
            }
            T!['['] => {
                let m = lhs.precede(p);
                bracket_arg_list(p);
                m.complete(p, INDEX_EXPR)
            }
            T![++] | T![--] => {
                let m = lhs.precede(p);
                p.bump_any();
                m.complete(p, POSTFIX_EXPR)
            }
            T!['{'] if matches!(lhs.kind(), NAME_EXPR | MEMBER_EXPR | TYPE_EXPR) => {
                let m = lhs.precede(p);
                initializer(p);
                m.complete(p, OBJECT_INIT_EXPR)
            }
            _ => return lhs,
        };
    }
}

fn primary(p: &mut Parser<'_>) -> Option<CompletedMarker> {
    let kind = p.current();
    if kind.is_literal() {
        let m = p.start();
        p.bump_any();
        return Some(m.complete(p, LITERAL));
    }
    if kind.is_predefined_type() {
        let m = p.start();
        let t = p.start();
        p.bump_any();
        t.complete(p, PREDEFINED_TYPE);
        return Some(m.complete(p, TYPE_EXPR));
    }
    Some(match kind {
        IDENT if p.at(T![var]) && p.nth_at(1, T!['(']) && is_tuple_designation(p) => {
            declaration_expr(p)
        }
        IDENT => {
            let m = p.start();
            name_ref(p);
            if p.at(T![<]) && scan::is_generic_args_in_expr(p) {
                types::generic_arg_list(p);
            }
            m.complete(p, NAME_EXPR)
        }
        T!['('] => paren_or_tuple(p),
        T!['['] => collection(p),
        T!['{'] => {
            // Target-typed initializer: `Light l = { Color = c };`
            let m = p.start();
            initializer(p);
            m.complete(p, OBJECT_INIT_EXPR)
        }
        T![this] => {
            let m = p.start();
            p.bump(T![this]);
            m.complete(p, THIS_EXPR)
        }
        T![default] => {
            let m = p.start();
            p.bump(T![default]);
            if p.at(T!['(']) {
                p.bump(T!['(']);
                types::type_ref(p);
                p.expect(T![')']);
            }
            m.complete(p, DEFAULT_EXPR)
        }
        INTERP_START => interpolated_string(p),
        T![new] => {
            // `new T(args)`, `new T { ... }`, `new T(args) { ... }`, `new(args)`
            let m = p.start();
            p.bump(T![new]);
            if !p.at(T!['(']) {
                types::type_ref(p);
            }
            if p.at(T!['(']) {
                arg_list(p);
            }
            if p.at(T!['{']) {
                initializer(p);
            }
            m.complete(p, NEW_EXPR)
        }
        T![base] => {
            let m = p.start();
            p.bump(T![base]);
            m.complete(p, BASE_EXPR)
        }
        TYPEOF_KW | SIZEOF_KW => {
            let m = p.start();
            p.bump_any();
            p.expect(T!['(']);
            types::type_ref(p);
            p.expect(T![')']);
            m.complete(
                p,
                if kind == TYPEOF_KW {
                    TYPEOF_EXPR
                } else {
                    SIZEOF_EXPR
                },
            )
        }
        THROW_KW => {
            let m = p.start();
            p.bump_any();
            if can_start_expr(p.current()) {
                expr(p);
            }
            m.complete(p, THROW_EXPR)
        }
        STACKALLOC_KW | CHECKED_KW | UNCHECKED_KW | DELEGATE_KW => {
            let text = p.raw_text(0).to_string();
            p.err_and_bump(format!("`{text}` expressions are not supported"));
            return None;
        }
        _ => {
            p.error_expected("an expression");
            return None;
        }
    })
}

/// `var (a, b)` with only names inside the parentheses.
fn is_tuple_designation(p: &Parser<'_>) -> bool {
    let mut i = 2;
    loop {
        if p.raw(i) != IDENT {
            return false;
        }
        i += 1;
        match p.raw(i) {
            COMMA => i += 1,
            R_PAREN => return true,
            _ => return false,
        }
    }
}

/// `var x`, `float3 n`, or `var (a, b)`: declares locals inside an expression (after `out`,
/// or on the left of a deconstructing assignment).
pub(super) fn declaration_expr(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    types::type_ref(p);
    if p.at(T!['(']) {
        let d = p.start();
        p.bump(T!['(']);
        while p.at(IDENT) {
            name(p);
            if !p.eat(T![,]) {
                break;
            }
        }
        p.expect(T![')']);
        d.complete(p, TUPLE_DESIGNATION);
    } else {
        name(p);
    }
    m.complete(p, DECL_EXPR)
}

/// `(e)` or `(a, b)` / `(Wi: a, Pdf: b)`.
fn paren_or_tuple(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(T!['(']);
    let no_gt = std::mem::replace(&mut p.no_gt, false);
    if p.at(T![')']) {
        p.error("expected an expression; Marlin shader has no empty tuple");
        p.bump(T![')']);
        p.no_gt = no_gt;
        return m.complete(p, TUPLE_EXPR);
    }
    let mut elements = 0;
    let mut named = false;
    loop {
        let a = p.start();
        if p.at(IDENT) && p.nth_at(1, T![:]) {
            let n = p.start();
            name_ref(p);
            p.bump(T![:]);
            n.complete(p, NAME_COLON);
            named = true;
        }
        let parsed = if p.at(T![var]) && p.nth(1) == IDENT {
            Some(declaration_expr(p))
        } else if scan::local_decl_name(p).is_some_and(|n| matches!(p.raw(n + 1), COMMA | R_PAREN))
        {
            // `(float a, var b) = t;`
            Some(declaration_expr(p))
        } else {
            expr(p)
        };
        elements += 1;
        if elements == 1 && !named && !p.at(T![,]) {
            a.abandon(p);
            break;
        }
        a.complete(p, ARG);
        if parsed.is_none() || !p.eat(T![,]) {
            break;
        }
    }
    p.expect(T![')']);
    p.no_gt = no_gt;
    m.complete(
        p,
        if elements == 1 && !named {
            PAREN_EXPR
        } else {
            TUPLE_EXPR
        },
    )
}

/// `[a, b, c]`
fn collection(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(T!['[']);
    let no_gt = std::mem::replace(&mut p.no_gt, false);
    while !p.at(T![']']) && !p.at(EOF) {
        if expr(p).is_none() {
            p.err_recover("", TokenSet::new(&[T![']'], T![,], T![;]]));
            if !p.at(T![,]) {
                break;
            }
        }
        if !p.at(T![']']) && !p.expect(T![,]) {
            break;
        }
    }
    p.expect(T![']']);
    p.no_gt = no_gt;
    m.complete(p, COLLECTION_EXPR)
}

/// `{ A = 1, B = 2 }`
pub(super) fn initializer(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(T!['{']);
    let no_gt = std::mem::replace(&mut p.no_gt, false);
    while !p.at(T!['}']) && !p.at(EOF) {
        let f = p.start();
        if p.at(IDENT) && p.nth_at(1, T![=]) && !p.nth_at(1, T![==]) {
            name_ref(p);
            p.bump(T![=]);
            expr(p);
            f.complete(p, FIELD_INIT);
        } else {
            p.error("expected `Field = value`");
            if expr(p).is_none() {
                p.err_recover("", TokenSet::new(&[T![,], T![;]]));
            }
            f.complete(p, ERROR);
        }
        if !p.at(T!['}']) && !p.expect(T![,]) {
            break;
        }
    }
    p.expect(T!['}']);
    p.no_gt = no_gt;
    m.complete(p, INITIALIZER);
}

/// `{ pattern when cond => expr, ... }` after `switch`.
fn switch_arms(p: &mut Parser<'_>) {
    if !p.at(T!['{']) {
        p.error_expected("`{`");
        return;
    }
    p.bump(T!['{']);
    let no_gt = std::mem::replace(&mut p.no_gt, false);
    while !p.at(T!['}']) && !p.at(EOF) {
        let arm = p.start();
        patterns::pattern(p);
        if p.at(T![when]) {
            let w = p.start();
            p.bump(T![when]);
            expr(p);
            w.complete(p, WHEN_CLAUSE);
        }
        p.expect(T![=>]);
        expr(p);
        arm.complete(p, SWITCH_ARM);
        if !p.at(T!['}']) && !p.expect(T![,]) {
            break;
        }
    }
    p.expect(T!['}']);
    p.no_gt = no_gt;
}

/// `$"text {expr,align:format} text"`
fn interpolated_string(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    p.bump(INTERP_START);
    loop {
        match p.current() {
            INTERP_TEXT => p.bump(INTERP_TEXT),
            T!['{'] => {
                let h = p.start();
                p.bump(T!['{']);
                expr(p);
                if p.eat(T![,]) {
                    expr(p);
                }
                if p.eat(T![:]) {
                    p.eat(INTERP_FORMAT);
                }
                if !p.eat(T!['}']) {
                    p.error_expected("`}`");
                    // Skip whatever is left of the hole, but not past the end of the line: the
                    // lexer ends an unterminated string there.
                    while !matches!(p.current(), R_CURLY | INTERP_TEXT | INTERP_END | EOF)
                        && !p.newline_before()
                    {
                        p.err_and_bump("unexpected token in an interpolation");
                    }
                    p.eat(T!['}']);
                }
                h.complete(p, INTERPOLATION);
            }
            INTERP_END => {
                p.bump(INTERP_END);
                break;
            }
            _ => break, // The lexer reported the unterminated string.
        }
    }
    m.complete(p, INTERPOLATED_STRING)
}

fn is_lambda_start(p: &Parser<'_>) -> bool {
    match p.raw(0) {
        IDENT => p.raw(1) == FAT_ARROW,
        L_PAREN => scan::is_paren_lambda(p),
        STATIC_KW => matches!(p.raw(1), IDENT | L_PAREN),
        _ => false,
    }
}

/// `x => ...`, `(x, y) => ...`, `(float x) => { ... }`
fn lambda(p: &mut Parser<'_>) -> CompletedMarker {
    let m = p.start();
    if p.at(T![static]) {
        p.bump(T![static]);
    }
    if p.at(IDENT) {
        let params = p.start();
        let param = p.start();
        name(p);
        param.complete(p, PARAM);
        params.complete(p, LAMBDA_PARAM_LIST);
    } else {
        let params = p.start();
        p.expect(T!['(']);
        while !p.at(T![')']) && !p.at(EOF) {
            let param = p.start();
            if p.at(T![in]) || p.at(T![out]) || p.at(T![ref]) {
                let mods = p.start();
                p.bump_any();
                mods.complete(p, MODIFIERS);
            }
            if scan::local_decl_name(p).is_some() {
                types::type_ref(p);
            }
            name(p);
            param.complete(p, PARAM);
            if !p.at(T![')']) && !p.expect(T![,]) {
                break;
            }
        }
        p.expect(T![')']);
        params.complete(p, LAMBDA_PARAM_LIST);
    }
    p.expect(T![=>]);
    if p.at(T!['{']) {
        stmts::block(p);
    } else {
        expr(p);
    }
    m.complete(p, LAMBDA_EXPR)
}
