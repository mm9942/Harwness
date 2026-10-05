//! Bäume als flache, nach Pfad sortierte Listen und Pfadsuche im Baum.
//!
//! # Härtung
//! Baumeinträge mit `/`, NUL, `.`, `..` oder `.git` im Namen sind ein
//! Fehler (kein Git erzeugt sie; sie wären ein Weg, später aus dem Workspace
//! auszubrechen). Tiefe und Eintragszahl sind begrenzt.

use crate::object::{Kind, TreeEntry, parse_tree};
use crate::odb::Odb;
use crate::oid::Oid;
use crate::pathspec::Pathspec;

/// Höchstzahl Einträge einer flachen Liste.
pub const MAX_FLAT_ENTRIES: usize = 2_000_000;

/// Größte Verzeichnistiefe.
pub const MAX_TREE_DEPTH: usize = 64;

/// Ein Nicht-Verzeichnis-Eintrag mit vollem Pfad.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlatEntry {
    /// Pfad (Bytes, `/`-getrennt, ohne führenden `/`).
    pub path: Vec<u8>,
    /// Modus.
    pub mode: u32,
    /// Objekt-ID.
    pub oid: Oid,
}

/// Ist `name` ein zulässiger Pfadbestandteil?
#[must_use]
pub fn safe_name(name: &[u8]) -> bool {
    !(name.is_empty()
        || name == b"."
        || name == b".."
        || name.contains(&b'/')
        || name.contains(&0)
        || name.eq_ignore_ascii_case(b".git"))
}

fn join(prefix: &[u8], name: &[u8]) -> Vec<u8> {
    let mut path = Vec::with_capacity(prefix.len() + 1 + name.len());
    path.extend_from_slice(prefix);
    if !prefix.is_empty() {
        path.push(b'/');
    }
    path.extend_from_slice(name);
    path
}

fn read_tree(odb: &Odb<'_>, oid: &Oid) -> Result<Vec<TreeEntry>, String> {
    let object = odb.read_kind(oid, Kind::Tree)?;
    parse_tree(&object.data)
}

/// Flacht einen Baum ab (nur Einträge, die zum Pfadfilter passen), sortiert nach Pfad.
///
/// # Errors
/// Meldung bei fehlenden/beschädigten Bäumen, unzulässigen Namen, Übergröße.
pub fn flatten(odb: &Odb<'_>, tree: &Oid, spec: &Pathspec) -> Result<Vec<FlatEntry>, String> {
    let mut out = Vec::new();
    flatten_into(odb, tree, b"", spec, 0, &mut out)?;
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

fn flatten_into(
    odb: &Odb<'_>,
    tree: &Oid,
    prefix: &[u8],
    spec: &Pathspec,
    depth: usize,
    out: &mut Vec<FlatEntry>,
) -> Result<(), String> {
    if depth > MAX_TREE_DEPTH {
        return Err(format!(
            "tree is nested deeper than {MAX_TREE_DEPTH} levels"
        ));
    }
    for entry in read_tree(odb, tree)? {
        if !safe_name(&entry.name) {
            return Err("tree contains an unsafe path name".to_owned());
        }
        let path = join(prefix, &entry.name);
        if entry.is_tree() {
            if spec.may_contain(&path) {
                flatten_into(odb, &entry.oid, &path, spec, depth + 1, out)?;
            }
        } else if spec.matches(&path) {
            if out.len() >= MAX_FLAT_ENTRIES {
                return Err(format!("tree has more than {MAX_FLAT_ENTRIES} entries"));
            }
            out.push(FlatEntry {
                path,
                mode: entry.mode,
                oid: entry.oid,
            });
        }
    }
    Ok(())
}

/// Sucht `path` im Baum; `Some((modus, oid))` auch für Verzeichnisse.
///
/// # Errors
/// Meldung bei beschädigten Bäumen oder Tiefenüberschreitung.
pub fn lookup(odb: &Odb<'_>, tree: &Oid, path: &[u8]) -> Result<Option<(u32, Oid)>, String> {
    let mut current = *tree;
    let mut parts = path
        .split(|b| *b == b'/')
        .filter(|p| !p.is_empty())
        .peekable();
    if parts.peek().is_none() {
        return Ok(Some((0o040_000, current)));
    }
    let mut depth = 0usize;
    while let Some(part) = parts.next() {
        depth += 1;
        if depth > MAX_TREE_DEPTH {
            return Err(format!(
                "path is nested deeper than {MAX_TREE_DEPTH} levels"
            ));
        }
        let Some(entry) = read_tree(odb, &current)?
            .into_iter()
            .find(|e| e.name == part)
        else {
            return Ok(None);
        };
        if parts.peek().is_none() {
            return Ok(Some((entry.mode, entry.oid)));
        }
        if !entry.is_tree() {
            return Ok(None);
        }
        current = entry.oid;
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::Repo;
    use crate::test_support::{TestError, TestRepo, TestResult};

    fn sample(repo: &TestRepo) -> TestResult<Oid> {
        let a = repo.blob("a\n")?;
        let b = repo.blob("b\n")?;
        let sub = repo.tree(&[(0o100_644, "b.txt", b), (0o100_755, "run.sh", a)])?;
        let deep = repo.tree(&[(0o040_000, "sub", sub), (0o100_644, "a.txt", a)])?;
        repo.tree(&[
            (0o040_000, "dir", deep),
            (0o100_644, "a.txt", a),
            (0o100_644, "a.txt2", b),
            (0o120_000, "link", b),
        ])
    }

    #[test]
    fn flattens_sorted_and_filters_by_pathspec() -> TestResult {
        let repo = TestRepo::new()?;
        let root = sample(&repo)?;
        let opened = Repo::open(&repo.ws)?;
        let odb = Odb::new(&opened);
        let all = flatten(&odb, &root, &Pathspec::all())?;
        let paths: Vec<String> = all
            .iter()
            .map(|e| String::from_utf8_lossy(&e.path).into_owned())
            .collect();
        assert_eq!(
            paths,
            vec![
                "a.txt",
                "a.txt2",
                "dir/a.txt",
                "dir/sub/b.txt",
                "dir/sub/run.sh",
                "link"
            ]
        );
        let only = flatten(&odb, &root, &Pathspec::parse(&["dir/sub".to_owned()])?)?;
        assert_eq!(only.len(), 2);
        let file = flatten(&odb, &root, &Pathspec::parse(&["a.txt".to_owned()])?)?;
        assert_eq!(file.len(), 1);
        assert_eq!(file[0].mode, 0o100_644);
        Ok(())
    }

    #[test]
    fn lookup_finds_files_dirs_and_misses() -> TestResult {
        let repo = TestRepo::new()?;
        let root = sample(&repo)?;
        let opened = Repo::open(&repo.ws)?;
        let odb = Odb::new(&opened);
        assert_eq!(
            lookup(&odb, &root, b"dir/sub/run.sh")?.map(|(m, _)| m),
            Some(0o100_755)
        );
        assert_eq!(
            lookup(&odb, &root, b"dir/sub")?.map(|(m, _)| m),
            Some(0o040_000)
        );
        assert_eq!(lookup(&odb, &root, b"")?.map(|(m, _)| m), Some(0o040_000));
        assert_eq!(lookup(&odb, &root, b"dir/nope")?, None);
        assert_eq!(lookup(&odb, &root, b"a.txt/x")?, None);
        Ok(())
    }

    fn raw_tree(mode: u32, name: &[u8], oid: &Oid) -> Vec<u8> {
        let mut data = format!("{mode:o} ").into_bytes();
        data.extend_from_slice(name);
        data.push(0);
        data.extend_from_slice(oid.as_bytes());
        data
    }

    #[test]
    fn unsafe_names_are_rejected() -> TestResult {
        let repo = TestRepo::new()?;
        let blob = repo.blob("x")?;
        for name in ["..", ".git", ".GIT"] {
            let tree = repo.write_loose("tree", &raw_tree(0o100_644, name.as_bytes(), &blob))?;
            let opened = Repo::open(&repo.ws)?;
            let odb = Odb::new(&opened);
            let error = flatten(&odb, &tree, &Pathspec::all())
                .err()
                .ok_or(TestError::Missing("error"))?;
            assert!(error.contains("unsafe"), "{name}: {error}");
        }
        assert!(safe_name(b"ok.txt"));
        assert!(!safe_name(b"a/b"));
        assert!(!safe_name(b""));
        Ok(())
    }
}
