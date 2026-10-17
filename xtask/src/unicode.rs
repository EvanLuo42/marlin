//! `cargo xtask gen-unicode`: marlin-text's Unicode tables from the Unicode Character Database.
//!
//! Reads the UCD text files (never committed; pass `--ucd <dir>` or set `MARLIN_UCD_DIR`) and
//! writes `runtime/core/text/src/unicode/tables.rs`. The files are only parsed, as data; every
//! line is checked and a malformed one is an error, not a panic. The output depends on nothing
//! but the files, so the same UCD always gives the same bytes.
//!
//! Besides extracting, it cross-checks properties the UCD states twice: the derived
//! Full_Composition_Exclusion, NFD_QC and NFKD_QC must equal what follows from UnicodeData and
//! CompositionExclusions, and the Hangul syllables' Grapheme_Cluster_Break must follow the
//! LV/LVT pattern the runtime computes instead of storing.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use crate::Result;

const OUTPUT: &str = "runtime/core/text/src/unicode/tables.rs";

const HANGUL_S_BASE: u32 = 0xac00;
const HANGUL_L_BASE: u32 = 0x1100;
const HANGUL_V_BASE: u32 = 0x1161;
const HANGUL_T_BASE: u32 = 0x11a7;
const HANGUL_T_COUNT: u32 = 28;
const HANGUL_N_COUNT: u32 = 21 * HANGUL_T_COUNT;
const HANGUL_S_COUNT: u32 = 19 * HANGUL_N_COUNT;

/// Range table entries hold `start << 11 | (len - 1)`.
const RANGE_LEN_BITS: u32 = 11;
/// Delta table entries hold `start << 11 | (count - 1) << 1 | stride_is_2`.
const DELTA_COUNT_BITS: u32 = 10;

/// Grapheme_Cluster_Break values in the order of the generated `GCB_*` constants.
const GCB_VALUES: &[&str] = &[
    "Other",
    "CR",
    "LF",
    "Control",
    "Extend",
    "ZWJ",
    "Regional_Indicator",
    "Prepend",
    "SpacingMark",
    "L",
    "V",
    "T",
    "LV",
    "LVT",
];
/// Set on top of the Grapheme_Cluster_Break value for Extended_Pictographic characters.
const EXT_PICT_BIT: u8 = 0x10;

/// Word_Break values in the order of the generated `WB_*` constants.
const WB_VALUES: &[&str] = &[
    "Other",
    "CR",
    "LF",
    "Newline",
    "Extend",
    "ZWJ",
    "Regional_Indicator",
    "Format",
    "Katakana",
    "Hebrew_Letter",
    "ALetter",
    "Single_Quote",
    "Double_Quote",
    "MidNumLet",
    "MidLetter",
    "MidNum",
    "Numeric",
    "ExtendNumLet",
    "WSegSpace",
];

const HELP: &str = "usage: cargo xtask gen-unicode [--ucd <dir>]   (default: $MARLIN_UCD_DIR)";

pub fn gen_unicode(root: &Path, args: &[String]) -> Result {
    let mut dir = std::env::var_os("MARLIN_UCD_DIR").map(PathBuf::from);
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--ucd" => dir = Some(args.next().ok_or(HELP)?.into()),
            _ => return Err(format!("unknown argument `{arg}`\n{HELP}").into()),
        }
    }
    let dir = dir.ok_or_else(|| format!("no UCD directory\n{HELP}"))?;
    let ucd = Ucd::load(&dir)?;
    let (text, sizes) = ucd.render();
    fs::write(root.join(OUTPUT), text)?;
    println!(
        "gen-unicode: wrote {OUTPUT} (Unicode {}, {} bytes of tables)",
        ucd.version,
        sizes.iter().map(|(_, n)| n).sum::<usize>()
    );
    for (name, bytes) in &sizes {
        println!("  {name:<20} {bytes:>7}");
    }
    Ok(())
}

/// A UCD file, read as text and parsed line by line.
struct File {
    name: String,
    text: String,
}

impl File {
    fn read(dir: &Path, name: &str) -> Result<Self> {
        let path = dir.join(name);
        let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Self {
            name: name.to_owned(),
            text,
        })
    }

    /// The `X.Y.Z` of a first line like `# Name-X.Y.Z.txt`.
    fn version(&self) -> Result<String> {
        let first = self.text.lines().next().unwrap_or("");
        first
            .strip_suffix(".txt")
            .and_then(|s| s.rsplit_once('-'))
            .map(|(_, v)| v.to_owned())
            .filter(|v| v.split('.').count() == 3 && v.split('.').all(|p| p.parse::<u8>().is_ok()))
            .ok_or_else(|| format!("{}: no version in the first line", self.name).into())
    }

    /// Data lines with comments stripped: `(line number, fields)`.
    fn records(&self) -> impl Iterator<Item = (usize, Vec<&str>)> {
        self.text.lines().enumerate().filter_map(|(i, line)| {
            let data = line.split('#').next().unwrap_or("").trim();
            (!data.is_empty()).then(|| (i + 1, data.split(';').map(str::trim).collect()))
        })
    }

    fn error(&self, line: usize, what: impl std::fmt::Display) -> Box<dyn std::error::Error> {
        format!("{}:{line}: {what}", self.name).into()
    }

    /// `XXXX` or `XXXX..YYYY`.
    fn range(&self, line: usize, field: &str) -> Result<(u32, u32)> {
        let (a, b) = field.split_once("..").unwrap_or((field, field));
        let (a, b) = (self.code_point(line, a)?, self.code_point(line, b)?);
        if a > b {
            return Err(self.error(line, "reversed range"));
        }
        Ok((a, b))
    }

    /// A code point, surrogates included (property files list them).
    fn code_point(&self, line: usize, field: &str) -> Result<u32> {
        let cp = u32::from_str_radix(field.trim(), 16)
            .map_err(|_| self.error(line, format!("bad code point `{field}`")))?;
        if cp > 0x10ffff {
            return Err(self.error(line, format!("code point {cp:X} out of range")));
        }
        Ok(cp)
    }

    /// A code point that is a Unicode scalar value, as every mapping target must be.
    fn scalar(&self, line: usize, field: &str) -> Result<u32> {
        let cp = self.code_point(line, field)?;
        if char::from_u32(cp).is_none() {
            return Err(self.error(line, format!("surrogate {cp:04X} in a mapping")));
        }
        Ok(cp)
    }

    fn code_points(&self, line: usize, field: &str) -> Result<Vec<u32>> {
        field
            .split_whitespace()
            .map(|cp| self.scalar(line, cp))
            .collect()
    }

    /// A two-field property file (`range ; Value`) as code point → value index in `names`,
    /// keeping only the values in `names`.
    fn enumerated(&self, names: &[&str], out: &mut BTreeMap<u32, u8>) -> Result {
        for (line, fields) in self.records() {
            let [range, value] = fields[..] else {
                return Err(self.error(line, "expected `range ; value`"));
            };
            let Some(index) = names.iter().position(|n| *n == value) else {
                continue;
            };
            let (a, b) = self.range(line, range)?;
            for cp in a..=b {
                if out.insert(cp, index as u8).is_some() {
                    return Err(self.error(line, format!("{cp:04X} listed twice")));
                }
            }
        }
        Ok(())
    }

    /// The code points that have the binary property `name`.
    fn binary(&self, name: &str) -> Result<BTreeSet<u32>> {
        let mut out = BTreeSet::new();
        for (line, fields) in self.records() {
            if fields.get(1) != Some(&name) {
                continue;
            }
            if fields.len() != 2 {
                return Err(self.error(line, "expected `range ; property`"));
            }
            let (a, b) = self.range(line, fields[0])?;
            out.extend(a..=b);
        }
        Ok(out)
    }

    /// A quick-check property (`range ; NFC_QC ; N|M`): code point → 1 (No) or 2 (Maybe).
    fn quick_check(&self, name: &str) -> Result<BTreeMap<u32, u8>> {
        let mut out = BTreeMap::new();
        for (line, fields) in self.records() {
            if fields.get(1) != Some(&name) {
                continue;
            }
            let value = match fields.get(2) {
                Some(&"N") => 1,
                Some(&"M") => 2,
                _ => return Err(self.error(line, format!("bad {name} value"))),
            };
            let (a, b) = self.range(line, fields[0])?;
            for cp in a..=b {
                out.insert(cp, value);
            }
        }
        Ok(out)
    }
}

#[derive(Default)]
struct Char {
    ccc: u8,
    /// `(compatibility, mapping)`: the one-level decomposition.
    decomposition: Option<(bool, Vec<u32>)>,
    upper: Option<u32>,
    lower: Option<u32>,
}

struct Ucd {
    version: String,
    license: String,
    chars: BTreeMap<u32, Char>,
    full_composition_exclusion: BTreeSet<u32>,
    nfc_qc: BTreeMap<u32, u8>,
    nfkc_qc: BTreeMap<u32, u8>,
    /// Simple case folding (statuses C and S).
    fold_simple: BTreeMap<u32, u32>,
    /// Full case folding where it differs from the simple one (status F).
    fold_full: BTreeMap<u32, Vec<u32>>,
    grapheme: BTreeMap<u32, u8>,
    word: BTreeMap<u32, u8>,
}

impl Ucd {
    fn load(dir: &Path) -> Result<Self> {
        let unicode_data = File::read(dir, "UnicodeData.txt")?;
        let case_folding = File::read(dir, "CaseFolding.txt")?;
        let normalization = File::read(dir, "DerivedNormalizationProps.txt")?;
        let exclusions = File::read(dir, "CompositionExclusions.txt")?;
        let grapheme = File::read(dir, "auxiliary/GraphemeBreakProperty.txt")?;
        let word = File::read(dir, "auxiliary/WordBreakProperty.txt")?;
        let emoji = File::read(dir, "emoji/emoji-data.txt")?;
        let license = File::read(dir, "license.txt")?;

        let version = normalization.version()?;
        for file in [&case_folding, &exclusions, &grapheme, &word] {
            let v = file.version()?;
            if v != version {
                return Err(format!(
                    "{} is version {v}, {} is {version}",
                    file.name, normalization.name
                )
                .into());
            }
        }

        let chars = parse_unicode_data(&unicode_data)?;

        // Full_Composition_Exclusion = the listed exclusions, singletons and non-starter
        // decompositions. The derived property must say the same.
        let mut excluded = BTreeSet::new();
        for (line, fields) in exclusions.records() {
            let [cp] = fields[..] else {
                return Err(exclusions.error(line, "expected one code point"));
            };
            excluded.insert(exclusions.code_point(line, cp)?);
        }
        for (&cp, c) in &chars {
            if let Some((false, mapping)) = &c.decomposition {
                let first_ccc = chars.get(&mapping[0]).map_or(0, |c| c.ccc);
                if mapping.len() == 1 || c.ccc != 0 || first_ccc != 0 {
                    excluded.insert(cp);
                }
            }
        }
        let full_composition_exclusion = normalization.binary("Full_Composition_Exclusion")?;
        if excluded != full_composition_exclusion {
            return Err(format!(
                "Full_Composition_Exclusion does not match UnicodeData + CompositionExclusions: \
                 {:04X?} vs {:04X?}",
                excluded
                    .difference(&full_composition_exclusion)
                    .collect::<Vec<_>>(),
                full_composition_exclusion
                    .difference(&excluded)
                    .collect::<Vec<_>>()
            )
            .into());
        }

        // NFD_QC / NFKD_QC = No exactly where a decomposition exists.
        let hangul = HANGUL_S_BASE..HANGUL_S_BASE + HANGUL_S_COUNT;
        for (property, compat) in [("NFD_QC", false), ("NFKD_QC", true)] {
            let derived: BTreeSet<u32> = normalization.quick_check(property)?.into_keys().collect();
            let mut expected: BTreeSet<u32> = chars
                .iter()
                .filter(|(_, c)| matches!(c.decomposition, Some((k, _)) if compat || !k))
                .map(|(&cp, _)| cp)
                .collect();
            expected.extend(hangul.clone());
            if derived != expected {
                return Err(
                    format!("{property} does not match UnicodeData's decompositions").into(),
                );
            }
        }

        let mut fold_simple = BTreeMap::new();
        let mut fold_full = BTreeMap::new();
        let mut has_simple_only = BTreeSet::new();
        for (line, fields) in case_folding.records() {
            let [cp, status, mapping, ..] = fields[..] else {
                return Err(case_folding.error(line, "expected `code; status; mapping;`"));
            };
            let cp = case_folding.scalar(line, cp)?;
            let mapping = case_folding.code_points(line, mapping)?;
            match (status, &mapping[..]) {
                ("C", &[m]) => {
                    fold_simple.insert(cp, m);
                }
                ("S", &[m]) => {
                    fold_simple.insert(cp, m);
                    has_simple_only.insert(cp);
                }
                ("F", m) if !m.is_empty() => {
                    fold_full.insert(cp, mapping.clone());
                }
                ("T", _) => {} // Turkic dotted/dotless i: not part of default folding.
                _ => return Err(case_folding.error(line, "bad status or mapping")),
            }
        }
        // Status S only ever accompanies F; full folding relies on that.
        if let Some(cp) = has_simple_only
            .iter()
            .find(|cp| !fold_full.contains_key(cp))
        {
            return Err(format!("CaseFolding: {cp:04X} has status S but no F").into());
        }

        let mut grapheme_values = BTreeMap::new();
        grapheme.enumerated(GCB_VALUES, &mut grapheme_values)?;
        // Hangul syllables are LV at every 28th code point and LVT otherwise; the runtime
        // computes that instead of storing 399 alternating ranges.
        for cp in hangul {
            let expected = if (cp - HANGUL_S_BASE).is_multiple_of(HANGUL_T_COUNT) {
                "LV"
            } else {
                "LVT"
            };
            match grapheme_values.remove(&cp) {
                Some(v) if GCB_VALUES[v as usize] == expected => {}
                _ => {
                    return Err(
                        format!("{cp:04X}: Grapheme_Cluster_Break is not {expected}").into(),
                    );
                }
            }
        }
        for cp in emoji.binary("Extended_Pictographic")? {
            *grapheme_values.entry(cp).or_insert(0) |= EXT_PICT_BIT;
        }

        let mut word_values = BTreeMap::new();
        word.enumerated(WB_VALUES, &mut word_values)?;

        Ok(Self {
            version,
            license: license.text,
            chars,
            full_composition_exclusion,
            nfc_qc: normalization.quick_check("NFC_QC")?,
            nfkc_qc: normalization.quick_check("NFKC_QC")?,
            fold_simple,
            fold_full,
            grapheme: grapheme_values,
            word: word_values,
        })
    }

    /// The full (recursive) decomposition of `cp`: canonical only, or compatibility too.
    fn decompose(&self, cp: u32, compat: bool, out: &mut Vec<u32>) {
        if (HANGUL_S_BASE..HANGUL_S_BASE + HANGUL_S_COUNT).contains(&cp) {
            let s = cp - HANGUL_S_BASE;
            out.push(HANGUL_L_BASE + s / HANGUL_N_COUNT);
            out.push(HANGUL_V_BASE + s % HANGUL_N_COUNT / HANGUL_T_COUNT);
            if !s.is_multiple_of(HANGUL_T_COUNT) {
                out.push(HANGUL_T_BASE + s % HANGUL_T_COUNT);
            }
            return;
        }
        match self.chars.get(&cp).and_then(|c| c.decomposition.as_ref()) {
            Some((is_compat, mapping)) if compat || !is_compat => {
                for &m in mapping {
                    self.decompose(m, compat, out);
                }
            }
            _ => out.push(cp),
        }
    }

    /// The source of `tables.rs`, and the byte size of each table.
    fn render(&self) -> (String, Vec<(&'static str, usize)>) {
        let mut out = String::new();
        let mut sizes = Vec::new();

        // Normalization.
        let ccc: BTreeMap<u32, u8> = self
            .chars
            .iter()
            .filter(|(_, c)| c.ccc != 0)
            .map(|(&cp, c)| (cp, c.ccc))
            .collect();
        let mut qc = BTreeMap::new();
        for (&cp, &v) in &self.nfc_qc {
            *qc.entry(cp).or_insert(0) |= v;
        }
        for (&cp, &v) in &self.nfkc_qc {
            *qc.entry(cp).or_insert(0) |= v << 2;
        }
        let mut canonical = BTreeMap::new();
        let mut compat = BTreeMap::new();
        for (&cp, c) in &self.chars {
            let Some((is_compat, _)) = c.decomposition else {
                continue;
            };
            let mut full_compat = Vec::new();
            self.decompose(cp, true, &mut full_compat);
            if is_compat {
                compat.insert(cp, full_compat);
            } else {
                let mut full = Vec::new();
                self.decompose(cp, false, &mut full);
                if full != full_compat {
                    compat.insert(cp, full_compat);
                }
                canonical.insert(cp, full);
            }
        }
        let mut compose = Vec::new();
        for (&cp, c) in &self.chars {
            if let Some((false, mapping)) = &c.decomposition
                && mapping.len() == 2
                && !self.full_composition_exclusion.contains(&cp)
            {
                compose.push(
                    u64::from(mapping[0]) << 42 | u64::from(mapping[1]) << 21 | u64::from(cp),
                );
            }
        }
        compose.sort_unstable();

        // Case.
        let lower: BTreeMap<u32, u32> = self
            .chars
            .iter()
            .filter_map(|(&cp, c)| Some((cp, c.lower?)))
            .collect();
        let upper: BTreeMap<u32, u32> = self
            .chars
            .iter()
            .filter_map(|(&cp, c)| Some((cp, c.upper?)))
            .collect();

        writeln!(
            out,
            "// Generated by `cargo xtask gen-unicode` (xtask/src/unicode.rs) from the Unicode \
             Character\n// Database {}. Do not edit; regenerate.\n//",
            self.version
        )
        .unwrap();
        writeln!(
            out,
            "// The data is derived from the Unicode Data Files, distributed under this license:\n//"
        )
        .unwrap();
        for line in self.license.lines() {
            if line.trim().is_empty() {
                out.push_str("//\n");
            } else {
                writeln!(out, "// {}", line.trim_end()).unwrap();
            }
        }
        out.push_str(
            "\n#![allow(dead_code, reason = \"every property value is listed, used or not\")]\n\n",
        );
        let mut parts = self.version.split('.').map(|p| p.parse::<u8>().unwrap());
        let (major, minor, update) = (
            parts.next().unwrap(),
            parts.next().unwrap(),
            parts.next().unwrap(),
        );
        writeln!(
            out,
            "/// The Unicode version of these tables.\npub const UNICODE_VERSION: (u8, u8, u8) = \
             ({major}, {minor}, {update});\n"
        )
        .unwrap();

        out.push_str("// Grapheme_Cluster_Break values in `GRAPHEME_VALUES` (low 4 bits).\n");
        for (i, name) in GCB_VALUES.iter().enumerate() {
            writeln!(out, "pub const GCB_{}: u8 = {i};", constant(name)).unwrap();
        }
        writeln!(
            out,
            "/// Set in `GRAPHEME_VALUES` for Extended_Pictographic.\npub const EXT_PICT: u8 = \
             {EXT_PICT_BIT:#x};\n"
        )
        .unwrap();
        out.push_str("// Word_Break values in `WORD_VALUES`.\n");
        for (i, name) in WB_VALUES.iter().enumerate() {
            writeln!(out, "pub const WB_{}: u8 = {i};", constant(name)).unwrap();
        }
        out.push('\n');

        range_table(
            &mut out,
            &mut sizes,
            "CCC",
            "Canonical_Combining_Class, where nonzero.",
            &ccc,
        );
        range_table(
            &mut out,
            &mut sizes,
            "QC",
            "Quick-check values: NFC_QC in bits 0-1, NFKC_QC in bits 2-3 (1 = No, 2 = Maybe).",
            &qc,
        );
        string_map(
            &mut out,
            &mut sizes,
            "CANONICAL",
            "Full canonical decompositions (Hangul syllables excluded).",
            &canonical,
        );
        string_map(
            &mut out,
            &mut sizes,
            "COMPAT",
            "Full compatibility decompositions that differ from the canonical one.",
            &compat,
        );
        writeln!(
            out,
            "/// Primary composites: `first << 42 | second << 21 | composite`, sorted.\npub static \
             COMPOSE: &[u64] = &["
        )
        .unwrap();
        items(&mut out, compose.iter().map(|v| format!("{v:#x}")));
        out.push_str("];\n\n");
        sizes.push(("COMPOSE", compose.len() * 8));

        delta_table(
            &mut out,
            &mut sizes,
            "LOWER",
            "Simple_Lowercase_Mapping.",
            &lower,
        );
        delta_table(
            &mut out,
            &mut sizes,
            "UPPER",
            "Simple_Uppercase_Mapping.",
            &upper,
        );
        delta_table(
            &mut out,
            &mut sizes,
            "FOLD",
            "Simple case folding (CaseFolding.txt statuses C and S).",
            &self.fold_simple,
        );
        string_map(
            &mut out,
            &mut sizes,
            "FOLD_FULL",
            "Full case folding where it is not a single character (status F).",
            &self.fold_full,
        );

        range_table(
            &mut out,
            &mut sizes,
            "GRAPHEME",
            "Grapheme_Cluster_Break (`GCB_*`) | `EXT_PICT`; Hangul syllables (LV/LVT) excluded.",
            &self.grapheme,
        );
        range_table(
            &mut out,
            &mut sizes,
            "WORD",
            "Word_Break (`WB_*`).",
            &self.word,
        );

        (out, sizes)
    }
}

fn parse_unicode_data(file: &File) -> Result<BTreeMap<u32, Char>> {
    let mut chars = BTreeMap::new();
    for (line, fields) in file.records() {
        if fields.len() != 15 {
            return Err(file.error(line, "expected 15 fields"));
        }
        let cp = file.code_point(line, fields[0])?;
        let ccc: u8 = fields[3]
            .parse()
            .map_err(|_| file.error(line, "bad combining class"))?;
        let decomposition = match fields[5] {
            "" => None,
            d => {
                let (compat, mapping) = match d.strip_prefix('<') {
                    Some(rest) => (
                        true,
                        rest.split_once('>')
                            .ok_or_else(|| file.error(line, "bad decomposition tag"))?
                            .1,
                    ),
                    None => (false, d),
                };
                let mapping = file.code_points(line, mapping)?;
                if mapping.is_empty() {
                    return Err(file.error(line, "empty decomposition"));
                }
                Some((compat, mapping))
            }
        };
        let case = |field: &str| -> Result<Option<u32>> {
            Ok(match field {
                "" => None,
                f => Some(file.scalar(line, f)?),
            })
        };
        let c = Char {
            ccc,
            decomposition,
            upper: case(fields[12])?,
            lower: case(fields[13])?,
        };
        // `<..., First>` / `<..., Last>` pairs (CJK ideographs, Hangul syllables, private use…)
        // stand for ranges of characters that all have the defaults.
        if fields[1].ends_with(", First>") || fields[1].ends_with(", Last>") {
            if c.ccc != 0 || c.decomposition.is_some() || c.upper.is_some() || c.lower.is_some() {
                return Err(file.error(line, "range with non-default properties"));
            }
            continue;
        }
        if chars.insert(cp, c).is_some() {
            return Err(file.error(line, format!("{cp:04X} listed twice")));
        }
    }
    Ok(chars)
}

/// `Regional_Indicator` → `REGIONAL_INDICATOR`, `ALetter` → `ALETTER`.
fn constant(name: &str) -> String {
    name.to_ascii_uppercase()
}

/// Comma-separated items, wrapped at 100 columns with 4 spaces of indent.
fn items(out: &mut String, items: impl IntoIterator<Item = String>) {
    let mut line = String::from("   ");
    for item in items {
        if line.len() + 1 + item.len() + 1 > 100 {
            out.push_str(&line);
            out.push('\n');
            line = String::from("   ");
        }
        line.push(' ');
        line.push_str(&item);
        line.push(',');
    }
    if line.len() > 3 {
        out.push_str(&line);
        out.push('\n');
    }
}

/// `{NAME}_KEYS` (`start << 11 | (len - 1)`) and `{NAME}_VALUES`: runs of equal nonzero values.
fn range_table(
    out: &mut String,
    sizes: &mut Vec<(&'static str, usize)>,
    name: &'static str,
    doc: &str,
    values: &BTreeMap<u32, u8>,
) {
    let max_len = 1u32 << RANGE_LEN_BITS;
    let mut runs: Vec<(u32, u32, u8)> = Vec::new();
    for (&cp, &v) in values {
        if v == 0 {
            continue;
        }
        match runs.last_mut() {
            Some((start, len, value)) if *start + *len == cp && *value == v && *len < max_len => {
                *len += 1;
            }
            _ => runs.push((cp, 1, v)),
        }
    }
    writeln!(out, "/// {doc}\npub static {name}_KEYS: &[u32] = &[").unwrap();
    items(
        out,
        runs.iter()
            .map(|&(start, len, _)| format!("{:#x}", start << RANGE_LEN_BITS | (len - 1))),
    );
    writeln!(out, "];\npub static {name}_VALUES: &[u8] = &[").unwrap();
    items(out, runs.iter().map(|&(_, _, v)| v.to_string()));
    out.push_str("];\n\n");
    sizes.push((name, runs.len() * 5));
}

/// `{NAME}_KEYS` (`start << 11 | (count - 1) << 1 | stride_is_2`) and `{NAME}_DELTAS`: runs of
/// code points, consecutive or every other one, that all map by the same offset.
fn delta_table(
    out: &mut String,
    sizes: &mut Vec<(&'static str, usize)>,
    name: &'static str,
    doc: &str,
    map: &BTreeMap<u32, u32>,
) {
    let max_count = 1u32 << DELTA_COUNT_BITS;
    // (start, count, stride, delta)
    let mut runs: Vec<(u32, u32, u32, i64)> = Vec::new();
    for (&cp, &to) in map {
        let delta = i64::from(to) - i64::from(cp);
        if let Some((start, count, stride, d)) = runs.last_mut()
            && *d == delta
            && *count < max_count
        {
            let last = *start + (*count - 1) * *stride;
            if *count == 1 && (cp == last + 1 || cp == last + 2) {
                *stride = cp - last;
                *count = 2;
                continue;
            }
            if *count > 1 && cp == last + *stride {
                *count += 1;
                continue;
            }
        }
        runs.push((cp, 1, 1, delta));
    }
    writeln!(out, "/// {doc}\npub static {name}_KEYS: &[u32] = &[").unwrap();
    items(
        out,
        runs.iter().map(|&(start, count, stride, _)| {
            format!(
                "{:#x}",
                start << RANGE_LEN_BITS | (count - 1) << 1 | u32::from(stride == 2)
            )
        }),
    );
    writeln!(out, "];\npub static {name}_DELTAS: &[i32] = &[").unwrap();
    items(out, runs.iter().map(|&(_, _, _, d)| d.to_string()));
    out.push_str("];\n\n");
    sizes.push((name, runs.len() * 8));
}

/// A map from code points to strings:
///
/// - `{NAME}_KEYS`: runs of consecutive mapped code points, `start << 11 | (len - 1)`, sorted;
/// - `{NAME}_INDEX`: the entry number of each run's first code point;
/// - `{NAME}_OFFSETS`: `entries + 1` byte offsets into `{NAME}_POOL`, entry `i` being
///   `POOL[OFFSETS[i]..OFFSETS[i + 1]]`.
fn string_map(
    out: &mut String,
    sizes: &mut Vec<(&'static str, usize)>,
    name: &'static str,
    doc: &str,
    map: &BTreeMap<u32, Vec<u32>>,
) {
    let max_len = 1u32 << RANGE_LEN_BITS;
    let mut pool = String::new();
    let mut offsets = vec![0u16];
    // (start, len, first entry)
    let mut runs: Vec<(u32, u32, usize)> = Vec::new();
    for (i, (&cp, mapping)) in map.iter().enumerate() {
        pool.extend(mapping.iter().map(|&cp| char::from_u32(cp).unwrap()));
        offsets.push(u16::try_from(pool.len()).expect("string pool over 64 KiB"));
        match runs.last_mut() {
            Some((start, len, _)) if *start + *len == cp && *len < max_len => *len += 1,
            _ => runs.push((cp, 1, i)),
        }
    }
    writeln!(out, "/// {doc}\npub static {name}_KEYS: &[u32] = &[").unwrap();
    items(
        out,
        runs.iter()
            .map(|&(start, len, _)| format!("{:#x}", start << RANGE_LEN_BITS | (len - 1))),
    );
    writeln!(out, "];\npub static {name}_INDEX: &[u16] = &[").unwrap();
    items(out, runs.iter().map(|&(_, _, i)| i.to_string()));
    writeln!(out, "];\npub static {name}_OFFSETS: &[u16] = &[").unwrap();
    items(out, offsets.iter().map(u16::to_string));
    writeln!(out, "];\npub static {name}_POOL: &str = \"\\").unwrap();
    // Everything but ASCII letters and digits is escaped, so no whitespace is eaten by the line
    // continuations and no combining mark or bidi control lands in the source.
    let mut line = String::new();
    for c in pool.chars() {
        let escaped = if c.is_ascii_alphanumeric() {
            c.to_string()
        } else {
            format!("\\u{{{:x}}}", c as u32)
        };
        if line.len() + escaped.len() > 99 {
            writeln!(out, "{line}\\").unwrap();
            line.clear();
        }
        line.push_str(&escaped);
    }
    writeln!(out, "{line}\";\n").unwrap();
    sizes.push((name, runs.len() * 6 + offsets.len() * 2 + pool.len()));
}
