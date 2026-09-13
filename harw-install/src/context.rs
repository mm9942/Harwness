//! Erkennung des Installationskontexts des laufenden `harw`-Binaries.
//!
//! Spezifikationsquelle: `docs/design/CONTRACT-setup-install.md`,
//! Abschnitt „Crate `harw-install` / `src/context.rs`".
//!
//! # Verantwortung
//! Dieses Modul besitzt ausschliesslich die Klassifikation, *wie* das aktuelle
//! ausführbare Programm installiert wurde (Cargo, Homebrew, Standalone-Release
//! oder unbekannt). Es delegiert die Fehlermodellierung an [`crate::error`]
//! ([`InstallError`]).
//!
//! # Exportierte Typen
//! - [`InstallMethod`] — klassifiziertes Installationsverfahren.
//! - [`InstallContext`] — Methode plus Pfad zum laufenden Binary.
//!
//! # Nebenläufigkeit
//! Alle Typen sind `Send + Sync` und tragen keinen geteilten Zustand.
//! [`InstallContext::detect`] liest lediglich [`std::env::current_exe`] und
//! führt reine String-Analyse durch; es werden keine Locks gehalten und keine
//! Threads gestartet.
//!
//! # Fehler
//! [`InstallContext::detect`] liefert [`InstallError`], wenn der Pfad des
//! laufenden Programms nicht bestimmt werden kann.
//!
//! # Examples
//! ```rust,no_run
//! use harw_install::context::InstallContext;
//!
//! let ctx = InstallContext::detect()?;
//! println!("Installations-Binary: {}", ctx.exe.display());
//! # Ok::<(), harw_install::error::InstallError>(())
//! ```

use std::path::{Path, PathBuf};

use crate::error::InstallError;

/// Klassifiziertes Verfahren, mit dem das laufende Binary installiert wurde.
///
/// # Description
/// Wird von [`classify_exe`] aus dem Pfad des ausführbaren Programms abgeleitet.
/// Als `#[non_exhaustive]` markiert, damit zukünftige Verfahren additiv ergänzt
/// werden können, ohne nachgelagerte `match`-Ausdrücke zu brechen.
///
/// # Concurrency
/// Reiner Wert-Typ ohne inneren Zustand; frei zwischen Threads bewegbar.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallMethod {
    /// Installiert via `cargo install` (Pfad enthält `/.cargo/bin/`).
    Cargo,
    /// Installiert via Homebrew (`/opt/homebrew` oder `/usr/local/Cellar`).
    Homebrew,
    /// Entpacktes Standalone-Release; `release_dir` ist das Elternverzeichnis
    /// des Binaries innerhalb von `.../packages/standalone/`.
    Standalone {
        /// Verzeichnis, in dem das Standalone-Release liegt (Elternverzeichnis
        /// der ausführbaren Datei).
        release_dir: PathBuf,
    },
    /// Kein bekannter Installationsmarker erkannt.
    Unknown,
}

/// Vollständiger Installationskontext des laufenden Binaries.
///
/// # Description
/// Kombiniert das erkannte [`InstallMethod`] mit dem absoluten Pfad zum
/// laufenden ausführbaren Programm. Wird typischerweise über
/// [`InstallContext::detect`] erzeugt.
///
/// # Concurrency
/// Reiner Daten-Container; `Send + Sync`.
#[derive(Debug, Clone)]
pub struct InstallContext {
    /// Erkanntes Installationsverfahren.
    pub method: InstallMethod,
    /// Absoluter Pfad zum laufenden Binary (aus [`std::env::current_exe`]).
    pub exe: PathBuf,
}

impl InstallContext {
    /// Erkennt den Installationskontext des laufenden Binaries.
    ///
    /// # Description
    /// Ermittelt den Pfad des laufenden Programms über
    /// [`std::env::current_exe`] und klassifiziert ihn über [`classify_exe`].
    ///
    /// # Returns
    /// [`InstallContext`] mit dem erkannten [`InstallMethod`] und dem Pfad des
    /// laufenden Binaries.
    ///
    /// # Errors
    /// - [`InstallError`]: wenn [`std::env::current_exe`] fehlschlägt (der
    ///   zugrunde liegende `std::io::Error` wird via `From` konvertiert).
    ///
    /// # Concurrency
    /// Führt nur Lesezugriffe auf Prozess-Metadaten und reine String-Analyse
    /// durch; sicher aus mehreren Threads aufrufbar.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_install::context::{InstallContext, InstallMethod};
    ///
    /// let ctx = InstallContext::detect()?;
    /// if matches!(ctx.method, InstallMethod::Cargo) {
    ///     println!("via cargo installiert");
    /// }
    /// # Ok::<(), harw_install::error::InstallError>(())
    /// ```
    pub fn detect() -> Result<Self, InstallError> {
        let exe = std::env::current_exe().map_err(InstallError::from)?;
        let method = classify_exe(&exe);
        Ok(Self { method, exe })
    }
}

/// Klassifiziert einen ausführbaren Pfad in ein [`InstallMethod`].
///
/// # Description
/// Reine Funktion ohne I/O. Prüft die Pfadbestandteile in fester Reihenfolge:
/// 1. enthält `/.cargo/bin/` → [`InstallMethod::Cargo`]
/// 2. liegt unter `/opt/homebrew` oder `/usr/local/Cellar` → [`InstallMethod::Homebrew`]
/// 3. liegt unter `.../packages/standalone/` → [`InstallMethod::Standalone`]
///    mit `release_dir` = Elternverzeichnis des Binaries
/// 4. sonst → [`InstallMethod::Unknown`]
///
/// # Arguments
/// - `exe` (`&Path`): Pfad zum laufenden Binary; nur gelesen.
///
/// # Returns
/// Das aus dem Pfad abgeleitete [`InstallMethod`].
///
/// # Concurrency
/// Reine Funktion; kein geteilter Zustand, aus jedem Thread aufrufbar.
///
/// # Examples
/// ```rust
/// use std::path::Path;
/// use harw_install::context::{classify_exe, InstallMethod};
///
/// let m = classify_exe(Path::new("/home/u/.cargo/bin/harw"));
/// assert_eq!(m, InstallMethod::Cargo);
/// ```
pub fn classify_exe(exe: &Path) -> InstallMethod {
    // Vergleich über die String-Darstellung mit Vorwärts-Slashes, damit die
    // Marker-Substrings plattformunabhängig erkannt werden.
    let raw = exe.to_string_lossy();
    let normalized = raw.replace('\\', "/");

    if normalized.contains("/.cargo/bin/") {
        return InstallMethod::Cargo;
    }

    if normalized.contains("/opt/homebrew") || normalized.contains("/usr/local/Cellar") {
        return InstallMethod::Homebrew;
    }

    if normalized.contains("/packages/standalone/") {
        // release_dir ist das Elternverzeichnis der ausführbaren Datei.
        let release_dir = exe
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| exe.to_path_buf());
        return InstallMethod::Standalone { release_dir };
    }

    InstallMethod::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_exe_cargo() {
        let m = classify_exe(Path::new("/home/user/.cargo/bin/harw"));
        assert_eq!(m, InstallMethod::Cargo);
    }

    #[test]
    fn test_classify_exe_homebrew_arm() {
        let m = classify_exe(Path::new("/opt/homebrew/bin/harw"));
        assert_eq!(m, InstallMethod::Homebrew);
    }

    #[test]
    fn test_classify_exe_homebrew_intel_cellar() {
        let m = classify_exe(Path::new("/usr/local/Cellar/harw/1.2.3/bin/harw"));
        assert_eq!(m, InstallMethod::Homebrew);
    }

    #[test]
    fn test_classify_exe_standalone_uses_parent_as_release_dir() {
        let m = classify_exe(Path::new("/home/u/packages/standalone/harw-0.4/bin/harw"));
        assert_eq!(
            m,
            InstallMethod::Standalone {
                release_dir: PathBuf::from("/home/u/packages/standalone/harw-0.4/bin"),
            }
        );
    }

    #[test]
    fn test_classify_exe_unknown() {
        let m = classify_exe(Path::new("/usr/bin/harw"));
        assert_eq!(m, InstallMethod::Unknown);
    }

    #[test]
    fn test_classify_exe_cargo_precedence_over_others() {
        // Cargo-Marker gewinnt, auch wenn andere Substrings auftauchen.
        let m = classify_exe(Path::new("/opt/homebrew/.cargo/bin/harw"));
        assert_eq!(m, InstallMethod::Cargo);
    }

    #[test]
    fn test_classify_exe_windows_style_path_normalized() {
        let m = classify_exe(Path::new("C:\\Users\\u\\.cargo\\bin\\harw.exe"));
        assert_eq!(m, InstallMethod::Cargo);
    }
}
