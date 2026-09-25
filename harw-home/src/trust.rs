//! Projekt-Trust für repo-lokale `.harw`-Layer (P0.8a, Register F-103).
//!
//! Ein repo-lokales `./.harw` darf die Konfiguration nur dann mit voller
//! Autorität überschreiben (Provider mit eigener `base_url`, `auth.toml`,
//! `.env`, MCP-Server …), wenn der Nutzer das Projekt **ausdrücklich**
//! freigegeben hat. Die Freigabe ist an drei Dinge gebunden:
//!
//! - den **kanonischen** Projekt-Root (`std::fs::canonicalize`, keine
//!   Symlinks im Schlüssel),
//! - die Eigentümer-UID dieses Roots zum Freigabezeitpunkt und
//! - einen BLAKE3-Digest über die sicherheitsrelevanten Dateien im
//!   Projekt-`.harw` ([`TRUST_DIGEST_FILES`], [`TRUST_DIGEST_DIRS`]).
//!
//! Ändert sich eines davon, meldet [`project_trust_status`]
//! [`TrustStatus::Changed`] — der Repo-Layer fällt dann wie ein nie
//! freigegebener auf die eingeschränkte Übernahme zurück, bis der Nutzer
//! erneut freigibt.
//!
//! # Speicherort und Format
//! `<home>/trusted-projects.toml`, geschrieben ausschließlich über
//! [`harw_fsutil::write_atomic`] mit Rechten `0600`. Beim Lesen wird die Datei
//! ohne Symlink-Folge geöffnet und muss dem effektiven Benutzer gehören und
//! privat sein ([`harw_fsutil::ensure_private_regular`]); sonst Fehler statt
//! stiller Leerannahme.
//!
//! ```toml
//! version = 1
//!
//! [[project]]
//! canonical_root = "/home/user/projects/beispiel"
//! owner_uid = 1000
//! digest = "blake3:…64 Hex-Zeichen…"
//! ```
//!
//! # Digest
//! Sortiert nach relativem Pfad (bytewise) gehen je Datei Pfadlänge, Pfad,
//! Inhaltslänge und Inhalt (jeweils Länge als `u64` little-endian) nach einer
//! Domänenkennung in den Hash ein. Gelesen wird ausschließlich symlinkfrei:
//! Verzeichnisse über [`harw_fsutil::walk_beneath`], Dateiinhalte über
//! [`harw_fsutil::open_beneath`] relativ zum Deskriptor des `.harw`.
//! Symlinks, FIFOs, Geräte und Grenzüberschreitungen machen das Projekt
//! **nicht vertrauenswürdig** ([`HomeError::UntrustableProject`]) — ein
//! Symlink wird nie gefolgt, und weil die Config-Discovery Symlinks sehr wohl
//! folgen würde, darf ein Digest über den Link selbst keine Freigabe tragen.
//!
//! # Plattform
//! Unix-only (wie `harw-fsutil`): Eigentümer-UID über
//! [`std::os::unix::fs::MetadataExt`].
//!
//! # Concurrency
//! Kein Lock: zwei gleichzeitige [`trust_project`]/[`untrust_project`]-Aufrufe
//! können sich gegenseitig überschreiben (letzter `rename` gewinnt, die Datei
//! bleibt aber immer vollständig). Zwischen Digest-Prüfung und späterem Laden
//! der Config liegt ein unvermeidbares Fenster; wer im Projekt schreiben
//! darf, kann in diesem Fenster Dateien tauschen.
//!
//! # Examples
//! ```rust,no_run
//! use harw_home::trust::{TrustStatus, project_trust_status, trust_project};
//! use std::path::Path;
//!
//! let home = Path::new("/home/user/.harw");
//! let repo = Path::new("/home/user/projects/beispiel");
//! if project_trust_status(home, repo)? != TrustStatus::Trusted {
//!     let record = trust_project(home, repo)?;
//!     println!("vertraut: {} ({})", record.canonical_root.display(), record.digest);
//! }
//! # Ok::<(), harw_home::HomeError>(())
//! ```

use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use harw_fsutil::{
    AtomicWriteOptions, EntryType, OpenMode, WalkLimits, ensure_private_regular, open_beneath,
    open_dir_nofollow, open_nofollow, walk_beneath, write_atomic,
};
use serde::{Deserialize, Serialize};

use crate::error::{HomeError, HomeResult};

/// Dateiname des Trust-Stores unterhalb des Root-Space.
pub const TRUST_STORE_FILE: &str = "trusted-projects.toml";

/// Einzeldateien direkt im Projekt-`.harw`, die in den Digest eingehen.
pub const TRUST_DIGEST_FILES: &[&str] = &["config.toml", "auth.toml", ".env"];

/// Verzeichnisse im Projekt-`.harw`, deren gesamter Inhalt (rekursiv) in den
/// Digest eingeht.
///
/// Obermenge der Vorgabe (`providers`, `mcps`, `agents`, `plugins`): auch
/// `models`, `skills` und `channels` werden von `harw_config::discover_config`
/// aus einem vertrauten Layer geladen (Channel-Bindungen mit Token-Refs,
/// Skill-Instruktionen) und dürfen sich nach der Freigabe nicht unbemerkt
/// ändern.
pub const TRUST_DIGEST_DIRS: &[&str] = &[
    "providers",
    "mcps",
    "agents",
    "plugins",
    "models",
    "skills",
    "channels",
];

/// Aktuelle Formatversion von `trusted-projects.toml`.
const TRUST_STORE_VERSION: u32 = 1;
/// Obergrenze für die Größe des Trust-Stores beim Lesen.
const MAX_TRUST_STORE_BYTES: u64 = 1024 * 1024;
/// Obergrenze je Datei, die in den Digest eingeht.
const MAX_DIGEST_FILE_BYTES: u64 = 4 * 1024 * 1024;
/// Obergrenze aller Dateien, die in den Digest eingehen.
const MAX_DIGEST_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
/// Maximale Verzeichnistiefe je Digest-Verzeichnis.
const DIGEST_WALK_MAX_DEPTH: usize = 16;
/// Maximale Einträge je Digest-Verzeichnis.
const DIGEST_WALK_MAX_ENTRIES: usize = 10_000;
/// Zeitbudget je Digest-Verzeichnis.
const DIGEST_WALK_TIMEOUT: Duration = Duration::from_secs(10);
/// Domänenkennung vor den Hash-Daten (Versionierung des Digest-Formats).
const DIGEST_DOMAIN: &[u8] = b"harw-project-trust/v1\0";
/// Präfix der Digest-Zeichenkette.
const DIGEST_PREFIX: &str = "blake3:";

/// Eine gespeicherte Projekt-Freigabe.
///
/// # Examples
/// ```rust
/// use harw_home::trust::TrustRecord;
/// use std::path::PathBuf;
///
/// let record = TrustRecord {
///     canonical_root: PathBuf::from("/home/user/projects/beispiel"),
///     owner_uid: 1000,
///     digest: format!("blake3:{}", "0".repeat(64)),
/// };
/// assert!(record.canonical_root.is_absolute());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustRecord {
    /// Kanonischer, absoluter Projekt-Root (das Verzeichnis, das `.harw`
    /// enthält).
    pub canonical_root: PathBuf,
    /// Eigentümer-UID des Projekt-Roots zum Freigabezeitpunkt.
    pub owner_uid: u32,
    /// `blake3:<64 Hex-Zeichen>` über die Digest-Dateien (siehe Modul-Doku).
    pub digest: String,
}

/// Vertrauensstatus eines Projekts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrustStatus {
    /// Freigegeben; Eigentümer und Digest stimmen mit der Freigabe überein.
    Trusted,
    /// Nie freigegeben (kein Eintrag im Trust-Store).
    Untrusted,
    /// Freigegeben, aber Eigentümer oder Digest weichen ab, oder der Digest
    /// ist nicht mehr berechenbar (z. B. neuer Symlink). Wird wie
    /// [`TrustStatus::Untrusted`] behandelt, bis erneut freigegeben wird.
    Changed,
}

/// Serialisierte Form von `trusted-projects.toml`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TrustFile {
    version: u32,
    #[serde(default, rename = "project")]
    projects: Vec<TrustRecord>,
}

/// In-Memory-Abbild von `<home>/trusted-projects.toml`.
///
/// # Examples
/// ```rust,no_run
/// use harw_home::trust::TrustStore;
/// use std::path::Path;
///
/// let store = TrustStore::load(Path::new("/home/user/.harw"))?;
/// for record in store.records() {
///     println!("{}", record.canonical_root.display());
/// }
/// # Ok::<(), harw_home::HomeError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustStore {
    path: PathBuf,
    records: Vec<TrustRecord>,
}

/// Pfad des Trust-Stores `<home>/trusted-projects.toml`.
///
/// # Examples
/// ```rust
/// use std::path::PathBuf;
/// let home = PathBuf::from("/tmp/harw-test-home");
/// assert_eq!(
///     harw_home::trust::trusted_projects_path(&home),
///     home.join("trusted-projects.toml")
/// );
/// ```
#[must_use]
pub fn trusted_projects_path(home: &Path) -> PathBuf {
    home.join(TRUST_STORE_FILE)
}

impl TrustStore {
    /// Lädt den Trust-Store; eine fehlende Datei ergibt einen leeren Store.
    ///
    /// # Errors
    /// - [`HomeError::Io`]: Datei ist ein Symlink (`ELOOP`), gehört nicht dem
    ///   effektiven Benutzer, ist für Gruppe/Andere zugänglich oder unlesbar.
    /// - [`HomeError::TrustStore`]: zu groß, kein UTF-8, ungültiges TOML,
    ///   unbekannte Version, relativer Root, ungültiger Digest oder doppelter
    ///   Root.
    pub fn load(home: &Path) -> HomeResult<Self> {
        let path = trusted_projects_path(home);
        let file = match open_nofollow(&path, OpenMode::read_only()) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Self {
                    path,
                    records: Vec::new(),
                });
            }
            Err(error) => return Err(HomeError::io(&path, error)),
        };
        ensure_private_regular(&file).map_err(|error| HomeError::io(&path, error))?;
        let bytes = read_capped(file, MAX_TRUST_STORE_BYTES)
            .map_err(|error| HomeError::io(&path, error))?
            .ok_or_else(|| {
                store_error(&path, format!("file exceeds {MAX_TRUST_STORE_BYTES} bytes"))
            })?;
        let text = String::from_utf8(bytes)
            .map_err(|_| store_error(&path, "file is not valid UTF-8".to_owned()))?;
        let parsed: TrustFile = toml::from_str(&text)
            .map_err(|error| store_error(&path, format!("invalid TOML: {error}")))?;
        if parsed.version != TRUST_STORE_VERSION {
            return Err(store_error(
                &path,
                format!(
                    "unsupported version {} (expected {TRUST_STORE_VERSION})",
                    parsed.version
                ),
            ));
        }
        for (index, record) in parsed.projects.iter().enumerate() {
            if !record.canonical_root.is_absolute() {
                return Err(store_error(
                    &path,
                    format!(
                        "project root {} is not absolute",
                        record.canonical_root.display()
                    ),
                ));
            }
            if !is_valid_digest(&record.digest) {
                return Err(store_error(
                    &path,
                    format!(
                        "project {} has a malformed digest",
                        record.canonical_root.display()
                    ),
                ));
            }
            if parsed.projects[..index]
                .iter()
                .any(|earlier| earlier.canonical_root == record.canonical_root)
            {
                return Err(store_error(
                    &path,
                    format!(
                        "project root {} is listed more than once",
                        record.canonical_root.display()
                    ),
                ));
            }
        }
        Ok(Self {
            path,
            records: parsed.projects,
        })
    }

    /// Schreibt den Store atomar mit Rechten `0600`.
    ///
    /// Legt ein fehlendes Home-Verzeichnis mit `0700` an.
    ///
    /// # Errors
    /// - [`HomeError::TrustStore`]: ein Root ist kein gültiges UTF-8 (TOML
    ///   kann ihn nicht darstellen).
    /// - [`HomeError::Io`]: Anlegen des Verzeichnisses oder
    ///   [`harw_fsutil::write_atomic`] scheitert.
    pub fn save(&self) -> HomeResult<()> {
        let file = TrustFile {
            version: TRUST_STORE_VERSION,
            projects: self.records.clone(),
        };
        let text = toml::to_string(&file)
            .map_err(|error| store_error(&self.path, format!("cannot serialize: {error}")))?;
        if let Some(parent) = self.path.parent() {
            ensure_private_dir(parent)?;
        }
        write_atomic(&self.path, text.as_bytes(), AtomicWriteOptions::private())
            .map_err(|error| HomeError::io(&self.path, error))
    }

    /// Pfad der zugrunde liegenden Datei.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Alle Freigaben in Dateireihenfolge.
    #[must_use]
    pub fn records(&self) -> &[TrustRecord] {
        &self.records
    }

    /// Freigabe für einen **kanonischen** Root.
    #[must_use]
    pub fn get(&self, canonical_root: &Path) -> Option<&TrustRecord> {
        self.records
            .iter()
            .find(|record| record.canonical_root == canonical_root)
    }

    /// Fügt eine Freigabe ein oder ersetzt die bestehende für denselben Root.
    pub fn upsert(&mut self, record: TrustRecord) {
        match self
            .records
            .iter_mut()
            .find(|existing| existing.canonical_root == record.canonical_root)
        {
            Some(existing) => *existing = record,
            None => self.records.push(record),
        }
    }

    /// Entfernt die Freigabe für einen kanonischen Root.
    ///
    /// # Returns
    /// `true`, wenn ein Eintrag entfernt wurde.
    pub fn remove(&mut self, canonical_root: &Path) -> bool {
        let before = self.records.len();
        self.records
            .retain(|record| record.canonical_root != canonical_root);
        self.records.len() != before
    }
}

/// Gibt das Projekt unter `root` frei (bzw. erneuert die Freigabe).
///
/// # Arguments
/// - `home` (`&Path`): Root-Space mit dem Trust-Store.
/// - `root` (`&Path`): Projekt-Root, also das Verzeichnis, das `.harw` enthält.
///
/// # Returns
/// Den gespeicherten [`TrustRecord`].
///
/// # Errors
/// - [`HomeError::Io`]: `root` existiert nicht / ist nicht kanonisierbar,
///   Lese- oder Schreibfehler.
/// - [`HomeError::UntrustableProject`]: Symlink, Sondertyp oder Grenze im
///   Projekt-`.harw`.
/// - [`HomeError::TrustStore`]: bestehender Store fehlerhaft.
pub fn trust_project(home: &Path, root: &Path) -> HomeResult<TrustRecord> {
    let canonical_root = canonical_project_root(root)?;
    let digest = project_digest(&canonical_root)?;
    let owner_uid = owner_uid(&canonical_root)?;
    let record = TrustRecord {
        canonical_root,
        owner_uid,
        digest,
    };
    let mut store = TrustStore::load(home)?;
    store.upsert(record.clone());
    store.save()?;
    Ok(record)
}

/// Entzieht die Freigabe für das Projekt unter `root`.
///
/// Existiert `root` nicht mehr, wird der Pfad unverändert als Schlüssel
/// verwendet (so lässt sich ein gelöschtes Projekt über seinen früheren
/// absoluten Pfad austragen).
///
/// # Returns
/// `true`, wenn eine Freigabe entfernt wurde; der Store wird nur dann
/// geschrieben.
///
/// # Errors
/// [`HomeError::Io`] / [`HomeError::TrustStore`] wie bei [`TrustStore::load`]
/// und [`TrustStore::save`].
pub fn untrust_project(home: &Path, root: &Path) -> HomeResult<bool> {
    let key = match std::fs::canonicalize(root) {
        Ok(path) => path,
        Err(error) if error.kind() == io::ErrorKind::NotFound => root.to_path_buf(),
        Err(error) => return Err(HomeError::io(root, error)),
    };
    let mut store = TrustStore::load(home)?;
    let removed = store.remove(&key);
    if removed {
        store.save()?;
    }
    Ok(removed)
}

/// Ermittelt den Vertrauensstatus des Projekts unter `root`.
///
/// Der Digest wird nur für Projekte mit Eintrag berechnet; ein nie
/// freigegebenes Projekt wird nicht gelesen. Jeder Fehler bei der
/// Digest-Berechnung eines eingetragenen Projekts ergibt
/// [`TrustStatus::Changed`] (fail-closed, aber kein Startabbruch).
///
/// # Errors
/// - [`HomeError::Io`]: `root` ist nicht kanonisierbar.
/// - [`HomeError::Io`] / [`HomeError::TrustStore`]: Trust-Store unlesbar oder
///   fehlerhaft.
pub fn project_trust_status(home: &Path, root: &Path) -> HomeResult<TrustStatus> {
    let canonical_root = canonical_project_root(root)?;
    let store = TrustStore::load(home)?;
    let Some(record) = store.get(&canonical_root) else {
        return Ok(TrustStatus::Untrusted);
    };
    match owner_uid(&canonical_root) {
        Ok(uid) if uid == record.owner_uid => {}
        _ => return Ok(TrustStatus::Changed),
    }
    match project_digest(&canonical_root) {
        Ok(digest) if digest == record.digest => Ok(TrustStatus::Trusted),
        _ => Ok(TrustStatus::Changed),
    }
}

/// Berechnet den Trust-Digest des Projekt-`.harw` unter `root`.
///
/// Fehlt `.harw`, ist der Digest der einer leeren Dateimenge (ein später
/// angelegtes `.harw` ändert ihn also).
///
/// # Errors
/// - [`HomeError::UntrustableProject`]: `.harw` oder ein Eintrag ist Symlink
///   oder Sondertyp, eine Einzeldatei aus [`TRUST_DIGEST_FILES`] ist keine
///   reguläre Datei, ein Eintrag aus [`TRUST_DIGEST_DIRS`] kein Verzeichnis,
///   oder Größen-/Tiefen-/Anzahl-/Zeitgrenzen sind überschritten.
/// - [`HomeError::Io`][]: Lesefehler.
pub fn project_digest(root: &Path) -> HomeResult<String> {
    let harw_dir = root.join(crate::paths::project_dir_name());
    let mut entries: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    match std::fs::symlink_metadata(&harw_dir) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(untrustable(&harw_dir, "`.harw` is a symlink"));
        }
        Ok(meta) if !meta.is_dir() => {
            return Err(untrustable(&harw_dir, "`.harw` is not a directory"));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(finish_digest(&entries));
        }
        Err(error) => return Err(HomeError::io(&harw_dir, error)),
    }
    // Alle Inhalte werden relativ zu diesem Deskriptor gelesen; `open_beneath`
    // lehnt jeden Symlink in jedem Pfadglied ab.
    let harw_fd = open_dir_nofollow(&harw_dir).map_err(|error| HomeError::io(&harw_dir, error))?;
    let mut total_bytes = 0_u64;

    for name in TRUST_DIGEST_FILES {
        let path = harw_dir.join(name);
        let Some(meta) = symlink_metadata_opt(&path)? else {
            continue;
        };
        if !meta.file_type().is_file() {
            return Err(untrustable(
                &path,
                "expected a regular file (symlinks are never followed)",
            ));
        }
        let content = read_beneath(harw_fd.as_fd(), Path::new(name), &path, &mut total_bytes)?;
        entries.push((name.as_bytes().to_vec(), content));
    }

    for dir in TRUST_DIGEST_DIRS {
        let path = harw_dir.join(dir);
        let Some(meta) = symlink_metadata_opt(&path)? else {
            continue;
        };
        if !meta.file_type().is_dir() {
            return Err(untrustable(
                &path,
                "expected a directory (symlinks are never followed)",
            ));
        }
        let limits = WalkLimits {
            max_depth: DIGEST_WALK_MAX_DEPTH,
            max_entries: DIGEST_WALK_MAX_ENTRIES,
            deadline: Some(Instant::now() + DIGEST_WALK_TIMEOUT),
        };
        let mut walk = walk_beneath(&path, limits).map_err(|error| HomeError::io(&path, error))?;
        for item in walk.by_ref() {
            let entry = item.map_err(|error| HomeError::io(&path, error))?;
            let rel = Path::new(dir).join(&entry.rel_path);
            match entry.entry_type {
                EntryType::Dir => {}
                EntryType::File => {
                    let display = harw_dir.join(&rel);
                    let content = read_beneath(harw_fd.as_fd(), &rel, &display, &mut total_bytes)?;
                    entries.push((rel.as_os_str().as_bytes().to_vec(), content));
                }
                EntryType::Symlink => {
                    return Err(untrustable(
                        &harw_dir.join(&rel),
                        "symlinks are never followed and cannot be trusted",
                    ));
                }
                EntryType::Other => {
                    return Err(untrustable(
                        &harw_dir.join(&rel),
                        "special files (FIFO, socket, device) cannot be trusted",
                    ));
                }
            }
        }
        if let Some(stop) = walk.stopped() {
            return Err(untrustable(
                &path,
                format!("directory walk incomplete ({stop:?})"),
            ));
        }
    }

    // Global sortieren: die Vorordnung des Walks ist nicht identisch mit der
    // bytewisen Ordnung vollständiger Pfade (`a/b` vs. `a-c`).
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(finish_digest(&entries))
}

/// Kanonisiert den Projekt-Root.
fn canonical_project_root(root: &Path) -> HomeResult<PathBuf> {
    std::fs::canonicalize(root).map_err(|error| HomeError::io(root, error))
}

/// Eigentümer-UID eines (bereits kanonischen) Pfads.
fn owner_uid(canonical_root: &Path) -> HomeResult<u32> {
    std::fs::symlink_metadata(canonical_root)
        .map(|meta| meta.uid())
        .map_err(|error| HomeError::io(canonical_root, error))
}

/// `symlink_metadata`, `None` bei `NotFound`.
fn symlink_metadata_opt(path: &Path) -> HomeResult<Option<std::fs::Metadata>> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => Ok(Some(meta)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(HomeError::io(path, error)),
    }
}

/// Liest `rel` symlinkfrei unterhalb von `root` mit Größengrenzen.
fn read_beneath(
    root: BorrowedFd<'_>,
    rel: &Path,
    display: &Path,
    total_bytes: &mut u64,
) -> HomeResult<Vec<u8>> {
    let file = open_beneath(root, rel, OpenMode::read_only())
        .map_err(|error| HomeError::io(display, error))?;
    let meta = file
        .metadata()
        .map_err(|error| HomeError::io(display, error))?;
    if !meta.is_file() {
        return Err(untrustable(display, "expected a regular file"));
    }
    let content = read_capped(file, MAX_DIGEST_FILE_BYTES)
        .map_err(|error| HomeError::io(display, error))?
        .ok_or_else(|| {
            untrustable(
                display,
                format!("file exceeds {MAX_DIGEST_FILE_BYTES} bytes"),
            )
        })?;
    *total_bytes = total_bytes.saturating_add(len_u64(content.len()));
    if *total_bytes > MAX_DIGEST_TOTAL_BYTES {
        return Err(untrustable(
            display,
            format!("project configuration exceeds {MAX_DIGEST_TOTAL_BYTES} bytes in total"),
        ));
    }
    Ok(content)
}

/// Liest höchstens `limit` Bytes; `Ok(None)`, wenn die Datei größer ist.
fn read_capped(file: File, limit: u64) -> io::Result<Option<Vec<u8>>> {
    let mut buffer = Vec::new();
    file.take(limit.saturating_add(1))
        .read_to_end(&mut buffer)?;
    if len_u64(buffer.len()) > limit {
        Ok(None)
    } else {
        Ok(Some(buffer))
    }
}

/// Hash über bereits sortierte `(relativer Pfad, Inhalt)`-Paare.
fn finish_digest(entries: &[(Vec<u8>, Vec<u8>)]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(DIGEST_DOMAIN);
    hasher.update(&len_u64(entries.len()).to_le_bytes());
    for (path, content) in entries {
        hasher.update(&len_u64(path.len()).to_le_bytes());
        hasher.update(path);
        hasher.update(&len_u64(content.len()).to_le_bytes());
        hasher.update(content);
    }
    format!("{DIGEST_PREFIX}{}", hasher.finalize().to_hex())
}

/// `usize` → `u64` ohne stillen Überlauf (auf allen Zielen verlustfrei).
fn len_u64(len: usize) -> u64 {
    u64::try_from(len).unwrap_or(u64::MAX)
}

/// `blake3:` + 64 kleingeschriebene Hex-Zeichen.
fn is_valid_digest(digest: &str) -> bool {
    digest.strip_prefix(DIGEST_PREFIX).is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

/// Legt ein fehlendes Verzeichnis mit `0700` an; bestehende bleiben unberührt.
fn ensure_private_dir(dir: &Path) -> HomeResult<()> {
    if dir.as_os_str().is_empty() || dir.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(dir).map_err(|error| HomeError::io(dir, error))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| HomeError::io(dir, error))
}

fn store_error(path: &Path, reason: String) -> HomeError {
    HomeError::TrustStore {
        path: path.to_path_buf(),
        reason,
    }
}

fn untrustable(path: &Path, reason: impl Into<String>) -> HomeError {
    HomeError::UntrustableProject {
        path: path.to_path_buf(),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};
    use std::os::unix::fs::symlink;

    /// Temporäres Verzeichnis, das beim Drop entfernt wird.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> TestResult<Self> {
            let path = std::env::temp_dir()
                .join(format!("harw-home-trust-{label}-{}", uuid::Uuid::now_v7()));
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

    /// Projekt mit einer minimalen, gültigen `.harw`-Struktur.
    fn project(label: &str) -> TestResult<TempDir> {
        let dir = TempDir::new(label)?;
        write(
            &dir.path().join(".harw/config.toml"),
            "default_provider = \"local\"\n",
        )?;
        write(
            &dir.path().join(".harw/providers/local.toml"),
            "name = \"local\"\napi = \"local\"\nbase_url = \"https://localhost\"\n",
        )?;
        Ok(dir)
    }

    #[test]
    fn trust_and_untrust_round_trip_with_private_store() -> TestResult {
        let home = TempDir::new("home")?;
        let repo = project("repo")?;

        assert_eq!(
            project_trust_status(home.path(), repo.path())?,
            TrustStatus::Untrusted
        );

        let record = trust_project(home.path(), repo.path())?;
        assert_eq!(record.canonical_root, std::fs::canonicalize(repo.path())?);
        assert!(is_valid_digest(&record.digest), "{}", record.digest);
        assert_eq!(record.owner_uid, std::fs::metadata(repo.path())?.uid());
        assert_eq!(
            project_trust_status(home.path(), repo.path())?,
            TrustStatus::Trusted
        );

        let store_path = trusted_projects_path(home.path());
        let mode = std::fs::metadata(&store_path)?.mode() & 0o777;
        assert_eq!(mode, 0o600);
        let store = TrustStore::load(home.path())?;
        assert_eq!(store.records(), std::slice::from_ref(&record));

        // Erneutes Freigeben ersetzt, statt zu duplizieren.
        trust_project(home.path(), repo.path())?;
        assert_eq!(TrustStore::load(home.path())?.records().len(), 1);

        assert!(untrust_project(home.path(), repo.path())?);
        assert_eq!(
            project_trust_status(home.path(), repo.path())?,
            TrustStatus::Untrusted
        );
        assert!(!untrust_project(home.path(), repo.path())?);
        Ok(())
    }

    #[test]
    fn digest_change_turns_trusted_into_changed() -> TestResult {
        let home = TempDir::new("home")?;
        let repo = project("repo")?;
        trust_project(home.path(), repo.path())?;

        // Nicht digest-relevante Dateien ändern nichts.
        write(&repo.path().join(".harw/sessions/log.jsonl"), "{}\n")?;
        write(&repo.path().join("README.md"), "hallo\n")?;
        assert_eq!(
            project_trust_status(home.path(), repo.path())?,
            TrustStatus::Trusted
        );

        // Provider-Endpunkt umgebogen.
        write(
            &repo.path().join(".harw/providers/local.toml"),
            "name = \"local\"\napi = \"local\"\nbase_url = \"https://evil.example\"\n",
        )?;
        assert_eq!(
            project_trust_status(home.path(), repo.path())?,
            TrustStatus::Changed
        );

        // Erneute Freigabe übernimmt den neuen Stand.
        trust_project(home.path(), repo.path())?;
        assert_eq!(
            project_trust_status(home.path(), repo.path())?,
            TrustStatus::Trusted
        );

        // Neue Dateien in einem Digest-Verzeichnis und `.env` zählen ebenfalls.
        write(
            &repo.path().join(".harw/mcps/nested/server.toml"),
            "name = \"x\"\n",
        )?;
        assert_eq!(
            project_trust_status(home.path(), repo.path())?,
            TrustStatus::Changed
        );
        trust_project(home.path(), repo.path())?;
        write(&repo.path().join(".harw/.env"), "OPENAI_API_KEY=x\n")?;
        assert_eq!(
            project_trust_status(home.path(), repo.path())?,
            TrustStatus::Changed
        );
        Ok(())
    }

    #[test]
    fn digest_is_independent_of_creation_order() -> TestResult {
        let first = TempDir::new("order-a")?;
        let second = TempDir::new("order-b")?;
        let files = [
            (".harw/agents/a/agent.toml", "name = \"a\"\n"),
            (".harw/agents/a-c.toml", "x\n"),
            (".harw/config.toml", "\n"),
        ];
        for (rel, contents) in files {
            write(&first.path().join(rel), contents)?;
        }
        for (rel, contents) in files.iter().rev() {
            write(&second.path().join(rel), contents)?;
        }
        assert_eq!(
            project_digest(first.path())?,
            project_digest(second.path())?
        );
        // Pfad gehört zum Digest: gleicher Inhalt unter anderem Namen weicht ab.
        std::fs::rename(
            second.path().join(".harw/agents/a-c.toml"),
            second.path().join(".harw/agents/a-d.toml"),
        )?;
        assert_ne!(
            project_digest(first.path())?,
            project_digest(second.path())?
        );
        Ok(())
    }

    #[test]
    fn symlinks_inside_harw_are_not_followed() -> TestResult {
        let home = TempDir::new("home")?;
        let outside = TempDir::new("outside")?;
        write(
            &outside.path().join("evil.toml"),
            "name = \"openai\"\napi = \"openai\"\nbase_url = \"https://evil.example\"\n",
        )?;

        // Symlink auf eine Datei in einem Digest-Verzeichnis.
        let repo = project("symlink-file")?;
        symlink(
            outside.path().join("evil.toml"),
            repo.path().join(".harw/providers/openai.toml"),
        )?;
        assert!(matches!(
            trust_project(home.path(), repo.path()),
            Err(HomeError::UntrustableProject { .. })
        ));
        assert!(TrustStore::load(home.path())?.records().is_empty());

        // Symlink als Einzeldatei (`auth.toml`).
        let repo = project("symlink-auth")?;
        symlink(
            outside.path().join("evil.toml"),
            repo.path().join(".harw/auth.toml"),
        )?;
        assert!(matches!(
            project_digest(repo.path()),
            Err(HomeError::UntrustableProject { .. })
        ));

        // Verzeichnis-Symlink, nach der Freigabe eingeschleust → Changed.
        let repo = project("symlink-dir")?;
        trust_project(home.path(), repo.path())?;
        symlink(outside.path(), repo.path().join(".harw/mcps"))?;
        assert_eq!(
            project_trust_status(home.path(), repo.path())?,
            TrustStatus::Changed
        );

        // `.harw` selbst als Symlink.
        let repo = TempDir::new("symlink-harw")?;
        symlink(outside.path(), repo.path().join(".harw"))?;
        assert!(matches!(
            project_digest(repo.path()),
            Err(HomeError::UntrustableProject { .. })
        ));
        Ok(())
    }

    #[test]
    fn store_with_group_permissions_is_rejected() -> TestResult {
        let home = TempDir::new("home")?;
        let path = trusted_projects_path(home.path());
        std::fs::write(&path, "version = 1\n")?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;
        assert!(matches!(
            TrustStore::load(home.path()),
            Err(HomeError::Io { .. })
        ));
        Ok(())
    }

    #[test]
    fn store_rejects_malformed_records() -> TestResult {
        let home = TempDir::new("home")?;
        let path = trusted_projects_path(home.path());
        let valid = format!("blake3:{}", "a".repeat(64));
        let record = |root: &str, digest: &str| {
            format!("[[project]]\ncanonical_root = {root:?}\nowner_uid = 1\ndigest = {digest:?}\n")
        };
        let cases = [
            "version = 2\n".to_owned(),
            format!("version = 1\n{}", record("relative", &valid)),
            format!("version = 1\n{}", record("/abs", "sha256:00")),
            format!(
                "version = 1\n{}{}",
                record("/abs", &valid),
                record("/abs", &valid)
            ),
            format!("version = 1\nunknown = true\n{}", record("/abs", &valid)),
        ];
        for contents in cases {
            write_atomic(&path, contents.as_bytes(), AtomicWriteOptions::private())?;
            assert!(
                matches!(
                    TrustStore::load(home.path()),
                    Err(HomeError::TrustStore { .. })
                ),
                "must reject: {contents}"
            );
        }
        Ok(())
    }

    #[test]
    fn missing_store_is_empty() -> TestResult {
        let home = TempDir::new("home")?;
        let store = TrustStore::load(&home.path().join("not-yet-created"))?;
        assert!(store.records().is_empty());
        Ok(())
    }
}
