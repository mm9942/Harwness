//! `.gitignore`-Auswertung für den Arbeitsverzeichnis-Lauf.
//!
//! # Verantwortung
//! [`Ignores`] hält einen Stapel von Mustern: `<git-dir>/info/exclude`
//! (zuunterst) und je betretenem Verzeichnis dessen `.gitignore`. Die
//! Musterlogik (Globs, `**`, Negation `!`, Verzeichnis-Muster mit `/`) stammt
//! aus der Crate `ignore` (`gitignore::Gitignore`); hier wird nur gelesen und
//! gestapelt: tiefere Dateien haben Vorrang vor flacheren, die letzte
//! passende Zeile gewinnt.
//!
//! # Härtung
//! `.gitignore`-Dateien werden ausschließlich über den symlinkfreien
//! [`Scope`] gelesen (ein Symlink als `.gitignore` wird ignoriert), sind auf
//! [`MAX_IGNORE_BYTES`] begrenzt und beeinflussen nur die Anzeige von
//! Untracked-/Ignored-Einträgen, nie, was gelesen wird.
//!
//! # Grenzen
//! Die globale Datei `core.excludesFile` (außerhalb des Repositories) wird
//! nicht gelesen.

use crate::repo::Repo;
use harw_tool_fsread::scope::RelPath;
use ignore::Match;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::ffi::OsStr;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// Höchstgröße einer `.gitignore`-Datei.
pub const MAX_IGNORE_BYTES: u64 = 1024 * 1024;

/// Höchsttiefe des Stapels.
pub const MAX_IGNORE_DEPTH: usize = 128;

/// Stapel der aktiven Ignore-Regeln.
pub struct Ignores {
    info: Option<Gitignore>,
    levels: Vec<(Vec<u8>, Option<Gitignore>)>,
}

fn fake_root(dir: &[u8]) -> PathBuf {
    let mut root = PathBuf::from("/");
    if !dir.is_empty() {
        root.push(OsStr::from_bytes(dir));
    }
    root
}

fn build(root: &Path, text: &str) -> Option<Gitignore> {
    let mut builder = GitignoreBuilder::new(root);
    for line in text.lines() {
        // Fehlerhafte Muster werden wie bei Git übersprungen.
        let _ = builder.add_line(None, line);
    }
    builder.build().ok()
}

impl Ignores {
    /// Legt den Stapel an und liest `<git-dir>/info/exclude`.
    #[must_use]
    pub fn new(repo: &Repo) -> Self {
        let info = repo
            .read_git_file("info/exclude", MAX_IGNORE_BYTES)
            .ok()
            .and_then(|bytes| build(&fake_root(b""), &String::from_utf8_lossy(&bytes)));
        Self {
            info,
            levels: Vec::new(),
        }
    }

    /// Betritt das Verzeichnis `dir` (Bytes, `""` = Wurzel) und liest dessen `.gitignore`.
    pub fn push(&mut self, repo: &Repo, dir: &[u8], rel_dir: &RelPath) {
        let rel = rel_dir.join(OsStr::new(".gitignore"));
        let level = repo.scope.open_read(&rel).ok().and_then(|file| {
            let mut bytes = Vec::new();
            file.take(MAX_IGNORE_BYTES).read_to_end(&mut bytes).ok()?;
            build(&fake_root(dir), &String::from_utf8_lossy(&bytes))
        });
        self.levels.push((dir.to_vec(), level));
    }

    /// Verlässt das zuletzt betretene Verzeichnis.
    pub fn pop(&mut self) {
        self.levels.pop();
    }

    /// Aktuelle Tiefe.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.levels.len()
    }

    /// Wird `path` (Bytes) ignoriert? `is_dir` für Verzeichnis-Muster (`build/`).
    #[must_use]
    pub fn is_ignored(&self, path: &[u8], is_dir: bool) -> bool {
        let mut full = PathBuf::from("/");
        full.push(OsStr::from_bytes(path));
        let applicable = self
            .levels
            .iter()
            .rev()
            .filter(|(dir, _)| {
                dir.is_empty() || (path.starts_with(dir) && path.get(dir.len()) == Some(&b'/'))
            })
            .filter_map(|(_, level)| level.as_ref());
        for level in applicable.chain(self.info.iter()) {
            match level.matched(&full, is_dir) {
                Match::Ignore(_) => return true,
                Match::Whitelist(_) => return false,
                Match::None => {}
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestRepo, TestResult};

    fn stack(repo: &TestRepo, dirs: &[&str]) -> TestResult<(Repo, Ignores)> {
        let opened = Repo::open(&repo.ws)?;
        let mut ignores = Ignores::new(&opened);
        let mut path = String::new();
        ignores.push(&opened, b"", &opened.scope.rel("")?);
        for dir in dirs {
            if !path.is_empty() {
                path.push('/');
            }
            path.push_str(dir);
            ignores.push(&opened, path.as_bytes(), &opened.scope.rel(&path)?);
        }
        Ok((opened, ignores))
    }

    #[test]
    fn root_patterns_negation_and_directory_rules() -> TestResult {
        let repo = TestRepo::new()?;
        repo.write(
            ".gitignore",
            b"*.log\n!keep.log\nbuild/\n/top.txt\n# comment\n\\#hash\n",
        )?;
        let (_opened, ignores) = stack(&repo, &[])?;
        assert!(ignores.is_ignored(b"a.log", false));
        assert!(ignores.is_ignored(b"sub/a.log", false));
        assert!(!ignores.is_ignored(b"keep.log", false));
        assert!(ignores.is_ignored(b"build", true));
        assert!(!ignores.is_ignored(b"build", false));
        assert!(ignores.is_ignored(b"top.txt", false));
        assert!(!ignores.is_ignored(b"sub/top.txt", false));
        assert!(ignores.is_ignored(b"#hash", false));
        assert!(!ignores.is_ignored(b"src/main.rs", false));
        Ok(())
    }

    #[test]
    fn deeper_files_override_and_info_exclude_applies() -> TestResult {
        let repo = TestRepo::new()?;
        repo.write(".gitignore", b"*.tmp\n")?;
        repo.write("sub/.gitignore", b"!special.tmp\nlocal.txt\n")?;
        repo.write_git("info/exclude", "excluded.dat\n")?;
        let (_opened, ignores) = stack(&repo, &["sub"])?;
        assert!(ignores.is_ignored(b"sub/other.tmp", false));
        assert!(!ignores.is_ignored(b"sub/special.tmp", false));
        assert!(ignores.is_ignored(b"sub/local.txt", false));
        assert!(!ignores.is_ignored(b"local.txt", false));
        assert!(ignores.is_ignored(b"excluded.dat", false));
        assert!(ignores.is_ignored(b"sub/excluded.dat", false));
        assert_eq!(ignores.depth(), 2);
        Ok(())
    }

    #[test]
    fn symlinked_and_oversized_ignore_files_are_not_used_and_pop_restores() -> TestResult {
        let repo = TestRepo::new()?;
        std::fs::write(repo.outside.join("evil"), "*\n")?;
        std::os::unix::fs::symlink(repo.outside.join("evil"), repo.ws.join(".gitignore"))?;
        let (_opened, mut ignores) = stack(&repo, &[])?;
        assert!(!ignores.is_ignored(b"anything", false));
        ignores.pop();
        assert_eq!(ignores.depth(), 0);
        assert!(!ignores.is_ignored(b"anything", false));
        Ok(())
    }
}
