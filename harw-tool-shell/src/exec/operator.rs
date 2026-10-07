//! Operator-Weg für `!`-Befehle der Nutzerin (Runde 6, Teil B).
//!
//! # Verantwortungsbereich
//! Führt einen von der Nutzerin selbst getippten Befehl (`!<cmd>` bzw.
//! `!!` in der TUI) **immer direkt auf dem Host** aus — ohne `bwrap`, ohne
//! Freigabe-Frage und ohne Permit-Ledger, weil es ihre eigenen Befehle sind.
//! Der Weg nutzt dieselben Bausteine wie der Host-Pfad der Agenten
//! (`ShellExecutor::run_host_command`):
//!
//! - geerbte Umgebung des harw-Prozesses (kein `env_clear`, echtes `HOME`),
//! - `setsid --wait /bin/sh -c <cmd>` (Trennung vom TUI-Terminal) bzw.
//!   `process_group(0)` ohne `setsid`,
//! - rlimits über festgepinntes `prlimit` ([`ShellLimits`]; `RLIMIT_CPU`
//!   wächst mit dem Zeitlimit wie in [`super::timeouts::limits_for`]),
//! - stdin `/dev/null`, gekappte Ausgabe mit gemeinsamem Byte-Budget
//!   ([`crate::capture::BoundedCapture`]),
//! - Zeitlimit mit SIGKILL des Prozessbaums, Teilausgabe bleibt erhalten.
//!
//! Rechte-Werkzeuge (`sudo`, `doas`, `pkexec`, `su`, …) in Befehlsposition
//! bleiben abgelehnt ([`crate::sudo::escalation_program`]); Root-Befehle
//! gibt es nur über das sudo-Fenster (`host.sudo_exec`).
//!
//! Jeder tatsächlich gestartete Befehl erzeugt ein Audit-Ereignis
//! `shell.operator_exec` (Ziel `harw::audit`) mit Befehl, cwd, Exit-Code und
//! Dauer; eine Ablehnung erzeugt `shell.operator_exec.denied`.
//!
//! # Nebenläufigkeit
//! [`OperatorCommand::run`] ist `async` und blockiert den aufrufenden Task
//! nicht; die TUI startet es als eigenen Tokio-Task. `kill_on_drop(true)`
//! beendet den Prozess, falls der Future fallengelassen wird. Mit
//! [`OperatorCommand::with_cancel`] bricht ein [`CancelToken`] (Ctrl+C in der
//! TUI) den Befehl ab: Der Prozessbaum wird beendet, die Teilausgabe bleibt
//! erhalten ([`OperatorEnd::Cancelled`]).

use super::{DEFAULT_MAX_OUTPUT_BYTES, ShellExecutor, timeouts};
use crate::limits::ShellLimits;
use harw_command::{
    CommandEnd, CommandOutcome, CommandPort, CommandRequest, CommandSandbox, Persistence,
    ResourceRequest,
};
use harw_types::cancel::CancelToken;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{info, warn};

/// Vorgabe-Zeitlimit eines `!`-Befehls, solange der Aufrufer keine
/// Konfiguration (`[shell] max_timeout_secs`) kennt.
pub const OPERATOR_DEFAULT_TIMEOUT_SECS: u64 = 600;

/// Wie ein `!`-Befehl endete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperatorEnd {
    /// Der Prozess lief zu Ende; `exit_code` trägt seinen Status.
    Exited,
    /// Die Ausgabe überschritt das Byte-Budget; der Prozessbaum wurde
    /// beendet (`exit_code` ist dann in der Regel `-1`).
    OutputLimit,
    /// Das Zeitlimit lief ab; der Prozessbaum wurde beendet.
    TimedOut {
        /// Das wirksame Zeitlimit in Sekunden.
        timeout_secs: u64,
    },
    /// Die Nutzerin hat den Befehl abgebrochen (Ctrl+C); der Prozessbaum
    /// wurde beendet, die Teilausgabe bleibt erhalten.
    Cancelled,
    /// Der Befehl wurde vor dem Start abgelehnt (leer oder Rechte-Werkzeug).
    Denied {
        /// Lesbare Begründung.
        message: String,
    },
    /// Der Start oder das Lesen der Ausgabe scheiterte.
    Failed {
        /// Lesbare Fehlermeldung.
        message: String,
    },
}

/// Ergebnis eines `!`-Befehls der Nutzerin.
///
/// # Beschreibung
/// `stdout`/`stderr` sind gemeinsam auf das Byte-Budget gekappt
/// (`truncated`). `executed_on_host` ist genau dann `true`, wenn der Befehl
/// tatsächlich auf dem Host gestartet wurde (nicht bei einer Ablehnung vor
/// dem Start).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorOutcome {
    /// Der ausgeführte Befehlstext (zugeschnitten).
    pub command: String,
    /// Arbeitsverzeichnis des Prozesses.
    pub cwd: PathBuf,
    /// Exit-Code; `-1`, wenn der Prozess per Signal endete oder nie lief.
    pub exit_code: i64,
    /// Gekappte Standardausgabe.
    pub stdout: String,
    /// Gekappte Fehlerausgabe.
    pub stderr: String,
    /// `true`, wenn die Ausgabe gekappt wurde.
    pub truncated: bool,
    /// `true`, wenn der Befehl auf dem Host lief (immer Host, nie `bwrap`).
    pub executed_on_host: bool,
    /// Laufzeit vom Start bis zum Ende.
    pub duration: Duration,
    /// Wie der Befehl endete.
    pub end: OperatorEnd,
}

impl OperatorOutcome {
    /// `stdout` und `stderr` zusammengeführt (leere Teile entfallen).
    #[must_use]
    pub fn combined_output(&self) -> String {
        match (self.stdout.is_empty(), self.stderr.is_empty()) {
            (true, true) => String::new(),
            (false, true) => self.stdout.clone(),
            (true, false) => self.stderr.clone(),
            (false, false) => format!("{}\n{}", self.stdout, self.stderr),
        }
    }

    /// Ergebnis ohne Start (Ablehnung oder Startfehler).
    fn not_started(command: &str, cwd: &Path, started: Instant, end: OperatorEnd) -> Self {
        Self {
            command: command.to_owned(),
            cwd: cwd.to_path_buf(),
            exit_code: -1,
            stdout: String::new(),
            stderr: String::new(),
            truncated: false,
            executed_on_host: false,
            duration: started.elapsed(),
            end,
        }
    }
}

/// Ein `!`-Befehl der Nutzerin mit allen Ausführungsparametern.
///
/// # Beschreibung
/// Baut über [`Self::new`] die Vorgaben (Standard-[`ShellLimits`],
/// [`OPERATOR_DEFAULT_TIMEOUT_SECS`], 64 KiB Ausgabe-Budget) und lässt sich
/// über die `with_*`-Methoden anpassen. [`Self::with_env`] setzt einzelne
/// Umgebungsvariablen **zusätzlich** zur geerbten Umgebung (z. B. ein
/// Test-`HOME`); die geerbte Umgebung selbst wird nie geleert.
#[derive(Debug, Clone)]
pub struct OperatorCommand {
    command: String,
    cwd: PathBuf,
    limits: ShellLimits,
    timeout: Duration,
    max_output_bytes: usize,
    env_overrides: Vec<(OsString, OsString)>,
    cancel: Option<CancelToken>,
    port: Option<Arc<dyn CommandPort>>,
}

impl OperatorCommand {
    /// Neuer Befehl mit Vorgaben; `command` wird an beiden Enden
    /// zugeschnitten.
    #[must_use]
    pub fn new(command: impl Into<String>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            command: command.into().trim().to_owned(),
            cwd: cwd.into(),
            limits: ShellLimits::default(),
            timeout: Duration::from_secs(OPERATOR_DEFAULT_TIMEOUT_SECS),
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            env_overrides: Vec::new(),
            cancel: None,
            port: None,
        }
    }

    /// Runs through this port instead of the installed one (tests, embedders).
    #[must_use]
    pub fn with_port(mut self, port: Arc<dyn CommandPort>) -> Self {
        self.port = Some(port);
        self
    }

    /// Setzt die rlimits.
    #[must_use]
    pub fn with_limits(mut self, limits: ShellLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Setzt das Zeitlimit (mindestens 1 s).
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout.max(Duration::from_secs(1));
        self
    }

    /// Setzt das gemeinsame Byte-Budget für stdout und stderr.
    #[must_use]
    pub fn with_max_output_bytes(mut self, max_output_bytes: usize) -> Self {
        self.max_output_bytes = max_output_bytes;
        self
    }

    /// Setzt eine Umgebungsvariable zusätzlich zur geerbten Umgebung.
    #[must_use]
    pub fn with_env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env_overrides.push((key.into(), value.into()));
        self
    }

    /// Setzt ein Abbruch-Token; wird es ausgelöst, endet der Befehl mit
    /// [`OperatorEnd::Cancelled`].
    #[must_use]
    pub fn with_cancel(mut self, cancel: CancelToken) -> Self {
        self.cancel = Some(cancel);
        self
    }

    /// Der (zugeschnittene) Befehlstext.
    #[must_use]
    pub fn command(&self) -> &str {
        &self.command
    }

    /// Das Arbeitsverzeichnis.
    #[must_use]
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// Führt den Befehl auf dem Host aus und liefert erst nach seinem Ende
    /// ein vollständiges Ergebnis.
    ///
    /// # Beschreibung
    /// 1. Leerer Befehl oder Rechte-Werkzeug → [`OperatorEnd::Denied`]
    ///    (kein Start).
    /// 2. rlimits prüfen, `prlimit` auflösen (fehlt es bei
    ///    `require_rlimits`, → [`OperatorEnd::Failed`]).
    /// 3. `[prlimit …] [setsid --wait] /bin/sh -c <cmd>` mit geerbter
    ///    Umgebung und `cwd` starten, Ausgabe gekappt lesen, Zeitlimit
    ///    durchsetzen.
    /// 4. Audit-Ereignis `shell.operator_exec`.
    ///
    /// # Errors
    /// Keine: jeder Fehlschlag steht in [`OperatorOutcome::end`].
    pub async fn run(&self) -> OperatorOutcome {
        let started = Instant::now();
        let command = self.command.as_str();
        if command.is_empty() {
            return self.denied(started, "Kein Befehl nach `!` angegeben.".to_owned());
        }
        if let Some(program) = crate::sudo::escalation_program(command) {
            return self.denied(started, operator_escalation_message(program));
        }

        if let Err(err) = self.limits.validate() {
            return self.failed(started, format!("rlimits ungültig: {err}"));
        }
        let Some(port) = self.port.clone().or_else(harw_command::installed) else {
            return self.failed(
                started,
                "Keine Job-Runtime montiert: `!`-Befehle laufen nur über die Job-Runtime."
                    .to_owned(),
            );
        };

        let timeout_secs = self.timeout.as_secs().max(1);
        let limits = timeouts::limits_for(self.limits, timeout_secs);
        let mut request = CommandRequest::new("/bin/sh", &self.cwd, self.timeout);
        request.args = vec!["-c".to_owned(), command.to_owned()];
        // Geerbte Umgebung bleibt vollständig (echtes `HOME`, `PATH` …);
        // nur ausdrücklich gesetzte Werte kommen hinzu. Die Umgebung wird
        // nie persistiert (`Persistence::Ephemeral`).
        request.env = std::env::vars().collect();
        for (key, value) in &self.env_overrides {
            let (Some(key), Some(value)) = (key.to_str(), value.to_str()) else {
                continue;
            };
            request.env.retain(|(name, _)| name != key);
            request.env.push((key.to_owned(), value.to_owned()));
        }
        request.max_output_bytes = self.max_output_bytes;
        request.limits = ResourceRequest {
            address_space_max: Some(limits.as_bytes),
            cpu_time_max: Some(limits.cpu_secs),
            file_size_max: Some(limits.fsize_bytes),
            open_files_max: u32::try_from(limits.nofile).ok(),
            require_rlimits: limits.require_rlimits,
            ..ResourceRequest::default()
        };
        request.sandbox = CommandSandbox::Host;
        request.persistence = Persistence::Ephemeral;

        let cancel = self.cancel.clone().unwrap_or_default();
        let done = port.run(request, cancel).await;
        let outcome = self.outcome_of(done, timeout_secs, started);
        audit(&outcome);
        outcome
    }

    /// Maps the port's outcome onto the operator vocabulary.
    fn outcome_of(
        &self,
        done: CommandOutcome,
        timeout_secs: u64,
        started: Instant,
    ) -> OperatorOutcome {
        let end = match &done.end {
            CommandEnd::Exited | CommandEnd::Signaled(_) => OperatorEnd::Exited,
            CommandEnd::TimedOut => OperatorEnd::TimedOut { timeout_secs },
            CommandEnd::OutputLimit => OperatorEnd::OutputLimit,
            CommandEnd::Cancelled => OperatorEnd::Cancelled,
            CommandEnd::Failed(message) => {
                return self.failed(
                    started,
                    format!("Start auf dem Host fehlgeschlagen: {message}"),
                );
            }
        };
        let (stdout, stderr, truncated) = ShellExecutor::truncate_combined_output(
            &done.stdout,
            &done.stderr,
            self.max_output_bytes,
        );
        let exit_code = match done.end {
            CommandEnd::Exited => i64::from(done.exit_code),
            _ => -1,
        };
        OperatorOutcome {
            command: self.command.clone(),
            cwd: self.cwd.clone(),
            exit_code,
            stdout,
            stderr,
            truncated: truncated || done.truncated || end == OperatorEnd::OutputLimit,
            executed_on_host: true,
            duration: started.elapsed(),
            end,
        }
    }

    /// Ablehnung vor dem Start, mit Audit-Ereignis.
    fn denied(&self, started: Instant, message: String) -> OperatorOutcome {
        warn!(
            target: "harw::audit",
            command = %self.command,
            cwd = %self.cwd.display(),
            reason = %message,
            "shell.operator_exec.denied"
        );
        OperatorOutcome::not_started(
            &self.command,
            &self.cwd,
            started,
            OperatorEnd::Denied { message },
        )
    }

    /// Startfehler vor bzw. beim Spawn.
    fn failed(&self, started: Instant, message: String) -> OperatorOutcome {
        warn!(
            command_len = self.command.len(),
            error = %message,
            "shell.operator_exec failed before start"
        );
        OperatorOutcome::not_started(
            &self.command,
            &self.cwd,
            started,
            OperatorEnd::Failed { message },
        )
    }
}

/// Führt einen `!`-Befehl der Nutzerin auf dem Host aus (Kurzform von
/// [`OperatorCommand`]).
///
/// # Argumente
/// - `command`: der Befehlstext (ohne führendes `!`).
/// - `cwd`: Arbeitsverzeichnis, in der TUI die Projektwurzel.
/// - `limits`: rlimits.
/// - `timeout`: Zeitlimit (mindestens 1 s).
///
/// # Rückgabe
/// Das vollständige [`OperatorOutcome`] nach dem Ende des Befehls.
pub async fn run_operator_command(
    command: &str,
    cwd: &Path,
    limits: ShellLimits,
    timeout: Duration,
) -> OperatorOutcome {
    OperatorCommand::new(command, cwd)
        .with_limits(limits)
        .with_timeout(timeout)
        .run()
        .await
}

/// Meldung, wenn ein `!`-Befehl ein Rechte-Werkzeug aufruft.
#[must_use]
pub fn operator_escalation_message(program: &str) -> String {
    format!(
        "`{program}` wird bei `!`-Befehlen nicht ausgeführt. Root-Befehle laufen nur über das \
         sudo-Fenster: den Agenten bitten, das Werkzeug `{}` zu nutzen (exaktes argv ohne \
         `{program}`, Freigabe mit Passwort im TUI-Fenster).",
        crate::sudo::SUDO_EXEC_TOOL
    )
}

/// Audit-Ereignis eines gestarteten `!`-Befehls.
fn audit(outcome: &OperatorOutcome) {
    let end = match &outcome.end {
        OperatorEnd::Exited => "exited",
        OperatorEnd::OutputLimit => "output_limit",
        OperatorEnd::TimedOut { .. } => "timed_out",
        OperatorEnd::Cancelled => "cancelled",
        OperatorEnd::Denied { .. } => "denied",
        OperatorEnd::Failed { .. } => "failed",
    };
    let duration_ms = u64::try_from(outcome.duration.as_millis()).unwrap_or(u64::MAX);
    info!(
        target: "harw::audit",
        command = %outcome.command,
        cwd = %outcome.cwd.display(),
        exit_code = outcome.exit_code,
        duration_ms,
        end,
        truncated = outcome.truncated,
        executed_on = "host",
        "shell.operator_exec"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_port() -> Arc<dyn CommandPort> {
        let dir = harw_test_support::unique_tmp("harw-operator", "jobs");
        let port = harw_command::JobCommandPort::host(&dir).expect("host job runtime");
        Arc::new(port)
    }

    fn test_command(command: impl Into<String>, cwd: impl Into<PathBuf>) -> OperatorCommand {
        OperatorCommand::new(command, cwd).with_port(test_port())
    }

    async fn run_with_test_port(
        command: &str,
        cwd: &Path,
        limits: ShellLimits,
        timeout: Duration,
    ) -> OperatorOutcome {
        test_command(command, cwd)
            .with_limits(limits)
            .with_timeout(timeout)
            .run()
            .await
    }
    use crate::test_support::{TestError, TestResult, ctx};
    use tempfile::TempDir;

    /// rlimits ohne harte `prlimit`-Pflicht, damit die Tests auch ohne
    /// util-linux laufen.
    fn test_limits() -> ShellLimits {
        ShellLimits {
            require_rlimits: false,
            ..ShellLimits::default()
        }
    }

    #[tokio::test]
    async fn operator_command_writes_into_real_home() -> TestResult {
        let home = TempDir::new().map_err(ctx("temp home"))?;
        let workspace = TempDir::new().map_err(ctx("temp workspace"))?;
        let outcome = test_command(
            "echo hallo > \"$HOME/operator-marker.txt\"",
            workspace.path(),
        )
        .with_limits(test_limits())
        .with_timeout(Duration::from_secs(30))
        .with_env("HOME", home.path())
        .run()
        .await;
        assert_eq!(outcome.end, OperatorEnd::Exited, "{outcome:?}");
        assert_eq!(outcome.exit_code, 0, "{outcome:?}");
        assert!(outcome.executed_on_host);
        let marker = home.path().join("operator-marker.txt");
        let content = std::fs::read_to_string(&marker).map_err(ctx("marker lesen"))?;
        assert_eq!(content.trim(), "hallo");
        Ok(())
    }

    #[tokio::test]
    async fn operator_command_runs_in_given_cwd() -> TestResult {
        let workspace = TempDir::new().map_err(ctx("temp workspace"))?;
        let expected = workspace
            .path()
            .canonicalize()
            .map_err(ctx("workspace kanonisieren"))?;
        let outcome =
            run_with_test_port("pwd -P", &expected, test_limits(), Duration::from_secs(30)).await;
        assert_eq!(outcome.exit_code, 0, "{outcome:?}");
        assert_eq!(Path::new(outcome.stdout.trim()), expected.as_path());
        Ok(())
    }

    #[tokio::test]
    async fn operator_command_rejects_sudo() -> TestResult {
        let workspace = TempDir::new().map_err(ctx("temp workspace"))?;
        for command in [
            "sudo ls",
            "doas ls",
            "pkexec ls",
            "echo x && sudo rm -rf /tmp/x",
        ] {
            let outcome = run_with_test_port(
                command,
                workspace.path(),
                test_limits(),
                Duration::from_secs(5),
            )
            .await;
            let OperatorEnd::Denied { message } = &outcome.end else {
                return Err(TestError::Unexpected(format!(
                    "{command} muss abgelehnt werden: {outcome:?}"
                )));
            };
            assert!(message.contains("sudo-Fenster"), "{message}");
            assert!(!outcome.executed_on_host);
            assert_eq!(outcome.exit_code, -1);
        }
        Ok(())
    }

    #[tokio::test]
    async fn operator_command_rejects_blank_command() -> TestResult {
        let workspace = TempDir::new().map_err(ctx("temp workspace"))?;
        let outcome = run_with_test_port(
            "   ",
            workspace.path(),
            test_limits(),
            Duration::from_secs(5),
        )
        .await;
        assert!(
            matches!(outcome.end, OperatorEnd::Denied { .. }),
            "{outcome:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn operator_command_times_out_and_keeps_partial_output() -> TestResult {
        let workspace = TempDir::new().map_err(ctx("temp workspace"))?;
        let started = Instant::now();
        let outcome = run_with_test_port(
            "echo vorher; sleep 30",
            workspace.path(),
            test_limits(),
            Duration::from_secs(1),
        )
        .await;
        assert_eq!(
            outcome.end,
            OperatorEnd::TimedOut { timeout_secs: 1 },
            "{outcome:?}"
        );
        assert_eq!(outcome.exit_code, -1);
        assert!(outcome.stdout.contains("vorher"), "{outcome:?}");
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "Zeitlimit muss den Prozess beenden"
        );
        Ok(())
    }

    #[tokio::test]
    async fn operator_command_cancel_stops_the_process_and_keeps_partial_output() -> TestResult {
        let workspace = TempDir::new().map_err(ctx("temp workspace"))?;
        let cancel = CancelToken::new();
        let command = test_command("echo vorher; sleep 30", workspace.path())
            .with_limits(test_limits())
            .with_timeout(Duration::from_secs(60))
            .with_cancel(cancel.clone());
        let started = Instant::now();
        let run = tokio::spawn(async move { command.run().await });
        tokio::time::sleep(Duration::from_millis(300)).await;
        cancel.cancel(harw_types::cancel::CancelReason::User);
        let outcome = run
            .await
            .map_err(|err| TestError::Unexpected(err.to_string()))?;
        assert_eq!(outcome.end, OperatorEnd::Cancelled, "{outcome:?}");
        assert_eq!(outcome.exit_code, -1);
        assert!(outcome.executed_on_host);
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "Abbruch muss den Prozess beenden"
        );
        Ok(())
    }

    #[tokio::test]
    async fn operator_command_reports_nonzero_exit_and_stderr() -> TestResult {
        let workspace = TempDir::new().map_err(ctx("temp workspace"))?;
        let outcome = run_with_test_port(
            "echo fehler >&2; exit 3",
            workspace.path(),
            test_limits(),
            Duration::from_secs(30),
        )
        .await;
        assert_eq!(outcome.end, OperatorEnd::Exited);
        assert_eq!(outcome.exit_code, 3);
        assert_eq!(outcome.stderr.trim(), "fehler");
        assert_eq!(outcome.combined_output().trim(), "fehler");
        Ok(())
    }

    #[tokio::test]
    async fn operator_command_caps_output() -> TestResult {
        let workspace = TempDir::new().map_err(ctx("temp workspace"))?;
        let outcome = test_command("yes x", workspace.path())
            .with_limits(test_limits())
            .with_timeout(Duration::from_secs(30))
            .with_max_output_bytes(1024)
            .run()
            .await;
        assert_eq!(outcome.end, OperatorEnd::OutputLimit, "{outcome:?}");
        assert!(outcome.truncated);
        assert!(outcome.stdout.len() <= 1024);
        Ok(())
    }

    #[test]
    fn operator_command_trims_command_text() {
        let command = test_command("  ls -la \n", "/");
        assert_eq!(command.command(), "ls -la");
    }
}
