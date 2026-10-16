//! Änderungen zu Dateidiffs und Ausgabe (`patch`, `stat`, `name_only`, `name_status`).
//!
//! # Verantwortung
//! [`render`] nimmt die [`Change`]-Liste zweier Momentaufnahmen und erzeugt
//! einen [`DiffReport`]: je Datei Pfad, Art, Zeilenzahlen und (für `patch`)
//! den Git-Unified-Diff (`diff --git`, `index`, `---`/`+++`, Hunks). Der
//! Hunk-Text kommt aus [`harw_tool_fsread::textdiff`] (Myers).
//!
//! # Sicherheit
//! - Pfade, die der Geheimnis-Denylist entsprechen
//!   ([`harw_tool_fsread::scope::is_secret_path`]), erscheinen mit Namen und
//!   Modi, ihr **Inhalt wird nie gelesen oder ausgegeben**.
//! - Binärdateien (NUL in den ersten 8000 Bytes) zeigen nur
//!   `Binary files … differ`.
//!
//! # Grenzen
//! - Je Seite höchstens [`MAX_SIDE_BYTES`] Bytes und [`MAX_LINES`] Zeilen;
//!   größere Dateien erscheinen als `too large to diff`.
//! - Höchstens `max_files` Dateien, Patch-Text höchstens [`PATCH_BUDGET`]
//!   Bytes; Überschreitungen setzen `truncated`/`patch_truncated`.
//! - Keine Umbenennungserkennung (ein Umbenennen ist `deleted` + `added`)
//!   und kein Funktionskontext hinter `@@`.

use crate::object::Kind;
use crate::odb::Odb;
use crate::oid::Oid;
use crate::snapshot::{Change, ChangeKind, Origin, SnapEntry, rel_of};
use harw_tool_fsread::budget::{Collector, clip_text};
use harw_tool_fsread::scope::{Scope, is_secret_path};
use harw_tool_fsread::textdiff::{DEFAULT_MAX_D, count, diff_lines, split_lines, unified};
use serde_json::{Value, json};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// Größte lesbare Seite.
pub const MAX_SIDE_BYTES: usize = 1024 * 1024;

/// Höchstzahl Zeilen je Seite.
pub const MAX_LINES: usize = 20_000;

/// Platz für Patch-Text in der Antwort.
pub const PATCH_BUDGET: usize = 40 * 1024;

/// Platz für die Dateiliste in der Antwort.
pub const FILES_BUDGET: usize = 12 * 1024;

/// Standard und Maximum der Dateizahl.
pub const DEFAULT_MAX_FILES: usize = 200;
/// Obergrenze der Dateizahl.
pub const HARD_MAX_FILES: usize = 2000;

/// Größter Kontext.
pub const MAX_CONTEXT: usize = 50;

/// Ausgabeformat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Dateiliste mit Zählern und Patch-Text.
    Patch,
    /// Dateiliste mit Zählern.
    Stat,
    /// Nur Pfade.
    NameOnly,
    /// Art und Pfad.
    NameStatus,
}

impl Format {
    /// Alle erlaubten Namen (für Fehlermeldungen/Allowlist).
    pub const NAMES: &'static [&'static str] = &["patch", "stat", "name_only", "name_status"];

    /// Parst den Namen.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "patch" => Some(Self::Patch),
            "stat" => Some(Self::Stat),
            "name_only" => Some(Self::NameOnly),
            "name_status" => Some(Self::NameStatus),
            _ => None,
        }
    }
}

/// Optionen für [`render`].
#[derive(Debug, Clone, Copy)]
pub struct RenderOpts {
    /// Format.
    pub format: Format,
    /// Kontextzeilen.
    pub context: usize,
    /// Leerraum ignorieren (`-w`).
    pub ignore_whitespace: bool,
    /// Höchstzahl Dateien.
    pub max_files: usize,
}

/// Ergebnis von [`render`].
#[derive(Debug)]
pub struct DiffReport {
    /// JSON-Einträge je Datei.
    pub files: Vec<Value>,
    /// Gesamtzahl geänderter Dateien (auch wenn nicht alle gelistet sind).
    pub total_files: usize,
    /// Summe hinzugefügter Zeilen (nur berechnete Dateien).
    pub additions: usize,
    /// Summe entfernter Zeilen (nur berechnete Dateien).
    pub deletions: usize,
    /// Patch-Text.
    pub patch: String,
    /// Die Dateiliste wurde gekürzt.
    pub files_truncated: bool,
    /// Der Patch-Text wurde gekürzt oder Dateien blieben ohne Patch.
    pub patch_truncated: bool,
}

impl DiffReport {
    /// Fügt die Felder in ein JSON-Objekt ein.
    pub fn into_json(self, format: Format, data: &mut Value) {
        data["format"] = json!(match format {
            Format::Patch => "patch",
            Format::Stat => "stat",
            Format::NameOnly => "name_only",
            Format::NameStatus => "name_status",
        });
        data["total_files"] = json!(self.total_files);
        data["files"] = Value::Array(self.files);
        if matches!(format, Format::Patch | Format::Stat) {
            data["additions"] = json!(self.additions);
            data["deletions"] = json!(self.deletions);
        }
        if format == Format::Patch {
            data["patch"] = json!(self.patch);
        }
        data["files_truncated"] = json!(self.files_truncated);
        data["patch_truncated"] = json!(self.patch_truncated);
    }
}

/// Inhalt einer Seite.
enum Content {
    Bytes(Vec<u8>),
    Absent,
    Secret,
    TooLarge,
}

/// Liest Inhalte aus der Objektdatenbank und dem Arbeitsverzeichnis.
pub struct Reader<'a, 'o> {
    /// Objektdatenbank.
    pub odb: &'a Odb<'o>,
    /// Zugriff auf das Arbeitsverzeichnis.
    pub scope: &'a Scope,
}

impl Reader<'_, '_> {
    fn read(&self, path: &[u8], entry: Option<&SnapEntry>) -> Result<Content, String> {
        let Some(entry) = entry else {
            return Ok(Content::Absent);
        };
        if is_secret_path(Path::new(std::ffi::OsStr::from_bytes(path))) {
            return Ok(Content::Secret);
        }
        if entry.mode & 0o170_000 == 0o160_000 {
            return Ok(Content::Bytes(
                format!("Subproject commit {}\n", entry.oid).into_bytes(),
            ));
        }
        match entry.origin {
            Origin::Odb => {
                let object = self.odb.read_kind(&entry.oid, Kind::Blob)?;
                if object.data.len() > MAX_SIDE_BYTES {
                    return Ok(Content::TooLarge);
                }
                Ok(Content::Bytes(object.data.clone()))
            }
            Origin::Worktree => {
                let rel = rel_of(path);
                if entry.mode & 0o170_000 == 0o120_000 {
                    let target = self.scope.read_link(&rel).map_err(|e| e.to_string())?;
                    return Ok(Content::Bytes(target.as_os_str().as_bytes().to_vec()));
                }
                let mut file = self.scope.open_read(&rel).map_err(|e| e.to_string())?;
                let (bytes, more) =
                    harw_tool_fsread::io::read_prefix(&mut file, MAX_SIDE_BYTES as u64)
                        .map_err(|e| e.to_string())?;
                if more {
                    Ok(Content::TooLarge)
                } else {
                    Ok(Content::Bytes(bytes))
                }
            }
        }
    }
}

fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|b| *b == 0)
}

fn short(oid: Option<&SnapEntry>) -> String {
    oid.map_or_else(
        || "0000000".to_owned(),
        |e| e.oid.hex().chars().take(7).collect(),
    )
}

fn key(line: &str, ignore_ws: bool) -> String {
    if ignore_ws {
        line.chars().filter(|c| !c.is_whitespace()).collect()
    } else {
        line.to_owned()
    }
}

struct FileOut {
    entry: Value,
    patch: String,
    additions: usize,
    deletions: usize,
    /// Nur Leerraum-Unterschiede bei `-w`: Datei weglassen.
    skip: bool,
}

fn path_text(path: &[u8]) -> String {
    String::from_utf8_lossy(path).into_owned()
}

fn one_file(
    reader: &Reader<'_, '_>,
    change: &Change,
    opts: &RenderOpts,
    with_patch: bool,
) -> Result<FileOut, String> {
    let name = path_text(&change.path);
    let mut entry = json!({
        "path": name,
        "status": change.kind.word(),
        "letter": change.kind.letter().to_string(),
    });
    if let Some(old) = &change.old {
        entry["old_mode"] = json!(format!("{:06o}", old.mode));
    }
    if let Some(new) = &change.new {
        entry["new_mode"] = json!(format!("{:06o}", new.mode));
    }
    let mut out = FileOut {
        entry,
        patch: String::new(),
        additions: 0,
        deletions: 0,
        skip: false,
    };
    let old_name = if change.old.is_some() {
        format!("a/{name}")
    } else {
        "/dev/null".to_owned()
    };
    let new_name = if change.new.is_some() {
        format!("b/{name}")
    } else {
        "/dev/null".to_owned()
    };
    let mut header = format!("diff --git a/{name} b/{name}\n");
    match (&change.old, &change.new) {
        (None, Some(new)) => header.push_str(&format!("new file mode {:06o}\n", new.mode)),
        (Some(old), None) => header.push_str(&format!("deleted file mode {:06o}\n", old.mode)),
        (Some(old), Some(new)) if old.mode != new.mode => {
            header.push_str(&format!(
                "old mode {:06o}\nnew mode {:06o}\n",
                old.mode, new.mode
            ));
        }
        _ => {}
    }
    let same_mode = matches!((&change.old, &change.new), (Some(o), Some(n)) if o.mode == n.mode);
    let oid_changed = change.old.map(|e| e.oid) != change.new.map(|e| e.oid);
    if oid_changed || change.old.is_none() || change.new.is_none() {
        let mode_suffix = if same_mode {
            change
                .new
                .map_or(String::new(), |n| format!(" {:06o}", n.mode))
        } else {
            String::new()
        };
        header.push_str(&format!(
            "index {}..{}{mode_suffix}\n",
            short(change.old.as_ref()),
            short(change.new.as_ref())
        ));
    }
    if !with_patch {
        return Ok(out);
    }
    let old = reader.read(&change.path, change.old.as_ref())?;
    let new = reader.read(&change.path, change.new.as_ref())?;
    if matches!(old, Content::Secret) || matches!(new, Content::Secret) {
        out.entry["omitted"] = json!("secret path: contents are never shown");
        out.patch =
            format!("{header}(contents omitted: the path matches the secret-file denylist)\n");
        return Ok(out);
    }
    if matches!(old, Content::TooLarge) || matches!(new, Content::TooLarge) {
        out.entry["omitted"] = json!(format!("file is larger than {MAX_SIDE_BYTES} bytes"));
        out.patch = format!("{header}(file too large to diff)\n");
        return Ok(out);
    }
    let (old_bytes, new_bytes) = (
        match old {
            Content::Bytes(bytes) => bytes,
            _ => Vec::new(),
        },
        match new {
            Content::Bytes(bytes) => bytes,
            _ => Vec::new(),
        },
    );
    if looks_binary(&old_bytes) || looks_binary(&new_bytes) {
        out.entry["binary"] = json!(true);
        out.patch = format!("{header}Binary files {old_name} and {new_name} differ\n");
        return Ok(out);
    }
    let old_text = String::from_utf8_lossy(&old_bytes).into_owned();
    let new_text = String::from_utf8_lossy(&new_bytes).into_owned();
    let old_lines = split_lines(&old_text);
    let new_lines = split_lines(&new_text);
    if old_lines.len() > MAX_LINES || new_lines.len() > MAX_LINES {
        out.entry["omitted"] = json!(format!("more than {MAX_LINES} lines"));
        out.patch = format!("{header}(file has too many lines to diff)\n");
        return Ok(out);
    }
    let old_keys: Vec<String> = old_lines
        .iter()
        .map(|l| key(l, opts.ignore_whitespace))
        .collect();
    let new_keys: Vec<String> = new_lines
        .iter()
        .map(|l| key(l, opts.ignore_whitespace))
        .collect();
    let Some(ops) = diff_lines(&old_keys, &new_keys, DEFAULT_MAX_D) else {
        out.entry["omitted"] = json!(format!("changes span more than {DEFAULT_MAX_D} lines"));
        out.patch = format!("{header}(diff too large to render)\n");
        return Ok(out);
    };
    let (additions, deletions) = count(&ops);
    out.additions = additions;
    out.deletions = deletions;
    out.entry["additions"] = json!(additions);
    out.entry["deletions"] = json!(deletions);
    if additions + deletions == 0 {
        // nur Modus-/Leerraumänderung
        if same_mode && opts.ignore_whitespace && change.kind == ChangeKind::Modified {
            out.skip = true;
        }
        out.patch = header;
        return Ok(out);
    }
    let (body, _) = unified(
        &old_name,
        &new_name,
        &old_lines,
        &new_lines,
        &ops,
        opts.context.min(MAX_CONTEXT),
    );
    out.patch = format!("{header}{body}");
    Ok(out)
}

/// Erzeugt den Bericht für `changes`.
///
/// # Errors
/// Meldung bei Lesefehlern (fehlende Objekte, Arbeitsverzeichnis).
pub fn render(
    reader: &Reader<'_, '_>,
    changes: &[Change],
    opts: &RenderOpts,
) -> Result<DiffReport, String> {
    let with_patch = matches!(opts.format, Format::Patch | Format::Stat);
    let max_files = opts.max_files.clamp(1, HARD_MAX_FILES);
    let mut files = Collector::with_budget(max_files, FILES_BUDGET);
    let mut report = DiffReport {
        files: Vec::new(),
        total_files: 0,
        additions: 0,
        deletions: 0,
        patch: String::new(),
        files_truncated: false,
        patch_truncated: false,
    };
    let mut patch_room = PATCH_BUDGET;
    // Git zeigt einen Typwechsel im Patch als Löschung plus Neuanlage (im
    // Stat/numstat als eine Zeile).
    let expanded: Vec<Change> = changes
        .iter()
        .flat_map(|change| {
            if opts.format == Format::Patch && change.kind == ChangeKind::TypeChange {
                vec![
                    Change {
                        path: change.path.clone(),
                        kind: ChangeKind::Deleted,
                        old: change.old,
                        new: None,
                    },
                    Change {
                        path: change.path.clone(),
                        kind: ChangeKind::Added,
                        old: None,
                        new: change.new,
                    },
                ]
            } else {
                vec![change.clone()]
            }
        })
        .collect();
    for change in &expanded {
        let out = one_file(reader, change, opts, with_patch)?;
        if out.skip {
            continue;
        }
        report.total_files += 1;
        report.additions += out.additions;
        report.deletions += out.deletions;
        let mut entry = out.entry;
        match opts.format {
            Format::NameOnly => entry = json!(path_text(&change.path)),
            Format::NameStatus => {
                entry = json!({"status": change.kind.letter().to_string(), "path": path_text(&change.path)})
            }
            Format::Patch | Format::Stat => {}
        }
        if opts.format == Format::Patch && !out.patch.is_empty() {
            if patch_room == 0 {
                report.patch_truncated = true;
            } else {
                let (clipped, was_clipped) = clip_text(&out.patch, patch_room);
                patch_room -= clipped.len().min(patch_room);
                report.patch.push_str(&clipped);
                if was_clipped {
                    report.patch_truncated = true;
                    patch_room = 0;
                }
            }
        }
        if !files.push(entry) {
            report.files_truncated = true;
        }
    }
    report.files_truncated |= files.truncated();
    report.files = files.into_items();
    Ok(report)
}

/// Kurzform einer Objekt-ID für Meldungen.
#[must_use]
pub fn short_oid(oid: &Oid) -> String {
    oid.short()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oid::hash_object;
    use crate::repo::Repo;
    use crate::snapshot::changes;
    use crate::test_support::{TestRepo, TestResult};
    use std::collections::BTreeMap;

    fn entry(oid: Oid, mode: u32) -> SnapEntry {
        SnapEntry {
            mode,
            oid,
            origin: Origin::Odb,
        }
    }

    struct Fx {
        repo: TestRepo,
    }

    fn side(
        repo: &TestRepo,
        items: &[(&str, &str, u32)],
    ) -> TestResult<BTreeMap<Vec<u8>, SnapEntry>> {
        let mut map = BTreeMap::new();
        for (path, text, mode) in items {
            let oid = repo.blob(text)?;
            map.insert(path.as_bytes().to_vec(), entry(oid, *mode));
        }
        Ok(map)
    }

    fn run(
        fx: &Fx,
        old: &[(&str, &str, u32)],
        new: &[(&str, &str, u32)],
        format: Format,
        ws: bool,
    ) -> TestResult<DiffReport> {
        let opened = Repo::open(&fx.repo.ws)?;
        let odb = Odb::new(&opened);
        let reader = Reader {
            odb: &odb,
            scope: &opened.scope,
        };
        let old = side(&fx.repo, old)?;
        let new = side(&fx.repo, new)?;
        let opts = RenderOpts {
            format,
            context: 3,
            ignore_whitespace: ws,
            max_files: 100,
        };
        Ok(render(&reader, &changes(&old, &new), &opts)?)
    }

    #[test]
    fn modified_added_deleted_and_mode_change() -> TestResult {
        let fx = Fx {
            repo: TestRepo::new()?,
        };
        let report = run(
            &fx,
            &[
                ("a.txt", "one\ntwo\nthree\n", 0o100_644),
                ("gone.txt", "bye\n", 0o100_644),
                ("x.sh", "run\n", 0o100_644),
            ],
            &[
                ("a.txt", "one\n2\nthree\n", 0o100_644),
                ("new.txt", "hi\n", 0o100_644),
                ("x.sh", "run\n", 0o100_755),
            ],
            Format::Patch,
            false,
        )?;
        assert_eq!(report.total_files, 4);
        assert_eq!((report.additions, report.deletions), (2, 2));
        let a_old = hash_object("blob", b"one\ntwo\nthree\n").short();
        assert!(
            report.patch.contains(&format!(
                "diff --git a/a.txt b/a.txt\nindex {}",
                &a_old[..7]
            )),
            "{}",
            report.patch
        );
        assert!(
            report
                .patch
                .contains("--- a/a.txt\n+++ b/a.txt\n@@ -1,3 +1,3 @@\n one\n-two\n+2\n three\n"),
            "{}",
            report.patch
        );
        assert!(report.patch.contains("new file mode 100644\n"));
        assert!(
            report
                .patch
                .contains("--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1 @@\n+hi\n"),
            "{}",
            report.patch
        );
        assert!(report.patch.contains("deleted file mode 100644\n"));
        assert!(
            report
                .patch
                .contains("--- a/gone.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-bye\n"),
            "{}",
            report.patch
        );
        assert!(report.patch.contains("old mode 100644\nnew mode 100755\n"));
        assert!(!report.patch_truncated);
        assert!(!report.files_truncated);
        Ok(())
    }

    #[test]
    fn stat_and_name_formats_do_not_need_patch_text() -> TestResult {
        let fx = Fx {
            repo: TestRepo::new()?,
        };
        let old = [("a", "1\n", 0o100_644)];
        let new = [("a", "1\n2\n", 0o100_644), ("b", "x\n", 0o100_644)];
        let stat = run(&fx, &old, &new, Format::Stat, false)?;
        assert_eq!(stat.files[0]["additions"], 1);
        assert!(stat.patch.is_empty());
        let names = run(&fx, &old, &new, Format::NameOnly, false)?;
        assert_eq!(names.files, vec![json!("a"), json!("b")]);
        let status = run(&fx, &old, &new, Format::NameStatus, false)?;
        assert_eq!(status.files[1], json!({"status": "A", "path": "b"}));
        Ok(())
    }

    #[test]
    fn binary_secret_symlink_and_gitlink_sides() -> TestResult {
        let fx = Fx {
            repo: TestRepo::new()?,
        };
        let bin = fx.repo.write_loose("blob", b"\0\x01binary")?;
        let bin2 = fx.repo.write_loose("blob", b"\0\x02binary")?;
        let secret = fx.repo.blob("API_KEY=hunter2\n")?;
        let secret2 = fx.repo.blob("API_KEY=other-secret\n")?;
        let target = fx.repo.blob("target-file")?;
        let sub = hash_object("commit", b"submodule head");
        let old: BTreeMap<Vec<u8>, SnapEntry> = [
            (b"b.bin".to_vec(), entry(bin, 0o100_644)),
            (b".env".to_vec(), entry(secret, 0o100_644)),
            (b"cfg/id_rsa".to_vec(), entry(secret, 0o100_644)),
        ]
        .into_iter()
        .collect();
        let new: BTreeMap<Vec<u8>, SnapEntry> = [
            (b"b.bin".to_vec(), entry(bin2, 0o100_644)),
            (b".env".to_vec(), entry(secret2, 0o100_644)),
            (b"cfg/id_rsa".to_vec(), entry(secret2, 0o100_644)),
            (b"lnk".to_vec(), entry(target, 0o120_000)),
            (b"sub".to_vec(), entry(sub, 0o160_000)),
        ]
        .into_iter()
        .collect();
        let opened = Repo::open(&fx.repo.ws)?;
        let odb = Odb::new(&opened);
        let reader = Reader {
            odb: &odb,
            scope: &opened.scope,
        };
        let opts = RenderOpts {
            format: Format::Patch,
            context: 3,
            ignore_whitespace: false,
            max_files: 100,
        };
        let report = render(&reader, &changes(&old, &new), &opts)?;
        assert!(
            report
                .patch
                .contains("Binary files a/b.bin and b/b.bin differ"),
            "{}",
            report.patch
        );
        assert!(!report.patch.contains("hunter2"), "{}", report.patch);
        assert!(!report.patch.contains("other-secret"), "{}", report.patch);
        assert!(report.patch.contains("diff --git a/.env b/.env"));
        assert!(report.patch.contains("contents omitted"));
        assert!(
            report
                .patch
                .contains("+target-file\n\\ No newline at end of file\n"),
            "{}",
            report.patch
        );
        assert!(
            report.patch.contains("+Subproject commit "),
            "{}",
            report.patch
        );
        let secrets = report
            .files
            .iter()
            .filter(|f| f.get("omitted").is_some())
            .count();
        assert_eq!(secrets, 2);
        Ok(())
    }

    #[test]
    fn whitespace_only_changes_vanish_with_w_and_limits_truncate() -> TestResult {
        let fx = Fx {
            repo: TestRepo::new()?,
        };
        let ws = run(
            &fx,
            &[("a", "x  y\n", 0o100_644)],
            &[("a", "x y\n", 0o100_644)],
            Format::Patch,
            true,
        )?;
        assert_eq!(ws.total_files, 0);
        let plain = run(
            &fx,
            &[("a", "x  y\n", 0o100_644)],
            &[("a", "x y\n", 0o100_644)],
            Format::Patch,
            false,
        )?;
        assert_eq!(plain.total_files, 1);
        // viele Dateien: Liste und Patch werden gekürzt, die Zahl bleibt ehrlich
        let big = "line of text that fills up the patch budget quickly\n".repeat(400);
        let names: Vec<String> = (0..30).map(|i| format!("file{i:02}.txt")).collect();
        let items: Vec<(&str, &str, u32)> = names
            .iter()
            .map(|n| (n.as_str(), big.as_str(), 0o100_644))
            .collect();
        let report = run(&fx, &[], &items, Format::Patch, false)?;
        assert_eq!(report.total_files, 30);
        assert!(report.patch_truncated);
        assert!(report.patch.len() <= PATCH_BUDGET, "{}", report.patch.len());
        let opened = Repo::open(&fx.repo.ws)?;
        let odb = Odb::new(&opened);
        let reader = Reader {
            odb: &odb,
            scope: &opened.scope,
        };
        let many: Vec<(&str, &str, u32)> = items.clone();
        let new = side(&fx.repo, &many)?;
        let opts = RenderOpts {
            format: Format::NameOnly,
            context: 3,
            ignore_whitespace: false,
            max_files: 5,
        };
        let limited = render(&reader, &changes(&BTreeMap::new(), &new), &opts)?;
        assert_eq!(limited.files.len(), 5);
        assert!(limited.files_truncated);
        assert_eq!(limited.total_files, 30);
        Ok(())
    }

    #[test]
    fn oversized_and_missing_blobs() -> TestResult {
        let fx = Fx {
            repo: TestRepo::new()?,
        };
        let huge = fx
            .repo
            .write_loose("blob", &vec![b'a'; MAX_SIDE_BYTES + 1])?;
        let ghost = hash_object("blob", b"never stored");
        let new: BTreeMap<Vec<u8>, SnapEntry> = [(b"huge".to_vec(), entry(huge, 0o100_644))]
            .into_iter()
            .collect();
        let opened = Repo::open(&fx.repo.ws)?;
        let odb = Odb::new(&opened);
        let reader = Reader {
            odb: &odb,
            scope: &opened.scope,
        };
        let opts = RenderOpts {
            format: Format::Patch,
            context: 3,
            ignore_whitespace: false,
            max_files: 10,
        };
        let report = render(&reader, &changes(&BTreeMap::new(), &new), &opts)?;
        assert!(report.patch.contains("too large"));
        let missing: BTreeMap<Vec<u8>, SnapEntry> = [(b"m".to_vec(), entry(ghost, 0o100_644))]
            .into_iter()
            .collect();
        assert!(render(&reader, &changes(&BTreeMap::new(), &missing), &opts).is_err());
        assert_eq!(short_oid(&ghost).len(), 7);
        assert_eq!(Format::from_name("patch"), Some(Format::Patch));
        assert_eq!(Format::from_name("nope"), None);
        Ok(())
    }
}
