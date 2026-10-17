use marlin_test_support::{TempDir, golden, panic_message};

#[test]
fn text_compares_across_line_endings_and_names_the_first_difference() {
    let dir = TempDir::new("golden");
    let path = dir.write("expected.txt", "a\r\nb\r\nc\r\n");
    golden::assert_text(&path, "a\nb\nc\n");
    let message = panic_message(|| golden::assert_text(&path, "a\nB\nc\n"));
    assert!(message.contains("line 2"), "{message}");
}

#[test]
fn a_missing_golden_file_says_how_to_create_it() {
    let dir = TempDir::new("golden");
    let message = panic_message(|| golden::assert_bytes(&dir.join("none.bin"), b"x"));
    assert!(message.contains("MARLIN_BLESS=1"), "{message}");
}
