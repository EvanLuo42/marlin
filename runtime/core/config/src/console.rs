//! Console commands, the console line syntax, scripts and the built-in commands.
//!
//! A line holds statements separated by `;` or newlines. A statement is words separated by
//! whitespace; a word may be quoted (`"a b"`, with the escapes `\"`, `\\`, `\n`, `\t` and
//! `\;`), and quoted and unquoted parts join (`a"b c"` is one word). `//` at the start of a
//! word, or `#` at the start of a statement, comments out the rest of the line (so `#ff8000` as
//! a value is a color, not a comment). Then:
//!
//! - `name` prints the cvar `name`, or runs the command `name` without arguments;
//! - `name value...` sets the cvar from the words joined by spaces (`r.tint 1 0.5 0`), or runs
//!   the command with them as its arguments.
//!
//! The built-in commands are `help`, `list`, `reset`, `toggle` and `exec`.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt::{self, Write};
use core::sync::atomic::{AtomicU32, Ordering};

use marlin_std::sync::RwLock;
use marlin_text::name_hash;

use crate::cvar::{Cvar, DEV_ENABLED, Flags, Kind, Source, cheats_enabled};
use crate::declared::Declare;
use crate::error::Error;
use crate::registry::{self, Entry};

/// What a command does with its arguments (the words after its name), writing to the console.
pub type Handler = fn(args: &[String], out: &mut dyn Write) -> Result<(), CommandError>;

/// Why a command failed.
#[derive(Clone, Debug, PartialEq, marlin_std::Error)]
pub enum CommandError {
    /// Wrong arguments; the console prints the command's help.
    #[error("wrong arguments")]
    Usage,
    /// Not allowed by the command's flags: [`Error::Cheat`] or [`Error::DevOnly`].
    #[error(transparent)]
    Denied(Error),
    #[error("{0}")]
    Failed(String),
    /// Writing to the console failed.
    #[error("output failed")]
    Output,
}

impl From<fmt::Error> for CommandError {
    fn from(_: fmt::Error) -> Self {
        Self::Output
    }
}

/// A console command: a name in the cvars' namespace, help text, [`Flags`] (only
/// [`Flags::CHEAT`] and [`Flags::DEV_ONLY`] mean anything here) and a [`Handler`].
pub struct Command {
    name: &'static str,
    hash: u64,
    help: &'static str,
    flags: Flags,
    handler: Handler,
}

impl Command {
    /// `help`'s first line is what `help` and `list` show; the whole text is the usage.
    pub const fn new(name: &'static str, help: &'static str, handler: Handler) -> Self {
        Self {
            name,
            hash: name_hash(name),
            help,
            flags: Flags::NONE,
            handler,
        }
    }

    /// Adds `flags`.
    pub const fn with_flags(mut self, flags: Flags) -> Self {
        self.flags = self.flags.union(flags);
        self
    }

    /// Runs only while cheats are enabled.
    pub const fn cheat(self) -> Self {
        self.with_flags(Flags::CHEAT)
    }

    /// Does not run in shipping builds, and is not listed there.
    pub const fn dev_only(self) -> Self {
        self.with_flags(Flags::DEV_ONLY)
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub(crate) fn hash(&self) -> u64 {
        self.hash
    }

    pub fn help(&self) -> &'static str {
        self.help
    }

    pub fn flags(&self) -> Flags {
        self.flags
    }

    /// Runs the handler if the flags allow it.
    pub fn run(&self, args: &[String], out: &mut dyn Write) -> Result<(), CommandError> {
        if self.flags.contains(Flags::CHEAT) && !cheats_enabled() {
            return Err(CommandError::Denied(Error::Cheat));
        }
        if self.flags.contains(Flags::DEV_ONLY) && !DEV_ENABLED {
            return Err(CommandError::Denied(Error::DevOnly));
        }
        (self.handler)(args, out)
    }
}

impl fmt::Debug for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Command")
            .field("name", &self.name)
            .field("flags", &self.flags)
            .finish()
    }
}

impl Declare for Command {
    fn entry(&'static self) -> Entry {
        Entry::Command(self)
    }
}

/// One statement of a console line or script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Statement {
    /// 1-based.
    pub line: u32,
    /// The name, then the arguments; never empty.
    pub words: Vec<String>,
}

/// A statement that could not be parsed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, marlin_std::Error)]
#[error("line {line}: {message}")]
pub struct SyntaxError {
    pub line: u32,
    pub message: &'static str,
}

/// Splits console input (a line or a whole script) into statements; see the module docs. A
/// statement with a syntax error is reported in its place and the rest still parses.
pub fn parse(text: &str) -> Vec<Result<Statement, SyntaxError>> {
    let mut out = Vec::new();
    let mut line = 1;
    let mut words: Vec<String> = Vec::new();
    let mut word: Option<String> = None;
    let mut error: Option<&'static str> = None;
    let mut chars = text.chars().peekable();

    fn end_word(word: &mut Option<String>, words: &mut Vec<String>) {
        if let Some(w) = word.take() {
            words.push(w);
        }
    }
    let mut end_statement =
        |line: u32, words: &mut Vec<String>, error: &mut Option<&'static str>| {
            match error.take() {
                Some(message) => out.push(Err(SyntaxError { line, message })),
                None if !words.is_empty() => out.push(Ok(Statement {
                    line,
                    words: core::mem::take(words),
                })),
                None => {}
            }
            words.clear();
        };

    while let Some(c) = chars.next() {
        match c {
            '\n' | ';' => {
                end_word(&mut word, &mut words);
                end_statement(line, &mut words, &mut error);
                if c == '\n' {
                    line += 1;
                }
            }
            c if c.is_whitespace() => end_word(&mut word, &mut words),
            '/' if word.is_none() && chars.peek() == Some(&'/') => {
                while chars.next_if(|&c| c != '\n').is_some() {}
            }
            '#' if word.is_none() && words.is_empty() => {
                while chars.next_if(|&c| c != '\n').is_some() {}
            }
            '"' => {
                let w = word.get_or_insert_with(String::new);
                loop {
                    match chars.peek() {
                        None | Some('\n') => {
                            error.get_or_insert("unterminated quote");
                            break;
                        }
                        _ => {}
                    }
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next_if(|&c| c != '\n') {
                            Some('"') => w.push('"'),
                            Some('\\') => w.push('\\'),
                            Some('n') => w.push('\n'),
                            Some('t') => w.push('\t'),
                            Some(';') => w.push(';'),
                            _ => {
                                error.get_or_insert("unknown escape");
                            }
                        },
                        Some(c) => w.push(c),
                        None => unreachable!("peeked"),
                    }
                }
            }
            c => word.get_or_insert_with(String::new).push(c),
        }
    }
    end_word(&mut word, &mut words);
    end_statement(line, &mut words, &mut error);
    out
}

/// Writes `s` as one console word: bare if that reads back the same, else quoted.
pub fn write_word(out: &mut dyn Write, s: &str) -> fmt::Result {
    let bare = !s.is_empty()
        && !s.starts_with(['#', '/'])
        && !s.contains(|c: char| c.is_whitespace() || matches!(c, '"' | ';' | '\\'));
    if bare {
        out.write_str(s)
    } else {
        write_quoted(out, s)
    }
}

fn write_quoted(out: &mut dyn Write, s: &str) -> fmt::Result {
    out.write_char('"')?;
    for c in s.chars() {
        match c {
            '"' => out.write_str("\\\"")?,
            '\\' => out.write_str("\\\\")?,
            '\n' => out.write_str("\\n")?,
            '\t' => out.write_str("\\t")?,
            c => out.write_char(c)?,
        }
    }
    out.write_char('"')
}

/// Runs console input, a line or a whole script (see the module docs), as [`Source::Console`],
/// writing results and errors to `out`. Returns how many statements failed; the others still
/// run.
pub fn execute(text: &str, out: &mut dyn Write) -> usize {
    let mut failed = 0;
    let lines = text.contains('\n');
    for statement in parse(text) {
        let result = match &statement {
            Ok(s) => run(&s.words, out),
            Err(e) => Err(e.message.into()),
        };
        if let Err(message) = result {
            failed += 1;
            let line = match &statement {
                Ok(s) => s.line,
                Err(e) => e.line,
            };
            // Console output is best effort.
            let _ = if lines {
                writeln!(out, "line {line}: {message}")
            } else {
                writeln!(out, "{message}")
            };
        }
    }
    failed
}

/// Runs one statement; the error is the message to print.
fn run(words: &[String], out: &mut dyn Write) -> Result<(), String> {
    let (name, args) = words.split_first().expect("statements are never empty");
    match registry::lookup(name) {
        Some(Entry::Cvar(cvar)) if args.is_empty() => {
            print(cvar, out).map_err(|_| String::from("output failed"))
        }
        Some(Entry::Cvar(cvar)) => {
            cvar.set_str(&args.join(" "), Source::Console)
                .map_err(|e| format!("{}: {e}", cvar.name()))?;
            if cvar.needs_restart() {
                let _ = writeln!(out, "{}: takes effect after a restart", cvar.name());
            }
            Ok(())
        }
        Some(Entry::Command(command)) => match command.run(args, out) {
            Ok(()) => Ok(()),
            Err(CommandError::Usage) => Err(format!("usage: {}", command.help())),
            Err(e) => Err(format!("{}: {e}", command.name())),
        },
        None => Err(format!("unknown command or cvar `{name}`")),
    }
}

/// `name = value (where it came from)`.
fn print(cvar: &Cvar, out: &mut dyn Write) -> fmt::Result {
    write!(out, "{} = ", cvar.name())?;
    write_value(out, cvar, &cvar.value_string())?;
    match cvar.set_by() {
        Some(source) => {
            write!(out, " ({source}; default ")?;
            write_value(out, cvar, &cvar.default_string())?;
        }
        None => out.write_str(" (default")?,
    }
    if let Some(pending) = cvar.pending_string() {
        out.write_str("; ")?;
        write_value(out, cvar, &pending)?;
        out.write_str(" after a restart")?;
    }
    out.write_str(")\n")
}

/// Strings quoted, other values as they are.
fn write_value(out: &mut dyn Write, cvar: &Cvar, value: &str) -> fmt::Result {
    match cvar.kind() {
        Kind::String => write_quoted(out, value),
        _ => out.write_str(value),
    }
}

type LoadFn = dyn Fn(&str) -> Result<String, String> + Send + Sync;
type Loader = Arc<LoadFn>;

static LOADER: RwLock<Option<Loader>> = RwLock::new(None);
static EXEC_DEPTH: AtomicU32 = AtomicU32::new(0);
/// How deep `exec` may nest, so a script that runs itself stops.
const MAX_EXEC_DEPTH: u32 = 16;

/// Sets how the `exec` command gets a script's text from its name (reading a file, typically):
/// this crate does no IO. The error is printed.
pub fn set_exec_loader(f: impl Fn(&str) -> Result<String, String> + Send + Sync + 'static) {
    let f: Box<LoadFn> = Box::new(f);
    *LOADER.write() = Some(Arc::from(f));
}

crate::declare! {
    /// The built-in commands. `HELP` also fills the slot section's sentinel.
    pub(crate) static HELP: Command = Command::new(
        "help",
        "help [name]: what a cvar or command is; without a name, how the console works.",
        help,
    );
    static LIST: Command = Command::new(
        "list",
        "list [cvars|commands] [filter]: cvars and commands whose names contain the filter.",
        list,
    );
    static RESET: Command = Command::new(
        "reset",
        "reset name: sets a cvar to its default (from the console, until the console changes it).",
        reset,
    );
    static TOGGLE: Command = Command::new(
        "toggle",
        "toggle name [a b]: flips a bool cvar, or switches any cvar between a and b.",
        toggle,
    );
    static EXEC: Command = Command::new(
        "exec",
        "exec name: runs the script the loader finds under that name.",
        exec,
    );
}

/// The cvar named by the only argument.
fn one_cvar(args: &[String]) -> Result<&'static Cvar, CommandError> {
    match args {
        [name] => registry::find(name)
            .ok_or_else(|| CommandError::Failed(format!("unknown cvar `{name}`"))),
        _ => Err(CommandError::Usage),
    }
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("")
}

fn help(args: &[String], out: &mut dyn Write) -> Result<(), CommandError> {
    let Some(name) = args.first() else {
        out.write_str(
            "`name` shows a cvar, `name value` sets it, `name args...` runs a command.\n\
             Statements are separated by `;`; quote words with spaces: \"a b\".\n\
             `list` shows everything, `help name` one cvar or command. Commands:\n",
        )?;
        for command in registry::commands() {
            if command.flags().contains(Flags::DEV_ONLY) && !DEV_ENABLED {
                continue;
            }
            writeln!(
                out,
                "  {:<10} {}",
                command.name(),
                first_line(command.help())
            )?;
        }
        return Ok(());
    };
    if args.len() > 1 {
        return Err(CommandError::Usage);
    }
    match registry::lookup(name) {
        Some(Entry::Cvar(cvar)) => describe(cvar, out),
        Some(Entry::Command(command)) => {
            writeln!(out, "{} (command): {}", command.name(), command.help())?;
            Ok(())
        }
        None => Err(CommandError::Failed(format!(
            "unknown command or cvar `{name}`"
        ))),
    }
}

/// Everything about a cvar.
fn describe(cvar: &Cvar, out: &mut dyn Write) -> Result<(), CommandError> {
    print(cvar, out)?;
    for line in cvar.help().lines() {
        writeln!(out, "  {line}")?;
    }
    write!(out, "  {:?}", cvar.kind())?;
    if let Some((min, max)) = cvar.int_range().filter(|&r| r != (i64::MIN, i64::MAX)) {
        write!(out, " in [{min}, {max}]")?;
    }
    if let Some((min, max)) = cvar.float_range().filter(|&r| r != (f64::MIN, f64::MAX)) {
        write!(out, " in [{min}, {max}]")?;
    }
    if !cvar.enum_names().is_empty() {
        write!(out, ": {}", cvar.enum_names().join(", "))?;
    }
    if cvar.flags() != Flags::NONE {
        write!(out, "; {:?}", cvar.flags())?;
    }
    out.write_char('\n')?;
    for source in Source::LAYERS {
        if let Some(value) = cvar.layer_string(source) {
            write!(out, "  {source}: ")?;
            write_value(out, cvar, &value)?;
            out.write_char('\n')?;
        }
    }
    Ok(())
}

fn list(args: &[String], out: &mut dyn Write) -> Result<(), CommandError> {
    let (cvars, commands, filter) = match args {
        [] => (true, true, ""),
        [kind, rest @ ..] if kind == "cvars" || kind == "commands" => {
            let filter = match rest {
                [] => "",
                [filter] => filter.as_str(),
                _ => return Err(CommandError::Usage),
            };
            (kind == "cvars", kind == "commands", filter)
        }
        [filter] => (true, true, filter.as_str()),
        _ => return Err(CommandError::Usage),
    };
    for entry in registry::matching(filter) {
        match entry {
            Entry::Cvar(cvar) if cvars => {
                write!(out, "{} = ", cvar.name())?;
                write_value(out, cvar, &cvar.value_string())?;
                out.write_char('\n')?;
            }
            Entry::Command(command) if commands => {
                writeln!(
                    out,
                    "{} (command): {}",
                    command.name(),
                    first_line(command.help())
                )?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn reset(args: &[String], out: &mut dyn Write) -> Result<(), CommandError> {
    let cvar = one_cvar(args)?;
    cvar.reset(Source::Console)
        .map_err(|e| CommandError::Failed(format!("{}: {e}", cvar.name())))?;
    print(cvar, out)?;
    Ok(())
}

fn toggle(args: &[String], out: &mut dyn Write) -> Result<(), CommandError> {
    let (cvar, next) = match args {
        [_] => {
            let cvar = one_cvar(args)?;
            if cvar.kind() != Kind::Bool {
                return Err(CommandError::Failed(format!(
                    "{}: not a bool; give two values to switch between",
                    cvar.name()
                )));
            }
            let next = if cvar.value_string() == "true" {
                "false"
            } else {
                "true"
            };
            (cvar, String::from(next))
        }
        [name, a, b] => {
            let cvar = one_cvar(core::slice::from_ref(name))?;
            let fail = |e: Error| CommandError::Failed(format!("{}: {e}", cvar.name()));
            let a = cvar.normalize(a).map_err(fail)?;
            let b = cvar.normalize(b).map_err(fail)?;
            let next = if cvar.value_string() == a { b } else { a };
            (cvar, next)
        }
        _ => return Err(CommandError::Usage),
    };
    cvar.set_str(&next, Source::Console)
        .map_err(|e| CommandError::Failed(format!("{}: {e}", cvar.name())))?;
    print(cvar, out)?;
    Ok(())
}

fn exec(args: &[String], out: &mut dyn Write) -> Result<(), CommandError> {
    let [name] = args else {
        return Err(CommandError::Usage);
    };
    let loader = LOADER.read().clone();
    let loader = loader.ok_or_else(|| CommandError::Failed("no script loader is set".into()))?;
    let text = loader(name).map_err(|e| CommandError::Failed(format!("`{name}`: {e}")))?;
    if EXEC_DEPTH.fetch_add(1, Ordering::Relaxed) >= MAX_EXEC_DEPTH {
        EXEC_DEPTH.fetch_sub(1, Ordering::Relaxed);
        return Err(CommandError::Failed(format!(
            "`{name}`: scripts nested too deeply"
        )));
    }
    let failed = execute(&text, out);
    EXEC_DEPTH.fetch_sub(1, Ordering::Relaxed);
    match failed {
        0 => Ok(()),
        n => Err(CommandError::Failed(format!(
            "`{name}`: {n} statement(s) failed"
        ))),
    }
}
