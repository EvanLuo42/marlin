//! Packages of in-memory files in a fresh database.

use std::collections::BTreeMap;
use std::sync::Arc;

use marlin_front_hir_def::{
    FileDiagnostics, FileId, FilePackage, FileText, ItemId, ItemTreeQuery, Language, LanguageInput,
    PackageData, PackageId, PackageInput, Path, PathSegment, Resolution, item_path,
};
use marlin_front_syntax::Dialect;
use marlin_lang_diag::{SourceMap, render_all};
use marlin_lang_query::{Cx, Db, Durability};

pub static TEST_LANGUAGE: Language = Language {
    name: "test",
    dialect: &Dialect::SUPERSET,
    implicit_usings: &["Marlin"],
    require_namespace: true,
};

pub struct Fixture {
    pub db: Db,
    pub files: BTreeMap<String, FileId>,
    pub packages: BTreeMap<String, PackageId>,
    names: Vec<String>,
}

/// `(package, dependencies, files)`, where each file is `(path, text)`.
pub type PackageSpec<'a> = (&'a str, &'a [&'a str], &'a [(&'a str, &'a str)]);

impl Fixture {
    pub fn new(specs: &[PackageSpec<'_>]) -> Self {
        let db = Db::new();
        let mut files = BTreeMap::new();
        let mut packages = BTreeMap::new();
        let mut names = Vec::new();
        for (i, (name, _, _)) in specs.iter().enumerate() {
            packages.insert(name.to_string(), PackageId(i as u32));
        }
        db.update(|u| {
            u.set::<LanguageInput>((), &TEST_LANGUAGE, Durability::High);
            for (name, deps, sources) in specs {
                let package = packages[*name];
                let mut ids = Vec::new();
                for (path, text) in *sources {
                    let file = FileId(names.len() as u32);
                    names.push(format!("{name}/{path}"));
                    files.insert(path.to_string(), file);
                    ids.push(file);
                    u.set::<FileText>(file, Arc::from(*text), Durability::Low);
                    u.set::<FilePackage>(file, package, Durability::Low);
                }
                let data = PackageData {
                    name: Arc::from(*name),
                    files: ids.into(),
                    dependencies: deps.iter().map(|d| packages[*d]).collect(),
                };
                u.set::<PackageInput>(package, Arc::new(data), Durability::Low);
            }
        });
        Self {
            db,
            files,
            packages,
            names,
        }
    }

    pub fn file(&self, path: &str) -> FileId {
        self.files[path]
    }

    pub fn package(&self, name: &str) -> PackageId {
        self.packages[name]
    }

    pub fn set_text(&self, path: &str, text: &str) {
        self.db
            .set::<FileText>(self.file(path), Arc::from(text), Durability::Low);
    }

    /// All diagnostics of all files, rendered.
    pub fn diagnostics(&self) -> String {
        let snapshot = self.db.snapshot();
        let mut all = Vec::new();
        for &file in self.files.values() {
            all.extend(
                snapshot
                    .get::<FileDiagnostics>(&file)
                    .unwrap()
                    .iter()
                    .cloned(),
            );
        }
        let texts: Vec<Arc<str>> = (0..self.names.len())
            .map(|i| snapshot.input::<FileText>(&FileId(i as u32)).unwrap())
            .collect();
        render_all(
            &all,
            &Sources {
                names: &self.names,
                texts,
            },
        )
    }
}

struct Sources<'a> {
    names: &'a [String],
    texts: Vec<Arc<str>>,
}

impl SourceMap for Sources<'_> {
    fn name(&self, file: FileId) -> String {
        self.names[file.0 as usize].clone()
    }
    fn text(&self, file: FileId) -> &str {
        &self.texts[file.0 as usize]
    }
}

/// The declaration whose full name is `full` (`Marlin.Post.Blur.Run`), searched in all files. A
/// name defined twice gives the first in package order, the one lookup sees.
pub fn find_item(cx: &Cx<'_>, fixture: &Fixture, full: &str) -> ItemId {
    let mut found = Vec::new();
    for &file in fixture.files.values() {
        let tree = cx.get::<ItemTreeQuery>(&file).unwrap();
        for (local, _) in tree.items.iter() {
            let item = ItemId { file, local };
            if item_path(cx, item).unwrap() == full {
                found.push(item);
            }
        }
    }
    found.sort();
    *found
        .first()
        .unwrap_or_else(|| panic!("no declaration {full}"))
}

/// `A.B<2>.C`: dots between segments, `<n>` for a segment's number of type arguments.
pub fn path(text: &str) -> Path {
    let (global, text) = match text.strip_prefix("global::") {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    Path {
        global,
        segments: text
            .split('.')
            .map(|s| match s.split_once('<') {
                Some((name, arity)) => PathSegment {
                    name: marlin_front_hir_def::Name::new(name),
                    arity: arity.trim_end_matches('>').parse().unwrap(),
                },
                None => PathSegment {
                    name: marlin_front_hir_def::Name::new(s),
                    arity: 0,
                },
            })
            .collect(),
    }
}

/// A resolution as text, with declarations by full name.
pub fn show(cx: &Cx<'_>, resolution: &Resolution) -> String {
    let items = |items: &[ItemId]| {
        items
            .iter()
            .map(|&i| item_path(cx, i).unwrap())
            .collect::<Vec<_>>()
            .join(", ")
    };
    match resolution {
        Resolution::Namespace(ns) => format!("namespace {ns}"),
        Resolution::Local(binding) => format!("local {binding:?}"),
        Resolution::Type(item) => format!("type {}", item_path(cx, *item).unwrap()),
        Resolution::TypeParam { owner, index } => {
            format!(
                "type parameter {index} of {}",
                item_path(cx, *owner).unwrap()
            )
        }
        Resolution::Values(values) => format!("values [{}]", items(values)),
        Resolution::Partial { base, resolved } => {
            format!("partial {} after {resolved} segment(s)", show(cx, base))
        }
        Resolution::Unresolved { segment } => format!("unresolved at segment {segment}"),
        Resolution::Inaccessible {
            segment,
            items: hidden,
        } => {
            format!("inaccessible at segment {segment} [{}]", items(hidden))
        }
        Resolution::Ambiguous {
            segment,
            candidates,
        } => format!(
            "ambiguous at segment {segment} [{}]",
            candidates
                .iter()
                .map(|c| show(cx, c))
                .collect::<Vec<_>>()
                .join("; ")
        ),
    }
}

const CORE: &str = r#"namespace Marlin;

public struct Vector<T, const int N> { public T x; }
public struct Light
{
    public float3 Color;
    Secret Hidden;
    private struct Secret { }
    public struct Nested { }
}
public float Dot(float3 a, float3 b) => 0;
public interface IMaterial { type Params : IParams; }
internal struct CoreOnly { }
public struct Func<T> { }
public struct Func<T, U> { }
"#;

const MATH: &str = r#"namespace Marlin.Math;

public const float Pi = 3.14;
public float Dot(float a, float b) => a * b;
public struct Light { }
"#;

const EXTRA: &str = r#"namespace Marlin.Extra;

public struct Light { }
"#;

const WAVE: &str = r#"namespace Marlin.Wave;

public float Sum(float x) => x;
public float Sum(int x) => x;
"#;

const BLUR: &str = r#"namespace Marlin.Post;

using Marlin.Wave;
using M = Marlin.Math;

public struct Blur<T>
{
    public T Run<U>(U u) => default;
    Marlin.Light Field;
}
public float Luma(float3 c) => Dot(c, c);
struct Rock : IMaterial { public type Params = RockParams; }
[GroupShared] float4[64] Tile;
extension(float3 c) { public float Luma => 0; }
"#;

const INNER: &str = r#"namespace Marlin.Post.Inner;

public struct Light { }
"#;

const GAME: &str = r#"namespace Game;

using Marlin.Math;
using Marlin.Extra;

struct UsesLight { }
"#;

const BAD: &str = r#"namespace Marlin.Post;
namespace Marlin.Again;

using Marlin.Missing;
using static Marlin.Math;

struct Blur<X> { }
struct Dup { int X; float X; void F() { } void F(int a) { } }
public float Prop { get; }
float Luma;
"#;

/// A core library package `Marlin` and a package `Post` that uses it, with name lookup cases
/// and deliberate errors (in `post/Bad.mshader` and `post/NoNamespace.mshader`).
pub fn world() -> Fixture {
    Fixture::new(&[
        (
            "Marlin",
            &[],
            &[
                ("core/Core.mshader", CORE),
                ("core/Math.mshader", MATH),
                ("core/Extra.mshader", EXTRA),
                ("core/Wave.mshader", WAVE),
            ],
        ),
        (
            "Post",
            &["Marlin"],
            &[
                ("post/Blur.mshader", BLUR),
                ("post/Inner.mshader", INNER),
                ("post/Game.mshader", GAME),
                ("post/Bad.mshader", BAD),
                ("post/NoNamespace.mshader", "struct Loose { }\n"),
            ],
        ),
    ])
}
