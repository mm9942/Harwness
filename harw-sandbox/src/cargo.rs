//! Rust/Cargo-Ausführungsprofil für Bubblewrap-Sandboxes.
//!
//! Das Profil trennt die unveränderliche Toolchain vom projektbezogenen,
//! beschreibbaren Cargo-Cache. Es ist absichtlich ein vertrauenswürdiger
//! Launcher-Input und keine Capability, die ein Tool-Aufruf selbst liefern darf.

use std::fmt;
use std::path::{Component, Path, PathBuf};

/// Art einer Cargo-Ausführung.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CargoExecutionMode {
    /// Nur bestehende Metadaten und Quellen lesen; Cargo wird offline erzwungen.
    Inspect,
    /// Bauen/Testen gegen einen projektisolierten Cache; ebenfalls offline.
    BuildOffline,
    /// Cache darf ergänzt werden. Netzwerk wird dadurch nicht gewährt; dafür sind
    /// weiterhin `NetworkAccess`, ein nichtleerer `NetworkScope` und `ProxyOnly`
    /// des Launchers erforderlich.
    Fetch,
}

impl CargoExecutionMode {
    #[must_use]
    pub(crate) fn cache_writable(self) -> bool {
        !matches!(self, Self::Inspect)
    }
    #[must_use]
    pub(crate) fn offline(self) -> bool {
        !matches!(self, Self::Fetch)
    }
}

/// Geprüfte, hostseitig konfigurierte Wurzeln eines Cargo-Profils.
///
/// `cargo_bin` ist ein konkretes, festes Executable. Es wird in der Sandbox unter
/// `/opt/harw/toolchain/bin/cargo` sichtbar, damit ein beschreibbarer `CARGO_HOME`
/// nie einen Eintrag im Suchpfad für Cargo selbst bereitstellen kann.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CargoSandboxProfile {
    mode: CargoExecutionMode,
    cargo_bin: PathBuf,
    rustup_home: PathBuf,
    cargo_home: PathBuf,
}

impl CargoSandboxProfile {
    /// Baut und prüft ein Profil aus vertrauenswürdiger Host-Konfiguration.
    pub fn new(
        mode: CargoExecutionMode,
        cargo_bin: impl AsRef<Path>,
        rustup_home: impl AsRef<Path>,
        cargo_home: impl AsRef<Path>,
    ) -> Result<Self, CargoProfileError> {
        let cargo_bin = canonical_file(cargo_bin.as_ref(), "cargo_bin")?;
        let rustup_home = canonical_dir(rustup_home.as_ref(), "rustup_home")?;
        let cargo_home = canonical_dir(cargo_home.as_ref(), "cargo_home")?;
        if cargo_home.starts_with(&rustup_home)
            || rustup_home.starts_with(&cargo_home)
            || cargo_bin.starts_with(&cargo_home)
        {
            return Err(CargoProfileError::OverlappingRoots);
        }
        Ok(Self {
            mode,
            cargo_bin,
            rustup_home,
            cargo_home,
        })
    }
    #[must_use]
    pub fn mode(&self) -> CargoExecutionMode {
        self.mode
    }
    #[must_use]
    pub fn cargo_bin(&self) -> &Path {
        &self.cargo_bin
    }
    #[must_use]
    pub fn rustup_home(&self) -> &Path {
        &self.rustup_home
    }
    #[must_use]
    pub fn cargo_home(&self) -> &Path {
        &self.cargo_home
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CargoProfileError {
    InvalidPath { field: &'static str, path: PathBuf },
    Missing { field: &'static str, path: PathBuf },
    NotDirectory { field: &'static str, path: PathBuf },
    NotExecutableFile { path: PathBuf },
    OverlappingRoots,
}
impl fmt::Display for CargoProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath { field, path } => write!(
                f,
                "Cargo profile {field} must be absolute and normal: {}",
                path.display()
            ),
            Self::Missing { field, path } => write!(
                f,
                "Cargo profile {field} does not exist: {}",
                path.display()
            ),
            Self::NotDirectory { field, path } => write!(
                f,
                "Cargo profile {field} is not a directory: {}",
                path.display()
            ),
            Self::NotExecutableFile { path } => write!(
                f,
                "Cargo executable is not an executable regular file: {}",
                path.display()
            ),
            Self::OverlappingRoots => write!(
                f,
                "Cargo cache must not overlap the Rustup home or Cargo executable"
            ),
        }
    }
}
impl std::error::Error for CargoProfileError {}
fn is_absolute_normal(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .any(|part| matches!(part, Component::Normal(_)))
        && path
            .components()
            .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
}
fn canonical_dir(path: &Path, field: &'static str) -> Result<PathBuf, CargoProfileError> {
    if !is_absolute_normal(path) {
        return Err(CargoProfileError::InvalidPath {
            field,
            path: path.to_path_buf(),
        });
    }
    let canonical = path
        .canonicalize()
        .map_err(|_| CargoProfileError::Missing {
            field,
            path: path.to_path_buf(),
        })?;
    if !canonical.is_dir() {
        return Err(CargoProfileError::NotDirectory {
            field,
            path: canonical,
        });
    }
    Ok(canonical)
}
fn canonical_file(path: &Path, field: &'static str) -> Result<PathBuf, CargoProfileError> {
    if !is_absolute_normal(path) {
        return Err(CargoProfileError::InvalidPath {
            field,
            path: path.to_path_buf(),
        });
    }
    let canonical = path
        .canonicalize()
        .map_err(|_| CargoProfileError::Missing {
            field,
            path: path.to_path_buf(),
        })?;
    let meta = std::fs::metadata(&canonical).map_err(|_| CargoProfileError::Missing {
        field,
        path: canonical.clone(),
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if !meta.is_file() || meta.permissions().mode() & 0o111 == 0 {
            return Err(CargoProfileError::NotExecutableFile { path: canonical });
        }
    }
    #[cfg(not(unix))]
    if !meta.is_file() {
        return Err(CargoProfileError::NotExecutableFile { path: canonical });
    }
    Ok(canonical)
}
