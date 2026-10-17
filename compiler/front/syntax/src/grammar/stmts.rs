//! Statements and blocks.

use super::{attribute_lists, exprs, items, modifiers, name, patterns, scan, types};
use crate::SyntaxKind::*;
use crate::T;
use crate::parser::{Parser, TokenSet};

/// Tokens that cannot start a statement but do start a member: a block that meets one is
/// missing its `}`, so it stops and lets the enclosing declaration list carry on.
const BLOCK_END_RECOVERY: TokenSet = TokenSet::new(&[
    PUBLIC_KW,
    INTERNAL_KW,
    PRIVATE_KW,
    PROTECTED_KW,
    STRUCT_KW,
    CLASS_KW,
    INTERFACE_KW,
    ENUM_KW,
    NAMESPACE_KW,
    USING_KW,
]);

/// `{ statements }`
pub(super) fn block(p: &mut Parser<'_>) {
    let m = p.start();
    if !p.expect(T!['{']) {
        m.complete(p, BLOCK);
        return;
    }
    while !p.at(T!['}']) && !p.at(EOF) {
        if p.at_ts(BLOCK_END_RECOVERY) {
            break;
        }
        statement(p);
    }
    p.expect(T!['}']);
    m.complete(p, BLOCK);
}

pub(super) fn statement(p: &mut Parser<'_>) {
    if !p.enter() {
        return;
    }
    statement_inner(p);
    p.leave();
}

fn statement_inner(p: &mut Parser<'_>) {
    match p.current() {
        T!['{'] => block(p),
        T![;] => {
            let m = p.start();
            p.bump(T![;]);
            m.complete(p, EMPTY_STMT);
        }
        T![if] => if_stmt(p),
        T![const] if p.nth_at(1, T![if]) => if_stmt(p),
        T![for] => for_stmt(p),
        T![foreach] => foreach_stmt(p),
        T![while] => while_stmt(p),
        T![do] => do_stmt(p),
        T![switch] => switch_stmt(p),
        T![break] => keyword_stmt(p, T![break], BREAK_STMT),
        T![continue] => keyword_stmt(p, T![continue], CONTINUE_STMT),
        T![return] => {
            let m = p.start();
            p.bump(T![return]);
            if !p.at(T![;]) {
                exprs::expr(p);
            }
            p.expect(T![;]);
            m.complete(p, RETURN_STMT);
        }
        GOTO_KW | TRY_KW | CATCH_KW | FINALLY_KW | LOCK_KW | UNSAFE_KW | FIXED_KW => {
            let text = p.raw_text(0).to_string();
            p.err_and_bump(format!("`{text}` statements are not supported"));
        }
        T!['['] | T![const] | T![static] | T![readonly] => local_decl_or_function(p),
        T![default] if p.nth_at(1, T![:]) => {
            p.err_and_bump("`default:` outside a `switch` statement");
        }
        _ if scan::local_decl_name(p).is_some() => local_decl_or_function(p),
        _ => expr_stmt(p),
    }
}

fn keyword_stmt(p: &mut Parser<'_>, keyword: crate::SyntaxKind, kind: crate::SyntaxKind) {
    let m = p.start();
    p.bump(keyword);
    p.expect(T![;]);
    m.complete(p, kind);
}

fn expr_stmt(p: &mut Parser<'_>) {
    let m = p.start();
    if exprs::expr(p).is_none() {
        m.abandon(p);
        // `expr` reported the problem without consuming; make progress.
        if !p.at(T!['}']) && !p.at(EOF) {
            let e = p.start();
            p.bump_any();
            e.complete(p, ERROR);
        }
        return;
    }
    p.expect(T![;]);
    m.complete(p, EXPR_STMT);
}

/// `[Precise] float x = 1, y;`, `const int N = 4;`, or a local function `float F(float x) => x;`.
fn local_decl_or_function(p: &mut Parser<'_>) {
    let m = p.start();
    attribute_lists(p);
    modifiers(p);
    let is_function =
        scan::local_decl_name(p).is_some_and(|name| matches!(p.raw(name + 1), L_PAREN | LT));
    if is_function {
        items::function_rest(p, m);
        return;
    }
    if types::type_ref(p).is_none() {
        m.complete(p, LOCAL_DECL_STMT);
        if !p.at(T!['}']) {
            p.bump_any();
        }
        return;
    }
    variable_declarators(p);
    p.expect(T![;]);
    m.complete(p, LOCAL_DECL_STMT);
}

/// `a = 1, b, c = 2`
pub(super) fn variable_declarators(p: &mut Parser<'_>) {
    loop {
        let d = p.start();
        name(p);
        if p.eat(T![=]) {
            exprs::expr(p);
        }
        d.complete(p, VARIABLE_DECLARATOR);
        if !p.eat(T![,]) {
            break;
        }
    }
}

/// `if (c) s else s`, `const if (c) s else s`
fn if_stmt(p: &mut Parser<'_>) {
    let m = p.start();
    p.eat(T![const]);
    p.bump(T![if]);
    paren_condition(p);
    statement(p);
    if p.at(T![else]) {
        let e = p.start();
        p.bump(T![else]);
        statement(p);
        e.complete(p, ELSE_CLAUSE);
    }
    m.complete(p, IF_STMT);
}

fn paren_condition(p: &mut Parser<'_>) {
    p.expect(T!['(']);
    exprs::expr(p);
    p.expect(T![')']);
}

/// `for (init; cond; update) s`
fn for_stmt(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(T![for]);
    p.expect(T!['(']);
    if !p.at(T![;]) {
        let init = p.start();
        if scan::local_decl_name(p).is_some() {
            let d = p.start();
            types::type_ref(p);
            variable_declarators(p);
            d.complete(p, LOCAL_DECL_STMT);
        } else {
            expr_list(p);
        }
        init.complete(p, FOR_INIT);
    }
    p.expect(T![;]);
    if !p.at(T![;]) {
        exprs::expr(p);
    }
    p.expect(T![;]);
    if !p.at(T![')']) {
        let update = p.start();
        expr_list(p);
        update.complete(p, FOR_UPDATE);
    }
    p.expect(T![')']);
    statement(p);
    m.complete(p, FOR_STMT);
}

fn expr_list(p: &mut Parser<'_>) {
    while exprs::expr(p).is_some() && p.eat(T![,]) {}
}

/// `foreach (var i in 0..n) s`
fn foreach_stmt(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(T![foreach]);
    p.expect(T!['(']);
    if p.at(T![var]) && p.nth_at(1, T!['(']) {
        exprs::declaration_expr(p);
    } else {
        types::type_ref(p);
        name(p);
    }
    p.expect(T![in]);
    exprs::expr(p);
    p.expect(T![')']);
    statement(p);
    m.complete(p, FOREACH_STMT);
}

fn while_stmt(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(T![while]);
    paren_condition(p);
    statement(p);
    m.complete(p, WHILE_STMT);
}

fn do_stmt(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(T![do]);
    statement(p);
    p.expect(T![while]);
    paren_condition(p);
    p.expect(T![;]);
    m.complete(p, DO_STMT);
}

/// `switch (x) { case A: case B when c: ...; break; default: ...; }`
fn switch_stmt(p: &mut Parser<'_>) {
    let m = p.start();
    p.bump(T![switch]);
    paren_condition(p);
    if p.expect(T!['{']) {
        while !p.at(T!['}']) && !p.at(EOF) {
            if p.at_ts(BLOCK_END_RECOVERY) {
                break;
            }
            switch_section(p);
        }
        p.expect(T!['}']);
    }
    m.complete(p, SWITCH_STMT);
}

fn at_switch_label(p: &Parser<'_>) -> bool {
    p.at(T![case]) || (p.at(T![default]) && p.nth_at(1, T![:]))
}

fn switch_section(p: &mut Parser<'_>) {
    let m = p.start();
    if !at_switch_label(p) {
        p.error("expected `case` or `default:`");
    }
    while at_switch_label(p) {
        let l = p.start();
        if p.eat(T![case]) {
            patterns::pattern(p);
            if p.at(T![when]) {
                let w = p.start();
                p.bump(T![when]);
                exprs::expr(p);
                w.complete(p, WHEN_CLAUSE);
            }
            p.expect(T![:]);
            l.complete(p, CASE_LABEL);
        } else {
            p.bump(T![default]);
            p.bump(T![:]);
            l.complete(p, DEFAULT_LABEL);
        }
    }
    while !at_switch_label(p) && !p.at(T!['}']) && !p.at(EOF) && !p.at_ts(BLOCK_END_RECOVERY) {
        statement(p);
    }
    m.complete(p, SWITCH_SECTION);
}
