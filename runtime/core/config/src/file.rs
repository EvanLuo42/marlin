//! TOML config files, layers, command-line overrides and archiving.
//!
//! A config file is a TOML document whose keys spell cvar names: `r.vsync = false` at the top
//! level, `vsync = false` under `[r]` and `r = { vsync = false }` all set `r.vsync`. Values are
//! typed: booleans, integers, floats (integers are accepted too) and strings; an enum takes its
//! name as a string or its index as an integer; vectors take an array of numbers, colors that
//! or a `"#rrggbbaa"` string.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::{self, Write};
use core::ptr;

use marlin_containers::{HashSet, HashSetExt};
use toml::de::{DeTable, DeValue};
use toml_writer::{TomlStringBuilder, TomlWrite};

use crate::cvar::{Archived, Cvar, Flags, Source, Typed, Value};
use crate::error::{ApplyError, Cause, Error, Location};
use crate::registry;

/// 1-based line of a byte offset.
pub(crate) fn line_of(text: &str, offset: usize) -> u32 {
    let offset = offset.min(text.len());
    text.as_bytes()[..offset]
        .iter()
        .filter(|&&b| b == b'\n')
        .count() as u32
        + 1
}

fn typed<'a>(value: &'a DeValue<'_>) -> Typed<'a> {
    match value {
        DeValue::Boolean(b) => Typed::Bool(*b),
        DeValue::Integer(i) => {
            i64::from_str_radix(i.as_str(), i.radix()).map_or(Typed::Other, Typed::Int)
        }
        DeValue::Float(f) => f.as_str().parse().map_or(Typed::Other, Typed::Float),
        DeValue::String(s) => Typed::Str(s),
        DeValue::Array(items) if items.len() <= 4 => {
            let mut numbers = [0.0; 4];
            for (n, item) in numbers.iter_mut().zip(items.iter()) {
                *n = match typed(item.get_ref()) {
                    Typed::Int(i) => i as f64,
                    Typed::Float(f) => f,
                    _ => return Typed::Other,
                };
            }
            Typed::Numbers(numbers, items.len())
        }
        _ => Typed::Other,
    }
}

/// A validated value of a document.
pub(crate) type Parsed = (&'static Cvar, Value);

/// Validates the leaves of `table`, whose keys are under `prefix`, for `source`.
pub(crate) fn collect(
    text: &str,
    table: &DeTable<'_>,
    prefix: &mut String,
    source: Source,
    errors: &mut Vec<ApplyError>,
    out: &mut Vec<Parsed>,
) {
    for (key, value) in table.iter() {
        let len = prefix.len();
        if len > 0 {
            prefix.push('.');
        }
        prefix.push_str(key.get_ref());
        match value.get_ref() {
            DeValue::Table(inner) => collect(text, inner, prefix, source, errors, out),
            leaf => {
                let result = registry::find(prefix)
                    .ok_or(Error::Unknown)
                    .and_then(|cvar| {
                        cvar.prepare(typed(leaf), source)
                            .map(|value| out.push((cvar, value)))
                    });
                if let Err(e) = result {
                    errors.push(ApplyError {
                        location: Location::Line(line_of(text, key.span().start)),
                        key: prefix.clone(),
                        source: Cause::Set(e),
                    });
                }
            }
        }
        prefix.truncate(len);
    }
}

/// Parses `text`, recovering from syntax errors where it can.
pub(crate) fn parse(text: &str) -> (toml::Spanned<DeTable<'_>>, Vec<ApplyError>) {
    let (table, syntax) = DeTable::parse_recoverable(text);
    let errors = syntax
        .iter()
        .map(|e| ApplyError {
            location: Location::Line(line_of(text, e.span().map_or(0, |s| s.start))),
            key: String::new(),
            source: Cause::Syntax(e.message().into()),
        })
        .collect();
    (table, errors)
}

/// Tables iterate by key; reports go in document order.
pub(crate) fn sort(errors: &mut [ApplyError]) {
    errors.sort_by_key(|e| match e.location {
        Location::Document => 0,
        Location::Line(n) | Location::Arg(n) | Location::Entry(n) => n,
    });
}

/// Applies a TOML config file to `source`'s layer, keeping what the layer holds for cvars the
/// file does not name. Whatever parses and validates takes effect even when other parts fail
/// (the parser recovers from syntax errors where it can); the failures are returned, by line,
/// for the caller to report.
#[must_use = "errors should be reported"]
pub fn apply_config(text: &str, source: Source) -> Vec<ApplyError> {
    let (table, mut errors) = parse(text);
    let mut parsed = Vec::new();
    collect(
        text,
        table.get_ref(),
        &mut String::new(),
        source,
        &mut errors,
        &mut parsed,
    );
    for (cvar, value) in parsed {
        cvar.put(source, Some(value));
    }
    sort(&mut errors);
    errors
}

/// Replaces `source`'s layer with a TOML config file: as [`apply_config`], but cvars the file
/// does not name lose this layer's value and fall back to the layers below. Loading a new
/// version of a file is a reload; each cvar changes (and notifies) at most once.
#[must_use = "errors should be reported"]
pub fn load_layer(source: Source, text: &str) -> Vec<ApplyError> {
    let (table, mut errors) = parse(text);
    let mut parsed = Vec::new();
    collect(
        text,
        table.get_ref(),
        &mut String::new(),
        source,
        &mut errors,
        &mut parsed,
    );
    replace_layer(source, parsed);
    sort(&mut errors);
    errors
}

/// Loads the table named `section` (ignoring ASCII case) of a TOML document into `source`'s
/// layer, replacing it as [`load_layer`] does: for documents with one section per platform or
/// device. Without that section the layer is cleared and the result holds a
/// [`Cause::NoSection`] error for it.
#[must_use = "errors should be reported"]
pub fn load_section(source: Source, text: &str, section: &str) -> Vec<ApplyError> {
    let (table, mut errors) = parse(text);
    let mut parsed = Vec::new();
    let found = table
        .get_ref()
        .iter()
        .find(|(key, _)| key.get_ref().eq_ignore_ascii_case(section));
    match found.map(|(key, value)| (key, value.get_ref())) {
        Some((_, DeValue::Table(inner))) => collect(
            text,
            inner,
            &mut String::new(),
            source,
            &mut errors,
            &mut parsed,
        ),
        found => errors.push(ApplyError {
            location: found.map_or(Location::Document, |(key, _)| {
                Location::Line(line_of(text, key.span().start))
            }),
            key: section.into(),
            source: Cause::NoSection,
        }),
    }
    replace_layer(source, parsed);
    sort(&mut errors);
    errors
}

/// Loads the platform layer from a document with one table per platform (`[windows]`,
/// `[linux]`...): the table named `platform`, the caller's name for the one it runs on. A
/// platform without a table just has an empty layer.
#[must_use = "errors should be reported"]
pub fn load_platform(text: &str, platform: &str) -> Vec<ApplyError> {
    let mut errors = load_section(Source::Platform, text, platform);
    errors.retain(|e| !(e.source == Cause::NoSection && e.location == Location::Document));
    errors
}

/// Loads a device profile, a named bundle of values chosen at boot (by hardware, or a
/// `-profile` argument the caller reads), into the [`Source::DeviceProfile`] layer: the
/// `[name]` table of a document with one per profile. A profile usually sets scalability
/// groups (`sg.shadows = 1`) and a few cvars of its own.
#[must_use = "errors should be reported"]
pub fn load_device_profile(text: &str, name: &str) -> Vec<ApplyError> {
    load_section(Source::DeviceProfile, text, name)
}

/// Removes every value `source`'s layer holds; the cvars fall back to the layers below.
pub fn clear_layer(source: Source) {
    replace_layer(source, Vec::new());
}

/// Makes `parsed` (later entries win) the whole of `source`'s layer.
pub(crate) fn replace_layer(source: Source, parsed: Vec<Parsed>) {
    let mut named = HashSet::new();
    let mut unique = Vec::with_capacity(parsed.len());
    for (cvar, value) in parsed.into_iter().rev() {
        if named.insert(ptr::from_ref(cvar) as usize) {
            unique.push((cvar, value));
        }
    }
    for cvar in registry::cvars() {
        if !named.contains(&(ptr::from_ref(cvar) as usize)) {
            cvar.put(source, None);
        }
    }
    for (cvar, value) in unique.into_iter().rev() {
        cvar.put(source, Some(value));
    }
}

/// Applies `+name value` and `+name=value` arguments with [`Source::CommandLine`]; other
/// arguments are skipped. The value is always the next argument, even if it starts with `+`,
/// and is parsed like console input (so a vector is one argument: `+r.tint "1 0.5 0"` or
/// `+r.tint=1,0.5,0`).
#[must_use = "errors should be reported"]
pub fn apply_command_line<I, S>(args: I) -> Vec<ApplyError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut errors = Vec::new();
    let mut args = args.into_iter().enumerate();
    while let Some((i, arg)) = args.next() {
        let Some(spec) = arg.as_ref().strip_prefix('+') else {
            continue;
        };
        let next;
        let (key, value) = match spec.split_once('=') {
            Some((key, value)) => (key, Some(value)),
            None => {
                next = args.next();
                (spec, next.as_ref().map(|(_, v)| v.as_ref()))
            }
        };
        let result = match value {
            _ if key.is_empty() => Err(Cause::Syntax("expected `+name value`".into())),
            None => Err(Cause::Syntax("missing value".into())),
            Some(value) => registry::set(key, value, Source::CommandLine).map_err(Cause::Set),
        };
        if let Err(source) = result {
            errors.push(ApplyError {
                location: Location::Arg(i as u32),
                key: key.into(),
                source,
            });
        }
    }
    errors
}

/// Writes `name = value` with the help text as comments.
fn write_entry(out: &mut impl Write, cvar: &Cvar, value: &Value) -> fmt::Result {
    for line in cvar.help().lines() {
        writeln!(out, "# {line}")?;
    }
    for (i, part) in cvar.name().split('.').enumerate() {
        if i > 0 {
            out.key_sep()?;
        }
        out.key(part)?;
    }
    out.space()?;
    out.keyval_sep()?;
    out.space()?;
    match cvar.archived(value) {
        Archived::Bool(b) => out.value(b)?,
        Archived::Int(i) => out.value(i)?,
        Archived::Float(f) => out.value(f)?,
        // One line per cvar: toml_writer would pick a multi-line literal for some strings.
        Archived::Str(s) => out.value(TomlStringBuilder::new(&s).as_basic())?,
        Archived::Floats(v, len) => out.value(&v[..len])?,
    }
    out.newline()
}

/// Writes every value `source`'s layer holds, as a TOML document of dotted keys sorted by
/// name, with the help text as comments: what [`load_layer`] reads back.
pub fn write_layer(source: Source, out: &mut impl Write) -> fmt::Result {
    writeln!(out, "# The {source} layer.")?;
    for cvar in registry::cvars() {
        if let Some(value) = cvar.layer_value(source) {
            write_entry(out, cvar, &value)?;
        }
    }
    Ok(())
}

/// Writes the user's settings, for loading into [`Source::User`] next time: every value of the
/// user layer, and the value code or the console set for each [`Flags::ARCHIVE`] cvar (pending
/// ones included). Command-line, server and config file values are not saved. Sorted by name,
/// as [`write_layer`] writes them.
pub fn write_archive(out: &mut impl Write) -> fmt::Result {
    out.write_str("# User settings.\n")?;
    for cvar in registry::cvars() {
        let value = Some(cvar)
            .filter(|c| c.flags().contains(Flags::ARCHIVE))
            .and_then(|c| c.layer_value(Source::Console))
            .or_else(|| cvar.layer_value(Source::User));
        if let Some(value) = value {
            write_entry(out, cvar, &value)?;
        }
    }
    Ok(())
}

/// [`write_archive`] into a new string.
pub fn archive_string() -> String {
    let mut out = String::new();
    // Writing to a String cannot fail.
    let _ = write_archive(&mut out);
    out
}
