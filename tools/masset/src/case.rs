//! Name conversion between Rust and source text.
//!
//! Text uses PascalCase for every name; Rust fields are snake_case. Only the Rust-to-text
//! direction is computed: reading looks names up in a table built from the Rust side, so the
//! conversion never has to guess word boundaries (`mip_0_size` and `mip0_size` would both read
//! back from `Mip0Size`). Acronyms are ordinary words: `hdr_texture` is `HdrTexture`.

/// `snake_case` to `PascalCase`: each `_`-separated word starts upper case. Names that are
/// already PascalCase (types, enum variants) come back unchanged.
pub fn to_pascal(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for word in name.split('_').filter(|w| !w.is_empty()) {
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    out
}

/// The last `::` segment of a reflected type path: the name a type has in source text.
pub fn short_type_name(path: &str) -> &str {
    path.rsplit("::").next().unwrap_or(path)
}
