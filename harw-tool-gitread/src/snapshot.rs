//! Momentaufnahmen (Baum, Index, Arbeitsverzeichnis) und ihr Vergleich.
//!
//! # Verantwortung
//! Ein [`Snapshot`] ist eine nach Pfad sortierte Abbildung
//! `Pfad → (Modus, Objekt-ID, Herkunft)`. Es gibt drei Quellen:
//! [`from_flat`] (ein Commit-Baum), [`from_index`] (Stage 0 des Index) und
//! [`worktree`] (die Dateien im Arbeitsverzeichnis, für eine Pfadliste).
//! [`changes`] vergleicht zwei Momentaufnahmen; daraus entstehen `status`
//! und `diff`.
//!
//! # Arbeitsverzeichnis
//! Für jede Datei wird `lstat` über den symlinkfreien [`harw_tool_fsread::scope::Scope`]
//! gemacht. Passen Größe und mtime zum Index-Eintrag (und ist der Eintrag
//! nicht „racy“, also nicht so neu wie der Index selbst), gilt die ID des
//! Index; sonst wird der Inhalt gestreamt gehasht (nie vollständig im
//! Speicher). Geheimnis-Dateien werden dabei gehasht, ihr Inhalt wird nie
//! ausgegeben ([`harw_tool_fsread::scope::Scope::open_read_for_comparison`]).
//!
//! # Grenzen
//! Keine Filter (`core.autocrlf`, `.gitattributes`, Clean/Smudge): Dateien,
//! die Git erst nach einer Umwandlung vergleicht, können als geändert
//! erscheinen. Submodule (Gitlinks) werden nicht in ihr Verzeichnis verfolgt.

use crate::index::{Index, IndexEntry};
use crate::oid::{Oid, hash_blob_stream, hash_object};
use crate::pathspec::Pathspec;
use crate::repo::Repo;
use crate::tree::FlatEntry;
use harw_tool_fsread::scope::RelPath;
use rustix::fs::FileType;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::time::Instant;

/// Woher der Inhalt einer Seite gelesen wird.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Aus der Objektdatenbank (Blob mit dieser ID).
    Odb,
    /// Aus dem Arbeitsverzeichnis (Datei bzw. Symlink-Ziel).
    Worktree,
}

/// Eine Seite eines Vergleichs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapEntry {
    /// Modus.
    pub mode: u32,
    /// Objekt-ID (bei Arbeitsverzeichnis-Dateien die des Inhalts).
    pub oid: Oid,
    /// Herkunft des Inhalts.
    pub origin: Origin,
}

/// Pfad → Eintrag.
pub type Snapshot = BTreeMap<Vec<u8>, SnapEntry>;

/// Art einer Änderung.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// Neu.
    Added,
    /// Gelöscht.
    Deleted,
    /// Inhalt oder Rechte geändert.
    Modified,
    /// Dateityp gewechselt (Datei ↔ Symlink ↔ Submodul).
    TypeChange,
}

impl ChangeKind {
    /// Kurzbuchstabe wie bei `--name-status`.
    #[must_use]
    pub fn letter(self) -> char {
        match self {
            Self::Added => 'A',
            Self::Deleted => 'D',
            Self::Modified => 'M',
            Self::TypeChange => 'T',
        }
    }

    /// Klartext.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Deleted => "deleted",
            Self::Modified => "modified",
            Self::TypeChange => "typechange",
        }
    }
}

/// Eine Änderung zwischen zwei Momentaufnahmen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// Pfad.
    pub path: Vec<u8>,
    /// Art.
    pub kind: ChangeKind,
    /// Alte Seite.
    pub old: Option<SnapEntry>,
    /// Neue Seite.
    pub new: Option<SnapEntry>,
}

/// Dateityp-Bits eines Modus.
#[must_use]
pub fn type_bits(mode: u32) -> u32 {
    mode & 0o170_000
}

/// Alte Git-Versionen schrieben auch `100664`: reguläre Dateien werden auf
/// `100644`/`100755` abgebildet (wie Git es beim Lesen tut).
#[must_use]
pub fn normalize_mode(mode: u32) -> u32 {
    if type_bits(mode) == 0o100_000 {
        if mode & 0o100 != 0 {
            0o100_755
        } else {
            0o100_644
        }
    } else {
        mode
    }
}

/// Aus einer flachen Baumliste.
#[must_use]
pub fn from_flat(flat: Vec<FlatEntry>) -> Snapshot {
    flat.into_iter()
        .map(|e| {
            (
                e.path,
                SnapEntry {
                    mode: normalize_mode(e.mode),
                    oid: e.oid,
                    origin: Origin::Odb,
                },
            )
        })
        .collect()
}

/// Zerlegung des Index in normale, Konflikt- und Intent-to-add-Pfade.
#[derive(Debug, Default)]
pub struct IndexView<'a> {
    /// Stage-0-Einträge (ohne Intent-to-add), gefiltert nach Pfadfilter.
    pub snapshot: Snapshot,
    /// Alle Stage-0-Einträge samt Metadaten (inkl. Intent-to-add), gefiltert.
    pub entries: BTreeMap<&'a [u8], &'a IndexEntry>,
    /// Konfliktpfade mit ihren Stages.
    pub conflicts: BTreeMap<Vec<u8>, Vec<u8>>,
    /// Intent-to-add-Pfade.
    pub intent_to_add: BTreeSet<Vec<u8>>,
}

/// Zerlegt den Index.
#[must_use]
pub fn from_index<'a>(index: &'a Index, spec: &Pathspec) -> IndexView<'a> {
    let mut view = IndexView::default();
    for entry in &index.entries {
        if !spec.matches(&entry.path) {
            continue;
        }
        if entry.stage > 0 {
            let stages = view.conflicts.entry(entry.path.clone()).or_default();
            if !stages.contains(&entry.stage) {
                stages.push(entry.stage);
            }
            continue;
        }
        view.entries.insert(entry.path.as_slice(), entry);
        if entry.intent_to_add {
            view.intent_to_add.insert(entry.path.clone());
        } else {
            view.snapshot.insert(
                entry.path.clone(),
                SnapEntry {
                    mode: normalize_mode(entry.mode),
                    oid: entry.oid,
                    origin: Origin::Odb,
                },
            );
        }
    }
    // Konfliktpfade zählen nicht als normale Einträge.
    for path in view.conflicts.keys() {
        view.snapshot.remove(path);
        view.entries.remove(path.as_slice());
    }
    view
}

/// Baut aus Bytes einen [`RelPath`] relativ zur Wurzel.
#[must_use]
pub fn rel_of(path: &[u8]) -> RelPath {
    let mut rel = RelPath::root();
    for part in path.split(|b| *b == b'/').filter(|p| !p.is_empty()) {
        rel = rel.join(OsStr::from_bytes(part));
    }
    rel
}

/// Fehlt der Eintrag (auch „hinter“ einem Symlink oder einer Datei)?
fn missing(error: &std::io::Error) -> bool {
    use rustix::io::Errno;
    matches!(error.kind(), std::io::ErrorKind::NotFound)
        || error.raw_os_error().is_some_and(|code| {
            [Errno::NOENT, Errno::NOTDIR, Errno::LOOP, Errno::XDEV]
                .iter()
                .any(|e| e.raw_os_error() == code)
        })
}

/// Hilfsdaten für den Arbeitsverzeichnis-Vergleich.
pub struct WorktreeCtx<'a> {
    /// Repository.
    pub repo: &'a Repo,
    /// Frist für den gesamten Vergleich.
    pub deadline: Instant,
    /// mtime-Sekunden des Index (für die Racy-Prüfung); `None` = immer hashen.
    pub index_mtime: Option<u32>,
}

fn nsec_u32(value: u64) -> u32 {
    u32::try_from(value & 0xFFFF_FFFF).unwrap_or(0)
}

fn to_u32(value: i64) -> u32 {
    u32::try_from(value.rem_euclid(1 << 32)).unwrap_or(0)
}

/// Mtime-Sekunden der Index-Datei.
#[must_use]
pub fn index_mtime(repo: &Repo) -> Option<u32> {
    let rel = repo.git_path("index").ok()?;
    let stat = repo.scope.lstat(&rel).ok()?;
    Some(to_u32(stat.st_mtime))
}

fn worktree_entry(
    ctx: &WorktreeCtx<'_>,
    path: &[u8],
    cached: Option<&IndexEntry>,
) -> Result<Option<SnapEntry>, String> {
    let describe =
        |error: &dyn std::fmt::Display| format!("{}: {error}", String::from_utf8_lossy(path));
    if let Some(entry) = cached {
        if entry.skip_worktree || entry.assume_valid || type_bits(entry.mode) == 0o160_000 {
            // Gitlinks, Sparse-Checkout und assume-valid: dem Index glauben.
            let present =
                type_bits(entry.mode) != 0o160_000 || ctx.repo.scope.lstat(&rel_of(path)).is_ok();
            return Ok(present.then_some(SnapEntry {
                mode: entry.mode,
                oid: entry.oid,
                origin: Origin::Odb,
            }));
        }
    }
    if Instant::now() > ctx.deadline {
        return Err("comparison with the working tree timed out".to_owned());
    }
    let rel = rel_of(path);
    let stat = match ctx.repo.scope.lstat(&rel) {
        Ok(stat) => stat,
        Err(error) if missing(&error) => return Ok(None),
        Err(error) => return Err(describe(&harw_tool_fsread::scope::io_message(&error))),
    };
    match FileType::from_raw_mode(stat.st_mode) {
        FileType::Symlink => {
            let target = ctx
                .repo
                .scope
                .read_link(&rel)
                .map_err(|e| describe(&harw_tool_fsread::scope::io_message(&e)))?;
            let oid = hash_object("blob", target.as_os_str().as_bytes());
            Ok(Some(SnapEntry {
                mode: 0o120_000,
                oid,
                origin: Origin::Worktree,
            }))
        }
        FileType::RegularFile => {
            let mode = if stat.st_mode & 0o100 != 0 {
                0o100_755
            } else {
                0o100_644
            };
            if let Some(entry) = cached {
                let size_matches = u64::try_from(stat.st_size)
                    .is_ok_and(|size| u32::try_from(size & 0xFFFF_FFFF).unwrap_or(0) == entry.size);
                let mtime_matches = to_u32(stat.st_mtime) == entry.mtime_sec
                    && nsec_u32(stat.st_mtime_nsec) == entry.mtime_nsec;
                let racy = ctx.index_mtime.is_none_or(|index| entry.mtime_sec >= index);
                if size_matches && mtime_matches && !racy && type_bits(entry.mode) == 0o100_000 {
                    return Ok(Some(SnapEntry {
                        mode,
                        oid: entry.oid,
                        origin: Origin::Odb,
                    }));
                }
            }
            let mut file = match ctx.repo.scope.open_read_for_comparison(&rel) {
                Ok(file) => file,
                Err(harw_tool_fsread::scope::ScopeError::Io(error)) if missing(&error) => {
                    return Ok(None);
                }
                Err(error) => return Err(describe(&error)),
            };
            let size = rustix::fs::fstat(&file).map_err(|e| describe(&e))?.st_size;
            let len = u64::try_from(size).map_err(|_| describe(&"negative file size"))?;
            let oid = hash_blob_stream(&mut file, len).map_err(|e| describe(&e))?;
            Ok(Some(SnapEntry {
                mode,
                oid,
                origin: Origin::Worktree,
            }))
        }
        // Verzeichnis, FIFO, Socket, Gerät: kein Git-Inhalt.
        _ => Ok(None),
    }
}

/// Momentaufnahme des Arbeitsverzeichnisses für `paths`.
/// Fehlende Dateien fehlen im Ergebnis.
///
/// # Errors
/// Meldung bei Frist oder unlesbaren Dateien.
pub fn worktree<'a>(
    ctx: &WorktreeCtx<'_>,
    paths: impl IntoIterator<Item = (&'a [u8], Option<&'a IndexEntry>)>,
) -> Result<Snapshot, String> {
    let mut out = Snapshot::new();
    for (path, cached) in paths {
        if let Some(entry) = worktree_entry(ctx, path, cached)? {
            out.insert(path.to_vec(), entry);
        }
    }
    Ok(out)
}

/// Vergleicht zwei Momentaufnahmen (sortiert nach Pfad).
#[must_use]
pub fn changes(old: &Snapshot, new: &Snapshot) -> Vec<Change> {
    let mut out = Vec::new();
    let mut a = old.iter().peekable();
    let mut b = new.iter().peekable();
    loop {
        let order = match (a.peek(), b.peek()) {
            (None, None) => break,
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (Some((x, _)), Some((y, _))) => x.cmp(y),
        };
        match order {
            std::cmp::Ordering::Less => {
                if let Some((path, entry)) = a.next() {
                    out.push(Change {
                        path: path.clone(),
                        kind: ChangeKind::Deleted,
                        old: Some(*entry),
                        new: None,
                    });
                }
            }
            std::cmp::Ordering::Greater => {
                if let Some((path, entry)) = b.next() {
                    out.push(Change {
                        path: path.clone(),
                        kind: ChangeKind::Added,
                        old: None,
                        new: Some(*entry),
                    });
                }
            }
            std::cmp::Ordering::Equal => {
                if let (Some((path, x)), Some((_, y))) = (a.next(), b.next()) {
                    if x.oid != y.oid || x.mode != y.mode {
                        let kind = if type_bits(x.mode) == type_bits(y.mode) {
                            ChangeKind::Modified
                        } else {
                            ChangeKind::TypeChange
                        };
                        out.push(Change {
                            path: path.clone(),
                            kind,
                            old: Some(*x),
                            new: Some(*y),
                        });
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestRepo, TestResult, build_index};
    use std::time::Duration;

    fn entry(mode: u32, byte: u8) -> SnapEntry {
        SnapEntry {
            mode,
            oid: Oid::from_bytes(&[byte; 20]).unwrap_or(Oid::ZERO),
            origin: Origin::Odb,
        }
    }

    fn snap(items: &[(&str, u32, u8)]) -> Snapshot {
        items
            .iter()
            .map(|(p, m, b)| (p.as_bytes().to_vec(), entry(*m, *b)))
            .collect()
    }

    #[test]
    fn changes_classifies_added_deleted_modified_and_typechange() -> TestResult {
        let old = snap(&[
            ("a", 0o100_644, 1),
            ("b", 0o100_644, 2),
            ("c", 0o100_644, 3),
            ("d", 0o100_644, 4),
            ("e", 0o100_644, 5),
        ]);
        let new = snap(&[
            ("a", 0o100_644, 1),
            ("c", 0o100_755, 3),
            ("d", 0o120_000, 4),
            ("e", 0o100_644, 9),
            ("f", 0o100_644, 6),
        ]);
        let result = changes(&old, &new);
        let summary: Vec<(String, char)> = result
            .iter()
            .map(|c| {
                (
                    String::from_utf8_lossy(&c.path).into_owned(),
                    c.kind.letter(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                ("b".into(), 'D'),
                ("c".into(), 'M'),
                ("d".into(), 'T'),
                ("e".into(), 'M'),
                ("f".into(), 'A')
            ]
        );
        assert!(changes(&old, &old).is_empty());
        assert!(changes(&Snapshot::new(), &Snapshot::new()).is_empty());
        assert_eq!(ChangeKind::Added.word(), "added");
        Ok(())
    }

    #[test]
    fn index_view_separates_conflicts_and_intent_to_add() -> TestResult {
        let repo = TestRepo::new()?;
        let o1 = repo.blob("1")?;
        let mut data = build_index(&[
            ("a", 0o100_644, o1, 1, 1, 0),
            ("conf", 0o100_644, o1, 1, 1, 0),
            ("conf2", 0o100_644, o1, 1, 1, 0),
        ]);
        // `conf` auf Stage 2 setzen: Flags des zweiten Eintrags.
        let first_len = (62 + 1 + 8) & !7;
        let second_flags = 12 + first_len + 60;
        data[second_flags] |= 0x20;
        let body = data[..data.len() - 20].to_vec();
        let mut sealed = body.clone();
        sealed.extend_from_slice(
            sha1::Digest::finalize(sha1::Digest::chain_update(sha1::Sha1::default(), &body))
                .as_slice(),
        );
        let index = crate::index::parse(&sealed)?;
        let view = from_index(&index, &Pathspec::all());
        assert_eq!(view.snapshot.len(), 2);
        assert!(view.conflicts.contains_key(b"conf".as_slice()));
        assert_eq!(view.conflicts[b"conf".as_slice()], vec![2]);
        assert!(!view.snapshot.contains_key(b"conf".as_slice()));
        let filtered = from_index(&index, &Pathspec::parse(&["a".to_owned()])?);
        assert_eq!(filtered.snapshot.len(), 1);
        assert!(filtered.conflicts.is_empty());
        Ok(())
    }

    fn ctx(repo: &Repo) -> WorktreeCtx<'_> {
        WorktreeCtx {
            repo,
            deadline: Instant::now() + Duration::from_secs(30),
            index_mtime: None,
        }
    }

    #[test]
    fn worktree_hashes_files_symlinks_and_reports_missing() -> TestResult {
        let repo = TestRepo::new()?;
        repo.write("f.txt", b"hello\n")?;
        repo.write("run.sh", b"#!/bin/sh\n")?;
        std::fs::set_permissions(
            repo.ws.join("run.sh"),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )?;
        std::os::unix::fs::symlink("f.txt", repo.ws.join("lnk"))?;
        std::fs::create_dir_all(repo.ws.join("adir"))?;
        std::os::unix::fs::symlink(&repo.outside, repo.ws.join("escape"))?;
        let opened = Repo::open(&repo.ws)?;
        let paths: Vec<(&[u8], Option<&IndexEntry>)> = vec![
            (b"f.txt", None),
            (b"run.sh", None),
            (b"lnk", None),
            (b"gone", None),
            (b"adir", None),
            (b"f.txt/x", None),
            (b"escape/y", None),
        ];
        let snapshot = worktree(&ctx(&opened), paths)?;
        assert_eq!(
            snapshot[b"f.txt".as_slice()].oid,
            hash_object("blob", b"hello\n")
        );
        assert_eq!(snapshot[b"f.txt".as_slice()].mode, 0o100_644);
        assert_eq!(snapshot[b"run.sh".as_slice()].mode, 0o100_755);
        assert_eq!(snapshot[b"lnk".as_slice()].mode, 0o120_000);
        assert_eq!(
            snapshot[b"lnk".as_slice()].oid,
            hash_object("blob", b"f.txt")
        );
        assert!(!snapshot.contains_key(b"gone".as_slice()));
        assert!(!snapshot.contains_key(b"adir".as_slice()));
        assert!(!snapshot.contains_key(b"f.txt/x".as_slice()));
        assert!(!snapshot.contains_key(b"escape/y".as_slice()));
        assert_eq!(snapshot.len(), 3);
        Ok(())
    }

    #[test]
    fn stat_cache_is_used_only_when_clean_and_not_racy() -> TestResult {
        let repo = TestRepo::new()?;
        repo.write("f.txt", b"changed!\n")?;
        let opened = Repo::open(&repo.ws)?;
        let stat = opened.scope.lstat(&rel_of(b"f.txt"))?;
        let stale_oid = hash_object("blob", b"old\n");
        let cached = IndexEntry {
            path: b"f.txt".to_vec(),
            mode: 0o100_644,
            oid: stale_oid,
            size: u32::try_from(stat.st_size).unwrap_or(0),
            mtime_sec: to_u32(stat.st_mtime),
            mtime_nsec: nsec_u32(stat.st_mtime_nsec),
            stage: 0,
            assume_valid: false,
            skip_worktree: false,
            intent_to_add: false,
        };
        let paths = || vec![(b"f.txt".as_slice(), Some(&cached))];
        // Index älter als die Datei: Stat passt, ist nicht racy → Cache gilt (liefert die Index-ID).
        let mut context = ctx(&opened);
        context.index_mtime = Some(cached.mtime_sec + 100);
        assert_eq!(
            worktree(&context, paths())?[b"f.txt".as_slice()].oid,
            stale_oid
        );
        // racy (Index nicht neuer) → wird gehasht und die Änderung erkannt.
        context.index_mtime = Some(cached.mtime_sec);
        assert_eq!(
            worktree(&context, paths())?[b"f.txt".as_slice()].oid,
            hash_object("blob", b"changed!\n")
        );
        // keine Index-Zeit bekannt → hashen.
        context.index_mtime = None;
        assert_eq!(
            worktree(&context, paths())?[b"f.txt".as_slice()].oid,
            hash_object("blob", b"changed!\n")
        );
        // Größe weicht ab → hashen.
        let mut resized = cached.clone();
        resized.size += 1;
        context.index_mtime = Some(cached.mtime_sec + 100);
        let result = worktree(&context, vec![(b"f.txt".as_slice(), Some(&resized))])?;
        assert_eq!(
            result[b"f.txt".as_slice()].oid,
            hash_object("blob", b"changed!\n")
        );
        Ok(())
    }

    #[test]
    fn expired_deadline_and_special_entries() -> TestResult {
        let repo = TestRepo::new()?;
        repo.write("f.txt", b"x")?;
        let opened = Repo::open(&repo.ws)?;
        let past = Instant::now()
            .checked_sub(Duration::from_secs(1))
            .ok_or(TestError::Missing("past"))?;
        let context = WorktreeCtx {
            repo: &opened,
            deadline: past,
            index_mtime: None,
        };
        assert!(worktree(&context, vec![(b"f.txt".as_slice(), None)]).is_err());
        let gitlink = IndexEntry {
            path: b"sub".to_vec(),
            mode: 0o160_000,
            oid: hash_object("commit", b"x"),
            size: 0,
            mtime_sec: 0,
            mtime_nsec: 0,
            stage: 0,
            assume_valid: false,
            skip_worktree: false,
            intent_to_add: false,
        };
        std::fs::create_dir_all(repo.ws.join("sub"))?;
        let snapshot = worktree(&ctx(&opened), vec![(b"sub".as_slice(), Some(&gitlink))])?;
        assert_eq!(snapshot[b"sub".as_slice()].mode, 0o160_000);
        let skipped = IndexEntry {
            skip_worktree: true,
            mode: 0o100_644,
            ..gitlink.clone()
        };
        let snapshot = worktree(&ctx(&opened), vec![(b"absent".as_slice(), Some(&skipped))])?;
        assert_eq!(snapshot[b"absent".as_slice()].oid, skipped.oid);
        Ok(())
    }
}
