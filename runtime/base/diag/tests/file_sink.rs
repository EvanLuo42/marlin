use std::path::PathBuf;

use marlin_diag::{FileSink, Level, LineSink, Record, Sink};

/// A unique path in the temp directory, deleted on drop.
struct TempFile(PathBuf);

impl TempFile {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "marlin-diag-{}-{name}-{}.log",
            std::process::id(),
            marlin_pal::time::ticks()
        ));
        Self(path)
    }

    fn path(&self) -> &str {
        self.0.to_str().unwrap()
    }

    fn read(&self) -> String {
        std::fs::read_to_string(&self.0).unwrap()
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn log(sink: &FileSink, level: Level, message: &str) {
    sink.log(
        &Record::new(level, "file_sink", file!(), 1, 9, format_args!("{message}"))
            .with_timestamp(0),
    );
}

#[test]
fn writes_formatted_lines() {
    let file = TempFile::new("lines");
    let sink = FileSink::open(file.path()).unwrap();
    log(&sink, Level::Info, "one");
    log(&sink, Level::Warn, "two");
    // Buffered until flushed.
    assert_eq!(file.read(), "");
    Sink::flush(&sink);
    assert_eq!(
        file.read(),
        "[   0.000] INFO  [9] file_sink: one\n[   0.000] WARN  [9] file_sink: two\n"
    );

    // Errors are written through at once, with what was buffered before them.
    log(&sink, Level::Debug, "three");
    log(&sink, Level::Error, "four");
    assert!(
        file.read()
            .ends_with("file_sink: three\n[   0.000] ERROR [9] file_sink: four\n")
    );

    // Dropping flushes.
    sink.write_line(Level::Info, "raw\n");
    drop(sink);
    assert!(file.read().ends_with("four\nraw\n"));
}

#[test]
fn open_truncates_and_open_append_appends() {
    let file = TempFile::new("append");
    std::fs::write(&file.0, "old\n").unwrap();
    let sink = FileSink::open_append(file.path()).unwrap();
    sink.write_line(Level::Info, "new\n");
    drop(sink);
    assert_eq!(file.read(), "old\nnew\n");

    let sink = FileSink::open(file.path()).unwrap();
    sink.write_line(Level::Info, "fresh\n");
    drop(sink);
    assert_eq!(file.read(), "fresh\n");
}

#[test]
fn full_buffer_is_written_out() {
    let file = TempFile::new("full");
    let sink = FileSink::open(file.path()).unwrap();
    let line = format!("{}\n", "x".repeat(99));
    let lines = FileSink::BUFFER / line.len() + 10;
    for _ in 0..lines {
        sink.write_line(Level::Info, &line);
    }
    let written = std::fs::metadata(&file.0).unwrap().len() as usize;
    assert!(written > 0 && written < lines * line.len(), "{written}");
    Sink::flush(&sink);
    assert_eq!(file.read(), line.repeat(lines));
}

#[test]
fn bad_path_fails() {
    assert!(FileSink::open("").is_err());
}
