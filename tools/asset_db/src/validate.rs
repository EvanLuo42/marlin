//! Content checks that need the whole project: run from the index alone, without opening files.

use marlin_asset::{SourceFile, Uuid};
use marlin_masset::SOURCE_SCHEME;

use crate::{AssetDb, FileId, ObjectId};

/// A problem in the content.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Issue {
    pub file: FileId,
    /// The object it is in; `None` for problems with the whole file.
    pub object: Option<ObjectId>,
    pub kind: IssueKind,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum IssueKind {
    /// The `.masset` file does not parse; nothing in it is indexed.
    ParseError { message: String },
    /// Another object already has this UUID (`first` is the one links resolve to).
    DuplicateUuid { uuid: Uuid, first: ObjectId },
    /// Link `link` of the object points at no known object.
    DanglingLink {
        link: usize,
        target: Option<Uuid>,
        path: String,
        name: String,
    },
    /// Link `link` resolves by UUID, but the path or name written beside it is out of date
    /// (the target moved or was renamed). Tools rewrite it.
    StaleLink {
        link: usize,
        written: (String, String),
        actual: (String, String),
    },
    /// Link `link` was written without a UUID; it resolves by path and name to `resolved`.
    /// Tools fill the UUID in.
    MissingUuid { link: usize, resolved: Uuid },
    /// A `source:` file link names a file that does not exist.
    MissingSourceFile { path: String },
    /// Pre-submit: link `link` points into `path`, which the revision does not track and the
    /// submit does not add. Everyone else would get a dangling link.
    UnsubmittedTarget { link: usize, path: String },
    /// Pre-submit: a source file the object uses is neither tracked nor in the submit.
    UnsubmittedSource { path: String },
    /// Pre-submit: link `link` points into `path`, which the submit deletes.
    DeletedTarget { link: usize, path: String },
}

impl IssueKind {
    /// Whether the content is wrong (rather than only out of date and fixable by tools).
    pub fn is_error(&self) -> bool {
        !matches!(self, Self::StaleLink { .. } | Self::MissingUuid { .. })
    }
}

impl Issue {
    /// One line for a terminal: `path: object: problem`.
    pub fn describe(&self, db: &AssetDb) -> String {
        let path = db.file_info(self.file).path;
        let object = self
            .object
            .map(|o| format!("{}: ", db.object_info(o).name))
            .unwrap_or_default();
        let problem = match &self.kind {
            IssueKind::ParseError { message } => format!("does not parse: {message}"),
            IssueKind::DuplicateUuid { uuid, first } => {
                let first = db.object_info(*first);
                format!(
                    "UUID {uuid} is also used by {}:{}",
                    db.file_info(first.file).path,
                    first.name
                )
            }
            IssueKind::DanglingLink {
                target, path, name, ..
            } => match target {
                Some(uuid) => format!("link to <{path}:{name} #{uuid}> points at no object"),
                None => format!("link to <{path}:{name}> points at no object"),
            },
            IssueKind::StaleLink {
                written, actual, ..
            } => format!(
                "link says <{}:{}> but its target is now <{}:{}>",
                written.0, written.1, actual.0, actual.1
            ),
            IssueKind::MissingUuid { resolved, .. } => {
                format!("link has no UUID (resolves to {resolved})")
            }
            IssueKind::MissingSourceFile { path } => format!("source file `{path}` does not exist"),
            IssueKind::UnsubmittedTarget { path, .. } => {
                format!("links into `{path}`, which is neither tracked nor in this submit")
            }
            IssueKind::UnsubmittedSource { path } => {
                format!("uses source file `{path}`, which is neither tracked nor in this submit")
            }
            IssueKind::DeletedTarget { path, .. } => {
                format!("links into `{path}`, which this submit deletes")
            }
        };
        format!("{path}: {object}{problem}")
    }
}

pub(crate) fn run(db: &mut AssetDb) -> Vec<Issue> {
    let mut issues = Vec::new();
    for (uuid, rows) in db.db.duplicate_rows() {
        let Some(first) = db.db.by_uuid.get(uuid).map(ObjectId) else {
            continue;
        };
        for row in rows {
            let object = ObjectId(row);
            issues.push(Issue {
                file: db.object_info(object).file,
                object: Some(object),
                kind: IssueKind::DuplicateUuid {
                    uuid: Uuid::from_u128(uuid),
                    first,
                },
            });
        }
    }
    let files: Vec<FileId> = db.files().collect();
    for file in files {
        if let Some(message) = db.file_info(file).parse_error {
            issues.push(Issue {
                file,
                object: None,
                kind: IssueKind::ParseError {
                    message: message.to_owned(),
                },
            });
        }
        let objects: Vec<ObjectId> = db.objects_in(file).collect();
        for object in objects {
            let own_path = db.asset_path(file);
            for (index, link) in db.links(object).enumerate() {
                let issue = |kind| Issue {
                    file,
                    object: Some(object),
                    kind,
                };
                let written_path = if link.path.is_empty() {
                    own_path.clone()
                } else {
                    link.path.to_owned()
                };
                match db.link_target(object, link) {
                    None => issues.push(issue(IssueKind::DanglingLink {
                        link: index,
                        target: link.target,
                        path: link.path.to_owned(),
                        name: link.name.to_owned(),
                    })),
                    Some(target) => {
                        let info = db.object_info(target);
                        if link.target.is_none() {
                            issues.push(issue(IssueKind::MissingUuid {
                                link: index,
                                resolved: info.uuid,
                            }));
                            continue;
                        }
                        let actual = (db.asset_path(info.file), info.name.to_owned());
                        if actual.0 != written_path || actual.1 != link.name {
                            issues.push(issue(IssueKind::StaleLink {
                                link: index,
                                written: (written_path, link.name.to_owned()),
                                actual,
                            }));
                        }
                    }
                }
            }
            for source in db.source_links(object) {
                if source.scheme == SOURCE_SCHEME && db.file(source.path).is_none() {
                    issues.push(Issue {
                        file,
                        object: Some(object),
                        kind: IssueKind::MissingSourceFile {
                            path: SourceFile::new(source.path).path,
                        },
                    });
                }
            }
        }
    }
    issues
}

/// `assets/` when the asset root is `<workspace>/assets`; empty when they are the same.
fn workspace_prefix(workspace: &std::path::Path, root: &std::path::Path) -> Option<String> {
    let workspace = std::fs::canonicalize(workspace).ok()?;
    let root = std::fs::canonicalize(root).ok()?;
    let rel = root.strip_prefix(&workspace).ok()?;
    let mut prefix = String::new();
    for part in rel.components() {
        prefix.push_str(part.as_os_str().to_str()?);
        prefix.push('/');
    }
    Some(prefix)
}

pub(crate) fn run_submit(
    db: &mut AssetDb,
    vcs: &dyn marlin_vcs::Vcs,
) -> marlin_vcs::Result<Vec<Issue>> {
    use std::collections::HashSet;

    use marlin_vcs::ChangeKind;

    let prefix = workspace_prefix(vcs.root(), db.root()).ok_or_else(|| {
        marlin_vcs::Error::NotFound(format!(
            "{} is not inside the workspace {}",
            db.root().display(),
            vcs.root().display()
        ))
    })?;
    let to_asset = |p: &str| p.strip_prefix(prefix.as_str()).map(str::to_owned);
    let mut submitted = HashSet::new();
    let mut deleted = HashSet::new();
    for change in vcs.submit_set()? {
        let Some(path) = to_asset(&change.path) else {
            continue;
        };
        match change.kind {
            ChangeKind::Deleted => {
                deleted.insert(path);
            }
            ChangeKind::Renamed { from } => {
                if let Some(from) = to_asset(&from) {
                    deleted.insert(from);
                }
                submitted.insert(path);
            }
            ChangeKind::Added | ChangeKind::Modified | ChangeKind::TypeChanged => {
                submitted.insert(path);
            }
            ChangeKind::Untracked | ChangeKind::Conflicted => {}
        }
    }

    // Errors in what is being submitted.
    let mut issues: Vec<Issue> = run(db)
        .into_iter()
        .filter(|i| i.kind.is_error() && submitted.contains(&db.file_info(i.file).path))
        .collect();

    // What submitted objects point at must be tracked already or be part of the submit.
    enum Target {
        Link(usize),
        Source,
    }
    let mut candidates: Vec<(FileId, ObjectId, Target, String)> = Vec::new();
    for path in &submitted {
        let Some(file) = db.file(path) else { continue };
        for object in db.objects_in(file) {
            for (index, link) in db.links(object).enumerate() {
                if let Some(target) = db.link_target(object, link) {
                    let target_path = db.file_info(db.object_info(target).file).path;
                    if !submitted.contains(&target_path) {
                        candidates.push((file, object, Target::Link(index), target_path));
                    }
                }
            }
            for source in db.source_links(object) {
                if source.scheme == SOURCE_SCHEME
                    && db.file(source.path).is_some()
                    && !submitted.contains(source.path)
                {
                    candidates.push((file, object, Target::Source, source.path.to_owned()));
                }
            }
        }
    }
    let vcs_paths: Vec<String> = candidates
        .iter()
        .map(|c| format!("{prefix}{}", c.3))
        .collect();
    let refs: Vec<&str> = vcs_paths.iter().map(String::as_str).collect();
    let tracked = vcs.tracked(&refs)?;
    for ((file, object, target, path), tracked) in candidates.into_iter().zip(tracked) {
        if tracked {
            continue;
        }
        let kind = match target {
            Target::Link(link) => IssueKind::UnsubmittedTarget { link, path },
            Target::Source => IssueKind::UnsubmittedSource { path },
        };
        issues.push(Issue {
            file,
            object: Some(object),
            kind,
        });
    }

    // Nothing that stays may point into what the submit deletes.
    if !deleted.is_empty() {
        let files: Vec<FileId> = db.files().collect();
        for file in files {
            let own = db.file_info(file).path;
            if deleted.contains(&own) {
                continue;
            }
            for object in db.objects_in(file) {
                for (index, link) in db.links(object).enumerate() {
                    if link.path.is_empty() {
                        continue;
                    }
                    let path = format!("{}.{}", link.path, crate::MASSET_EXT);
                    if deleted.contains(&path) {
                        issues.push(Issue {
                            file,
                            object: Some(object),
                            kind: IssueKind::DeletedTarget { link: index, path },
                        });
                    }
                }
            }
        }
    }
    Ok(issues)
}
