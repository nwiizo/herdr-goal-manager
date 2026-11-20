use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

pub(crate) const SKIPPED: [&str; 3] = [".git", "target", "node_modules"];

/// Runs a read-only git command. Optional locks stay off, so reading the
/// status never holds the index lock a worker's own git command needs.
fn git(dir: &Path, args: &[&str]) -> std::io::Result<std::process::Output> {
    std::process::Command::new("git")
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .output()
}

pub(crate) fn is_git(dir: &Path) -> bool {
    git(dir, &["rev-parse", "--is-inside-work-tree"]).is_ok_and(|output| output.status.success())
}

pub(crate) fn digest(dir: &Path) -> Result<String> {
    anyhow::ensure!(dir.is_dir(), "{} is not a directory", dir.display());
    if is_git(dir) {
        git_digest(dir)
    } else {
        anyhow::ensure!(
            !dir.ancestors().any(|p| p.join(".git").exists()),
            "could not inspect the Git work tree at {}",
            dir.display()
        );
        files_digest(dir)
    }
}

fn git_digest(dir: &Path) -> Result<String> {
    use std::os::unix::ffi::OsStrExt;
    let checked = |args: &[&str]| -> Result<Vec<u8>> {
        let output = git(dir, args)?;
        anyhow::ensure!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(output.stdout)
    };
    let mut hasher = Sha256::new();
    let head = git(dir, &["rev-parse", "--verify", "--quiet", "HEAD"])?;
    let status = checked(&["status", "--porcelain=v1", "-uall"])?;
    if head.status.success() {
        hasher.update(head.stdout);
        hasher.update(status);
        hasher.update(checked(&["diff", "HEAD", "--binary"])?);
    } else {
        anyhow::ensure!(head.status.code() == Some(1), "could not resolve Git HEAD");
        hasher.update(checked(&["symbolic-ref", "HEAD"])?);
        hasher.update(status);
        hasher.update(checked(&["diff", "--cached", "--binary"])?);
        hasher.update(checked(&["diff", "--binary"])?);
    }
    let untracked = checked(&["ls-files", "--others", "--exclude-standard", "-z"])?;
    for file in untracked.split(|b| *b == 0).filter(|f| !f.is_empty()) {
        let path = dir.join(std::ffi::OsStr::from_bytes(file));
        hasher.update(file);
        if path.symlink_metadata()?.is_symlink() {
            hasher.update(b"\0symlink\0");
            hasher.update(std::fs::read_link(&path)?.as_os_str().as_bytes());
        } else {
            hasher.update(
                std::fs::read(&path)
                    .with_context(|| format!("read untracked file {}", path.display()))?,
            );
        }
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn files_digest(dir: &Path) -> Result<String> {
    let mut stamps = vec![];
    stamp_files(dir, dir, &mut stamps)?;
    stamps.sort();
    let mut hasher = Sha256::new();
    for (path, stamp) in stamps {
        hasher.update(path.as_bytes());
        hasher.update([0]);
        hasher.update(stamp.as_bytes());
        hasher.update([0]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub(crate) fn stamp_files(root: &Path, path: &Path, out: &mut Vec<(String, String)>) -> Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).with_context(|| format!("inspect {}", path.display())),
    };
    let relative = || {
        path.strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned()
    };
    if metadata.is_dir() {
        let skipped = path != root
            && path
                .file_name()
                .is_some_and(|name| SKIPPED.iter().any(|s| name == *s));
        if skipped {
            return Ok(());
        }
        for entry in std::fs::read_dir(path).with_context(|| format!("list {}", path.display()))? {
            stamp_files(root, &entry?.path(), out)?;
        }
    } else if metadata.is_symlink() {
        let target = std::fs::read_link(path).unwrap_or_default();
        out.push((relative(), format!("link:{}", target.display())));
    } else if metadata.is_file() {
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |since| since.as_nanos());
        out.push((relative(), format!("{}:{modified}", metadata.len())));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Snapshot {
    pub git: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    pub files: std::collections::BTreeMap<String, String>,
}
const DELETED: &str = "deleted";
impl Snapshot {
    pub(crate) fn take(dir: &Path) -> Result<Self> {
        anyhow::ensure!(dir.is_dir(), "{} is not a directory", dir.display());
        if !is_git(dir) {
            let mut stamps = vec![];
            stamp_files(dir, dir, &mut stamps)?;
            return Ok(Self {
                git: false,
                head: None,
                files: stamps.into_iter().collect(),
            });
        }
        let head = git(dir, &["rev-parse", "HEAD"])
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string());
        let status = git(dir, &["status", "--porcelain=v1", "-z", "-uall"])?;
        anyhow::ensure!(
            status.status.success(),
            "git status failed in {}",
            dir.display()
        );
        let mut paths = vec![];
        let mut entries = status.stdout.split(|b| *b == 0).filter(|e| !e.is_empty());
        while let Some(entry) = entries.next() {
            let entry = String::from_utf8_lossy(entry);
            let (code, path) = entry.split_at(entry.len().min(3));
            paths.push(path.to_string());
            if code.contains(['R', 'C'])
                && let Some(from) = entries.next()
            {
                paths.push(String::from_utf8_lossy(from).into_owned());
            }
        }
        let mut files = std::collections::BTreeMap::new();
        for path in paths {
            let mut stamps = vec![];
            stamp_files(dir, &dir.join(&path), &mut stamps)?;
            if stamps.is_empty() {
                files.insert(path, DELETED.to_string());
            }
            files.extend(stamps);
        }
        Ok(Self {
            git: true,
            head,
            files,
        })
    }

    pub(crate) fn changed_since(&self, earlier: &Self, dir: &Path) -> Vec<String> {
        let mut changed: std::collections::BTreeSet<String> = self
            .files
            .iter()
            .filter(|(path, stamp)| earlier.files.get(*path) != Some(*stamp))
            .map(|(path, _)| path.clone())
            .chain(
                earlier
                    .files
                    .keys()
                    .filter(|path| !self.files.contains_key(*path))
                    .cloned(),
            )
            .collect();
        if self.git
            && let (Some(old), Some(new)) = (&earlier.head, &self.head)
            && old != new
            && let Ok(output) = git(dir, &["diff", "--name-only", "-z", old, new])
        {
            changed.extend(
                output
                    .stdout
                    .split(|b| *b == 0)
                    .filter(|p| !p.is_empty())
                    .map(|p| String::from_utf8_lossy(p).into_owned()),
            );
        }
        changed.into_iter().collect()
    }

    pub(crate) fn last_change(&self, dir: &Path) -> Option<i64> {
        let modified = |path: &Path| {
            std::fs::symlink_metadata(path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
        };
        let files = self.files.iter().filter_map(|(path, stamp)| {
            if stamp == DELETED {
                modified(dir.join(path).parent()?)
            } else {
                let nanos: u128 = stamp.split_once(':')?.1.parse().ok()?;
                Some((nanos / 1_000_000_000) as i64)
            }
        });
        let head = self
            .git
            .then(|| git(dir, &["rev-parse", "--git-path", "logs/HEAD"]).ok())
            .flatten()
            .filter(|output| output.status.success())
            .and_then(|output| {
                let log = String::from_utf8_lossy(&output.stdout).trim().to_string();
                modified(&dir.join(log))
            });
        files.chain(head).chain(modified(dir)).max()
    }
}

#[derive(Default)]
pub(crate) struct Fingerprints(HashMap<PathBuf, Option<String>>);
impl Fingerprints {
    pub(crate) fn of(&mut self, dir: &Path) -> Option<String> {
        self.0
            .entry(dir.to_path_buf())
            .or_insert_with(|| digest(dir).ok())
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_outside_git_changes_with_its_files_but_not_with_build_output() -> Result<()> {
        let dir = tempfile::tempdir()?;
        std::fs::write(dir.path().join("a.txt"), "one")?;
        std::fs::create_dir_all(dir.path().join("target/debug"))?;
        std::fs::create_dir_all(dir.path().join("src/node_modules/x"))?;
        let before = digest(dir.path())?;
        std::fs::write(dir.path().join("target/debug/out"), "built")?;
        std::fs::write(dir.path().join("src/node_modules/x/i.js"), "dep")?;
        assert_eq!(digest(dir.path())?, before, "build output is skipped");
        std::fs::write(dir.path().join("a.txt"), "two!")?;
        let changed = digest(dir.path())?;
        assert_ne!(changed, before);
        std::fs::write(dir.path().join("src/new.rs"), "")?;
        assert_ne!(digest(dir.path())?, changed, "a new file counts");
        Ok(())
    }

    #[test]
    fn a_repository_changes_with_edits_and_untracked_files_but_not_ignored_ones() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let run = |args: &[&str]| {
            let output = git(dir.path(), args).unwrap();
            assert!(output.status.success(), "git {args:?}");
        };
        run(&["init", "-q"]);
        std::fs::write(dir.path().join(".gitignore"), "target/\n")?;
        std::fs::write(dir.path().join("a.txt"), "one")?;
        run(&["add", "."]);
        let staged = digest(dir.path())?;
        std::fs::write(dir.path().join("a.txt"), "two")?;
        run(&["add", "a.txt"]);
        assert_ne!(digest(dir.path())?, staged);
        std::fs::write(dir.path().join("a.txt"), "one")?;
        run(&["add", "a.txt"]);
        run(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "-qm",
            "init",
        ]);
        assert!(is_git(dir.path()));
        let clean = digest(dir.path())?;
        std::fs::create_dir(dir.path().join("target"))?;
        std::fs::write(dir.path().join("target/out"), "built")?;
        assert_eq!(digest(dir.path())?, clean, "ignored output is skipped");
        std::fs::write(dir.path().join("a.txt"), "two")?;
        let edited = digest(dir.path())?;
        assert_ne!(edited, clean);
        std::fs::write(dir.path().join("b.txt"), "new")?;
        assert_ne!(digest(dir.path())?, edited);
        let mut cache = Fingerprints::default();
        assert_eq!(cache.of(dir.path()), Some(digest(dir.path())?));
        assert_eq!(cache.of(&dir.path().join("missing")), None);
        Ok(())
    }
}
