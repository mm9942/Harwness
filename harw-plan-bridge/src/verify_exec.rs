//! Ausführung von Verifikationsschritten — aus deklarierten Prüfungen wird
//! zitierfähige Evidenz.
//!
//! # Verantwortungsbereich
//! `harw_plan::VerificationStep` (an jedem [`harw_plan::Criterion`]) und
//! `harw_agent_dsl::ir_v2::Verification::commands` beschreiben, *wie* ein
//! Ziel nachgewiesen wird. Bis hierher hat sie niemand ausgeführt; Ziele
//! wurden allein an angehängter Evidenz gemessen. [`VerificationExecutor`]
//! schließt die Lücke: **ein** Verifikationslauf pro Aufruf über eine Liste
//! von Schritten (zentral, auf Wellen-Ebene), sequentiell, optional mit
//! frühem Abbruch ([`StopPolicy`]).
//!
//! | Schritt                       | Ergebnis                                                     |
//! |-------------------------------|--------------------------------------------------------------|
//! | `Command { cmd, expect_exit }`| im Sandbox-Runner ausgeführt, Exit-Code verglichen            |
//! | `Artifact { path }`           | Existenz + Inhalts-Digest unter der Workspace-Wurzel          |
//! | `TraceEvent` / `Manual`       | immer [`VerifyOutcome::Unverifiable`] (Trace/Mensch nötig)    |
//!
//! # Fail closed — nie „bestanden" ohne Beleg
//! - Befehle laufen **nie** unsandboxed auf dem Host. Die Ausführung liegt
//!   hinter dem Trait [`VerifyRunner`]; die Produktions-Implementierung
//!   [`CoordinatorVerifyRunner`] reicht jeden Befehl als `JobSpec` mit
//!   [`SandboxProfileName::WorkspaceBuild`] und
//!   [`SandboxRequirement::Required`] an den Job-Coordinator
//!   (`harw-job-runtime`) — der verweigert den Start, wenn der Sandbox-
//!   Backend nicht jede Dimension durchsetzt. Ohne Backend liefert
//!   [`NoSandboxRunner`] bzw. der Coordinator
//!   [`RunnerError::NoSandbox`] → [`VerifyOutcome::Unverifiable`].
//! - Zusätzlich prüft der Executor selbst den zurückgemeldeten
//!   [`SandboxReport`]: ist er nicht vollständig `enforced`, wird das
//!   Ergebnis verworfen (Unverifiable), auch wenn der Exit-Code passt.
//! - `cmd` wird **ohne Shell** in Wörter zerlegt (einfache/doppelte
//!   Anführungszeichen, Backslash). Shell-Syntax (Pipes, Umleitungen,
//!   `$`-Expansion, Globs, `;`, `&&`, Variablenzuweisungen …) wird nicht
//!   nachgebildet, sondern als Unverifiable abgelehnt — der Befehl erreicht
//!   den Runner dann gar nicht.
//! - Ein unbekannter Exit, ein Runner-Fehler, ein leerer Lauf: Unverifiable.
//!
//! # Evidenz
//! Ein ausgeführter Befehl ergibt ein [`EvidenceRef`], dessen Art aus dem
//! Befehl abgeleitet wird ([`infer_evidence_kind`]: `cargo test`/`nextest`
//! → `CargoTest`, `cargo clippy` → `Clippy`, sonst `Other`) und dessen
//! `digest` über das Ende der Ausgabe gebildet wird (nur der Schwanz bis
//! [`VerifyConfig::output_tail_bytes`] je Strom bleibt, siehe
//! [`CommandTrace::material`] für die exakt gehashten Bytes). Artefakte
//! liefern `Other` mit dem Digest des Dateiinhalts.
//!
//! # Exportierte Typen
//! [`VerificationExecutor`], [`VerifyConfig`], [`StopPolicy`],
//! [`VerifyRunner`], [`CommandRequest`], [`CommandRun`], [`CommandExit`],
//! [`RunnerError`], [`NoSandboxRunner`], [`CoordinatorVerifyRunner`],
//! [`VerifyRun`], [`VerifyRunOutcome`], [`RunVerdict`], [`StepReport`],
//! [`VerifyOutcome`], [`StepFailure`], [`CommandTrace`],
//! [`infer_evidence_kind`], [`steps_from_ir`], [`steps_from_commands`].
//!
//! # Concurrency — eine Verifikation je Workspace, prozessübergreifend
//! [`VerificationExecutor`] ist zustandslos bis auf seinen Runner und die
//! Konfiguration; `Send + Sync`, sobald der Runner es ist. Innerhalb eines
//! Laufs sind alle Schritte strikt sequentiell. Über Läufe *und Prozesse*
//! hinweg gilt zusätzlich: höchstens eine Verifikation je Workspace zur
//! selben Zeit. Mehrere Work-Driver-Läufe oder Jobs auf demselben Workspace
//! dürfen nicht parallel `cargo` starten — das füllt `target/` mit
//! widersprüchlichen Build-Zuständen, kämpft um Cargos eigenen Lock und
//! verifiziert Zwischenzustände, die es nach dem letzten Schritt nie gab.
//!
//! Durchgesetzt wird das über eine exklusive `fs4`-Advisory-Lock-Datei unter
//! `<workspace_root>/.harw/verify.lock` (Verzeichnis und Datei werden bei
//! Bedarf angelegt). [`VerificationExecutor::run`] nimmt sie **vor** dem
//! ersten Schritt und hält sie über die gesamte Befehlsliste; eine
//! [`WorkspaceLockGuard`] gibt sie beim Drop frei (auch bei Panic/frühem
//! `return` — RAII, kein manuelles Aufräumen nötig). Das Warten ist
//! begrenzt ([`VerifyConfig::lock_timeout`], Standard 30 Minuten) und pollt
//! (`try_lock` + Sleep, siehe [`DEFAULT_LOCK_POLL_INTERVAL`]) statt
//! blockierend zu warten. Läuft der Deckel ab, bevor die Sperre frei wird,
//! liefert `run` [`VerifyRunOutcome::Busy`] — **kein** [`VerifyOutcome::Failed`]
//! und keinen Testfehlschlag: kein Schritt lief, keine Evidenz wurde
//! erzeugt oder verworfen. Ein Aufrufer (Work-Driver) soll das als „später
//! erneut versuchen" lesen, nicht als Scheitern des Ziels.
//!
//! Artefakt-Prüfungen lesen blockierend vom Dateisystem (begrenzt durch
//! [`VerifyConfig::max_artifact_bytes`]).
//!
//! # Fehler
//! Keine `Result`-Rückgabe: jeder Fehlschlag ist ein Ergebnis
//! ([`VerifyOutcome::Failed`] oder [`VerifyOutcome::Unverifiable`]) — ein
//! Verifikationslauf kann nicht „abstürzen" und dabei still als bestanden
//! gelten.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::future::Future;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use fs4::FileExt;
use harw_agent_dsl::ir_v2::Verification;
use harw_job_runtime::{
    Coordinator, CoordinatorStore, Executor, ExitOutcome, JobResult, JobSpec, JobSpecEnvelope,
    LifecycleState, ResourceRequest, RuntimeError, SandboxProfileName, SandboxReport,
    SandboxRequirement, WorkspacePath,
};
use harw_plan::{EvidenceKind, EvidenceRef, VerificationStep};
use harw_types::ContentDigest;
use jiff::SignedDuration;
use time::OffsetDateTime;

/// Standard-Zeitdeckel je Befehl.
pub const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(600);
/// Standard: so viele Bytes vom Ende jedes Ausgabestroms bleiben erhalten.
pub const DEFAULT_OUTPUT_TAIL_BYTES: usize = 64 * 1024;
/// Standard: größtes Artefakt, das gehasht wird.
pub const DEFAULT_MAX_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;
/// Standard: wie lange [`VerificationExecutor::run`] auf die
/// Workspace-Sperre wartet, bevor es [`VerifyRunOutcome::Busy`] liefert.
pub const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// Abstand zwischen zwei `try_lock`-Versuchen beim Warten auf die Sperre.
pub const DEFAULT_LOCK_POLL_INTERVAL: Duration = Duration::from_millis(200);
/// Verzeichnis unter der Workspace-Wurzel, das die Lock-Datei hält.
const LOCK_DIR_NAME: &str = ".harw";
/// Name der Advisory-Lock-Datei im [`LOCK_DIR_NAME`]-Verzeichnis.
const LOCK_FILE_NAME: &str = "verify.lock";

/// Kennung des Digest-Materials eines Befehls (Version des Formats).
const COMMAND_MATERIAL_TAG: &str = "harw-verify-command/v1";

// ---------------------------------------------------------------------------
// Runner-Vertrag
// ---------------------------------------------------------------------------

/// Ein Befehl, fertig zerlegt, für die sandboxed Ausführung.
///
/// # Description
/// `program` und `args` stammen aus der shell-freien Zerlegung von
/// `VerificationStep::Command::cmd`; `raw` ist der Originaltext (nur für
/// Diagnose und Lokator). Das Arbeitsverzeichnis ist immer die
/// Workspace-Wurzel des Runners.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRequest {
    /// Programm (vom Runner aufgelöst, nicht von einer Shell).
    pub program: String,
    /// Argumente, unverändert.
    pub args: Vec<String>,
    /// Originaltext des Schritts.
    pub raw: String,
    /// Wanduhr-Deckel; der Runner muss ihn durchsetzen.
    pub timeout: Duration,
    /// Bytes je Strom, die der Executor behält (Hinweis für den Runner).
    pub output_tail_bytes: usize,
}

/// Wie ein Befehl endete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandExit {
    /// Normales Ende mit diesem Status.
    Exited(i32),
    /// Durch ein Signal beendet.
    Signaled {
        /// Rohe Signalnummer.
        signal: i32,
    },
    /// Der Zeitdeckel ist abgelaufen.
    TimedOut,
    /// Der Status war nicht beobachtbar.
    Unknown,
}

impl fmt::Display for CommandExit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exited(code) => write!(f, "exited {code}"),
            Self::Signaled { signal } => write!(f, "signaled {signal}"),
            Self::TimedOut => f.write_str("timed_out"),
            Self::Unknown => f.write_str("unknown"),
        }
    }
}

/// Was ein Runner über einen ausgeführten Befehl meldet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRun {
    /// Wie der Befehl endete.
    pub exit: CommandExit,
    /// Erfasste Standardausgabe (der Executor behält nur das Ende).
    pub stdout: Vec<u8>,
    /// Erfasste Standardfehlerausgabe (der Executor behält nur das Ende).
    pub stderr: Vec<u8>,
    /// Der Runner hat selbst schon Ausgabe verworfen (z. B. die Mitte
    /// zwischen Kopf und Ende beim Job-Coordinator).
    pub upstream_truncated: bool,
    /// Tatsächlich durchgesetzte Sandbox — pro Dimension, nie ein `bool`.
    pub sandbox: SandboxReport,
    /// Lokator des Laufs (z. B. `job:<WorkId>`), falls der Runner einen hat.
    pub locator: Option<String>,
}

/// Warum ein Runner einen Befehl nicht (vertrauenswürdig) ausführen konnte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunnerError {
    /// Kein Sandbox-Backend verfügbar oder die Anforderung ist nicht
    /// erfüllbar. Der Befehl lief **nicht**.
    NoSandbox {
        /// Begründung.
        reason: String,
    },
    /// Jeder andere Fehler der Ausführungsschicht (Store, Spawn, Spec).
    Backend {
        /// Begründung.
        reason: String,
    },
}

impl fmt::Display for RunnerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSandbox { reason } => write!(f, "no sandbox: {reason}"),
            Self::Backend { reason } => write!(f, "runner: {reason}"),
        }
    }
}

impl std::error::Error for RunnerError {}

/// Führt einen [`CommandRequest`] sandboxed aus.
///
/// # Description
/// Die einzige Stelle, an der Verifikationsbefehle Prozesse werden.
/// Implementierungen **müssen** in einer Sandbox ausführen und
/// [`RunnerError::NoSandbox`] liefern, statt auf dem Host auszuweichen;
/// sie müssen den Zeitdeckel durchsetzen und den erreichten
/// [`SandboxReport`] ehrlich melden. Tests injizieren hier einen
/// Fake-Runner und brauchen so kein `bwrap`.
///
/// # Concurrency
/// Der Executor ruft `run` sequentiell auf; das Future muss `Send` sein,
/// damit ein Lauf auf einem Multi-Thread-Runtime gespawnt werden kann.
pub trait VerifyRunner {
    /// Führt `request` aus.
    ///
    /// # Errors
    /// [`RunnerError::NoSandbox`], wenn keine ausreichende Sandbox
    /// verfügbar ist (der Befehl lief nicht); [`RunnerError::Backend`]
    /// für alle übrigen Fehler.
    fn run(
        &self,
        request: &CommandRequest,
    ) -> impl Future<Output = Result<CommandRun, RunnerError>> + Send;
}

/// Runner für Hosts ohne Sandbox-Backend: verweigert jeden Befehl.
///
/// # Description
/// Die ehrliche Wahl für eine Composition-Root, die keinen Backend hat —
/// jeder `Command`-Schritt wird [`VerifyOutcome::Unverifiable`], nie
/// „bestanden" und nie unsandboxed ausgeführt.
#[derive(Debug, Clone, Default)]
pub struct NoSandboxRunner {
    reason: Option<String>,
}

impl NoSandboxRunner {
    /// Ein Runner, der mit `reason` ablehnt.
    #[must_use]
    pub fn with_reason(reason: impl Into<String>) -> Self {
        Self {
            reason: Some(reason.into()),
        }
    }
}

impl VerifyRunner for NoSandboxRunner {
    async fn run(&self, _request: &CommandRequest) -> Result<CommandRun, RunnerError> {
        Err(RunnerError::NoSandbox {
            reason: self
                .reason
                .clone()
                .unwrap_or_else(|| "no sandbox backend configured".to_owned()),
        })
    }
}

// ---------------------------------------------------------------------------
// Produktions-Runner über den Job-Coordinator
// ---------------------------------------------------------------------------

/// [`VerifyRunner`] über den Job-Coordinator von `harw-job-runtime`.
///
/// # Description
/// Jeder Befehl wird ein `JobSpec` (Arbeitsverzeichnis: Workspace-Wurzel
/// des Coordinators, Profil standardmäßig
/// [`SandboxProfileName::WorkspaceBuild`], **immer**
/// [`SandboxRequirement::Required`], Zeitdeckel aus dem Request, explizite
/// Umgebung aus [`Self::with_env`] — nichts wird geerbt). Damit gelten
/// cgroup-Grenzen, Deadline-Durchsetzung und der ehrliche
/// [`SandboxReport`] des Coordinators. Ein Executor ohne Sandbox-Backend
/// (`LinuxSandboxBackend::None`) lässt den Job-Körper gar nicht erst
/// laufen; das erscheint hier als [`RunnerError::NoSandbox`].
///
/// # Grenzen
/// Der Coordinator behält Anfang und Ende jedes Stroms
/// (`CoordinatorConfig::output_capture`); der Executor nimmt davon das Ende
/// als Evidence. Fehlt eine Mitte, meldet
/// [`CommandRun::upstream_truncated`] das. Die Workspace-Wurzel des Coordinators
/// sollte [`VerifyConfig::workspace_root`] entsprechen.
///
/// # Concurrency
/// Wie [`Coordinator`]: billig zu klonen, `Send + Sync`. `run` muss in
/// einem Tokio-Runtime laufen (Vorgabe von `Coordinator::submit`).
pub struct CoordinatorVerifyRunner<S, E> {
    coordinator: Coordinator<S, E>,
    profile: SandboxProfileName,
    resources: ResourceRequest,
    env: Vec<(String, String)>,
}

impl<S, E> fmt::Debug for CoordinatorVerifyRunner<S, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CoordinatorVerifyRunner")
            .field("profile", &self.profile)
            .field("resources", &self.resources)
            .field(
                "env",
                &self.env.iter().map(|(name, _)| name).collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

impl<S, E> CoordinatorVerifyRunner<S, E> {
    /// Runner über `coordinator`, Profil `WorkspaceBuild`, keine Limits
    /// außer dem Zeitdeckel, leere Umgebung.
    #[must_use]
    pub fn new(coordinator: Coordinator<S, E>) -> Self {
        Self {
            coordinator,
            profile: SandboxProfileName::WorkspaceBuild,
            resources: ResourceRequest::default(),
            env: Vec::new(),
        }
    }

    /// Setzt das Sandbox-Profil (z. B. `NoNetwork` für Offline-Prüfungen).
    #[must_use]
    pub fn with_profile(mut self, profile: SandboxProfileName) -> Self {
        self.profile = profile;
        self
    }

    /// Setzt Ressourcen-Limits (Speicher, PIDs, CPU). `wall_timeout` wird
    /// je Befehl vom Request überschrieben.
    #[must_use]
    pub fn with_resources(mut self, resources: ResourceRequest) -> Self {
        self.resources = resources;
        self
    }

    /// Fügt eine Umgebungsvariable hinzu (z. B. `PATH`, `CARGO_HOME`).
    #[must_use]
    pub fn with_env(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((name.into(), value.into()));
        self
    }

    /// Baut die Job-Hülle für `request`.
    fn envelope(&self, request: &CommandRequest) -> Result<JobSpecEnvelope, RunnerError> {
        let timeout =
            SignedDuration::try_from(request.timeout).map_err(|error| RunnerError::Backend {
                reason: format!("timeout {:?} out of range: {error}", request.timeout),
            })?;
        let mut builder = JobSpec::command(request.program.clone())
            .args(request.args.iter().cloned())
            .workspace(WorkspacePath::root())
            .resources(self.resources.clone())
            .timeout(timeout)
            .sandbox(self.profile)
            .sandbox_requirement(SandboxRequirement::Required);
        for (name, value) in &self.env {
            builder = builder.env(name.clone(), value.clone());
        }
        let spec = builder.build().map_err(|error| RunnerError::Backend {
            reason: format!("invalid job spec: {error}"),
        })?;
        JobSpecEnvelope::new(spec).map_err(|error| RunnerError::Backend {
            reason: format!("invalid job spec: {error}"),
        })
    }
}

impl<S: CoordinatorStore, E: Executor> VerifyRunner for CoordinatorVerifyRunner<S, E> {
    async fn run(&self, request: &CommandRequest) -> Result<CommandRun, RunnerError> {
        let envelope = self.envelope(request)?;
        let handle = self
            .coordinator
            .submit(envelope)
            .await
            .map_err(runner_error)?;
        let result = handle.wait().await.map_err(runner_error)?;
        command_run_from(result)
    }
}

/// Ordnet einen Coordinator-Fehler ein: alles, was die Sandbox betrifft,
/// ist `NoSandbox` (der Job-Körper lief nicht).
fn runner_error(error: RuntimeError) -> RunnerError {
    match error {
        RuntimeError::SandboxRequirementNotMet { .. }
        | RuntimeError::Sandbox { .. }
        | RuntimeError::Unsupported { .. } => RunnerError::NoSandbox {
            reason: error.to_string(),
        },
        other => RunnerError::Backend {
            reason: other.to_string(),
        },
    }
}

/// Übersetzt ein [`JobResult`] in einen [`CommandRun`].
fn command_run_from(result: JobResult) -> Result<CommandRun, RunnerError> {
    let exit = match (result.state, result.exit) {
        (LifecycleState::TimedOut, _) => CommandExit::TimedOut,
        (_, Some(ExitOutcome::Exited(code))) => CommandExit::Exited(code),
        (_, Some(ExitOutcome::Signaled { signal, .. })) => CommandExit::Signaled { signal },
        (_, Some(ExitOutcome::Unknown)) => CommandExit::Unknown,
        (state, None) => {
            // Der Job-Körper lief nicht (Start verweigert oder gescheitert).
            let reason = result
                .reason
                .clone()
                .unwrap_or_else(|| format!("job ended in state {state} without running"));
            return Err(match result.sandbox {
                Some(report) if !report.satisfies(SandboxRequirement::Required) => {
                    RunnerError::NoSandbox { reason }
                }
                _ => RunnerError::Backend { reason },
            });
        }
    };
    let Some(sandbox) = result.sandbox else {
        return Err(RunnerError::NoSandbox {
            reason: format!(
                "job {} reported no sandbox enforcement; result discarded",
                result.job_id
            ),
        });
    };
    // Der Coordinator behält Kopf + Ende jedes Stroms; als Beleg zählt das
    // Ende (Testfehler und Zusammenfassungen stehen hinten). Ohne Lücke ist
    // `*_tail` die vollständige Ausgabe.
    Ok(CommandRun {
        exit,
        stdout: result.stdout_tail().to_vec(),
        stderr: result.stderr_tail().to_vec(),
        upstream_truncated: result.output_truncated,
        sandbox,
        locator: Some(format!("job:{}", result.job_id)),
    })
}

// ---------------------------------------------------------------------------
// Konfiguration und Ergebnis
// ---------------------------------------------------------------------------

/// Wann ein Lauf vorzeitig endet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StopPolicy {
    /// Alle Schritte ausführen (Standard).
    #[default]
    RunAll,
    /// Nach dem ersten [`VerifyOutcome::Failed`] aufhören.
    OnFailure,
    /// Nach dem ersten Schritt aufhören, der nicht bestanden hat.
    OnFirstNonPass,
}

/// Konfiguration eines [`VerificationExecutor`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyConfig {
    /// Workspace-Wurzel; Artefakt-Pfade sind relativ dazu.
    pub workspace_root: PathBuf,
    /// Akteur, der in [`EvidenceRef::actor`] steht.
    pub actor: String,
    /// Zeitdeckel je Befehl.
    pub command_timeout: Duration,
    /// Bytes vom Ende jedes Ausgabestroms, die behalten und gehasht werden.
    pub output_tail_bytes: usize,
    /// Größtes Artefakt, das gehasht wird; größere sind Unverifiable.
    pub max_artifact_bytes: u64,
    /// Vorzeitiger Abbruch.
    pub stop: StopPolicy,
    /// Wie lange auf die Workspace-Sperre gewartet wird, bevor
    /// [`VerificationExecutor::run`] [`VerifyRunOutcome::Busy`] liefert.
    pub lock_timeout: Duration,
}

impl VerifyConfig {
    /// Konfiguration mit Standardwerten.
    #[must_use]
    pub fn new(workspace_root: impl Into<PathBuf>, actor: impl Into<String>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            actor: actor.into(),
            command_timeout: DEFAULT_COMMAND_TIMEOUT,
            output_tail_bytes: DEFAULT_OUTPUT_TAIL_BYTES,
            max_artifact_bytes: DEFAULT_MAX_ARTIFACT_BYTES,
            stop: StopPolicy::RunAll,
            lock_timeout: DEFAULT_LOCK_TIMEOUT,
        }
    }

    /// Setzt den Zeitdeckel je Befehl.
    #[must_use]
    pub fn with_command_timeout(mut self, timeout: Duration) -> Self {
        self.command_timeout = timeout;
        self
    }

    /// Setzt die behaltene Ausgabelänge je Strom.
    #[must_use]
    pub fn with_output_tail_bytes(mut self, bytes: usize) -> Self {
        self.output_tail_bytes = bytes;
        self
    }

    /// Setzt die größte hashbare Artefaktgröße.
    #[must_use]
    pub fn with_max_artifact_bytes(mut self, bytes: u64) -> Self {
        self.max_artifact_bytes = bytes;
        self
    }

    /// Setzt die Abbruchpolitik.
    #[must_use]
    pub fn with_stop(mut self, stop: StopPolicy) -> Self {
        self.stop = stop;
        self
    }

    /// Setzt den Zeitdeckel fürs Warten auf die Workspace-Sperre.
    #[must_use]
    pub fn with_lock_timeout(mut self, timeout: Duration) -> Self {
        self.lock_timeout = timeout;
        self
    }
}

/// Warum ein Schritt nicht bestanden hat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepFailure {
    /// Der Befehl endete mit einem anderen Exit-Code.
    ExitMismatch {
        /// Erwartet.
        expected: i32,
        /// Tatsächlich.
        actual: i32,
    },
    /// Der Befehl wurde durch ein Signal beendet.
    Signaled {
        /// Signalnummer.
        signal: i32,
    },
    /// Der Zeitdeckel ist abgelaufen.
    TimedOut {
        /// Der gesetzte Deckel.
        after: Duration,
    },
    /// Das Artefakt existiert nicht.
    ArtifactMissing {
        /// Der angefragte Pfad.
        path: String,
    },
    /// Das Artefakt ist keine reguläre Datei.
    ArtifactNotAFile {
        /// Der angefragte Pfad.
        path: String,
    },
    /// Der Pfad verlässt die Workspace-Wurzel (absolut, `..` oder Symlink).
    PathEscape {
        /// Der angefragte Pfad.
        path: String,
    },
}

impl fmt::Display for StepFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExitMismatch { expected, actual } => {
                write!(f, "exit code {actual}, expected {expected}")
            }
            Self::Signaled { signal } => write!(f, "terminated by signal {signal}"),
            Self::TimedOut { after } => write!(f, "timed out after {after:?}"),
            Self::ArtifactMissing { path } => write!(f, "artifact '{path}' does not exist"),
            Self::ArtifactNotAFile { path } => {
                write!(f, "artifact '{path}' is not a regular file")
            }
            Self::PathEscape { path } => {
                write!(f, "path '{path}' escapes the workspace root")
            }
        }
    }
}

/// Ergebnis eines einzelnen Schritts.
#[derive(Debug, Clone)]
pub enum VerifyOutcome {
    /// Nachgewiesen; die Evidenz kann an den Knoten.
    Passed {
        /// Der Nachweis.
        evidence: EvidenceRef,
    },
    /// Widerlegt. `evidence` belegt den *Fehlschlag* (z. B. die rote
    /// Testausgabe) — sie darf nicht als Erfüllungsnachweis angehängt
    /// werden.
    Failed {
        /// Warum.
        failure: StepFailure,
        /// Beleg des Fehlschlags, falls es einen gibt.
        evidence: Option<EvidenceRef>,
    },
    /// Weder nachgewiesen noch widerlegt (keine Sandbox, Shell-Syntax,
    /// Trace/Mensch nötig, Runner-Fehler …). Zählt nie als bestanden.
    Unverifiable {
        /// Warum.
        reason: String,
    },
}

impl VerifyOutcome {
    /// Ob der Schritt bestanden hat.
    #[must_use]
    pub fn is_passed(&self) -> bool {
        matches!(self, Self::Passed { .. })
    }

    fn unverifiable(reason: impl Into<String>) -> Self {
        Self::Unverifiable {
            reason: reason.into(),
        }
    }
}

/// Spur eines ausgeführten Befehls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandTrace {
    /// Wie der Befehl endete.
    pub exit: CommandExit,
    /// Ende der Standardausgabe (höchstens `output_tail_bytes`).
    pub stdout_tail: Vec<u8>,
    /// Ende der Standardfehlerausgabe (höchstens `output_tail_bytes`).
    pub stderr_tail: Vec<u8>,
    /// Ob irgendwo Ausgabe verworfen wurde (hier oder im Runner).
    pub truncated: bool,
    /// Erreichte Sandbox.
    pub sandbox: SandboxReport,
    /// Exakt die Bytes, über die [`EvidenceRef::digest`] gebildet wurde.
    pub material: Vec<u8>,
}

/// Bericht zu einem Schritt eines Laufs.
#[derive(Debug, Clone)]
pub struct StepReport {
    /// Position in der übergebenen Liste.
    pub index: usize,
    /// Der Schritt selbst.
    pub step: VerificationStep,
    /// Ergebnis.
    pub outcome: VerifyOutcome,
    /// Spur, falls ein Befehl tatsächlich lief.
    pub trace: Option<CommandTrace>,
}

/// Gesamturteil eines Laufs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunVerdict {
    /// Jeder Schritt bestand, und es gab mindestens einen.
    Passed,
    /// Mindestens ein Schritt ist gescheitert.
    Failed,
    /// Nichts gescheitert, aber nicht alles nachgewiesen (oder leer).
    Unverifiable,
}

/// Ergebnis eines Verifikationslaufs.
#[derive(Debug, Clone)]
pub struct VerifyRun {
    /// Berichte der ausgeführten Schritte, in Reihenfolge.
    pub steps: Vec<StepReport>,
    /// Zahl der übersprungenen Schritte nach einem frühen Abbruch.
    pub skipped: usize,
}

impl VerifyRun {
    /// Ob der Lauf vorzeitig endete.
    #[must_use]
    pub fn stopped_early(&self) -> bool {
        self.skipped > 0
    }

    /// Gesamturteil: ein Fehlschlag gewinnt; sonst bestanden nur, wenn
    /// jeder Schritt lief und bestand und es mindestens einen gab.
    #[must_use]
    pub fn verdict(&self) -> RunVerdict {
        if self
            .steps
            .iter()
            .any(|report| matches!(report.outcome, VerifyOutcome::Failed { .. }))
        {
            RunVerdict::Failed
        } else if !self.steps.is_empty()
            && self.skipped == 0
            && self.steps.iter().all(|report| report.outcome.is_passed())
        {
            RunVerdict::Passed
        } else {
            RunVerdict::Unverifiable
        }
    }

    /// Die Erfüllungsnachweise (nur bestandene Schritte).
    #[must_use]
    pub fn passing_evidence(&self) -> Vec<EvidenceRef> {
        self.steps
            .iter()
            .filter_map(|report| match &report.outcome {
                VerifyOutcome::Passed { evidence } => Some(evidence.clone()),
                _ => None,
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Workspace-Sperre — höchstens eine Verifikation je Workspace, prozessübergreifend
// ---------------------------------------------------------------------------

/// Ergebnis von [`VerificationExecutor::run`]: entweder ist die
/// Befehlsliste gelaufen, oder die Workspace-Sperre war innerhalb von
/// [`VerifyConfig::lock_timeout`] nicht frei.
#[derive(Debug, Clone)]
pub enum VerifyRunOutcome {
    /// Die Sperre wurde genommen, alle Schritte liefen (oder wurden per
    /// [`StopPolicy`] übersprungen).
    Completed(VerifyRun),
    /// Eine andere Verifikation hielt die Sperre über den ganzen
    /// Zeitdeckel; **kein** Schritt lief. Kein [`VerifyOutcome::Failed`] und
    /// kein Testfehlschlag — der Aufrufer soll später erneut versuchen,
    /// statt das Ziel als gescheitert zu werten.
    Busy {
        /// Pfad der umkämpften Lock-Datei.
        lock_path: PathBuf,
        /// Wie lange gewartet wurde, bevor aufgegeben wurde.
        waited: Duration,
    },
}

impl VerifyRunOutcome {
    /// Der Lauf, falls die Sperre genommen wurde.
    #[must_use]
    pub fn completed(&self) -> Option<&VerifyRun> {
        match self {
            Self::Completed(run) => Some(run),
            Self::Busy { .. } => None,
        }
    }

    /// Ob dieser Aufruf mangels freier Sperre gar nicht lief.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        matches!(self, Self::Busy { .. })
    }
}

/// Hält die exklusive `fs4`-Sperre auf `<workspace_root>/.harw/verify.lock`.
///
/// # Description
/// RAII: das Feld hält die einzige `File`-Handle, an der der OS-Lock hängt.
/// [`Drop`] gibt ihn explizit frei (`FileExt::unlock`); ein Fehler dabei
/// wird verschluckt — spätestens das Schließen der Handle löst die
/// Advisory-Lock ohnehin, es gibt keine sinnvolle Fehlerbehandlung im Drop.
#[derive(Debug)]
struct WorkspaceLockGuard {
    /// Hält den fd, an dem der OS-Lock hängt; nur für [`Drop`] gebraucht.
    file: File,
}

impl Drop for WorkspaceLockGuard {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

/// Öffnet (und legt bei Bedarf an) die Lock-Datei unter
/// `<workspace_root>/.harw/verify.lock`.
fn open_lock_file(workspace_root: &Path) -> io::Result<File> {
    let dir = workspace_root.join(LOCK_DIR_NAME);
    fs::create_dir_all(&dir)?;
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(LOCK_FILE_NAME))
}

/// Ein einzelner, nicht blockierender Versuch, die Workspace-Sperre zu
/// nehmen.
///
/// # Description
/// Von der Wartelogik in [`acquire_workspace_lock`] getrennt, damit Tests
/// die Sperr-Mechanik direkt prüfen können — ohne async-Laufzeit und ohne
/// einen echten Verifikationsbefehl laufen zu lassen.
///
/// # Returns
/// `Ok(Some(guard))`, wenn die Sperre frei war und genommen wurde;
/// `Ok(None)`, wenn sie gerade eine andere Verifikation hält.
///
/// # Errors
/// [`io::Error`] bei Problemen mit Verzeichnis/Datei (z. B. keine
/// Schreibrechte); das ist kein Lock-Konflikt und wird nicht zu `Busy`.
fn try_lock_workspace(workspace_root: &Path) -> io::Result<Option<WorkspaceLockGuard>> {
    let file = open_lock_file(workspace_root)?;
    match FileExt::try_lock(&file) {
        Ok(()) => Ok(Some(WorkspaceLockGuard { file })),
        Err(fs4::TryLockError::WouldBlock) => Ok(None),
        Err(fs4::TryLockError::Error(error)) => Err(error),
    }
}

/// Wartet begrenzt auf die Workspace-Sperre: pollt `try_lock_workspace` im
/// Abstand von `poll_interval`, bis sie frei wird oder `timeout` verstrichen
/// ist.
///
/// # Description
/// Das Polling schläft mit [`std::thread::sleep`] statt mit einem
/// Runtime-Timer: das Nehmen der Sperre ist selbst ein blockierender
/// Syscall (wie die Artefakt-Prüfungen im selben Executor), und dieses
/// Crate bindet sich bewusst an keine bestimmte Async-Laufzeit — der
/// Produktions-Runner ([`CoordinatorVerifyRunner`]) läuft ohnehin auf
/// `tokio`, aber [`VerifyRunner`] selbst verspricht das nicht.
///
/// # Returns
/// `Ok(guard)`, sobald die Sperre genommen ist; `Err(waited)` mit der
/// tatsächlich verstrichenen Wartezeit, wenn `timeout` ablief.
///
/// # Errors
/// [`io::Error`] wird wie in [`try_lock_workspace`] sofort durchgereicht
/// (kein Retry auf echte I/O-Fehler).
async fn acquire_workspace_lock(
    workspace_root: &Path,
    timeout: Duration,
    poll_interval: Duration,
) -> io::Result<Result<WorkspaceLockGuard, Duration>> {
    let start = Instant::now();
    loop {
        if let Some(guard) = try_lock_workspace(workspace_root)? {
            return Ok(Ok(guard));
        }
        let waited = start.elapsed();
        if waited >= timeout {
            return Ok(Err(waited));
        }
        std::thread::sleep(poll_interval.min(timeout - waited));
    }
}

// ---------------------------------------------------------------------------
// Executor
// ---------------------------------------------------------------------------

/// Führt Verifikationsschritte aus und macht aus ihnen Evidenz.
///
/// # Description
/// Siehe Moduldoku. `R` ist der [`VerifyRunner`]; in Produktion
/// [`CoordinatorVerifyRunner`], ohne Backend [`NoSandboxRunner`].
///
/// # Examples
/// ```rust,no_run
/// use harw_plan::VerificationStep;
/// use harw_plan_bridge::verify_exec::{
///     NoSandboxRunner, RunVerdict, VerificationExecutor, VerifyConfig, VerifyRunOutcome,
/// };
///
/// # async fn demo() {
/// let executor = VerificationExecutor::new(
///     NoSandboxRunner::default(),
///     VerifyConfig::new("/srv/workspace", "verifier"),
/// );
/// let steps = [VerificationStep::Command {
///     cmd: "cargo test -p app".to_owned(),
///     expect_exit: 0,
/// }];
/// let outcome = executor.run(&steps, time::OffsetDateTime::now_utc()).await;
/// let VerifyRunOutcome::Completed(run) = outcome else {
///     panic!("workspace lock was contended");
/// };
/// // Ohne Sandbox wird nichts ausgeführt und nichts „bestanden".
/// assert_eq!(run.verdict(), RunVerdict::Unverifiable);
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct VerificationExecutor<R> {
    runner: R,
    config: VerifyConfig,
}

impl<R: VerifyRunner> VerificationExecutor<R> {
    /// Executor über `runner` mit `config`.
    #[must_use]
    pub fn new(runner: R, config: VerifyConfig) -> Self {
        Self { runner, config }
    }

    /// Die Konfiguration.
    #[must_use]
    pub fn config(&self) -> &VerifyConfig {
        &self.config
    }

    /// Der Runner.
    #[must_use]
    pub fn runner(&self) -> &R {
        &self.runner
    }

    /// Ein Verifikationslauf über `steps`, sequentiell; `now` wird als
    /// `attached_at` jeder Evidenz eingetragen (injiziert, keine
    /// Systemzeit hier).
    ///
    /// # Description
    /// Nimmt zuerst die exklusive Workspace-Sperre (siehe Moduldoku,
    /// „Concurrency"); erst danach läuft der erste Schritt. Wartet die
    /// Sperre länger als [`VerifyConfig::lock_timeout`], liefert dieser
    /// Aufruf [`VerifyRunOutcome::Busy`] — kein Schritt lief, keine Evidenz
    /// entstand. Die Sperre wird über die gesamte Befehlsliste gehalten und
    /// vor der Rückgabe wieder freigegeben.
    pub async fn run(&self, steps: &[VerificationStep], now: OffsetDateTime) -> VerifyRunOutcome {
        let lock_path = self
            .config
            .workspace_root
            .join(LOCK_DIR_NAME)
            .join(LOCK_FILE_NAME);
        let guard = match acquire_workspace_lock(
            &self.config.workspace_root,
            self.config.lock_timeout,
            DEFAULT_LOCK_POLL_INTERVAL,
        )
        .await
        {
            Ok(Ok(guard)) => guard,
            Ok(Err(waited)) => return VerifyRunOutcome::Busy { lock_path, waited },
            Err(error) => {
                // Sperre nicht einmal versuchbar (z. B. keine Schreibrechte
                // unter der Workspace-Wurzel): fail closed wie jeder andere
                // Runner-Fehler, nie stillschweigend unsandboxed weiterlaufen.
                let reports = steps
                    .iter()
                    .enumerate()
                    .map(|(index, step)| StepReport {
                        index,
                        step: step.clone(),
                        outcome: VerifyOutcome::unverifiable(format!(
                            "workspace lock '{}' unusable: {error}",
                            lock_path.display()
                        )),
                        trace: None,
                    })
                    .collect();
                return VerifyRunOutcome::Completed(VerifyRun {
                    steps: reports,
                    skipped: 0,
                });
            }
        };
        let run = self.run_locked(steps, now).await;
        drop(guard);
        VerifyRunOutcome::Completed(run)
    }

    /// Der eigentliche sequentielle Lauf über `steps`, unter der bereits
    /// genommenen Workspace-Sperre.
    async fn run_locked(&self, steps: &[VerificationStep], now: OffsetDateTime) -> VerifyRun {
        let mut reports = Vec::with_capacity(steps.len());
        let mut skipped = 0;
        for (index, step) in steps.iter().enumerate() {
            let (outcome, trace) = self.run_step(step, now).await;
            let stop = match self.config.stop {
                StopPolicy::RunAll => false,
                StopPolicy::OnFailure => matches!(outcome, VerifyOutcome::Failed { .. }),
                StopPolicy::OnFirstNonPass => !outcome.is_passed(),
            };
            reports.push(StepReport {
                index,
                step: step.clone(),
                outcome,
                trace,
            });
            if stop {
                skipped = steps.len() - index - 1;
                break;
            }
        }
        VerifyRun {
            steps: reports,
            skipped,
        }
    }

    async fn run_step(
        &self,
        step: &VerificationStep,
        now: OffsetDateTime,
    ) -> (VerifyOutcome, Option<CommandTrace>) {
        match step {
            VerificationStep::Command { cmd, expect_exit } => {
                self.run_command(cmd, *expect_exit, now).await
            }
            VerificationStep::Artifact { path } => (self.check_artifact(path, now), None),
            VerificationStep::TraceEvent { name } => (
                VerifyOutcome::unverifiable(format!(
                    "trace event '{name}' needs a trace source; not verifiable by execution"
                )),
                None,
            ),
            VerificationStep::Manual { note } => (
                VerifyOutcome::unverifiable(format!("manual check needs a human: {note}")),
                None,
            ),
        }
    }

    async fn run_command(
        &self,
        cmd: &str,
        expect_exit: i32,
        now: OffsetDateTime,
    ) -> (VerifyOutcome, Option<CommandTrace>) {
        let words = match split_command(cmd) {
            Ok(words) => words,
            Err(reason) => {
                return (
                    VerifyOutcome::unverifiable(format!("command '{cmd}' not runnable: {reason}")),
                    None,
                );
            }
        };
        let kind = infer_from_words(&words);
        let mut words = words.into_iter();
        let Some(program) = words.next() else {
            return (VerifyOutcome::unverifiable("empty command"), None);
        };
        let request = CommandRequest {
            program,
            args: words.collect(),
            raw: cmd.to_owned(),
            timeout: self.config.command_timeout,
            output_tail_bytes: self.config.output_tail_bytes,
        };
        let run = match self.runner.run(&request).await {
            Ok(run) => run,
            Err(error) => return (VerifyOutcome::unverifiable(error.to_string()), None),
        };

        let cap = self.config.output_tail_bytes;
        let (stdout_tail, stdout_cut) = keep_tail(&run.stdout, cap);
        let (stderr_tail, stderr_cut) = keep_tail(&run.stderr, cap);
        let material = command_material(cmd, run.exit, stdout_tail, stderr_tail);
        let trace = CommandTrace {
            exit: run.exit,
            stdout_tail: stdout_tail.to_vec(),
            stderr_tail: stderr_tail.to_vec(),
            truncated: run.upstream_truncated || stdout_cut || stderr_cut,
            sandbox: run.sandbox,
            material,
        };

        // Zweite Verteidigungslinie: ein Ergebnis aus einer nicht voll
        // durchgesetzten Sandbox wird nicht geglaubt.
        if !run.sandbox.satisfies(SandboxRequirement::Required) {
            let reason = format!(
                "sandbox not fully enforced (shortfalls: {}); result discarded",
                run.sandbox.shortfalls().join(", ")
            );
            return (VerifyOutcome::unverifiable(reason), Some(trace));
        }

        let evidence = EvidenceRef {
            kind,
            locator: run
                .locator
                .clone()
                .unwrap_or_else(|| format!("verify-command:{cmd}")),
            attached_at: now,
            actor: self.config.actor.clone(),
            digest: Some(ContentDigest::of(&trace.material)),
        };
        let outcome = match run.exit {
            CommandExit::Exited(code) if code == expect_exit => VerifyOutcome::Passed { evidence },
            CommandExit::Exited(code) => VerifyOutcome::Failed {
                failure: StepFailure::ExitMismatch {
                    expected: expect_exit,
                    actual: code,
                },
                evidence: Some(evidence),
            },
            CommandExit::Signaled { signal } => VerifyOutcome::Failed {
                failure: StepFailure::Signaled { signal },
                evidence: Some(evidence),
            },
            CommandExit::TimedOut => VerifyOutcome::Failed {
                failure: StepFailure::TimedOut {
                    after: self.config.command_timeout,
                },
                evidence: Some(evidence),
            },
            CommandExit::Unknown => {
                VerifyOutcome::unverifiable(format!("exit status of '{cmd}' was not observable"))
            }
        };
        (outcome, Some(trace))
    }

    fn check_artifact(&self, raw: &str, now: OffsetDateTime) -> VerifyOutcome {
        let relative = match workspace_relative(raw) {
            Some(relative) => relative,
            None => {
                return VerifyOutcome::Failed {
                    failure: StepFailure::PathEscape {
                        path: raw.to_owned(),
                    },
                    evidence: None,
                };
            }
        };
        let root = match self.config.workspace_root.canonicalize() {
            Ok(root) => root,
            Err(error) => {
                return VerifyOutcome::unverifiable(format!(
                    "workspace root '{}' unusable: {error}",
                    self.config.workspace_root.display()
                ));
            }
        };
        let resolved = match root.join(&relative).canonicalize() {
            Ok(resolved) => resolved,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return VerifyOutcome::Failed {
                    failure: StepFailure::ArtifactMissing {
                        path: raw.to_owned(),
                    },
                    evidence: None,
                };
            }
            Err(error) => {
                return VerifyOutcome::unverifiable(format!(
                    "artifact '{raw}' not inspectable: {error}"
                ));
            }
        };
        // Symlinks werden aufgelöst; das Ziel muss unter der Wurzel liegen.
        if !resolved.starts_with(&root) {
            return VerifyOutcome::Failed {
                failure: StepFailure::PathEscape {
                    path: raw.to_owned(),
                },
                evidence: None,
            };
        }
        if !resolved.is_file() {
            return VerifyOutcome::Failed {
                failure: StepFailure::ArtifactNotAFile {
                    path: raw.to_owned(),
                },
                evidence: None,
            };
        }
        let bytes = match read_capped(&resolved, self.config.max_artifact_bytes) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => {
                return VerifyOutcome::unverifiable(format!(
                    "artifact '{raw}' exceeds {} bytes; not hashed",
                    self.config.max_artifact_bytes
                ));
            }
            Err(error) => {
                return VerifyOutcome::unverifiable(format!(
                    "artifact '{raw}' not readable: {error}"
                ));
            }
        };
        VerifyOutcome::Passed {
            evidence: EvidenceRef {
                kind: EvidenceKind::Other,
                locator: format!("workspace:{}", relative.display()),
                attached_at: now,
                actor: self.config.actor.clone(),
                digest: Some(ContentDigest::of(&bytes)),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Hilfsfunktionen
// ---------------------------------------------------------------------------

/// Übersetzt `[verification].commands` der Agent-IR in Schritte (jeweils
/// Exit-Code `0` erwartet), in Deklarationsreihenfolge.
#[must_use]
pub fn steps_from_ir(verification: &Verification) -> Vec<VerificationStep> {
    steps_from_commands(&verification.commands)
}

/// Übersetzt eine Befehlsliste (z. B. `[work_driver].verify`) in Schritte
/// mit erwartetem Exit-Code `0`, in Reihenfolge.
#[must_use]
pub fn steps_from_commands(commands: &[String]) -> Vec<VerificationStep> {
    commands
        .iter()
        .map(|cmd| VerificationStep::Command {
            cmd: cmd.clone(),
            expect_exit: 0,
        })
        .collect()
}

/// Leitet die Evidenzart aus einem Befehlstext ab: `cargo test`,
/// `cargo nextest …`, `cargo-nextest` → [`EvidenceKind::CargoTest`];
/// `cargo clippy`, `cargo-clippy` → [`EvidenceKind::Clippy`]; alles
/// andere (auch nicht zerlegbare Befehle) → [`EvidenceKind::Other`].
#[must_use]
pub fn infer_evidence_kind(cmd: &str) -> EvidenceKind {
    match split_command(cmd) {
        Ok(words) => infer_from_words(&words),
        Err(_) => EvidenceKind::Other,
    }
}

/// Globale Cargo-Optionen, die ein Wert-Argument verbrauchen.
const CARGO_VALUE_FLAGS: &[&str] = &["--manifest-path", "--config", "--color", "-Z", "-C"];

fn infer_from_words(words: &[String]) -> EvidenceKind {
    let Some((program, rest)) = words.split_first() else {
        return EvidenceKind::Other;
    };
    let name = program.rsplit('/').next().unwrap_or(program.as_str());
    match name {
        "cargo-clippy" | "clippy-driver" => return EvidenceKind::Clippy,
        "cargo-nextest" => return EvidenceKind::CargoTest,
        "cargo" => {}
        _ => return EvidenceKind::Other,
    }
    let mut skip_value = false;
    let mut subcommand = None;
    for word in rest {
        if skip_value {
            skip_value = false;
            continue;
        }
        if CARGO_VALUE_FLAGS.contains(&word.as_str()) {
            skip_value = true;
            continue;
        }
        if word.starts_with('-') || word.starts_with('+') {
            continue;
        }
        subcommand = Some(word.as_str());
        break;
    }
    match subcommand {
        Some("test" | "t" | "nextest") => EvidenceKind::CargoTest,
        Some("clippy") => EvidenceKind::Clippy,
        _ => EvidenceKind::Other,
    }
}

/// Zeichen, die außerhalb von Anführungszeichen Shell-Semantik hätten.
fn is_shell_meta(c: char) -> bool {
    matches!(
        c,
        '|' | '&' | ';' | '<' | '>' | '(' | ')' | '$' | '`' | '*' | '?' | '[' | ']' | '{' | '}'
    )
}

/// Zerlegt `cmd` shell-frei in Wörter.
///
/// Unterstützt Leerraum-Trennung, `'…'` (wörtlich), `"…"` (mit `\"`, `\\`,
/// `\$`, `` \` ``) und `\x` außerhalb von Anführungszeichen. Alles, was
/// eine Shell anders deuten würde, ist ein Fehler.
fn split_command(cmd: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut chars = cmd.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            '\n' | '\r' => return Err("multi-line commands are not supported".to_owned()),
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(inner) => current.push(inner),
                        None => return Err("unterminated single quote".to_owned()),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(escaped @ ('"' | '\\' | '$' | '`')) => current.push(escaped),
                            Some(other) => {
                                current.push('\\');
                                current.push(other);
                            }
                            None => return Err("unterminated double quote".to_owned()),
                        },
                        Some(inner @ ('$' | '`')) => {
                            return Err(format!(
                                "shell expansion '{inner}' inside double quotes is not supported"
                            ));
                        }
                        Some(inner) => current.push(inner),
                        None => return Err("unterminated double quote".to_owned()),
                    }
                }
            }
            '\\' => match chars.next() {
                Some(escaped) => {
                    in_word = true;
                    current.push(escaped);
                }
                None => return Err("trailing backslash".to_owned()),
            },
            '#' | '~' if !in_word => {
                return Err(format!("shell syntax '{c}' is not supported"));
            }
            c if is_shell_meta(c) => {
                return Err(format!("shell syntax '{c}' is not supported"));
            }
            c => {
                in_word = true;
                current.push(c);
            }
        }
    }
    if in_word {
        words.push(current);
    }
    match words.first() {
        None => Err("empty command".to_owned()),
        Some(program) if program.contains('=') => {
            Err("environment assignments are not supported".to_owned())
        }
        Some(_) => Ok(words),
    }
}

/// Die letzten `cap` Bytes von `bytes` und ob abgeschnitten wurde.
fn keep_tail(bytes: &[u8], cap: usize) -> (&[u8], bool) {
    if bytes.len() <= cap {
        (bytes, false)
    } else {
        let (_, tail) = bytes.split_at(bytes.len() - cap);
        (tail, true)
    }
}

/// Eindeutig gerahmtes Digest-Material eines Befehls.
fn command_material(cmd: &str, exit: CommandExit, stdout: &[u8], stderr: &[u8]) -> Vec<u8> {
    let mut material = Vec::with_capacity(cmd.len() + stdout.len() + stderr.len() + 96);
    material.extend_from_slice(COMMAND_MATERIAL_TAG.as_bytes());
    material.extend_from_slice(format!("\ncmd {}\n", cmd.len()).as_bytes());
    material.extend_from_slice(cmd.as_bytes());
    material.extend_from_slice(format!("\nexit {exit}\nstdout {}\n", stdout.len()).as_bytes());
    material.extend_from_slice(stdout);
    material.extend_from_slice(format!("\nstderr {}\n", stderr.len()).as_bytes());
    material.extend_from_slice(stderr);
    material
}

/// Normalisiert `raw` zu einem Pfad unter der Wurzel; `None` für leere,
/// absolute oder `..`-haltige Pfade.
fn workspace_relative(raw: &str) -> Option<PathBuf> {
    let mut relative = PathBuf::new();
    for component in Path::new(raw).components() {
        match component {
            Component::Normal(part) => relative.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if relative.as_os_str().is_empty() {
        None
    } else {
        Some(relative)
    }
}

/// Liest höchstens `max` Bytes; `Ok(None)`, wenn die Datei größer ist.
fn read_capped(path: &Path, max: u64) -> io::Result<Option<Vec<u8>>> {
    let file = File::open(path)?;
    let mut bytes = Vec::new();
    file.take(max.saturating_add(1)).read_to_end(&mut bytes)?;
    match u64::try_from(bytes.len()) {
        Ok(len) if len <= max => Ok(Some(bytes)),
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use harw_job_runtime::EnforcementState;

    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    /// Treibt ein Future ohne Async-Laufzeit; der Fake-Runner wartet nie.
    fn block_on<F: Future>(future: F) -> TestResult<F::Output> {
        use std::task::{Context, Poll, Waker};

        let mut future = std::pin::pin!(future);
        let mut cx = Context::from_waker(Waker::noop());
        for _ in 0..1_000 {
            if let Poll::Ready(value) = future.as_mut().poll(&mut cx) {
                return Ok(value);
            }
        }
        Err(TestError::Unexpected(
            "Future wurde nach 1000 Durchläufen nicht fertig".to_owned(),
        ))
    }

    /// Skriptbarer Runner: liefert vorab hinterlegte Antworten und merkt
    /// sich jede Anfrage. Ohne Antwort meldet er `NoSandbox`.
    #[derive(Default)]
    struct FakeRunner {
        responses: Mutex<VecDeque<Result<CommandRun, RunnerError>>>,
        seen: Mutex<Vec<CommandRequest>>,
    }

    impl FakeRunner {
        fn with(responses: Vec<Result<CommandRun, RunnerError>>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
                seen: Mutex::new(Vec::new()),
            }
        }

        fn seen(&self) -> TestResult<Vec<CommandRequest>> {
            self.seen
                .lock()
                .map(|seen| seen.clone())
                .map_err(ctx("Anfragen lesen"))
        }
    }

    impl VerifyRunner for FakeRunner {
        async fn run(&self, request: &CommandRequest) -> Result<CommandRun, RunnerError> {
            fn poisoned<T>(_: T) -> RunnerError {
                RunnerError::Backend {
                    reason: "fake runner lock poisoned".to_owned(),
                }
            }
            self.seen.lock().map_err(poisoned)?.push(request.clone());
            self.responses
                .lock()
                .map_err(poisoned)?
                .pop_front()
                .unwrap_or_else(|| {
                    Err(RunnerError::NoSandbox {
                        reason: "fake: no backend".to_owned(),
                    })
                })
        }
    }

    /// Ein voll sandboxed Lauf mit Exit `code`.
    fn ran(code: i32, stdout: &[u8]) -> CommandRun {
        CommandRun {
            exit: CommandExit::Exited(code),
            stdout: stdout.to_vec(),
            stderr: Vec::new(),
            upstream_truncated: false,
            sandbox: SandboxReport::uniform(EnforcementState::Enforced),
            locator: None,
        }
    }

    fn exited(code: i32, stdout: &[u8]) -> Result<CommandRun, RunnerError> {
        Ok(ran(code, stdout))
    }

    fn command(cmd: &str, expect_exit: i32) -> VerificationStep {
        VerificationStep::Command {
            cmd: cmd.to_owned(),
            expect_exit,
        }
    }

    fn executor<R: VerifyRunner>(runner: R, root: &Path) -> VerificationExecutor<R> {
        VerificationExecutor::new(runner, VerifyConfig::new(root, "verifier"))
    }

    fn now() -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH
    }

    /// Führt `exec.run(steps, now())` aus und entpackt `Completed`; in
    /// diesen Tests ist die Sperre frei, ein `Busy` wäre ein Testfehler.
    fn run_completed<R: VerifyRunner>(
        exec: &VerificationExecutor<R>,
        steps: &[VerificationStep],
    ) -> TestResult<VerifyRun> {
        match block_on(exec.run(steps, now()))? {
            VerifyRunOutcome::Completed(run) => Ok(run),
            VerifyRunOutcome::Busy { waited, lock_path } => Err(TestError::Unexpected(format!(
                "workspace lock '{}' unexpectedly busy after {waited:?}",
                lock_path.display()
            ))),
        }
    }

    fn only_outcome(run: &VerifyRun) -> TestResult<&VerifyOutcome> {
        match run.steps.as_slice() {
            [report] => Ok(&report.outcome),
            other => Err(TestError::Unexpected(format!(
                "genau ein Schritt erwartet, {} erhalten",
                other.len()
            ))),
        }
    }

    #[test]
    fn exit_match_passes_with_cargo_test_evidence() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let runner = FakeRunner::with(vec![exited(0, b"test result: ok")]);
        let exec = executor(runner, dir.path());
        let run = run_completed(&exec, &[command("cargo test -p app", 0)])?;
        assert_eq!(run.verdict(), RunVerdict::Passed);
        let VerifyOutcome::Passed { evidence } = only_outcome(&run)? else {
            return Err(TestError::Unexpected(format!("{run:?}")));
        };
        assert_eq!(evidence.kind, EvidenceKind::CargoTest);
        assert_eq!(evidence.actor, "verifier");
        assert_eq!(evidence.locator, "verify-command:cargo test -p app");
        let trace = run.steps[0]
            .trace
            .as_ref()
            .ok_or(TestError::Missing("trace"))?;
        assert_eq!(evidence.digest, Some(ContentDigest::of(&trace.material)));
        assert_eq!(run.passing_evidence().len(), 1);

        let seen = exec.runner().seen()?;
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].program, "cargo");
        assert_eq!(seen[0].args, vec!["test", "-p", "app"]);
        assert_eq!(seen[0].timeout, DEFAULT_COMMAND_TIMEOUT);
        Ok(())
    }

    #[test]
    fn exit_mismatch_fails_and_nonzero_expectation_passes() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let runner = FakeRunner::with(vec![exited(1, b"FAILED"), exited(3, b"")]);
        let exec = executor(runner, dir.path());
        let steps = [
            command("cargo clippy -- -D warnings", 0),
            command("tool --check", 3),
        ];
        let run = run_completed(&exec, &steps)?;
        assert_eq!(run.verdict(), RunVerdict::Failed);
        match &run.steps[0].outcome {
            VerifyOutcome::Failed {
                failure:
                    StepFailure::ExitMismatch {
                        expected: 0,
                        actual: 1,
                    },
                evidence: Some(evidence),
            } => assert_eq!(evidence.kind, EvidenceKind::Clippy),
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        assert!(run.steps[1].outcome.is_passed());
        // Nur der bestandene Schritt liefert Erfüllungsnachweise.
        assert_eq!(run.passing_evidence().len(), 1);
        Ok(())
    }

    #[test]
    fn timeout_fails_closed() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let runner = FakeRunner::with(vec![Ok(CommandRun {
            exit: CommandExit::TimedOut,
            ..ran(0, b"")
        })]);
        let config =
            VerifyConfig::new(dir.path(), "verifier").with_command_timeout(Duration::from_secs(5));
        let exec = VerificationExecutor::new(runner, config);
        let run = run_completed(&exec, &[command("cargo test", 0)])?;
        assert_eq!(run.verdict(), RunVerdict::Failed);
        assert!(matches!(
            only_outcome(&run)?,
            VerifyOutcome::Failed {
                failure: StepFailure::TimedOut { after },
                ..
            } if *after == Duration::from_secs(5)
        ));
        assert_eq!(exec.runner().seen()?[0].timeout, Duration::from_secs(5));
        Ok(())
    }

    #[test]
    fn output_cap_keeps_only_the_tail() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let runner = FakeRunner::with(vec![Ok(CommandRun {
            stderr: b"0123456789".to_vec(),
            ..ran(0, b"head-part-TAIL")
        })]);
        let config = VerifyConfig::new(dir.path(), "verifier").with_output_tail_bytes(4);
        let exec = VerificationExecutor::new(runner, config);
        let run = run_completed(&exec, &[command("make check", 0)])?;
        let trace = run.steps[0]
            .trace
            .as_ref()
            .ok_or(TestError::Missing("trace"))?;
        assert_eq!(trace.stdout_tail, b"TAIL");
        assert_eq!(trace.stderr_tail, b"6789");
        assert!(trace.truncated);
        assert_eq!(
            trace.material,
            command_material("make check", CommandExit::Exited(0), b"TAIL", b"6789")
        );
        let VerifyOutcome::Passed { evidence } = only_outcome(&run)? else {
            return Err(TestError::Unexpected(format!("{run:?}")));
        };
        assert_eq!(evidence.kind, EvidenceKind::Other);
        assert_eq!(evidence.digest, Some(ContentDigest::of(&trace.material)));
        Ok(())
    }

    #[test]
    fn no_sandbox_is_unverifiable() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        // Fake ohne hinterlegte Antwort meldet NoSandbox.
        let exec = executor(FakeRunner::default(), dir.path());
        let run = run_completed(&exec, &[command("cargo test", 0)])?;
        assert_eq!(run.verdict(), RunVerdict::Unverifiable);
        assert!(matches!(
            only_outcome(&run)?,
            VerifyOutcome::Unverifiable { reason } if reason.contains("no sandbox")
        ));
        assert!(run.passing_evidence().is_empty());

        let exec = executor(NoSandboxRunner::default(), dir.path());
        let run = run_completed(&exec, &[command("cargo test", 0)])?;
        assert_eq!(run.verdict(), RunVerdict::Unverifiable);
        Ok(())
    }

    #[test]
    fn weak_sandbox_report_discards_a_matching_exit() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let mut report = SandboxReport::uniform(EnforcementState::Enforced);
        report.filesystem = EnforcementState::NotEnforced;
        let runner = FakeRunner::with(vec![Ok(CommandRun {
            sandbox: report,
            ..ran(0, b"ok")
        })]);
        let exec = executor(runner, dir.path());
        let run = run_completed(&exec, &[command("cargo test", 0)])?;
        assert!(matches!(
            only_outcome(&run)?,
            VerifyOutcome::Unverifiable { reason } if reason.contains("filesystem")
        ));
        Ok(())
    }

    #[test]
    fn shell_syntax_never_reaches_the_runner() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let exec = executor(FakeRunner::with(vec![exited(0, b"")]), dir.path());
        let steps = [
            command("cargo test && rm -rf /", 0),
            command("echo $HOME", 0),
            command("cat *.rs", 0),
            command("FOO=1 cargo test", 0),
            command("echo \"$(id)\"", 0),
            command("echo 'unterminated", 0),
            command("   ", 0),
        ];
        let run = run_completed(&exec, &steps)?;
        for report in &run.steps {
            assert!(
                matches!(report.outcome, VerifyOutcome::Unverifiable { .. }),
                "{report:?}"
            );
        }
        assert!(exec.runner().seen()?.is_empty());
        Ok(())
    }

    #[test]
    fn split_command_handles_quotes_without_a_shell() -> TestResult {
        let words = split_command(r#"cargo test -p 'my crate' -- "a \"b\" \$c" x\ y"#)
            .map_err(TestError::Unexpected)?;
        assert_eq!(
            words,
            vec!["cargo", "test", "-p", "my crate", "--", "a \"b\" $c", "x y"]
        );
        Ok(())
    }

    #[test]
    fn artifact_present_passes_with_file_digest() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        std::fs::create_dir(dir.path().join("out")).map_err(ctx("mkdir"))?;
        std::fs::write(dir.path().join("out/report.json"), b"{}").map_err(ctx("write"))?;
        let exec = executor(FakeRunner::default(), dir.path());
        let steps = [VerificationStep::Artifact {
            path: "./out/report.json".to_owned(),
        }];
        let run = run_completed(&exec, &steps)?;
        let VerifyOutcome::Passed { evidence } = only_outcome(&run)? else {
            return Err(TestError::Unexpected(format!("{run:?}")));
        };
        assert_eq!(evidence.locator, "workspace:out/report.json");
        assert_eq!(evidence.digest, Some(ContentDigest::of(b"{}")));
        Ok(())
    }

    #[test]
    fn artifact_missing_directory_and_oversize() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        std::fs::create_dir(dir.path().join("sub")).map_err(ctx("mkdir"))?;
        std::fs::write(dir.path().join("big.bin"), [0_u8; 16]).map_err(ctx("write"))?;
        let config = VerifyConfig::new(dir.path(), "verifier").with_max_artifact_bytes(8);
        let exec = VerificationExecutor::new(FakeRunner::default(), config);
        let steps = [
            VerificationStep::Artifact {
                path: "nope.txt".to_owned(),
            },
            VerificationStep::Artifact {
                path: "sub".to_owned(),
            },
            VerificationStep::Artifact {
                path: "big.bin".to_owned(),
            },
        ];
        let run = run_completed(&exec, &steps)?;
        assert!(matches!(
            run.steps[0].outcome,
            VerifyOutcome::Failed {
                failure: StepFailure::ArtifactMissing { .. },
                ..
            }
        ));
        assert!(matches!(
            run.steps[1].outcome,
            VerifyOutcome::Failed {
                failure: StepFailure::ArtifactNotAFile { .. },
                ..
            }
        ));
        assert!(matches!(
            run.steps[2].outcome,
            VerifyOutcome::Unverifiable { .. }
        ));
        Ok(())
    }

    #[test]
    fn artifact_escape_attempts_are_rejected() -> TestResult {
        let outer = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let root = outer.path().join("ws");
        std::fs::create_dir(&root).map_err(ctx("mkdir"))?;
        std::fs::write(outer.path().join("secret"), b"x").map_err(ctx("write"))?;
        let exec = executor(FakeRunner::default(), &root);
        let mut steps = vec![
            VerificationStep::Artifact {
                path: "../secret".to_owned(),
            },
            VerificationStep::Artifact {
                path: "a/../../secret".to_owned(),
            },
            VerificationStep::Artifact {
                path: outer.path().join("secret").display().to_string(),
            },
            VerificationStep::Artifact {
                path: String::new(),
            },
        ];
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outer.path().join("secret"), root.join("link"))
                .map_err(ctx("symlink"))?;
            steps.push(VerificationStep::Artifact {
                path: "link".to_owned(),
            });
        }
        let run = run_completed(&exec, &steps)?;
        assert_eq!(run.steps.len(), steps.len());
        for report in &run.steps {
            assert!(
                matches!(
                    report.outcome,
                    VerifyOutcome::Failed {
                        failure: StepFailure::PathEscape { .. },
                        evidence: None,
                    }
                ),
                "{report:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn trace_and_manual_are_unverifiable() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let exec = executor(FakeRunner::default(), dir.path());
        let steps = [
            VerificationStep::TraceEvent {
                name: "plan.applied".to_owned(),
            },
            VerificationStep::Manual {
                note: "UI ansehen".to_owned(),
            },
        ];
        let run = run_completed(&exec, &steps)?;
        assert_eq!(run.verdict(), RunVerdict::Unverifiable);
        assert!(
            run.steps
                .iter()
                .all(|report| matches!(report.outcome, VerifyOutcome::Unverifiable { .. }))
        );
        assert!(exec.runner().seen()?.is_empty());
        Ok(())
    }

    #[test]
    fn stop_policies_skip_the_rest() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let steps = [
            command("cargo test", 0),
            command("cargo clippy", 0),
            command("cargo doc", 0),
        ];

        let runner = FakeRunner::with(vec![exited(0, b""), exited(1, b""), exited(0, b"")]);
        let config = VerifyConfig::new(dir.path(), "v").with_stop(StopPolicy::OnFailure);
        let exec = VerificationExecutor::new(runner, config);
        let run = run_completed(&exec, &steps)?;
        assert_eq!(run.steps.len(), 2);
        assert_eq!(run.skipped, 1);
        assert!(run.stopped_early());
        assert_eq!(run.verdict(), RunVerdict::Failed);

        // Unverifiable stoppt nur bei OnFirstNonPass; das Urteil bleibt
        // Unverifiable, auch wenn übersprungene Schritte bestanden hätten.
        let config = VerifyConfig::new(dir.path(), "v").with_stop(StopPolicy::OnFirstNonPass);
        let exec = VerificationExecutor::new(FakeRunner::default(), config);
        let run = run_completed(&exec, &steps)?;
        assert_eq!(run.steps.len(), 1);
        assert_eq!(run.skipped, 2);
        assert_eq!(run.verdict(), RunVerdict::Unverifiable);

        let exec = executor(FakeRunner::default(), dir.path());
        let run = run_completed(&exec, &steps)?;
        assert_eq!(run.steps.len(), 3);
        assert!(!run.stopped_early());
        Ok(())
    }

    #[test]
    fn empty_run_is_not_a_pass() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let exec = executor(FakeRunner::default(), dir.path());
        let run = run_completed(&exec, &[])?;
        assert_eq!(run.verdict(), RunVerdict::Unverifiable);
        Ok(())
    }

    #[test]
    fn unknown_exit_is_unverifiable() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let runner = FakeRunner::with(vec![Ok(CommandRun {
            exit: CommandExit::Unknown,
            ..ran(0, b"")
        })]);
        let exec = executor(runner, dir.path());
        let run = run_completed(&exec, &[command("cargo test", 0)])?;
        assert!(matches!(
            only_outcome(&run)?,
            VerifyOutcome::Unverifiable { .. }
        ));
        Ok(())
    }

    #[test]
    fn evidence_kind_inference() {
        let cases = [
            ("cargo test", EvidenceKind::CargoTest),
            ("cargo t -p x", EvidenceKind::CargoTest),
            ("cargo +nightly test --workspace", EvidenceKind::CargoTest),
            (
                "cargo -q --manifest-path a/Cargo.toml test",
                EvidenceKind::CargoTest,
            ),
            ("cargo nextest run", EvidenceKind::CargoTest),
            ("/usr/bin/cargo-nextest run", EvidenceKind::CargoTest),
            (
                "cargo clippy --workspace -- -D warnings",
                EvidenceKind::Clippy,
            ),
            ("cargo-clippy", EvidenceKind::Clippy),
            ("cargo build", EvidenceKind::Other),
            ("cargo", EvidenceKind::Other),
            ("make test", EvidenceKind::Other),
            ("cargo test | tee log", EvidenceKind::Other),
        ];
        for (cmd, expected) in cases {
            assert_eq!(infer_evidence_kind(cmd), expected, "{cmd}");
        }
    }

    #[test]
    fn ir_commands_become_zero_exit_steps() {
        let verification = Verification {
            profile: None,
            commands: vec!["cargo test".to_owned(), "cargo clippy".to_owned()],
        };
        let steps = steps_from_ir(&verification);
        assert_eq!(steps.len(), 2);
        assert!(matches!(
            &steps[1],
            VerificationStep::Command { cmd, expect_exit: 0 } if cmd == "cargo clippy"
        ));
    }

    #[test]
    fn coordinator_results_map_honestly() -> TestResult {
        use harw_job_runtime::coordinator::OutputCapture;
        use harw_job_runtime::{AttemptId, WorkId};

        let job_id = WorkId::try_from_str("verify-1").map_err(ctx("work id"))?;
        let attempt_id = AttemptId::new("verify-1-a1").map_err(ctx("attempt id"))?;
        let base = JobResult {
            job_id,
            attempt_id,
            state: LifecycleState::Succeeded,
            exit: Some(ExitOutcome::Exited(0)),
            cancellation: None,
            reason: None,
            sandbox: Some(SandboxReport::uniform(EnforcementState::Enforced)),
            recovered: false,
            stdout: b"ok".to_vec(),
            stdout_omitted: 0,
            stderr: Vec::new(),
            stderr_omitted: 0,
            output_truncated: false,
            output_capture: OutputCapture::default(),
        };
        let run = command_run_from(base.clone()).map_err(ctx("success"))?;
        assert_eq!(run.exit, CommandExit::Exited(0));
        assert!(!run.upstream_truncated);
        assert_eq!(run.stdout, b"ok");
        assert_eq!(run.locator.as_deref(), Some("job:verify-1"));

        // Gekürzte Ausgabe: nur das Ende (hinter der Lücke) wird Beleg.
        let cut = JobResult {
            stdout: b"headTAIL".to_vec(),
            stdout_omitted: 100,
            stderr: b"errsLAST".to_vec(),
            stderr_omitted: 7,
            output_truncated: true,
            output_capture: OutputCapture::new(4, 4),
            ..base.clone()
        };
        let run = command_run_from(cut).map_err(ctx("truncated"))?;
        assert!(run.upstream_truncated);
        assert_eq!(run.stdout, b"TAIL");
        assert_eq!(run.stderr, b"LAST");

        let timed_out = JobResult {
            state: LifecycleState::TimedOut,
            exit: Some(ExitOutcome::Signaled {
                signal: 9,
                core_dumped: false,
            }),
            ..base.clone()
        };
        assert_eq!(
            command_run_from(timed_out).map_err(ctx("timeout"))?.exit,
            CommandExit::TimedOut
        );

        // Start verweigert, Sandbox unzureichend → NoSandbox.
        let refused = JobResult {
            state: LifecycleState::Failed,
            exit: None,
            reason: Some("policy: requirement not met".to_owned()),
            sandbox: Some(SandboxReport::uniform(EnforcementState::NotEnforced)),
            ..base.clone()
        };
        assert!(matches!(
            command_run_from(refused),
            Err(RunnerError::NoSandbox { .. })
        ));

        // Lief, aber ohne Sandbox-Bericht → nicht geglaubt.
        let unreported = JobResult {
            sandbox: None,
            ..base
        };
        assert!(matches!(
            command_run_from(unreported),
            Err(RunnerError::NoSandbox { .. })
        ));
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Workspace-Sperre
    // -----------------------------------------------------------------------

    #[test]
    fn second_executor_is_busy_while_the_workspace_lock_is_held() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        // Hält die Sperre, wie es eine parallele Verifikation täte.
        let holder = try_lock_workspace(dir.path())
            .map_err(ctx("lock"))?
            .ok_or_else(|| TestError::Unexpected("lock already held".to_owned()))?;

        let config =
            VerifyConfig::new(dir.path(), "verifier").with_lock_timeout(Duration::from_millis(50));
        let exec = VerificationExecutor::new(FakeRunner::default(), config);
        let outcome = block_on(exec.run(&[command("cargo test", 0)], now()))?;
        match outcome {
            VerifyRunOutcome::Busy { waited, lock_path } => {
                assert!(waited >= Duration::from_millis(50));
                assert!(lock_path.ends_with("verify.lock"));
            }
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        // Kein Befehl lief, während die Sperre umkämpft war.
        assert!(exec.runner().seen()?.is_empty());

        drop(holder);
        Ok(())
    }

    #[test]
    fn a_different_workspace_is_not_blocked() -> TestResult {
        let held = tempfile::tempdir().map_err(ctx("tempdir held"))?;
        let free = tempfile::tempdir().map_err(ctx("tempdir free"))?;
        let holder = try_lock_workspace(held.path())
            .map_err(ctx("lock"))?
            .ok_or_else(|| TestError::Unexpected("lock already held".to_owned()))?;

        let exec = executor(FakeRunner::with(vec![exited(0, b"")]), free.path());
        let run = run_completed(&exec, &[command("cargo test", 0)])?;
        assert_eq!(run.verdict(), RunVerdict::Passed);

        drop(holder);
        Ok(())
    }

    #[test]
    fn the_lock_is_released_after_the_run_completes() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let exec = executor(FakeRunner::with(vec![exited(0, b"")]), dir.path());
        let _run = run_completed(&exec, &[command("cargo test", 0)])?;

        // Der Guard wurde am Ende von `run` gedroppt; ein frischer
        // Versuch muss die Sperre sofort bekommen.
        let guard = try_lock_workspace(dir.path()).map_err(ctx("lock"))?;
        assert!(
            guard.is_some(),
            "lock should be free after the run completed"
        );
        Ok(())
    }

    #[test]
    fn a_second_run_on_the_same_executor_reacquires_the_lock() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let exec = executor(
            FakeRunner::with(vec![exited(0, b""), exited(0, b"")]),
            dir.path(),
        );
        let first = run_completed(&exec, &[command("cargo test", 0)])?;
        assert_eq!(first.verdict(), RunVerdict::Passed);
        let second = run_completed(&exec, &[command("cargo test", 0)])?;
        assert_eq!(second.verdict(), RunVerdict::Passed);
        Ok(())
    }
}
