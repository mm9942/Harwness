//! [`CompilerEnv`]: where the compiler looks for definitions, skills,
//! runners, sources and caches, and [`InstallRecord`], the file `make
//! install` leaves behind.
//!
//! Every path the compiler reads or writes is derived from one
//! [`CompilerEnv`], so tests build one over temporary directories and the
//! CLI and the `/agent` op build one from `--home`/`HARW_HOME` and the
//! working directory.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::CompileError;

/// The harw version this compiler belongs to (the workspace version).
pub const HARW_VERSION: &str = env!("CARGO_PKG_VERSION");

/// File name of the install record in the harw home.
pub const INSTALL_RECORD_FILE: &str = "install.toml";

/// Schema label of [`InstallRecord`].
pub const INSTALL_RECORD_SCHEMA: &str = "harwness.install/v1";

/// Environment variable that overrides the harw source directory for
/// `--native`.
pub const HARW_SRC_ENV: &str = "HARW_SRC";

/// Name of the runner executable.
pub const RUNNER_BINARY: &str = "harw-agent-runner";

/// The target triple of the running host, in rustc's spelling.
///
/// # Description
/// Built from `std::env::consts` (Linux is assumed to be `gnu`); this is the
/// directory name under `runners/` and the `--target` default.
#[must_use]
pub fn host_target() -> String {
    let arch = std::env::consts::ARCH;
    match std::env::consts::OS {
        "linux" => format!("{arch}-unknown-linux-gnu"),
        "macos" => format!("{arch}-apple-darwin"),
        "windows" => format!("{arch}-pc-windows-msvc"),
        other => format!("{arch}-unknown-{other}"),
    }
}

/// What `make install` records about the installation
/// (`~/.harw/install.toml`).
///
/// # Description
/// `source_dir` lets `--native` find the harw sources without a flag;
/// `bindir` is where `harw` and `harw-agent-runner` were installed. The file
/// is written by `harw agent install-record` (called from `make install`)
/// and removed by uninstall.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallRecord {
    /// Always [`INSTALL_RECORD_SCHEMA`].
    pub schema: String,
    /// Installed harw version.
    pub version: String,
    /// Target triple of the installed binaries.
    pub target: String,
    /// The harw source checkout `make install` ran in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_dir: Option<PathBuf>,
    /// Directory `harw` and `harw-agent-runner` were installed to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bindir: Option<PathBuf>,
}

impl InstallRecord {
    /// A record for this harw version and the host target.
    #[must_use]
    pub fn new(source_dir: Option<PathBuf>, bindir: Option<PathBuf>) -> Self {
        Self {
            schema: INSTALL_RECORD_SCHEMA.to_owned(),
            version: HARW_VERSION.to_owned(),
            target: host_target(),
            source_dir,
            bindir,
        }
    }

    /// Reads `<home>/install.toml`; `Ok(None)` if it does not exist.
    ///
    /// # Errors
    /// [`CompileError::Io`] if it cannot be read, [`CompileError::Other`] if
    /// it does not parse or has another schema.
    pub fn read(home: &Path) -> Result<Option<Self>, CompileError> {
        let path = home.join(INSTALL_RECORD_FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(CompileError::Io {
                    context: format!("read {}", path.display()),
                    source: error,
                });
            }
        };
        let record: Self = toml::from_str(&text).map_err(|error| {
            CompileError::Other(format!("{} does not parse: {error}", path.display()))
        })?;
        if record.schema != INSTALL_RECORD_SCHEMA {
            return Err(CompileError::Other(format!(
                "{} has schema `{}`, expected `{INSTALL_RECORD_SCHEMA}`",
                path.display(),
                record.schema
            )));
        }
        Ok(Some(record))
    }

    /// Writes the record to `<home>/install.toml` (creating `home`).
    ///
    /// # Errors
    /// [`CompileError::Io`] on a write failure.
    pub fn write(&self, home: &Path) -> Result<PathBuf, CompileError> {
        std::fs::create_dir_all(home)
            .map_err(CompileError::io(format!("create {}", home.display())))?;
        let path = home.join(INSTALL_RECORD_FILE);
        let text = toml::to_string(self)
            .map_err(|error| CompileError::Other(format!("serialize install record: {error}")))?;
        std::fs::write(&path, text).map_err(CompileError::io(format!("write {}", path.display())))?;
        Ok(path)
    }
}

/// Everything the compiler needs to know about its surroundings.
#[derive(Debug, Clone)]
pub struct CompilerEnv {
    /// The harw home (`~/.harw` or `HARW_HOME`).
    pub home: PathBuf,
    /// Working directory (relative outputs, project layer).
    pub cwd: PathBuf,
    /// Config layers in ascending precedence (home, profile, trusted
    /// project `.harw`).
    pub layers: Vec<PathBuf>,
    /// Target triple of the host.
    pub host_target: String,
    /// The running executable (`harw`), for the runner-next-to-harw lookup.
    pub current_exe: Option<PathBuf>,
    /// `HARW_SRC`, if set.
    pub env_harw_src: Option<PathBuf>,
    /// `PATH`, for `cargo` detection and the `~/.harw/bin` doctor check.
    pub path_var: Option<OsString>,
    /// The user's home directory (for `~/.cargo/bin`).
    pub user_home: Option<PathBuf>,
}

impl CompilerEnv {
    /// Detects the environment of the running process.
    ///
    /// # Description
    /// `home` is `home_override`, else `harw_home::paths::home_dir()`
    /// (respects `HARW_HOME`). The layers come from
    /// `harw_home::paths::config_layers_report_at` (home, active profile,
    /// a *trusted* project `.harw`); if that fails, the home alone.
    ///
    /// # Errors
    /// [`CompileError::Other`] if no home directory can be determined.
    pub fn detect(home_override: Option<PathBuf>, cwd: PathBuf) -> Result<Self, CompileError> {
        let home = match home_override {
            Some(home) => home,
            None => harw_home::paths::home_dir()
                .map_err(|error| CompileError::Other(format!("no harw home: {error}")))?,
        };
        let layers = harw_home::paths::config_layers_report_at(&home, &cwd)
            .map(|report| report.layers)
            .unwrap_or_else(|_| vec![home.clone()]);
        Ok(Self {
            layers,
            cwd,
            host_target: host_target(),
            current_exe: std::env::current_exe().ok(),
            env_harw_src: std::env::var_os(HARW_SRC_ENV)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
            path_var: std::env::var_os("PATH"),
            user_home: std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
            home,
        })
    }

    /// An isolated environment: `home` is the only layer, nothing is taken
    /// from the process (tests, embedding).
    #[must_use]
    pub fn isolated(home: PathBuf, cwd: PathBuf) -> Self {
        Self {
            layers: vec![home.clone()],
            home,
            cwd,
            host_target: host_target(),
            current_exe: None,
            env_harw_src: None,
            path_var: None,
            user_home: None,
        }
    }

    /// `~/.harw/bin`: installed compiled agents.
    #[must_use]
    pub fn bin_dir(&self) -> PathBuf {
        self.home.join("bin")
    }

    /// `~/.harw/cache/agent-builds`: the native build cache.
    #[must_use]
    pub fn build_cache_dir(&self) -> PathBuf {
        harw_home::paths::cache_dir(&self.home).join("agent-builds")
    }

    /// `~/.harw/bin/.runners/<target>/<version>`.
    #[must_use]
    pub fn bin_runner_dir(&self, target: &str) -> PathBuf {
        self.bin_dir()
            .join(".runners")
            .join(target)
            .join(HARW_VERSION)
    }

    /// `~/.harw/runners/<target>/<version>`.
    #[must_use]
    pub fn home_runner_dir(&self, target: &str) -> PathBuf {
        self.home.join("runners").join(target).join(HARW_VERSION)
    }

    /// The install record, if any (a broken record reads as none).
    #[must_use]
    pub fn install_record(&self) -> Option<InstallRecord> {
        InstallRecord::read(&self.home).ok().flatten()
    }

    /// Resolves `path` against [`Self::cwd`].
    #[must_use]
    pub fn resolve(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.cwd.join(path)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_install_record_roundtrip_and_absence() -> Result<(), Box<dyn std::error::Error>> {
        let home = tempfile::tempdir()?;
        assert_eq!(InstallRecord::read(home.path())?, None);
        let record = InstallRecord::new(
            Some(PathBuf::from("/src/harwness")),
            Some(PathBuf::from("/usr/local/bin")),
        );
        let path = record.write(home.path())?;
        assert_eq!(path, home.path().join(INSTALL_RECORD_FILE));
        let back = InstallRecord::read(home.path())?;
        assert_eq!(back.as_ref(), Some(&record));
        assert_eq!(
            back.and_then(|record| record.source_dir),
            Some(PathBuf::from("/src/harwness"))
        );
        Ok(())
    }

    #[test]
    fn test_install_record_rejects_foreign_schema() -> Result<(), Box<dyn std::error::Error>> {
        let home = tempfile::tempdir()?;
        std::fs::write(
            home.path().join(INSTALL_RECORD_FILE),
            "schema = \"other/v9\"\nversion = \"0\"\ntarget = \"x\"\n",
        )?;
        assert!(InstallRecord::read(home.path()).is_err());
        Ok(())
    }

    #[test]
    fn test_paths_follow_the_home() {
        let env = CompilerEnv::isolated(PathBuf::from("/h"), PathBuf::from("/w"));
        assert_eq!(env.bin_dir(), PathBuf::from("/h/bin"));
        assert_eq!(
            env.build_cache_dir(),
            PathBuf::from("/h/cache/agent-builds")
        );
        assert_eq!(
            env.bin_runner_dir("x86_64-unknown-linux-gnu"),
            PathBuf::from(format!(
                "/h/bin/.runners/x86_64-unknown-linux-gnu/{HARW_VERSION}"
            ))
        );
        assert_eq!(env.resolve(Path::new("out")), PathBuf::from("/w/out"));
    }

    #[test]
    fn test_host_target_has_three_or_four_parts() {
        let target = host_target();
        assert!(target.split('-').count() >= 3, "{target}");
    }
}
