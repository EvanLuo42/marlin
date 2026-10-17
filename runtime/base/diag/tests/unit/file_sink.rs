use marlin_diag::{FileSink, Level, LineSink, Record, Sink};
use marlin_test_support::TempFile;

fn path(file: &TempFile) -> &str {
    file.to_str().unwrap()
}

fn read(file: &TempFile) -> String {
    std::fs::read_to_string(file).unwrap()
}

fn log(sink: &FileSink, level: Level, message: &str) {
    sink.log(
        &Record::new(level, "file_sink", file!(), 1, 9, format_args!("{message}"))
            .with_timestamp(0),
    );
}

#[test]
fn writes_formatted_lines() {
    let file = TempFile::new("diag-lines.log");
    let sink = FileSink::open(path(&file)).unwrap();
    log(&sink, Level::Info, "one");
    log(&sink, Level::Warn, "two");
    // Buffered until flushed.
    assert_eq!(read(&file), "");
    Sink::flush(&sink);
    assert_eq!(
        read(&file),
        "[   0.000] INFO  [9] file_sink: one\n[   0.000] WARN  [9] file_sink: two\n"
    );

    // Errors are written through at once, with what was buffered before them.
    log(&sink, Level::Debug, "three");
    log(&sink, Level::Error, "four");
    assert!(read(&file).ends_with("file_sink: three\n[   0.000] ERROR [9] file_sink: four\n"));

    // Dropping flushes.
    sink.write_line(Level::Info, "raw\n");
    drop(sink);
    assert!(read(&file).ends_with("four\nraw\n"));
}

#[test]
fn open_truncates_and_open_append_appends() {
    let file = TempFile::new("diag-append.log");
    std::fs::write(&file, "old\n").unwrap();
    let sink = FileSink::open_append(path(&file)).unwrap();
    sink.write_line(Level::Info, "new\n");
    drop(sink);
    assert_eq!(read(&file), "old\nnew\n");

    let sink = FileSink::open(path(&file)).unwrap();
    sink.write_line(Level::Info, "fresh\n");
    drop(sink);
    assert_eq!(read(&file), "fresh\n");
}

#[test]
fn full_buffer_is_written_out() {
    let file = TempFile::new("diag-full.log");
    let sink = FileSink::open(path(&file)).unwrap();
    let line = format!("{}\n", "x".repeat(99));
    let lines = FileSink::BUFFER / line.len() + 10;
    for _ in 0..lines {
        sink.write_line(Level::Info, &line);
    }
    let written = std::fs::metadata(&file).unwrap().len() as usize;
    assert!(written > 0 && written < lines * line.len(), "{written}");
    Sink::flush(&sink);
    assert_eq!(read(&file), line.repeat(lines));
}

#[test]
fn bad_path_fails() {
    assert!(FileSink::open("").is_err());
}
