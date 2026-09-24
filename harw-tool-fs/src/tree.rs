//! Gemeinsame, symlinkfeste Öffnungs- und Walk-Basis der fs-Tools (W1-02).
//!
//! # Verantwortung
//! - [`Workspace`]: Deskriptor der Workspace-Wurzel; alle Pfade der Tools
//!   werden **relativ zu diesem Deskriptor** über
//!   [`harw_fsutil::open_beneath`] geöffnet (`RESOLVE_BENEATH |
//!   RESOLVE_NO_SYMLINKS`). Kein Pfadglied darf ein Symlink sein.
//!   Den vom Modell genannten **Startpfad** lösen die Tools vorher über
//!   [`crate::symlink`] auf (Symlinks nach innen frei, nach außen nur mit
//!   Freigabe, Schreiben nur nach innen); geöffnet wird danach wieder strikt
//!   symlinkfrei (Details und TOCTOU-Argument dort).
//! - [`walk_tree`]: begrenzter Tiefensuche-Walk auf Basis von
//!   [`harw_fsutil::walk_beneath`]. Jedes Verzeichnis wird einzeln mit
//!   `max_depth = 1` gelesen; Unterverzeichnisse werden relativ zum
//!   Eltern-Deskriptor geöffnet. Dadurch sind Ausschlüsse (`target`,
//!   `.gitignore`) echte Beschneidungen, und Symlinks werden nie gefolgt.
//! - Harte Grenzen gegen DoS: [`HARD_MAX_RESULTS`], [`HARD_MAX_DEPTH`],
//!   [`HARD_MAX_ENTRIES`], [`WALK_DEADLINE`], [`MAX_SCAN_FILE_BYTES`],
//!   [`MAX_OUTPUT_BYTES`]. Der Abbruchgrund wird als [`StopReason`] gemeldet.
//!
//! # Verzeichnis-Pfade für `walk_beneath`
//! `walk_beneath` erwartet einen Pfad (nur das letzte Glied ist symlinkfest).
//! Unter Linux wird deshalb der Pfad `/proc/self/fd/<fd>/.` eines bereits
//! symlinkfrei geöffneten Verzeichnis-Deskriptors übergeben: der Kernel löst
//! den Magic-Link auf genau dieses Verzeichnis auf, auch wenn es inzwischen
//! umbenannt oder durch einen Symlink ersetzt wurde. Vor der Nutzung wird per
//! `(st_dev, st_ino)` geprüft, dass der Pfad wirklich auf den Deskriptor
//! zeigt. Ohne nutzbares `/proc` fällt der Walk auf `<wurzel>/<rel>` zurück;
//! dann bleibt ein Restfenster, in dem ein getauschtes **Zwischen**glied die
//! Namensliste eines fremden Verzeichnisses liefern kann. Dateiinhalte und
//! Unterverzeichnisse werden auch dann ausschließlich über `open_beneath`
//! relativ zum echten Eltern-Deskriptor geöffnet.
//!
//! # `.gitignore`
//! Wird je Verzeichnis aus `.gitignore` und `.ignore` (in dieser Reihenfolge,
//! spätere Zeilen gewinnen) über `open_beneath` gelesen und mit
//! [`ignore::gitignore::GitignoreBuilder`] ausgewertet. Tiefere Dateien haben
//! Vorrang. Berücksichtigt werden nur Dateien zwischen Workspace-Wurzel und
//! dem jeweiligen Verzeichnis — keine Vorfahren außerhalb des Workspace,
//! keine globale Git-Konfiguration des Hosts, kein `.git/info/exclude`.

use harw_fsutil::{EntryType, OpenMode, WalkEntry, WalkLimits, WalkStop, open_beneath};
use ignore::Match;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read};
use std::ops::ControlFlow;
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

/// Harte Obergrenze für Treffer/Ergebnisse je Aufruf (unabhängig von Argumenten).
pub const HARD_MAX_RESULTS: usize = 1000;

/// Harte Obergrenze der Walk-Tiefe (Kinder des Startverzeichnisses = 1).
pub const HARD_MAX_DEPTH: usize = 32;

/// Harte Obergrenze gelesener Verzeichniseinträge je Walk.
pub const HARD_MAX_ENTRIES: usize = 50_000;

/// Zeitbudget eines Walks inklusive Datei-Scans.
pub const WALK_DEADLINE: Duration = Duration::from_secs(10);

/// Größere Dateien werden von `fs.search`/`fs.grep` übersprungen.
pub const MAX_SCAN_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// Obergrenze der Nutzlast eines Tool-Ergebnisses (wie `fs.read`).
pub const MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// Obergrenze einer einzelnen ausgegebenen Zeile (längere werden gekürzt).
pub const MAX_LINE_BYTES: usize = 1024;

/// Obergrenze einer gelesenen `.gitignore`/`.ignore`-Datei.
const MAX_IGNORE_FILE_BYTES: u64 = 1024 * 1024;

/// Namen der Ignore-Dateien je Verzeichnis, in Auswertungsreihenfolge.
const IGNORE_FILE_NAMES: [&str; 2] = [".gitignore", ".ignore"];

/// Grund, aus dem ein Ergebnis unvollständig ist (Feld `stopped`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// Treffer-/Ergebnisgrenze erreicht, es gab weitere Treffer.
    ResultLimit,
    /// Ausgabegrenze ([`MAX_OUTPUT_BYTES`]) erreicht.
    OutputLimit,
    /// Mindestens ein Verzeichnis lag jenseits der Tiefengrenze.
    DepthLimit,
    /// Eintragsgrenze erreicht.
    EntryLimit,
    /// Zeitbudget ([`WALK_DEADLINE`]) abgelaufen.
    Deadline,
}

impl StopReason {
    /// Stabile, maschinenlesbare Bezeichnung für das Ergebnisfeld `stopped`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ResultLimit => "result_limit",
            Self::OutputLimit => "output_limit",
            Self::DepthLimit => "depth_limit",
            Self::EntryLimit => "entry_limit",
            Self::Deadline => "deadline",
        }
    }
}

/// Prüft einen vom Modell gelieferten Pfad lexikalisch und normalisiert ihn.
///
/// Erlaubt sind nur normale Komponenten und `.`; abschließende `/` werden
/// verworfen. Ein leeres Ergebnis bezeichnet die Workspace-Wurzel.
///
/// # Errors
/// Meldungstext für leere, absolute oder `..`-haltige Pfade.
pub fn normalize_relative(input: &str) -> Result<PathBuf, String> {
    if input.is_empty() {
        return Err("leerer Pfad ist nicht erlaubt".to_owned());
    }
    let mut normalized = PathBuf::new();
    for component in Path::new(input).components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(format!("'{input}': `..` ist nicht erlaubt"));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!("'{input}': absolute Pfade sind nicht erlaubt"));
            }
        }
    }
    Ok(normalized)
}

/// `rel` in der Form, die [`open_beneath`] erwartet (`.` für die Wurzel).
fn beneath_arg(rel: &Path) -> &Path {
    if rel.as_os_str().is_empty() {
        Path::new(".")
    } else {
        rel
    }
}

/// `InvalidInput`-Fehler mit statischer Meldung.
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// Geöffnete Workspace-Wurzel.
#[derive(Debug)]
pub struct Workspace {
    /// Kanonische Wurzel (nur für den `/proc`-losen Rückfallpfad).
    root: PathBuf,
    /// Deskriptor der Wurzel; Basis aller `open_beneath`-Aufrufe.
    dir: File,
}

impl Workspace {
    /// Öffnet die kanonische Workspace-Wurzel (letztes Glied kein Symlink).
    ///
    /// # Errors
    /// Fehler von [`harw_fsutil::open_dir_nofollow`].
    pub fn open(canonical_root: &Path) -> io::Result<Self> {
        let fd = harw_fsutil::open_dir_nofollow(canonical_root)?;
        Ok(Self {
            root: canonical_root.to_path_buf(),
            dir: File::from(fd),
        })
    }

    /// Öffnet `rel` (Datei oder Verzeichnis) symlinkfrei unterhalb der Wurzel.
    ///
    /// # Errors
    /// Fehler von [`open_beneath`] (z. B. `ELOOP` bei Symlinks).
    pub fn open_any(&self, rel: &Path) -> io::Result<File> {
        open_beneath(self.dir.as_fd(), beneath_arg(rel), OpenMode::read_only())
    }

    /// Kanonischer Pfad dieser Wurzel.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Metadaten des geöffneten Wurzel-Deskriptors (`fstat`).
    ///
    /// # Errors
    /// Fehler von `fstat`.
    pub fn root_metadata(&self) -> io::Result<std::fs::Metadata> {
        self.dir.metadata()
    }

    /// Öffnet das Verzeichnis `rel` symlinkfrei unterhalb der Wurzel.
    ///
    /// # Errors
    /// Wie [`Self::open_any`]; `InvalidInput`, wenn `rel` kein Verzeichnis ist.
    pub fn open_dir(&self, rel: &Path) -> io::Result<File> {
        let file = self.open_any(rel)?;
        if file.metadata()?.is_dir() {
            Ok(file)
        } else {
            Err(invalid("kein Verzeichnis"))
        }
    }

    /// Pfad, unter dem das geöffnete Verzeichnis `dir` (relativ zur Wurzel:
    /// `rel`) für pfadbasierte APIs erreichbar ist: `/proc/self/fd/<fd>`,
    /// falls nachweislich identisch, sonst `<wurzel>/<rel>`.
    ///
    /// Das letzte Glied von `/proc/self/fd/<fd>` ist ein Magic-Link; es darf
    /// nur mit folgenden Öffnern (z. B. als Elternverzeichnis) genutzt werden.
    #[must_use]
    pub fn dir_path(&self, dir: &File, rel: &Path) -> PathBuf {
        proc_fd_path(dir).unwrap_or_else(|| self.root.join(rel))
    }

    /// Wie [`Self::dir_path`], aber für Öffner mit `O_NOFOLLOW` auf dem
    /// letzten Glied (z. B. `walk_beneath`): `/proc/self/fd/<fd>/.`.
    fn walk_path(&self, dir: &File, rel: &Path) -> PathBuf {
        match proc_fd_path(dir) {
            Some(path) => path.join("."),
            None => self.root.join(rel),
        }
    }
}

/// `/proc/self/fd/<fd>`, wenn der Pfad nachweislich auf `dir` zeigt.
fn proc_fd_path(dir: &File) -> Option<PathBuf> {
    let path = PathBuf::from(format!("/proc/self/fd/{}", dir.as_raw_fd()));
    let via_proc = std::fs::metadata(&path).ok()?;
    let direct = dir.metadata().ok()?;
    let same =
        via_proc.is_dir() && via_proc.dev() == direct.dev() && via_proc.ino() == direct.ino();
    same.then_some(path)
}

/// Öffnet die reguläre Datei `name` direkt im Verzeichnis `dir`.
///
/// # Errors
/// Fehler von [`open_beneath`]; `InvalidInput`, wenn keine reguläre Datei.
pub fn open_file_in(dir: &File, name: &OsStr) -> io::Result<File> {
    let file = open_beneath(dir.as_fd(), Path::new(name), OpenMode::read_only())?;
    if file.metadata()?.is_file() {
        Ok(file)
    } else {
        Err(invalid("keine reguläre Datei"))
    }
}

/// Liest eine geöffnete Datei vollständig, wenn sie höchstens `max` Bytes hat.
///
/// Die Grenze wird beim Lesen erzwungen (`take(max + 1)`), nicht nur über
/// `fstat` — eine wachsende Datei sprengt den Speicher also nicht.
///
/// # Errors
/// Lesefehler.
pub fn read_bounded(file: &File, max: u64) -> io::Result<Option<Vec<u8>>> {
    if file.metadata()?.len() > max {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    file.take(max.saturating_add(1)).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max {
        return Ok(None);
    }
    Ok(Some(bytes))
}

/// Kürzt `line` UTF-8-sicher auf höchstens [`MAX_LINE_BYTES`] (plus `…`).
#[must_use]
pub fn truncate_line(line: &str) -> String {
    if line.len() <= MAX_LINE_BYTES {
        return line.to_owned();
    }
    let mut cut = MAX_LINE_BYTES;
    while !line.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &line[..cut])
}

/// Liest die direkten Kinder von `dir` (sortiert, nie Symlinks folgend).
///
/// Liefert höchstens `max_entries` Einträge; Fehler-Items werden verworfen,
/// zählen aber gegen die Grenze. Der Abbruchgrund ist `EntryLimit` oder
/// `Deadline` (die Tiefengrenze des Einzel-Walks wird nicht gemeldet).
///
/// # Errors
/// Fehler beim Öffnen oder Einlesen des Verzeichnisses.
pub fn read_dir_limited(
    workspace: &Workspace,
    dir: &File,
    rel: &Path,
    max_entries: usize,
    deadline: Instant,
) -> io::Result<(Vec<WalkEntry>, Option<StopReason>)> {
    let limits = WalkLimits {
        max_depth: 1,
        max_entries,
        deadline: Some(deadline),
    };
    let mut walk = harw_fsutil::walk_beneath(&workspace.walk_path(dir, rel), limits)?;
    let entries: Vec<WalkEntry> = walk.by_ref().filter_map(Result::ok).collect();
    let stop = match walk.stopped() {
        Some(WalkStop::EntryLimit) => Some(StopReason::EntryLimit),
        Some(WalkStop::Deadline) => Some(StopReason::Deadline),
        Some(WalkStop::DepthLimit) | None => None,
    };
    Ok((entries, stop))
}

/// Ausschlussregel: `(name, ist_verzeichnis) -> auslassen`.
pub type ExcludeFn = fn(&OsStr, bool) -> bool;

/// Optionen für [`walk_tree`].
#[derive(Debug, Clone, Copy)]
pub struct WalkOptions {
    /// Maximale Tiefe gemeldeter Einträge (≤ [`HARD_MAX_DEPTH`] wird erzwungen).
    pub max_depth: usize,
    /// Maximale Anzahl gelesener Einträge (≤ [`HARD_MAX_ENTRIES`] wird erzwungen).
    pub max_entries: usize,
    /// Abbruchzeitpunkt.
    pub deadline: Instant,
    /// `.gitignore`/`.ignore` auswerten.
    pub gitignore: bool,
    /// Harte Ausschlüsse (werden nicht gemeldet und nicht betreten).
    pub exclude: ExcludeFn,
}

impl WalkOptions {
    /// Standardgrenzen: [`HARD_MAX_DEPTH`], [`HARD_MAX_ENTRIES`], Deadline in
    /// [`WALK_DEADLINE`].
    #[must_use]
    pub fn standard(gitignore: bool, exclude: ExcludeFn) -> Self {
        Self {
            max_depth: HARD_MAX_DEPTH,
            max_entries: HARD_MAX_ENTRIES,
            deadline: Instant::now() + WALK_DEADLINE,
            gitignore,
            exclude,
        }
    }
}

/// Ein vom Walk gemeldeter Eintrag.
#[derive(Debug)]
pub struct Entry<'a> {
    /// Deskriptor des Elternverzeichnisses (Basis für [`open_file_in`]).
    pub dir: &'a File,
    /// Name im Elternverzeichnis.
    pub name: &'a OsStr,
    /// Pfad relativ zur Workspace-Wurzel.
    pub rel: &'a Path,
    /// Typ laut `fstatat(AT_SYMLINK_NOFOLLOW)`.
    pub entry_type: EntryType,
    /// Größe (nur für Dateien aussagekräftig).
    pub len: u64,
}

/// Zustand eines laufenden [`walk_tree`].
struct WalkState<'w> {
    workspace: &'w Workspace,
    opts: WalkOptions,
    entries: usize,
    depth_limited: bool,
    visited: HashSet<(u64, u64)>,
    /// Ignore-Matcher je Verzeichnis (relativ zur Wurzel), flach nach tief.
    ignores: Vec<(PathBuf, Gitignore)>,
}

/// Walkt `start_rel` (relativ zur Wurzel) in sortierter Tiefensuche.
///
/// `visit` wird für jeden nicht ausgeschlossenen Eintrag aufgerufen und kann
/// mit `ControlFlow::Break(grund)` abbrechen. Symlinks werden gemeldet, aber
/// nie gefolgt; nicht lesbare Unterverzeichnisse werden still übersprungen.
///
/// # Errors
/// Nur Fehler beim Öffnen oder Einlesen des Startverzeichnisses.
pub fn walk_tree<F>(
    workspace: &Workspace,
    start_rel: &Path,
    opts: WalkOptions,
    mut visit: F,
) -> io::Result<Option<StopReason>>
where
    F: FnMut(&Entry<'_>) -> ControlFlow<StopReason>,
{
    let opts = WalkOptions {
        max_depth: opts.max_depth.min(HARD_MAX_DEPTH),
        max_entries: opts.max_entries.min(HARD_MAX_ENTRIES),
        ..opts
    };
    let start = workspace.open_dir(start_rel)?;
    let mut state = WalkState {
        workspace,
        opts,
        entries: 0,
        depth_limited: false,
        visited: HashSet::new(),
        ignores: Vec::new(),
    };
    if opts.gitignore {
        load_ancestor_ignores(&mut state, start_rel);
    }
    if opts.max_depth == 0 {
        return Ok(Some(StopReason::DepthLimit));
    }
    let stop = walk_dir(&mut state, &start, start_rel, 0, &mut visit, true)?;
    Ok(stop.or_else(|| state.depth_limited.then_some(StopReason::DepthLimit)))
}

/// Lädt die Ignore-Dateien aller Verzeichnisse von der Wurzel bis
/// ausschließlich `start_rel` (das Startverzeichnis lädt [`walk_dir`]).
fn load_ancestor_ignores(state: &mut WalkState<'_>, start_rel: &Path) {
    let mut ancestor = PathBuf::new();
    for component in start_rel.components() {
        if let Ok(dir) = state.workspace.open_dir(&ancestor) {
            if let Some(matcher) = load_ignore(&dir) {
                state.ignores.push((ancestor.clone(), matcher));
            }
        }
        ancestor.push(component);
    }
}

/// Baut den Ignore-Matcher eines Verzeichnisses (oder `None`, wenn leer).
fn load_ignore(dir: &File) -> Option<Gitignore> {
    let mut builder = GitignoreBuilder::new(".");
    let mut any = false;
    for name in IGNORE_FILE_NAMES {
        let Ok(file) = open_file_in(dir, OsStr::new(name)) else {
            continue;
        };
        let mut bytes = Vec::new();
        if file
            .take(MAX_IGNORE_FILE_BYTES)
            .read_to_end(&mut bytes)
            .is_err()
        {
            continue;
        }
        for line in String::from_utf8_lossy(&bytes).lines() {
            // Unparsebare Zeilen werden wie bei `ignore::WalkBuilder` übergangen.
            if builder.add_line(None, line).is_ok() {
                any = true;
            }
        }
    }
    if any { builder.build().ok() } else { None }
}

/// `true`, wenn `rel` laut der geladenen Ignore-Dateien ausgelassen wird.
fn is_ignored(ignores: &[(PathBuf, Gitignore)], rel: &Path, is_dir: bool) -> bool {
    for (dir_rel, matcher) in ignores.iter().rev() {
        let Ok(relative) = rel.strip_prefix(dir_rel) else {
            continue;
        };
        match matcher.matched(relative, is_dir) {
            Match::Ignore(_) => return true,
            Match::Whitelist(_) => return false,
            Match::None => {}
        }
    }
    false
}

/// Walkt ein geöffnetes Verzeichnis auf Tiefe `depth`; `Some` = globaler Abbruch.
fn walk_dir<F>(
    state: &mut WalkState<'_>,
    dir: &File,
    dir_rel: &Path,
    depth: usize,
    visit: &mut F,
    is_start: bool,
) -> io::Result<Option<StopReason>>
where
    F: FnMut(&Entry<'_>) -> ControlFlow<StopReason>,
{
    if Instant::now() >= state.opts.deadline {
        return Ok(Some(StopReason::Deadline));
    }
    let meta = match dir.metadata() {
        Ok(meta) => meta,
        Err(err) if is_start => return Err(err),
        Err(_) => return Ok(None),
    };
    if !state.visited.insert((meta.dev(), meta.ino())) {
        // Bereits betreten (z. B. Bind-Mount): nicht erneut absteigen.
        return Ok(None);
    }
    let remaining = state.opts.max_entries.saturating_sub(state.entries);
    let listed = read_dir_limited(
        state.workspace,
        dir,
        dir_rel,
        remaining,
        state.opts.deadline,
    );
    let (children, pending_stop) = match listed {
        Ok(listed) => listed,
        Err(err) if is_start => return Err(err),
        Err(_) => return Ok(None),
    };
    state.entries = state.entries.saturating_add(children.len());

    let pushed_ignore = if state.opts.gitignore {
        match load_ignore(dir) {
            Some(matcher) => {
                state.ignores.push((dir_rel.to_path_buf(), matcher));
                true
            }
            None => false,
        }
    } else {
        false
    };

    let result = walk_children(state, dir, dir_rel, depth, &children, pending_stop, visit);
    if pushed_ignore {
        state.ignores.pop();
    }
    result
}

/// Verarbeitet die gelesenen Kinder eines Verzeichnisses.
fn walk_children<F>(
    state: &mut WalkState<'_>,
    dir: &File,
    dir_rel: &Path,
    depth: usize,
    children: &[WalkEntry],
    pending_stop: Option<StopReason>,
    visit: &mut F,
) -> io::Result<Option<StopReason>>
where
    F: FnMut(&Entry<'_>) -> ControlFlow<StopReason>,
{
    for child in children {
        let name = child.rel_path.as_os_str();
        let is_dir = child.entry_type == EntryType::Dir;
        if (state.opts.exclude)(name, is_dir) {
            continue;
        }
        let child_rel = dir_rel.join(name);
        if state.opts.gitignore && is_ignored(&state.ignores, &child_rel, is_dir) {
            continue;
        }
        let entry = Entry {
            dir,
            name,
            rel: &child_rel,
            entry_type: child.entry_type,
            len: child.len,
        };
        if let ControlFlow::Break(reason) = visit(&entry) {
            return Ok(Some(reason));
        }
        if Instant::now() >= state.opts.deadline {
            return Ok(Some(StopReason::Deadline));
        }
        if !is_dir || pending_stop.is_some() {
            continue;
        }
        if depth + 1 >= state.opts.max_depth {
            state.depth_limited = true;
            continue;
        }
        // Unterverzeichnis relativ zum Eltern-Deskriptor öffnen: wurde es
        // inzwischen gegen einen Symlink getauscht, scheitert das (ELOOP).
        let Ok(sub) = open_beneath(dir.as_fd(), Path::new(name), OpenMode::read_only()) else {
            continue;
        };
        if !sub.metadata().is_ok_and(|meta| meta.is_dir()) {
            continue;
        }
        if let Some(stop) = walk_dir(state, &sub, &child_rel, depth + 1, visit, false)? {
            return Ok(Some(stop));
        }
    }
    Ok(pending_stop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};
    use std::fs;
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;

    fn no_exclude(_: &OsStr, _: bool) -> bool {
        false
    }

    /// Workspace `ws/` neben einem fremden Verzeichnis `outside/`.
    fn fixture() -> TestResult<(TempDir, PathBuf, PathBuf)> {
        let dir = TempDir::new()?;
        let base = dir.path().canonicalize()?;
        let ws = base.join("ws");
        let outside = base.join("outside");
        fs::create_dir_all(&ws)?;
        fs::create_dir_all(outside.join("deep"))?;
        fs::write(outside.join("secret.txt"), "TOPSECRET")?;
        fs::write(outside.join("deep/more.txt"), "TOPSECRET")?;
        Ok((dir, ws, outside))
    }

    fn collect(
        workspace: &Workspace,
        start: &Path,
        opts: WalkOptions,
    ) -> TestResult<(Vec<String>, Option<StopReason>)> {
        let mut seen = Vec::new();
        let stop = walk_tree(workspace, start, opts, |entry| {
            seen.push(entry.rel.to_string_lossy().into_owned());
            ControlFlow::Continue(())
        })?;
        Ok((seen, stop))
    }

    #[test]
    fn normalize_relative_lehnt_ausbrueche_ab() -> TestResult {
        assert_eq!(
            normalize_relative(".").map_err(TestError::Unexpected)?,
            PathBuf::new()
        );
        assert_eq!(
            normalize_relative("./src/").map_err(TestError::Unexpected)?,
            PathBuf::from("src")
        );
        assert_eq!(
            normalize_relative("a//b/.").map_err(TestError::Unexpected)?,
            PathBuf::from("a/b")
        );
        assert!(normalize_relative("").is_err());
        assert!(normalize_relative("../x").is_err());
        assert!(normalize_relative("a/../b").is_err());
        assert!(normalize_relative("/etc/passwd").is_err());
        Ok(())
    }

    #[test]
    fn walk_folgt_keinen_symlinks_und_terminiert_bei_schleifen() -> TestResult {
        let (_dir, ws, outside) = fixture()?;
        fs::create_dir_all(ws.join("a"))?;
        fs::write(ws.join("a/inner.txt"), "x")?;
        symlink(&outside, ws.join("link_dir"))?;
        symlink(outside.join("secret.txt"), ws.join("link_file"))?;
        symlink(".", ws.join("loop"))?;
        symlink("..", ws.join("a/up"))?;

        let workspace = Workspace::open(&ws)?;
        let opts = WalkOptions::standard(false, no_exclude);
        let (seen, stop) = collect(&workspace, Path::new(""), opts)?;
        assert_eq!(stop, None);
        assert_eq!(
            seen,
            vec!["a", "a/inner.txt", "a/up", "link_dir", "link_file", "loop"]
        );
        Ok(())
    }

    #[test]
    fn start_ueber_symlink_wird_abgelehnt() -> TestResult {
        let (_dir, ws, outside) = fixture()?;
        symlink(&outside, ws.join("link_dir"))?;
        let workspace = Workspace::open(&ws)?;
        let result = walk_tree(
            &workspace,
            Path::new("link_dir"),
            WalkOptions::standard(false, no_exclude),
            |_| ControlFlow::Continue(()),
        );
        assert!(result.is_err(), "Start über Symlink muss scheitern");
        assert!(
            workspace
                .open_any(Path::new("link_dir/secret.txt"))
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn race_verzeichnis_durch_symlink_ersetzt() -> TestResult {
        let (_dir, ws, outside) = fixture()?;
        fs::write(ws.join("a.txt"), "a")?;
        fs::create_dir_all(ws.join("sub"))?;
        fs::write(ws.join("sub/own.txt"), "own")?;

        let workspace = Workspace::open(&ws)?;
        let mut seen = Vec::new();
        let mut read_outside = false;
        let mut io_error: Option<io::Error> = None;
        let stop = walk_tree(
            &workspace,
            Path::new(""),
            WalkOptions::standard(false, no_exclude),
            |entry| {
                if entry.rel == Path::new("a.txt") {
                    // Wettlauf-Surrogat: `sub` wurde bereits als Verzeichnis
                    // gelistet und wird jetzt gegen einen Symlink getauscht.
                    if let Err(err) = fs::rename(ws.join("sub"), ws.join("sub_old")) {
                        io_error.get_or_insert(err);
                    }
                    if let Err(err) = symlink(&outside, ws.join("sub")) {
                        io_error.get_or_insert(err);
                    }
                }
                if entry.entry_type == EntryType::File {
                    if let Ok(file) = open_file_in(entry.dir, entry.name) {
                        match read_bounded(&file, 1024) {
                            Ok(Some(bytes)) => read_outside |= bytes == b"TOPSECRET",
                            Ok(None) => {}
                            Err(err) => {
                                io_error.get_or_insert(err);
                            }
                        }
                    }
                }
                seen.push(entry.rel.to_string_lossy().into_owned());
                ControlFlow::Continue(())
            },
        )?;
        if let Some(err) = io_error {
            return Err(TestError::from(err));
        }
        assert_eq!(stop, None);
        assert!(!read_outside, "fremder Inhalt gelesen: {seen:?}");
        assert!(seen.iter().all(|rel| !rel.starts_with("sub/")), "{seen:?}");
        // Direktes Öffnen über das getauschte Glied scheitert ebenfalls.
        assert!(workspace.open_any(Path::new("sub/secret.txt")).is_err());
        Ok(())
    }

    #[test]
    fn geoeffnetes_verzeichnis_bleibt_nach_tausch_gebunden() -> TestResult {
        let (_dir, ws, outside) = fixture()?;
        fs::create_dir_all(ws.join("sub"))?;
        fs::write(ws.join("sub/own.txt"), "own")?;
        let workspace = Workspace::open(&ws)?;
        let sub = workspace.open_dir(Path::new("sub"))?;

        fs::rename(ws.join("sub"), ws.join("sub_old"))?;
        symlink(&outside, ws.join("sub"))?;

        let deadline = Instant::now() + WALK_DEADLINE;
        match read_dir_limited(&workspace, &sub, Path::new("sub"), 100, deadline) {
            Ok((entries, _)) => {
                let names: Vec<_> = entries.iter().map(|e| e.rel_path.clone()).collect();
                // Mit `/proc`: das ursprüngliche Verzeichnis; ohne: Symlink abgelehnt.
                assert_eq!(names, vec![PathBuf::from("own.txt")]);
            }
            Err(err) => assert!(proc_fd_path(&sub).is_none(), "unerwarteter Fehler: {err}"),
        }
        Ok(())
    }

    #[test]
    fn eintrags_tiefen_und_zeitgrenze_greifen() -> TestResult {
        let (_dir, ws, _outside) = fixture()?;
        for i in 0..20 {
            fs::write(ws.join(format!("f{i:02}")), "x")?;
        }
        let mut deep = ws.join("zz");
        for _ in 0..(HARD_MAX_DEPTH + 3) {
            fs::create_dir_all(&deep)?;
            deep.push("d");
        }
        let workspace = Workspace::open(&ws)?;

        let mut opts = WalkOptions::standard(false, no_exclude);
        opts.max_entries = 5;
        let (seen, stop) = collect(&workspace, Path::new(""), opts)?;
        assert_eq!(seen.len(), 5);
        assert_eq!(stop, Some(StopReason::EntryLimit));

        let mut opts = WalkOptions::standard(false, no_exclude);
        opts.max_depth = usize::MAX;
        let (seen, stop) = collect(&workspace, Path::new("zz"), opts)?;
        assert_eq!(
            seen.len(),
            HARD_MAX_DEPTH,
            "Tiefe wird hart auf 32 begrenzt"
        );
        assert_eq!(stop, Some(StopReason::DepthLimit));

        let mut opts = WalkOptions::standard(false, no_exclude);
        opts.deadline = Instant::now();
        let (seen, stop) = collect(&workspace, Path::new(""), opts)?;
        assert!(seen.is_empty());
        assert_eq!(stop, Some(StopReason::Deadline));
        Ok(())
    }

    #[test]
    fn gitignore_beschneidet_und_vorfahren_zaehlen() -> TestResult {
        let (_dir, ws, _outside) = fixture()?;
        fs::create_dir_all(ws.join("src/gen"))?;
        fs::create_dir_all(ws.join("node_modules/pkg"))?;
        fs::write(ws.join(".gitignore"), "node_modules/\n*.log\n")?;
        fs::write(ws.join("src/.gitignore"), "gen/\n!keep.log\n")?;
        fs::write(ws.join("src/a.rs"), "")?;
        fs::write(ws.join("src/x.log"), "")?;
        fs::write(ws.join("src/keep.log"), "")?;
        fs::write(ws.join("src/gen/out.rs"), "")?;
        fs::write(ws.join("node_modules/pkg/index.js"), "")?;
        let workspace = Workspace::open(&ws)?;

        let opts = WalkOptions::standard(true, no_exclude);
        let (seen, _) = collect(&workspace, Path::new(""), opts)?;
        assert_eq!(
            seen,
            vec![
                ".gitignore",
                "src",
                "src/.gitignore",
                "src/a.rs",
                "src/keep.log"
            ]
        );

        let (seen, _) = collect(&workspace, Path::new("src"), opts)?;
        assert_eq!(seen, vec!["src/.gitignore", "src/a.rs", "src/keep.log"]);
        Ok(())
    }

    #[test]
    fn read_bounded_und_zeilenkuerzung() -> TestResult {
        let (_dir, ws, _outside) = fixture()?;
        fs::write(ws.join("small"), "abc")?;
        let workspace = Workspace::open(&ws)?;
        let file = workspace.open_any(Path::new("small"))?;
        assert_eq!(read_bounded(&file, 3)?.as_deref(), Some(&b"abc"[..]));
        let file = workspace.open_any(Path::new("small"))?;
        assert_eq!(read_bounded(&file, 2)?, None);

        let long = "ä".repeat(MAX_LINE_BYTES);
        let cut = truncate_line(&long);
        assert!(cut.len() <= MAX_LINE_BYTES + '…'.len_utf8());
        assert!(cut.ends_with('…'));
        assert_eq!(truncate_line("kurz"), "kurz");
        Ok(())
    }
}
