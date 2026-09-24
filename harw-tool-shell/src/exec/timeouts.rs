//! Zeitlimit-Politik für `shell.exec` (Runde 5, Teil N, Folgeauftrag).
//!
//! # Verantwortungsbereich
//! Bestimmt **vor** der Ausführung das wirksame Zeitlimit eines Aufrufs —
//! gleich für Sandbox-Pfad, Host-Pfad und Host-Mode-Anfrage, weil
//! [`super::escalation::EscalatingShellExecutor`] den Wert in die Argumente
//! schreibt, bevor irgendein Pfad sie liest:
//!
//! - `timeout_secs` des Modells wird respektiert und auf
//!   [`TimeoutPolicy::max_secs`] (Konfig `[shell] max_timeout_secs`, Vorgabe
//!   900 s) geklemmt.
//! - Ohne Angabe gilt [`TimeoutPolicy::default_secs`] (30 s); für Build-/
//!   Test-Befehle ([`is_build_command`]) [`BUILD_COMMAND_DEFAULT_TIMEOUT_SECS`]
//!   (600 s), ebenfalls gedeckelt durch die Obergrenze.
//! - Ein Zeitablauf wird mit einer klaren Handlungsanweisung versehen
//!   ([`annotate_timeout`]); die gekappte Teilausgabe bleibt erhalten.
//!
//! Die Budget-Wall-Time eines Kindes bleibt die äußere Grenze — dieses Modul
//! ändert nichts an der Budget-Durchsetzung im Controller.

use harw_tools::ToolOutput;
use serde::Deserialize;
use serde_json::Value;

/// Vorgabe der Obergrenze, solange die Runtime keine eigene setzt
/// (entspricht `harw_config::shell_limits::DEFAULT_SHELL_MAX_TIMEOUT_SECS`).
pub const DEFAULT_MAX_TIMEOUT_SECS: u64 = 900;

/// Vorgabe-Zeitlimit für Build-/Test-Befehle ohne `timeout_secs`.
pub const BUILD_COMMAND_DEFAULT_TIMEOUT_SECS: u64 = 600;

/// Programme, deren Aufruf typischerweise baut oder testet und deshalb ohne
/// Angabe das längere Vorgabe-Zeitlimit bekommt.
const BUILD_PROGRAMS: &[&str] = &[
    "cargo",
    "make",
    "gmake",
    "cmake",
    "ninja",
    "meson",
    "npm",
    "npx",
    "pnpm",
    "yarn",
    "bun",
    "deno",
    "pytest",
    "tox",
    "nox",
    "go",
    "gradle",
    "gradlew",
    "mvn",
    "mvnw",
    "bazel",
    "dotnet",
    "mix",
    "rustc",
    "latexmk",
    "just",
    "nextest",
    "cargo-nextest",
];

/// Die Zeitlimit-Politik eines `shell.exec`-Providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeoutPolicy {
    /// Vorgabe ohne `timeout_secs` für gewöhnliche Befehle.
    pub default_secs: u64,
    /// Vorgabe ohne `timeout_secs` für Build-/Test-Befehle.
    pub build_default_secs: u64,
    /// Obergrenze für jedes Zeitlimit.
    pub max_secs: u64,
}

impl TimeoutPolicy {
    /// Das wirksame Zeitlimit für `command` bei angefragtem `requested`.
    ///
    /// # Returns
    /// `requested` (falls > 0) bzw. die passende Vorgabe, jeweils höchstens
    /// [`Self::max_secs`]. `Some(0)` bleibt `0` — die Ablehnung eines
    /// Null-Zeitlimits bleibt beim Executor.
    #[must_use]
    pub fn effective(&self, command: &str, requested: Option<u64>) -> u64 {
        let max = self.max_secs.max(1);
        match requested {
            Some(0) => 0,
            Some(secs) => secs.min(max),
            None if is_build_command(command) => self.build_default_secs.min(max),
            None => self.default_secs.min(max),
        }
    }
}

/// `true`, wenn das erste Programm des Befehls ein Build-/Test-Werkzeug ist.
///
/// # Description
/// Betrachtet das erste Wort nach führenden Umgebungszuweisungen
/// (`RUST_LOG=debug cargo test`) und einem `cd … &&`-Präfix; der Pfadanteil
/// wird abgeschnitten (`/usr/bin/make` → `make`). Reine Heuristik für die
/// Vorgabe — sie erweitert nie eine Obergrenze.
#[must_use]
pub fn is_build_command(command: &str) -> bool {
    command
        .split("&&")
        .flat_map(|segment| segment.split(';'))
        .any(|segment| {
            segment
                .split_whitespace()
                .find(|word| !word.contains('='))
                .map(|word| word.rsplit('/').next().unwrap_or(word))
                .is_some_and(|program| BUILD_PROGRAMS.contains(&program))
        })
}

#[derive(Deserialize)]
struct TimeoutProbe {
    #[serde(default)]
    command: Option<String>,
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_u64"
    )]
    timeout_secs: Option<u64>,
}

/// Schreibt das wirksame Zeitlimit in die Aufrufargumente.
///
/// # Returns
/// Das eingetragene Zeitlimit, oder `None`, wenn die Argumente nicht lesbar
/// sind (dann bleiben sie unverändert und der Executor meldet den Fehler).
pub fn apply_timeout(arguments: &mut Value, policy: &TimeoutPolicy) -> Option<u64> {
    let probe: TimeoutProbe = serde_json::from_value(arguments.clone()).ok()?;
    let command = probe.command?;
    let effective = policy.effective(&command, probe.timeout_secs);
    let object = arguments.as_object_mut()?;
    object.insert("timeout_secs".to_owned(), Value::from(effective));
    Some(effective)
}

/// Versieht eine Zeitablauf-Meldung mit einer klaren Handlungsanweisung.
///
/// # Description
/// Erkennt die Timeout-Meldung des Executors (`shell.exec timed out after`,
/// auch mit `[host] `-Präfix) und stellt „Zeitlimit N s erreicht – Befehl
/// ggf. mit höherem timeout_secs (max M) erneut ausführen oder im
/// Hintergrund als Job starten“ voran. Die gekappte Teilausgabe dahinter
/// bleibt unverändert.
#[must_use]
pub fn annotate_timeout(output: ToolOutput, effective_secs: u64, max_secs: u64) -> ToolOutput {
    match output {
        ToolOutput::Error { message } if message.contains("shell.exec timed out after") => {
            ToolOutput::Error {
                message: format!("{}\n{message}", timeout_message(effective_secs, max_secs)),
            }
        }
        other => other,
    }
}

/// Die Handlungsanweisung bei Zeitablauf.
#[must_use]
pub fn timeout_message(effective_secs: u64, max_secs: u64) -> String {
    if effective_secs >= max_secs {
        format!(
            "Zeitlimit {effective_secs} s erreicht (Obergrenze [shell] max_timeout_secs = \
             {max_secs} s) – Befehl in kleinere Schritte teilen oder im Hintergrund als Job \
             starten."
        )
    } else {
        format!(
            "Zeitlimit {effective_secs} s erreicht – Befehl ggf. mit höherem timeout_secs (max \
             {max_secs}) erneut ausführen oder im Hintergrund als Job starten."
        )
    }
}

/// Passt `RLIMIT_CPU` an das gewährte Zeitlimit an.
///
/// # Description
/// Die feste Vorgabe (`ShellLimits::cpu_secs`, 60 s CPU je Prozess) würde
/// einen `rustc`-/Test-Prozess unter einem 10-Minuten-Zeitlimit nach 60 s
/// CPU-Zeit per `SIGXCPU` abbrechen. Die CPU-Grenze wächst deshalb auf
/// `Zeitlimit × verfügbare Kerne` (mehrfädige Prozesse verbrauchen CPU-Zeit
/// schneller als Wanduhrzeit); sie sinkt nie unter den konfigurierten Wert.
/// Die Wanduhr-Grenze bleibt das Zeitlimit selbst.
#[must_use]
pub fn limits_for(base: crate::ShellLimits, effective_timeout: u64) -> crate::ShellLimits {
    let cores = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1);
    let cores = u64::try_from(cores).unwrap_or(1);
    crate::ShellLimits {
        cpu_secs: base.cpu_secs.max(effective_timeout.saturating_mul(cores)),
        ..base
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const POLICY: TimeoutPolicy = TimeoutPolicy {
        default_secs: 30,
        build_default_secs: BUILD_COMMAND_DEFAULT_TIMEOUT_SECS,
        max_secs: 900,
    };

    #[test]
    fn test_requested_timeout_is_respected_and_clamped() {
        assert_eq!(POLICY.effective("sleep 1", Some(10)), 10);
        assert_eq!(POLICY.effective("sleep 1", Some(1200)), 900);
        assert_eq!(POLICY.effective("sleep 1", Some(0)), 0);
        assert_eq!(POLICY.effective("sleep 1", None), 30);
    }

    #[test]
    fn test_build_commands_get_the_longer_default_capped_by_max() {
        for command in [
            "cargo test --workspace",
            "RUST_LOG=debug cargo build",
            "cd crates/x && cargo check",
            "/usr/bin/make -j8",
            "npm test",
            "pytest -q",
            "go test ./...",
        ] {
            assert!(is_build_command(command), "{command}");
            assert_eq!(POLICY.effective(command, None), 600, "{command}");
        }
        assert!(!is_build_command("ls -la"));
        assert!(!is_build_command("echo cargo"));
        let tight = TimeoutPolicy {
            max_secs: 120,
            ..POLICY
        };
        assert_eq!(tight.effective("cargo test", None), 120);
    }

    #[test]
    fn test_apply_timeout_writes_the_effective_value_into_the_arguments() {
        let mut arguments = json!({ "command": "cargo test" });
        assert_eq!(apply_timeout(&mut arguments, &POLICY), Some(600));
        assert_eq!(arguments["timeout_secs"], 600);

        let mut lenient = json!({ "command": "echo", "timeout_secs": "5000" });
        assert_eq!(apply_timeout(&mut lenient, &POLICY), Some(900));

        let mut broken = json!({ "timeout_secs": 5 });
        assert_eq!(apply_timeout(&mut broken, &POLICY), None);
        assert!(broken.get("command").is_none());
    }

    #[test]
    fn test_cpu_limit_grows_with_the_granted_timeout_but_never_shrinks() {
        let base = crate::ShellLimits::default();
        assert!(limits_for(base, 600).cpu_secs >= 600);
        assert_eq!(limits_for(base, 1).cpu_secs, base.cpu_secs);
        assert_eq!(limits_for(base, 600).as_bytes, base.as_bytes);
    }

    #[test]
    fn test_timeout_message_names_limit_and_maximum_and_keeps_partial_output() {
        let output = annotate_timeout(
            ToolOutput::error(
                "[host] shell.exec timed out after 30s; process tree killed. Partial output \
                 (truncated: false):\n[stdout]\nhalf\n[stderr]\n",
            ),
            30,
            900,
        );
        match output {
            ToolOutput::Error { message } => {
                assert!(message.starts_with(
                    "Zeitlimit 30 s erreicht – Befehl ggf. mit höherem timeout_secs (max 900) \
                     erneut ausführen oder im Hintergrund als Job starten."
                ));
                assert!(message.contains("[stdout]\nhalf"), "{message}");
            }
            other => assert!(
                matches!(other, ToolOutput::Error { .. }),
                "expected error, got {other:?}"
            ),
        }
        assert!(timeout_message(900, 900).contains("Obergrenze"));
    }
}
