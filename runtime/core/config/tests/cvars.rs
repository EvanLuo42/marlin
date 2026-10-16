//! Typed cvars, parsing, the registry and change notification. Each test uses its own cvars, so
//! they can run in parallel against the global registry.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use marlin_config::{
    Bounds, CvarBool, CvarEnum, CvarF64, CvarI64, CvarString, Error, Expected, Flags, Kind, Source,
    complete, cvars, find, generation, on_any_change, on_change, register, register_all,
    remove_callback, set, unregister,
};

#[test]
fn bool_cvar() {
    static B: CvarBool = CvarBool::new("test.bool", true, "A bool.");
    assert!(B.get());
    assert_eq!(
        (B.name(), B.help(), B.kind()),
        ("test.bool", "A bool.", Kind::Bool)
    );
    B.set(false);
    assert!(!B.get());
    for (text, value) in [
        ("TRUE", true),
        ("0", false),
        ("on", true),
        ("No", false),
        (" yes ", true),
    ] {
        B.set_str(text, Source::Console).unwrap();
        assert_eq!(B.get(), value, "{text}");
    }
    assert_eq!(
        B.set_str("maybe", Source::Console),
        Err(Error::Invalid(Expected::Bool))
    );
    assert_eq!(B.value_string(), "true");
    assert_eq!(B.default_string(), "true");
}

#[test]
fn int_cvar() {
    static I: CvarI64 = CvarI64::new("test.int", 10, "An int.").range(-100, 100);
    assert_eq!(I.get(), 10);
    assert_eq!(I.int_range(), Some((-100, 100)));
    for (text, value) in [
        ("42", 42),
        ("-7", -7),
        ("+5", 5),
        ("0x1F", 31),
        ("-0x10", -16),
    ] {
        I.set_str(text, Source::Console).unwrap();
        assert_eq!(I.get(), value, "{text}");
    }
    for text in ["", "12a", "1.5", "--3", "+-3", "0x", "0x+1"] {
        assert_eq!(
            I.set_str(text, Source::Console),
            Err(Error::Invalid(Expected::Int)),
            "{text}"
        );
    }
    let err = I.set_str("101", Source::Console).unwrap_err();
    assert_eq!(err, Error::OutOfRange(Bounds::Int(-100, 100)));
    assert_eq!(err.to_string(), "out of range [-100, 100]");
    assert_eq!(I.get(), -16, "a failed set leaves the value alone");
    // Code clamps instead of failing.
    I.set(1000);
    assert_eq!(I.get(), 100);
    I.set(i64::MIN);
    assert_eq!(I.get(), -100);

    static WIDE: CvarI64 = CvarI64::new("test.int.wide", 0, "");
    WIDE.set_str("-9223372036854775808", Source::Console)
        .unwrap();
    assert_eq!(WIDE.get(), i64::MIN);
    WIDE.set_str("9223372036854775807", Source::Console)
        .unwrap();
    assert_eq!(WIDE.get(), i64::MAX);
    assert!(
        WIDE.set_str("9223372036854775808", Source::Console)
            .is_err()
    );
}

#[test]
fn float_cvar() {
    static F: CvarF64 = CvarF64::new("test.float", 0.5, "A float.").range(0.0, 2.0);
    assert_eq!(F.get(), 0.5);
    F.set_str("1.25", Source::Console).unwrap();
    assert_eq!(F.get(), 1.25);
    F.set_str("1", Source::Console).unwrap();
    assert_eq!(F.value_string(), "1");
    for text in ["nan", "inf", "-inf", "x"] {
        assert_eq!(
            F.set_str(text, Source::Console),
            Err(Error::Invalid(Expected::Float))
        );
    }
    assert_eq!(
        F.set_str("2.5", Source::Console),
        Err(Error::OutOfRange(Bounds::Float(0.0, 2.0)))
    );
    F.set(-3.0);
    assert_eq!(F.get(), 0.0);
    F.set(f64::NAN);
    assert_eq!(F.get(), 0.0);
    F.set(0.1);
    assert_eq!(F.value_string().parse::<f64>().unwrap(), 0.1);
}

#[test]
fn string_cvar() {
    static S: CvarString = CvarString::new("test.string", "default", "A string.");
    assert_eq!(S.get(), "default");
    assert!(S.is_default());
    S.set("  spaced  ");
    assert_eq!(S.get(), "  spaced  ");
    S.with(|s| assert_eq!(s, "  spaced  "));
    assert!(!S.is_default());
    S.set_str("from console", Source::Console).unwrap();
    assert_eq!(S.value_string(), "from console");
    // Setting the default text counts as the default.
    S.set("default");
    assert!(S.is_default());
}

#[test]
fn enum_cvar() {
    static Q: CvarEnum = CvarEnum::new(
        "test.quality",
        1,
        &["low", "medium", "high"],
        "Quality preset.",
    );
    assert_eq!((Q.get(), Q.get_name()), (1, "medium"));
    assert_eq!(Q.enum_names(), ["low", "medium", "high"]);
    Q.set_str("HIGH", Source::Console).unwrap();
    assert_eq!(Q.get(), 2);
    Q.set_str("0", Source::Console).unwrap();
    assert_eq!(Q.get_name(), "low");
    let err = Q.set_str("ultra", Source::Console).unwrap_err();
    assert_eq!(err.to_string(), "expected one of low, medium, high");
    assert!(Q.set_str("3", Source::Console).is_err());
    Q.set(99);
    assert_eq!(Q.get_name(), "high");
    assert_eq!(Q.value_string(), "high");
}

#[test]
fn registry() {
    static A: CvarBool = CvarBool::new("reg.Alpha", false, "");
    static B: CvarI64 = CvarI64::new("reg.beta", 1, "");
    static C: CvarString = CvarString::new("reg.gamma", "", "");
    static CLASH: CvarBool = CvarBool::new("REG.ALPHA", false, "");
    register_all(&[&A, &B, &C]).unwrap();
    register(&A).unwrap();
    assert_eq!(register(&CLASH), Err(Error::Duplicate("REG.ALPHA")));
    assert_eq!(
        Error::Duplicate("x").to_string(),
        "`x` is already registered"
    );

    assert!(std::ptr::eq(find("reg.alpha").unwrap(), &*A));
    assert!(std::ptr::eq(find("REG.Beta").unwrap(), &*B));
    assert!(find("reg.delta").is_none());

    set("REG.BETA", "5", Source::Console).unwrap();
    assert_eq!(B.get(), 5);
    assert_eq!(set("reg.nope", "1", Source::Console), Err(Error::Unknown));

    let names: Vec<_> = complete("Reg.").iter().map(|c| c.name()).collect();
    assert_eq!(names, ["reg.Alpha", "reg.beta", "reg.gamma"]);
    assert!(cvars().iter().any(|c| c.name() == "reg.gamma"));

    unregister(&C);
    assert!(find("reg.gamma").is_none());
    // Unregistering a cvar that only shares the name does nothing.
    unregister(&CLASH);
    assert!(find("reg.alpha").is_some());
}

#[test]
fn generations_and_callbacks() {
    static X: CvarI64 = CvarI64::new("cb.x", 0, "");
    static Y: CvarI64 = CvarI64::new("cb.y", 0, "");
    register_all(&[&X, &Y]).unwrap();

    let x_calls = Arc::new(AtomicUsize::new(0));
    let any_calls = Arc::new(AtomicUsize::new(0));
    let id = on_change(&X, {
        let x_calls = x_calls.clone();
        move |c| {
            assert_eq!(c.name(), "cb.x");
            x_calls.fetch_add(1, Ordering::SeqCst);
        }
    });
    let any = on_any_change({
        let any_calls = any_calls.clone();
        move |c| {
            if c.name().starts_with("cb.") {
                any_calls.fetch_add(1, Ordering::SeqCst);
            }
        }
    });
    // A callback may set other cvars.
    let chain = on_change(&Y, |_| X.set(Y.get() * 10));

    let (g, gx) = (generation(), X.generation());
    X.set(1);
    assert!(generation() > g);
    assert_eq!(X.generation(), gx + 1);
    assert_eq!(x_calls.load(Ordering::SeqCst), 1);

    // No change, no notification.
    X.set(1);
    X.set_str("1", Source::Console).unwrap();
    assert_eq!(X.generation(), gx + 1);
    assert_eq!(x_calls.load(Ordering::SeqCst), 1);

    Y.set(2);
    assert_eq!(X.get(), 20);
    assert_eq!(x_calls.load(Ordering::SeqCst), 2);
    assert_eq!(any_calls.load(Ordering::SeqCst), 3);

    assert!(remove_callback(id));
    assert!(!remove_callback(id));
    X.set(3);
    assert_eq!(x_calls.load(Ordering::SeqCst), 2);
    assert!(remove_callback(any) && remove_callback(chain));

    X.reset(Source::Console).unwrap();
    assert!(X.is_default());
}

#[test]
fn flags() {
    static F: CvarBool = CvarBool::new("flags.f", false, "").archive().cheat();
    static G: CvarBool = CvarBool::new("flags.g", false, "")
        .with_flags(Flags::READ_ONLY.union(Flags::REQUIRES_RESTART));
    assert_eq!(F.flags(), Flags::ARCHIVE | Flags::CHEAT);
    assert!(F.flags().contains(Flags::CHEAT) && !F.flags().contains(Flags::READ_ONLY));
    assert_eq!(format!("{:?}", G.flags()), "READ_ONLY | REQUIRES_RESTART");
    assert_eq!(format!("{:?}", Flags::NONE), "NONE");
}
