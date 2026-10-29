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

/// Wert eines Byte-Limits in [`ShellLimits`], der „keine Grenze“ bedeutet
/// (`RLIM_INFINITY`); [`ShellLimits::prlimit_args`] schreibt dafür
/// `unlimited`. Nur für `as_bytes` und `fsize_bytes` vorgesehen
/// ([`ShellLimits::for_jobs`]).
pub const RLIMIT_UNLIMITED: u64 = u64::MAX;

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
    /// `require_rlimits` is set, but `prlimit --nproc` would cap nothing
    /// (PL-97 E2g): the kernel exempts this harw process (real uid 0,
    /// `CAP_SYS_ADMIN` or `CAP_SYS_RESOURCE`, inherited through `prlimit`
    /// and `bwrap`) from `RLIMIT_NPROC`, and this path has no job cgroup
    /// whose `pids.max` could cap the process count instead.
    NprocNotEnforced {
        /// Why the identity is exempt.
        reason: &'static str,
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
            Self::NprocNotEnforced { reason } => write!(
                f,
                "RLIMIT_NPROC (nproc) wird nicht durchgesetzt: {reason}, was der Kernel von \
                 RLIMIT_NPROC ausnimmt, und kein Job-cgroup-pids.max deckt die Prozesszahl ab; \
                 harw als unprivilegierten Nutzer starten, für shell.exec die Sandbox-Kette \
                 mit delegierter cgroup-Wurzel nutzen, oder ShellLimits::require_rlimits = false \
                 setzen, um das Werkzeug bewusst ohne Prozessdeckel laufen zu lassen"
            ),
        }
    }
}

impl std::error::Error for ShellLimitsError {}

impl ShellLimits {
    /// Großzügige Grenzen für Hintergrund-Jobs (`job.start` in
    /// `harw-tool-job`, Nutzerentscheidung Plan R9 Teil F).
    ///
    /// # Beschreibung
    /// Builds, Paket-Restores und Testläufe brauchen mehr als ein einzelner
    /// `shell.exec`-Aufruf: **kein** Adressraum-Deckel (`RLIMIT_AS`, Linker
    /// und JIT-lastige Werkzeuge reservieren viel virtuellen Speicher) und
    /// **kein** Dateigrößen-Deckel (`RLIMIT_FSIZE`, Logs und Artefakte können
    /// groß werden) — beide [`RLIMIT_UNLIMITED`]. Offene Dateien 4096 statt
    /// 256 (parallele Compiler/Linker). Die CPU-Grenze bleibt die Vorgabe und
    /// wächst wie bei `shell.exec` mit dem Budget des Aufrufs
    /// (`exec::timeouts::limits_for`); `nproc`, tmpfs und
    /// `require_rlimits` bleiben unverändert.
    ///
    /// # Beispiele
    /// ```rust
    /// use harw_tool_shell::ShellLimits;
    ///
    /// let jobs = ShellLimits::for_jobs();
    /// assert!(jobs.validate().is_ok());
    /// assert_eq!(jobs.cpu_secs, ShellLimits::default().cpu_secs);
    /// ```
    #[must_use]
    pub fn for_jobs() -> Self {
        Self {
            as_bytes: RLIMIT_UNLIMITED,
            fsize_bytes: RLIMIT_UNLIMITED,
            nofile: 4096,
            ..Self::default()
        }
    }

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
            OsString::from(format!("--as={}", rlimit_value(self.as_bytes))),
            OsString::from(format!("--cpu={}", self.cpu_secs)),
            OsString::from(format!("--fsize={}", rlimit_value(self.fsize_bytes))),
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
    ///
    /// [`ShellLimitsError::NprocNotEnforced`], wenn `require_rlimits ==
    /// true`, `nproc` begrenzt ist und der harw-Prozess selbst von
    /// `RLIMIT_NPROC` ausgenommen ist ([`nproc_exemption`], PL-97 E2g):
    /// `prlimit --nproc` würde dann nichts deckeln.
    ///
    /// Gilt für die Wege ohne Job-Runtime: Legacy-`shell.exec` und die
    /// LaTeX-Werkzeuge. Die Sandbox-Kette und `host.sudo_exec` nehmen
    /// [`resolve_prlimit_without_nproc_identity`](Self::resolve_prlimit_without_nproc_identity).
    pub fn resolve_prlimit(&self) -> Result<Option<PathBuf>, ShellLimitsError> {
        let prlimit = self.resolve_prlimit_without_nproc_identity()?;
        self.check_nproc_identity(current_nproc_exemption())?;
        Ok(prlimit)
    }

    /// Like [`resolve_prlimit`](Self::resolve_prlimit), without the
    /// `RLIMIT_NPROC` identity check (PL-97 E2g).
    ///
    /// # Description
    /// For two callers only. The plain-bwrap route of the sandbox chain
    /// (`shell.exec`, background `job.start`): the job runtime puts its
    /// own `prlimit` in front of `bwrap` and checks the same identity
    /// fail-closed, and only it knows whether the job cgroup's `pids.max`
    /// covers `nproc`. And `host.sudo_exec`: the target runs as uid 0
    /// after `sudo`, so `RLIMIT_NPROC` never binds it whatever harw's own
    /// identity is; the caller warns about that instead.
    ///
    /// # Errors
    /// [`ShellLimitsError::PrlimitUnavailable`], wenn nicht gefunden und
    /// `require_rlimits == true`.
    pub fn resolve_prlimit_without_nproc_identity(
        &self,
    ) -> Result<Option<PathBuf>, ShellLimitsError> {
        #[cfg(test)]
        if test_seam::prlimit_hidden() {
            return self.resolve_prlimit_in(&[Path::new(test_seam::MISSING_PRLIMIT)]);
        }
        let candidates = PRLIMIT_CANDIDATES.map(Path::new);
        self.resolve_prlimit_in(&candidates)
    }

    /// Refuses limits that require their rlimits when `RLIMIT_NPROC` cannot
    /// bind this process (`exemption`, from [`nproc_exemption`]). Pure.
    ///
    /// # Errors
    /// [`ShellLimitsError::NprocNotEnforced`] for `require_rlimits`, a
    /// bounded `nproc` and an exempt identity.
    pub(crate) fn check_nproc_identity(
        &self,
        exemption: Option<&'static str>,
    ) -> Result<(), ShellLimitsError> {
        match exemption {
            Some(reason) if self.require_rlimits && self.nproc != RLIMIT_UNLIMITED => {
                Err(ShellLimitsError::NprocNotEnforced { reason })
            }
            _ => Ok(()),
        }
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

impl ShellLimits {
    /// These limits as the job runtime's resource request (PL-97 E2b, the
    /// sandbox chain of `shell.exec` and `job.start`).
    ///
    /// # Description
    /// | Limit | Request field | Enforced by |
    /// |---|---|---|
    /// | `as_bytes` | `address_space_max` (`RLIMIT_AS`, per process) | trampoline or `prlimit` |
    /// | `cpu_secs` | `cpu_time_max` (`RLIMIT_CPU`) | trampoline or `prlimit` |
    /// | `fsize_bytes` | `file_size_max` (`RLIMIT_FSIZE`) | trampoline or `prlimit` |
    /// | `nofile` | `open_files_max` (`RLIMIT_NOFILE`) | trampoline or `prlimit` |
    /// | `nproc` | `process_count_max` (`RLIMIT_NPROC`, per real UID) | trampoline or `prlimit` |
    /// | `nproc` | `pids_max` (the whole job) | the job cgroup |
    /// | `require_rlimits` | `require_rlimits` | the runtime refuses to start without rlimits |
    ///
    /// [`RLIMIT_UNLIMITED`] becomes "no limit" (`None`). `tmpfs_bytes` is
    /// not a resource of the request but the Bubblewrap `/tmp` size of the
    /// sandbox. `nproc` sets both process caps: `process_count_max` is the
    /// `RLIMIT_NPROC` the legacy `prlimit` path set (it counts every task of
    /// the real UID, see the module note), so the fork-bomb guard holds
    /// without a delegated cgroup root; `pids_max` is the exact per-job cap
    /// of the job cgroup's `pids.max`, reported as not enforced where there
    /// is no cgroup root.
    ///
    /// `memory_max` stays `None`: the legacy path never had a job-wide
    /// memory cap, only the per-process `RLIMIT_AS`. Mapping `as_bytes` to
    /// the job cgroup would OOM-kill a parallel build (several compilers of
    /// up to `as_bytes` each) that runs today.
    #[must_use]
    pub fn job_resources(&self) -> harw_command::ResourceRequest {
        let bounded = |value: u64| (value != RLIMIT_UNLIMITED).then_some(value);
        harw_command::ResourceRequest {
            address_space_max: bounded(self.as_bytes),
            cpu_time_max: Some(self.cpu_secs),
            file_size_max: bounded(self.fsize_bytes),
            open_files_max: u32::try_from(self.nofile).ok(),
            process_count_max: bounded(self.nproc),
            pids_max: u32::try_from(self.nproc).ok(),
            require_rlimits: self.require_rlimits,
            ..harw_command::ResourceRequest::default()
        }
    }
}

/// Linux capability number of `CAP_SYS_ADMIN`.
const CAP_SYS_ADMIN: u32 = 21;
/// Linux capability number of `CAP_SYS_RESOURCE`.
const CAP_SYS_RESOURCE: u32 = 24;

/// Why `RLIMIT_NPROC` would not bind a process with this
/// `/proc/<pid>/status` (PL-97 E2g), `None` when it binds. Pure.
///
/// The kernel exempts real UID 0, `CAP_SYS_ADMIN` and `CAP_SYS_RESOURCE`
/// (getrlimit(2)); a successful `prlimit --nproc` then caps nothing. A
/// missing or unparsable status counts as exempt (fail-closed). Mirrors
/// `harw_job_exec::report::ProcessIdentity` (this crate does not depend
/// on the job runtime).
pub(crate) fn nproc_exemption(status: Option<&str>) -> Option<&'static str> {
    const UNKNOWN: &str = "die Identität des harw-Prozesses ist unbekannt";
    let Some(status) = status else {
        return Some(UNKNOWN);
    };
    let field = |key: &str| {
        status.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            (name == key).then(|| value.trim())
        })
    };
    let real_uid = field("Uid")
        .and_then(|value| value.split_whitespace().next())
        .and_then(|uid| uid.parse::<u32>().ok());
    let caps = |key: &str| field(key).and_then(|value| u64::from_str_radix(value, 16).ok());
    let (Some(real_uid), Some(effective), Some(permitted)) =
        (real_uid, caps("CapEff"), caps("CapPrm"))
    else {
        return Some(UNKNOWN);
    };
    let exempting = (1_u64 << CAP_SYS_ADMIN) | (1_u64 << CAP_SYS_RESOURCE);
    if real_uid == 0 {
        Some("der harw-Prozess läuft mit realer uid 0")
    } else if (effective | permitted) & exempting != 0 {
        Some("der harw-Prozess hält CAP_SYS_ADMIN oder CAP_SYS_RESOURCE")
    } else {
        None
    }
}

/// [`nproc_exemption`] of this process as [`ShellLimits::resolve_prlimit`]
/// sees it.
///
/// In this crate's unit tests the identity is unprivileged unless a test
/// installs an exempt one ([`test_seam::ExemptIdentity`]), so the host and
/// legacy paths behave the same under any uid. The sandbox tests that run
/// the job runtime (which always probes the real identity) check
/// [`real_nproc_exemption`] themselves.
pub(crate) fn current_nproc_exemption() -> Option<&'static str> {
    #[cfg(test)]
    {
        test_seam::nproc_exemption_override().unwrap_or(None)
    }
    #[cfg(not(test))]
    {
        real_nproc_exemption()
    }
}

/// [`nproc_exemption`] of this process, read from `/proc/self/status`
/// (bounded).
pub(crate) fn real_nproc_exemption() -> Option<&'static str> {
    use std::io::Read as _;
    // Obergrenze für `/proc/self/status` (real ~1,5 KiB).
    const MAX_PROC_STATUS_BYTES: u64 = 64 * 1024;
    let mut status = String::new();
    let read = std::fs::File::open("/proc/self/status")
        .and_then(|file| file.take(MAX_PROC_STATUS_BYTES).read_to_string(&mut status));
    nproc_exemption(read.ok().map(|_| status.as_str()))
}

/// Zahlenwert für `prlimit`; [`RLIMIT_UNLIMITED`] wird zu `unlimited`
/// (prlimit(1) versteht beides, das Wort ist unabhängig von der Breite von
/// `rlim_t`).
fn rlimit_value(value: u64) -> String {
    if value == RLIMIT_UNLIMITED {
        "unlimited".to_owned()
    } else {
        value.to_string()
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
/// Host-Pfad ohne `bwrap` trägt
/// (`crate::exec::ShellExecutor::run_host_command`): dort gibt es keinen
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

/// Test seam (PL-97 E2b): a host without a fixed-path `prlimit`, for the
/// regressions that the sandbox chain picks its route before it looks for
/// `prlimit` (`shell.exec` and `job.start`). Per thread, so parallel tests
/// on other threads still see the real candidates.
#[cfg(test)]
pub(crate) mod test_seam {
    use std::cell::Cell;

    /// A candidate path that never exists.
    pub(crate) const MISSING_PRLIMIT: &str = "/nonexistent/harw-test/prlimit";

    thread_local! {
        static HIDDEN: Cell<bool> = const { Cell::new(false) };
        static NPROC_EXEMPTION: Cell<Option<Option<&'static str>>> = const { Cell::new(None) };
    }

    /// The `RLIMIT_NPROC` exemption [`super::current_nproc_exemption`]
    /// reports on this thread (PL-97 E2g): `None` (unprivileged) unless
    /// [`ExemptIdentity`] is installed.
    pub(crate) fn nproc_exemption_override() -> Option<Option<&'static str>> {
        NPROC_EXEMPTION.with(Cell::get)
    }

    /// Makes this thread's harw process look exempt from `RLIMIT_NPROC`
    /// (real uid 0 or a capability) until dropped.
    pub(crate) struct ExemptIdentity {
        previous: Option<Option<&'static str>>,
    }

    impl ExemptIdentity {
        /// From now on this thread's identity is exempt for `reason`.
        pub(crate) fn install(reason: &'static str) -> Self {
            Self {
                previous: NPROC_EXEMPTION.with(|cell| cell.replace(Some(Some(reason)))),
            }
        }
    }

    impl Drop for ExemptIdentity {
        fn drop(&mut self) {
            NPROC_EXEMPTION.with(|cell| cell.set(self.previous));
        }
    }

    /// Whether [`super::ShellLimits::resolve_prlimit`] finds nothing on
    /// this thread.
    pub(crate) fn prlimit_hidden() -> bool {
        HIDDEN.with(Cell::get)
    }

    /// Hides `prlimit` on this thread until dropped.
    pub(crate) struct HiddenPrlimit {
        previous: bool,
    }

    impl HiddenPrlimit {
        /// From now on this thread's `prlimit` lookup finds nothing.
        pub(crate) fn install() -> Self {
            Self {
                previous: HIDDEN.with(|hidden| hidden.replace(true)),
            }
        }
    }

    impl Drop for HiddenPrlimit {
        fn drop(&mut self) {
            HIDDEN.with(|hidden| hidden.set(self.previous));
        }
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

    #[test]
    fn job_resources_map_every_limit() {
        let request = ShellLimits::default().job_resources();
        // Kein jobweiter Speicherdeckel: der bisherige Weg kannte nur RLIMIT_AS.
        assert_eq!(request.memory_max, None);
        assert_eq!(request.address_space_max, Some(2 * GIB));
        assert_eq!(request.cpu_time_max, Some(60));
        assert_eq!(request.file_size_max, Some(256 * MIB));
        assert_eq!(request.open_files_max, Some(256));
        assert_eq!(request.pids_max, Some(4096));
        // RLIMIT_NPROC wie auf dem bisherigen prlimit-Weg, auch ohne cgroup.
        assert_eq!(request.process_count_max, Some(4096));
        assert!(request.requests_rlimits());
        assert!(request.require_rlimits);
        assert!(request.validate().is_ok());
        // Jobs: no address-space or file-size cap.
        let jobs = ShellLimits::for_jobs().job_resources();
        assert_eq!(jobs.memory_max, None);
        assert_eq!(jobs.address_space_max, None);
        assert_eq!(jobs.file_size_max, None);
        assert_eq!(jobs.open_files_max, Some(4096));
        assert_eq!(jobs.pids_max, Some(4096));
        assert_eq!(jobs.process_count_max, Some(4096));
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

    /// PL-97 E2g: `prlimit --nproc` caps nothing for a harw process with
    /// real uid 0, `CAP_SYS_ADMIN` or `CAP_SYS_RESOURCE`; with
    /// `require_rlimits` the plain path then refuses (it has no job cgroup
    /// whose `pids.max` could cover it). An unprivileged identity passes.
    #[test]
    fn nproc_exempt_identity_is_refused_under_require_rlimits() -> TestResult {
        let root = "Uid:\t0\t0\t0\t0\nCapPrm:\t0\nCapEff:\t0\n";
        let capable = "Uid:\t1000\t1000\t1000\t1000\nCapPrm:\t1000000\nCapEff:\t0\n";
        let admin = "Uid:\t1000\t1000\t1000\t1000\nCapPrm:\t200000\nCapEff:\t200000\n";
        let user = "Uid:\t1000\t0\t0\t0\nCapPrm:\t3000\nCapEff:\t3000\n";
        for status in [
            Some(root),
            Some(capable),
            Some(admin),
            None,
            Some("Uid:\t7\n"),
        ] {
            assert!(nproc_exemption(status).is_some(), "{status:?}");
        }
        assert_eq!(nproc_exemption(Some(user)), None);

        let limits = ShellLimits::default();
        let exempt = nproc_exemption(Some(root));
        let Err(error) = limits.check_nproc_identity(exempt) else {
            return Err(TestError::Unexpected("root must be refused".into()));
        };
        let message = error.to_string();
        assert!(message.contains("RLIMIT_NPROC"), "{message}");
        assert!(message.contains("require_rlimits"), "{message}");
        limits
            .check_nproc_identity(None)
            .map_err(ctx("unprivileged"))?;
        let optional = ShellLimits {
            require_rlimits: false,
            ..ShellLimits::default()
        };
        optional
            .check_nproc_identity(exempt)
            .map_err(ctx("not required"))?;
        let unbounded = ShellLimits {
            nproc: RLIMIT_UNLIMITED,
            ..ShellLimits::default()
        };
        unbounded
            .check_nproc_identity(exempt)
            .map_err(ctx("no nproc cap"))?;

        // Through the seam: `resolve_prlimit` itself refuses, when a
        // trusted `prlimit` exists at all.
        let _exempt = test_seam::ExemptIdentity::install("der harw-Prozess läuft mit realer uid 0");
        match limits.resolve_prlimit() {
            Err(ShellLimitsError::NprocNotEnforced { .. }) => {}
            Err(ShellLimitsError::PrlimitUnavailable { .. }) => {
                eprintln!("skipped: no trusted prlimit at the fixed paths");
            }
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        assert!(optional.resolve_prlimit().is_ok());
        // The chain route and `host.sudo_exec` leave the identity to the
        // job runtime resp. never rely on it: no `NprocNotEnforced` there.
        assert!(!matches!(
            limits.resolve_prlimit_without_nproc_identity(),
            Err(ShellLimitsError::NprocNotEnforced { .. })
        ));
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

    /// Plan R9, Teil F: Jobs laufen ohne Adressraum- und Dateigrößen-Deckel.
    #[test]
    fn prlimit_args_for_jobs_are_unlimited_for_as_and_fsize() {
        let jobs = ShellLimits::for_jobs();
        assert!(jobs.validate().is_ok());
        assert!(jobs.require_rlimits);
        assert_eq!(
            strings(&jobs.prlimit_args()),
            [
                "--as=unlimited",
                "--cpu=60",
                "--fsize=unlimited",
                "--nofile=4096",
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
