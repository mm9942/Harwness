//! Referenzen: `HEAD`, lose Refs und `packed-refs`.
//!
//! # Verantwortung
//! - [`Refs::head`]: aktueller Branch oder abgekoppelter Commit,
//! - [`Refs::resolve`]: Referenzname → Objekt-ID (folgt symbolischen Refs bis
//!   zu [`MAX_SYMREF_DEPTH`] Stufen),
//! - [`Refs::list`]: alle Refs unter einem Präfix (lose überschreiben
//!   `packed-refs`; deterministisch nach Name sortiert).
//!
//! # Härtung
//! Referenznamen werden nach `git check-ref-format` validiert
//! ([`valid_ref_name`]): kein `..`, keine Steuerzeichen, kein `~^:?*[\\`.
//! Unbekannte Dateiinhalte, SHA-256-IDs und zyklische symbolische Refs sind
//! Fehler mit Meldung, nie Panics.

use crate::oid::{OID_HEX_LEN, Oid};
use crate::repo::Repo;
use std::collections::BTreeMap;

/// Höchstzahl gefolgter symbolischer Refs.
pub const MAX_SYMREF_DEPTH: usize = 5;

/// Höchstgröße von `packed-refs`.
pub const MAX_PACKED_REFS_BYTES: u64 = 64 * 1024 * 1024;

/// Höchstzahl gelisteter Refs.
pub const MAX_REFS: usize = 100_000;

/// Zustand von `HEAD`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Head {
    /// Auf einem Branch; `oid` ist `None` auf einem ungeborenen Branch.
    Branch {
        /// Kurzname (`main`).
        name: String,
        /// Spitze des Branches.
        oid: Option<Oid>,
    },
    /// Abgekoppelt auf einem Commit.
    Detached(Oid),
}

impl Head {
    /// Die Commit-ID, falls es schon einen Commit gibt.
    #[must_use]
    pub fn oid(&self) -> Option<Oid> {
        match self {
            Self::Branch { oid, .. } => *oid,
            Self::Detached(oid) => Some(*oid),
        }
    }
}

/// Prüft einen Referenznamen.
#[must_use]
pub fn valid_ref_name(name: &str) -> bool {
    if name.is_empty()
        || name.len() > 255
        || name.starts_with('/')
        || name.ends_with('/')
        || name.ends_with('.')
        || name.ends_with(".lock")
    {
        return false;
    }
    if name.contains("..") || name.contains("//") || name.contains("@{") || name == "@" {
        return false;
    }
    if name
        .chars()
        .any(|c| c.is_control() || matches!(c, ' ' | '~' | '^' | ':' | '?' | '*' | '[' | '\\'))
    {
        return false;
    }
    name.split('/')
        .all(|part| !part.is_empty() && !part.starts_with('.') && !part.ends_with(".lock"))
}

/// Zugriff auf die Referenzen eines Repositories.
pub struct Refs<'a> {
    repo: &'a Repo,
}

fn parse_oid_line(text: &str) -> Result<Oid, String> {
    let trimmed = text.trim();
    if trimmed.len() == 64 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("SHA-256 repositories are not supported".to_owned());
    }
    Oid::from_hex(trimmed).ok_or_else(|| {
        format!(
            "invalid object id in a reference file: '{}'",
            trimmed.chars().take(OID_HEX_LEN + 2).collect::<String>()
        )
    })
}

impl<'a> Refs<'a> {
    /// Neuer Zugriff.
    #[must_use]
    pub fn new(repo: &'a Repo) -> Self {
        Self { repo }
    }

    /// Liest `packed-refs` als Name → (ID, geschälte ID).
    fn packed(&self) -> BTreeMap<String, Oid> {
        let Ok(bytes) = self
            .repo
            .read_git_file("packed-refs", MAX_PACKED_REFS_BYTES)
        else {
            return BTreeMap::new();
        };
        let text = String::from_utf8_lossy(&bytes);
        let mut map = BTreeMap::new();
        for line in text.lines() {
            if line.starts_with('#') || line.starts_with('^') || line.trim().is_empty() {
                continue;
            }
            let Some((hex, name)) = line.split_once(' ') else {
                continue;
            };
            if let (Some(oid), true) = (Oid::from_hex(hex), valid_ref_name(name.trim())) {
                map.insert(name.trim().to_owned(), oid);
            }
            if map.len() >= MAX_REFS {
                break;
            }
        }
        map
    }

    /// Liest eine lose Ref-Datei: `Ok(None)`, wenn es sie nicht gibt.
    fn loose(&self, name: &str) -> Result<Option<LooseRef>, String> {
        let bytes = match self.repo.read_git_file(name, 4096) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            // Ein Verzeichnis (`refs/heads/feature` mit Unterref) ist keine Ref-Datei.
            Err(error) if matches!(error.raw_os_error(), Some(21)) => return Ok(None),
            Err(error) => return Err(format!("cannot read reference {name}: {error}")),
        };
        let text = String::from_utf8_lossy(&bytes);
        if let Some(target) = text.trim().strip_prefix("ref:") {
            return Ok(Some(LooseRef::Symbolic(target.trim().to_owned())));
        }
        Ok(Some(LooseRef::Direct(parse_oid_line(&text)?)))
    }

    /// Löst eine vollständige Referenz (`HEAD`, `refs/heads/main`) auf.
    ///
    /// # Errors
    /// Meldung bei ungültigem Namen, Zyklus oder unlesbarer Datei.
    pub fn resolve(&self, name: &str) -> Result<Option<Oid>, String> {
        let mut current = name.to_owned();
        for _ in 0..=MAX_SYMREF_DEPTH {
            if current != "HEAD" && !valid_ref_name(&current) {
                return Err(format!(
                    "invalid reference name '{}'",
                    current.chars().take(64).collect::<String>()
                ));
            }
            match self.loose(&current)? {
                Some(LooseRef::Direct(oid)) => return Ok(Some(oid)),
                Some(LooseRef::Symbolic(target)) => current = target,
                None => return Ok(self.packed().get(&current).copied()),
            }
        }
        Err(format!(
            "symbolic reference '{name}' is nested too deeply or cyclic"
        ))
    }

    /// Der Zustand von `HEAD`.
    ///
    /// # Errors
    /// Meldung, wenn `HEAD` fehlt oder unlesbar ist.
    pub fn head(&self) -> Result<Head, String> {
        let bytes = self
            .repo
            .read_git_file("HEAD", 4096)
            .map_err(|e| format!("cannot read HEAD: {e}"))?;
        let text = String::from_utf8_lossy(&bytes);
        if let Some(target) = text.trim().strip_prefix("ref:") {
            let target = target.trim();
            let name = target
                .strip_prefix("refs/heads/")
                .unwrap_or(target)
                .to_owned();
            let oid = self.resolve(target)?;
            return Ok(Head::Branch { name, oid });
        }
        Ok(Head::Detached(parse_oid_line(&text)?))
    }

    /// Listet alle Refs, deren Name mit `prefix` beginnt (`refs/heads/`).
    #[must_use]
    pub fn list(&self, prefix: &str) -> Vec<(String, Oid)> {
        let mut map = self.packed();
        map.retain(|name, _| name.starts_with(prefix));
        let base = prefix.trim_end_matches('/');
        self.collect_loose(base, &mut map, 0);
        let mut out: Vec<(String, Oid)> = map
            .into_iter()
            .filter(|(name, _)| name.starts_with(prefix))
            .collect();
        out.truncate(MAX_REFS);
        out
    }

    fn collect_loose(&self, dir: &str, map: &mut BTreeMap<String, Oid>, depth: usize) {
        if depth > 16 || map.len() >= MAX_REFS {
            return;
        }
        for entry in self.repo.list_git_dir(dir) {
            let name = format!("{dir}/{}", entry.name);
            if entry.is_dir {
                self.collect_loose(&name, map, depth + 1);
            } else if valid_ref_name(&name) {
                if let Ok(Some(LooseRef::Direct(oid))) = self.loose(&name) {
                    map.insert(name, oid);
                } else if let Ok(Some(LooseRef::Symbolic(target))) = self.loose(&name) {
                    // z. B. `refs/remotes/origin/HEAD`: auf das Ziel auflösen.
                    if let Ok(Some(oid)) = self.resolve(&target) {
                        map.insert(name, oid);
                    }
                }
            }
        }
    }
}

enum LooseRef {
    Direct(Oid),
    Symbolic(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestRepo, TestResult};

    const A: &str = "1111111111111111111111111111111111111111";
    const B: &str = "2222222222222222222222222222222222222222";

    fn oid(hex: &str) -> TestResult<Oid> {
        Oid::from_hex(hex).ok_or(TestError::Missing("oid"))
    }

    #[test]
    fn ref_name_validation_follows_check_ref_format() -> TestResult {
        for good in [
            "HEAD2",
            "refs/heads/main",
            "refs/heads/feature/x-1",
            "refs/tags/v1.0.0",
            "a/b",
        ] {
            assert!(valid_ref_name(good), "{good}");
        }
        for bad in [
            "",
            "/a",
            "a/",
            "a//b",
            "a..b",
            "a b",
            "a~1",
            "a^",
            "a:b",
            "a?",
            "a*",
            "a[",
            "a\\b",
            "refs/heads/.hidden",
            "a.lock",
            "refs/heads/x.lock/y",
            "@",
            "a@{1}",
            "a\u{0}b",
            "a\nb",
            "a.",
            &"a".repeat(256),
        ] {
            assert!(!valid_ref_name(bad), "{bad:?}");
        }
        Ok(())
    }

    #[test]
    fn head_branch_unborn_and_detached() -> TestResult {
        let repo = TestRepo::new()?;
        repo.head_branch("main")?;
        let opened = Repo::open(&repo.ws)?;
        let refs = Refs::new(&opened);
        assert_eq!(
            refs.head().map_err(TestError::Unexpected)?,
            Head::Branch {
                name: "main".into(),
                oid: None
            }
        );
        repo.write_git("refs/heads/main", &format!("{A}\n"))?;
        let head = refs.head().map_err(TestError::Unexpected)?;
        assert_eq!(
            head,
            Head::Branch {
                name: "main".into(),
                oid: Some(oid(A)?)
            }
        );
        assert_eq!(head.oid(), Some(oid(A)?));
        repo.write_git("HEAD", &format!("{B}\n"))?;
        assert_eq!(
            refs.head().map_err(TestError::Unexpected)?,
            Head::Detached(oid(B)?)
        );
        repo.write_git("HEAD", "garbage")?;
        assert!(refs.head().is_err());
        repo.write_git("HEAD", &"a".repeat(64))?;
        assert!(refs.head().err().is_some_and(|e| e.contains("SHA-256")));
        Ok(())
    }

    #[test]
    fn loose_overrides_packed_and_listing_is_sorted() -> TestResult {
        let repo = TestRepo::new()?;
        repo.write_git("packed-refs", &format!("# pack-refs with: peeled fully-peeled sorted \n{A} refs/heads/main\n{A} refs/heads/old\n{B} refs/tags/v1\n^{A}\n"))?;
        repo.write_git("refs/heads/main", &format!("{B}\n"))?;
        repo.write_git("refs/heads/feature/x", &format!("{A}\n"))?;
        let opened = Repo::open(&repo.ws)?;
        let refs = Refs::new(&opened);
        assert_eq!(
            refs.resolve("refs/heads/main")
                .map_err(TestError::Unexpected)?,
            Some(oid(B)?)
        );
        assert_eq!(
            refs.resolve("refs/heads/old")
                .map_err(TestError::Unexpected)?,
            Some(oid(A)?)
        );
        assert_eq!(
            refs.resolve("refs/tags/v1")
                .map_err(TestError::Unexpected)?,
            Some(oid(B)?)
        );
        assert_eq!(
            refs.resolve("refs/heads/none")
                .map_err(TestError::Unexpected)?,
            None
        );
        let names: Vec<String> = refs
            .list("refs/heads/")
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(
            names,
            vec!["refs/heads/feature/x", "refs/heads/main", "refs/heads/old"]
        );
        assert_eq!(refs.list("refs/tags/").len(), 1);
        assert!(refs.list("refs/remotes/").is_empty());
        Ok(())
    }

    #[test]
    fn symbolic_refs_cycles_and_bad_names() -> TestResult {
        let repo = TestRepo::new()?;
        repo.write_git("refs/heads/a", "ref: refs/heads/b\n")?;
        repo.write_git("refs/heads/b", "ref: refs/heads/a\n")?;
        repo.write_git(
            "refs/remotes/origin/HEAD",
            "ref: refs/remotes/origin/main\n",
        )?;
        repo.write_git("refs/remotes/origin/main", &format!("{A}\n"))?;
        let opened = Repo::open(&repo.ws)?;
        let refs = Refs::new(&opened);
        assert!(
            refs.resolve("refs/heads/a")
                .err()
                .is_some_and(|e| e.contains("cyclic"))
        );
        assert_eq!(
            refs.resolve("refs/remotes/origin/HEAD")
                .map_err(TestError::Unexpected)?,
            Some(oid(A)?)
        );
        let listed = refs.list("refs/remotes/");
        assert_eq!(listed.len(), 2);
        for bad in ["../x", "refs/heads/../../HEAD", "refs/heads/a b", ""] {
            assert!(refs.resolve(bad).is_err(), "{bad:?}");
        }
        Ok(())
    }

    #[test]
    fn corrupt_ref_files_are_errors_and_packed_garbage_is_skipped() -> TestResult {
        let repo = TestRepo::new()?;
        repo.write_git("refs/heads/bad", "not an oid\n")?;
        repo.write_git(
            "packed-refs",
            &format!("garbage line\n{A} refs/heads/ok\nzzzz refs/heads/nope\n{A} bad name\n"),
        )?;
        let opened = Repo::open(&repo.ws)?;
        let refs = Refs::new(&opened);
        assert!(refs.resolve("refs/heads/bad").is_err());
        assert_eq!(
            refs.resolve("refs/heads/ok")
                .map_err(TestError::Unexpected)?,
            Some(oid(A)?)
        );
        assert_eq!(
            refs.resolve("refs/heads/nope")
                .map_err(TestError::Unexpected)?,
            None
        );
        Ok(())
    }
}
