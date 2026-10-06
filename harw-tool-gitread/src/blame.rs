//! `git blame`: Zeilen-Herkunft einer Datei.
//!
//! # Verantwortung
//! [`run`] ordnet jeder Zeile von `pfad` (Stand `rev`, Standard `HEAD`) den
//! Commit zu, der sie eingeführt hat. Verfahren: Die Zeilen eines Commits
//! werden gegen die Datei jedes Elternteils verglichen (Myers); Zeilen, die
//! dort unverändert vorkommen, wandern zum Elternteil, der Rest gehört dem
//! Commit. Bei Merges bekommt der erste passende Elternteil die Zeile.
//! Commits werden nach Committer-Zeit abgearbeitet.
//!
//! # Grenzen
//! - Es wird der **committete** Stand von `rev` betrachtet, nicht
//!   ungespeicherte Änderungen im Arbeitsverzeichnis.
//! - Keine Verfolgung von Umbenennungen oder verschobenem Code (`-M`/`-C`):
//!   fehlt die Datei im Elternteil, gehören die Zeilen dem Commit.
//! - Dateien bis [`MAX_BLAME_BYTES`] und [`MAX_BLAME_LINES`] Zeilen; ist ein
//!   Vergleich zu groß (Editdistanz über Grenze), werden die Zeilen dem
//!   Commit zugeschrieben und `approximate` ist `true`.
//! - Geheimnis-Pfade werden verweigert.

use crate::fmtutil::{deadline, iso_with_tz};
use crate::graph::{MAX_WALK_COMMITS, read_commit};
use crate::object::{Commit, Kind};
use crate::odb::Odb;
use crate::oid::Oid;
use crate::refs::Refs;
use crate::rev::Revs;
use crate::tree::lookup;
use harw_tool_fsread::budget::Collector;
use harw_tool_fsread::scope::is_secret_path;
use harw_tool_fsread::textdiff::{DEFAULT_MAX_D, Op, diff_lines, split_lines};
use serde_json::{Value, json};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// Größte Datei.
pub const MAX_BLAME_BYTES: usize = 1024 * 1024;
/// Höchstzahl Zeilen.
pub const MAX_BLAME_LINES: usize = 20_000;
/// Standardzahl ausgegebener Zeilen.
pub const DEFAULT_LINES: usize = 200;
/// Obergrenze ausgegebener Zeilen.
pub const HARD_LINES: usize = 2000;

/// Optionen für [`run`].
pub struct BlameOpts {
    /// Pfad im Repository.
    pub path: String,
    /// Revision (Standard `HEAD`).
    pub rev: String,
    /// Erste Zeile (1-basiert).
    pub start_line: Option<usize>,
    /// Letzte Zeile (einschließlich).
    pub end_line: Option<usize>,
    /// Höchstzahl ausgegebener Zeilen.
    pub max_lines: usize,
}

fn blob_lines(
    odb: &Odb<'_>,
    commit: &Commit,
    path: &[u8],
) -> Result<Option<(Oid, Vec<String>)>, String> {
    let Some((mode, oid)) = lookup(odb, &commit.tree, path)? else {
        return Ok(None);
    };
    if mode & 0o170_000 == 0o040_000 || mode & 0o170_000 == 0o160_000 {
        return Ok(None);
    }
    let object = odb.read_kind(&oid, Kind::Blob)?;
    if object.data.len() > MAX_BLAME_BYTES {
        return Err(format!("file is larger than {MAX_BLAME_BYTES} bytes"));
    }
    let text = String::from_utf8_lossy(&object.data).into_owned();
    Ok(Some((
        oid,
        split_lines(&text).into_iter().map(str::to_owned).collect(),
    )))
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Queued {
    when: i64,
    order: Reverse<u64>,
    oid: Oid,
}

/// Zuweisung einer Zeile: `(endgültige Zeile, Zeile im aktuellen Commit)`.
type Pending = Vec<(usize, usize)>;

/// Führt `git blame` aus.
///
/// # Errors
/// Meldung bei Geheimnispfad, fehlender Datei, Binärdatei oder Übergröße.
pub fn run(odb: &Odb<'_>, refs: &Refs<'_>, opts: &BlameOpts) -> Result<Value, String> {
    let path = opts.path.trim_matches('/');
    if path.is_empty()
        || path.contains(['\0', '\\'])
        || path
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == ".." || p.eq_ignore_ascii_case(".git"))
    {
        return Err("invalid path".to_owned());
    }
    if is_secret_path(Path::new(std::ffi::OsStr::from_bytes(path.as_bytes()))) {
        return Err(format!(
            "{path}: the path matches the secret-file denylist; it is never shown"
        ));
    }
    let revs = Revs::new(odb, refs);
    let tip = revs.commit(if opts.rev.is_empty() {
        "HEAD"
    } else {
        &opts.rev
    })?;
    let limit = deadline();
    let tip_commit = read_commit(odb, &tip)?;
    let path_bytes = path.as_bytes();
    let (_, tip_lines) = blob_lines(odb, &tip_commit, path_bytes)?
        .ok_or_else(|| format!("'{path}' does not exist in {}", tip.short()))?;
    if tip_lines.len() > MAX_BLAME_LINES {
        return Err(format!("file has more than {MAX_BLAME_LINES} lines"));
    }
    if tip_lines.iter().any(|l| l.contains('\0')) {
        return Err("binary file".to_owned());
    }
    let total = tip_lines.len();
    let start = opts.start_line.unwrap_or(1).max(1);
    let end = opts.end_line.unwrap_or(total).min(total);
    if total == 0 || start > end {
        return Ok(
            json!({"path": path, "rev": tip.hex(), "total_lines": total, "lines": [], "commits": {}, "truncated": false, "approximate": false}),
        );
    }
    let wanted: Pending = (start - 1..end).map(|i| (i, i)).collect();

    let mut blamed: HashMap<usize, Oid> = HashMap::new();
    let mut pending: HashMap<Oid, Pending> = HashMap::new();
    let mut in_heap: HashSet<Oid> = HashSet::new();
    let mut heap: BinaryHeap<Queued> = BinaryHeap::new();
    let mut counter = 0u64;
    let mut approximate = false;
    let mut processed = 0usize;
    let mut cache: HashMap<Oid, Commit> = HashMap::new();
    cache.insert(tip, tip_commit);
    pending.insert(tip, wanted);
    counter += 1;
    in_heap.insert(tip);
    heap.push(Queued {
        when: cache[&tip].committer.when,
        order: Reverse(counter),
        oid: tip,
    });

    while let Some(item) = heap.pop() {
        in_heap.remove(&item.oid);
        let Some(mut lines) = pending.remove(&item.oid) else {
            continue;
        };
        processed += 1;
        if processed > MAX_WALK_COMMITS {
            return Err(format!(
                "history has more than {MAX_WALK_COMMITS} commits to examine"
            ));
        }
        if std::time::Instant::now() > limit {
            return Err("blame timed out".to_owned());
        }
        let commit = match cache.get(&item.oid) {
            Some(c) => c.clone(),
            None => read_commit(odb, &item.oid)?,
        };
        let Some((blob, mine)) = blob_lines(odb, &commit, path_bytes)? else {
            for (fin, _) in lines {
                blamed.insert(fin, item.oid);
            }
            continue;
        };
        for parent in &commit.parents {
            if lines.is_empty() {
                break;
            }
            let parent_commit = match cache.get(parent) {
                Some(c) => c.clone(),
                None => {
                    let c = read_commit(odb, parent)?;
                    cache.insert(*parent, c.clone());
                    c
                }
            };
            let Some((parent_blob, theirs)) = blob_lines(odb, &parent_commit, path_bytes)? else {
                continue;
            };
            let mapping: HashMap<usize, usize> = if parent_blob == blob {
                (0..mine.len()).map(|i| (i, i)).collect()
            } else {
                match diff_lines(&mine, &theirs, DEFAULT_MAX_D) {
                    Some(ops) => {
                        let (mut a, mut b) = (0usize, 0usize);
                        let mut map = HashMap::new();
                        for op in ops {
                            match op {
                                Op::Equal => {
                                    map.insert(a, b);
                                    a += 1;
                                    b += 1;
                                }
                                Op::Delete => a += 1,
                                Op::Insert => b += 1,
                            }
                        }
                        map
                    }
                    None => {
                        approximate = true;
                        HashMap::new()
                    }
                }
            };
            let mut rest: Pending = Vec::new();
            let mut passed: Pending = Vec::new();
            for (fin, line) in lines {
                match mapping.get(&line) {
                    Some(parent_line) => passed.push((fin, *parent_line)),
                    None => rest.push((fin, line)),
                }
            }
            lines = rest;
            if !passed.is_empty() {
                pending.entry(*parent).or_default().extend(passed);
                if in_heap.insert(*parent) {
                    counter += 1;
                    heap.push(Queued {
                        when: parent_commit.committer.when,
                        order: Reverse(counter),
                        oid: *parent,
                    });
                }
            }
        }
        for (fin, _) in lines {
            blamed.insert(fin, item.oid);
        }
    }

    let mut commits: serde_json::Map<String, Value> = serde_json::Map::new();
    let max_lines = opts.max_lines.clamp(1, HARD_LINES);
    let mut out = Collector::new(max_lines);
    for index in start - 1..end {
        let Some(oid) = blamed.get(&index) else {
            continue;
        };
        let commit = match cache.get(oid) {
            Some(c) => c.clone(),
            None => read_commit(odb, oid)?,
        };
        let key = oid.short();
        commits.entry(key.clone()).or_insert_with(|| {
            json!({
                "commit": oid.hex(), "author": commit.author.name, "email": commit.author.email,
                "date": iso_with_tz(commit.author.when, commit.author.tz_minutes), "summary": commit.subject().chars().take(200).collect::<String>(),
            })
        });
        let text = tip_lines
            .get(index)
            .map_or("", |l| l.trim_end_matches(['\n', '\r']));
        if !out.push(json!({"line": index + 1, "commit": key, "text": text.chars().take(300).collect::<String>()})) {
            break;
        }
    }
    let truncated = out.truncated();
    Ok(json!({
        "path": path, "rev": tip.hex(), "total_lines": total, "start_line": start, "end_line": end,
        "lines": out.into_items(), "commits": commits, "truncated": truncated, "approximate": approximate,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::Repo;
    use crate::test_support::{TestError, TestRepo, TestResult};

    fn blame(repo: &TestRepo, path: &str) -> Result<Value, String> {
        blame_with(
            repo,
            BlameOpts {
                path: path.to_owned(),
                rev: "HEAD".into(),
                start_line: None,
                end_line: None,
                max_lines: 500,
            },
        )
    }

    fn blame_with(repo: &TestRepo, opts: BlameOpts) -> Result<Value, String> {
        let opened = Repo::open(&repo.ws)?;
        let odb = Odb::new(&opened);
        let refs = Refs::new(&opened);
        run(&odb, &refs, &opts)
    }

    fn summary(value: &Value) -> Vec<(u64, String)> {
        value["lines"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|l| {
                        let key = l["commit"].as_str().unwrap_or("");
                        (
                            l["line"].as_u64().unwrap_or(0),
                            value["commits"][key]["summary"]
                                .as_str()
                                .unwrap_or("")
                                .to_owned(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn history() -> TestResult<(TestRepo, Vec<Oid>)> {
        let repo = TestRepo::new()?;
        let c1 = repo.commit_files(&[("f.txt", 0o100_644, "a\nb\nc\n")], &[], "first", 100)?;
        let t2 = repo.tree_from(&[("f.txt", 0o100_644, "a\nB\nc\nd\n")])?;
        let c2 = repo.commit_as(t2, &[c1], "second", 200, "Bob", "bob@example.com")?;
        let t3 = repo.tree_from(&[("f.txt", 0o100_644, "a\nB\nc\nd\ne\n")])?;
        let c3 = repo.commit(t3, &[c2], "third", 300)?;
        repo.set_ref("refs/heads/main", c3)?;
        Ok((repo, vec![c1, c2, c3]))
    }

    #[test]
    fn attributes_each_line_to_the_introducing_commit() -> TestResult {
        let (repo, ids) = history()?;
        let data = blame(&repo, "f.txt")?;
        assert_eq!(
            summary(&data),
            vec![
                (1, "first".into()),
                (2, "second".into()),
                (3, "first".into()),
                (4, "second".into()),
                (5, "third".into())
            ]
        );
        assert_eq!(data["total_lines"], 5);
        assert_eq!(data["lines"][1]["text"], "B");
        assert_eq!(data["commits"][ids[1].short()]["author"], "Bob");
        assert_eq!(data["approximate"], false);
        Ok(())
    }

    #[test]
    fn ranges_revisions_and_merges() -> TestResult {
        let (repo, ids) = history()?;
        let ranged = blame_with(
            &repo,
            BlameOpts {
                path: "f.txt".into(),
                rev: "HEAD".into(),
                start_line: Some(2),
                end_line: Some(3),
                max_lines: 10,
            },
        )?;
        assert_eq!(ranged["lines"].as_array().map(Vec::len), Some(2));
        let old = blame_with(
            &repo,
            BlameOpts {
                path: "f.txt".into(),
                rev: ids[0].hex(),
                start_line: None,
                end_line: None,
                max_lines: 10,
            },
        )?;
        assert_eq!(old["total_lines"], 3);
        // Merge: Seitenzweig ändert Zeile 1, Hauptzweig Zeile 3.
        let base = repo.tree_from(&[("m.txt", 0o100_644, "x\ny\nz\n")])?;
        let b = repo.commit(base, &[ids[2]], "base", 400)?;
        let left_t = repo.tree_from(&[("m.txt", 0o100_644, "x\ny\nZ\n")])?;
        let left = repo.commit(left_t, &[b], "left", 500)?;
        let right_t = repo.tree_from(&[("m.txt", 0o100_644, "X\ny\nz\n")])?;
        let right = repo.commit(right_t, &[b], "right", 450)?;
        let merged_t = repo.tree_from(&[("m.txt", 0o100_644, "X\ny\nZ\n")])?;
        let merge = repo.commit(merged_t, &[left, right], "merge", 600)?;
        repo.set_ref("refs/heads/main", merge)?;
        let data = blame(&repo, "m.txt")?;
        assert_eq!(
            summary(&data),
            vec![(1, "right".into()), (2, "base".into()), (3, "left".into())]
        );
        Ok(())
    }

    #[test]
    fn errors_and_refusals() -> TestResult {
        let (repo, _) = history()?;
        for bad in ["", "../x", ".git/config", "a//b", "/"] {
            let error = blame(&repo, bad).err().unwrap_or_default();
            assert!(error.contains("invalid path"), "{bad}: {error}");
        }
        for missing in ["nope.txt", "f.txt/x"] {
            assert!(blame(&repo, missing).is_err(), "{missing}");
        }
        let secret = TestRepo::new()?;
        secret.commit_files(&[(".env", 0o100_644, "TOKEN=1\n")], &[], "s", 1)?;
        let error = blame(&secret, ".env")
            .err()
            .ok_or(TestError::Missing("error"))?;
        assert!(error.contains("secret"));
        let bin = TestRepo::new()?;
        bin.commit_files(&[("b.bin", 0o100_644, "a\0b\n")], &[], "b", 1)?;
        assert!(blame(&bin, "b.bin").is_err());
        let empty = TestRepo::new()?;
        empty.commit_files(&[("e.txt", 0o100_644, "")], &[], "e", 1)?;
        assert_eq!(blame(&empty, "e.txt")?["total_lines"], 0);
        assert!(
            blame_with(
                &repo,
                BlameOpts {
                    path: "f.txt".into(),
                    rev: "nope".into(),
                    start_line: None,
                    end_line: None,
                    max_lines: 1
                }
            )
            .is_err()
        );
        let past_end = blame_with(
            &repo,
            BlameOpts {
                path: "f.txt".into(),
                rev: "HEAD".into(),
                start_line: Some(50),
                end_line: None,
                max_lines: 5,
            },
        )?;
        assert_eq!(past_end["lines"].as_array().map(Vec::len), Some(0));
        Ok(())
    }

    #[test]
    fn output_is_limited() -> TestResult {
        let repo = TestRepo::new()?;
        let text = "line\n".repeat(50);
        repo.commit_files(&[("l.txt", 0o100_644, text.as_str())], &[], "l", 1)?;
        let data = blame_with(
            &repo,
            BlameOpts {
                path: "l.txt".into(),
                rev: "HEAD".into(),
                start_line: None,
                end_line: None,
                max_lines: 10,
            },
        )?;
        assert_eq!(data["lines"].as_array().map(Vec::len), Some(10));
        assert_eq!(data["truncated"], true);
        Ok(())
    }
}
