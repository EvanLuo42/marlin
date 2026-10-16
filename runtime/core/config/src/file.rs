//! TOML config files, command-line overrides and archiving.
//!
//! A config file is a TOML document whose keys spell cvar names: `r.vsync = false` at the top
//! level, `vsync = false` under `[r]` and `r = { vsync = false }` all set `r.vsync`. Values are
//! typed: booleans, integers, floats (integers are accepted too) and strings; an enum takes its
//! name as a string or its index as an integer.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::{self, Write};

use toml::de::{DeTable, DeValue};
use toml_writer::TomlWrite;

use crate::cvar::{Archived, Flags, Source, Typed};
use crate::error::{ApplyError, Cause, Error, Location};
use crate::registry;

/// 1-based line of a byte offset.
fn line_of(text: &str, offset: usize) -> u32 {
    let offset = offset.min(text.len());
    text.as_bytes()[..offset].iter().filter(|&&b| b == b'\n').count() as u32 + 1
}

fn typed<'a>(value: &'a DeValue<'_>) -> Typed<'a> {
    match value {
        DeValue::Boolean(b) => Typed::Bool(*b),
        DeValue::Integer(i) => i64::from_str_radix(i.as_str(), i.radix()).map_or(Typed::Other, Typed::Int),
        DeValue::Float(f) => f.as_str().parse().map_or(Typed::Other, Typed::Float),
        DeValue::String(s) => Typed::Str(s),
        _ => Typed::Other,
    }
}

/// Applies the leaves of `table`, whose keys are under `prefix`.
fn apply_table(
    text: &str,
    table: &DeTable<'_>,
    prefix: &mut String,
    source: Source,
    errors: &mut Vec<ApplyError>,
) {
    for (key, value) in table.iter() {
        let len = prefix.len();
        if len > 0 {
            prefix.push('.');
        }
        prefix.push_str(key.get_ref());
        match value.get_ref() {
            DeValue::Table(inner) => apply_table(text, inner, prefix, source, errors),
            leaf => {
                let result = registry::find(prefix)
                    .ok_or(Error::Unknown)
                    .and_then(|cvar| cvar.set_typed(typed(leaf), source));
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

/// Applies a TOML config file. Whatever parses and validates takes effect even when other parts
/// fail (the parser recovers from syntax errors where it can); the failures are returned, by
/// line, for the caller to report.
#[must_use = "errors should be reported"]
pub fn apply_config(text: &str, source: Source) -> Vec<ApplyError> {
    let (table, syntax) = DeTable::parse_recoverable(text);
    let mut errors: Vec<ApplyError> = syntax
        .iter()
        .map(|e| ApplyError {
            location: Location::Line(line_of(text, e.span().map_or(0, |s| s.start))),
            key: String::new(),
            source: Cause::Syntax(e.message().into()),
        })
        .collect();
    apply_table(text, table.get_ref(), &mut String::new(), source, &mut errors);
    // Tables iterate by key; report in file order.
    errors.sort_by_key(|e| match e.location {
        Location::Line(n) | Location::Arg(n) => n,
    });
    errors
}

/// Applies `+name value` and `+name=value` arguments with [`Source::CommandLine`]; other
/// arguments are skipped. The value is always the next argument, even if it starts with `+`,
/// and is parsed like console input.
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

/// Writes every registered [`Flags::ARCHIVE`] cvar whose value (pending, if a restart is due)
/// differs from its default, as a TOML document of dotted keys sorted by name, with the help
/// text as comments.
pub fn write_archive(out: &mut impl Write) -> fmt::Result {
    out.write_str("# Archived cvars that differ from their defaults.\n")?;
    for cvar in registry::cvars() {
        if !cvar.flags().contains(Flags::ARCHIVE) {
            continue;
        }
        let Some(value) = cvar.archive_value() else {
            continue;
        };
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
        match value {
            Archived::Bool(b) => out.value(b)?,
            Archived::Int(i) => out.value(i)?,
            Archived::Float(f) => out.value(f)?,
            Archived::Str(s) => out.value(s.as_str())?,
        }
        out.newline()?;
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
