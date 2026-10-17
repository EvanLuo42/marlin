//! rustc-style plain-text rendering.
//!
//! ```text
//! error[E0201]: expected `;`
//!  --> shaders/post/Blur.mshader:3:14
//!   |
//! 3 |     var x = 1
//!   |              ^ expected `;`
//!   |
//!   = note: ...
//! ```

use std::fmt::Write;

use marlin_lang_span::{FileId, LineIndex, Span, TextSize};

use crate::{Diagnostic, Label};

/// What the renderer needs to know about the session's files.
pub trait SourceMap {
    /// The name shown in `-->` lines, normally the file's `SourcePath`.
    fn name(&self, file: FileId) -> String;
    fn text(&self, file: FileId) -> &str;
}

const TAB_WIDTH: usize = 4;

/// Renders one diagnostic. The result ends with a newline.
pub fn render(diagnostic: &Diagnostic, sources: &dyn SourceMap) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{}[{}]: {}",
        diagnostic.severity, diagnostic.code, diagnostic.message
    );

    // Labels grouped by file, the primary label's file first.
    let mut labels: Vec<(&Label, bool)> = Vec::new();
    labels.extend(diagnostic.primary.iter().map(|l| (l, true)));
    labels.extend(diagnostic.labels.iter().map(|l| (l, false)));
    let mut files: Vec<FileId> = Vec::new();
    for (label, _) in &labels {
        if !files.contains(&label.span.file) {
            files.push(label.span.file);
        }
    }

    let snippets: Vec<FileSnippet<'_>> = files
        .iter()
        .map(|&file| FileSnippet::new(file, sources, &labels))
        .collect();
    let gutter = snippets
        .iter()
        .flat_map(|s| s.lines.iter())
        .map(|&line| digits(line + 1))
        .max()
        .unwrap_or(0);
    let pad = " ".repeat(gutter);

    for (i, snippet) in snippets.iter().enumerate() {
        let arrow = if i == 0 { "-->" } else { ":::" };
        let first = snippet.labels[0].0.span.range.start();
        let at = snippet.index.line_col(first, snippet.text);
        let _ = writeln!(
            out,
            "{pad}{arrow} {}:{}:{}",
            sources.name(snippet.file),
            at.line + 1,
            at.col + 1
        );
        let _ = writeln!(out, "{pad} |");
        snippet.write(&mut out, gutter);
    }

    if !snippets.is_empty() && (!diagnostic.notes.is_empty() || !diagnostic.suggestions.is_empty())
    {
        let _ = writeln!(out, "{pad} |");
    }
    for note in &diagnostic.notes {
        let _ = writeln!(out, "{pad} = note: {note}");
    }
    for suggestion in &diagnostic.suggestions {
        match suggestion.edits.as_slice() {
            [edit] if !edit.replacement.is_empty() => {
                let _ = writeln!(
                    out,
                    "{pad} = help: {}: `{}`",
                    suggestion.message, edit.replacement
                );
            }
            _ => {
                let _ = writeln!(out, "{pad} = help: {}", suggestion.message);
            }
        }
    }
    out
}

/// Renders several diagnostics separated by blank lines.
pub fn render_all(diagnostics: &[Diagnostic], sources: &dyn SourceMap) -> String {
    diagnostics
        .iter()
        .map(|d| render(d, sources))
        .collect::<Vec<_>>()
        .join("\n")
}

struct FileSnippet<'a> {
    file: FileId,
    text: &'a str,
    index: LineIndex,
    labels: Vec<(&'a Label, bool)>,
    /// 0-based lines to show, sorted.
    lines: Vec<u32>,
}

impl<'a> FileSnippet<'a> {
    fn new(file: FileId, sources: &'a dyn SourceMap, all: &[(&'a Label, bool)]) -> Self {
        let text = sources.text(file);
        let index = LineIndex::new(text);
        let labels: Vec<_> = all
            .iter()
            .copied()
            .filter(|(l, _)| l.span.file == file)
            .collect();
        let mut lines: Vec<u32> = labels
            .iter()
            .map(|(l, _)| index.line(l.span.range.start()))
            .collect();
        lines.sort_unstable();
        lines.dedup();
        Self {
            file,
            text,
            index,
            labels,
            lines,
        }
    }

    fn write(&self, out: &mut String, gutter: usize) {
        let pad = " ".repeat(gutter);
        let mut previous: Option<u32> = None;
        for &line in &self.lines {
            if previous.is_some_and(|p| line > p + 1) {
                let _ = writeln!(out, "...");
            }
            previous = Some(line);

            let range = self.index.line_range(line, self.text);
            let source = &self.text[range.to_range()];
            let _ = writeln!(
                out,
                "{:<gutter$} | {}",
                line + 1,
                expand_tabs(source).trim_end()
            );

            // Primary label first, then the others in the order they were added.
            let mut on_line: Vec<_> = self
                .labels
                .iter()
                .filter(|(l, _)| self.index.line(l.span.range.start()) == line)
                .collect();
            on_line.sort_by_key(|(_, primary)| !primary);
            for (label, primary) in on_line {
                let span = clamp_to_line(label.span, range.start(), range.end());
                let start = (span.0 - range.start().0) as usize;
                let end = (span.1 - range.start().0) as usize;
                let indent = display_width(&source[..floor_boundary(source, start)]);
                let width = display_width(
                    &source[floor_boundary(source, start)..floor_boundary(source, end)],
                )
                .max(1);
                let mark = if *primary { "^" } else { "-" };
                let message = if label.message.is_empty() {
                    String::new()
                } else {
                    format!(" {}", label.message)
                };
                let _ = writeln!(
                    out,
                    "{pad} | {}{}{message}",
                    " ".repeat(indent),
                    mark.repeat(width)
                );
            }
        }
    }
}

/// The part of `span` on the line `line_start..line_end`, as raw offsets.
fn clamp_to_line(span: Span, line_start: TextSize, line_end: TextSize) -> (u32, u32) {
    let start = span.range.start().max(line_start).min(line_end);
    let end = span.range.end().max(start).min(line_end);
    (start.0, end.0)
}

fn floor_boundary(text: &str, mut at: usize) -> usize {
    at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

fn display_width(text: &str) -> usize {
    text.chars()
        .map(|c| if c == '\t' { TAB_WIDTH } else { 1 })
        .sum()
}

fn expand_tabs(text: &str) -> String {
    text.replace('\t', &" ".repeat(TAB_WIDTH))
}

fn digits(mut n: u32) -> usize {
    let mut count = 1;
    while n >= 10 {
        n /= 10;
        count += 1;
    }
    count
}
