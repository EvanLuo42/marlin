//! Scalability groups: a cvar whose value picks a set of cvar values from a table.
//!
//! A group is a bool, integer or enum cvar (`sg.shadows = 0..3`), and its table a TOML document
//! with one table per level:
//!
//! ```toml
//! [sg.shadows.0]
//! r.shadow.enable = false
//!
//! [sg.shadows.2]
//! r.shadow.enable = true
//! r.shadow.resolution = 2048
//! ```
//!
//! (Levels of an enum group may also be named: `[sg.quality.high]`.) Whenever the group cvar
//! changes, from whatever layer, the level's values go into the [`Source::Scalability`] layer,
//! replacing the previous level's; cvars the level does not name lose their scalability value.
//! Every layer above it (project, device profile, user, command line, console) still wins, so
//! a cvar set there after the group applied stays as set: [`scalability_overrides`] lists
//! those.

use alloc::string::String;
use alloc::vec::Vec;
use core::ptr;

use marlin_std::sync::Mutex;
use toml::de::{DeTable, DeValue};

use crate::cvar::{Cvar, Source, Value};
use crate::error::{ApplyError, Cause, Error, Location};
use crate::file::{self, Parsed, line_of};
use crate::registry::{self, CallbackId};

struct Group {
    cvar: &'static Cvar,
    /// By level; each level's values in table order.
    levels: Vec<(i64, Vec<Parsed>)>,
    callback: CallbackId,
}

static GROUPS: Mutex<Vec<Group>> = Mutex::new(Vec::new());

/// Loads scalability tables (see the module docs) and applies the groups they touch. Loading
/// more tables merges them: a later value for the same group, level and cvar replaces the
/// earlier one, so a project can adjust the engine's tables.
#[must_use = "errors should be reported"]
pub fn load_scalability(text: &str) -> Vec<ApplyError> {
    let (table, mut errors) = file::parse(text);
    let mut found = Vec::new();
    walk(
        text,
        table.get_ref(),
        &mut String::new(),
        &mut errors,
        &mut found,
    );

    let mut touched: Vec<&'static Cvar> = Vec::new();
    {
        let mut groups = GROUPS.lock();
        for (group, level, (cvar, value)) in found {
            let i = match groups.iter().position(|g| ptr::eq(g.cvar, group)) {
                Some(i) => i,
                None => {
                    groups.push(Group {
                        cvar: group,
                        levels: Vec::new(),
                        callback: registry::on_change(group, apply),
                    });
                    groups.len() - 1
                }
            };
            let levels = &mut groups[i].levels;
            let values = match levels.iter().position(|(l, _)| *l == level) {
                Some(j) => &mut levels[j].1,
                None => {
                    levels.push((level, Vec::new()));
                    &mut levels.last_mut().expect("just pushed").1
                }
            };
            match values.iter_mut().find(|(c, _)| ptr::eq(*c, cvar)) {
                Some(slot) => slot.1 = value,
                None => values.push((cvar, value)),
            }
            if !touched.iter().any(|&g| ptr::eq(g, group)) {
                touched.push(group);
            }
        }
    }
    for group in touched {
        apply(group);
    }
    file::sort(&mut errors);
    errors
}

/// Finds the group tables under `prefix`.
fn walk(
    text: &str,
    table: &DeTable<'_>,
    prefix: &mut String,
    errors: &mut Vec<ApplyError>,
    found: &mut Vec<(&'static Cvar, i64, Parsed)>,
) {
    for (key, value) in table.iter() {
        let len = prefix.len();
        if len > 0 {
            prefix.push('.');
        }
        prefix.push_str(key.get_ref());
        let error = |errors: &mut Vec<ApplyError>, offset: usize, key: String, cause| {
            errors.push(ApplyError {
                location: Location::Line(line_of(text, offset)),
                key,
                source: cause,
            })
        };
        match (registry::find(prefix), value.get_ref()) {
            (Some(group), DeValue::Table(levels)) => {
                for (level_key, members) in levels.iter() {
                    let name = || alloc::format!("{prefix}.{}", level_key.get_ref());
                    let level = match group.parse_level(level_key.get_ref()) {
                        Ok(level) => level,
                        Err(e) => {
                            error(errors, level_key.span().start, name(), Cause::Set(e));
                            continue;
                        }
                    };
                    let DeValue::Table(members) = members.get_ref() else {
                        let cause = Cause::Syntax("expected a table of cvar values".into());
                        error(errors, level_key.span().start, name(), cause);
                        continue;
                    };
                    let mut parsed = Vec::new();
                    file::collect(
                        text,
                        members,
                        &mut String::new(),
                        Source::Scalability,
                        errors,
                        &mut parsed,
                    );
                    found.extend(parsed.into_iter().map(|p| (group, level, p)));
                }
            }
            (None, DeValue::Table(inner)) => walk(text, inner, prefix, errors, found),
            (Some(_), _) => {
                let cause = Cause::Syntax("expected a table of levels".into());
                error(errors, key.span().start, prefix.clone(), cause);
            }
            (None, _) => error(
                errors,
                key.span().start,
                prefix.clone(),
                Cause::Set(Error::Unknown),
            ),
        }
        prefix.truncate(len);
    }
}

/// Puts the values of `group`'s current level into the scalability layer.
fn apply(group: &'static Cvar) {
    let puts: Vec<(&'static Cvar, Option<Value>)> = {
        let groups = GROUPS.lock();
        let Some(g) = groups.iter().find(|g| ptr::eq(g.cvar, group)) else {
            return;
        };
        let level = group.level();
        let current = g
            .levels
            .iter()
            .find(|(l, _)| Some(*l) == level)
            .map(|(_, values)| values.as_slice())
            .unwrap_or_default();
        let mut puts: Vec<(&'static Cvar, Option<Value>)> = Vec::new();
        for (cvar, _) in g.levels.iter().flat_map(|(_, values)| values) {
            if !puts.iter().any(|(c, _)| ptr::eq(*c, *cvar)) {
                let value = current.iter().find(|(c, _)| ptr::eq(*c, *cvar));
                puts.push((cvar, value.map(|(_, v)| v.clone())));
            }
        }
        puts
    };
    // Outside the lock: a change may apply another group.
    for (cvar, value) in puts {
        cvar.put(Source::Scalability, value);
    }
}

/// The group cvars that have tables, sorted by name ignoring case.
pub fn scalability_groups() -> Vec<&'static Cvar> {
    let mut groups: Vec<_> = GROUPS.lock().iter().map(|g| g.cvar).collect();
    groups.sort_by(|a, b| marlin_text::utf8::cmp_ignore_ascii_case(a.name(), b.name()));
    groups
}

/// The cvars of `group`'s current level whose value comes from a layer above
/// [`Source::Scalability`] instead.
pub fn scalability_overrides(group: &'static Cvar) -> Vec<&'static Cvar> {
    let groups = GROUPS.lock();
    let Some(g) = groups.iter().find(|g| ptr::eq(g.cvar, group)) else {
        return Vec::new();
    };
    let level = group.level();
    g.levels
        .iter()
        .filter(|(l, _)| Some(*l) == level)
        .flat_map(|(_, values)| values)
        .map(|(cvar, _)| *cvar)
        .filter(|cvar| {
            cvar.set_by()
                .is_some_and(|s| s.layer() > Source::Scalability.layer())
        })
        .collect()
}

/// Drops every table and clears the scalability layer.
pub fn clear_scalability() {
    let groups = core::mem::take(&mut *GROUPS.lock());
    for group in groups {
        registry::remove_callback(group.callback);
        for (cvar, _) in group.levels.iter().flat_map(|(_, values)| values) {
            cvar.put(Source::Scalability, None);
        }
    }
}
