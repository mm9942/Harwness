//! Symlinks in fs-Werkzeugpfaden: auflösen, freigeben, sicher öffnen.
//!
//! # Regeln
//! - **Lesende Tools** (`fs.read`, `fs.list`, `fs.search`, `fs.grep`,
//!   `fs.glob`) folgen einem Symlink im angegebenen Pfad,
//!   - ohne Rückfrage, wenn sein Ziel im Workspace bleibt;
//!   - mit Freigabe, wenn es außerhalb liegt: die Freigabe läuft über den
//!     normalen Freigabeweg (`DefaultApprovalPolicy` fragt über
//!     [`SymlinkAccess::review_call`] **vor** dem Dispatch; unter Full Access
//!     ohne Rückfrage). Eine erteilte Freigabe gilt für das aufgelöste
//!     Zielverzeichnis bis zum Prozessende ([`SymlinkAccess::grant_dir`]),
//!     damit z. B. `vcpkg_installed/*/share/*` nicht dreißigmal fragt.
//! - **Schreibende Tools** (`fs.write`, `fs.edit`) folgen einem Symlink nur,
//!   wenn sein Ziel im Workspace bleibt (`resolve_for_write`); außerhalb
//!   gibt es — wie für jedes Schreiben außerhalb des Workspace — keinen Weg.
//! - Jede Ablehnung nennt Link und Ziel: „Symlink zeigt außerhalb des
//!   Arbeitsbereichs: '<link>' -> '<ziel>'“.
//!
//! Symlinks **innerhalb** eines Verzeichnis-Walks werden weiterhin nie
//! gefolgt; nur der vom Modell genannte Startpfad wird aufgelöst.
//!
//! # TOCTOU
//! [`resolve_in_root`] ist nur ein **Vorschlag**: komponentenweise `lstat`/
//! `readlink` auf Pfaden unter der Wurzel. Geöffnet wird danach immer strikt:
//! - innen über `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)` relativ zum
//!   Wurzel-Deskriptor (`Workspace::open_any`) — ein zwischen
//!   Auflösen und Öffnen eingetauschter Symlink lässt das Öffnen mit `ELOOP`
//!   scheitern;
//! - außen (nur mit Freigabe) über den kanonischen Pfad: das freigegebene
//!   Verzeichnis wird mit `O_NOFOLLOW` geöffnet, seine Identität
//!   (`st_dev`/`st_ino`) gegen den Stand bei der Auflösung geprüft, und der
//!   Rest wieder strikt darunter geöffnet (`open_start`).
//!
//! Vor einer Freigabe wird ein Ziel außerhalb nur kanonisiert (`stat`), nie
//! gelesen.
//!
//! # Nebenläufigkeit
//! [`SymlinkAccess`] ist `Send + Sync` (ein `Mutex`); [`global`] ist die
//! prozessweite Instanz, die Freigabepolitik und Werkzeuge teilen.

use crate::tree::Workspace;
use serde_json::Value;
use std::collections::VecDeque;
use std::ffi::OsString;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

/// Höchstzahl aufgelöster Symlinks je Pfad (wie Linux' `MAXSYMLINKS`).
pub const MAX_SYMLINK_HOPS: usize = 40;

/// Lesende fs-Werkzeuge, deren `path` einem Symlink folgen darf.
pub const SYMLINK_FOLLOWING_TOOLS: [&str; 5] =
    ["fs.read", "fs.list", "fs.search", "fs.grep", "fs.glob"];

/// Präfix der Ablehnung eines Symlinks nach außen.
pub const OUTSIDE_PREFIX: &str = "Symlink zeigt außerhalb des Arbeitsbereichs";

/// Wie lange eine vor dem Dispatch erteilte Einzelfreigabe auf ihren Aufruf
/// wartet.
const CALL_APPROVAL_TTL: Duration = Duration::from_secs(15 * 60);

/// Obergrenze offener Einzelfreigaben (älteste fallen heraus).
const MAX_PENDING_APPROVALS: usize = 256;

/// Ein noch aufzulösendes Pfadglied.
enum Part {
    /// Normales Glied.
    Normal(OsString),
    /// `..` (nur aus Symlink-Zielen; Modellpfade enthalten keins).
    Parent,
}

impl Part {
    /// `.`/Wurzel/Präfix entfallen (absolute Ziele sind bereits entwurzelt).
    fn from_component(component: Component<'_>) -> Option<Self> {
        match component {
            Component::Normal(name) => Some(Self::Normal(name.to_os_string())),
            Component::ParentDir => Some(Self::Parent),
            Component::CurDir | Component::RootDir | Component::Prefix(_) => None,
        }
    }
}

/// Ein Pfad, der über einen Symlink die Wurzel verlässt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutsideTarget {
    /// Workspace-relativer Pfad des Symlinks, der hinausführt.
    pub link: PathBuf,
    /// Rohes Ziel dieses Symlinks (`readlink`).
    pub target: PathBuf,
    /// Kanonischer, absoluter Pfad des angefragten Eintrags.
    pub canonical: PathBuf,
    /// Kanonisches Verzeichnis, für das die Freigabe gilt und gemerkt wird:
    /// der Eintrag selbst, wenn er ein Verzeichnis ist, sonst sein Elternteil.
    pub grant_dir: PathBuf,
    /// `(st_dev, st_ino)` von [`Self::grant_dir`] bei der Auflösung.
    grant_identity: (u64, u64),
}

impl OutsideTarget {
    /// Die Meldung, die Link und Ziel nennt.
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "{OUTSIDE_PREFIX}: '{}' -> '{}' (aufgelöst: {})",
            self.link.display(),
            self.target.display(),
            self.canonical.display()
        )
    }
}

/// Ergebnis von [`resolve_in_root`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Symlinkfreier Pfad relativ zur Wurzel (leer = Wurzel).
    Inside(PathBuf),
    /// Der Pfad verlässt die Wurzel über einen Symlink.
    Outside(OutsideTarget),
}

/// Löst Symlinks in `rel` (normalisiert, ohne `..`) unter `root` auf.
///
/// # Beschreibung
/// Komponentenweise: je Glied `lstat`, bei Symlinks `readlink`. Relative
/// Ziele gelten gegen das bereits aufgelöste, symlinkfreie Elternverzeichnis;
/// `..` darf die Wurzel nicht verlassen, absolute Ziele müssen mit `root`
/// beginnen — sonst ist das Ergebnis [`Resolution::Outside`]: der Rest des
/// Pfads wird an das Ziel gehängt und kanonisiert. Führt der kanonische Pfad
/// zurück unter `root`, ist das Ergebnis doch `Inside`.
///
/// # Errors
/// `errno` von `lstat`/`readlink`/`canonicalize` (z. B. `ENOENT`,
/// `ENOTDIR`); `Other` bei mehr als [`MAX_SYMLINK_HOPS`] Ebenen. Ein Fehler
/// hinter einem Symlink nennt Link und Ziel.
pub fn resolve_in_root(root: &Path, rel: &Path) -> io::Result<Resolution> {
    let mut pending: VecDeque<Part> = rel.components().filter_map(Part::from_component).collect();
    let mut resolved = PathBuf::new();
    let mut hops = 0usize;
    let mut last_link: Option<(PathBuf, PathBuf)> = None;
    while let Some(part) = pending.pop_front() {
        let name = match part {
            Part::Parent => {
                if resolved.pop() {
                    continue;
                }
                // `..` über die Wurzel hinaus: nur aus einem Symlink-Ziel.
                let (link, target) = last_link.clone().unwrap_or_default();
                pending.push_front(Part::Parent);
                return outside(root, root.to_path_buf(), pending, link, target);
            }
            Part::Normal(name) => name,
        };
        let candidate = resolved.join(&name);
        let host_path = root.join(&candidate);
        let metadata = std::fs::symlink_metadata(&host_path)
            .map_err(|error| with_link_context(error, last_link.as_ref()))?;
        if !metadata.file_type().is_symlink() {
            resolved = candidate;
            continue;
        }
        hops += 1;
        if hops > MAX_SYMLINK_HOPS {
            return Err(io::Error::other(format!(
                "zu viele Symlink-Ebenen (mehr als {MAX_SYMLINK_HOPS}) bei '{}'",
                candidate.display()
            )));
        }
        let target = std::fs::read_link(&host_path)?;
        let tail: Vec<Part> = if target.is_absolute() {
            let Some(inner) = target.strip_prefix(root).ok().map(Path::to_path_buf) else {
                return outside(root, target.clone(), pending, candidate, target);
            };
            resolved = PathBuf::new();
            inner
                .components()
                .filter_map(Part::from_component)
                .collect()
        } else {
            target
                .components()
                .filter_map(Part::from_component)
                .collect()
        };
        for part in tail.into_iter().rev() {
            pending.push_front(part);
        }
        last_link = Some((candidate, target));
    }
    Ok(Resolution::Inside(resolved))
}

/// Hängt den Link an einen Fehler hinter einem Symlink.
fn with_link_context(error: io::Error, link: Option<&(PathBuf, PathBuf)>) -> io::Error {
    match link {
        Some((link, target)) => io::Error::new(
            error.kind(),
            format!(
                "Symlink '{}' -> '{}': Ziel nicht lesbar: {error}",
                link.display(),
                target.display()
            ),
        ),
        None => error,
    }
}

/// Baut das `Outside`-Ergebnis: `base` plus die restlichen Glieder,
/// kanonisiert; führt es zurück unter `root`, ist es `Inside`.
fn outside(
    root: &Path,
    base: PathBuf,
    rest: VecDeque<Part>,
    link: PathBuf,
    target: PathBuf,
) -> io::Result<Resolution> {
    let mut path = base;
    for part in rest {
        match part {
            Part::Normal(name) => path.push(name),
            Part::Parent => path.push(".."),
        }
    }
    let canonical = std::fs::canonicalize(&path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "Symlink '{}' -> '{}': Ziel nicht lesbar: {error}",
                link.display(),
                target.display()
            ),
        )
    })?;
    if let Ok(inner) = canonical.strip_prefix(root) {
        return Ok(Resolution::Inside(inner.to_path_buf()));
    }
    let metadata = std::fs::metadata(&canonical)?;
    let grant_dir = if metadata.is_dir() {
        canonical.clone()
    } else {
        canonical
            .parent()
            .map_or_else(|| canonical.clone(), Path::to_path_buf)
    };
    let grant_metadata = std::fs::metadata(&grant_dir)?;
    Ok(Resolution::Outside(OutsideTarget {
        link,
        target,
        canonical,
        grant_dir,
        grant_identity: (grant_metadata.dev(), grant_metadata.ino()),
    }))
}

/// Löst den Zielpfad eines **schreibenden** Tools auf: Symlinks im Pfad (auch
/// ein Symlink als letztes Glied) werden nur gefolgt, wenn das Ziel im
/// Workspace bleibt. Ein noch nicht existierendes letztes Glied bleibt, wie
/// es ist.
///
/// # Errors
/// `PermissionDenied` mit [`OUTSIDE_PREFIX`] und Link/Ziel, wenn ein Symlink
/// hinausführt; `InvalidInput` ohne Dateinamen; sonst der `errno` der
/// Auflösung.
pub(crate) fn resolve_for_write(root: &Path, rel: &Path) -> io::Result<PathBuf> {
    let (Some(name), Some(parent)) = (rel.file_name(), rel.parent()) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Pfad enthält keinen Dateinamen",
        ));
    };
    let parent = inside_or_refuse(resolve_in_root(root, parent)?)?;
    let candidate = parent.join(name);
    match std::fs::symlink_metadata(root.join(&candidate)) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            inside_or_refuse(resolve_in_root(root, &candidate)?)
        }
        _ => Ok(candidate),
    }
}

/// `Inside` → Pfad, `Outside` → Ablehnung mit Link und Ziel.
fn inside_or_refuse(resolution: Resolution) -> io::Result<PathBuf> {
    match resolution {
        Resolution::Inside(rel) => Ok(rel),
        Resolution::Outside(target) => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            target.describe(),
        )),
    }
}

/// Ein geöffneter Startpunkt eines lesenden Tools.
#[derive(Debug)]
pub(crate) struct Start {
    /// Die Wurzel, unter der `rel` liegt: der Workspace oder — nach
    /// Freigabe — das freigegebene Verzeichnis außerhalb.
    pub(crate) workspace: Workspace,
    /// Symlinkfreier Pfad relativ zu [`Self::workspace`].
    pub(crate) rel: PathBuf,
}

/// Löst `rel` für ein lesendes Tool auf und öffnet die passende Wurzel.
///
/// # Beschreibung
/// Registriert `root` bei [`global`] (damit die Freigabepolitik künftige
/// Aufrufe vor dem Dispatch prüfen kann). Liegt das Ziel außerhalb, wird es
/// nur geöffnet, wenn [`SymlinkAccess::allows`] zustimmt; das freigegebene
/// Verzeichnis wird mit `O_NOFOLLOW` geöffnet und seine Identität geprüft.
///
/// # Errors
/// `PermissionDenied` mit [`OutsideTarget::describe`], wenn keine Freigabe
/// vorliegt oder das Verzeichnis inzwischen ausgetauscht wurde; sonst
/// Auflösungs- und Öffnungsfehler.
pub(crate) fn open_start(root: &Path, rel: &Path, tool: &str, path_arg: &str) -> io::Result<Start> {
    let access = global();
    access.register_root(root);
    match resolve_in_root(root, rel)? {
        Resolution::Inside(rel) => Ok(Start {
            workspace: Workspace::open(root)?,
            rel,
        }),
        Resolution::Outside(target) => {
            if !access.allows(&target, tool, path_arg) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "{}; Lesen außerhalb braucht eine Freigabe (der nächste gleiche Aufruf \
                         fragt nach)",
                        target.describe()
                    ),
                ));
            }
            let workspace = Workspace::open(&target.grant_dir)?;
            let opened = workspace.root_metadata()?;
            if (opened.dev(), opened.ino()) != target.grant_identity {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "{}; das Verzeichnis wurde zwischen Prüfen und Öffnen ausgetauscht",
                        target.describe()
                    ),
                ));
            }
            let rel = target
                .canonical
                .strip_prefix(&target.grant_dir)
                .map(Path::to_path_buf)
                .unwrap_or_default();
            Ok(Start { workspace, rel })
        }
    }
}

/// Offene Einzelfreigabe: `(Werkzeug, path)` → freigegebenes Verzeichnis.
#[derive(Debug)]
struct PendingApproval {
    tool: String,
    path: String,
    grant_dir: PathBuf,
    at: Instant,
}

#[derive(Debug, Default)]
struct State {
    /// Kanonische Workspace-Wurzeln, unter denen Aufrufe aufgelöst werden.
    roots: Vec<PathBuf>,
    /// Freigegebene Verzeichnisse außerhalb (gemerkt bis Prozessende).
    granted: Vec<PathBuf>,
    /// Vor dem Dispatch erteilte, noch nicht verbrauchte Freigaben.
    pending: VecDeque<PendingApproval>,
}

/// Freigabezustand für Symlinks nach außen (siehe Moduldoku).
#[derive(Debug, Default)]
pub struct SymlinkAccess {
    state: Mutex<State>,
}

/// Die prozessweite Instanz, die Freigabepolitik und fs-Werkzeuge teilen.
#[must_use]
pub fn global() -> &'static SymlinkAccess {
    static GLOBAL: OnceLock<SymlinkAccess> = OnceLock::new();
    GLOBAL.get_or_init(SymlinkAccess::default)
}

impl SymlinkAccess {
    // Ein vergifteter Mutex hält nur Listen: der letzte Stand bleibt gültig.
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Merkt eine kanonische Workspace-Wurzel (idempotent).
    pub fn register_root(&self, root: &Path) {
        let mut state = self.state();
        // Zuletzt benutzte Wurzel ans Ende: `review_call` prüft von hinten,
        // so gewinnt bei mehreren Wurzeln die zuletzt aktive.
        state.roots.retain(|known| known != root);
        state.roots.push(root.to_path_buf());
    }

    /// `true`, wenn `path` in einem freigegebenen Verzeichnis liegt.
    #[must_use]
    pub fn is_granted(&self, path: &Path) -> bool {
        self.state().granted.iter().any(|dir| path.starts_with(dir))
    }

    /// Merkt `dir` als freigegeben (bis Prozessende).
    pub fn grant_dir(&self, dir: &Path) {
        let mut state = self.state();
        if !state.granted.iter().any(|known| dir.starts_with(known)) {
            state.granted.push(dir.to_path_buf());
        }
    }

    /// Prüft einen Aufruf **vor** dem Dispatch: `Some(ziel)`, wenn `tool` ein
    /// lesendes fs-Werkzeug ist und sein `path` über einen Symlink in ein
    /// noch nicht freigegebenes Verzeichnis außerhalb jeder bekannten
    /// Workspace-Wurzel führt. Reine Prüfung ohne Zustandsänderung.
    #[must_use]
    pub fn review_call(&self, tool: &str, arguments: &Value) -> Option<OutsideTarget> {
        if !SYMLINK_FOLLOWING_TOOLS.contains(&tool) {
            return None;
        }
        let path = path_argument(arguments)?;
        let rel = normalized(path)?;
        let roots = self.state().roots.clone();
        roots
            .iter()
            .rev()
            .find_map(|root| match resolve_in_root(root, &rel) {
                Ok(Resolution::Outside(target)) if !self.is_granted(&target.canonical) => {
                    Some(target)
                }
                _ => None,
            })
    }

    /// Vermerkt die Freigabe eines Aufrufs, den die Freigabepolitik eben
    /// durchlässt oder dem Menschen vorlegt: führt das Werkzeug genau diesen
    /// Aufruf danach aus, gilt `target.grant_dir` als freigegeben. Ein
    /// abgelehnter Aufruf läuft nie; sein Vermerk verfällt.
    pub fn approve_call(&self, tool: &str, arguments: &Value, target: &OutsideTarget) {
        let Some(path) = path_argument(arguments) else {
            return;
        };
        let mut state = self.state();
        let now = Instant::now();
        state
            .pending
            .retain(|pending| now.duration_since(pending.at) < CALL_APPROVAL_TTL);
        while state.pending.len() >= MAX_PENDING_APPROVALS {
            state.pending.pop_front();
        }
        state.pending.push_back(PendingApproval {
            tool: tool.to_owned(),
            path: path.to_owned(),
            grant_dir: target.grant_dir.clone(),
            at: now,
        });
    }

    /// Darf ein Werkzeug `target` lesen? Ja, wenn das Verzeichnis bereits
    /// freigegeben ist oder für genau diesen Aufruf (`tool`, `path`) eine
    /// Freigabe vorliegt — die wird dabei verbraucht und das Verzeichnis
    /// gemerkt.
    #[must_use]
    pub fn allows(&self, target: &OutsideTarget, tool: &str, path: &str) -> bool {
        if self.is_granted(&target.canonical) {
            return true;
        }
        let now = Instant::now();
        let mut state = self.state();
        let position = state.pending.iter().position(|pending| {
            pending.tool == tool
                && pending.path == path
                && pending.grant_dir == target.grant_dir
                && now.duration_since(pending.at) < CALL_APPROVAL_TTL
        });
        let Some(position) = position else {
            return false;
        };
        state.pending.remove(position);
        drop(state);
        self.grant_dir(&target.grant_dir);
        true
    }
}

/// `path`-Argument eines fs-Aufrufs; leer/fehlend = Wurzel (kein Symlink).
fn path_argument(arguments: &Value) -> Option<&str> {
    arguments
        .get("path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
}

/// Lexikalisch normalisierter, relativer Pfad (wie `normalize_relative`).
fn normalized(input: &str) -> Option<PathBuf> {
    crate::tree::normalize_relative(input).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;
    use serde_json::json;
    use std::fs;
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;

    /// `ws/` mit `real/file.txt` neben `outside/{a,b}/file.txt`.
    fn fixture() -> TestResult<(TempDir, PathBuf, PathBuf)> {
        let dir = TempDir::new()?;
        let base = dir.path().canonicalize()?;
        let ws = base.join("ws");
        let outside = base.join("outside");
        fs::create_dir_all(ws.join("real"))?;
        fs::write(ws.join("real/file.txt"), "inside")?;
        for sub in ["a", "b"] {
            fs::create_dir_all(outside.join(sub))?;
            fs::write(outside.join(sub).join("file.txt"), "outside")?;
        }
        Ok((dir, ws, outside))
    }

    #[test]
    fn inside_links_resolve_without_leaving_the_root() -> TestResult {
        let (_dir, ws, _) = fixture()?;
        symlink("real", ws.join("rel_link"))?;
        symlink(ws.join("real"), ws.join("abs_link"))?;
        fs::create_dir_all(ws.join("deep"))?;
        symlink("../rel_link/file.txt", ws.join("deep/chain"))?;
        for (path, expected) in [
            ("rel_link/file.txt", "real/file.txt"),
            ("abs_link/file.txt", "real/file.txt"),
            ("deep/chain", "real/file.txt"),
        ] {
            assert_eq!(
                resolve_in_root(&ws, Path::new(path))?,
                Resolution::Inside(PathBuf::from(expected)),
                "{path}"
            );
        }
        Ok(())
    }

    #[test]
    fn outside_links_name_link_and_target() -> TestResult {
        let (_dir, ws, outside) = fixture()?;
        symlink(outside.join("a"), ws.join("abs_out"))?;
        symlink("../outside/b", ws.join("rel_out"))?;
        let Resolution::Outside(target) = resolve_in_root(&ws, Path::new("abs_out/file.txt"))?
        else {
            return Err(crate::test_support::TestError::Unexpected(
                "abs_out must leave the workspace".to_owned(),
            ));
        };
        assert_eq!(target.canonical, outside.join("a/file.txt"));
        assert_eq!(target.grant_dir, outside.join("a"));
        assert!(target.describe().starts_with(OUTSIDE_PREFIX));
        assert!(
            target
                .describe()
                .contains(&outside.join("a").display().to_string()),
            "{}",
            target.describe()
        );
        let Resolution::Outside(target) = resolve_in_root(&ws, Path::new("rel_out"))? else {
            return Err(crate::test_support::TestError::Unexpected(
                "rel_out must leave the workspace".to_owned(),
            ));
        };
        assert_eq!(target.grant_dir, outside.join("b"));
        assert!(target.describe().contains("'rel_out' -> '../outside/b'"));
        Ok(())
    }

    /// Ein Ziel außerhalb fragt; Full Access (Politik vermerkt die Freigabe
    /// ohne Rückfrage) lässt lesen; ein gemerktes Verzeichnis fragt nicht
    /// noch einmal.
    #[test]
    fn outside_target_asks_once_and_the_directory_is_remembered() -> TestResult {
        let (_dir, ws, outside) = fixture()?;
        symlink(outside.join("a"), ws.join("share_a"))?;
        symlink(outside.join("b"), ws.join("share_b"))?;
        let access = SymlinkAccess::default();
        access.register_root(&ws);

        // Ohne Freigabe: die Politik muss fragen, das Werkzeug lehnt ab.
        let args = json!({ "path": "share_a/file.txt" });
        let Some(target) = access.review_call("fs.read", &args) else {
            return Err(crate::test_support::TestError::Unexpected(
                "an outside target must ask".to_owned(),
            ));
        };
        assert!(!access.allows(&target, "fs.read", "share_a/file.txt"));

        // Freigabe (Dialog bestätigt bzw. Full Access ohne Dialog) → lesbar.
        access.approve_call("fs.read", &args, &target);
        assert!(access.allows(&target, "fs.read", "share_a/file.txt"));

        // Gemerkt: dasselbe Verzeichnis fragt nicht mehr, auch mit anderem
        // Werkzeug und anderem Pfad.
        assert!(
            access
                .review_call("fs.list", &json!({ "path": "share_a" }))
                .is_none()
        );
        assert!(access.review_call("fs.read", &args).is_none());
        // Ein anderes Verzeichnis fragt weiterhin.
        assert!(
            access
                .review_call("fs.read", &json!({ "path": "share_b/file.txt" }))
                .is_some()
        );
        // Innen und schreibende Werkzeuge fragen nie.
        assert!(
            access
                .review_call("fs.read", &json!({ "path": "real/file.txt" }))
                .is_none()
        );
        assert!(access.review_call("fs.write", &args).is_none());
        Ok(())
    }

    #[test]
    fn a_consumed_approval_only_covers_its_own_call() -> TestResult {
        let (_dir, ws, outside) = fixture()?;
        symlink(outside.join("a"), ws.join("share_a"))?;
        let access = SymlinkAccess::default();
        access.register_root(&ws);
        let args = json!({ "path": "share_a" });
        let target = access
            .review_call("fs.list", &args)
            .ok_or(crate::test_support::TestError::Missing("outside target"))?;
        access.approve_call("fs.list", &args, &target);
        // Ein anderes Werkzeug mit demselben Pfad ist nicht gedeckt.
        assert!(!access.allows(&target, "fs.read", "share_a"));
        assert!(access.allows(&target, "fs.list", "share_a"));
        Ok(())
    }

    #[test]
    fn writes_follow_only_inside_links() -> TestResult {
        let (_dir, ws, outside) = fixture()?;
        symlink("real/file.txt", ws.join("file_link"))?;
        symlink("real", ws.join("dir_link"))?;
        symlink(outside.join("a/file.txt"), ws.join("out_file"))?;
        symlink(outside.join("a"), ws.join("out_dir"))?;
        assert_eq!(
            resolve_for_write(&ws, Path::new("file_link"))?,
            PathBuf::from("real/file.txt")
        );
        assert_eq!(
            resolve_for_write(&ws, Path::new("dir_link/new.txt"))?,
            PathBuf::from("real/new.txt")
        );
        for path in ["out_file", "out_dir/new.txt"] {
            let Err(error) = resolve_for_write(&ws, Path::new(path)) else {
                return Err(crate::test_support::TestError::Unexpected(format!(
                    "{path} must be refused"
                )));
            };
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            assert!(error.to_string().starts_with(OUTSIDE_PREFIX), "{error}");
            assert!(
                error.to_string().contains(&outside.display().to_string()),
                "{error}"
            );
        }
        Ok(())
    }
}
