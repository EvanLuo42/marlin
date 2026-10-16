//! Version control for the asset pipeline (`docs/design/0003-asset-pipeline.md` §3).
//!
//! Tools ask the [`Vcs`] trait what is changed, what a submit would contain, what is tracked or
//! ignored, and lock binary files, without knowing which system the project uses. [`open`] finds
//! the system that owns a directory. Git is implemented (through the `git` command line, so the
//! user's own configuration applies); Perforce and Epic Lore are recognized and report
//! `Unsupported` until their backends exist.
//!
//! Paths are relative to the workspace root, UTF-8, with `/` separators.

mod git;

use std::path::{Path, PathBuf};

pub use git::Git;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum VcsKind {
    Git,
    Perforce,
    Lore,
}

/// How a path differs from the last revision.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    /// Moved from `from` (relative path).
    Renamed {
        from: String,
    },
    /// A file became a symlink or the like.
    TypeChanged,
    /// Not tracked and not ignored.
    Untracked,
    /// Unresolved merge conflict.
    Conflicted,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Change {
    pub path: String,
    pub kind: ChangeKind,
    /// Part of what the next submit contains (Git: in the index). Untracked and conflicted
    /// changes are never staged. A Git path can appear twice: staged and, changed again since,
    /// unstaged.
    pub staged: bool,
    /// The path is a submodule (Git) rather than a file.
    pub submodule: bool,
}

/// An exclusive lock on a file (Git LFS, Perforce exclusive checkout).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Lock {
    pub path: String,
    pub owner: String,
}

#[derive(Debug, marlin_error::Error)]
pub enum Error {
    #[error("no version control found at {0}")]
    NotFound(String),
    #[error("{0}")]
    Unsupported(String),
    #[error("`{command}` failed ({status}): {stderr}")]
    CommandFailed {
        command: String,
        status: String,
        stderr: String,
    },
    #[error("unexpected output from `{command}`: {detail}")]
    Parse { command: String, detail: String },
    #[error("running `{command}`")]
    Io {
        command: String,
        source: std::io::Error,
    },
}

pub type Result<T> = core::result::Result<T, Error>;

/// A version-controlled workspace.
pub trait Vcs: Send + Sync {
    fn kind(&self) -> VcsKind;

    /// The workspace root; every path is relative to it.
    fn root(&self) -> &Path;

    /// The checked-out revision: a commit hash (Git), a changelist number (Perforce). Empty
    /// before the first commit.
    fn revision(&self) -> Result<String>;

    /// Every difference from the revision, untracked files included; only under `paths` when
    /// given (files or directories).
    fn status(&self, paths: Option<&[&str]>) -> Result<Vec<Change>>;

    /// What a submit made now would contain (Git: the staged changes). Pre-submit validation
    /// checks exactly this set.
    fn submit_set(&self) -> Result<Vec<Change>> {
        Ok(self
            .status(None)?
            .into_iter()
            .filter(|c| c.staged)
            .collect())
    }

    /// For each path, whether the revision (or the submit set) tracks it.
    fn tracked(&self, paths: &[&str]) -> Result<Vec<bool>>;

    /// For each path, whether the ignore rules exclude it.
    fn ignored(&self, paths: &[&str]) -> Result<Vec<bool>>;

    fn lock(&self, paths: &[&str]) -> Result<()>;

    fn unlock(&self, paths: &[&str]) -> Result<()>;

    fn locks(&self) -> Result<Vec<Lock>>;

    /// Runs `command` (a shell command line, run from the root) before every submit; a non-zero
    /// exit blocks the submit. Replaces a hook installed before.
    fn install_pre_submit_hook(&self, command: &str) -> Result<()>;
}

/// The version control system that owns `path` (a file or directory), searching upward.
pub fn open(path: impl AsRef<Path>) -> Result<Box<dyn Vcs>> {
    let start = path.as_ref();
    let start = if start.is_file() {
        start.parent().unwrap_or(start)
    } else {
        start
    };
    let mut dir: Option<&Path> = Some(start);
    while let Some(d) = dir {
        // `.git` is a directory in a normal clone, a file in worktrees and submodules.
        if d.join(".git").exists() {
            return Ok(Box::new(Git::open(d)?));
        }
        if d.join(".lore").exists() {
            return Err(Error::Unsupported(format!(
                "{}: Epic Lore workspaces are not supported yet",
                d.display()
            )));
        }
        if p4_config_names().iter().any(|name| d.join(name).is_file()) {
            return Err(Error::Unsupported(format!(
                "{}: Perforce workspaces are not supported yet",
                d.display()
            )));
        }
        dir = d.parent();
    }
    Err(Error::NotFound(start.display().to_string()))
}

/// Names Perforce looks for to find its configuration (`P4CONFIG`, conventionally `.p4config`).
fn p4_config_names() -> Vec<PathBuf> {
    let mut names = vec![PathBuf::from(".p4config")];
    if let Ok(name) = std::env::var("P4CONFIG")
        && !name.is_empty()
    {
        names.push(PathBuf::from(name));
    }
    names
}
