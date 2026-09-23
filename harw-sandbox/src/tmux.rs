//! tmux-Inspektionsmodul für die Prozess-Sandbox.
//!
//! Das Modul bindet ausschließlich einen einzigen, beim Aufbau validierten
//! lokalen tmux-Socket in die Sandbox ein. Es bindet niemals pauschal `/tmp`
//! oder `$HOME`. Der Socket wird an einen festen Pfad innerhalb der Sandbox
//! gelegt, damit ein beschreibbarer `CARGO_HOME` oder `PATH`-Eintrag keinen
//! Einfluss auf die tmux-Kommunikation hat.
//!
//! [`TmuxSandboxProfile`] ist ein vertrauenswürdiger Launcher-Input, kein
//! Tool-Argument. Ein Modell kann das Profil weder setzen noch den Socket-Pfad
//! zur Laufzeit überschreiben.

use std::fmt;
use std::path::{Component, Path, PathBuf};

/// Fester Pfad des tmux-Sockets *in* der Sandbox (read-write gebunden).
pub const SANDBOX_TMUX_SOCKET_PATH: &str = "/run/harw/tmux.sock";

/// Art einer tmux-Operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TmuxOperationMode {
    /// Nur lesende Abfragen: `list-sessions`, `capture-pane`, `show-options`.
    Inspect,
    /// Schreibende Aktionen wie `send-keys`, `kill-session`. Erfordert eine
    /// separate, zustimmungspflichtige Freigabe.
    Write,
}

impl TmuxOperationMode {
    #[must_use]
    pub(crate) fn socket_writable(self) -> bool {
        matches!(self, Self::Write)
    }
}

/// Geprüfter, hostseitig konfigurierter tmux-Socket.
///
/// `socket_path` ist ein konkretes, auf dem Host existierendes Unix-Socket-File.
/// Es wird in der Sandbox unter [`SANDBOX_TMUX_SOCKET_PATH`] sichtbar gebunden.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxSandboxProfile {
    mode: TmuxOperationMode,
    socket_path: PathBuf,
}

impl TmuxSandboxProfile {
    /// Baut und prüft ein Profil aus vertrauenswürdiger Host-Konfiguration.
    ///
    /// # Errors
    /// [`TmuxProfileError::InvalidPath`]: Pfad nicht absolut oder nicht normal.
    /// [`TmuxProfileError::Missing`]: Pfad existiert nicht.
    /// [`TmuxProfileError::NotSocket`]: Pfad ist kein Unix-Domain-Socket.
    pub fn new(
        mode: TmuxOperationMode,
        socket_path: impl AsRef<Path>,
    ) -> Result<Self, TmuxProfileError> {
        let socket_path = canonical_socket(socket_path.as_ref())?;
        Ok(Self { mode, socket_path })
    }

    #[must_use]
    pub fn mode(&self) -> TmuxOperationMode {
        self.mode
    }

    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TmuxProfileError {
    InvalidPath { path: PathBuf },
    Missing { path: PathBuf },
    NotSocket { path: PathBuf },
}

impl fmt::Display for TmuxProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath { path } => write!(
                f,
                "tmux socket path must be absolute and normal: '{}'",
                path.display()
            ),
            Self::Missing { path } => {
                write!(f, "tmux socket path does not exist: '{}'", path.display())
            }
            Self::NotSocket { path } => write!(
                f,
                "tmux socket path is not a Unix domain socket: '{}'",
                path.display()
            ),
        }
    }
}

impl std::error::Error for TmuxProfileError {}

/// Absolut, mindestens eine normale Komponente, nur RootDir/Normal-Komponenten.
fn is_absolute_normal(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .any(|component| matches!(component, Component::Normal(_)))
        && path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
}

fn canonical_socket(path: &Path) -> Result<PathBuf, TmuxProfileError> {
    if !is_absolute_normal(path) {
        return Err(TmuxProfileError::InvalidPath {
            path: path.to_path_buf(),
        });
    }
    let canonical = path.canonicalize().map_err(|_| TmuxProfileError::Missing {
        path: path.to_path_buf(),
    })?;
    // Unix: Prüfe, ob der Pfad ein Socket ist.
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        let meta = std::fs::metadata(&canonical).map_err(|_| TmuxProfileError::Missing {
            path: canonical.clone(),
        })?;
        if !meta.file_type().is_socket() {
            return Err(TmuxProfileError::NotSocket { path: canonical });
        }
    }
    #[cfg(not(unix))]
    {
        let _ = &canonical;
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestError;
    use crate::test_support::TestResult;

    #[test]
    fn rejects_relative_path() -> TestResult {
        let result = TmuxSandboxProfile::new(TmuxOperationMode::Inspect, "tmux-socket");
        let Err(err) = result else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(matches!(err, TmuxProfileError::InvalidPath { .. }));
        Ok(())
    }

    #[test]
    fn rejects_dotdot_path() -> TestResult {
        let result = TmuxSandboxProfile::new(TmuxOperationMode::Inspect, "/tmp/../etc/tmux-socket");
        let Err(err) = result else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(matches!(err, TmuxProfileError::InvalidPath { .. }));
        Ok(())
    }

    #[test]
    fn rejects_missing_socket() -> TestResult {
        let missing = std::env::temp_dir().join(format!(
            "harwness-tmux-missing-{}/socket",
            std::process::id()
        ));
        let result = TmuxSandboxProfile::new(TmuxOperationMode::Inspect, &missing);
        let Err(err) = result else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(matches!(err, TmuxProfileError::Missing { .. }));
        Ok(())
    }

    #[test]
    fn rejects_non_socket_file() -> TestResult {
        let tmp =
            std::env::temp_dir().join(format!("harwness-tmux-notsock-{}", std::process::id()));
        std::fs::write(&tmp, b"not a socket")?;
        let result = TmuxSandboxProfile::new(TmuxOperationMode::Inspect, &tmp);
        let Err(err) = result else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(matches!(err, TmuxProfileError::NotSocket { .. }));
        std::fs::remove_file(&tmp).ok();
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn accepts_real_unix_socket() -> TestResult {
        use std::os::unix::net::UnixListener;
        let dir = std::env::temp_dir().join(format!("harwness-tmux-real-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let sock = dir.join("tmux.sock");
        let _listener = UnixListener::bind(&sock)?;
        let profile = TmuxSandboxProfile::new(TmuxOperationMode::Inspect, &sock)?;
        assert_eq!(profile.mode(), TmuxOperationMode::Inspect);
        assert_eq!(profile.socket_path(), &sock.canonicalize()?);
        std::fs::remove_dir_all(&dir).ok();
        Ok(())
    }
}
