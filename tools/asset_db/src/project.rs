//! A project: its `.mproject` file, the engine it is built with and its content's mounts
//! (`docs/design/0003-asset-pipeline.md` §4.5).
//!
//! ```text
//! MyGame/
//!   MyGame.mproject     [project] name = "MyGame", optionally engine = "<dir>"
//!   content/            the `Game` mount
//!   .marlin/            per machine: index/<mount>/, ddc/, build/<platform>.{manifest,mpak}
//! ```
//!
//! The engine is, in this order: the directory the file's `engine` names (relative to the
//! file); the nearest directory above the project that is an engine (a project inside the
//! engine's tree, as the samples are); the engine the running tool belongs to (a project on its
//! own, opened with an installed editor). An engine root is a directory with `content/engine/`.

use std::path::{Path, PathBuf};

use toml::de::DeTable;

use crate::{AssetDb, Content, ENGINE_MOUNT, GAME_MOUNT, Mount, Options, UpdateReport};

/// Extension of project files.
pub const PROJECT_EXT: &str = "mproject";

/// A project, as found by [`Project::open`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Project {
    /// `[project] name`.
    pub name: String,
    /// The `.mproject` file.
    pub file: PathBuf,
    /// The directory the file is in.
    pub dir: PathBuf,
    /// The engine's root directory.
    pub engine: PathBuf,
}

#[derive(Debug, marlin_error::Error)]
pub enum ProjectError {
    #[error("{path}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("{path}: no `.{PROJECT_EXT}` file here")]
    NoProjectFile { path: String },
    #[error("{path}: more than one `.{PROJECT_EXT}` file; name the one to open")]
    Ambiguous { path: String },
    #[error("{path}: {message}")]
    Invalid { path: String, message: String },
    #[error(
        "{path}: no engine: the file names none, no directory above it has `content/engine/`, \
         and neither does any above this tool"
    )]
    NoEngine { path: String },
    #[error("indexing the content")]
    Index(#[from] crate::Error),
}

/// Paths without Windows' `\\?\` prefix, which `canonicalize` adds.
fn plain(path: PathBuf) -> PathBuf {
    match path.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => path,
    }
}

fn is_engine(dir: &Path) -> bool {
    dir.join("content").join("engine").is_dir()
}

/// The nearest of `start` and the directories above it that is an engine root.
pub fn find_engine(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|d| is_engine(d))
        .map(Path::to_path_buf)
}

impl Project {
    /// The project of `path`: a `.mproject` file, or a directory with exactly one.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ProjectError> {
        Self::open_with(path, None)
    }

    /// [`open`](Self::open), with `engine` (a root with `content/engine/`) in place of the
    /// engine the file or the directories would give: for tools and tests that bring their own.
    pub fn open_with(path: impl AsRef<Path>, engine: Option<&Path>) -> Result<Self, ProjectError> {
        let path = path.as_ref();
        let io = |p: &Path| {
            let path = p.display().to_string();
            move |source| ProjectError::Io { path, source }
        };
        let file = if path.is_dir() {
            let mut found = Vec::new();
            for entry in std::fs::read_dir(path).map_err(io(path))? {
                let entry = entry.map_err(io(path))?.path();
                if entry.extension().is_some_and(|e| e == PROJECT_EXT) && entry.is_file() {
                    found.push(entry);
                }
            }
            match found.len() {
                0 => {
                    return Err(ProjectError::NoProjectFile {
                        path: path.display().to_string(),
                    });
                }
                1 => found.pop().unwrap(),
                _ => {
                    return Err(ProjectError::Ambiguous {
                        path: path.display().to_string(),
                    });
                }
            }
        } else {
            path.to_path_buf()
        };
        let file = plain(std::fs::canonicalize(&file).map_err(io(&file))?);
        let shown = file.display().to_string();
        let invalid = |message: String| ProjectError::Invalid {
            path: shown.clone(),
            message,
        };
        let text = std::fs::read_to_string(&file).map_err(io(&file))?;
        let (name, named) = parse(&text).map_err(invalid)?;
        let dir = file.parent().map(Path::to_path_buf).unwrap_or_default();
        let named = engine.map(|e| e.display().to_string()).or(named);
        let engine = match named {
            Some(rel) => {
                let named = dir.join(&rel);
                if !is_engine(&named) {
                    return Err(invalid(format!(
                        "`engine = \"{rel}\"` has no `content/engine/`"
                    )));
                }
                plain(std::fs::canonicalize(&named).map_err(io(&named))?)
            }
            None => dir
                .parent()
                .and_then(find_engine)
                .or_else(|| {
                    let exe = std::env::current_exe().ok()?;
                    find_engine(exe.parent()?)
                })
                .ok_or(ProjectError::NoEngine { path: shown })?,
        };
        Ok(Self {
            name,
            file,
            dir,
            engine,
        })
    }

    /// The `Game` mount's root.
    pub fn content_dir(&self) -> PathBuf {
        self.dir.join("content")
    }

    /// The `Engine` mount's root.
    pub fn engine_content_dir(&self) -> PathBuf {
        self.engine.join("content").join("engine")
    }

    /// What the tools keep for this project on this machine (never submitted).
    pub fn data_dir(&self) -> PathBuf {
        self.dir.join(".marlin")
    }

    /// The local derived data cache.
    pub fn ddc_dir(&self) -> PathBuf {
        self.data_dir().join("ddc")
    }

    /// Where builds are published: `<platform>.manifest` and `<platform>.mpak`.
    pub fn build_dir(&self) -> PathBuf {
        self.data_dir().join("build")
    }

    /// The mounts, bottom first, as (name, root).
    pub fn mounts(&self) -> [(&'static str, PathBuf); 2] {
        [
            (ENGINE_MOUNT, self.engine_content_dir()),
            (GAME_MOUNT, self.content_dir()),
        ]
    }

    /// Every mount's index under `.marlin/index/<mount>` (the engine may be installed read-only,
    /// and two projects may share it), so `options.index_dir` is ignored.
    fn mount_options(&self, mount: &str, options: &Options) -> Options {
        Options {
            index_dir: Some(self.data_dir().join("index").join(mount)),
            ..options.clone()
        }
    }

    /// The content, each mount's index opened from its snapshot and brought up to date.
    pub fn open_content(&self, options: &Options) -> Result<(Content, UpdateReport), ProjectError> {
        let mut mounts = Vec::new();
        let mut report = UpdateReport::default();
        for (name, root) in self.mounts() {
            self.require(&root)?;
            let (db, r) = AssetDb::open(root, self.mount_options(name, options))?;
            crate::content::add(&mut report, r);
            mounts.push(Mount {
                name: name.into(),
                db,
            });
        }
        Ok((Content::new(mounts), report))
    }

    /// The content indexed from scratch.
    pub fn build_content(&self, options: &Options) -> Result<Content, ProjectError> {
        let mut mounts = Vec::new();
        for (name, root) in self.mounts() {
            self.require(&root)?;
            let db = AssetDb::build(root, self.mount_options(name, options))?;
            mounts.push(Mount {
                name: name.into(),
                db,
            });
        }
        Ok(Content::new(mounts))
    }

    fn require(&self, root: &Path) -> Result<(), ProjectError> {
        if root.is_dir() {
            Ok(())
        } else {
            Err(ProjectError::Invalid {
                path: self.file.display().to_string(),
                message: format!("no content directory {}", root.display()),
            })
        }
    }
}

/// `[project] name` and `engine`; unknown keys are errors.
fn parse(text: &str) -> Result<(String, Option<String>), String> {
    let table = DeTable::parse(text).map_err(|e| e.to_string())?;
    let table = table.get_ref();
    let mut project = None;
    for (key, value) in table.iter() {
        match key.get_ref().as_ref() {
            "project" => {
                project = Some(
                    value
                        .get_ref()
                        .as_table()
                        .ok_or("`project` must be a table")?,
                )
            }
            other => return Err(format!("unknown table `{other}`")),
        }
    }
    let project = project.ok_or("missing the `[project]` table")?;
    let (mut name, mut engine) = (None, None);
    for (key, value) in project.iter() {
        let key = key.get_ref().as_ref();
        let string = || {
            value
                .get_ref()
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("`project.{key}` must be a string"))
        };
        match key {
            "name" => name = Some(string()?),
            "engine" => engine = Some(string()?),
            other => return Err(format!("unknown field `project.{other}`")),
        }
    }
    let name = name
        .filter(|n| !n.is_empty())
        .ok_or("missing `project.name`")?;
    Ok((name, engine))
}
