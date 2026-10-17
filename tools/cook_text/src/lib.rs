//! The cook pipeline's string table cooker: a [`StringTable`] source object in, a cooked
//! `marlin_text::loc::StringTable` out (zero-copy; see `marlin_text::loc` for the layout and
//! the message syntax).
//!
//! Every message is parsed when cooking, so a syntax error fails the cook with the key and the
//! byte position, and the runtime never meets one. Plural arms for categories the table's
//! language never selects (`few` in English) are warnings.

use marlin_asset::cooked::Encoding;
use marlin_cook_core::{CookContext, Cooked, Cooker, SourceObject};
use marlin_reflect::{Reflect, type_info};
use marlin_text::loc::{Locale, Message, Part, PluralRules, Selector, StringTableBuilder};

/// The localized messages of one locale, as authored.
///
/// ```text
/// StringTable
/// {
///     !Uuid = "…"
///     !Name = "UiFrench"
///     Locale = "fr"
///     Entries = [
///         { Key = "menu.start", Message = "Commencer" },
///         { Key = "hud.coins", Message = "{n, plural, one {# pièce} other {# pièces}}" },
///     ]
/// }
/// ```
///
/// The reflected name is pinned to `marlin_text::StringTable`, so the cooked type id is
/// `marlin_text::loc::STRING_TABLE_TYPE_ID` and the runtime can recognize tables without this
/// crate.
#[derive(Clone, PartialEq, Debug, Default, Reflect)]
#[reflect(name = "marlin_text::StringTable", default)]
pub struct StringTable {
    /// A BCP 47 tag: `en`, `pt-BR`, `zh-Hant`.
    pub locale: String,
    pub entries: Vec<StringEntry>,
}

/// One key and its message.
#[derive(Clone, PartialEq, Debug, Default, Reflect)]
#[reflect(default)]
pub struct StringEntry {
    pub key: String,
    pub message: String,
}

/// Cooks [`StringTable`] objects.
pub struct StringTableCooker;

impl Cooker for StringTableCooker {
    fn version(&self) -> u32 {
        1
    }

    fn cook(&self, object: &SourceObject<'_>, ctx: &mut CookContext<'_>) -> Result<Cooked, String> {
        let table: &StringTable = object
            .value
            .as_ref()
            .downcast()
            .ok_or("the object is not a StringTable")?;
        if table.locale.is_empty() {
            return Err("`Locale` is not set".into());
        }
        let locale = Locale::parse(&table.locale)
            .map_err(|e| format!("`Locale` = {:?}: {e}", table.locale))?;
        let rules = PluralRules::for_locale(&locale);
        let mut builder = StringTableBuilder::new(locale);
        let mut errors = Vec::new();
        for entry in &table.entries {
            if let Err(e) = builder.insert(&entry.key, &entry.message) {
                errors.push(e.to_string());
                continue;
            }
            lint(&entry.key, &entry.message, &locale, &rules, ctx);
        }
        if !errors.is_empty() {
            return Err(errors.join("; "));
        }
        Ok(Cooked {
            type_id: type_info::<StringTable>().id,
            encoding: Encoding::Cooked,
            payload: builder.build().map_err(|e| e.to_string())?,
        })
    }
}

/// Warns about plural arms that can never be chosen in `locale`.
fn lint(key: &str, message: &str, locale: &Locale, rules: &PluralRules, ctx: &mut CookContext<'_>) {
    let mut unused = Vec::new();
    // The message parsed in `insert`, so the walk cannot fail.
    let _ = Message::new_unchecked(message).walk(|part| {
        if let Part::Selector(Selector::Category(c)) = part
            && !rules.categories().contains(&c)
            && !unused.contains(&c)
        {
            unused.push(c);
        }
    });
    for category in unused {
        ctx.warn(format!(
            "message `{key}`: plural category `{category}` is never selected in {locale}"
        ));
    }
}
