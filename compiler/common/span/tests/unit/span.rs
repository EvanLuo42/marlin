use marlin_lang_span::{LineCol, LineIndex, SourcePath, TextRange, TextSize};

fn range(start: u32, end: u32) -> TextRange {
    TextRange::new(TextSize(start), TextSize(end))
}

#[test]
fn ranges_measure_contain_and_cover() {
    let r = range(2, 5);
    assert_eq!(r.len(), TextSize(3));
    assert!(r.contains(TextSize(2)) && r.contains(TextSize(4)) && !r.contains(TextSize(5)));
    assert!(r.contains_range(range(3, 5)) && !r.contains_range(range(1, 3)));
    assert_eq!(r.cover(range(7, 9)), range(2, 9));
    assert!(TextRange::empty(TextSize(4)).is_empty());
    assert_eq!(r.to_range(), 2..5);
}

#[test]
fn an_inverted_range_is_a_misuse_panic() {
    marlin_test_support::assert_panics_with("inverted text range 5..2", || {
        range(5, 2);
    });
}

#[test]
fn line_index_handles_lf_and_crlf() {
    let text = "ab\r\ncd\n\nef";
    let index = LineIndex::new(text);
    assert_eq!(index.line_count(), 4);
    assert_eq!(index.line(TextSize(0)), 0);
    assert_eq!(
        index.line(TextSize(3)),
        0,
        "the \\n belongs to the line it ends"
    );
    assert_eq!(index.line(TextSize(4)), 1);
    assert_eq!(index.line(TextSize(7)), 2);
    assert_eq!(index.line(TextSize(100)), 3, "offsets past the end clamp");
    assert_eq!(&text[index.line_range(0, text).to_range()], "ab");
    assert_eq!(&text[index.line_range(1, text).to_range()], "cd");
    assert_eq!(&text[index.line_range(2, text).to_range()], "");
    assert_eq!(&text[index.line_range(3, text).to_range()], "ef");
    assert_eq!(
        index.line_col(TextSize(5), text),
        LineCol { line: 1, col: 1 }
    );
}

#[test]
fn columns_count_characters() {
    let text = "// é漢\nx";
    let index = LineIndex::new(text);
    // "// " is 3 bytes, é is 2, 漢 is 3.
    assert_eq!(
        index.line_col(TextSize(5), text),
        LineCol { line: 0, col: 4 }
    );
    assert_eq!(
        index.line_col(TextSize(6), text),
        LineCol { line: 0, col: 4 },
        "inside 漢 maps to 漢"
    );
    assert_eq!(
        index.line_col(TextSize(8), text),
        LineCol { line: 0, col: 5 }
    );
    assert_eq!(
        index.line_col(TextSize(9), text),
        LineCol { line: 1, col: 0 }
    );
}

#[test]
fn source_path_displays_as_package_slash_path() {
    let path = SourcePath::new("Marlin.Post", "Blur.mshader");
    assert_eq!(path.to_string(), "Marlin.Post/Blur.mshader");
    assert_eq!(path.package(), "Marlin.Post");
    assert_eq!(path.path(), "Blur.mshader");
}
