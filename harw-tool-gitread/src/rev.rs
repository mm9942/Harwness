//! Revisionsausdrücke (`gitrevisions`, der nützliche Teil).
//!
//! # Verantwortung
//! [`Revs`] löst Ausdrücke wie `HEAD`, `main`, `v1.2`, `origin/main`,
//! `a1b2c3d`, `HEAD~3`, `main^2`, `v1^{commit}`, `HEAD^{tree}` und
//! `rev:pfad` zu Objekt-IDs auf.
//!
//! Auflösungsreihenfolge für einen Namen `x` (wie `git rev-parse`): `HEAD` und
//! `@`, 40 Hex-Zeichen, dann `refs/x`, `refs/tags/x`, `refs/heads/x`,
//! `refs/remotes/x`, `refs/remotes/x/HEAD`, zuletzt ein eindeutiges
//! Hex-Präfix (mindestens 4 Zeichen).
//!
//! # Grenzen
//! Nicht unterstützt: `@{…}` (Reflog), `^{/regex}`, `:/text`, `rev:./rel`.
//! `~N` ist auf [`MAX_ANCESTOR_STEPS`] Schritte begrenzt, Ausdrücke auf
//! [`MAX_SPEC_BYTES`] Bytes.

use crate::object::{Kind, parse_commit, parse_tag};
use crate::odb::Odb;
use crate::oid::Oid;
use crate::refs::{Refs, valid_ref_name};

/// Längster Ausdruck.
pub const MAX_SPEC_BYTES: usize = 512;

/// Größtes `N` bei `~N`.
pub const MAX_ANCESTOR_STEPS: usize = 100_000;

/// Höchstzahl entpackter Tag-Ebenen.
const MAX_PEEL: usize = 16;

/// Auflöser für Revisionsausdrücke.
pub struct Revs<'a> {
    odb: &'a Odb<'a>,
    refs: &'a Refs<'a>,
}

/// Zerlegt `rev:pfad` in Revision und Pfad.
#[must_use]
pub fn split_path(spec: &str) -> (&str, Option<&str>) {
    match spec.split_once(':') {
        Some((rev, path)) => (rev, Some(path)),
        None => (spec, None),
    }
}

impl<'a> Revs<'a> {
    /// Neuer Auflöser.
    #[must_use]
    pub fn new(odb: &'a Odb<'a>, refs: &'a Refs<'a>) -> Self {
        Self { odb, refs }
    }

    fn base(&self, name: &str) -> Result<Oid, String> {
        if name.is_empty() {
            return Err("empty revision".to_owned());
        }
        if name == "HEAD" || name == "@" {
            return self
                .refs
                .resolve("HEAD")?
                .ok_or_else(|| "HEAD does not point to a commit yet (no commits)".to_owned());
        }
        if let Some(full) = Oid::from_hex(name) {
            return Ok(full);
        }
        let pseudo = name.chars().all(|c| c.is_ascii_uppercase() || c == '_') && name.contains('_');
        let mut candidates: Vec<String> = Vec::new();
        if name.starts_with("refs/") || pseudo {
            candidates.push(name.to_owned());
        }
        candidates.push(format!("refs/{name}"));
        candidates.push(format!("refs/tags/{name}"));
        candidates.push(format!("refs/heads/{name}"));
        candidates.push(format!("refs/remotes/{name}"));
        candidates.push(format!("refs/remotes/{name}/HEAD"));
        for candidate in candidates {
            if !valid_ref_name(&candidate) {
                continue;
            }
            if let Some(oid) = self.refs.resolve(&candidate)? {
                return Ok(oid);
            }
        }
        if name.len() >= 4 && name.chars().all(|c| c.is_ascii_hexdigit()) {
            return self.odb.expand_prefix(name);
        }
        Err(format!(
            "unknown revision '{}'",
            name.chars().take(64).collect::<String>()
        ))
    }

    /// Entpackt Tags, bis ein Nicht-Tag-Objekt erreicht ist.
    ///
    /// # Errors
    /// Meldung bei fehlenden Objekten oder zu tiefer Tag-Kette.
    pub fn peel_tags(&self, mut oid: Oid) -> Result<Oid, String> {
        for _ in 0..MAX_PEEL {
            let object = self.odb.read(&oid)?;
            if object.kind != Kind::Tag {
                return Ok(oid);
            }
            oid = parse_tag(&object.data)?.target;
        }
        Err("tag chain is too deep".to_owned())
    }

    /// Entpackt bis zu einem Commit.
    ///
    /// # Errors
    /// Meldung, wenn das Ziel kein Commit ist.
    pub fn peel_commit(&self, oid: Oid) -> Result<Oid, String> {
        let peeled = self.peel_tags(oid)?;
        let object = self.odb.read(&peeled)?;
        if object.kind == Kind::Commit {
            Ok(peeled)
        } else {
            Err(format!(
                "{} is a {}, not a commit",
                peeled.short(),
                object.kind.name()
            ))
        }
    }

    /// Entpackt bis zu einem Baum (Commit → Wurzelbaum).
    ///
    /// # Errors
    /// Meldung, wenn das Ziel weder Commit noch Baum ist.
    pub fn peel_tree(&self, oid: Oid) -> Result<Oid, String> {
        let peeled = self.peel_tags(oid)?;
        let object = self.odb.read(&peeled)?;
        match object.kind {
            Kind::Tree => Ok(peeled),
            Kind::Commit => Ok(parse_commit(&object.data)?.tree),
            other => Err(format!(
                "{} is a {}, not a tree or commit",
                peeled.short(),
                other.name()
            )),
        }
    }

    /// Löst einen Ausdruck (ohne `:pfad`) zu einer Objekt-ID auf.
    ///
    /// # Errors
    /// Meldung bei unbekannten Namen, ungültigen Suffixen oder fehlenden Eltern.
    pub fn object(&self, spec: &str) -> Result<Oid, String> {
        if spec.len() > MAX_SPEC_BYTES {
            return Err(format!("revision is longer than {MAX_SPEC_BYTES} bytes"));
        }
        if spec.contains(['\0', '\n', ' ']) {
            return Err("revision must not contain whitespace or NUL".to_owned());
        }
        if spec.contains("@{") || spec.contains("..") {
            return Err("unsupported revision syntax (ranges and reflog expressions are handled elsewhere or not at all)".to_owned());
        }
        let split = spec.find(['~', '^']).unwrap_or(spec.len());
        let (name, mut rest) = spec.split_at(split);
        let mut current = self.base(name)?;
        while !rest.is_empty() {
            if let Some(after) = rest.strip_prefix("^{") {
                let (kind, tail) = after.split_once('}').ok_or("unterminated ^{…}")?;
                current = match kind {
                    "" => self.peel_tags(current)?,
                    "commit" => self.peel_commit(current)?,
                    "tree" => self.peel_tree(current)?,
                    "blob" => {
                        let peeled = self.peel_tags(current)?;
                        self.odb.read_kind(&peeled, Kind::Blob)?;
                        peeled
                    }
                    "tag" => {
                        self.odb.read_kind(&current, Kind::Tag)?;
                        current
                    }
                    other => {
                        return Err(format!(
                            "unsupported peel type '^{{{}}}'",
                            other.chars().take(16).collect::<String>()
                        ));
                    }
                };
                rest = tail;
            } else if let Some(after) = rest.strip_prefix('^') {
                let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
                let n: usize = if digits.is_empty() {
                    1
                } else {
                    digits
                        .parse()
                        .map_err(|_| "parent number is too large".to_owned())?
                };
                rest = &after[digits.len()..];
                let commit = parse_commit(
                    &self
                        .odb
                        .read_kind(&self.peel_commit(current)?, Kind::Commit)?
                        .data,
                )?;
                if n == 0 {
                    current = self.peel_commit(current)?;
                } else {
                    current = *commit
                        .parents
                        .get(n - 1)
                        .ok_or_else(|| format!("commit has no parent number {n}"))?;
                }
            } else if let Some(after) = rest.strip_prefix('~') {
                let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
                let n: usize = if digits.is_empty() {
                    1
                } else {
                    digits
                        .parse()
                        .map_err(|_| "ancestor count is too large".to_owned())?
                };
                if n > MAX_ANCESTOR_STEPS {
                    return Err(format!("~{n} exceeds the limit of {MAX_ANCESTOR_STEPS}"));
                }
                rest = &after[digits.len()..];
                current = self.peel_commit(current)?;
                for _ in 0..n {
                    let commit = parse_commit(&self.odb.read_kind(&current, Kind::Commit)?.data)?;
                    current = *commit.parents.first().ok_or("commit has no parent")?;
                }
            } else {
                return Err(format!(
                    "invalid revision suffix '{}'",
                    rest.chars().take(16).collect::<String>()
                ));
            }
        }
        Ok(current)
    }

    /// Löst einen Ausdruck zu einem Commit auf (Tags werden entpackt).
    ///
    /// # Errors
    /// Siehe [`Revs::object`] und [`Revs::peel_commit`].
    pub fn commit(&self, spec: &str) -> Result<Oid, String> {
        self.peel_commit(self.object(spec)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::Repo;
    use crate::test_support::{TestError, TestRepo, TestResult};

    struct Fixture {
        repo: TestRepo,
        c1: Oid,
        c2: Oid,
        c3: Oid,
        merge: Oid,
        tag: Oid,
    }

    fn fixture() -> TestResult<Fixture> {
        let repo = TestRepo::new()?;
        let blob = repo.blob("x\n")?;
        let tree = repo.tree(&[(0o100_644, "f", blob)])?;
        let c1 = repo.commit(tree, &[], "one", 1_000)?;
        let c2 = repo.commit(tree, &[c1], "two", 2_000)?;
        let side = repo.commit(tree, &[c1], "side", 2_500)?;
        let c3 = repo.commit(tree, &[c2], "three", 3_000)?;
        let merge = repo.commit(tree, &[c3, side], "merge", 4_000)?;
        let tag_body =
            format!("object {c2}\ntype commit\ntag v1\ntagger A <a@x> 5 +0000\n\nrelease\n");
        let tag = repo.write_loose("tag", tag_body.as_bytes())?;
        repo.set_ref("refs/heads/main", merge)?;
        repo.set_ref("refs/heads/feature/x", c2)?;
        repo.set_ref("refs/tags/v1", tag)?;
        repo.set_ref("refs/tags/light", c1)?;
        repo.set_ref("refs/remotes/origin/main", c3)?;
        repo.head_branch("main")?;
        Ok(Fixture {
            repo,
            c1,
            c2,
            c3,
            merge,
            tag,
        })
    }

    fn with<T>(fx: &Fixture, body: impl FnOnce(&Revs<'_>) -> Result<T, String>) -> TestResult<T> {
        let opened = Repo::open(&fx.repo.ws)?;
        let odb = Odb::new(&opened);
        let refs = Refs::new(&opened);
        Ok(body(&Revs::new(&odb, &refs))?)
    }

    #[test]
    fn names_resolve_in_git_order() -> TestResult {
        let fx = fixture()?;
        with(&fx, |r| {
            assert_eq!(r.object("HEAD")?, fx.merge);
            assert_eq!(r.object("@")?, fx.merge);
            assert_eq!(r.object("main")?, fx.merge);
            assert_eq!(r.object("refs/heads/main")?, fx.merge);
            assert_eq!(r.object("feature/x")?, fx.c2);
            assert_eq!(r.object("light")?, fx.c1);
            assert_eq!(r.object("origin/main")?, fx.c3);
            assert_eq!(r.object("v1")?, fx.tag);
            assert_eq!(r.commit("v1")?, fx.c2);
            assert_eq!(r.object(&fx.c3.hex())?, fx.c3);
            assert_eq!(r.object(&fx.c3.hex()[..8])?, fx.c3);
            Ok(())
        })
    }

    #[test]
    fn suffixes_walk_the_graph() -> TestResult {
        let fx = fixture()?;
        with(&fx, |r| {
            assert_eq!(r.object("HEAD^")?, fx.c3);
            assert_eq!(r.object("HEAD^1")?, fx.c3);
            assert_eq!(r.object("HEAD~1")?, fx.c3);
            assert_eq!(r.object("HEAD~2")?, fx.c2);
            assert_eq!(r.object("HEAD~3")?, fx.c1);
            assert_eq!(r.object("HEAD^^")?, fx.c2);
            assert_eq!(r.object("HEAD^0")?, fx.merge);
            assert_eq!(r.object("v1~1")?, fx.c1);
            assert_eq!(r.object("v1^{commit}")?, fx.c2);
            assert_eq!(r.object("v1^{}")?, fx.c2);
            assert_eq!(r.object("v1^{tag}")?, fx.tag);
            assert!(r.object("HEAD^2")? != fx.c3);
            Ok(())
        })
    }

    #[test]
    fn tree_peel_and_errors() -> TestResult {
        let fx = fixture()?;
        with(&fx, |r| {
            let tree = r.object("HEAD^{tree}")?;
            assert_eq!(r.peel_tree(r.object("HEAD")?)?, tree);
            for bad in [
                "",
                "nope",
                "HEAD^3",
                "HEAD~9",
                "HEAD^{blob}",
                "HEAD^{xx}",
                "HEAD^{",
                "HEAD~x",
                "a b",
                "main..HEAD",
                "HEAD@{1}",
                "zzzz",
                "abc",
                "HEAD^{tag}",
                "HEAD~1000000",
            ] {
                assert!(r.object(bad).is_err(), "{bad:?} must fail");
            }
            assert!(r.commit(&format!("{}", r.object("HEAD^{tree}")?)).is_err());
            let error = r.object("HEAD~1000000").err().unwrap_or_default();
            assert!(error.contains("exceeds the limit"), "{error}");
            let long = "a".repeat(MAX_SPEC_BYTES + 1);
            let error = r.object(&long).err().unwrap_or_default();
            assert!(error.contains("longer than"), "{error}");
            Ok(())
        })
    }

    #[test]
    fn unborn_head_and_path_split() -> TestResult {
        let repo = TestRepo::new()?;
        repo.head_branch("main")?;
        let fx = Fixture {
            repo,
            c1: Oid::ZERO,
            c2: Oid::ZERO,
            c3: Oid::ZERO,
            merge: Oid::ZERO,
            tag: Oid::ZERO,
        };
        let error = with(&fx, |r| r.object("HEAD"))
            .err()
            .ok_or(TestError::Missing("error"));
        assert!(error.is_ok());
        assert_eq!(split_path("HEAD:src/a.rs"), ("HEAD", Some("src/a.rs")));
        assert_eq!(split_path("main"), ("main", None));
        Ok(())
    }
}
