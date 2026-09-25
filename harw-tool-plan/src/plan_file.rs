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
//! - Das Plan-Verzeichnis und sein Elternteil (`.harw`) dürfen keine
//!   symbolischen Verknüpfungen sein; eine vorhandene Plan-Datei darf kein
//!   Symlink sein.
//! - Schreiben über eine frische temporäre Datei im selben Verzeichnis plus
//!   `rename` (atomar, folgt keinem Symlink am Ziel).
//! - Inhalt höchstens [`PLAN_MAX_BYTES`].
//!
//! # Nebenläufigkeit
//! [`PlanDir`] ist ein unveränderlicher Pfadwert (`Clone`, `Send + Sync`).
//! Gleichzeitige Schreiber auf denselben Slug überschreiben sich gegenseitig
//! atomar (der letzte `rename` gewinnt); es entsteht nie eine halbe Datei.

use std::fmt;
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Höchstgröße einer Plan-Datei in Bytes.
pub const PLAN_MAX_BYTES: usize = 256 * 1024;

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
        /// Tatsächliche Größe in Bytes.
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
                "{} ist ein symbolischer Link — Plan-Dateien werden dort nicht geschrieben",
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
    /// (keine Symlinks) sind; legt das Plan-Verzeichnis bei Bedarf an.
    fn ensure_dir(&self) -> PlanFileResult<()> {
        if let Some(parent) = self.root.parent() {
            reject_symlink(parent)?;
        }
        reject_symlink(&self.root)?;
        std::fs::create_dir_all(&self.root).map_err(|error| io_error(&self.root, &error))?;
        reject_symlink(&self.root)
    }

    /// Schreibt (oder ersetzt) die Plan-Datei zu `slug`.
    ///
    /// # Errors
    /// [`PlanFileError::InvalidSlug`], [`PlanFileError::TooLarge`],
    /// [`PlanFileError::Symlink`] oder [`PlanFileError::Io`].
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
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let temp = self.root.join(format!(
            ".{}.{}.{nonce}.tmp",
            validate_slug(slug)?,
            std::process::id()
        ));
        let written = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .and_then(|mut file| {
                file.write_all(content.as_bytes())?;
                file.sync_all()
            });
        if let Err(error) = written {
            let _ = std::fs::remove_file(&temp);
            return Err(io_error(&temp, &error));
        }
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
    /// [`PlanFileError::TooLarge`] oder [`PlanFileError::Io`].
    pub fn read(&self, slug: &str) -> PlanFileResult<String> {
        let path = self.path_for(slug)?;
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(PlanFileError::Symlink(path));
            }
            Ok(meta) if meta.len() > PLAN_MAX_BYTES as u64 => {
                return Err(PlanFileError::TooLarge {
                    bytes: usize::try_from(meta.len()).unwrap_or(usize::MAX),
                });
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(PlanFileError::NotFound(slug.to_owned()));
            }
            Err(error) => return Err(io_error(&path, &error)),
        }
        std::fs::read_to_string(&path).map_err(|error| io_error(&path, &error))
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
}
