//! `harw agent doctor`: is everything there to build agents?

use std::path::Path;

use serde::Serialize;

use crate::backend::native::{NativeFlavor, find_cargo, find_sources};
use crate::backend::runner::{RunnerProbe, locate_runner};
use crate::bin_dir::BinDir;
use crate::cache::{AgentCompilerSettings, dir_size};
use crate::env::{CompilerEnv, HARW_VERSION, InstallRecord};
use crate::uia::auto_build_state;

/// Status of one check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DoctorStatus {
    /// Fine.
    Ok,
    /// Works, but something is missing or off.
    Warn,
    /// Informational.
    Info,
}

/// One check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorCheck {
    /// Check name.
    pub name: String,
    /// Status.
    pub status: DoctorStatus,
    /// What was found.
    pub detail: String,
    /// How to fix it, if needed.
    pub fix: Option<String>,
}

fn check(name: &str, status: DoctorStatus, detail: String, fix: Option<String>) -> DoctorCheck {
    DoctorCheck {
        name: name.to_owned(),
        status,
        detail,
        fix,
    }
}

/// Human-readable byte size.
#[must_use]
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes;
    let mut unit = 0;
    while value >= 1024 * 10 && unit + 1 < UNITS.len() {
        value /= 1024;
        unit += 1;
    }
    format!("{value} {}", UNITS[unit])
}

/// `true` if `dir` is one of the `PATH` entries.
#[must_use]
pub fn on_path(env: &CompilerEnv, dir: &Path) -> bool {
    env.path_var
        .as_ref()
        .is_some_and(|path| std::env::split_paths(path).any(|entry| entry == dir))
}

/// The line that puts `dir` on `PATH`, for the user's shell.
#[must_use]
pub fn path_line(dir: &Path) -> String {
    format!("export PATH=\"{}:$PATH\"", dir.display())
}

/// Runs every check.
#[must_use]
pub fn run_doctor(env: &CompilerEnv, probe: &dyn RunnerProbe) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();
    let record = InstallRecord::read(&env.home);
    checks.push(match &record {
        Ok(Some(record)) => check(
            "install record",
            if record.version == HARW_VERSION {
                DoctorStatus::Ok
            } else {
                DoctorStatus::Warn
            },
            format!(
                "{} (harw {}, {}, sources {})",
                env.home.join(crate::env::INSTALL_RECORD_FILE).display(),
                record.version,
                record.target,
                record
                    .source_dir
                    .as_ref()
                    .map_or_else(|| "-".to_owned(), |dir| dir.display().to_string())
            ),
            (record.version != HARW_VERSION)
                .then(|| "run `make install` again in the harw sources".to_owned()),
        ),
        Ok(None) => check(
            "install record",
            DoctorStatus::Warn,
            "none (harw was not installed with `make install`)".to_owned(),
            Some(
                "run `make install` in the harw sources, or pass `--harw-src` to `--native` builds"
                    .to_owned(),
            ),
        ),
        Err(error) => check(
            "install record",
            DoctorStatus::Warn,
            error.to_string(),
            None,
        ),
    });

    match locate_runner(env, None, &env.host_target) {
        Ok(runner) => {
            let detail = format!("{} ({:?})", runner.path.display(), runner.source);
            match probe.capabilities(&runner.path) {
                Ok(capabilities) => checks.push(check(
                    "runner",
                    DoctorStatus::Ok,
                    format!(
                        "{detail}; interfaces {}; {} features{}",
                        capabilities.interfaces.join(", "),
                        capabilities.features.len(),
                        capabilities
                            .version_note()
                            .map(|note| format!("; {note}"))
                            .unwrap_or_default()
                    ),
                    None,
                )),
                Err(error) => checks.push(check(
                    "runner",
                    DoctorStatus::Warn,
                    format!("{detail}; {error}"),
                    Some("reinstall the runner with `make install`".to_owned()),
                )),
            }
        }
        Err(error) => checks.push(check(
            "runner",
            DoctorStatus::Warn,
            error.to_string(),
            Some("run `make install`; until then use `harw agent build --artifact-only` or `--native`".to_owned()),
        )),
    }

    checks.push(match find_cargo(env) {
        Ok(cargo) => check(
            "native: cargo",
            DoctorStatus::Ok,
            cargo.display().to_string(),
            None,
        ),
        Err(error) => check(
            "native: cargo",
            DoctorStatus::Info,
            "not found (only `--native` needs it)".to_owned(),
            Some(error.to_string()),
        ),
    });
    checks.push(match find_sources(env, None, NativeFlavor::Runner) {
        Ok((dir, origin)) => check(
            "native: harw sources",
            DoctorStatus::Ok,
            format!("{} ({origin:?})", dir.display()),
            None,
        ),
        Err(error) => check(
            "native: harw sources",
            DoctorStatus::Info,
            "not available (only `--native` needs them)".to_owned(),
            Some(error.to_string()),
        ),
    });

    let settings = AgentCompilerSettings::load(env);
    let cache = env.build_cache_dir();
    checks.push(check(
        "build cache",
        DoctorStatus::Info,
        format!(
            "{}: {} of {} (`harw agent clean` trims it)",
            cache.display(),
            human_bytes(dir_size(&cache)),
            human_bytes(settings.max_bytes())
        ),
        None,
    ));

    let bin = env.bin_dir();
    let installed = BinDir::new(bin.clone()).names();
    checks.push(check(
        "bin dir",
        DoctorStatus::Info,
        format!(
            "{}: {} agent(s), {}",
            bin.display(),
            installed.len(),
            human_bytes(dir_size(&bin))
        ),
        None,
    ));
    checks.push(if on_path(env, &bin) {
        check("bin dir on PATH", DoctorStatus::Ok, "yes".to_owned(), None)
    } else {
        check(
            "bin dir on PATH",
            DoctorStatus::Warn,
            format!("{} is not on PATH", bin.display()),
            Some(format!(
                "add this line to your shell rc (~/.bashrc, ~/.zshrc): {}",
                path_line(&bin)
            )),
        )
    });

    checks.push(match auto_build_state(env) {
        Some(state) => check(
            "UIA auto-build",
            if state.status == "built" {
                DoctorStatus::Ok
            } else {
                DoctorStatus::Warn
            },
            format!(
                "{} `{}` ({}){}",
                state.status,
                state.name,
                state.uia,
                state
                    .message
                    .as_deref()
                    .map(|message| format!(": {message}"))
                    .unwrap_or_default()
            ),
            None,
        ),
        None => check(
            "UIA auto-build",
            DoctorStatus::Info,
            if settings.auto_build_uia() {
                "no build yet (needs an active UIA and an installed runner)".to_owned()
            } else {
                "disabled ([agent_compiler] auto_build_uia = false)".to_owned()
            },
            None,
        ),
    });
    checks
}

/// Text form.
#[must_use]
pub fn render_doctor(checks: &[DoctorCheck]) -> String {
    let mut out = String::new();
    for item in checks {
        let mark = match item.status {
            DoctorStatus::Ok => "ok  ",
            DoctorStatus::Warn => "warn",
            DoctorStatus::Info => "info",
        };
        out.push_str(&format!("[{mark}] {}: {}\n", item.name, item.detail));
        if let Some(fix) = &item.fix {
            out.push_str(&format!("       → {fix}\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn test_path_detection_and_rc_line() {
        let mut env = CompilerEnv::isolated(PathBuf::from("/h"), PathBuf::from("/w"));
        assert!(!on_path(&env, Path::new("/h/bin")));
        env.path_var = Some(std::ffi::OsString::from("/usr/bin:/h/bin"));
        assert!(on_path(&env, Path::new("/h/bin")));
        assert_eq!(
            path_line(Path::new("/h/bin")),
            "export PATH=\"/h/bin:$PATH\""
        );
    }

    #[test]
    fn test_human_bytes() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(5 * 1024 * 1024 * 1024), "5120 MiB");
    }
}
