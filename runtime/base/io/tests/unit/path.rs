use marlin_io::{Path, PathBuf};

fn norm(s: &str) -> String {
    PathBuf::from(s).into_string()
}

#[test]
fn normalizes() {
    let cases = [
        ("", ""),
        (".", ""),
        ("a", "a"),
        ("a/", "a"),
        ("a//b", "a/b"),
        ("a/./b", "a/b"),
        ("a\\b\\c", "a/b/c"),
        ("a/b/..", "a"),
        ("a/..", ""),
        ("../a", "../a"),
        ("a/../../b", "../b"),
        ("../../a/..", "../.."),
        ("/", "/"),
        ("/..", "/"),
        ("/a/../../b", "/b"),
        ("//server/share/x", "//server/share/x"),
        ("\\\\server\\share", "//server/share"),
        ("C:\\Users\\x\\..\\y", "C:/Users/y"),
        ("C:/..", "C:/"),
        ("C:a/../..", "C:.."),
    ];
    for (input, expected) in cases {
        assert_eq!(norm(input), expected, "{input:?}");
        assert!(Path::new(expected).is_normalized(), "{expected:?}");
    }
}

#[test]
fn roots() {
    assert!(Path::new("/a").is_absolute());
    assert!(Path::new("C:/a").is_absolute());
    assert!(Path::new("C:\\a").is_absolute());
    assert!(Path::new("//s/x").is_absolute());
    assert!(!Path::new("C:a").is_absolute());
    assert!(!Path::new("a/b").is_absolute());
    assert!(!Path::new("").is_absolute());
}

#[test]
fn parts() {
    let p = Path::new("assets/textures/rock.albedo.dds");
    assert_eq!(p.file_name(), Some("rock.albedo.dds"));
    assert_eq!(p.file_stem(), Some("rock.albedo"));
    assert_eq!(p.extension(), Some("dds"));
    assert_eq!(p.parent().map(Path::as_str), Some("assets/textures"));
    assert_eq!(
        p.components().collect::<Vec<_>>(),
        ["assets", "textures", "rock.albedo.dds"]
    );

    assert_eq!(Path::new(".config").extension(), None);
    assert_eq!(Path::new(".config").file_stem(), Some(".config"));
    assert_eq!(Path::new("a/..").file_name(), None);
    assert_eq!(Path::new("a").parent().map(Path::as_str), Some(""));
    assert_eq!(Path::new("/a").parent().map(Path::as_str), Some("/"));
    assert_eq!(Path::new("C:/a").parent().map(Path::as_str), Some("C:/"));
    assert_eq!(Path::new("/").parent(), None);
    assert_eq!(Path::new("").parent(), None);
}

#[test]
fn join_push_pop() {
    assert_eq!(Path::new("a/b").join("../c").as_str(), "a/c");
    assert_eq!(Path::new("a").join("/abs").as_str(), "/abs");
    assert_eq!(Path::new("").join("x").as_str(), "x");
    assert_eq!(Path::new("/").join("x").as_str(), "/x");

    let mut p = PathBuf::from("/data");
    p.push("levels\\one");
    assert_eq!(p.as_str(), "/data/levels/one");
    assert!(p.pop());
    assert_eq!(p.as_str(), "/data/levels");
    assert!(p.pop());
    assert!(p.pop());
    assert_eq!(p.as_str(), "/");
    assert!(!p.pop());
}

#[test]
fn starts_with() {
    let p = Path::new("/data/levels/one");
    assert!(p.starts_with("/data"));
    assert!(p.starts_with("\\data\\levels"));
    assert!(!p.starts_with("/dat"));
    assert!(!p.starts_with("data"));
    assert!(Path::new("a/b").starts_with(""));
}
