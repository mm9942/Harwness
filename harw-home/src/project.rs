//! Projekt-Erkennung und Projekt-Home (`<root>/.harw`), Contract §3.
//!
//! Dieses Modul beantwortet zwei Fragen, die vor jeder Projekt-Config,
//! jedem Projekt-Gedächtnis und jeder Projekt-Trust-Prüfung geklärt sein
//! müssen:
//!
//! - **Wo beginnt das Projekt?** [`discover_project`] läuft von `cwd`
//!   aufwärts durch die Vorfahren und stoppt beim ersten, der einen Marker
//!   trägt (Default `[".git"]`, konfigurierbar über
//!   `project_root_markers`). Ein Git-Worktree wird dabei nicht mit dem
//!   Worktree-Verzeichnis, sondern mit dem Haupt-Repo als **Trust-Anker**
//!   verknüpft ([`ProjectRoot::trust_key`]) — genau wie
//!   `resolve_root_git_project_for_trust` im Vorbild (§1).
//! - **Wo liegt der projekt-lokale Zustand?** [`ProjectHome`] bündelt die
//!   Unterverzeichnisse von `<root>/.harw` (Gedächtnis, Pläne, Goals,
//!   lokaler, gitignorierter Zustand) und legt sie idempotent an.
//!
//! # Sicherheit
//! Die `.git`-Datei eines Worktrees wird ausschließlich symlinkfrei über
//! [`harw_fsutil::open_nofollow`] gelesen (ebenso `commondir`): ein
//! Angreifer, der `.git` durch einen Symlink ersetzt, kann so nicht in eine
//! beliebige Datei außerhalb des Projekts hineinlesen lassen. Scheitert die
//! Auflösung des Haupt-Repos an irgendeiner Stelle (kein gültiger
//! `gitdir:`-Kopf, `gitdir` oder gemeinsamer `.git`-Ordner nicht
//! auflösbar), fällt [`ProjectRoot::trust_key`] auf den Worktree-Root selbst
//! zurück — der Aufrufer bekommt dann einen (fail-closed) eigenständigen
//! Trust-Anker statt eines Fehlers.
//!
//! # Concurrency
//! Alle Funktionen sind zustandslos und `Send + Sync`; [`ProjectHome::ensure`]
//! synchronisiert nicht gegen parallele Aufrufe (idempotent, letzter
//! Schreiber gewinnt bei den angelegten Verzeichnissen).
//!
//! # Errors
//! Alle fallierbaren Operationen liefern [`HomeError`].
//!
//! # Examples
//! ```rust,no_run
//! use harw_home::project::{ProjectHome, discover_project};
//! use std::path::Path;
//!
//! let project = discover_project(Path::new("."), &[])?;
//! let home = ProjectHome::at(&project);
//! home.ensure()?;
//! println!("Projekt-Gedächtnis unter {}", home.memories_dir().display());
//! # Ok::<(), harw_home::HomeError>(())
//! ```

use std::io::Read;
use std::path::{Path, PathBuf};

use harw_fsutil::{AtomicWriteOptions, OpenMode, open_nofollow, write_atomic};

use crate::error::{HomeError, HomeResult};
use crate::paths;

/// Marker, der ohne explizite `project_root_markers`-Konfiguration gesucht
/// wird.
const DEFAULT_MARKER: &str = ".git";

/// Regel für die Gitignore im Projektroot: der gesamte Harw-Zustand bleibt lokal.
const ROOT_GITIGNORE_RULE: &str = ".harw/";

/// Obergrenze für die `.git`-Datei eines Worktrees und ihre `commondir`.
///
/// Beide Dateien sind normalerweise wenige Dutzend Bytes lang; die Grenze
/// verhindert nur, dass ein manipuliertes `.git` unbegrenzt Speicher zieht.
const MAX_GIT_POINTER_FILE_BYTES: u64 = 64 * 1024;

/// Standard-relativer Pfad vom `gitdir` zum gemeinsamen `.git`-Verzeichnis,
/// falls `commondir` fehlt (Standard-Layout `<repo>/.git/worktrees/<name>`).
const DEFAULT_COMMONDIR_RELATIVE: &str = "../..";

/// Ein erkannter Projekt-Root samt Trust-Anker und Erkennungsart.
///
/// # Examples
/// ```rust
/// use harw_home::project::{ProjectKind, ProjectRoot};
/// use std::path::PathBuf;
///
/// let root = PathBuf::from("/home/mia/projects/beispiel");
/// let project = ProjectRoot {
///     trust_key: root.clone(),
///     root,
///     kind: ProjectKind::Git,
/// };
/// assert_eq!(project.trust_key, project.root);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRoot {
    /// Kanonischer Projekt-Root (das Verzeichnis, das den Marker trägt, bzw.
    /// `cwd` selbst bei [`ProjectKind::Directory`]).
    pub root: PathBuf,
    /// Kanonischer Anker für Trust-Entscheidungen. Bei
    /// [`ProjectKind::GitWorktree`] ist das der Haupt-Repo-Root, sonst
    /// identisch mit `root`.
    pub trust_key: PathBuf,
    /// Wie der Root erkannt wurde.
    pub kind: ProjectKind,
}

/// Erkennungsart eines Projekt-Roots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectKind {
    /// `.git` ist ein Verzeichnis: ein gewöhnliches Git-Repository.
    Git,
    /// `.git` ist eine Datei (`gitdir: …`): ein Git-Worktree.
    GitWorktree,
    /// Ein anderer, konfigurierter Marker (nicht `.git`) wurde gefunden.
    Marker(String),
    /// Kein Marker gefunden; jeder Ordner ist ein Projekt.
    Directory,
}

/// Erkennt den Projekt-Root ausgehend von `cwd`.
///
/// # Description
/// `cwd` wird zunächst kanonisiert, dann aufwärts durch alle Vorfahren
/// gelaufen (der nächste zuerst). Der erste Vorfahr, der einen der `markers`
/// trägt, gewinnt. Ist `markers` leer, wird `[".git"]` verwendet. Trägt der
/// gewinnende Vorfahr `.git`, entscheidet dessen Dateityp über
/// [`ProjectKind::Git`] vs. [`ProjectKind::GitWorktree`] (siehe Moduldoku zum
/// Sicherheitsmodell der Worktree-Auflösung). Findet sich kein Marker bis
/// zum Dateisystem-Root, ist das Ergebnis [`ProjectKind::Directory`] mit
/// `root = cwd` (kanonisch).
///
/// # Arguments
/// - `cwd` (`&Path`): Startverzeichnis der Suche.
/// - `markers` (`&[String]`): Marker-Dateinamen in Präzedenzreihenfolge;
///   leer bedeutet `[".git"]`.
///
/// # Returns
/// Den erkannten [`ProjectRoot`].
///
/// # Errors
/// [`HomeError::Io`], wenn `cwd` nicht kanonisierbar ist (z. B. nicht
/// existent).
pub fn discover_project(cwd: &Path, markers: &[String]) -> HomeResult<ProjectRoot> {
    discover_project_with_home_stop(cwd, markers, user_home_canonical().as_deref())
}

/// Wie [`discover_project`], aber mit explizit injizierter Home-Grenze.
///
/// `home_stop` ist das kanonische Home-Verzeichnis, bei dem die Aufwärtssuche
/// endet; `None` läuft bis zum Dateisystem-Root. Nur für Tests und für
/// Aufrufer, die die Home-Auflösung selbst besitzen, öffentlich sichtbar.
pub fn discover_project_with_home_stop(
    cwd: &Path,
    markers: &[String],
    home_stop: Option<&Path>,
) -> HomeResult<ProjectRoot> {
    let canonical_cwd = std::fs::canonicalize(cwd).map_err(|error| HomeError::io(cwd, error))?;
    let effective_markers: Vec<&str> = if markers.is_empty() {
        vec![DEFAULT_MARKER]
    } else {
        markers.iter().map(String::as_str).collect()
    };

    // Das Home-Verzeichnis des Benutzers beendet die Suche nach oben: eine
    // dotfile-lastige Heimstätte ist selbst nie ein Projekt-Root. Ohne diesen
    // Stopper würde eine markerlose Sitzung in `$HOME` bis `/` aufsteigen und
    // `discover_project` auf genau dem Root landen, den `ProjectHome::ensure`
    // fail-closed ablehnt — die Sitzung wäre ohne Projekt-Home blockiert.
    // Ein Marker **in** `$HOME` selbst (etwa ein versehentliches `~/.git`)
    // bindet die Sitzung ebenfalls nicht an das Home: sie fällt dann auf das
    // ursprüngliche Arbeitsverzeichnis zurück.
    // Das System-Temp-Verzeichnis (`/tmp`) ist ebenfalls eine harte Grenze:
    // dort liegen fremde Arbeitsreste (etwa leere `.git`-Mountpunkte eines
    // Sandbox-Laufs mit Arbeitswurzel `/tmp`), die sonst jedes Temp-Projekt
    // an `/tmp` binden und `/tmp/.harw` anlegen würden.
    let temp_stop = temp_dir_canonical();
    for ancestor in canonical_cwd.ancestors() {
        if home_stop == Some(ancestor) || temp_stop.as_deref() == Some(ancestor) {
            break;
        }
        for marker in &effective_markers {
            let marker_path = ancestor.join(marker);
            if std::fs::symlink_metadata(&marker_path).is_err() {
                continue;
            }
            let root = ancestor.to_path_buf();
            return Ok(if *marker == DEFAULT_MARKER {
                resolve_git_marker(root)?
            } else {
                ProjectRoot {
                    trust_key: root.clone(),
                    root,
                    kind: ProjectKind::Marker((*marker).to_owned()),
                }
            });
        }
    }

    // Kein Marker gefunden (oder nur einer im Home-Verzeichnis): das
    // Arbeitsverzeichnis selbst ist der Projekt-Root, nicht der oberste
    // besuchte Vorfahr.
    Ok(ProjectRoot {
        trust_key: canonical_cwd.clone(),
        root: canonical_cwd,
        kind: ProjectKind::Directory,
    })
}

/// Kanonisches System-Temp-Verzeichnis als zusätzlicher Such-Stopper und als
/// unzulässiger Projekt-Root (siehe [`discover_project_with_home_stop`] und
/// `classify_project_home_root`).
fn temp_dir_canonical() -> Option<PathBuf> {
    let temp = std::env::temp_dir();
    std::fs::canonicalize(&temp).ok()
}

/// Kanonisches Home-Verzeichnis des aktuellen Benutzers als Such-Stopper.
///
/// Nur für den Vergleich in [`discover_project`]; `None`, wenn `$HOME` nicht
/// gesetzt oder nicht kanonisierbar ist — dann läuft die Suche wie bisher bis
/// zum Dateisystem-Root.
fn user_home_canonical() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").filter(|value| !value.is_empty())?;
    let home_path = PathBuf::from(home);
    Some(std::fs::canonicalize(&home_path).unwrap_or(home_path))
}

/// Klassifiziert einen gefundenen `.git`-Marker unter `root`.
///
/// `root` ist bereits kanonisch (Vorfahr eines kanonisierten Pfads).
fn resolve_git_marker(root: PathBuf) -> HomeResult<ProjectRoot> {
    let git_path = root.join(DEFAULT_MARKER);
    let meta =
        std::fs::symlink_metadata(&git_path).map_err(|error| HomeError::io(&git_path, error))?;

    if meta.is_dir() {
        return Ok(ProjectRoot {
            trust_key: root.clone(),
            root,
            kind: ProjectKind::Git,
        });
    }

    if meta.file_type().is_file() {
        let trust_key =
            resolve_worktree_trust_key(&root, &git_path).unwrap_or_else(|| root.clone());
        return Ok(ProjectRoot {
            trust_key,
            root,
            kind: ProjectKind::GitWorktree,
        });
    }

    // Symlink oder Sondertyp: nie folgen, also weder als Verzeichnis noch als
    // Datei lesbar behandeln. Der Marker-Treffer bleibt gültig (der Vorfahr
    // gewinnt weiterhin die Suche), aber ohne Git-Semantik.
    Ok(ProjectRoot {
        trust_key: root.clone(),
        root,
        kind: ProjectKind::Marker(DEFAULT_MARKER.to_owned()),
    })
}

/// Löst den Haupt-Repo-Root eines Git-Worktrees auf.
///
/// `None` bei jedem Auflösungsfehler (fehlender/ungültiger `gitdir:`-Kopf,
/// nicht auflösbares `gitdir`, nicht auflösbarer gemeinsamer `.git`-Ordner) —
/// der Aufrufer fällt dann auf `trust_key = root` zurück.
fn resolve_worktree_trust_key(root: &Path, git_file: &Path) -> Option<PathBuf> {
    let contents = read_small_file_nofollow(git_file)?;
    let text = String::from_utf8(contents).ok()?;
    let header = text.trim_end_matches(['\n', '\r']);
    let gitdir_str = header.strip_prefix("gitdir:")?.trim();
    if gitdir_str.is_empty() {
        return None;
    }

    let gitdir_raw = PathBuf::from(gitdir_str);
    let gitdir = if gitdir_raw.is_absolute() {
        gitdir_raw
    } else {
        root.join(gitdir_raw)
    };
    let gitdir = std::fs::canonicalize(&gitdir).ok()?;

    let commondir_relative = match read_small_file_nofollow(&gitdir.join("commondir")) {
        Some(bytes) => {
            let text = String::from_utf8(bytes).ok()?;
            let trimmed = text.trim().to_owned();
            if trimmed.is_empty() {
                return None;
            }
            trimmed
        }
        None => DEFAULT_COMMONDIR_RELATIVE.to_owned(),
    };

    let common_raw = PathBuf::from(&commondir_relative);
    let common_dir = if common_raw.is_absolute() {
        common_raw
    } else {
        gitdir.join(common_raw)
    };
    let common_dir = std::fs::canonicalize(&common_dir).ok()?;
    common_dir.parent().map(Path::to_path_buf)
}

/// Liest eine kleine Datei symlinkfrei; `None` bei jedem Fehler (fehlt, ist
/// Symlink/Sondertyp, überschreitet [`MAX_GIT_POINTER_FILE_BYTES`]).
fn read_small_file_nofollow(path: &Path) -> Option<Vec<u8>> {
    let file = open_nofollow(path, OpenMode::read_only()).ok()?;
    let mut buffer = Vec::new();
    file.take(MAX_GIT_POINTER_FILE_BYTES.saturating_add(1))
        .read_to_end(&mut buffer)
        .ok()?;
    if u64::try_from(buffer.len()).unwrap_or(u64::MAX) > MAX_GIT_POINTER_FILE_BYTES {
        return None;
    }
    Some(buffer)
}

/// Errechnet den dateisystemsicheren Schlüssel eines Projekt-Roots.
///
/// # Description
/// `<sanitisierter Basisname>-<erste 12 Hex-Zeichen von blake3(root)>`. Der
/// Basisname wird auf ASCII-Alnum, `-` und `_` reduziert (jedes andere
/// Zeichen wird zu `-`) und auf 40 Zeichen gekappt; ein Root ohne Basisname
/// (z. B. `/`) ergibt `"root"`. Der Hash geht über die rohen Pfad-Bytes,
/// nicht über einen kanonisierten oder normalisierten Pfad — Aufrufer
/// übergeben in der Regel bereits [`ProjectRoot::root`] (kanonisch).
///
/// # Arguments
/// - `root` (`&Path`): der Projekt-Root, dessen Schlüssel berechnet wird.
///
/// # Returns
/// Einen stabilen, für Dateinamen sicheren Schlüssel.
///
/// # Examples
/// ```rust
/// use harw_home::project::project_key;
/// use std::path::Path;
///
/// let key = project_key(Path::new("/home/mia/projects/beispiel"));
/// assert!(key.starts_with("beispiel-"));
/// assert_eq!(key, project_key(Path::new("/home/mia/projects/beispiel")));
/// ```
#[must_use]
pub fn project_key(root: &Path) -> String {
    let raw_basename = root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let sanitized = sanitize_basename(&raw_basename);
    let hash = blake3::hash(&path_bytes(root));
    let hex = hash.to_hex();
    format!("{sanitized}-{}", &hex.as_str()[..12])
}

/// Reduziert `raw` auf ASCII-Alnum/`-`/`_`, gekappt auf 40 Zeichen.
fn sanitize_basename(raw: &str) -> String {
    let mut out = String::new();
    for ch in raw.chars() {
        if out.len() >= 40 {
            break;
        }
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('-');
        }
    }
    if out.is_empty() {
        "root".to_owned()
    } else {
        out
    }
}

/// Rohe Bytes eines Pfads für den Hash in [`project_key`].
#[cfg(unix)]
fn path_bytes(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}

/// Rohe Bytes eines Pfads für den Hash in [`project_key`] (verlustbehaftet
/// außerhalb von Unix, da `OsStr` dort keinen stabilen Byte-Zugriff bietet).
#[cfg(not(unix))]
fn path_bytes(path: &Path) -> Vec<u8> {
    path.to_string_lossy().as_bytes().to_vec()
}

/// Projekt-lokaler Zustand unter `<root>/.harw`.
///
/// # Examples
/// ```rust,no_run
/// use harw_home::project::{ProjectHome, discover_project};
/// use std::path::Path;
///
/// let project = discover_project(Path::new("."), &[])?;
/// let home = ProjectHome::at(&project);
/// home.ensure()?;
/// # Ok::<(), harw_home::HomeError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectHome {
    /// `<root>/.harw`.
    pub dir: PathBuf,
}

impl ProjectHome {
    /// Baut das Projekt-Home für einen erkannten [`ProjectRoot`].
    #[must_use]
    pub fn at(root: &ProjectRoot) -> Self {
        Self {
            dir: root.root.join(".harw"),
        }
    }

    /// Verzeichnis der Projekt-Erinnerungen (`.harw/memories`).
    #[must_use]
    pub fn memories_dir(&self) -> PathBuf {
        self.dir.join("memories")
    }

    /// Verzeichnis der persistierten Pläne (`.harw/plans`).
    #[must_use]
    pub fn plans_dir(&self) -> PathBuf {
        self.dir.join("plans")
    }

    /// Verzeichnis der Goals (`.harw/goals`).
    #[must_use]
    pub fn goals_dir(&self) -> PathBuf {
        self.dir.join("goals")
    }

    /// Lokaler, gitignorierter Zustand (`.harw/state`).
    #[must_use]
    pub fn state_dir(&self) -> PathBuf {
        self.dir.join("state")
    }

    /// Legt die Projekt-Home-Verzeichnisse (`0700`) an und stellt sicher, dass
    /// der gesamte Harw-Zustand in der Gitignore des Projektroots steht.
    ///
    /// # Description
    /// Lehnt das Dateisystem-Root (`/`) als Projekt-Root ab. Ist der Root das
    /// Home-Verzeichnis des Benutzers selbst (`$HOME`, kanonisiert), wird das
    /// dortige `~/.harw` — der Root-Space — als Projekt-Home verwendet: ein
    /// Start direkt in `~` hat kein eigenes Projekt, soll aber nicht
    /// scheitern. In allen anderen Fällen wird die Root-`.gitignore`
    /// idempotent um `.harw/` ergänzt; im Home-Verzeichnis nicht, weil `~`
    /// kein Repository ist und keine `~/.gitignore` entstehen soll.
    ///
    /// Die Ignore-Regel ist best-effort (siehe `ensure_gitignore_best_effort`):
    /// ein nicht beschreibbarer oder für alle beschreibbarer Projektroot
    /// blockiert den Start nicht.
    ///
    /// # Returns
    /// `Ok(())`, wenn alle Verzeichnisse existieren.
    ///
    /// # Errors
    /// - [`HomeError::UnsupportedProjectHomeRoot`]: `root` ist `/`.
    /// - [`HomeError::Io`]: `root` nicht kanonisierbar oder Anlegen eines
    ///   Verzeichnisses schlägt fehl.
    pub fn ensure(&self) -> HomeResult<()> {
        let root = self
            .dir
            .parent()
            .ok_or_else(|| HomeError::io(&self.dir, project_home_without_parent()))?;
        let root_is_user_home = classify_project_home_root(root)?;

        for dir in [
            self.dir.clone(),
            self.memories_dir(),
            self.plans_dir(),
            self.goals_dir(),
            self.state_dir(),
        ] {
            create_private_dir(&dir)?;
        }

        if !root_is_user_home {
            ensure_gitignore_best_effort(root);
        }
        Ok(())
    }
}

/// Trägt `.harw/` best-effort als Ignore-Regel ein; scheitert nie.
///
/// Zuerst die Root-`.gitignore`. Lässt sie sich nicht schreiben – etwa weil
/// der Projektroot ein für alle beschreibbares Gemeinschaftsverzeichnis ohne
/// Sticky-Bit ist, in dem `write_atomic` aus Sicherheitsgründen ablehnt, oder
/// weil er schreibgeschützt ist –, wird `.git/info/exclude` versucht (liegt
/// im privaten Git-Verzeichnis und landet nicht im Repository). Scheitert
/// auch das, fehlt die Regel eben: eine fehlende Ignore-Regel darf
/// den Start nie blockieren.
fn ensure_gitignore_best_effort(root: &Path) {
    if ensure_root_gitignore(root).is_ok() {
        return;
    }
    let info_dir = root.join(".git").join("info");
    let git_dir_present =
        std::fs::symlink_metadata(root.join(".git")).is_ok_and(|meta| meta.is_dir());
    if git_dir_present {
        // Ergebnis bewusst verworfen: best-effort, siehe oben.
        let _ = std::fs::create_dir_all(&info_dir)
            .map_err(|error| HomeError::io(&info_dir, error))
            .and_then(|()| ensure_ignore_rule(&info_dir.join("exclude")));
    }
}

/// Ergänzt die Root-`.gitignore` um die kanonische Regel für lokalen Harw-Zustand.
///
/// Bestehende Regeln bleiben bytegenau erhalten. Die Erkennung akzeptiert auch
/// die äquivalente Regel `/.harw/`, damit wiederholte Starts keinen Diff erzeugen.
fn ensure_root_gitignore(root: &Path) -> HomeResult<()> {
    ensure_ignore_rule(&root.join(".gitignore"))
}

/// Ergänzt eine Ignore-Datei (`.gitignore` oder `.git/info/exclude`)
/// idempotent um [`ROOT_GITIGNORE_RULE`].
fn ensure_ignore_rule(gitignore: &Path) -> HomeResult<()> {
    let existing = match std::fs::read_to_string(gitignore) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(HomeError::io(gitignore, error)),
    };
    if existing
        .lines()
        .any(|line| matches!(line.trim(), ".harw/" | "/.harw/"))
    {
        return Ok(());
    }
    let separator = if existing.is_empty() || existing.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    let updated = format!("{existing}{separator}{ROOT_GITIGNORE_RULE}\n");
    write_atomic(
        gitignore,
        updated.as_bytes(),
        AtomicWriteOptions::with_mode(0o644),
    )
    .map_err(|error| HomeError::io(gitignore, error))
}

/// Baut den `std::io::Error`, der [`HomeError::io`] beschreibt, wenn
/// `ProjectHome::dir` unerwartet keinen Elternpfad hat.
fn project_home_without_parent() -> std::io::Error {
    std::io::Error::other("project home directory has no parent path")
}

/// Lehnt `/` und das System-Temp-Verzeichnis als Projekt-Root ab.
// Lehnt `/` als Projekt-Root ab und meldet, ob der Root das Home-Verzeichnis
// des Benutzers ist (dort dient `~/.harw` als Projekt-Home, ohne `.gitignore`).
fn classify_project_home_root(root: &Path) -> HomeResult<bool> {
    let canonical_root = std::fs::canonicalize(root).map_err(|error| HomeError::io(root, error))?;
    if canonical_root == Path::new("/")
        || temp_dir_canonical().is_some_and(|temp| temp == canonical_root)
    {
        return Err(HomeError::UnsupportedProjectHomeRoot {
            root: canonical_root,
        });
    }
    let is_user_home = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .is_some_and(|home_path| {
            std::fs::canonicalize(&home_path).unwrap_or(home_path) == canonical_root
        });
    Ok(is_user_home)
}

/// Legt ein Verzeichnis (rekursiv) mit `0700` an; bestehende Rechte anderer
/// Verzeichnisse bleiben unberührt.
fn create_private_dir(path: &Path) -> HomeResult<()> {
    std::fs::create_dir_all(path).map_err(|error| HomeError::io(path, error))?;
    harden_dir(path)
}

/// Setzt `0o700` auf ein Verzeichnis (nur Unix; sonst No-op).
#[cfg(unix)]
fn harden_dir(path: &Path) -> HomeResult<()> {
    use std::os::unix::fs::PermissionsExt;
    let permissions = std::fs::Permissions::from_mode(0o700);
    std::fs::set_permissions(path, permissions).map_err(|error| HomeError::io(path, error))
}

#[cfg(not(unix))]
fn harden_dir(_path: &Path) -> HomeResult<()> {
    Ok(())
}

/// Pfad des projektbezogenen, autoritätsgewährenden Settings-Verzeichnisses.
///
/// # Description
/// `~/.harw/profiles/<profile>/projects/<key>` — der Ort, an dem
/// Allow-Regeln, Extra-Workdirs und der projekt-lokale Default-Modus liegen
/// (Contract §2): bewusst außerhalb des Repos, damit ein geklontes Projekt
/// sich keine Rechte selbst geben kann.
///
/// # Arguments
/// - `home` (`&Path`): Root-Space (siehe [`crate::paths::home_dir`]).
/// - `profile` (`&str`): aktives Profil.
/// - `key` (`&str`): Projekt-Schlüssel, üblicherweise aus [`project_key`].
///
/// # Returns
/// Den aufgelösten Pfad; wird nicht angelegt.
///
/// # Errors
/// - [`HomeError::InvalidProfileName`]: `profile` enthält unzulässige
///   Zeichen ([`crate::paths::profile_dir`]).
/// - [`HomeError::InvalidProjectKey`]: `key` enthält Zeichen außerhalb von
///   `[A-Za-z0-9_-]` (Traversal-Schutz, gleiche Regel wie
///   [`crate::paths::is_valid_profile_name`]).
///
/// # Examples
/// ```rust
/// use harw_home::project::project_settings_dir;
/// use std::path::Path;
///
/// let home = Path::new("/tmp/harw-example-home");
/// let dir = project_settings_dir(home, "default", "beispiel-0123456789ab").unwrap();
/// assert_eq!(
///     dir,
///     home.join("profiles/default/projects/beispiel-0123456789ab")
/// );
/// ```
pub fn project_settings_dir(home: &Path, profile: &str, key: &str) -> HomeResult<PathBuf> {
    if !paths::is_valid_profile_name(key) {
        return Err(HomeError::InvalidProjectKey {
            key: key.to_owned(),
        });
    }
    Ok(paths::profile_dir(home, profile)?
        .join("projects")
        .join(key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};
    use std::os::unix::fs::PermissionsExt;

    /// Temporäres Verzeichnis, das beim Drop entfernt wird.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> TestResult<Self> {
            let path = std::env::temp_dir().join(format!(
                "harw-home-project-{label}-{}",
                uuid::Uuid::now_v7()
            ));
            std::fs::create_dir_all(&path)?;
            Ok(Self(path))
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write(path: &Path, contents: &str) -> TestResult {
        let parent = path
            .parent()
            .ok_or(TestError::Missing("Path::parent() der Testdatei"))?;
        std::fs::create_dir_all(parent)?;
        std::fs::write(path, contents)?;
        Ok(())
    }

    #[test]
    fn discover_project_stops_at_home_directory_and_falls_back_to_cwd() -> TestResult {
        // Ohne Stopper stiege eine markerlose Sitzung in `$HOME` bis `/` auf
        // und lieferte das Home-Verzeichnis selbst als Root — genau der Wert,
        // den `ProjectHome::ensure` fail-closed ablehnt.
        let home = TempDir::new("home-stopper")?;
        let work = home.path().join("scratch");
        std::fs::create_dir_all(&work)?;
        let canonical_home = std::fs::canonicalize(home.path())?;

        let project = discover_project_with_home_stop(&work, &[], Some(canonical_home.as_path()))?;

        let expected = std::fs::canonicalize(&work)?;
        assert_eq!(project.root, expected);
        assert_eq!(project.kind, ProjectKind::Directory);
        Ok(())
    }

    #[test]
    fn discover_project_ignores_a_marker_inside_the_home_directory() -> TestResult {
        // Selbst ein versehentliches `~/.git` bindet die Sitzung nicht an das
        // Home-Verzeichnis: die Suche endet vorher, das Arbeitsverzeichnis
        // bleibt der Projekt-Root.
        let home = TempDir::new("home-marker")?;
        std::fs::create_dir_all(home.path().join(".git"))?;
        let work = home.path().join("scratch");
        std::fs::create_dir_all(&work)?;
        let canonical_home = std::fs::canonicalize(home.path())?;

        let project = discover_project_with_home_stop(&work, &[], Some(canonical_home.as_path()))?;

        assert_eq!(project.root, std::fs::canonicalize(&work)?);
        assert_eq!(project.kind, ProjectKind::Directory);
        Ok(())
    }

    #[test]
    fn discover_project_finds_git_dir_from_nested_cwd() -> TestResult {
        let repo = TempDir::new("git-repo")?;
        std::fs::create_dir_all(repo.path().join(".git"))?;
        let nested = repo.path().join("a/b/c");
        std::fs::create_dir_all(&nested)?;

        let project = discover_project(&nested, &[])?;

        let expected_root = std::fs::canonicalize(repo.path())?;
        assert_eq!(project.root, expected_root);
        assert_eq!(project.trust_key, expected_root);
        assert_eq!(project.kind, ProjectKind::Git);
        Ok(())
    }

    #[test]
    fn discover_project_maps_worktree_git_file_to_main_repo_trust_key() -> TestResult {
        let main_repo = TempDir::new("main-repo")?;
        let git_dir = main_repo.path().join(".git");
        let worktree_gitdir = git_dir.join("worktrees").join("wt1");
        std::fs::create_dir_all(&worktree_gitdir)?;
        write(&worktree_gitdir.join("commondir"), "../..\n")?;

        let worktree = TempDir::new("worktree")?;
        write(
            &worktree.path().join(".git"),
            &format!("gitdir: {}\n", worktree_gitdir.display()),
        )?;

        let project = discover_project(worktree.path(), &[])?;

        let expected_root = std::fs::canonicalize(worktree.path())?;
        let expected_trust_key = std::fs::canonicalize(main_repo.path())?;
        assert_eq!(project.root, expected_root);
        assert_eq!(project.kind, ProjectKind::GitWorktree);
        assert_eq!(project.trust_key, expected_trust_key);
        Ok(())
    }

    #[test]
    fn discover_project_falls_back_to_root_trust_key_on_unparsable_worktree_pointer() -> TestResult
    {
        let worktree = TempDir::new("broken-worktree")?;
        write(&worktree.path().join(".git"), "not-a-gitdir-line\n")?;

        let project = discover_project(worktree.path(), &[])?;

        let expected_root = std::fs::canonicalize(worktree.path())?;
        assert_eq!(project.kind, ProjectKind::GitWorktree);
        assert_eq!(project.trust_key, expected_root);
        Ok(())
    }

    #[test]
    fn discover_project_yields_directory_when_no_marker_is_found() -> TestResult {
        let plain = TempDir::new("plain")?;

        let project = discover_project(plain.path(), &[])?;

        let expected_root = std::fs::canonicalize(plain.path())?;
        assert_eq!(project.root, expected_root);
        assert_eq!(project.trust_key, expected_root);
        assert_eq!(project.kind, ProjectKind::Directory);
        Ok(())
    }

    #[test]
    fn discover_project_honors_custom_marker() -> TestResult {
        let repo = TempDir::new("custom-marker-repo")?;
        write(&repo.path().join("PROJECT_MARKER"), "")?;
        let nested = repo.path().join("nested");
        std::fs::create_dir_all(&nested)?;

        let markers = vec!["PROJECT_MARKER".to_owned()];
        let project = discover_project(&nested, &markers)?;

        let expected_root = std::fs::canonicalize(repo.path())?;
        assert_eq!(project.root, expected_root);
        assert_eq!(project.trust_key, expected_root);
        assert_eq!(
            project.kind,
            ProjectKind::Marker("PROJECT_MARKER".to_owned())
        );
        Ok(())
    }

    #[test]
    fn project_key_is_stable_and_sanitized() -> TestResult {
        let root = Path::new("/home/mia/projects/Beispiel Projekt!");
        let key = project_key(root);
        let again = project_key(root);
        assert_eq!(key, again, "project_key must be deterministic");
        assert!(key.starts_with("Beispiel-Projekt--"), "{key}");
        let (basename, hash) = key
            .rsplit_once('-')
            .ok_or(TestError::Missing("'-'-Trenner im project_key"))?;
        assert_eq!(hash.len(), 12);
        assert!(hash.bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(
            basename
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        );
        Ok(())
    }

    #[test]
    fn project_key_caps_basename_length() -> TestResult {
        let long_name = "a".repeat(100);
        let root = Path::new("/tmp").join(&long_name);
        let key = project_key(&root);
        let (basename, _hash) = key
            .rsplit_once('-')
            .ok_or(TestError::Missing("'-'-Trenner im project_key"))?;
        assert_eq!(basename.len(), 40);
        Ok(())
    }

    #[test]
    fn project_key_falls_back_to_root_for_rootless_path() {
        let key = project_key(Path::new("/"));
        assert!(key.starts_with("root-"), "{key}");
    }

    #[test]
    fn ensure_creates_directories_and_gitignore() -> TestResult {
        let repo = TempDir::new("ensure-repo")?;
        let project = ProjectRoot {
            trust_key: repo.path().to_path_buf(),
            root: repo.path().to_path_buf(),
            kind: ProjectKind::Directory,
        };
        let home = ProjectHome::at(&project);

        home.ensure()?;

        for dir in [
            &home.dir,
            &home.memories_dir(),
            &home.plans_dir(),
            &home.goals_dir(),
            &home.state_dir(),
        ] {
            assert!(dir.is_dir(), "{dir:?} must exist");
            let mode = std::fs::metadata(dir)?.permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "{dir:?} must be 0700");
        }

        let gitignore = repo.path().join(".gitignore");
        assert_eq!(std::fs::read_to_string(&gitignore)?, ".harw/\n");

        // Re-run is idempotent and preserves existing rules while adding the
        // one required rule exactly once.
        std::fs::write(&gitignore, "custom\n")?;
        home.ensure()?;
        assert_eq!(std::fs::read_to_string(&gitignore)?, "custom\n.harw/\n");
        home.ensure()?;
        assert_eq!(std::fs::read_to_string(&gitignore)?, "custom\n.harw/\n");
        Ok(())
    }

    #[test]
    fn ensure_tolerates_world_writable_root_without_sticky_bit() -> TestResult {
        let repo = TempDir::new("ensure-shared")?;
        std::fs::create_dir_all(repo.path().join(".git").join("info"))?;
        std::fs::set_permissions(repo.path(), std::fs::Permissions::from_mode(0o777))?;
        let project = ProjectRoot {
            trust_key: repo.path().to_path_buf(),
            root: repo.path().to_path_buf(),
            kind: ProjectKind::Directory,
        };
        let home = ProjectHome::at(&project);

        home.ensure()?;

        assert!(home.state_dir().is_dir());
        assert!(!repo.path().join(".gitignore").exists());
        let exclude = repo.path().join(".git").join("info").join("exclude");
        assert_eq!(std::fs::read_to_string(&exclude)?, ".harw/\n");
        home.ensure()?;
        assert_eq!(std::fs::read_to_string(&exclude)?, ".harw/\n");
        std::fs::set_permissions(repo.path(), std::fs::Permissions::from_mode(0o700))?;
        Ok(())
    }

    #[test]
    fn ensure_refuses_filesystem_root() {
        let project = ProjectRoot {
            trust_key: PathBuf::from("/"),
            root: PathBuf::from("/"),
            kind: ProjectKind::Directory,
        };
        let home = ProjectHome::at(&project);

        let result = home.ensure();

        assert!(matches!(
            result,
            Err(HomeError::UnsupportedProjectHomeRoot { .. })
        ));
        assert!(!home.dir.exists(), "must never attempt to create /.harw");
    }

    #[test]
    fn project_settings_dir_rejects_invalid_key() {
        let home = Path::new("/tmp/harw-example-home");
        assert!(matches!(
            project_settings_dir(home, "default", "../escape"),
            Err(HomeError::InvalidProjectKey { .. })
        ));
    }

    #[test]
    fn project_settings_dir_builds_expected_path() -> TestResult {
        let home = Path::new("/tmp/harw-example-home");
        let dir = project_settings_dir(home, "default", "beispiel-0123456789ab")?;
        assert_eq!(
            dir,
            home.join("profiles/default/projects/beispiel-0123456789ab")
        );
        Ok(())
    }
}
