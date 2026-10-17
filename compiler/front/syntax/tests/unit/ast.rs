use marlin_front_syntax::ast::{self, AstNode, HasGenerics, HasModifiers, HasName};
use marlin_front_syntax::{Dialect, SyntaxKind, parse};

const SOURCE: &str = "\
namespace Marlin.Post;
using Marlin.Math;

public struct Blur<T, const int N> : IPass where T : IFloat<T>
{
    public float Radius = 1, Sigma;
    public T Run(in T x) { return x; }
}

public float Luma(float3 c) => Dot(c, 1);
";

#[test]
fn ast_views_expose_files_and_members() {
    let parse = parse(SOURCE, &Dialect::SUPERSET);
    assert!(parse.errors().is_empty(), "{:?}", parse.errors());
    let file = parse.tree();

    let ns = file.namespace().unwrap();
    let segments: Vec<String> = ns
        .path()
        .unwrap()
        .segments()
        .map(|s| s.name_ref().unwrap().text())
        .collect();
    assert_eq!(segments, ["Marlin", "Post"]);
    assert_eq!(file.usings().count(), 1);

    let members: Vec<ast::Member> = file.members().collect();
    assert_eq!(members.len(), 2);
    let ast::Member::StructDecl(blur) = &members[0] else {
        panic!("expected a struct, got {:?}", members[0]);
    };
    assert_eq!(blur.name().unwrap().text(), "Blur");
    assert!(blur.has_modifier(SyntaxKind::PUBLIC_KW));
    assert!(!blur.has_modifier(SyntaxKind::STATIC_KW));
    let params: Vec<ast::TypeParam> = blur
        .type_param_list()
        .unwrap()
        .syntax()
        .children()
        .filter_map(ast::TypeParam::cast)
        .collect();
    assert_eq!(params.len(), 2);
    assert!(params[0].const_type().is_none());
    assert!(params[1].const_type().is_some());
    assert_eq!(blur.where_clauses().count(), 1);
    assert_eq!(blur.base_list().unwrap().types().count(), 1);

    let inner: Vec<ast::Member> = blur.member_list().unwrap().members().collect();
    let ast::Member::FieldDecl(field) = &inner[0] else {
        panic!("expected a field");
    };
    let names: Vec<String> = field
        .declarators()
        .map(|d| d.name().unwrap().text())
        .collect();
    assert_eq!(names, ["Radius", "Sigma"]);
    assert!(field.declarators().next().unwrap().initializer().is_some());

    let ast::Member::FunctionDecl(run) = &inner[1] else {
        panic!("expected a method");
    };
    assert_eq!(run.param_list().unwrap().params().count(), 1);
    assert_eq!(run.body().unwrap().statements().count(), 1);

    let ast::Member::FunctionDecl(luma) = &members[1] else {
        panic!("expected a function");
    };
    assert!(luma.body().is_none() && luma.expr_body().is_some());
}

#[test]
fn bin_expr_operator_is_one_token() {
    let parse = parse("void F() { x = a >> b; }", &Dialect::SUPERSET);
    let bin = parse
        .syntax_node()
        .descendants()
        .filter_map(ast::BinExpr::cast)
        .find(|b| b.op().is_some_and(|op| op.kind() == SyntaxKind::SHR))
        .expect("a `>>` expression");
    assert_eq!(bin.op().unwrap().text(), ">>");
    assert_eq!(bin.lhs().unwrap().syntax().text(), "a");
    assert_eq!(bin.rhs().unwrap().syntax().text(), "b");
}

#[test]
fn token_lookup_and_ancestors_walk_the_tree() {
    let parse = parse(SOURCE, &Dialect::SUPERSET);
    let root = parse.syntax_node();
    let offset = SOURCE.find("Sigma").unwrap() as u32;
    let token = root.token_at_offset(offset.into()).unwrap();
    assert_eq!(token.text(), "Sigma");
    let kinds: Vec<SyntaxKind> = token.parent().ancestors().map(|n| n.kind()).collect();
    assert_eq!(
        kinds,
        [
            SyntaxKind::NAME,
            SyntaxKind::VARIABLE_DECLARATOR,
            SyntaxKind::FIELD_DECL,
            SyntaxKind::MEMBER_LIST,
            SyntaxKind::STRUCT_DECL,
            SyntaxKind::SOURCE_FILE
        ]
    );
}
