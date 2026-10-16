//! The Git backend: the `git` command line with machine-readable output (`--porcelain=v2 -z`),
//! so the user's git, configuration and credential helpers apply unchanged. Calls are batched:
//! one process per query, never one per file.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use crate::{Change, ChangeKind, Error, Lock, Result, Vcs, VcsKind};

pub struct Git {
    root: PathBuf,
}

fn command_line(args: &[&str]) -> String {
    format!("git {}", args.join(" "))
}

impl Git {
    /// The repository containing `dir`.
    pub fn open(dir: impl AsRef<Path>) -> Result<Self> {
        let out = run_in(dir.as_ref(), &["rev-parse", "--show-toplevel"], None, &[0])?;
        let root = String::from_utf8(out.stdout)
            .map_err(|_| Error::Parse {
                command: "git rev-parse --show-toplevel".into(),
                detail: "not UTF-8".into(),
            })?
            .trim()
            .to_owned();
        Ok(Self {
            root: PathBuf::from(root),
        })
    }

    fn run(&self, args: &[&str], stdin: Option<&[u8]>, ok: &[i32]) -> Result<Output> {
        run_in(&self.root, args, stdin, ok)
    }

    fn lfs(&self) -> Result<()> {
        match self.run(&["lfs", "version"], None, &[0]) {
            Ok(_) => Ok(()),
            Err(_) => Err(Error::Unsupported(
                "file locks need Git LFS (`git lfs install`)".into(),
            )),
        }
    }
}

/// Runs git in `dir`; exit codes outside `ok` are errors.
fn run_in(dir: &Path, args: &[&str], stdin: Option<&[u8]>, ok: &[i32]) -> Result<Output> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(dir)
        // Paths verbatim (no octal quoting) and no pager or prompts, whatever the user's config.
        .args(["-c", "core.quotepath=false", "--no-pager"])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let io_error = |source| Error::Io {
        command: command_line(args),
        source,
    };
    let mut child = command.spawn().map_err(io_error)?;
    if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
        pipe.write_all(input).map_err(io_error)?;
    }
    let out = child.wait_with_output().map_err(io_error)?;
    let code = out.status.code().unwrap_or(-1);
    if !ok.contains(&code) {
        return Err(Error::CommandFailed {
            command: command_line(args),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        });
    }
    Ok(out)
}

fn utf8(bytes: &[u8], command: &str) -> Result<String> {
    String::from_utf8(bytes.to_vec()).map_err(|_| Error::Parse {
        command: command.into(),
        detail: "not UTF-8".into(),
    })
}

/// NUL-separated records.
fn records(bytes: &[u8], command: &str) -> Result<Vec<String>> {
    bytes
        .split(|&b| b == 0)
        .filter(|r| !r.is_empty())
        .map(|r| utf8(r, command))
        .collect()
}

fn kind_of(code: char, from: Option<&str>) -> Option<ChangeKind> {
    Some(match code {
        'A' | 'C' => ChangeKind::Added,
        'M' => ChangeKind::Modified,
        'D' => ChangeKind::Deleted,
        'T' => ChangeKind::TypeChanged,
        'R' => ChangeKind::Renamed {
            from: from.unwrap_or_default().to_owned(),
        },
        _ => return None,
    })
}

/// Parses `git status --porcelain=v2 -z` output.
pub(crate) fn parse_status(out: &[u8]) -> Result<Vec<Change>> {
    const COMMAND: &str = "git status --porcelain=v2 -z";
    let parse_error = |detail: &str| Error::Parse {
        command: COMMAND.into(),
        detail: detail.into(),
    };
    let entries = records(out, COMMAND)?;
    let mut changes = Vec::new();
    let mut i = 0;
    while i < entries.len() {
        let entry = &entries[i];
        i += 1;
        let mut fields = entry.splitn(2, ' ');
        let tag = fields.next().unwrap_or_default();
        let rest = fields.next().unwrap_or_default();
        match tag {
            "?" => changes.push(Change {
                path: rest.to_owned(),
                kind: ChangeKind::Untracked,
                staged: false,
                submodule: false,
            }),
            "!" | "#" => {}
            "1" | "2" => {
                // 1 XY sub mH mI mW hH hI path
                // 2 XY sub mH mI mW hH hI Xscore path NUL origPath
                let n = if tag == "1" { 8 } else { 9 };
                let parts: Vec<&str> = rest.splitn(n, ' ').collect();
                if parts.len() != n {
                    return Err(parse_error(entry));
                }
                let xy: Vec<char> = parts[0].chars().collect();
                if xy.len() != 2 {
                    return Err(parse_error(entry));
                }
                let submodule = parts[1].starts_with('S');
                let path = parts[n - 1].to_owned();
                let from = if tag == "2" {
                    let from = entries.get(i).cloned().ok_or_else(|| parse_error(entry))?;
                    i += 1;
                    Some(from)
                } else {
                    None
                };
                for (code, staged) in [(xy[0], true), (xy[1], false)] {
                    if let Some(kind) = kind_of(code, from.as_deref()) {
                        changes.push(Change {
                            path: path.clone(),
                            kind,
                            staged,
                            submodule,
                        });
                    }
                }
            }
            "u" => {
                // u XY sub m1 m2 m3 mW h1 h2 h3 path
                let parts: Vec<&str> = rest.splitn(10, ' ').collect();
                let path = parts.last().ok_or_else(|| parse_error(entry))?;
                changes.push(Change {
                    path: (*path).to_owned(),
                    kind: ChangeKind::Conflicted,
                    staged: false,
                    submodule: parts.get(1).is_some_and(|s| s.starts_with('S')),
                });
            }
            _ => return Err(parse_error(entry)),
        }
    }
    Ok(changes)
}

impl Vcs for Git {
    fn kind(&self) -> VcsKind {
        VcsKind::Git
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn revision(&self) -> Result<String> {
        // Exit 128 before the first commit: no revision yet.
        let out = self.run(
            &["rev-parse", "--verify", "--quiet", "HEAD"],
            None,
            &[0, 1, 128],
        )?;
        Ok(utf8(&out.stdout, "git rev-parse HEAD")?.trim().to_owned())
    }

    fn status(&self, paths: Option<&[&str]>) -> Result<Vec<Change>> {
        let mut args = vec![
            "status",
            "--porcelain=v2",
            "-z",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ];
        if let Some(paths) = paths {
            args.push("--");
            args.extend(paths);
        }
        let out = self.run(&args, None, &[0])?;
        parse_status(&out.stdout)
    }

    fn tracked(&self, paths: &[&str]) -> Result<Vec<bool>> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        // One listing of the whole index, then lookups: `ls-files` takes paths only as arguments,
        // which a big batch would overflow.
        let out = self.run(&["ls-files", "-z", "--cached"], None, &[0])?;
        let listed: std::collections::HashSet<String> =
            records(&out.stdout, "git ls-files")?.into_iter().collect();
        Ok(paths.iter().map(|p| listed.contains(*p)).collect())
    }

    fn ignored(&self, paths: &[&str]) -> Result<Vec<bool>> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let mut input = Vec::new();
        for p in paths {
            input.extend_from_slice(p.as_bytes());
            input.push(0);
        }
        // Exit 1 when nothing is ignored. `--no-index` also answers for tracked paths.
        let out = self.run(
            &["check-ignore", "-z", "--stdin", "--no-index"],
            Some(&input),
            &[0, 1],
        )?;
        let ignored: std::collections::HashSet<String> = records(&out.stdout, "git check-ignore")?
            .into_iter()
            .collect();
        Ok(paths.iter().map(|p| ignored.contains(*p)).collect())
    }

    fn lock(&self, paths: &[&str]) -> Result<()> {
        self.lfs()?;
        for path in paths {
            self.run(&["lfs", "lock", path], None, &[0])?;
        }
        Ok(())
    }

    fn unlock(&self, paths: &[&str]) -> Result<()> {
        self.lfs()?;
        for path in paths {
            self.run(&["lfs", "unlock", path], None, &[0])?;
        }
        Ok(())
    }

    fn locks(&self) -> Result<Vec<Lock>> {
        self.lfs()?;
        let out = self.run(&["lfs", "locks", "--json"], None, &[0])?;
        let json: serde_json::Value =
            serde_json::from_slice(&out.stdout).map_err(|e| Error::Parse {
                command: "git lfs locks --json".into(),
                detail: e.to_string(),
            })?;
        Ok(json
            .as_array()
            .map(|locks| {
                locks
                    .iter()
                    .map(|l| Lock {
                        path: l["path"].as_str().unwrap_or_default().to_owned(),
                        owner: l["owner"]["name"].as_str().unwrap_or_default().to_owned(),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    fn install_pre_submit_hook(&self, command: &str) -> Result<()> {
        // `--git-path` honours core.hooksPath and worktrees.
        let out = self.run(&["rev-parse", "--git-path", "hooks"], None, &[0])?;
        let hooks = utf8(&out.stdout, "git rev-parse --git-path hooks")?;
        let hooks = self.root.join(hooks.trim());
        let io_error = |source| Error::Io {
            command: "writing the pre-commit hook".into(),
            source,
        };
        std::fs::create_dir_all(&hooks).map_err(io_error)?;
        let hook = hooks.join("pre-commit");
        // Git runs hooks with sh on every platform, Git for Windows included.
        let script = format!(
            "#!/bin/sh\n# Installed by marlin-vcs: asset validation before every commit.\ncd \"$(git rev-parse --show-toplevel)\" || exit 1\n{command}\n"
        );
        std::fs::write(&hook, script).map_err(io_error)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))
                .map_err(io_error)?;
        }
        Ok(())
    }
}
