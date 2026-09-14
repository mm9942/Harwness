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

/// Inhalt von `.harw/.gitignore`, wie von [`ProjectHome::ensure`] geschrieben.
const PROJECT_GITIGNORE_CONTENTS: &str = "state/\n";

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
    let canonical_cwd = std::fs::canonicalize(cwd).map_err(|error| HomeError::io(cwd, error))?;
    let effective_markers: Vec<&str> = if markers.is_empty() {
        vec![DEFAULT_MARKER]
    } else {
        markers.iter().map(String::as_str).collect()
    };

    for ancestor in canonical_cwd.ancestors() {
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

    Ok(ProjectRoot {
        trust_key: canonical_cwd.clone(),
        root: canonical_cwd,
        kind: ProjectKind::Directory,
    })
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
        let trust_key = resolve_worktree_trust_key(&root, &git_path).unwrap_or_else(|| root.clone());
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
    if out.is_empty() { "root".to_owned() } else { out }
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

    /// Legt die Projekt-Home-Verzeichnisse (`0700`) und `.harw/.gitignore`
    /// idempotent an.
    ///
    /// # Description
    /// Lehnt Roots ab, für die kein Projekt-Home entstehen darf: das
    /// Dateisystem-Root (`/`) und das Home-Verzeichnis des Benutzers selbst
    /// (`$HOME`, kanonisiert). `.harw/.gitignore` wird nur geschrieben, wenn
    /// es noch nicht existiert, und trägt ausschließlich `state/\n` — der
    /// lokale Zustand soll nie versehentlich committet werden, auch wenn der
    /// Rest von `.harw` versioniert wird.
    ///
    /// # Returns
    /// `Ok(())`, wenn alle Verzeichnisse existieren und `.gitignore`
    /// geschrieben ist (oder bereits war).
    ///
    /// # Errors
    /// - [`HomeError::UnsupportedProjectHomeRoot`]: `root` ist `/` oder
    ///   `$HOME`.
    /// - [`HomeError::Io`]: `root` nicht kanonisierbar, Anlegen eines
    ///   Verzeichnisses oder Schreiben von `.gitignore` schlägt fehl.
    pub fn ensure(&self) -> HomeResult<()> {
        let root = self
            .dir
            .parent()
            .ok_or_else(|| HomeError::io(&self.dir, project_home_without_parent()))?;
        refuse_unsupported_root(root)?;

        for dir in [
            self.dir.clone(),
            self.memories_dir(),
            self.plans_dir(),
            self.goals_dir(),
            self.state_dir(),
        ] {
            create_private_dir(&dir)?;
        }

        let gitignore = self.dir.join(".gitignore");
        if !gitignore.exists() {
            write_atomic(
                &gitignore,
                PROJECT_GITIGNORE_CONTENTS.as_bytes(),
                AtomicWriteOptions::with_mode(0o644),
            )
            .map_err(|error| HomeError::io(&gitignore, error))?;
        }
        Ok(())
    }
}

/// Baut den `std::io::Error`, der [`HomeError::io`] beschreibt, wenn
/// `ProjectHome::dir` unerwartet keinen Elternpfad hat.
fn project_home_without_parent() -> std::io::Error {
    std::io::Error::other("project home directory has no parent path")
}

/// Lehnt `/` und das kanonisierte `$HOME` als Projekt-Root ab.
fn refuse_unsupported_root(root: &Path) -> HomeResult<()> {
    let canonical_root = std::fs::canonicalize(root).map_err(|error| HomeError::io(root, error))?;
    if canonical_root == Path::new("/") {
        return Err(HomeError::UnsupportedProjectHomeRoot {
            root: canonical_root,
        });
    }
    if let Some(home) = std::env::var_os("HOME").filter(|value| !value.is_empty()) {
        let home_path = PathBuf::from(home);
        let canonical_home = std::fs::canonicalize(&home_path).unwrap_or(home_path);
        if canonical_root == canonical_home {
            return Err(HomeError::UnsupportedProjectHomeRoot {
                root: canonical_root,
            });
        }
    }
    Ok(())
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
    Ok(paths::profile_dir(home, profile)?.join("projects").join(key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Temporäres Verzeichnis, das beim Drop entfernt wird.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "harw-home-project-{label}-{}",
                uuid::Uuid::now_v7()
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
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

    fn write(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    #[test]
    fn discover_project_finds_git_dir_from_nested_cwd() {
        let repo = TempDir::new("git-repo");
        std::fs::create_dir_all(repo.path().join(".git")).unwrap();
        let nested = repo.path().join("a/b/c");
        std::fs::create_dir_all(&nested).unwrap();

        let project = discover_project(&nested, &[]).unwrap();

        let expected_root = std::fs::canonicalize(repo.path()).unwrap();
        assert_eq!(project.root, expected_root);
        assert_eq!(project.trust_key, expected_root);
        assert_eq!(project.kind, ProjectKind::Git);
    }

    #[test]
    fn discover_project_maps_worktree_git_file_to_main_repo_trust_key() {
        let main_repo = TempDir::new("main-repo");
        let git_dir = main_repo.path().join(".git");
        let worktree_gitdir = git_dir.join("worktrees").join("wt1");
        std::fs::create_dir_all(&worktree_gitdir).unwrap();
        write(&worktree_gitdir.join("commondir"), "../..\n");

        let worktree = TempDir::new("worktree");
        write(
            &worktree.path().join(".git"),
            &format!("gitdir: {}\n", worktree_gitdir.display()),
        );

        let project = discover_project(worktree.path(), &[]).unwrap();

        let expected_root = std::fs::canonicalize(worktree.path()).unwrap();
        let expected_trust_key = std::fs::canonicalize(main_repo.path()).unwrap();
        assert_eq!(project.root, expected_root);
        assert_eq!(project.kind, ProjectKind::GitWorktree);
        assert_eq!(project.trust_key, expected_trust_key);
    }

    #[test]
    fn discover_project_falls_back_to_root_trust_key_on_unparsable_worktree_pointer() {
        let worktree = TempDir::new("broken-worktree");
        write(&worktree.path().join(".git"), "not-a-gitdir-line\n");

        let project = discover_project(worktree.path(), &[]).unwrap();

        let expected_root = std::fs::canonicalize(worktree.path()).unwrap();
        assert_eq!(project.kind, ProjectKind::GitWorktree);
        assert_eq!(project.trust_key, expected_root);
    }

    #[test]
    fn discover_project_yields_directory_when_no_marker_is_found() {
        let plain = TempDir::new("plain");

        let project = discover_project(plain.path(), &[]).unwrap();

        let expected_root = std::fs::canonicalize(plain.path()).unwrap();
        assert_eq!(project.root, expected_root);
        assert_eq!(project.trust_key, expected_root);
        assert_eq!(project.kind, ProjectKind::Directory);
    }

    #[test]
    fn discover_project_honors_custom_marker() {
        let repo = TempDir::new("custom-marker-repo");
        write(&repo.path().join("PROJECT_MARKER"), "");
        let nested = repo.path().join("nested");
        std::fs::create_dir_all(&nested).unwrap();

        let markers = vec!["PROJECT_MARKER".to_owned()];
        let project = discover_project(&nested, &markers).unwrap();

        let expected_root = std::fs::canonicalize(repo.path()).unwrap();
        assert_eq!(project.root, expected_root);
        assert_eq!(project.trust_key, expected_root);
        assert_eq!(
            project.kind,
            ProjectKind::Marker("PROJECT_MARKER".to_owned())
        );
    }

    #[test]
    fn project_key_is_stable_and_sanitized() {
        let root = Path::new("/home/mia/projects/Beispiel Projekt!");
        let key = project_key(root);
        let again = project_key(root);
        assert_eq!(key, again, "project_key must be deterministic");
        assert!(key.starts_with("Beispiel-Projekt--"), "{key}");
        let (basename, hash) = key.rsplit_once('-').unwrap();
        assert_eq!(hash.len(), 12);
        assert!(hash.bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(basename.bytes().all(|b| b.is_ascii_alphanumeric()
            || b == b'-'
            || b == b'_'));
    }

    #[test]
    fn project_key_caps_basename_length() {
        let long_name = "a".repeat(100);
        let root = Path::new("/tmp").join(&long_name);
        let key = project_key(&root);
        let (basename, _hash) = key.rsplit_once('-').unwrap();
        assert_eq!(basename.len(), 40);
    }

    #[test]
    fn project_key_falls_back_to_root_for_rootless_path() {
        let key = project_key(Path::new("/"));
        assert!(key.starts_with("root-"), "{key}");
    }

    #[test]
    fn ensure_creates_directories_and_gitignore() {
        let repo = TempDir::new("ensure-repo");
        let project = ProjectRoot {
            trust_key: repo.path().to_path_buf(),
            root: repo.path().to_path_buf(),
            kind: ProjectKind::Directory,
        };
        let home = ProjectHome::at(&project);

        home.ensure().unwrap();

        for dir in [
            &home.dir,
            &home.memories_dir(),
            &home.plans_dir(),
            &home.goals_dir(),
            &home.state_dir(),
        ] {
            assert!(dir.is_dir(), "{dir:?} must exist");
            let mode = std::fs::metadata(dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "{dir:?} must be 0700");
        }

        let gitignore = home.dir.join(".gitignore");
        assert_eq!(
            std::fs::read_to_string(&gitignore).unwrap(),
            "state/\n"
        );

        // Re-run is idempotent and never overwrites a customized gitignore.
        std::fs::write(&gitignore, "custom\n").unwrap();
        home.ensure().unwrap();
        assert_eq!(std::fs::read_to_string(&gitignore).unwrap(), "custom\n");
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
        assert!(
            !home.dir.exists(),
            "must never attempt to create /.harw"
        );
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
    fn project_settings_dir_builds_expected_path() {
        let home = Path::new("/tmp/harw-example-home");
        let dir = project_settings_dir(home, "default", "beispiel-0123456789ab").unwrap();
        assert_eq!(
            dir,
            home.join("profiles/default/projects/beispiel-0123456789ab")
        );
    }
}
