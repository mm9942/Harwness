//! Platform- und Umgebungserkennung für den Installer.
//!
//! Spezifikationsquelle: `docs/design/CONTRACT-setup-install.md`, Abschnitt
//! `harw-install / src/platform.rs`.
//!
//! # Verantwortung
//! Dieses Modul besitzt ausschließlich die read-only Erkennung der
//! Laufzeitumgebung: Betriebssystem, Container-, WSL-, Termux- und
//! Managed-Umgebung (z. B. Nix). Es delegiert jegliche darauf aufbauende
//! Installationslogik an andere Module (`context.rs`, `service.rs` …).
//!
//! # Exportierte Typen
//! - [`Os`][]: Betriebssystem-Klassifikation.
//! - [`Platform`][]: aggregiertes Ergebnis der Umgebungssonden.
//!
//! # Fehlerbehandlung
//! Dieses Modul kann nicht fehlschlagen. Alle Sonden sind fehlertolerant:
//! nicht lesbare Dateien oder fehlende Umgebungsvariablen werden als
//! „Merkmal nicht vorhanden" gewertet. Es gibt keinen Error-Typ.
//!
//! # Concurrency
//! Alle Typen sind `Send + Sync`. [`Platform::detect`] liest nur den Prozess-
//! Environment-Snapshot und einige Dateien; es werden keine Locks gehalten und
//! keine Threads gestartet. Der Aufruf ist von mehreren Threads aus sicher.
//!
//! # Examples
//! ```rust,no_run
//! use harw_install::platform::Platform;
//!
//! let p = Platform::detect();
//! if p.container {
//!     // container-spezifische Pfade wählen
//! }
//! ```

use std::path::Path;

/// Betriebssystem-Klassifikation der aktuellen Zielplattform.
///
/// # Description
/// Wird zur Compile-Zeit aus `cfg!(target_os = ...)` abgeleitet und ist somit
/// für ein gegebenes Binary konstant. `Other` deckt alle nicht explizit
/// unterstützten Zielsysteme ab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    /// Linux-basierte Systeme.
    Linux,
    /// Apple macOS.
    MacOs,
    /// Microsoft Windows.
    Windows,
    /// Ein nicht explizit unterstütztes Zielsystem.
    Other,
}

/// Aggregiertes Ergebnis der Umgebungssonden.
///
/// # Description
/// Fasst Betriebssystem und Umgebungsmerkmale zusammen, die die
/// Installationsstrategie beeinflussen. Alle Felder sind das Resultat
/// fehlertoleranter, read-only Sonden aus [`Platform::detect`].
///
/// # Concurrency
/// `Platform` ist `Send + Sync` und enthält nur `Copy`-Felder; es kann frei
/// zwischen Threads geteilt oder kopiert werden.
#[derive(Debug, Clone)]
pub struct Platform {
    /// Erkanntes Betriebssystem (aus `cfg!(target_os = ...)`).
    pub os: Os,
    /// `true`, wenn der Prozess in einem Container läuft.
    pub container: bool,
    /// `true`, wenn der Prozess unter Windows Subsystem for Linux läuft.
    pub wsl: bool,
    /// `true`, wenn der Prozess in einer Termux-Umgebung läuft.
    pub termux: bool,
    /// `true`, wenn die Umgebung deklarativ verwaltet ist (z. B. Nix).
    pub managed: bool,
}

impl Platform {
    /// Erkennt die aktuelle Laufzeitumgebung über read-only Sonden.
    ///
    /// # Description
    /// Führt reine Environment- und Datei-Sonden aus (siehe Contract-Abschnitt
    /// `src/platform.rs`). Jede Sonde ist fehlertolerant: ein fehlendes
    /// Environment oder eine nicht lesbare Datei wird als „nicht vorhanden"
    /// interpretiert. Die Funktion verändert nichts am System.
    ///
    /// # Returns
    /// Ein [`Platform`] mit den erkannten Merkmalen.
    ///
    /// # Concurrency
    /// Sicher aus mehreren Threads aufrufbar; hält keine Locks, startet keine
    /// Threads.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_install::platform::Platform;
    ///
    /// let p = Platform::detect();
    /// println!("{:?}", p.os);
    /// ```
    pub fn detect() -> Self {
        Platform {
            os: detect_os(),
            container: detect_container(),
            wsl: detect_wsl(),
            termux: detect_termux(),
            managed: detect_managed(),
        }
    }
}

/// Leitet das Betriebssystem aus den Compile-Zeit-Flags ab.
fn detect_os() -> Os {
    if cfg!(target_os = "linux") {
        Os::Linux
    } else if cfg!(target_os = "macos") {
        Os::MacOs
    } else if cfg!(target_os = "windows") {
        Os::Windows
    } else {
        Os::Other
    }
}

/// Erkennt eine Container-Umgebung: `/.dockerenv`, cgroup-Marker oder
/// `HARW_CONTAINER`-Env.
fn detect_container() -> bool {
    if std::env::var_os("HARW_CONTAINER").is_some() {
        return true;
    }
    if Path::new("/.dockerenv").exists() {
        return true;
    }
    match std::fs::read_to_string("/proc/1/cgroup") {
        Ok(contents) => contents.contains("docker") || contents.contains("containerd"),
        Err(_) => false,
    }
}

/// Erkennt WSL über die `WSL_DISTRO_NAME`-Env oder den `microsoft`-Marker in
/// `/proc/version`.
fn detect_wsl() -> bool {
    if std::env::var_os("WSL_DISTRO_NAME").is_some() {
        return true;
    }
    match std::fs::read_to_string("/proc/version") {
        Ok(contents) => contents.to_ascii_lowercase().contains("microsoft"),
        Err(_) => false,
    }
}

/// Erkennt Termux über die `TERMUX_VERSION`-Env.
fn detect_termux() -> bool {
    std::env::var_os("TERMUX_VERSION").is_some()
}

/// Erkennt eine verwaltete Umgebung über `HARW_MANAGED`-Env oder die Präsenz
/// von `/nix/store`.
fn detect_managed() -> bool {
    if std::env::var_os("HARW_MANAGED").is_some() {
        return true;
    }
    Path::new("/nix/store").exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_does_not_panic() {
        // Sonden sind fehlertolerant; detect() muss immer ein Platform liefern.
        let p = Platform::detect();
        // container/wsl/termux/managed sind bool und damit stets valide.
        let _ = (p.container, p.wsl, p.termux, p.managed);
    }

    #[test]
    fn test_detect_os_matches_cfg() {
        let os = detect_os();
        if cfg!(target_os = "linux") {
            assert_eq!(os, Os::Linux);
        } else if cfg!(target_os = "macos") {
            assert_eq!(os, Os::MacOs);
        } else if cfg!(target_os = "windows") {
            assert_eq!(os, Os::Windows);
        } else {
            assert_eq!(os, Os::Other);
        }
    }

    #[test]
    fn test_detect_os_field_consistent() {
        assert_eq!(Platform::detect().os, detect_os());
    }
}
