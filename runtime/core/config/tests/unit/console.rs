//! The console: line syntax, statements, commands, the built-ins and completion. Each test
//! uses its own cvars and commands, so they can run in parallel.

use std::fmt::Write;
use std::sync::atomic::{AtomicUsize, Ordering};

use marlin_config::{
    Command, CommandError, CvarBool, CvarEnum, CvarF64, CvarI64, CvarString, Entry, Error, Source,
    Statement, SyntaxError, complete, declare, execute, parse, set_cheats_enabled, set_exec_loader,
    write_word,
};

fn words(line: u32, words: &[&str]) -> Result<Statement, SyntaxError> {
    Ok(Statement {
        line,
        words: words.iter().map(|w| w.to_string()).collect(),
    })
}

fn run(text: &str) -> (usize, String) {
    let mut out = String::new();
    let failed = execute(text, &mut out);
    (failed, out)
}

#[test]
fn syntax() {
    assert_eq!(
        parse("r.vsync 0; name \"Ann \\\"The\\\" Lee\";;  echo a\"b c\"d"),
        [
            words(1, &["r.vsync", "0"]),
            words(1, &["name", "Ann \"The\" Lee"]),
            words(1, &["echo", "ab cd"]),
        ]
    );
    // Lines, comments, empty words, and `#` / `//` only where they start something.
    let script = "# a comment\n\
                  r.tint #ff8000 // the rest is ignored\n\
                  \n\
                  url http://host/a//b \"\"  # not a comment here\n\
                  // whole line\n\
                  say \"x;y\" \"\\t\\\\\"; next";
    assert_eq!(
        parse(script),
        [
            words(2, &["r.tint", "#ff8000"]),
            words(
                4,
                &[
                    "url",
                    "http://host/a//b",
                    "",
                    "#",
                    "not",
                    "a",
                    "comment",
                    "here"
                ]
            ),
            words(6, &["say", "x;y", "\t\\"]),
            words(6, &["next"]),
        ]
    );
    // An error spoils its statement only.
    assert_eq!(
        parse("a \"open\nb \"\\q\"; c\nd"),
        [
            Err(SyntaxError {
                line: 1,
                message: "unterminated quote"
            }),
            Err(SyntaxError {
                line: 2,
                message: "unknown escape"
            }),
            words(2, &["c"]),
            words(3, &["d"]),
        ]
    );
    assert_eq!(
        SyntaxError {
            line: 2,
            message: "unknown escape"
        }
        .to_string(),
        "line 2: unknown escape"
    );

    // Words written back read the same.
    for word in [
        "plain",
        "two words",
        "",
        "quo\"te",
        "back\\slash",
        "#hash",
        "//x",
        "a;b",
    ] {
        let mut out = String::new();
        write_word(&mut out, word).unwrap();
        assert_eq!(
            parse(&format!("cmd {out}")),
            [words(1, &["cmd", word])],
            "{out}"
        );
    }
}

declare! {
    static VOLUME: CvarF64 = CvarF64::new("con.volume", 1.0, "Master volume.\nLinear.").range(0.0, 1.0);
    static NAME: CvarString = CvarString::new("con.name", "Player", "Display name.");
    static WIDTH: CvarI64 = CvarI64::new("con.width", 1280, "").requires_restart();
    static ECHO: Command = Command::new("con.echo", "con.echo words...: prints the words.", echo);
    static FAIL: Command = Command::new("con.fail", "con.fail: always fails.", |_, _| {
        Err(CommandError::Failed("broken".into()))
    });
}

fn echo(args: &[String], out: &mut dyn Write) -> Result<(), CommandError> {
    if args.is_empty() {
        return Err(CommandError::Usage);
    }
    writeln!(out, "{}", args.join("|"))?;
    Ok(())
}

#[test]
fn statements() {
    // Set, print, run.
    assert_eq!(
        run("con.volume 0.25; con.name \"Ann Lee\""),
        (0, String::new())
    );
    assert_eq!(VOLUME.get(), 0.25);
    assert_eq!(VOLUME.set_by(), Some(Source::Console));
    assert_eq!(NAME.get(), "Ann Lee");
    assert_eq!(
        run("con.volume; CON.NAME"),
        (
            0,
            "con.volume = 0.25 (console; default 1)\ncon.name = \"Ann Lee\" (console; default \"Player\")\n"
                .into()
        )
    );
    // Several words set a string joined by spaces.
    run("con.name Ann   Lee  Jr");
    assert_eq!(NAME.get(), "Ann Lee Jr");
    assert_eq!(run("con.echo a \"b c\""), (0, "a|b c\n".into()));

    // Errors are counted and printed; the other statements still run.
    let (failed, out) = run("con.volume 2; nope 1; con.echo; con.fail; con.volume 0.5");
    assert_eq!(failed, 4);
    assert_eq!(
        out,
        "con.volume: out of range [0, 1]\n\
         unknown command or cvar `nope`\n\
         usage: con.echo words...: prints the words.\n\
         con.fail: broken\n"
    );
    assert_eq!(VOLUME.get(), 0.5);
    // A script reports lines.
    assert_eq!(
        run("con.volume 0.75\ncon.volume x\n\"open"),
        (
            2,
            "line 2: con.volume: expected a finite number\nline 3: unterminated quote\n".into()
        )
    );

    // Requires restart, after startup: there is no startup in this binary, so it applies.
    assert_eq!(run("con.width 1920"), (0, String::new()));
    assert_eq!(WIDTH.get(), 1920);
}

declare! {
    static FULLSCREEN: CvarBool = CvarBool::new("bi.fullscreen", false, "Fullscreen.").archive();
    static QUALITY: CvarEnum = CvarEnum::new("bi.quality", 1, &["low", "medium", "high"], "Preset.");
    static FPS: CvarI64 = CvarI64::new("bi.fps", 60, "Frame cap.").range(0, 240);
    static HIDDEN: Command = Command::new("bi.hidden", "Development only.", |_, _| Ok(())).dev_only();
}

#[test]
fn built_ins() {
    // help: everything about a cvar, a command's help, or an overview.
    FPS.set_str("30", Source::User).unwrap();
    FPS.set_str("120", Source::Console).unwrap();
    let (failed, out) = run("help bi.fps");
    assert_eq!(failed, 0);
    assert_eq!(
        out,
        "bi.fps = 120 (console; default 60)\n  Frame cap.\n  Int in [0, 240]\n  user: 30\n  console: 120\n"
    );
    assert_eq!(
        run("help bi.quality").1,
        "bi.quality = medium (default)\n  Preset.\n  Enum: low, medium, high\n"
    );
    assert_eq!(
        run("help toggle").1,
        "toggle (command): toggle name [a b]: flips a bool cvar, or switches any cvar between a and b.\n"
    );
    let (failed, out) = run("help");
    assert_eq!(failed, 0);
    assert!(out.contains("\n  exec       exec name: runs"), "{out}");
    assert!(out.contains("bi.hidden"));
    assert_eq!(run("help nope").1, "help: unknown command or cvar `nope`\n");

    // list: by kind and filter.
    assert_eq!(
        run("list bi.").1,
        "bi.fps = 120\nbi.fullscreen = false\nbi.hidden (command): Development only.\nbi.quality = medium\n"
    );
    assert_eq!(
        run("list commands BI.").1,
        "bi.hidden (command): Development only.\n"
    );
    assert_eq!(run("list cvars full").1, "bi.fullscreen = false\n");
    assert_eq!(run("list a b c").0, 1);

    // reset: the default, from the console; the user's value is still there below it.
    assert_eq!(run("reset bi.fps").1, "bi.fps = 60 (console; default 60)\n");
    assert_eq!(FPS.layer_string(Source::User).as_deref(), Some("30"));
    FPS.unset(Source::Console).unwrap();
    assert_eq!(FPS.get(), 30);
    assert_eq!(
        run("reset").1,
        "usage: reset name: sets a cvar to its default (from the console, until the console changes it).\n"
    );

    // toggle: bools flip, anything switches between two values.
    run("toggle bi.fullscreen");
    assert!(FULLSCREEN.get());
    run("toggle BI.FULLSCREEN");
    assert!(!FULLSCREEN.get());
    assert_eq!(
        run("toggle bi.quality").1,
        "toggle: bi.quality: not a bool; give two values to switch between\n"
    );
    run("toggle bi.quality LOW 2");
    assert_eq!(QUALITY.get_name(), "low");
    run("toggle bi.quality low high");
    assert_eq!(QUALITY.get_name(), "high");
    run("toggle bi.quality 0 high");
    assert_eq!(QUALITY.get_name(), "low");
    assert_eq!(
        run("toggle bi.fps 1 999").1,
        "toggle: bi.fps: out of range [0, 240]\n"
    );

    // Development-only commands run outside shipping builds.
    assert_eq!(run("bi.hidden").0, usize::from(!marlin_config::DEV_ENABLED));
}

declare! {
    static GOD: Command = Command::new("cheat.god", "Invulnerability.", |_, out| {
        out.write_str("god mode\n")?;
        Ok(())
    })
    .cheat();
}

#[test]
fn cheat_commands() {
    assert_eq!(
        run("cheat.god"),
        (
            1,
            "cheat.god: cheat-protected and cheats are disabled\n".into()
        )
    );
    assert_eq!(
        GOD.run(&[], &mut String::new()),
        Err(CommandError::Denied(Error::Cheat))
    );
    set_cheats_enabled(true);
    assert_eq!(run("cheat.god"), (0, "god mode\n".into()));
    set_cheats_enabled(false);
}

declare! {
    static SCRIPTED: CvarI64 = CvarI64::new("exec.value", 0, "");
}

static LOADS: AtomicUsize = AtomicUsize::new(0);

#[test]
fn exec_scripts() {
    // The only test that sets the loader: it is global.
    assert_eq!(run("exec a").1, "exec: no script loader is set\n");
    set_exec_loader(|name| {
        LOADS.fetch_add(1, Ordering::SeqCst);
        match name {
            "autoexec" => {
                Ok("// startup\nexec.value 5\nexec nested; exec.value; exec missing".into())
            }
            "nested" => Ok("exec.value 6 // from nested".into()),
            "self" => Ok("exec self".into()),
            other => Err(format!("no file {other}.cfg")),
        }
    });
    let (failed, out) = run("exec autoexec");
    assert_eq!(SCRIPTED.get(), 6);
    assert_eq!(failed, 1);
    assert_eq!(
        out,
        "exec.value = 6 (console; default 0)\n\
         line 3: exec: `missing`: no file missing.cfg\n\
         exec: `autoexec`: 1 statement(s) failed\n"
    );
    // A script that runs itself stops at the depth limit.
    let (failed, out) = run("exec self");
    assert_eq!(failed, 1);
    assert!(out.contains("scripts nested too deeply"), "{out}");
    // autoexec, nested and missing; then self 16 times, and once more past the limit.
    assert_eq!(LOADS.load(Ordering::SeqCst), 3 + 17);
}

declare! {
    static C1: CvarBool = CvarBool::new("cmp.alpha", false, "");
    static C2: CvarBool = CvarBool::new("cmp.Beta", false, "");
    static C3: Command = Command::new("cmp.gamma", "", |_, _| Ok(()));
}

#[test]
fn completion() {
    let names: Vec<_> = complete("CMP.").iter().map(|e| e.name()).collect();
    assert_eq!(names, ["cmp.alpha", "cmp.Beta", "cmp.gamma"]);
    assert!(matches!(complete("cmp.g")[..], [Entry::Command(_)]));
    assert!(matches!(complete("cmp.b")[..], [Entry::Cvar(_)]));
    let builtins: Vec<_> = complete("e").iter().map(|e| e.name()).collect();
    assert!(builtins.contains(&"exec"));
    assert!(complete("cmp.x").is_empty());
}
