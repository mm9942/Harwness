//! Ressourcengrenzen für `shell.exec` (W1-03, F-061).
//!
//! Das Crate verbietet `unsafe` (`unsafe_code = "forbid"`), daher gibt es kein
//! `pre_exec`/`setrlimit` im Kindprozess. Stattdessen wird vor `bwrap` ein
//! festgepinntes `prlimit` (util-linux) gestartet, das die Limits für sich
//! selbst setzt und dann `bwrap` per `exec` startet. rlimits werden über
//! `exec`, `fork` und `bwrap` hinweg vererbt; der Sandbox-Prozessbaum kann sie
//! nur senken, nie erhöhen (Soft = Hard).
//!
//! Das tmpfs unter `/tmp` wird zusätzlich über bwrap `--size` begrenzt
//! (bubblewrap ≥ 0.7.0).
//!
//! # Hinweis zu `nproc`
//! `RLIMIT_NPROC` zählt **alle Tasks (inkl. Threads) der realen UID**, auch
//! außerhalb der Sandbox: Beim Anlegen des User-Namespace übernimmt der Kernel
//! das Limit des Erzeugers als Obergrenze für die Eltern-Ebene. Liegt der Wert
//! unter der aktuellen Task-Zahl des Nutzers, scheitert bereits bwrap
//! („Creating new namespace failed: Resource temporarily unavailable“). Auf
//! dem Referenzsystem (Desktop mit sway/kitty) liefen 571 bzw. 759 Tasks, ein
//! Limit von 256 ist dort nachweislich unbrauchbar. Der Default 4096 lässt dem
//! Desktop Luft und stoppt eine Fork-Bombe weit vor dem Nutzerlimit (32072).

use std::ffi::OsString;
use std::fmt;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};

use harw_sandbox::BwrapLauncher;

/// Feste Suchpfade für `prlimit` (util-linux). `PATH` wird nie ausgewertet,
/// nur diese beiden absoluten Pfade — analog zu `BWRAP_CANDIDATES` in
/// `harw-sandbox/src/bwrap.rs` deckt das die beiden verbreiteten Layouts ab,
/// unter denen util-linux `prlimit` typischerweise liegt (`/usr/bin` bzw.
/// `/bin`, Letzteres u. a. wenn `/bin` kein Symlink auf `/usr/bin` ist).
/// Andere Layouts (z. B. nur `/usr/local/bin/prlimit`) sind bewusst **nicht**
/// abgedeckt: fehlt `prlimit` an beiden Pfaden, bleibt
/// `ShellLimits::default().require_rlimits == true` fail-closed (siehe
/// [`ShellLimitsError::PrlimitUnavailable`], dessen Fehlertext das fehlende
/// util-linux/prlimit benennt) und der Konfig-Schalter `require_rlimits` auf
/// [`ShellLimits`], mit dem ein Betreiber den Aufruf bewusst ohne rlimits
/// zulassen kann.
pub const PRLIMIT_CANDIDATES: [&str; 2] = ["/usr/bin/prlimit", "/bin/prlimit"];

const GIB: u64 = 1024 * 1024 * 1024;
const MIB: u64 = 1024 * 1024;

/// Ressourcengrenzen je `shell.exec`-Aufruf.
///
/// Alle Zahlenwerte müssen größer als null sein. `require_rlimits = true`
/// (Default) lässt den Aufruf fehlschlagen, wenn `prlimit` fehlt; mit `false`
/// läuft das Kommando ohne rlimits (nur tmpfs-Grenze) und es wird gewarnt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShellLimits {
    /// `RLIMIT_AS`: maximaler virtueller Adressraum je Prozess in Bytes.
    pub as_bytes: u64,
    /// `RLIMIT_CPU`: CPU-Zeit je Prozess in Sekunden.
    pub cpu_secs: u64,
    /// `RLIMIT_FSIZE`: maximale Dateigröße in Bytes.
    pub fsize_bytes: u64,
    /// `RLIMIT_NOFILE`: maximale Zahl offener Dateideskriptoren je Prozess.
    pub nofile: u64,
    /// `RLIMIT_NPROC`: Obergrenze der Tasks der realen UID (siehe Moduldoku).
    pub nproc: u64,
    /// Größe des tmpfs unter `/tmp` in Bytes (bwrap `--size`).
    pub tmpfs_bytes: u64,
    /// Fehlendes `prlimit` ist ein Fehler statt eines stillen Weglassens.
    pub require_rlimits: bool,
}

impl Default for ShellLimits {
    fn default() -> Self {
        Self {
            as_bytes: 2 * GIB,
            cpu_secs: 60,
            fsize_bytes: 256 * MIB,
            nofile: 256,
            nproc: 4096,
            tmpfs_bytes: 256 * MIB,
            require_rlimits: true,
        }
    }
}

/// Typisierte Fehler beim Anwenden von [`ShellLimits`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellLimitsError {
    /// Ein Limit ist null; `prlimit`/bwrap würden es ablehnen oder die Sandbox
    /// wäre unbenutzbar.
    ZeroLimit {
        /// Name des Feldes in [`ShellLimits`].
        name: &'static str,
    },
    /// `require_rlimits` ist gesetzt, aber an keinem festen Pfad liegt ein
    /// vertrauenswürdiges `prlimit`.
    PrlimitUnavailable {
        /// Geprüfte feste Pfade.
        candidates: Vec<PathBuf>,
    },
}

impl fmt::Display for ShellLimitsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroLimit { name } => {
                write!(f, "resource limit '{name}' must be greater than zero")
            }
            Self::PrlimitUnavailable { candidates } => {
                let paths = candidates
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(
                    f,
                    "util-linux/prlimit fehlt: kein vertrauenswürdiges prlimit an den festen \
                     Pfaden {paths} gefunden (PATH is never searched); util-linux installieren, \
                     oder ShellLimits::require_rlimits = false setzen, um shell.exec bewusst \
                     ohne rlimits laufen zu lassen"
                )
            }
        }
    }
}

impl std::error::Error for ShellLimitsError {}

impl ShellLimits {
    /// Prüft, dass alle Zahlenwerte größer als null sind.
    ///
    /// # Errors
    /// [`ShellLimitsError::ZeroLimit`] mit dem ersten Feld, das null ist.
    pub fn validate(&self) -> Result<(), ShellLimitsError> {
        let fields = [
            ("as_bytes", self.as_bytes),
            ("cpu_secs", self.cpu_secs),
            ("fsize_bytes", self.fsize_bytes),
            ("nofile", self.nofile),
            ("nproc", self.nproc),
            ("tmpfs_bytes", self.tmpfs_bytes),
        ];
        match fields.iter().find(|(_, value)| *value == 0) {
            Some((name, _)) => Err(ShellLimitsError::ZeroLimit { name }),
            None => Ok(()),
        }
    }

    /// tmpfs-Größe als `NonZeroU64` für [`BwrapLauncher::with_tmpfs_size`].
    ///
    /// # Errors
    /// [`ShellLimitsError::ZeroLimit`], wenn `tmpfs_bytes == 0`.
    pub fn tmpfs_size(&self) -> Result<NonZeroU64, ShellLimitsError> {
        NonZeroU64::new(self.tmpfs_bytes).ok_or(ShellLimitsError::ZeroLimit {
            name: "tmpfs_bytes",
        })
    }

    /// Argumente für `prlimit` bis einschließlich `--`. Soft- und Hard-Limit
    /// werden gleich gesetzt (`--res=N` setzt beide, prlimit(1)), damit die
    /// Sandbox das Soft-Limit nicht wieder bis zum Hard-Limit anheben kann.
    #[must_use]
    pub fn prlimit_args(&self) -> Vec<OsString> {
        vec![
            OsString::from(format!("--as={}", self.as_bytes)),
            OsString::from(format!("--cpu={}", self.cpu_secs)),
            OsString::from(format!("--fsize={}", self.fsize_bytes)),
            OsString::from(format!("--nofile={}", self.nofile)),
            OsString::from(format!("--nproc={}", self.nproc)),
            OsString::from("--"),
        ]
    }

    /// Sucht `prlimit` an [`PRLIMIT_CANDIDATES`].
    ///
    /// # Returns
    /// `Ok(Some(path))` bei Fund, `Ok(None)` wenn nicht gefunden und
    /// `require_rlimits == false`.
    ///
    /// # Errors
    /// [`ShellLimitsError::PrlimitUnavailable`], wenn nicht gefunden und
    /// `require_rlimits == true`.
    pub fn resolve_prlimit(&self) -> Result<Option<PathBuf>, ShellLimitsError> {
        let candidates = PRLIMIT_CANDIDATES.map(Path::new);
        self.resolve_prlimit_in(&candidates)
    }

    /// Wie [`resolve_prlimit`](Self::resolve_prlimit), mit expliziten Kandidaten
    /// (für Tests). Die Vertrauensregel stammt aus
    /// [`BwrapLauncher::find_pinned_executable`]: absolut, regulär, ausführbar,
    /// Eigentümer root, nicht group-/world-beschreibbar.
    pub(crate) fn resolve_prlimit_in(
        &self,
        candidates: &[&Path],
    ) -> Result<Option<PathBuf>, ShellLimitsError> {
        match BwrapLauncher::find_pinned_executable(candidates) {
            Some(path) => Ok(Some(path)),
            None if self.require_rlimits => Err(ShellLimitsError::PrlimitUnavailable {
                candidates: candidates.iter().map(|path| path.to_path_buf()).collect(),
            }),
            None => Ok(None),
        }
    }
}

/// Vollständig bestimmter Prozessstart: Programm plus Argumente. Das Programm
/// ist immer ein absoluter, festgepinnter Pfad (`prlimit` oder `bwrap`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LaunchCommand {
    pub(crate) program: PathBuf,
    pub(crate) args: Vec<OsString>,
}

/// Baut `prlimit <limits> -- <program> <args…>` bzw. ohne `prlimit` direkt
/// `<program> <args…>`. `program` wird als absoluter Pfad übergeben, damit
/// auch `prlimit` (execvp) keine `PATH`-Suche durchführt.
///
/// Nimmt bewusst ein loses `program`/`args`-Paar statt eines
/// [`harw_sandbox::BwrapCommandPlan`], damit derselbe Mechanismus auch den
/// Host-Pfad ohne `bwrap` trägt (Plan `recursive-cooking-lobster.md` Teil B1,
/// `crate::exec::ShellExecutor::run_host_command`): dort gibt es keinen
/// `BwrapCommandPlan`, nur `/bin/sh -c <command>`. Der `bwrap`-Pfad
/// (`crate::exec::ShellExecutor::run_command`) übergibt weiterhin
/// `launcher.executable()` und `plan.args()`.
pub(crate) fn launch_command(
    prlimit: Option<&Path>,
    limits: &ShellLimits,
    program: &Path,
    args: &[OsString],
) -> LaunchCommand {
    match prlimit {
        Some(prlimit) => {
            let mut prlimit_args = limits.prlimit_args();
            prlimit_args.push(program.as_os_str().to_owned());
            prlimit_args.extend(args.iter().cloned());
            LaunchCommand {
                program: prlimit.to_path_buf(),
                args: prlimit_args,
            }
        }
        None => LaunchCommand {
            program: program.to_path_buf(),
            args: args.to_vec(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_types::{TenantId, WorkspaceId};
    use tempfile::TempDir;

    fn sandbox(dir: &TempDir) -> TestResult<SandboxSpec> {
        let workspace = dir.path().join("project");
        std::fs::create_dir_all(&workspace).map_err(ctx("workspace dir"))?;
        let registry = WorkspaceRegistry::build(
            dir.path(),
            [WorkspaceRegistration {
                tenant: TenantId::from_str("tenant"),
                workspace: WorkspaceId::from_str("project"),
                root: workspace,
            }],
        )
        .map_err(ctx("registry"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("tenant"),
                &WorkspaceId::from_str("project"),
            )
            .map_err(ctx("binding"))?;
        Ok(SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy([Permission::ExecuteProcess]),
        ))
    }

    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn prlimit_candidates_are_fixed_absolute_paths_usr_bin_then_bin() {
        // R1-03: `/bin/prlimit` ergänzt `/usr/bin/prlimit` (Reihenfolge zählt —
        // `find_pinned_executable` liefert den ersten Treffer). Beide Kandidaten
        // sind feste absolute Pfade, keine PATH-Suche.
        assert_eq!(
            PRLIMIT_CANDIDATES,
            ["/usr/bin/prlimit", "/bin/prlimit"],
            "PRLIMIT_CANDIDATES must stay a fixed, ordered, absolute-path list"
        );
        for candidate in PRLIMIT_CANDIDATES {
            assert!(
                Path::new(candidate).is_absolute(),
                "candidate must be absolute: {candidate}"
            );
        }
    }

    #[test]
    fn error_message_names_util_linux_prlimit_and_the_config_switch() -> TestResult {
        // R1-03: die Fehlermeldung muss klar machen, *was* fehlt (util-linux/
        // prlimit) und *wie* man es fail-open umgehen kann (der Konfig-Schalter
        // `require_rlimits`), damit ein Betreiber nicht raten muss.
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let missing = dir.path().join("prlimit");
        let Err(error) = ShellLimits::default().resolve_prlimit_in(&[missing.as_path()]) else {
            return Err(TestError::Unexpected(
                "missing prlimit must error when required".into(),
            ));
        };
        let message = error.to_string();
        assert!(message.contains("util-linux/prlimit fehlt"), "{message}");
        assert!(message.contains("require_rlimits"), "{message}");
        assert!(message.contains("PATH is never searched"), "{message}");
        Ok(())
    }

    #[test]
    fn defaults_are_bounded_and_fail_closed() {
        let limits = ShellLimits::default();
        assert_eq!(limits.as_bytes, 2_147_483_648);
        assert_eq!(limits.cpu_secs, 60);
        assert_eq!(limits.fsize_bytes, 268_435_456);
        assert_eq!(limits.nofile, 256);
        assert_eq!(limits.nproc, 4096);
        assert_eq!(limits.tmpfs_bytes, 268_435_456);
        assert!(limits.require_rlimits);
        assert_eq!(limits.validate(), Ok(()));
    }

    #[test]
    fn zero_limits_are_rejected_by_name() {
        let limits = ShellLimits {
            nproc: 0,
            ..ShellLimits::default()
        };
        assert_eq!(
            limits.validate(),
            Err(ShellLimitsError::ZeroLimit { name: "nproc" })
        );
        let limits = ShellLimits {
            tmpfs_bytes: 0,
            ..ShellLimits::default()
        };
        assert_eq!(
            limits.tmpfs_size(),
            Err(ShellLimitsError::ZeroLimit {
                name: "tmpfs_bytes"
            })
        );
    }

    #[test]
    fn prlimit_args_golden() {
        assert_eq!(
            strings(&ShellLimits::default().prlimit_args()),
            [
                "--as=2147483648",
                "--cpu=60",
                "--fsize=268435456",
                "--nofile=256",
                "--nproc=4096",
                "--",
            ]
        );
    }

    #[test]
    fn launch_command_golden_prlimit_then_bwrap_then_plan() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let limits = ShellLimits::default();
        let launcher = BwrapLauncher::new(PathBuf::from("/usr/bin/bwrap"))
            .with_tmpfs_size(limits.tmpfs_size().map_err(ctx("tmpfs size"))?);
        let plan = launcher
            .plan(
                &sandbox(&dir)?,
                &[
                    OsString::from("/bin/sh"),
                    OsString::from("-c"),
                    OsString::from("echo hi"),
                ],
            )
            .map_err(ctx("plan"))?;

        let command = launch_command(
            Some(Path::new("/usr/bin/prlimit")),
            &limits,
            launcher.executable(),
            plan.args(),
        );

        assert_eq!(command.program, PathBuf::from("/usr/bin/prlimit"));
        let args = strings(&command.args);
        assert_eq!(
            args[..7],
            [
                "--as=2147483648",
                "--cpu=60",
                "--fsize=268435456",
                "--nofile=256",
                "--nproc=4096",
                "--",
                "/usr/bin/bwrap",
            ]
        );
        assert_eq!(args[7..], strings(plan.args())[..]);
        let tmpfs = args
            .iter()
            .position(|argument| argument == "--tmpfs")
            .ok_or(TestError::Missing("tmpfs"))?;
        assert_eq!(
            args[tmpfs - 2..tmpfs + 2],
            ["--size", "268435456", "--tmpfs", "/tmp"]
        );
        assert_eq!(args[args.len() - 3..], ["/bin/sh", "-c", "echo hi"]);
        Ok(())
    }

    #[test]
    fn launch_command_without_prlimit_starts_bwrap_directly() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let launcher = BwrapLauncher::new(PathBuf::from("/usr/bin/bwrap"));
        let plan = launcher
            .plan(&sandbox(&dir)?, &[OsString::from("/bin/true")])
            .map_err(ctx("plan"))?;

        let command = launch_command(
            None,
            &ShellLimits::default(),
            launcher.executable(),
            plan.args(),
        );

        assert_eq!(command.program, PathBuf::from("/usr/bin/bwrap"));
        assert_eq!(command.args, plan.args());
        assert!(
            !strings(&command.args)
                .iter()
                .any(|argument| argument.starts_with("--nproc"))
        );
        Ok(())
    }

    #[test]
    fn launch_command_wraps_a_bare_program_without_a_bwrap_plan() {
        // Plan Teil B1: `ShellExecutor::run_host_command` hat keinen `BwrapCommandPlan`
        // (kein `bwrap` läuft dort), sondern nur `/bin/sh -c <command>`. Derselbe
        // `launch_command`-Mechanismus muss auch dieses lose Programm/Args-Paar mit
        // `prlimit` umschließen können, ohne eine bwrap-spezifische Plan-Struktur zu
        // brauchen.
        let limits = ShellLimits::default();
        let args = [OsString::from("-c"), OsString::from("echo host_ok")];

        let command = launch_command(
            Some(Path::new("/usr/bin/prlimit")),
            &limits,
            Path::new("/bin/sh"),
            &args,
        );

        assert_eq!(command.program, PathBuf::from("/usr/bin/prlimit"));
        let strung = strings(&command.args);
        assert_eq!(
            strung,
            [
                "--as=2147483648",
                "--cpu=60",
                "--fsize=268435456",
                "--nofile=256",
                "--nproc=4096",
                "--",
                "/bin/sh",
                "-c",
                "echo host_ok",
            ]
        );
    }

    #[test]
    fn launch_command_wraps_a_bare_program_without_prlimit() {
        let args = [OsString::from("-c"), OsString::from("echo host_ok")];

        let command = launch_command(None, &ShellLimits::default(), Path::new("/bin/sh"), &args);

        assert_eq!(command.program, PathBuf::from("/bin/sh"));
        assert_eq!(command.args, args.to_vec());
    }

    #[test]
    fn missing_prlimit_is_typed_error_when_required() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let missing = dir.path().join("prlimit");
        let result = ShellLimits::default().resolve_prlimit_in(&[missing.as_path()]);
        assert_eq!(
            result,
            Err(ShellLimitsError::PrlimitUnavailable {
                candidates: vec![missing.clone()],
            })
        );
        let Err(error) = result else {
            return Err(TestError::Unexpected("error erwartet".into()));
        };
        let message = error.to_string();
        assert!(message.contains("PATH is never searched"), "{message}");
        Ok(())
    }

    #[test]
    fn missing_prlimit_is_allowed_only_when_not_required() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let missing = dir.path().join("prlimit");
        let limits = ShellLimits {
            require_rlimits: false,
            ..ShellLimits::default()
        };
        assert_eq!(limits.resolve_prlimit_in(&[missing.as_path()]), Ok(None));
        Ok(())
    }

    #[test]
    fn prlimit_is_never_resolved_via_path() {
        // `prlimit` liegt typischerweise im PATH; ein relativer Kandidat darf
        // trotzdem nie gefunden werden.
        let result = ShellLimits::default().resolve_prlimit_in(&[Path::new("prlimit")]);
        assert!(matches!(
            result,
            Err(ShellLimitsError::PrlimitUnavailable { .. })
        ));
    }

    #[test]
    fn user_owned_fake_prlimit_is_not_trusted() -> TestResult {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let fake = dir.path().join("prlimit");
        std::fs::write(&fake, b"#!/bin/sh\nexec \"$@\"\n").map_err(ctx("write fake"))?;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755))
            .map_err(ctx("chmod fake"))?;
        if std::fs::metadata(&fake).map_err(ctx("metadata"))?.uid() == 0 {
            eprintln!("übersprungen: Test läuft als root, Eigentümerprüfung nicht beobachtbar");
            return Ok(());
        }
        assert!(matches!(
            ShellLimits::default().resolve_prlimit_in(&[fake.as_path()]),
            Err(ShellLimitsError::PrlimitUnavailable { .. })
        ));
        Ok(())
    }

    #[test]
    fn fixed_prlimit_resolution_returns_only_pinned_path() -> TestResult {
        let limits = ShellLimits {
            require_rlimits: false,
            ..ShellLimits::default()
        };
        match limits.resolve_prlimit() {
            Ok(Some(path)) => assert_eq!(path, PathBuf::from(PRLIMIT_CANDIDATES[0])),
            Ok(None) => eprintln!("hinweis: kein vertrauenswürdiges /usr/bin/prlimit vorhanden"),
            Err(error) => {
                return Err(TestError::Unexpected(format!(
                    "require_rlimits=false darf nicht fehlschlagen: {error}"
                )));
            }
        }
        Ok(())
    }
}
