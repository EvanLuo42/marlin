//! String tables through the cook pipeline: `.masset` → cook → load in place → format.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use marlin_asset::cooked::{Encoding, UnitView};
use marlin_asset_db::{AssetDb, Options};
use marlin_cook_core::{CookOptions, Cookers, Ddc, LocalDdc, cook_all};
use marlin_cook_text::{StringTable as Source, StringTableCooker};
use marlin_reflect::type_info;
use marlin_text::loc::{
    Arg, Catalog, Date, FormatError, Locale, STRING_TABLE_TYPE_ID, StringTable, Time,
};

static NEXT: AtomicU32 = AtomicU32::new(0);

struct Project {
    root: PathBuf,
}

/// A cooked unit: its asset path, bytes and warnings.
#[derive(Debug)]
struct Unit {
    path: String,
    bytes: Vec<u8>,
    warnings: Vec<String>,
}

impl Project {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "marlin-cook-text-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn write(&self, rel: &str, text: &str) {
        let path = self.root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn copy_sample(&self, name: &str) {
        let sample = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data")
            .join(name);
        self.write(
            &format!("strings/{name}"),
            &fs::read_to_string(sample).unwrap(),
        );
    }

    fn cook(&self) -> Result<Vec<Unit>, Vec<String>> {
        let db = AssetDb::build(
            &self.root,
            Options {
                journal: false,
                ..Options::default()
            },
        )
        .unwrap();
        let mut cookers = Cookers::new();
        cookers.register::<Source>(StringTableCooker).unwrap();
        let ddc = LocalDdc::open(self.root.join(".marlin/ddc")).unwrap();
        let (report, _) = cook_all(&db, &cookers, &ddc, &CookOptions::default());
        if !report.errors.is_empty() {
            return Err(report.errors.iter().map(|e| e.to_string()).collect());
        }
        let mut units: Vec<Unit> = report
            .units
            .iter()
            .map(|u| Unit {
                path: u.path.clone(),
                bytes: ddc.get(u.key).unwrap().unwrap(),
                warnings: u.warnings.clone(),
            })
            .collect();
        units.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(units)
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// The payload of the unit's only object, checked to be a cooked string table.
fn payload(unit: &Unit) -> &[u8] {
    let view = UnitView::new(&unit.bytes).unwrap();
    let object = view.object(0).unwrap();
    assert_eq!(object.type_id, STRING_TABLE_TYPE_ID);
    assert_eq!(object.encoding, Encoding::Cooked);
    view.payload(&object)
}

#[test]
fn the_type_id_is_pinned() {
    assert_eq!(type_info::<Source>().id.0, STRING_TABLE_TYPE_ID);
}

#[test]
fn samples_cook_load_and_format() {
    let p = Project::new("samples");
    p.copy_sample("strings_en.masset");
    p.copy_sample("strings_ru.masset");
    let units = p.cook().unwrap();
    assert_eq!(units.len(), 2);
    assert_eq!(units[0].path, "strings/strings_en");
    assert!(units.iter().all(|u| u.warnings.is_empty()), "no warnings");
    let en = StringTable::new(payload(&units[0])).unwrap();
    let ru = StringTable::new(payload(&units[1])).unwrap();
    assert_eq!(en.locale(), Locale::parse("en").unwrap());
    assert_eq!(ru.locale(), Locale::parse("ru-RU").unwrap());
    assert_eq!((en.len(), ru.len()), (6, 3));

    let tables = [ru, en];
    let catalog = Catalog::new(&tables);
    let text = |key: &str, args: &[Arg]| catalog.format_to_string(key, args).unwrap();
    assert_eq!(text("menu.start", &[]), "Начать игру");
    assert_eq!(text("menu.quit", &[]), "Quit", "falls back to English");
    for (n, expected) in [
        (0, "Нет монет"),
        (1, "1 монета"),
        (3, "3 монеты"),
        (5, "5 монет"),
        (21, "21 монета"),
        (12345, "12\u{a0}345 монет"),
    ] {
        assert_eq!(text("hud.coins", &[Arg::new("n", n)]), expected);
    }
    assert_eq!(
        text("hud.greeting", &[Arg::new("player", "Игорь")]),
        "Welcome back, Игорь!"
    );
    // The English fallback formats numbers the English way.
    assert_eq!(
        text("hud.distance", &[Arg::new("km", 1234.56)]),
        "1,234.6 km to go"
    );
    let when = [
        Arg::new("when", Date::new(2025, 3, 8).unwrap()),
        Arg::new("at", Time::new(18, 45, 0).unwrap()),
    ];
    assert_eq!(
        text("save.date", &when),
        "Сохранено 8 марта 2025 г. в 18:45"
    );
    assert_eq!(
        tables[1]
            .message("save.date")
            .unwrap()
            .format_to_string(&tables[1].locale(), &when),
        Ok("Saved on March 8, 2025 at 6:45\u{202f}PM".into())
    );
    assert_eq!(
        catalog.format_to_string("hud.coins", &[]),
        Err(FormatError::MissingArgument("n"))
    );

    // Cooking is deterministic: a second project cooks the same bytes.
    let q = Project::new("again");
    q.copy_sample("strings_ru.masset");
    q.copy_sample("strings_en.masset");
    let again = q.cook().unwrap();
    assert_eq!(payload(&again[0]), payload(&units[0]));
    assert_eq!(payload(&again[1]), payload(&units[1]));
}

const TABLE: &str = "019a0000-0000-7000-8000-000000000e10";

fn table_object(locale: &str, entries: &str) -> String {
    format!(
        "!Version 1\nStringTable {{ !Uuid = \"{TABLE}\" !Name = \"T\" Locale = \"{locale}\" Entries = [{entries}] }}\n"
    )
}

#[test]
fn bad_messages_fail_the_cook_with_key_and_position() {
    let p = Project::new("bad");
    p.write(
        "t.masset",
        &table_object(
            "en",
            r#"{ Key = "ok", Message = "fine" }, { Key = "broken", Message = "{n, plural, one {x}}" }, { Key = "ok", Message = "again" }"#,
        ),
    );
    let errors = p.cook().unwrap_err().join("\n");
    assert!(
        errors.contains("message `broken`: the plural has no `other` arm at byte 19"),
        "{errors}"
    );
    assert!(errors.contains("key `ok` appears twice"), "{errors}");
}

#[test]
fn bad_locales_fail_the_cook() {
    for (locale, expected) in [
        ("", "`Locale` is not set"),
        ("english", "bad language subtag"),
    ] {
        let p = Project::new("locale");
        p.write("t.masset", &table_object(locale, ""));
        let errors = p.cook().unwrap_err().join("\n");
        assert!(errors.contains(expected), "{errors}");
    }
}

#[test]
fn unreachable_plural_arms_warn() {
    let p = Project::new("lint");
    p.write(
        "t.masset",
        &table_object(
            "en",
            r#"{ Key = "k", Message = "{n, plural, one {a} few {b} other {c}}" }"#,
        ),
    );
    let units = p.cook().unwrap();
    assert_eq!(
        units[0].warnings,
        ["T: message `k`: plural category `few` is never selected in en"],
        "the pipeline prefixes the object name"
    );
    let table = StringTable::new(payload(&units[0])).unwrap();
    assert_eq!(
        table.get("k"),
        Some("{n, plural, one {a} few {b} other {c}}")
    );
}
