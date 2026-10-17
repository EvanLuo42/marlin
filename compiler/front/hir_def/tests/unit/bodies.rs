use std::fs;

use marlin_front_hir_def::store::{BindingKind, Expr};
use marlin_front_hir_def::{
    BodyQuery, BodyWithSourceMap, ExprScopesQuery, ItemSignature, ItemTreeQuery, body_owners,
    item_path, pretty_body, pretty_signature,
};
use marlin_test_support::{data_path, golden};

use crate::fixture::Fixture;

fn bodies_fixture() -> Fixture {
    let text = fs::read_to_string(data_path(env!("CARGO_MANIFEST_DIR"), "bodies.mshader"))
        .unwrap()
        .replace("\r\n", "\n");
    let text: &'static str = Box::leak(text.into_boxed_str());
    Fixture::new(&[("Post", &[], &[("bodies.mshader", text)])])
}

#[test]
fn bodies_and_signatures_lower_to_hir() {
    let fixture = bodies_fixture();
    let snapshot = fixture.db.snapshot();
    let file = fixture.file("bodies.mshader");
    let tree = snapshot.get::<ItemTreeQuery>(&file).unwrap();
    let mut signatures = String::new();
    let mut bodies = String::new();
    for (local, _) in tree.items.iter() {
        let item = marlin_front_hir_def::ItemId { file, local };
        let name = item_path(&snapshot, item).unwrap();
        let sig = snapshot.get::<ItemSignature>(&item).unwrap();
        signatures.push_str(&format!("== {name}\n{}", pretty_signature(&sig)));
        for owner in body_owners(&snapshot, item).unwrap() {
            let body = snapshot.get::<BodyQuery>(&owner).unwrap();
            bodies.push_str(&format!(
                "== {name} {:?}\n{}",
                owner.part,
                pretty_body(&body)
            ));
        }
    }
    let data = |name| data_path(env!("CARGO_MANIFEST_DIR"), name);
    golden::assert_text(&data("signatures.txt"), &signatures);
    golden::assert_text(&data("bodies.txt"), &bodies);
}

/// Every `Probe(name)` in `text`'s function `F`, resolved as a local: `name -> kind` or
/// `name -> none`.
fn probes(text: &str) -> Vec<String> {
    let fixture = Fixture::new(&[("P", &[], &[("p.mshader", text)])]);
    let snapshot = fixture.db.snapshot();
    let file = fixture.file("p.mshader");
    let tree = snapshot.get::<ItemTreeQuery>(&file).unwrap();
    let (local, _) = tree
        .items
        .iter()
        .find(|(_, i)| i.name.as_ref().is_some_and(|n| n.as_str() == "F"))
        .unwrap();
    let item = marlin_front_hir_def::ItemId { file, local };
    let owner = body_owners(&snapshot, item).unwrap()[0];
    let (body, _) = snapshot.get::<BodyWithSourceMap>(&owner).unwrap();
    let scopes = snapshot.get::<ExprScopesQuery>(&owner).unwrap();
    let mut out = Vec::new();
    for (_, expr) in body.store.exprs.iter() {
        let Expr::Call { callee, args } = expr else {
            continue;
        };
        let Expr::Path(p) = &body.store.exprs[*callee] else {
            continue;
        };
        if p.as_name().is_none_or(|n| n.as_str() != "Probe") {
            continue;
        }
        let arg = args[0].expr;
        let Expr::Path(name) = &body.store.exprs[arg] else {
            continue;
        };
        let name = name.as_name().unwrap();
        let scope = scopes.scope_of_expr(arg).unwrap();
        out.push(match scopes.resolve(scope, name) {
            Some(b) => {
                let kind = match body.store.bindings[b].kind {
                    BindingKind::Param(i) => format!("param {i}"),
                    BindingKind::Local { .. } => "local".into(),
                    BindingKind::Declared => "declared".into(),
                    BindingKind::LambdaParam => "lambda param".into(),
                    BindingKind::LocalFunction => "local function".into(),
                    BindingKind::LocalFunctionParam => "local function param".into(),
                    BindingKind::Receiver => "receiver".into(),
                    BindingKind::SetterValue => "value".into(),
                };
                format!("{name} -> {kind}")
            }
            None => format!("{name} -> none"),
        });
    }
    out
}

#[test]
fn locals_follow_csharp_scoping() {
    let text = "namespace N;
void F(int a)
{
    Probe(a);
    Probe(b);
    int b = 1;
    Probe(b);
    Probe(Later);
    { int inner = 2; Probe(inner); }
    Probe(inner);
    if (a is var v) Probe(v);
    Probe(v);
    Get(out var o);
    Probe(o);
    while (a is var w) Probe(w);
    Probe(w);
    foreach (var e in 0..a) Probe(e);
    Probe(e);
    var f = (int x) => Probe(x);
    Probe(x);
    var r = a switch { var s when Probe(s) => 1, _ => 0 };
    Probe(s);
    float Later(float t) => Probe(t);
}
";
    assert_eq!(
        probes(text),
        [
            "a -> param 0",
            "b -> none",
            "b -> local",
            "Later -> local function",
            "inner -> local",
            "inner -> none",
            "v -> declared",
            "v -> declared",
            "o -> declared",
            "w -> declared",
            "w -> none",
            "e -> declared",
            "e -> none",
            "x -> lambda param",
            "x -> none",
            "s -> declared",
            "s -> none",
            "t -> local function param",
        ]
    );
}

#[test]
fn a_local_may_not_reuse_an_enclosing_name() {
    let fixture = Fixture::new(&[(
        "P",
        &[],
        &[(
            "p.mshader",
            "namespace N;\nvoid F(int a)\n{\n    int a = 1;\n    { int b = 0; }\n    int b = 2;\n    { int c; { int c; } }\n}\n",
        )],
    )]);
    golden::assert_text(
        &data_path(env!("CARGO_MANIFEST_DIR"), "redeclared.txt"),
        &fixture.diagnostics(),
    );
}
