//! Plan-Dateien unter `.harw/plans/<slug>.md` (Runde 5, Teil F, Punkt 1) —
//! `.<name>/plans/<slug>.md` in einer personalisierten harw (#22, siehe
//! [`plan_display_prefix`]).
//!
//! # Verantwortung
//! Einzige Stelle, an der Plan-Markdown gelesen, geschrieben und aufgelistet
//! wird. Das Modell nennt nie einen Pfad zum Schreiben, sondern höchstens einen
//! **Slug** (`[a-z0-9-]`, 1–64 Zeichen); der Pfad entsteht ausschließlich hier
//! aus dem von der Montage gesetzten Plan-Verzeichnis. Damit kann `plan.write`
//! nur unter `.harw/plans/` schreiben — sonst nirgends.
//!
//! # Härtung
//! - Slug-Grammatik ohne `/`, `\`, `.` und Steuerzeichen: kein Pfadausbruch.
//! - Beim Lesen wie beim Schreiben dürfen das Plan-Verzeichnis und sein
//!   Elternteil (`.harw`) keine symbolischen Verknüpfungen sein; eine
//!   vorhandene Plan-Datei darf kein Symlink sein.
//! - Lesen und Schreiben laufen über einen Deskriptor des Plan-Verzeichnisses:
//!   `.harw` per `harw_fsutil::open_dir_nofollow`, `plans` und die Datei per
//!   `harw_fsutil::open_beneath`. Ein nach einer Prüfung untergeschobener
//!   Symlink wird nie verfolgt; FIFOs und Geräte werden abgelehnt.
//! - Lesen nimmt höchstens [`PLAN_MAX_BYTES`] + 1 Bytes, auch wenn die Datei
//!   währenddessen wächst.
//! - Schreiben über eine frische temporäre Datei, angelegt nur im geöffneten
//!   Verzeichnis, plus `rename` (atomar, folgt keinem Symlink am Ziel). Vor
//!   dem `rename` muss der Pfad noch dasselbe Verzeichnis nennen (dev/ino).
//! - Inhalt höchstens [`PLAN_MAX_BYTES`].
//! - Restrisiko: der `rename` selbst ist pfadbasiert; ein Tausch des
//!   Verzeichnisses genau zwischen dev/ino-Prüfung und `rename` bleibt ein
//!   schmales Wettlauffenster.
//!
//! # Nebenläufigkeit
//! [`PlanDir`] ist ein unveränderlicher Pfadwert (`Clone`, `Send + Sync`).
//! Gleichzeitige Schreiber auf denselben Slug überschreiben sich gegenseitig
//! atomar (der letzte `rename` gewinnt); es entsteht nie eine halbe Datei.

use std::fmt;
use std::fs::File;
use std::io::Read as _;
use std::io::Write as _;
use std::os::fd::AsFd as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use harw_fsutil::OpenMode;

/// Höchstgröße einer Plan-Datei in Bytes.
pub const PLAN_MAX_BYTES: usize = 256 * 1024;

/// Rechte neuer Plan-Dateien; der `umask` wird noch abgezogen. Plan-Dateien
/// sind Projektdokumente, keine Geheimnisse.
const PLAN_FILE_MODE: u32 = 0o644;

/// Höchstlänge eines Slugs in Zeichen.
pub const SLUG_MAX_CHARS: usize = 64;

/// Dateiendung einer Plan-Datei.
pub const PLAN_FILE_EXTENSION: &str = "md";

/// Anzeigepräfix relativ zur Projektwurzel (`.harw/plans`, in einer
/// personalisierten harw (#22) `.<name>/plans`).
///
/// # Description
/// Nutzt [`harw_home::project_dir_name`] statt eines fest verdrahteten
/// `.harw` — der Anzeigepfad muss demselben Namen folgen wie das tatsächlich
/// gemountete Plan-Verzeichnis (`harw_home::project::ProjectHome::plans_dir`),
/// sonst weist [`PlanDir::resolve`] einen vom Modell zurückgegebenen, gültigen
/// Anzeigepfad fälschlich zurück.
#[must_use]
pub fn plan_display_prefix() -> String {
    format!("{}/plans", harw_home::project_dir_name())
}

/// Fehler rund um Plan-Dateien. Trägt nie Planinhalt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanFileError {
    /// Der Slug verletzt die Grammatik `[a-z0-9-]{1,64}` (ohne `-` am Rand).
    InvalidSlug(String),
    /// Der Verweis zeigt nicht auf eine Datei direkt in `.harw/plans/`.
    OutsidePlansDir(String),
    /// Der Inhalt ist größer als [`PLAN_MAX_BYTES`].
    TooLarge {
        /// Tatsächliche Größe in Bytes; beim Lesen einer wachsenden Datei
        /// eine Untergrenze.
        bytes: usize,
    },
    /// Es gibt keine Plan-Datei zu diesem Slug.
    NotFound(String),
    /// Plan-Verzeichnis, `.harw` oder die Plan-Datei ist ein Symlink.
    Symlink(PathBuf),
    /// Ein Dateisystemfehler.
    Io {
        /// Betroffener Pfad.
        path: PathBuf,
        /// Fehlertext des Betriebssystems.
        detail: String,
    },
}

impl fmt::Display for PlanFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSlug(slug) => write!(
                f,
                "ungültiger Plan-Name „{slug}“: erlaubt sind a–z, 0–9 und '-' \
                 (1–{SLUG_MAX_CHARS} Zeichen, nicht mit '-' am Rand)"
            ),
            Self::OutsidePlansDir(reference) => write!(
                f,
                "„{reference}“ liegt nicht direkt in {}/ — Pläne gibt es nur dort",
                plan_display_prefix()
            ),
            Self::TooLarge { bytes } => write!(
                f,
                "Plan ist zu groß ({bytes} Bytes, höchstens {PLAN_MAX_BYTES})"
            ),
            Self::NotFound(slug) => {
                write!(f, "kein Plan „{slug}“ unter {}/", plan_display_prefix())
            }
            Self::Symlink(path) => write!(
                f,
                "{} ist ein symbolischer Link — Plan-Dateien werden dort weder gelesen noch geschrieben",
                path.display()
            ),
            Self::Io { path, detail } => write!(f, "{}: {detail}", path.display()),
        }
    }
}

impl std::error::Error for PlanFileError {}

/// Kurzform für Ergebnisse dieses Moduls.
pub type PlanFileResult<T> = Result<T, PlanFileError>;

/// Prüft einen Slug gegen `[a-z0-9-]{1,64}` ohne `-` am Anfang oder Ende.
///
/// # Arguments
/// - `raw` (`&str`): der zu prüfende Name; umgebender Leerraum wird entfernt.
///
/// # Returns
/// Den normalisierten Slug.
///
/// # Errors
/// [`PlanFileError::InvalidSlug`] bei jeder Abweichung — auch bei
/// Großbuchstaben (keine stille Umdeutung).
pub fn validate_slug(raw: &str) -> PlanFileResult<String> {
    let slug = raw.trim();
    let valid_chars = slug
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if slug.is_empty()
        || slug.chars().count() > SLUG_MAX_CHARS
        || !valid_chars
        || slug.starts_with('-')
        || slug.ends_with('-')
    {
        return Err(PlanFileError::InvalidSlug(raw.to_owned()));
    }
    Ok(slug.to_owned())
}

/// Leitet aus einem freien Titel einen gültigen Slug ab.
///
/// # Beschreibung
/// Kleinschreibung, Umlaute als `ae`/`oe`/`ue`/`ss`, jede andere Nicht-ASCII-
/// Alphanumerik wird zu `-`, mehrfache `-` werden zusammengefasst, das
/// Ergebnis auf [`SLUG_MAX_CHARS`] gekürzt.
///
/// # Returns
/// `None`, wenn nach der Umwandlung nichts übrig bleibt.
#[must_use]
pub fn slugify(title: &str) -> Option<String> {
    let mut out = String::new();
    for c in title.chars().flat_map(char::to_lowercase) {
        let mapped: &str = match c {
            'ä' => "ae",
            'ö' => "oe",
            'ü' => "ue",
            'ß' => "ss",
            c if c.is_ascii_alphanumeric() => {
                out.push(c);
                continue;
            }
            _ => "-",
        };
        if mapped == "-" {
            if !out.is_empty() && !out.ends_with('-') {
                out.push('-');
            }
        } else {
            out.push_str(mapped);
        }
    }
    let truncated: String = out.chars().take(SLUG_MAX_CHARS).collect();
    let trimmed = truncated.trim_matches('-');
    validate_slug(trimmed).ok()
}

/// Ein Slug aus der Uhrzeit, wenn weder Slug noch Titel vorliegen.
#[must_use]
pub fn fallback_slug() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default();
    format!("plan-{secs}")
}

/// Anzeigepfad eines Plans relativ zur Projektwurzel.
#[must_use]
pub fn display_path(slug: &str) -> String {
    format!("{}/{slug}.{PLAN_FILE_EXTENSION}", plan_display_prefix())
}

/// Ergebnis eines Schreibvorgangs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanWriteOutcome {
    /// Der Slug.
    pub slug: String,
    /// Absoluter Pfad der Datei.
    pub path: PathBuf,
    /// Geschriebene Bytes.
    pub bytes: usize,
    /// `true`, wenn eine vorhandene Datei ersetzt wurde.
    pub overwritten: bool,
}

/// Ein Eintrag von [`PlanDir::list`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanEntry {
    /// Der Slug (Dateiname ohne Endung).
    pub slug: String,
    /// Größe in Bytes.
    pub bytes: u64,
    /// Letzte Änderung (Sekunden seit der Unix-Epoche), falls bekannt.
    pub modified_secs: Option<u64>,
}

/// Das Plan-Verzeichnis eines Projekts (`<projekt>/.harw/plans`).
///
/// # Beschreibung
/// Wird ausschließlich von der Montage (`harw-runtime`) aus
/// `harw_home::project::ProjectHome::plans_dir` gebaut — nie aus
/// Modellargumenten.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanDir {
    root: PathBuf,
}

impl PlanDir {
    /// Baut den Wert über dem Plan-Verzeichnis.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Das Plan-Verzeichnis.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Absoluter Pfad der Plan-Datei zu `slug`.
    ///
    /// # Errors
    /// [`PlanFileError::InvalidSlug`], wenn `slug` die Grammatik verletzt.
    pub fn path_for(&self, slug: &str) -> PlanFileResult<PathBuf> {
        let slug = validate_slug(slug)?;
        Ok(self.root.join(format!("{slug}.{PLAN_FILE_EXTENSION}")))
    }

    /// Löst einen Planverweis des Modells oder der Nutzerin auf einen Slug auf.
    ///
    /// # Beschreibung
    /// Zulässig sind genau: ein Slug (`mein-plan`), ein Dateiname
    /// (`mein-plan.md`), der Anzeigepfad (`.harw/plans/mein-plan.md`) oder
    /// ein absoluter Pfad, dessen Elternverzeichnis dieses Plan-Verzeichnis
    /// ist. `..`, weitere Verzeichnisebenen und andere Endungen werden
    /// abgelehnt.
    ///
    /// # Errors
    /// [`PlanFileError::OutsidePlansDir`] oder [`PlanFileError::InvalidSlug`].
    pub fn resolve(&self, reference: &str) -> PlanFileResult<String> {
        let trimmed = reference.trim();
        let outside = || PlanFileError::OutsidePlansDir(reference.to_owned());
        if trimmed.is_empty() {
            return Err(outside());
        }
        let path = Path::new(trimmed);
        if path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
        {
            return Err(outside());
        }
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(outside)?;
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty());
        match parent {
            None => {}
            Some(parent) if path.is_absolute() => {
                if parent != self.root {
                    return Err(outside());
                }
            }
            Some(parent) if parent != Path::new(&plan_display_prefix()) => {
                return Err(outside());
            }
            Some(_) => {}
        }
        let stem = match file_name.strip_suffix(&format!(".{PLAN_FILE_EXTENSION}")) {
            Some(stem) => stem,
            None if parent.is_none() && !file_name.contains('.') => file_name,
            None => return Err(outside()),
        };
        validate_slug(stem)
    }

    /// Stellt sicher, dass Plan-Verzeichnis und `.harw` echte Verzeichnisse
    /// (keine Symlinks) sind; legt das Plan-Verzeichnis bei Bedarf an. Diese
    /// Pfadprüfungen sind nur Diagnose; verbindlich ist das
    /// deskriptorbasierte Öffnen in [`Self::open_dir`].
    fn ensure_dir(&self) -> PlanFileResult<()> {
        if let Some(parent) = self.root.parent() {
            reject_symlink(parent)?;
        }
        reject_symlink(&self.root)?;
        std::fs::create_dir_all(&self.root).map_err(|error| io_error(&self.root, &error))?;
        reject_symlink(&self.root)
    }

    /// Öffnet das Plan-Verzeichnis als Deskriptor, ohne einem Symlink in
    /// `.harw` oder `plans` zu folgen.
    ///
    /// # Returns
    /// `None`, wenn `.harw` oder `plans` nicht existiert.
    ///
    /// # Errors
    /// [`PlanFileError::Symlink`], wenn `.harw` oder `plans` ein Symlink ist;
    /// [`PlanFileError::Io`] bei jedem anderen Fehler oder wenn `plans` kein
    /// Verzeichnis ist.
    fn open_dir(&self) -> PlanFileResult<Option<File>> {
        let Some(name) = self.root.file_name() else {
            return Err(io_error(
                &self.root,
                &std::io::Error::from(std::io::ErrorKind::InvalidInput),
            ));
        };
        let parent = self
            .root
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let parent_fd = match harw_fsutil::open_dir_nofollow(parent) {
            Ok(fd) => fd,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                // Unter Linux meldet `O_DIRECTORY|O_NOFOLLOW` einen Symlink als
                // `ENOTDIR` statt `ELOOP`. Das Öffnen ist bereits gescheitert;
                // die Pfadprüfung liefert nur die genauere Diagnose.
                reject_symlink(parent)?;
                return Err(open_error(parent, &error));
            }
        };
        let dir = match harw_fsutil::open_beneath(
            parent_fd.as_fd(),
            Path::new(name),
            OpenMode::read_only(),
        ) {
            Ok(dir) => dir,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(open_error(&self.root, &error)),
        };
        // fstat auf dem Deskriptor: `open_beneath` lässt auch reguläre Dateien zu.
        let meta = dir.metadata().map_err(|error| io_error(&self.root, &error))?;
        if !meta.is_dir() {
            return Err(io_error(
                &self.root,
                &std::io::Error::from(std::io::ErrorKind::NotADirectory),
            ));
        }
        Ok(Some(dir))
    }

    /// Prüft, ob `self.root` noch das gepinnte Verzeichnis `dir` nennt
    /// (gleiches `dev`/`ino`).
    ///
    /// # Errors
    /// [`PlanFileError::Symlink`], wenn der Pfad inzwischen ein Symlink ist;
    /// [`PlanFileError::Io`], wenn er fehlt oder etwas anderes nennt.
    fn verify_pinned(&self, dir: &File) -> PlanFileResult<()> {
        let pinned = dir.metadata().map_err(|error| io_error(&self.root, &error))?;
        match std::fs::symlink_metadata(&self.root) {
            Ok(meta) if meta.file_type().is_symlink() => {
                Err(PlanFileError::Symlink(self.root.clone()))
            }
            Ok(meta)
                if meta.is_dir() && meta.dev() == pinned.dev() && meta.ino() == pinned.ino() =>
            {
                Ok(())
            }
            Ok(_) => Err(PlanFileError::Io {
                path: self.root.clone(),
                detail: "Plan-Verzeichnis wurde während des Schreibens ersetzt".to_owned(),
            }),
            Err(error) => Err(io_error(&self.root, &error)),
        }
    }

    /// Schreibt (oder ersetzt) die Plan-Datei zu `slug`.
    ///
    /// # Errors
    /// [`PlanFileError::InvalidSlug`], [`PlanFileError::TooLarge`],
    /// [`PlanFileError::Symlink`] oder [`PlanFileError::Io`] — Letzteres auch,
    /// wenn das Plan-Verzeichnis während des Schreibens ersetzt wurde.
    pub fn write(&self, slug: &str, content: &str) -> PlanFileResult<PlanWriteOutcome> {
        let path = self.path_for(slug)?;
        if content.len() > PLAN_MAX_BYTES {
            return Err(PlanFileError::TooLarge {
                bytes: content.len(),
            });
        }
        self.ensure_dir()?;
        let overwritten = match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(PlanFileError::Symlink(path));
            }
            Ok(_) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(io_error(&path, &error)),
        };
        // Ab hier gilt der Deskriptor: die temporäre Datei entsteht nur im
        // tatsächlich geöffneten Verzeichnis.
        let Some(dir) = self.open_dir()? else {
            return Err(io_error(
                &self.root,
                &std::io::Error::from(std::io::ErrorKind::NotFound),
            ));
        };
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let temp_name = format!(
            ".{}.{}.{nonce}.tmp",
            validate_slug(slug)?,
            std::process::id()
        );
        let temp = self.root.join(&temp_name);
        let written = harw_fsutil::open_beneath(
            dir.as_fd(),
            Path::new(&temp_name),
            OpenMode::write_create_new(PLAN_FILE_MODE),
        )
        .and_then(|mut file| {
            file.write_all(content.as_bytes())?;
            file.sync_all()
        });
        if let Err(error) = written {
            let _ = std::fs::remove_file(&temp);
            return Err(io_error(&temp, &error));
        }
        // Vor dem `rename` muss der Pfad noch das gepinnte Verzeichnis nennen.
        // Scheitert die Prüfung, bleibt die temporäre Datei bewusst im
        // verschobenen Verzeichnis liegen (kein Löschen über einen Pfad, der
        // jetzt woanders hinzeigt); `list` übergeht sie wegen des führenden `.`
        // und der Slug-Grammatik.
        self.verify_pinned(&dir)?;
        if let Err(error) = std::fs::rename(&temp, &path) {
            let _ = std::fs::remove_file(&temp);
            return Err(io_error(&path, &error));
        }
        Ok(PlanWriteOutcome {
            slug: validate_slug(slug)?,
            path,
            bytes: content.len(),
            overwritten,
        })
    }

    /// Liest die Plan-Datei zu `slug`.
    ///
    /// # Errors
    /// [`PlanFileError::NotFound`], [`PlanFileError::Symlink`],
    /// [`PlanFileError::TooLarge`] oder [`PlanFileError::Io`]. Ist `.harw`,
    /// `plans` oder die Plan-Datei ein Symlink, folgt
    /// [`PlanFileError::Symlink`]; ein FIFO, Gerät, Verzeichnis oder
    /// ungültiges UTF-8 ergibt [`PlanFileError::Io`].
    pub fn read(&self, slug: &str) -> PlanFileResult<String> {
        let path = self.path_for(slug)?;
        let Some(dir) = self.open_dir()? else {
            return Err(PlanFileError::NotFound(slug.to_owned()));
        };
        read_plan_in(&dir, &path, slug)
    }

    /// Listet alle Pläne, neueste zuerst.
    ///
    /// # Beschreibung
    /// Berücksichtigt nur reguläre `*.md`-Dateien mit gültigem Slug direkt im
    /// Plan-Verzeichnis (keine Unterverzeichnisse wie `default/`, keine
    /// Symlinks, keine temporären Dateien). Ein fehlendes Verzeichnis ergibt
    /// eine leere Liste.
    ///
    /// # Errors
    /// [`PlanFileError::Io`], wenn das Verzeichnis nicht lesbar ist.
    pub fn list(&self) -> PlanFileResult<Vec<PlanEntry>> {
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(io_error(&self.root, &error)),
        };
        let mut plans = Vec::new();
        for entry in entries.flatten() {
            let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if !meta.file_type().is_file() {
                continue;
            }
            let name = entry.file_name();
            let Some(stem) = name
                .to_str()
                .and_then(|name| name.strip_suffix(&format!(".{PLAN_FILE_EXTENSION}")))
            else {
                continue;
            };
            let Ok(slug) = validate_slug(stem) else {
                continue;
            };
            let modified_secs = meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|elapsed| elapsed.as_secs());
            plans.push(PlanEntry {
                slug,
                bytes: meta.len(),
                modified_secs,
            });
        }
        plans.sort_by(|a, b| {
            b.modified_secs
                .cmp(&a.modified_secs)
                .then_with(|| a.slug.cmp(&b.slug))
        });
        Ok(plans)
    }
}

/// Lehnt einen vorhandenen Symlink ab; ein fehlender Pfad ist in Ordnung.
fn reject_symlink(path: &Path) -> PlanFileResult<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(PlanFileError::Symlink(path.to_owned())),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(path, &error)),
    }
}

fn io_error(path: &Path, error: &std::io::Error) -> PlanFileError {
    PlanFileError::Io {
        path: path.to_owned(),
        detail: error.to_string(),
    }
}

/// Fehler der `harw_fsutil`-Öffner: `ELOOP` (Symlink in einem Pfadglied)
/// wird zu [`PlanFileError::Symlink`], alles andere zu [`PlanFileError::Io`].
fn open_error(path: &Path, error: &std::io::Error) -> PlanFileError {
    if harw_fsutil::is_symlink_loop(error) {
        PlanFileError::Symlink(path.to_owned())
    } else {
        io_error(path, error)
    }
}

/// Liest höchstens [`PLAN_MAX_BYTES`] + 1 Bytes aus `reader` — auch eine
/// Datei, die während des Lesens wächst, sprengt die Grenze nicht.
///
/// # Errors
/// [`PlanFileError::TooLarge`] ab [`PLAN_MAX_BYTES`] + 1 Bytes,
/// [`PlanFileError::Io`] bei Lesefehlern und ungültigem UTF-8.
fn read_capped(reader: impl std::io::Read, path: &Path) -> PlanFileResult<String> {
    let mut bytes = Vec::new();
    reader
        .take(PLAN_MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| io_error(path, &error))?;
    if bytes.len() > PLAN_MAX_BYTES {
        return Err(PlanFileError::TooLarge { bytes: bytes.len() });
    }
    String::from_utf8(bytes).map_err(|error| {
        io_error(
            path,
            &std::io::Error::new(std::io::ErrorKind::InvalidData, error),
        )
    })
}

/// Liest die Plan-Datei `path` über den Deskriptor des Plan-Verzeichnisses.
///
/// # Beschreibung
/// Geöffnet wird nur der Dateiname unterhalb von `dir` (kein Symlink, kein
/// FIFO, kein Gerät); Größe und Typ prüft `fstat` auf dem geöffneten
/// Deskriptor, gelesen wird über [`read_capped`].
///
/// # Errors
/// [`PlanFileError::NotFound`], [`PlanFileError::Symlink`],
/// [`PlanFileError::TooLarge`] oder [`PlanFileError::Io`].
fn read_plan_in(dir: &File, path: &Path, slug: &str) -> PlanFileResult<String> {
    let Some(name) = path.file_name() else {
        return Err(PlanFileError::InvalidSlug(slug.to_owned()));
    };
    let file = match harw_fsutil::open_beneath(dir.as_fd(), Path::new(name), OpenMode::read_only())
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(PlanFileError::NotFound(slug.to_owned()));
        }
        Err(error) => return Err(open_error(path, &error)),
    };
    let meta = file.metadata().map_err(|error| io_error(path, &error))?;
    if !meta.is_file() {
        return Err(io_error(
            path,
            &std::io::Error::from(std::io::ErrorKind::IsADirectory),
        ));
    }
    if meta.len() > PLAN_MAX_BYTES as u64 {
        return Err(PlanFileError::TooLarge {
            bytes: usize::try_from(meta.len()).unwrap_or(usize::MAX),
        });
    }
    read_capped(file, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    fn plan_dir() -> TestResult<(tempfile::TempDir, PlanDir)> {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let dir = PlanDir::new(temp.path().join(".harw").join("plans"));
        Ok((temp, dir))
    }

    #[test]
    fn slug_grammar_rejects_paths_and_odd_characters() {
        let too_long = "x".repeat(SLUG_MAX_CHARS + 1);
        let longest = "x".repeat(SLUG_MAX_CHARS);
        for bad in [
            "",
            "-a",
            "a-",
            "A",
            "a/b",
            "../x",
            "a.b",
            "a b",
            "ä",
            "a\u{0}",
            too_long.as_str(),
        ] {
            assert!(validate_slug(bad).is_err(), "{bad:?}");
        }
        for good in ["a", "plan-1", "auth-refactor-v2", longest.as_str()] {
            assert_eq!(validate_slug(good).ok().as_deref(), Some(good));
        }
    }

    #[test]
    fn slugify_maps_umlauts_and_collapses_separators() {
        assert_eq!(
            slugify("Größere Änderung: Auth / Login!").as_deref(),
            Some("groessere-aenderung-auth-login")
        );
        assert_eq!(slugify("  ---  "), None);
        assert!(fallback_slug().starts_with("plan-"));
        assert!(validate_slug(&fallback_slug()).is_ok());
    }

    #[test]
    fn resolve_accepts_only_references_into_the_plans_dir() -> TestResult {
        let (_temp, dir) = plan_dir()?;
        assert_eq!(dir.resolve("mein-plan").ok().as_deref(), Some("mein-plan"));
        assert_eq!(
            dir.resolve("mein-plan.md").ok().as_deref(),
            Some("mein-plan")
        );
        assert_eq!(
            dir.resolve(".harw/plans/mein-plan.md").ok().as_deref(),
            Some("mein-plan")
        );
        let absolute = dir.root().join("mein-plan.md");
        let absolute = absolute.to_str().ok_or(TestError::Missing("utf-8 path"))?;
        assert_eq!(dir.resolve(absolute).ok().as_deref(), Some("mein-plan"));
        for outside in [
            "",
            "../etc/passwd",
            ".harw/plans/../../x.md",
            "src/main.rs",
            ".harw/plans/sub/x.md",
            "/etc/x.md",
            "plan.txt",
            ".harw/plans/x.txt",
        ] {
            assert!(dir.resolve(outside).is_err(), "{outside:?}");
        }
        Ok(())
    }

    #[test]
    fn write_read_overwrite_and_list() -> TestResult {
        let (_temp, dir) = plan_dir()?;
        let first = dir.write("auth", "# Plan\n").map_err(ctx("first write"))?;
        assert!(!first.overwritten);
        assert_eq!(first.path, dir.root().join("auth.md"));
        let second = dir
            .write("auth", "# Plan v2\n")
            .map_err(ctx("second write"))?;
        assert!(second.overwritten);
        assert_eq!(dir.read("auth").map_err(ctx("read"))?, "# Plan v2\n");
        dir.write("zweiter", "x").map_err(ctx("other"))?;
        std::fs::create_dir_all(dir.root().join("default")).map_err(ctx("subdir"))?;
        std::fs::write(dir.root().join("notes.txt"), "x").map_err(ctx("txt"))?;
        let slugs: Vec<String> = dir
            .list()
            .map_err(ctx("list"))?
            .into_iter()
            .map(|entry| entry.slug)
            .collect();
        assert_eq!(slugs.len(), 2);
        assert!(slugs.contains(&"auth".to_owned()));
        assert!(slugs.contains(&"zweiter".to_owned()));
        assert!(matches!(dir.read("fehlt"), Err(PlanFileError::NotFound(_))));
        Ok(())
    }

    #[test]
    fn write_rejects_oversized_content_and_invalid_slug() -> TestResult {
        let (_temp, dir) = plan_dir()?;
        let big = "x".repeat(PLAN_MAX_BYTES + 1);
        assert!(matches!(
            dir.write("gross", &big),
            Err(PlanFileError::TooLarge { .. })
        ));
        assert!(matches!(
            dir.write("../ausbruch", "x"),
            Err(PlanFileError::InvalidSlug(_))
        ));
        assert!(!dir.root().join("gross.md").exists());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn write_refuses_symlinked_plan_file_and_dir() -> TestResult {
        let (temp, dir) = plan_dir()?;
        std::fs::create_dir_all(dir.root()).map_err(ctx("dir"))?;
        let target = temp.path().join("fremd.txt");
        std::fs::write(&target, "unberührt").map_err(ctx("target"))?;
        std::os::unix::fs::symlink(&target, dir.root().join("link.md")).map_err(ctx("symlink"))?;
        assert!(matches!(
            dir.write("link", "boese"),
            Err(PlanFileError::Symlink(_))
        ));
        assert_eq!(
            std::fs::read_to_string(&target).map_err(ctx("read target"))?,
            "unberührt"
        );

        let other = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let harw = other.path().join(".harw");
        std::fs::create_dir_all(&harw).map_err(ctx("harw"))?;
        let elsewhere = other.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).map_err(ctx("elsewhere"))?;
        std::os::unix::fs::symlink(&elsewhere, harw.join("plans")).map_err(ctx("dir link"))?;
        let linked = PlanDir::new(harw.join("plans"));
        assert!(matches!(
            linked.write("x", "y"),
            Err(PlanFileError::Symlink(_))
        ));
        assert!(!elsewhere.join("x.md").exists());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn read_refuses_symlinked_plan_file_and_dirs() -> TestResult {
        // (1) Die Plan-Datei selbst ist ein Symlink.
        let (temp, dir) = plan_dir()?;
        dir.write("geheim", "# Plan\n").map_err(ctx("write"))?;
        let secret = temp.path().join("secret.txt");
        std::fs::write(&secret, "GEHEIM").map_err(ctx("secret"))?;
        let plan = dir.root().join("geheim.md");
        std::fs::remove_file(&plan).map_err(ctx("remove plan"))?;
        std::os::unix::fs::symlink(&secret, &plan).map_err(ctx("file link"))?;
        let result = dir.read("geheim");
        assert!(
            matches!(result, Err(PlanFileError::Symlink(_))),
            "{result:?}"
        );

        // (2) `plans` ist ein Symlink.
        let (plans_temp, plans_dir) = plan_dir()?;
        plans_dir
            .write("geheim", "# Plan\n")
            .map_err(ctx("write"))?;
        std::fs::rename(plans_dir.root(), plans_temp.path().join("alt"))
            .map_err(ctx("move plans"))?;
        let elsewhere = plans_temp.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).map_err(ctx("elsewhere"))?;
        std::fs::write(elsewhere.join("geheim.md"), "GEHEIM").map_err(ctx("secret"))?;
        std::os::unix::fs::symlink(&elsewhere, plans_dir.root()).map_err(ctx("plans link"))?;
        let result = plans_dir.read("geheim");
        assert!(
            matches!(result, Err(PlanFileError::Symlink(_))),
            "{result:?}"
        );

        // (3) `.harw` ist ein Symlink.
        let harw_temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let target = harw_temp.path().join("ziel");
        std::fs::create_dir_all(target.join("plans")).map_err(ctx("target"))?;
        std::fs::write(target.join("plans").join("geheim.md"), "GEHEIM")
            .map_err(ctx("secret"))?;
        let harw = harw_temp.path().join(".harw");
        std::os::unix::fs::symlink(&target, &harw).map_err(ctx("harw link"))?;
        let harw_dir = PlanDir::new(harw.join("plans"));
        let result = harw_dir.read("geheim");
        assert!(
            matches!(result, Err(PlanFileError::Symlink(_))),
            "{result:?}"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn pinned_dir_ignores_a_swap_after_opening() -> TestResult {
        let (temp, dir) = plan_dir()?;
        dir.write("auth", "# echt\n").map_err(ctx("write"))?;
        let pinned = dir
            .open_dir()
            .map_err(ctx("open"))?
            .ok_or(TestError::Missing("plan dir"))?;

        // Nach dem Öffnen wird das Verzeichnis gegen einen Symlink getauscht.
        std::fs::rename(dir.root(), temp.path().join("verschoben"))
            .map_err(ctx("move plans"))?;
        let elsewhere = temp.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).map_err(ctx("elsewhere"))?;
        std::fs::write(elsewhere.join("auth.md"), "GEHEIM").map_err(ctx("secret"))?;
        std::os::unix::fs::symlink(&elsewhere, dir.root()).map_err(ctx("swap link"))?;

        let path = dir.path_for("auth").map_err(ctx("path"))?;
        assert_eq!(
            read_plan_in(&pinned, &path, "auth").map_err(ctx("pinned read"))?,
            "# echt\n"
        );
        let verified = dir.verify_pinned(&pinned);
        assert!(
            matches!(verified, Err(PlanFileError::Symlink(_))),
            "{verified:?}"
        );

        // Ein neues, echtes Verzeichnis am selben Pfad ist nicht das gepinnte.
        std::fs::remove_file(dir.root()).map_err(ctx("remove link"))?;
        std::fs::create_dir(dir.root()).map_err(ctx("new dir"))?;
        let verified = dir.verify_pinned(&pinned);
        assert!(
            matches!(verified, Err(PlanFileError::Io { .. })),
            "{verified:?}"
        );
        Ok(())
    }

    #[test]
    fn read_enforces_size_cap_and_utf8() -> TestResult {
        let (_temp, dir) = plan_dir()?;
        let full = "x".repeat(PLAN_MAX_BYTES);
        dir.write("voll", &full).map_err(ctx("write"))?;
        assert_eq!(dir.read("voll").map_err(ctx("read"))?.len(), PLAN_MAX_BYTES);

        std::fs::write(dir.root().join("gross.md"), vec![b'x'; PLAN_MAX_BYTES + 1])
            .map_err(ctx("gross"))?;
        assert!(matches!(
            dir.read("gross"),
            Err(PlanFileError::TooLarge { bytes }) if bytes == PLAN_MAX_BYTES + 1
        ));

        std::fs::write(dir.root().join("kaputt.md"), [0xff_u8, 0xfe]).map_err(ctx("kaputt"))?;
        let result = dir.read("kaputt");
        assert!(
            matches!(result, Err(PlanFileError::Io { .. })),
            "{result:?}"
        );
        Ok(())
    }

    #[test]
    fn read_capped_stops_on_an_endless_reader() -> TestResult {
        // Deckt eine Datei ab, die während des Lesens wächst.
        let result = read_capped(std::io::repeat(b'x'), Path::new("x.md"));
        assert!(matches!(
            result,
            Err(PlanFileError::TooLarge { bytes }) if bytes == PLAN_MAX_BYTES + 1
        ));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn read_rejects_a_fifo_without_blocking() -> TestResult {
        let (_temp, dir) = plan_dir()?;
        std::fs::create_dir_all(dir.root()).map_err(ctx("dir"))?;
        let fifo = dir.root().join("rohr.md");
        match std::process::Command::new("mkfifo").arg(&fifo).status() {
            Ok(status) if status.success() => {}
            other => {
                eprintln!("übersprungen: `mkfifo` nicht verfügbar ({other:?})");
                return Ok(());
            }
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let reader = dir.clone();
        std::thread::spawn(move || {
            // Nach einem Timeout gibt es keinen Empfänger mehr; dann ist der
            // Test ohnehin schon fehlgeschlagen.
            let _ = tx.send(reader.read("rohr"));
        });
        let result = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .map_err(ctx("read blockiert"))?;
        assert!(
            matches!(result, Err(PlanFileError::Io { .. })),
            "{result:?}"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn write_creates_plan_without_group_or_world_write() -> TestResult {
        use std::os::unix::fs::PermissionsExt as _;

        let (_temp, dir) = plan_dir()?;
        dir.write("rechte", "x").map_err(ctx("write"))?;
        let meta = std::fs::metadata(dir.root().join("rechte.md")).map_err(ctx("meta"))?;
        assert_eq!(meta.permissions().mode() & 0o022, 0);
        Ok(())
    }
}
