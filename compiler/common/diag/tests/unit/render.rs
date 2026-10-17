use marlin_lang_diag::{
    Applicability, Code, Diagnostic, FileId, SourceMap, Span, has_errors, render, render_all,
};
use marlin_lang_span::{TextRange, TextSize};

struct Files(Vec<(&'static str, &'static str)>);

impl SourceMap for Files {
    fn name(&self, file: FileId) -> String {
        self.0[file.0 as usize].0.to_string()
    }
    fn text(&self, file: FileId) -> &str {
        self.0[file.0 as usize].1
    }
}

fn span(file: u32, start: u32, end: u32) -> Span {
    Span::new(FileId(file), TextRange::new(TextSize(start), TextSize(end)))
}

#[test]
fn primary_label_notes_and_help_render_like_rustc() {
    let files = Files(vec![(
        "Marlin.Post/Blur.mshader",
        "namespace Marlin.Post;\n\nvar x = 1\nvar y = 2;\n",
    )]);
    let d = Diagnostic::error(Code("E0201"), "expected `;`")
        .at(span(0, 33, 33), "expected `;`")
        .note("statements end with `;`")
        .suggest(
            "insert a semicolon",
            span(0, 33, 33),
            ";",
            Applicability::MachineApplicable,
        );
    assert_eq!(
        render(&d, &files),
        "\
error[E0201]: expected `;`
 --> Marlin.Post/Blur.mshader:3:10
  |
3 | var x = 1
  |          ^ expected `;`
  |
  = note: statements end with `;`
  = help: insert a semicolon: `;`
"
    );
}

#[test]
fn secondary_labels_elide_gaps_and_name_other_files() {
    let a = "struct A\n{\n    int X;\n\n\n\n\n\n\n\n    int X;\n}\n";
    let files = Files(vec![
        ("pkg/A.mshader", a),
        ("pkg/B.mshader", "\tstruct A {}\n"),
    ]);
    let first = a.find("X;").unwrap() as u32;
    let second = a.rfind("X;").unwrap() as u32;
    let d = Diagnostic::error(Code("E0401"), "duplicate field `X`")
        .at(span(0, second, second + 1), "redefined here")
        .label(span(0, first, first + 1), "first defined here")
        .label(span(1, 8, 9), "");
    assert_eq!(
        render(&d, &files),
        "\
error[E0401]: duplicate field `X`
  --> pkg/A.mshader:11:9
   |
3  |     int X;
   |         - first defined here
...
11 |     int X;
   |         ^ redefined here
  ::: pkg/B.mshader:1:9
   |
1  |     struct A {}
   |            -
"
    );
}

#[test]
fn diagnostics_without_a_location_render_header_and_notes() {
    let files = Files(vec![]);
    let d = Diagnostic::warning(Code("E0001"), "entry `Main` not found").note("check the name");
    assert_eq!(
        render(&d, &files),
        "warning[E0001]: entry `Main` not found\n = note: check the name\n"
    );
    assert!(!has_errors(std::slice::from_ref(&d)));
    let e = Diagnostic::error(Code("E0002"), "x");
    assert!(has_errors(&[d.clone(), e.clone()]));
    assert_eq!(
        render_all(&[d, e], &files),
        "warning[E0001]: entry `Main` not found\n = note: check the name\n\nerror[E0002]: x\n"
    );
}

#[test]
fn multi_line_spans_are_underlined_to_the_end_of_their_first_line() {
    let files = Files(vec![("f", "abc\ndef\n")]);
    let d = Diagnostic::error(Code("E0003"), "multi-line").at(span(0, 1, 6), "here");
    assert_eq!(
        render(&d, &files),
        "error[E0003]: multi-line\n --> f:1:2\n  |\n1 | abc\n  |  ^^ here\n"
    );
}
