//! Registration without lists: cvars and commands declared with `declare!` in this file, in a
//! helper module and inside a function are registered before the registry is first used. One
//! test: what the registry holds before anything else touches it matters.
//!
//! A process of its own (quick tier, docs/design/0014-testing.md §5): the cvar registry is global.
//! Run alone with `cargo test -p marlin-config --test isolated_registration`.

mod elsewhere;

use std::fmt::Write;
use std::ptr;

use marlin_config::{
    Command, CommandError, CvarBool, CvarI64, CvarString, Entry, Error, commands, complete, cvars,
    declare, execute, find, find_command, lookup, register_all, register_declared,
};

declare! {
    /// Attributes and doc comments stay on the static.
    #[allow(dead_code)]
    pub static DECLARED_BOOL: CvarBool = CvarBool::new("decl.bool", true, "Declared here.");
    static DECLARED_INT: CvarI64 = CvarI64::new("decl.int", 3, "").range(0, 10);
    static GREET: Command = Command::new("decl.greet", "decl.greet name: says hello.", greet);
    static DUPLICATE: CvarString = CvarString::new("decl.dup", "", "");
}

mod nested {
    marlin_config::declare! {
        pub(crate) static NESTED: marlin_config::CvarBool =
            marlin_config::CvarBool::new("decl.nested", false, "");
    }
}

fn greet(args: &[String], out: &mut dyn Write) -> Result<(), CommandError> {
    let [name] = args else {
        return Err(CommandError::Usage);
    };
    writeln!(out, "hello, {name}")?;
    Ok(())
}

/// Never called before the registry is used: its static is listed all the same.
fn local() -> &'static CvarString {
    declare! {
        static LOCAL: CvarString = CvarString::new("decl.local", "x", "Declared in a function.");
    }
    &LOCAL
}

#[test]
fn declared_entries_register_themselves() {
    // Nothing registered these by hand; the first lookup does.
    assert!(ptr::eq(find("decl.bool").unwrap(), &*DECLARED_BOOL));
    assert!(ptr::eq(find("DECL.INT").unwrap(), &*DECLARED_INT));
    assert!(ptr::eq(find_command("decl.greet").unwrap(), &GREET));
    assert!(ptr::eq(find("decl.nested").unwrap(), &*nested::NESTED));
    assert!(ptr::eq(
        find("decl.elsewhere.float").unwrap(),
        &*elsewhere::ELSEWHERE_FLOAT
    ));
    assert!(ptr::eq(
        find_command("decl.elsewhere.ping").unwrap(),
        &elsewhere::PING
    ));
    assert!(ptr::eq(find("decl.local").unwrap(), &**local()));
    for name in ["help", "list", "reset", "toggle", "exec"] {
        assert!(find_command(name).is_some(), "built-in {name}");
    }
    assert!(matches!(lookup("decl.greet"), Some(Entry::Command(_))));
    assert!(find("decl.greet").is_none() && find_command("decl.bool").is_none());

    // Two declarations of one name: the first in link order wins, the other is reported.
    let dup = find("decl.dup").unwrap();
    assert!(ptr::eq(dup, &*DUPLICATE) || ptr::eq(dup, &*elsewhere::DUPLICATE));
    match register_declared() {
        Err(Error::Duplicate(name)) => assert!(name.eq_ignore_ascii_case("decl.dup")),
        other => panic!("{other:?}"),
    }
    // The explicit path agrees with the declared one.
    register_all(&[&DECLARED_BOOL, &DECLARED_INT]).unwrap();

    // Each is there once.
    let declared: Vec<_> = cvars()
        .iter()
        .map(|c| c.name())
        // Names are case-insensitive: the winning `decl.dup` may be spelled `DECL.DUP`.
        .filter(|n| n.get(..5).is_some_and(|p| p.eq_ignore_ascii_case("decl.")))
        .collect();
    assert_eq!(
        declared,
        [
            "decl.bool",
            dup.name(),
            "decl.elsewhere.float",
            "decl.int",
            "decl.local",
            "decl.nested"
        ]
    );
    assert_eq!(commands().iter().filter(|c| c.name() == "help").count(), 1);
    let names: Vec<_> = complete("decl.e").iter().map(|e| e.name()).collect();
    assert_eq!(names, ["decl.elsewhere.float", "decl.elsewhere.ping"]);

    // The console sees them.
    let mut out = String::new();
    assert_eq!(
        execute(
            "decl.int 7; decl.greet Ann; decl.elsewhere.ping; decl.local",
            &mut out
        ),
        0
    );
    assert_eq!(DECLARED_INT.get(), 7);
    assert_eq!(out, "hello, Ann\npong\ndecl.local = \"x\" (default)\n");
}
