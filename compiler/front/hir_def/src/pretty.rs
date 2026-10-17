//! HIR as C#-like text, for tests and debugging. Binary operators are fully parenthesized so
//! the tree shape is visible.

use std::fmt::Write;

use crate::body::Body;
use crate::signature::{Bound, ParamSig, Signature};
use crate::store::*;

/// A body: its parameters, its constructor initializer and its code.
pub fn pretty_body(body: &Body) -> String {
    let p = Printer { store: &body.store };
    let mut out = String::new();
    let params: Vec<_> = body.params.iter().map(|&b| p.binding(b)).collect();
    let _ = writeln!(out, "params: {}", params.join(", "));
    if let Some(init) = body.constructor_initializer {
        let _ = writeln!(out, "initializer: {}", p.expr(init));
    }
    match body.root {
        Some(LambdaBody::Expr(e)) => {
            let _ = writeln!(out, "=> {}", p.expr(e));
        }
        Some(LambdaBody::Block(s)) => p.stmt(&mut out, s, 0),
        None => out.push_str("<no body>\n"),
    }
    out
}

/// A signature on one line per part.
pub fn pretty_signature(sig: &Signature) -> String {
    let p = Printer { store: &sig.store };
    let mut out = String::new();
    for a in &sig.attributes {
        let _ = writeln!(out, "attribute {}", p.attribute(a));
    }
    if !sig.generic_params.is_empty() {
        let params: Vec<_> = sig
            .generic_params
            .iter()
            .map(|g| match g.const_ty {
                Some(t) => format!("const {} {}", p.ty(t), g.name),
                None => g.name.to_string(),
            })
            .collect();
        let _ = writeln!(out, "generics <{}>", params.join(", "));
    }
    if let Some(receiver) = &sig.receiver {
        let _ = writeln!(out, "receiver {}", p.param(receiver));
    }
    if !sig.params.is_empty() {
        let params: Vec<_> = sig.params.iter().map(|x| p.param(x)).collect();
        let _ = writeln!(out, "params ({})", params.join(", "));
    }
    if let Some(op) = &sig.operator {
        let _ = writeln!(out, "operator {op}");
    }
    if let Some(ty) = sig.ty {
        let _ = writeln!(out, "type {}", p.ty(ty));
    }
    if !sig.bases.is_empty() {
        let bases: Vec<_> = sig.bases.iter().map(|&t| p.ty(t)).collect();
        let _ = writeln!(out, "bases {}", bases.join(", "));
    }
    for w in &sig.where_clauses {
        let bounds: Vec<_> = w
            .bounds
            .iter()
            .map(|b| match b {
                Bound::Type(t) => p.ty(*t),
                Bound::Struct => "struct".into(),
                Bound::Class => "class".into(),
            })
            .collect();
        let _ = writeln!(out, "where {} : {}", p.ty(w.target), bounds.join(", "));
    }
    out
}

struct Printer<'a> {
    store: &'a ExprStore,
}

impl Printer<'_> {
    fn binding(&self, b: BindingId) -> String {
        self.store.bindings[b].name.to_string()
    }

    fn param(&self, param: &ParamSig) -> String {
        let mut s = String::new();
        for a in &param.attributes {
            let _ = write!(s, "[{}] ", self.attribute(a));
        }
        if param.is_this {
            s.push_str("this ");
        }
        if param.is_params {
            s.push_str("params ");
        }
        if let Some(m) = param.modifier {
            s.push_str(ref_kind(m));
            s.push(' ');
        }
        s.push_str(&self.ty(param.ty));
        if let Some(name) = &param.name {
            let _ = write!(s, " {name}");
        }
        if let Some(d) = param.default {
            let _ = write!(s, " = {}", self.expr(d));
        }
        s
    }

    fn attribute(&self, a: &Attribute) -> String {
        if a.args.is_empty() {
            self.path(&a.path)
        } else {
            format!("{}({})", self.path(&a.path), self.args(&a.args))
        }
    }

    fn ty(&self, t: TypeRefId) -> String {
        match &self.store.types[t] {
            TypeRef::Missing => "<missing>".into(),
            TypeRef::Predefined(p) => p.keyword().into(),
            TypeRef::Path(path) => self.path(path),
            TypeRef::Array { element, len } => format!(
                "{}[{}]",
                self.ty(*element),
                len.map(|l| self.expr(l)).unwrap_or_default()
            ),
            TypeRef::Tuple(items) => format!(
                "({})",
                items
                    .iter()
                    .map(|(t, n)| match n {
                        Some(n) => format!("{} {n}", self.ty(*t)),
                        None => self.ty(*t),
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }

    fn path(&self, path: &HirPath) -> String {
        let mut s = String::new();
        if path.global {
            s.push_str("global::");
        }
        for (i, segment) in path.segments.iter().enumerate() {
            if i > 0 {
                s.push('.');
            }
            s.push_str(segment.name.as_str());
            if let Some(args) = &segment.args {
                s.push_str(&self.generic_args(args));
            }
        }
        s
    }

    fn generic_args(&self, args: &[GenericArg]) -> String {
        let args: Vec<_> = args
            .iter()
            .map(|a| match a {
                GenericArg::Type(t) => self.ty(*t),
                GenericArg::Const(e) => self.expr(*e),
            })
            .collect();
        format!("<{}>", args.join(", "))
    }

    fn args(&self, args: &[Arg]) -> String {
        args.iter()
            .map(|a| {
                let mut s = String::new();
                if let Some(name) = &a.name {
                    let _ = write!(s, "{name}: ");
                }
                if let Some(m) = a.modifier {
                    s.push_str(ref_kind(m));
                    s.push(' ');
                }
                s.push_str(&self.expr(a.expr));
                s
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn inits(&self, inits: &[FieldInit]) -> String {
        let inits: Vec<_> = inits
            .iter()
            .map(|i| format!("{} = {}", i.name, self.expr(i.value)))
            .collect();
        format!("{{ {} }}", inits.join(", "))
    }

    fn local_params(&self, params: &[LocalParam]) -> String {
        params
            .iter()
            .map(|p| {
                let mut s = String::new();
                if let Some(m) = p.modifier {
                    s.push_str(ref_kind(m));
                    s.push(' ');
                }
                if let Some(t) = p.ty {
                    s.push_str(&self.ty(t));
                    s.push(' ');
                }
                s.push_str(&self.binding(p.binding));
                s
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn expr(&self, e: ExprId) -> String {
        match &self.store.exprs[e] {
            Expr::Missing => "<missing>".into(),
            Expr::Literal(l) => match l {
                Literal::Number(t) | Literal::String(t) | Literal::Char(t) => t.to_string(),
                Literal::Bool(b) => b.to_string(),
                Literal::Null => "null".into(),
            },
            Expr::Interpolated(parts) => {
                let mut s = String::from("$\"");
                for part in parts {
                    match part {
                        InterpolationPart::Text(t) => s.push_str(t),
                        InterpolationPart::Hole {
                            expr,
                            alignment,
                            format,
                        } => {
                            let _ = write!(s, "{{{}", self.expr(*expr));
                            if let Some(a) = alignment {
                                let _ = write!(s, ",{}", self.expr(*a));
                            }
                            if let Some(f) = format {
                                let _ = write!(s, ":{f}");
                            }
                            s.push('}');
                        }
                    }
                }
                s.push('"');
                s
            }
            Expr::Path(p) => self.path(p),
            Expr::Type(t) => self.ty(*t),
            Expr::Member {
                receiver,
                name,
                args,
            } => format!(
                "{}.{name}{}",
                self.expr(*receiver),
                args.as_ref()
                    .map(|a| self.generic_args(a))
                    .unwrap_or_default()
            ),
            Expr::Call { callee, args } => format!("{}({})", self.expr(*callee), self.args(args)),
            Expr::Index { base, args } => format!("{}[{}]", self.expr(*base), self.args(args)),
            Expr::Unary { op, operand } => {
                let o = self.expr(*operand);
                match op {
                    UnaryOp::Neg => format!("-{o}"),
                    UnaryOp::Plus => format!("+{o}"),
                    UnaryOp::Not => format!("!{o}"),
                    UnaryOp::BitNot => format!("~{o}"),
                    UnaryOp::PreInc => format!("++{o}"),
                    UnaryOp::PreDec => format!("--{o}"),
                    UnaryOp::PostInc => format!("{o}++"),
                    UnaryOp::PostDec => format!("{o}--"),
                }
            }
            Expr::Binary { op, lhs, rhs } => {
                format!("({} {} {})", self.expr(*lhs), op.symbol(), self.expr(*rhs))
            }
            Expr::Assign { op, target, value } => format!(
                "{} {}= {}",
                self.expr(*target),
                op.map(|o| o.symbol()).unwrap_or_default(),
                self.expr(*value)
            ),
            Expr::Conditional {
                cond,
                then,
                otherwise,
            } => format!(
                "({} ? {} : {})",
                self.expr(*cond),
                self.expr(*then),
                self.expr(*otherwise)
            ),
            Expr::Cast { ty, expr } => format!("(({}){})", self.ty(*ty), self.expr(*expr)),
            Expr::Range { start, end } => format!(
                "{}..{}",
                start.map(|e| self.expr(e)).unwrap_or_default(),
                end.map(|e| self.expr(e)).unwrap_or_default()
            ),
            Expr::Is { expr, pat } => format!("({} is {})", self.expr(*expr), self.pat(*pat)),
            Expr::As { expr, ty } => format!("({} as {})", self.expr(*expr), self.ty(*ty)),
            Expr::Switch { scrutinee, arms } => {
                let arms: Vec<_> = arms
                    .iter()
                    .map(|a| {
                        let guard = a
                            .guard
                            .map(|g| format!(" when {}", self.expr(g)))
                            .unwrap_or_default();
                        format!("{}{guard} => {}", self.pat(a.pat), self.expr(a.expr))
                    })
                    .collect();
                format!(
                    "({} switch {{ {} }})",
                    self.expr(*scrutinee),
                    arms.join(", ")
                )
            }
            Expr::With { expr, inits } => {
                format!("({} with {})", self.expr(*expr), self.inits(inits))
            }
            Expr::ObjectInit { ty, inits } => match ty {
                Some(t) => format!("{} {}", self.ty(*t), self.inits(inits)),
                None => self.inits(inits),
            },
            Expr::Default(t) => match t {
                Some(t) => format!("default({})", self.ty(*t)),
                None => "default".into(),
            },
            Expr::This => "this".into(),
            Expr::Base => "base".into(),
            Expr::New { ty, args, inits } => {
                let mut s = String::from("new");
                if let Some(t) = ty {
                    let _ = write!(s, " {}", self.ty(*t));
                }
                if let Some(args) = args {
                    let _ = write!(s, "({})", self.args(args));
                }
                if let Some(inits) = inits {
                    let _ = write!(s, " {}", self.inits(inits));
                }
                s
            }
            Expr::Typeof(t) => format!("typeof({})", self.ty(*t)),
            Expr::Sizeof(t) => format!("sizeof({})", self.ty(*t)),
            Expr::Throw(e) => match e {
                Some(e) => format!("throw {}", self.expr(*e)),
                None => "throw".into(),
            },
            Expr::Collection(items) => format!(
                "[{}]",
                items
                    .iter()
                    .map(|&e| self.expr(e))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Expr::Tuple(items) => format!(
                "({})",
                items
                    .iter()
                    .map(|(n, e)| match n {
                        Some(n) => format!("{n}: {}", self.expr(*e)),
                        None => self.expr(*e),
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Expr::Lambda { params, body } => {
                let body = match body {
                    LambdaBody::Expr(e) => self.expr(*e),
                    LambdaBody::Block(s) => {
                        let mut out = String::new();
                        self.stmt(&mut out, *s, 0);
                        out.trim_end().replace('\n', " ")
                    }
                };
                format!("(({}) => {body})", self.local_params(params))
            }
            Expr::DeclVar { ty, binding } => format!(
                "{} {}",
                ty.map(|t| self.ty(t)).unwrap_or_else(|| "var".into()),
                self.binding(*binding)
            ),
            Expr::DeclTuple(bindings) => format!(
                "var ({})",
                bindings
                    .iter()
                    .map(|&b| self.binding(b))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }

    fn pat(&self, p: PatId) -> String {
        match &self.store.pats[p] {
            Pat::Missing => "<missing>".into(),
            Pat::Constant(e) => self.expr(*e),
            Pat::Relational { op, value } => format!("{} {}", op.symbol(), self.expr(*value)),
            Pat::Discard => "_".into(),
            Pat::Var(b) => format!("var {}", self.binding(*b)),
            Pat::And(a, b) => format!("({} and {})", self.pat(*a), self.pat(*b)),
            Pat::Or(a, b) => format!("({} or {})", self.pat(*a), self.pat(*b)),
            Pat::Not(a) => format!("not {}", self.pat(*a)),
            Pat::Property(items) => format!(
                "{{ {} }}",
                items
                    .iter()
                    .map(|(m, p)| match m {
                        Some(m) => format!(
                            "{}: {}",
                            m.iter().map(|n| n.as_str()).collect::<Vec<_>>().join("."),
                            self.pat(*p)
                        ),
                        None => self.pat(*p),
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Pat::Positional(items) => format!(
                "({})",
                items
                    .iter()
                    .map(|&p| self.pat(p))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }

    fn stmt(&self, out: &mut String, s: StmtId, depth: usize) {
        let pad = "    ".repeat(depth);
        match &self.store.stmts[s] {
            Stmt::Missing => {
                let _ = writeln!(out, "{pad}<missing>;");
            }
            Stmt::Empty => {
                let _ = writeln!(out, "{pad};");
            }
            Stmt::Break => {
                let _ = writeln!(out, "{pad}break;");
            }
            Stmt::Continue => {
                let _ = writeln!(out, "{pad}continue;");
            }
            Stmt::Expr(e) => {
                let _ = writeln!(out, "{pad}{};", self.expr(*e));
            }
            Stmt::Return(e) => {
                let _ = match e {
                    Some(e) => writeln!(out, "{pad}return {};", self.expr(*e)),
                    None => writeln!(out, "{pad}return;"),
                };
            }
            Stmt::Local {
                attributes,
                is_const,
                ty,
                declarators,
            } => {
                let mut line = pad.clone();
                for a in attributes {
                    let _ = write!(line, "[{}] ", self.attribute(a));
                }
                if *is_const {
                    line.push_str("const ");
                }
                line.push_str(&ty.map(|t| self.ty(t)).unwrap_or_else(|| "var".into()));
                let decls: Vec<_> = declarators
                    .iter()
                    .map(|&(b, init)| match init {
                        Some(i) => format!("{} = {}", self.binding(b), self.expr(i)),
                        None => self.binding(b),
                    })
                    .collect();
                let _ = writeln!(out, "{line} {};", decls.join(", "));
            }
            Stmt::Block(stmts) => {
                let _ = writeln!(out, "{pad}{{");
                for &st in stmts {
                    self.stmt(out, st, depth + 1);
                }
                let _ = writeln!(out, "{pad}}}");
            }
            Stmt::If {
                is_const,
                cond,
                then,
                otherwise,
            } => {
                let kw = if *is_const { "const if" } else { "if" };
                let _ = writeln!(out, "{pad}{kw} ({})", self.expr(*cond));
                self.stmt(out, *then, depth + 1);
                if let Some(o) = otherwise {
                    let _ = writeln!(out, "{pad}else");
                    self.stmt(out, *o, depth + 1);
                }
            }
            Stmt::For {
                init,
                cond,
                update,
                body,
            } => {
                let _ = writeln!(
                    out,
                    "{pad}for ({} init; {}; {})",
                    init.len(),
                    cond.map(|c| self.expr(c)).unwrap_or_default(),
                    update
                        .iter()
                        .map(|&u| self.expr(u))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                for &i in init {
                    self.stmt(out, i, depth + 2);
                }
                self.stmt(out, *body, depth + 1);
            }
            Stmt::Foreach {
                variable,
                iterable,
                body,
            } => {
                let _ = writeln!(
                    out,
                    "{pad}foreach ({} in {})",
                    self.expr(*variable),
                    self.expr(*iterable)
                );
                self.stmt(out, *body, depth + 1);
            }
            Stmt::While { cond, body } => {
                let _ = writeln!(out, "{pad}while ({})", self.expr(*cond));
                self.stmt(out, *body, depth + 1);
            }
            Stmt::Do { body, cond } => {
                let _ = writeln!(out, "{pad}do");
                self.stmt(out, *body, depth + 1);
                let _ = writeln!(out, "{pad}while ({});", self.expr(*cond));
            }
            Stmt::Switch {
                scrutinee,
                sections,
            } => {
                let _ = writeln!(out, "{pad}switch ({})", self.expr(*scrutinee));
                for section in sections {
                    for label in &section.labels {
                        let _ = match label {
                            Some((p, guard)) => writeln!(
                                out,
                                "{pad}case {}{}:",
                                self.pat(*p),
                                guard
                                    .map(|g| format!(" when {}", self.expr(g)))
                                    .unwrap_or_default()
                            ),
                            None => writeln!(out, "{pad}default:"),
                        };
                    }
                    for &st in &section.body {
                        self.stmt(out, st, depth + 1);
                    }
                }
            }
            Stmt::LocalFunction(f) => {
                let generics = if f.generic_params.is_empty() {
                    String::new()
                } else {
                    format!(
                        "<{}>",
                        f.generic_params
                            .iter()
                            .map(|n| n.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                };
                let st = if f.is_static { "static " } else { "" };
                let _ = writeln!(
                    out,
                    "{pad}{st}{} {}{generics}({})",
                    self.ty(f.ret),
                    f.name,
                    self.local_params(&f.params)
                );
                match f.body {
                    Some(LambdaBody::Expr(e)) => {
                        let _ = writeln!(out, "{pad}    => {};", self.expr(e));
                    }
                    Some(LambdaBody::Block(b)) => self.stmt(out, b, depth),
                    None => {}
                }
            }
        }
    }
}

fn ref_kind(kind: RefKind) -> &'static str {
    match kind {
        RefKind::In => "in",
        RefKind::Out => "out",
        RefKind::Ref => "ref",
    }
}
