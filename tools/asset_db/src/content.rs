//! A project's content: asset roots mounted under names (`docs/design/0003-asset-pipeline.md`
//! §4.5).
//!
//! Each mount is an asset root with its own [`AssetDb`]: the engine's content (`Engine`) at the
//! bottom, the project's (`Game`) above it, plugins in between later. A mount sees itself and the
//! mounts below it: game content links into the engine's, never the other way round. Inside a
//! mount, paths are relative to its root as before; a link into another mount writes the path
//! qualified, `/Engine/shaders/package`, and every unit is named by its qualified path, so units
//! of different mounts never share an id.

use std::path::Path;

use marlin_asset::Uuid;

use crate::validate::{self, Outside};
use crate::{
    AssetDb, FileId, Issue, IssueKind, LinkInfo, ObjectId, ObjectInfo, Result, UpdateReport,
};

/// The engine's content (`<engine>/content/engine`).
pub const ENGINE_MOUNT: &str = "Engine";
/// The project's own content (`<project>/content`).
pub const GAME_MOUNT: &str = "Game";

/// One mounted asset root.
pub struct Mount {
    /// What qualified paths start with: `/<name>/…`.
    pub name: String,
    pub db: AssetDb,
}

/// A mount, by its place in the stack (0 is the bottom).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct MountId(pub usize);

/// A file of some mount.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct ContentFile {
    pub mount: MountId,
    pub file: FileId,
}

/// An object of some mount.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct ContentObject {
    pub mount: MountId,
    pub object: ObjectId,
}

/// A problem in one mount's content.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ContentIssue {
    pub mount: MountId,
    pub issue: Issue,
}

/// `/Engine/shaders/package` → `("Engine", "shaders/package")`.
pub fn split_qualified(path: &str) -> Option<(&str, &str)> {
    path.strip_prefix('/')?.split_once('/')
}

/// The mounted roots of a project, bottom first. See the module docs.
pub struct Content {
    mounts: Vec<Mount>,
}

impl Content {
    /// # Panics
    /// If two mounts share a name or a name is empty or has a `/`.
    pub fn new(mounts: Vec<Mount>) -> Self {
        for (i, m) in mounts.iter().enumerate() {
            assert!(
                !m.name.is_empty() && !m.name.contains('/'),
                "mount name `{}`",
                m.name
            );
            assert!(
                mounts[..i].iter().all(|o| o.name != m.name),
                "mount `{}` given twice",
                m.name
            );
        }
        Self { mounts }
    }

    /// Content with one mount, `Game`.
    pub fn single(db: AssetDb) -> Self {
        Self::new(vec![Mount {
            name: GAME_MOUNT.into(),
            db,
        }])
    }

    pub fn mounts(&self) -> &[Mount] {
        &self.mounts
    }

    pub fn mount_ids(&self) -> impl Iterator<Item = MountId> + use<> {
        (0..self.mounts.len()).map(MountId)
    }

    pub fn mount(&self, id: MountId) -> &Mount {
        &self.mounts[id.0]
    }

    pub fn db(&self, id: MountId) -> &AssetDb {
        &self.mounts[id.0].db
    }

    pub fn db_mut(&mut self, id: MountId) -> &mut AssetDb {
        &mut self.mounts[id.0].db
    }

    pub fn find_mount(&self, name: &str) -> Option<MountId> {
        self.mounts.iter().position(|m| m.name == name).map(MountId)
    }

    /// Brings every mount's index in line with the disk; the reports summed.
    pub fn update(&mut self) -> Result<UpdateReport> {
        let mut total = UpdateReport::default();
        for m in &mut self.mounts {
            add(&mut total, m.db.update()?);
        }
        Ok(total)
    }

    /// Saves every mount's index.
    pub fn save(&mut self) -> Result<()> {
        for m in &mut self.mounts {
            m.db.save()?;
        }
        Ok(())
    }

    /// A second, independent copy: every mount opened again from its snapshot and the disk.
    pub fn reopen(&self) -> Result<Self> {
        let mounts = self
            .mounts
            .iter()
            .map(|m| {
                let (db, _) = AssetDb::open(m.db.root(), m.db.options().clone())?;
                Ok(Mount {
                    name: m.name.clone(),
                    db,
                })
            })
            .collect::<Result<_>>()?;
        Ok(Self { mounts })
    }

    pub fn file_count(&self) -> usize {
        self.mounts.iter().map(|m| m.db.file_count()).sum()
    }

    pub fn object_count(&self) -> usize {
        self.mounts.iter().map(|m| m.db.object_count()).sum()
    }

    pub fn files(&self) -> impl Iterator<Item = ContentFile> + '_ {
        self.mount_ids().flat_map(move |mount| {
            self.db(mount)
                .files()
                .map(move |file| ContentFile { mount, file })
        })
    }

    pub fn objects(&self) -> impl Iterator<Item = ContentObject> + '_ {
        self.mount_ids().flat_map(move |mount| {
            self.db(mount)
                .objects()
                .map(move |object| ContentObject { mount, object })
        })
    }

    /// The file at `/<mount>/<path>` (with its extension).
    pub fn file(&self, qualified: &str) -> Option<ContentFile> {
        let (name, rest) = split_qualified(qualified)?;
        let mount = self.find_mount(name)?;
        let file = self.db(mount).file(rest)?;
        Some(ContentFile { mount, file })
    }

    /// The qualified asset path of a file: `/Engine/shaders/package` (no extension). Units are
    /// named by it.
    pub fn path(&self, f: ContentFile) -> String {
        format!(
            "/{}/{}",
            self.mount(f.mount).name,
            self.db(f.mount).asset_path(f.file)
        )
    }

    /// The qualified path of a file with its extension, for messages.
    pub fn file_path(&self, f: ContentFile) -> String {
        format!(
            "/{}/{}",
            self.mount(f.mount).name,
            self.db(f.mount).file_info(f.file).path
        )
    }

    /// The root directory on disk of a mount.
    pub fn root(&self, mount: MountId) -> &Path {
        self.db(mount).root()
    }

    /// The object with `uuid`, from the bottom mount up (an id given twice resolves to the
    /// lowest; [`validate`](Self::validate) reports it).
    pub fn object(&self, uuid: Uuid) -> Option<ContentObject> {
        self.mount_ids().find_map(|mount| {
            let object = self.db(mount).object(uuid)?;
            Some(ContentObject { mount, object })
        })
    }

    pub fn object_info(&self, o: ContentObject) -> ObjectInfo<'_> {
        self.db(o.mount).object_info(o.object)
    }

    /// The file an object is in.
    pub fn object_file(&self, o: ContentObject) -> ContentFile {
        ContentFile {
            mount: o.mount,
            file: self.object_info(o).file,
        }
    }

    /// `uuid` as seen from `mount`: in it, else in the mounts below, nearest first.
    pub fn visible_object(&self, mount: MountId, uuid: Uuid) -> Option<ContentObject> {
        (0..=mount.0).rev().map(MountId).find_map(|m| {
            let object = self.db(m).object(uuid)?;
            Some(ContentObject { mount: m, object })
        })
    }

    /// The target of a link of `from`: by UUID, or by path and name for hand-written ones; only
    /// in mounts `from` sees.
    pub fn link_target(&self, from: ContentObject, link: LinkInfo<'_>) -> Option<ContentObject> {
        match link.target {
            Some(uuid) => self.visible_object(from.mount, uuid),
            None => match self.qualified_mount(from.mount, link.path) {
                Some((mount, rest)) => {
                    let uuid = self.db(mount).resolve(rest, link.name)?;
                    self.visible_object(mount, uuid)
                }
                None => {
                    let object = self.db(from.mount).link_target(from.object, link)?;
                    Some(ContentObject {
                        mount: from.mount,
                        object,
                    })
                }
            },
        }
    }

    /// For a qualified `path` into a mount `from` sees: that mount and the rest of the path.
    fn qualified_mount<'p>(&self, from: MountId, path: &'p str) -> Option<(MountId, &'p str)> {
        let (name, rest) = split_qualified(path)?;
        let mount = self.find_mount(name).filter(|m| m.0 <= from.0)?;
        Some((mount, rest))
    }

    /// How links in `mount` refer to `target`: its path relative to `mount` when it is in it,
    /// else qualified; and its name.
    pub fn link_path(&self, mount: MountId, target: ContentObject) -> (String, String) {
        let info = self.object_info(target);
        let path = if target.mount == mount {
            self.db(mount).asset_path(info.file)
        } else {
            self.path(self.object_file(target))
        };
        (path, info.name.to_owned())
    }

    /// Link resolution for documents of `mount` (reading objects, writing links).
    pub fn links(&self, mount: MountId) -> MountLinks<'_> {
        MountLinks {
            content: self,
            mount,
        }
    }

    /// Problems in the content of every mount ([`AssetDb::validate`]), with links resolved
    /// across mounts, plus UUIDs that two mounts both use.
    pub fn validate(&mut self) -> Vec<ContentIssue> {
        let mut issues = Vec::new();
        for mount in self.mount_ids() {
            issues.extend(self.duplicates_below(mount));
            let (below, rest) = self.mounts.split_at_mut(mount.0);
            let outside = Below {
                mounts: below,
                own: &rest[0].name,
            };
            issues.extend(
                validate::run(&mut rest[0].db, &outside)
                    .into_iter()
                    .map(|issue| ContentIssue { mount, issue }),
            );
        }
        issues
    }

    /// Pre-submit validation of one mount ([`AssetDb::validate_submit`]), with links resolved
    /// across mounts.
    pub fn validate_submit(
        &mut self,
        mount: MountId,
        vcs: &dyn marlin_vcs::Vcs,
    ) -> marlin_vcs::Result<Vec<ContentIssue>> {
        let (below, rest) = self.mounts.split_at_mut(mount.0);
        let outside = Below {
            mounts: below,
            own: &rest[0].name,
        };
        Ok(validate::run_submit(&mut rest[0].db, vcs, &outside)?
            .into_iter()
            .map(|issue| ContentIssue { mount, issue })
            .collect())
    }

    /// Objects of `mount` whose UUID a mount below already uses.
    fn duplicates_below(&self, mount: MountId) -> Vec<ContentIssue> {
        let db = self.db(mount);
        let mut issues = Vec::new();
        for object in db.objects() {
            let info = db.object_info(object);
            let Some(first) = (0..mount.0).map(MountId).find_map(|m| {
                Some(ContentObject {
                    mount: m,
                    object: self.db(m).object(info.uuid)?,
                })
            }) else {
                continue;
            };
            let first_info = self.object_info(first);
            issues.push(ContentIssue {
                mount,
                issue: Issue {
                    file: info.file,
                    object: Some(object),
                    kind: IssueKind::DuplicateUuidBelow {
                        uuid: info.uuid,
                        first: format!(
                            "{}:{}",
                            self.file_path(self.object_file(first)),
                            first_info.name
                        ),
                    },
                },
            });
        }
        issues
    }

    /// One line for a terminal: `/<mount>/<path>: object: problem`.
    pub fn describe(&self, issue: &ContentIssue) -> String {
        format!(
            "/{}/{}",
            self.mount(issue.mount).name,
            issue.issue.describe(self.db(issue.mount))
        )
    }
}

/// The mounts below one being validated, for the links its own index does not resolve.
struct Below<'a> {
    mounts: &'a [Mount],
    own: &'a str,
}

impl Outside for Below<'_> {
    fn find(&self, link: LinkInfo<'_>) -> Option<(Uuid, String, String)> {
        let describe = |m: &Mount, object: ObjectId| {
            let info = m.db.object_info(object);
            let path = format!("/{}/{}", m.name, m.db.asset_path(info.file));
            (info.uuid, path, info.name.to_owned())
        };
        match link.target {
            Some(uuid) => self
                .mounts
                .iter()
                .rev()
                .find_map(|m| Some(describe(m, m.db.object(uuid)?))),
            None => {
                let (name, rest) = split_qualified(link.path)?;
                if name == self.own {
                    return None;
                }
                let m = self.mounts.iter().find(|m| m.name == name)?;
                let uuid = m.db.resolve(rest, link.name)?;
                Some(describe(m, m.db.object(uuid)?))
            }
        }
    }
}

/// [`marlin_masset::Links`] for the documents of one mount: relative paths within it, qualified
/// ones into the mounts below.
pub struct MountLinks<'a> {
    content: &'a Content,
    mount: MountId,
}

impl marlin_masset::Links for MountLinks<'_> {
    fn resolve(&self, path: &str, name: &str) -> Option<Uuid> {
        match self.content.qualified_mount(self.mount, path) {
            Some((mount, rest)) => self.content.db(mount).resolve(rest, name),
            None => self.content.db(self.mount).resolve(path, name),
        }
    }

    fn describe(&self, target: Uuid) -> Option<(String, String)> {
        let object = self.content.visible_object(self.mount, target)?;
        Some(self.content.link_path(self.mount, object))
    }
}

/// Adds `r` to `total` (the mode is the last one's).
pub(crate) fn add(total: &mut UpdateReport, r: UpdateReport) {
    total.mode = r.mode;
    total.checked += r.checked;
    total.read += r.read;
    total.added += r.added;
    total.changed += r.changed;
    total.removed += r.removed;
    total.skipped_names += r.skipped_names;
}
