//! `git status`: Staged-, Unstaged-, Konflikt-, Untracked- und Ignored-Listen.
//!
//! # Verantwortung
//! [`compute`] vergleicht `HEAD`-Baum, Index und Arbeitsverzeichnis:
//! - *staged*: `HEAD` ↔ Index,
//! - *unstaged*: Index ↔ Arbeitsverzeichnis (nur getrackte Pfade),
//! - *conflicts*: Index-Einträge mit Stage > 0,
//! - *untracked* / *ignored*: Lauf durch das Arbeitsverzeichnis mit
//!   `.gitignore`-Stapel ([`crate::ignores::Ignores`]).
//!
//! # Untracked-Modi
//! `normal` meldet ein komplett ungetracktes Verzeichnis als `dir/`
//! (wie `git status`), `all` jede Datei, `no` nichts. Ein verschachteltes
//! Repository (Verzeichnis mit `.git`) erscheint als `dir/` und wird nicht
//! betreten. Ignorierte Verzeichnisse werden nicht betreten.
//!
//! # Grenzen
//! Höchstens [`MAX_ENTRIES`] Untracked-/Ignored-Einträge und
//! [`MAX_VISITED`] besuchte Verzeichniseinträge; darüber hinaus wird mit
//! `incomplete` abgebrochen. Innerhalb eines komplett ungetrackten
//! Verzeichnisses werden ignorierte Dateien nicht einzeln aufgelistet.

use crate::graph::read_commit;
use crate::ignores::{Ignores, MAX_IGNORE_DEPTH};
use crate::index::{Index, read as read_index};
use crate::odb::Odb;
use crate::pathspec::Pathspec;
use crate::refs::{Head, Refs};
use crate::repo::Repo;
use crate::snapshot::{
    Change, SnapEntry, WorktreeCtx, changes, from_flat, from_index, index_mtime, rel_of, worktree,
};
use crate::tree::flatten;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

/// Höchstzahl gesammelter Untracked-/Ignored-Einträge.
pub const MAX_ENTRIES: usize = 100_000;

/// Höchstzahl besuchter Verzeichniseinträge.
pub const MAX_VISITED: usize = 500_000;

/// Anzeige von Untracked-Dateien.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Untracked {
    /// Keine.
    No,
    /// Ungetrackte Verzeichnisse als `dir/`.
    Normal,
    /// Jede Datei einzeln.
    All,
}

impl Untracked {
    /// Erlaubte Namen.
    pub const NAMES: &'static [&'static str] = &["normal", "all", "no"];

    /// Parst einen Namen.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "normal" => Some(Self::Normal),
            "all" => Some(Self::All),
            "no" => Some(Self::No),
            _ => None,
        }
    }
}

/// Optionen für [`compute`].
pub struct StatusOpts {
    /// Untracked-Modus.
    pub untracked: Untracked,
    /// Ignorierte Einträge sammeln.
    pub ignored: bool,
    /// Pfadfilter.
    pub spec: Pathspec,
    /// Frist.
    pub deadline: Instant,
}

/// Ergebnis von [`compute`].
#[derive(Debug)]
pub struct Status {
    /// Zustand von `HEAD`.
    pub head: Head,
    /// Laufende Operation (`merge`, `rebase`, `cherry-pick`, `revert`, `bisect`).
    pub operation: Option<&'static str>,
    /// `HEAD` ↔ Index.
    pub staged: Vec<Change>,
    /// Index ↔ Arbeitsverzeichnis.
    pub unstaged: Vec<Change>,
    /// Konfliktpfade samt Stages.
    pub conflicts: BTreeMap<Vec<u8>, Vec<u8>>,
    /// Ungetrackte Pfade (Verzeichnisse mit `/` am Ende).
    pub untracked: Vec<Vec<u8>>,
    /// Ignorierte Pfade.
    pub ignored: Vec<Vec<u8>>,
    /// Grund für einen vorzeitigen Abbruch des Laufs.
    pub incomplete: Option<&'static str>,
}

/// Erkennt eine laufende Operation anhand der Marker im Git-Verzeichnis.
#[must_use]
pub fn operation(repo: &Repo) -> Option<&'static str> {
    let exists = |rest: &str| {
        repo.git_path(rest)
            .ok()
            .is_some_and(|rel| repo.scope.lstat(&rel).is_ok())
    };
    if exists("rebase-merge") || exists("rebase-apply") {
        Some("rebase")
    } else if exists("MERGE_HEAD") {
        Some("merge")
    } else if exists("CHERRY_PICK_HEAD") {
        Some("cherry-pick")
    } else if exists("REVERT_HEAD") {
        Some("revert")
    } else if exists("BISECT_LOG") {
        Some("bisect")
    } else {
        None
    }
}

#[derive(Default)]
struct Found {
    untracked: Vec<Vec<u8>>,
    ignored: Vec<Vec<u8>>,
}

struct Walker<'a> {
    repo: &'a Repo,
    spec: &'a Pathspec,
    tracked: &'a BTreeSet<Vec<u8>>,
    ignores: Ignores,
    mode: Untracked,
    want_ignored: bool,
    visited: usize,
    deadline: Instant,
    stop: Option<&'static str>,
}

fn join(dir: &[u8], name: &[u8]) -> Vec<u8> {
    let mut path = dir.to_vec();
    if !path.is_empty() {
        path.push(b'/');
    }
    path.extend_from_slice(name);
    path
}

impl Walker<'_> {
    fn tracked_under(&self, dir: &[u8]) -> bool {
        let mut prefix = dir.to_vec();
        prefix.push(b'/');
        self.tracked
            .range(prefix.clone()..)
            .next()
            .is_some_and(|p| p.starts_with(&prefix))
    }

    fn has_nested_repo(&self, path: &[u8]) -> bool {
        let rel = rel_of(&join(path, b".git"));
        self.repo.scope.lstat(&rel).is_ok()
    }

    fn check_limits(&mut self, found: &Found) -> bool {
        if self.stop.is_some() {
            return false;
        }
        if self.visited > MAX_VISITED {
            self.stop = Some("visit_limit");
        } else if found.untracked.len() + found.ignored.len() > MAX_ENTRIES {
            self.stop = Some("entry_limit");
        } else if Instant::now() > self.deadline {
            self.stop = Some("timeout");
        }
        self.stop.is_none()
    }

    /// Läuft `dir`; mit `first_only` endet der Lauf beim ersten Untracked-Fund.
    fn walk(&mut self, dir: &[u8], found: &mut Found, first_only: bool, depth: usize) {
        if depth > MAX_IGNORE_DEPTH {
            self.stop = Some("depth_limit");
            return;
        }
        let rel = rel_of(dir);
        self.ignores.push(self.repo, dir, &rel);
        for entry in self.repo.list_dir(&rel) {
            self.visited += 1;
            if !self.check_limits(found) || (first_only && !found.untracked.is_empty()) {
                break;
            }
            if entry.name == ".git" {
                continue;
            }
            let path = join(dir, entry.name.as_bytes());
            if entry.is_dir {
                if !self.spec.may_contain(&path) {
                    continue;
                }
                self.directory(&path, found, first_only, depth);
            } else if self.spec.matches(&path) && !self.tracked.contains(&path) {
                if self.ignores.is_ignored(&path, false) {
                    if self.want_ignored {
                        found.ignored.push(path);
                    }
                } else {
                    found.untracked.push(path);
                }
            }
        }
        self.ignores.pop();
    }

    fn directory(&mut self, path: &[u8], found: &mut Found, first_only: bool, depth: usize) {
        if self.tracked.contains(path) {
            return; // Gitlink
        }
        let mut shown = path.to_vec();
        shown.push(b'/');
        let tracked_inside = self.tracked_under(path);
        if self.has_nested_repo(path) && !tracked_inside {
            if !self.ignores.is_ignored(path, true) {
                found.untracked.push(shown);
            }
            return;
        }
        if self.ignores.is_ignored(path, true) && !tracked_inside {
            if self.want_ignored {
                found.ignored.push(shown);
            }
            return;
        }
        if !tracked_inside && self.mode == Untracked::Normal && !first_only {
            let mut inner = Found::default();
            self.walk(path, &mut inner, true, depth + 1);
            if !inner.untracked.is_empty() {
                found.untracked.push(shown);
            }
        } else {
            self.walk(path, found, first_only, depth + 1);
        }
    }
}

/// Berechnet den Status.
///
/// # Errors
/// Meldung bei unlesbarem `HEAD`, Index, Baum oder Arbeitsverzeichnis.
pub fn compute(
    repo: &Repo,
    odb: &Odb<'_>,
    refs: &Refs<'_>,
    opts: &StatusOpts,
) -> Result<Status, String> {
    let head = refs.head()?;
    let head_snapshot = match head.oid() {
        Some(oid) => {
            let commit = read_commit(odb, &oid)?;
            from_flat(flatten(odb, &commit.tree, &opts.spec)?)
        }
        None => BTreeMap::new(),
    };
    let index: Index = read_index(repo)?.unwrap_or_default();
    let view = from_index(&index, &opts.spec);
    let mut head_snapshot = head_snapshot;
    for path in view.conflicts.keys() {
        head_snapshot.remove(path);
    }
    let staged = changes(&head_snapshot, &view.snapshot);
    let ctx = WorktreeCtx {
        repo,
        deadline: opts.deadline,
        index_mtime: index_mtime(repo),
    };
    let wt = worktree(
        &ctx,
        view.entries
            .iter()
            .map(|(path, entry)| (*path, Some(*entry))),
    )?;
    let old_for_wt: BTreeMap<Vec<u8>, SnapEntry> = view.snapshot.clone();
    let unstaged = changes(&old_for_wt, &wt);

    let mut result = Status {
        head,
        operation: operation(repo),
        staged,
        unstaged,
        conflicts: view.conflicts.clone(),
        untracked: Vec::new(),
        ignored: Vec::new(),
        incomplete: None,
    };
    if opts.untracked != Untracked::No || opts.ignored {
        let tracked: BTreeSet<Vec<u8>> = index.entries.iter().map(|e| e.path.clone()).collect();
        let mut walker = Walker {
            repo,
            spec: &opts.spec,
            tracked: &tracked,
            ignores: Ignores::new(repo),
            mode: if opts.untracked == Untracked::No {
                Untracked::Normal
            } else {
                opts.untracked
            },
            want_ignored: opts.ignored,
            visited: 0,
            deadline: opts.deadline,
            stop: None,
        };
        let mut found = Found::default();
        walker.walk(b"", &mut found, false, 0);
        found.untracked.sort();
        found.ignored.sort();
        if opts.untracked != Untracked::No {
            result.untracked = found.untracked;
        }
        result.ignored = found.ignored;
        result.incomplete = walker.stop;
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::ChangeKind;
    use crate::test_support::{TestRepo, TestResult};
    use std::time::Duration;

    fn opts(untracked: Untracked, ignored: bool) -> StatusOpts {
        StatusOpts {
            untracked,
            ignored,
            spec: Pathspec::all(),
            deadline: Instant::now() + Duration::from_secs(30),
        }
    }

    fn run(repo: &TestRepo, opts: &StatusOpts) -> TestResult<Status> {
        let opened = Repo::open(&repo.ws)?;
        let odb = Odb::new(&opened);
        let refs = Refs::new(&opened);
        Ok(compute(&opened, &odb, &refs, opts)?)
    }

    fn names(list: &[Vec<u8>]) -> Vec<String> {
        list.iter()
            .map(|p| String::from_utf8_lossy(p).into_owned())
            .collect()
    }

    fn summary(changes: &[Change]) -> Vec<(String, char)> {
        changes
            .iter()
            .map(|c| {
                (
                    String::from_utf8_lossy(&c.path).into_owned(),
                    c.kind.letter(),
                )
            })
            .collect()
    }

    const BASE: &[(&str, u32, &str)] = &[
        ("a.txt", 0o100_644, "one\n"),
        ("src/lib.rs", 0o100_644, "fn x() {}\n"),
        ("run.sh", 0o100_755, "#!/bin/sh\n"),
    ];

    #[test]
    fn clean_repository_is_clean() -> TestResult {
        let repo = TestRepo::new()?;
        repo.commit_files(BASE, &[], "init", 1)?;
        let status = run(&repo, &opts(Untracked::Normal, true))?;
        assert!(
            status.staged.is_empty() && status.unstaged.is_empty() && status.untracked.is_empty()
        );
        assert!(status.conflicts.is_empty() && status.ignored.is_empty());
        assert_eq!(status.incomplete, None);
        assert_eq!(status.operation, None);
        assert!(matches!(status.head, Head::Branch { ref name, .. } if name == "main"));
        Ok(())
    }

    #[test]
    fn staged_unstaged_and_untracked_changes() -> TestResult {
        let repo = TestRepo::new()?;
        repo.commit_files(BASE, &[], "init", 1)?;
        // staged: neue Datei im Index, geänderte Datei im Index, gelöschte Datei aus dem Index
        let staged_state: &[(&str, u32, &str)] = &[
            ("a.txt", 0o100_644, "one\nstaged\n"),
            ("src/lib.rs", 0o100_644, "fn x() {}\n"),
            ("new.txt", 0o100_644, "n\n"),
        ];
        repo.stage(staged_state)?;
        // unstaged: lib.rs im Arbeitsverzeichnis geändert, new.txt gelöscht, a.txt gleich dem Index
        repo.write("a.txt", b"one\nstaged\n")?;
        repo.write("src/lib.rs", b"fn x() { changed }\n")?;
        // untracked
        repo.write("scratch.txt", b"s")?;
        repo.write("newdir/deep/file.txt", b"d")?;
        let status = run(&repo, &opts(Untracked::Normal, false))?;
        assert_eq!(
            summary(&status.staged),
            vec![
                ("a.txt".into(), 'M'),
                ("new.txt".into(), 'A'),
                ("run.sh".into(), 'D')
            ]
        );
        assert_eq!(
            summary(&status.unstaged),
            vec![("new.txt".into(), 'D'), ("src/lib.rs".into(), 'M')]
        );
        assert_eq!(
            names(&status.untracked),
            vec!["newdir/", "run.sh", "scratch.txt"]
        );
        let all = run(&repo, &opts(Untracked::All, false))?;
        assert_eq!(
            names(&all.untracked),
            vec!["newdir/deep/file.txt", "run.sh", "scratch.txt"]
        );
        let none = run(&repo, &opts(Untracked::No, false))?;
        assert!(none.untracked.is_empty());
        Ok(())
    }

    #[test]
    fn mode_and_type_changes_in_the_worktree() -> TestResult {
        let repo = TestRepo::new()?;
        repo.commit_files(BASE, &[], "init", 1)?;
        std::fs::set_permissions(
            repo.ws.join("run.sh"),
            std::os::unix::fs::PermissionsExt::from_mode(0o644),
        )?;
        std::fs::remove_file(repo.ws.join("a.txt"))?;
        std::os::unix::fs::symlink("elsewhere", repo.ws.join("a.txt"))?;
        let status = run(&repo, &opts(Untracked::No, false))?;
        assert_eq!(
            summary(&status.unstaged),
            vec![("a.txt".into(), 'T'), ("run.sh".into(), 'M')]
        );
        assert_eq!(status.unstaged[1].kind, ChangeKind::Modified);
        Ok(())
    }

    #[test]
    fn ignore_rules_nested_repos_and_collapsing() -> TestResult {
        let repo = TestRepo::new()?;
        repo.commit_files(BASE, &[], "init", 1)?;
        repo.write(".gitignore", b"*.log\ntarget/\n!keep.log\n")?;
        repo.write("a.log", b"x")?;
        repo.write("keep.log", b"x")?;
        repo.write("target/debug/out", b"x")?;
        repo.write("src/trace.log", b"x")?;
        repo.write("src/new.rs", b"x")?;
        std::fs::create_dir_all(repo.ws.join("vendor/lib/.git"))?;
        repo.write("vendor/lib/x.rs", b"x")?;
        std::fs::create_dir_all(repo.ws.join("emptydir"))?;
        let status = run(&repo, &opts(Untracked::Normal, true))?;
        assert_eq!(
            names(&status.untracked),
            vec![".gitignore", "keep.log", "src/new.rs", "vendor/"]
        );
        assert_eq!(
            names(&status.ignored),
            vec!["a.log", "src/trace.log", "target/"]
        );
        // .git selbst taucht nie auf
        assert!(!names(&status.untracked).iter().any(|p| p.contains(".git/")));
        Ok(())
    }

    #[test]
    fn pathspec_limits_every_list() -> TestResult {
        let repo = TestRepo::new()?;
        repo.commit_files(BASE, &[], "init", 1)?;
        repo.write("a.txt", b"changed\n")?;
        repo.write("src/lib.rs", b"changed\n")?;
        repo.write("src/extra.rs", b"x")?;
        repo.write("other.txt", b"x")?;
        let mut options = opts(Untracked::Normal, false);
        options.spec = Pathspec::parse(&["src".to_owned()])?;
        let status = run(&repo, &options)?;
        assert_eq!(summary(&status.unstaged), vec![("src/lib.rs".into(), 'M')]);
        assert_eq!(names(&status.untracked), vec!["src/extra.rs"]);
        Ok(())
    }

    #[test]
    fn unborn_branch_detached_head_and_operation_markers() -> TestResult {
        let repo = TestRepo::new()?;
        repo.head_branch("main")?;
        repo.stage(&[("first.txt", 0o100_644, "x\n")])?;
        repo.checkout(&[("first.txt", 0o100_644, "x\n")])?;
        let status = run(&repo, &opts(Untracked::Normal, false))?;
        assert!(matches!(status.head, Head::Branch { oid: None, .. }));
        assert_eq!(summary(&status.staged), vec![("first.txt".into(), 'A')]);
        let commit = repo.commit_files(BASE, &[], "init", 1)?;
        repo.head_detached(commit)?;
        repo.write_git("MERGE_HEAD", &format!("{commit}\n"))?;
        let status = run(&repo, &opts(Untracked::No, false))?;
        assert!(matches!(status.head, Head::Detached(_)));
        assert_eq!(status.operation, Some("merge"));
        std::fs::create_dir_all(repo.ws.join(".git/rebase-merge"))?;
        assert_eq!(
            run(&repo, &opts(Untracked::No, false))?.operation,
            Some("rebase")
        );
        Ok(())
    }

    #[test]
    fn missing_index_means_everything_is_untracked_and_deleted_from_index() -> TestResult {
        let repo = TestRepo::new()?;
        repo.commit_files(BASE, &[], "init", 1)?;
        std::fs::remove_file(repo.ws.join(".git/index"))?;
        let status = run(&repo, &opts(Untracked::All, false))?;
        assert_eq!(status.staged.len(), 3);
        assert!(status.staged.iter().all(|c| c.kind == ChangeKind::Deleted));
        assert_eq!(
            names(&status.untracked),
            vec!["a.txt", "run.sh", "src/lib.rs"]
        );
        Ok(())
    }

    #[test]
    fn secret_files_are_hashed_but_never_read_out() -> TestResult {
        let repo = TestRepo::new()?;
        let files: &[(&str, u32, &str)] = &[(".env", 0o100_644, "TOKEN=old\n")];
        repo.commit_files(files, &[], "init", 1)?;
        repo.write(".env", b"TOKEN=new-secret\n")?;
        let status = run(&repo, &opts(Untracked::No, false))?;
        assert_eq!(summary(&status.unstaged), vec![(".env".into(), 'M')]);
        Ok(())
    }

    #[test]
    fn broken_index_is_an_error_and_limits_stop_the_walk() -> TestResult {
        let repo = TestRepo::new()?;
        repo.commit_files(BASE, &[], "init", 1)?;
        let mut options = opts(Untracked::All, false);
        options.deadline = Instant::now()
            .checked_sub(Duration::from_secs(1))
            .unwrap_or_else(Instant::now);
        assert!(run(&repo, &options).is_err());
        repo.write_git("index", "garbage")?;
        assert!(run(&repo, &opts(Untracked::No, false)).is_err());
        Ok(())
    }
}
