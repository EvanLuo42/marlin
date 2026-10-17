use marlin_lang_diag::{FileId, SourceMap, render_all};
use marlin_shaderc_syntax::{SyntaxKind, parse};

struct One<'a>(&'a str);

impl SourceMap for One<'_> {
    fn name(&self, _: FileId) -> String {
        "Test.mshader".into()
    }
    fn text(&self, _: FileId) -> &str {
        self.0
    }
}

fn rendered(text: &str) -> String {
    render_all(&parse(text).diagnostics(FileId(0)), &One(text))
}

#[test]
fn the_syntax_tour_parses_cleanly() {
    let text = include_str!("../data/tour.mshader").replace("\r\n", "\n");
    let parse = parse(&text);
    assert_eq!(parse.syntax_node().text(), text);
    assert!(parse.errors().is_empty(), "{}", rendered(&text));
}

#[test]
fn half_is_a_keyword() {
    let parse = parse("half h;");
    assert!(
        parse
            .syntax_node()
            .descendant_tokens()
            .any(|t| t.kind() == SyntaxKind::HALF_KW)
    );
}

#[test]
fn rejected_csharp_constructs_have_hints() {
    let text = "\
namespace Game { }
class Player { }
void F()
{
    var p = Light(1) as Light;
    var q = new Light(1);
    var n = null;
    var c = 'x';
    var m = 1.5m;
}
";
    assert_eq!(
        rendered(text),
        "\
error[E0301]: Marlin shader has no block-scoped namespaces
 --> Test.mshader:1:1
  |
1 | namespace Game { }
  | ^^^^^^^^^^^^^^
  |
  = note: declare the namespace for the whole file: `namespace A.B;`

error[E0301]: Marlin shader has no classes
 --> Test.mshader:2:1
  |
2 | class Player { }
  | ^^^^^
  |
  = note: use `struct`: every Marlin shader type is a value type, and `with` works on any struct

error[E0301]: Marlin shader has no `as` conversions
 --> Test.mshader:5:22
  |
5 |     var p = Light(1) as Light;
  |                      ^^
  |
  = note: use a cast: `(T)x`

error[E0301]: Marlin shader has no `new` expressions
 --> Test.mshader:6:13
  |
6 |     var q = new Light(1);
  |             ^^^
  |
  = note: construct values with `T(...)`, `T { ... }` or a target-typed `{ ... }`

error[E0301]: Marlin shader has no `null`
 --> Test.mshader:7:13
  |
7 |     var n = null;
  |             ^^^^
  |
  = note: every type is a value type; use `default` for a zero value

error[E0301]: Marlin shader has no character literals
 --> Test.mshader:8:13
  |
8 |     var c = 'x';
  |             ^^^
  |
  = note: strings appear only in attributes, `nameof` and `Debug` messages

error[E0301]: Marlin shader has no `decimal` literals
 --> Test.mshader:9:13
  |
9 |     var m = 1.5m;
  |             ^^^^
  |
  = note: use the `f`, `d` or `h` suffix
"
    );
}

#[test]
fn shader_extensions_are_allowed() {
    let text = "\
const int N = 4;
public struct V<T, const int K> { type P = int; }
extension(float3 c) : ILuma { }
void F() { const if (true) { } var h = 1.0h; }
";
    let parse = parse(text);
    assert!(parse.errors().is_empty(), "{}", rendered(text));
}
