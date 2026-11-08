//! Audit-Netlink-Sonde, Push-Only — PL-100 WP1.8 (Entscheidung O11).
//!
//! # Zweck
//! Dieses Binary ist die privilegierte Quelle für Anmeldeereignisse: es tritt
//! der nur lesenden Audit-Multicast-Gruppe des Kernels bei
//! (`AUDIT_NLGRP_READLOG`, über `harw_dod_netlink::socket::NetlinkAuditSource`),
//! wandelt jeden Anmelderecord (`USER_AUTH`, `USER_LOGIN`) mit der einen Regel
//! aus `harw_dod_authlog` in ein `SecurityEvent` mit `EventKind::AuthEvent`
//! und schiebt es an `harw-sentinel` — über denselben `SOCK_SEQPACKET`-Socket
//! und dasselbe Wire-Format wie `harw-probe-bpf` (ein JSON-Dokument je
//! Nachricht, kein Längenpräfix). Der Sentinel selbst bleibt unprivilegiert
//! und linkt `harw-dod-authlog` nicht.
//!
//! # Die Fähigkeit: nur `CAP_AUDIT_READ`
//! Der Kernel erlaubt das Binden an die Lesegruppe nur mit `CAP_AUDIT_READ`.
//! Mehr braucht diese Sonde nicht: sie wird nie zum Audit-Daemon (kein
//! `CAP_AUDIT_CONTROL`, keine Audit-Regeln) und schreibt nie selbst Records
//! (kein `CAP_AUDIT_WRITE`). Klasse `harw_dod_cap::CapabilityClass::Netlink`
//! (`Capability::ReadAuditNetlink`).
//!
//! # Push-Only
//! Gegenüber dem Sentinel hat diese Sonde keinen Empfangspfad:
//! [`sink::EventSink`] hat genau eine Methode, `send`. Der Lesevorgang auf
//! dem Kernel-Strom liegt in `harw-dod-netlink`, nicht in dieser Crate.
//! `src/push_only_guard.rs` prüft das am Quelltext.
//!
//! # Reihenfolge in [`run`] und Landlock
//! 1. `--sensor-id` prüfen (nicht leer).
//! 2. Den Audit-Socket öffnen und an die Lesegruppe binden — scheitert ohne
//!    `CAP_AUDIT_READ` sofort, bevor irgendetwas anderes geschieht.
//! 3. Mit dem Sentinel verbinden — ein Pfadzugriff, der nach Landlock
//!    scheitern würde.
//! 4. Landlock: **jeder** Dateisystemzugriff wird verboten, keine Wurzel
//!    bleibt erlaubt ([`landlock`]-Moduldoku). Jeder Ausgang außer
//!    vollständiger Durchsetzung ist ein harter Startfehler, wie bei
//!    `harw-probe-bpf` und `harw-probe-fs`.
//! 5. Sammelschleife ([`collect::run_forever`]); sie endet nur über einen
//!    Fehler, `Restart=on-failure` der Unit startet die Sonde neu.
//!
//! # Das `SensorId`-Schema
//! Vorgabe `probe-netlink-auth-0` (`--sensor-id`), Muster
//! `<binary>-<teilsensor>-<instanz>` wie `probe-bpf-procmon-0`; siehe
//! [`cli`]-Moduldoku. `SecurityEvent::sensor` ist die einzige
//! Herkunftsangabe beim Sentinel; dessen Peer-Allowlist (`SO_PEERCRED`)
//! bindet sie an die uid dieser Sonde.
//!
//! # Verlust, Drossel und Fehler
//! Ein Überlauf des Kernel-Puffers, ein abgeschnittenes Datagramm, ein
//! fehlerhafter Anmelderecord und jedes Anmeldeereignis über dem Budget je
//! Fenster (eine Anmeldeflut darf die gemeinsame IPC-Inbox des Sentinels
//! nicht füllen) beenden die Sonde nicht, sondern werden gezählt und als
//! `SensorDegraded` gemeldet, höchstens einmal je Meldeabstand. Ebenso,
//! eigens gedrosselt, das Abschalten von Auditing zur Laufzeit und ein
//! Audit-Strom, der eine Stunde lang ganz schweigt ([`collect`]-Moduldoku).
//! Jeder andere
//! Lesefehler, ein verschwundener Sentinel und jeder Startfehler beenden sie
//! mit `ExitCode::FAILURE`; der Fehler steht inhaltsfrei auf `error` im Log
//! ([`error::ProbeError`]). Kommandozeilenfehler gibt `clap` selbst aus.
//!
//! # Nebenläufigkeit
//! Ein Thread: Landlock wird vor jedem weiteren Thread gesetzt, die
//! Sammelschleife läuft im Hauptthread.
//!
//! # Examples
//! ```text
//! $ harw-probe-netlink --log info --sentinel-socket /run/harw/sentinel.sock
//! ```

#![forbid(unsafe_code)]

mod cli;
mod collect;
mod error;
mod landlock;
mod sink;

#[cfg(test)]
mod push_only_guard;
// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;

use std::io::Write as _;
use std::path::Path;
use std::process::ExitCode;

use clap::Parser as _;
use harw_completions::CompletionsSubcommand;
use harw_dod_netlink::AuditSource;
use harw_types::SensorId;

use cli::{Cli, LogLevel};
use error::ProbeError;

/// Einstiegspunkt.
///
/// # Description
/// Parst die Kommandozeile ohne `clap::Parser::parse` (das selbst den
/// Prozess beenden würde), initialisiert `tracing` und delegiert an [`run`]
/// bzw. den `completions`-Unterbefehl.
///
/// # Returns
/// `ExitCode::SUCCESS` für `--help`, `--version` und einen gelungenen
/// `completions`-Lauf; sonst `ExitCode::FAILURE` — [`run`] kehrt nur über
/// einen Fehler zurück.
fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            // `clap` schreibt Hilfe und Version nach stdout, Fehler nach
            // stderr. Scheitert schon diese Ausgabe, bleibt nur der
            // Exit-Code — es gibt keinen anderen Kanal mehr.
            let _ = err.print();
            return if err.use_stderr() {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            };
        }
    };

    init_tracing(cli.log);

    // `completions` läuft vor jedem Socket- oder Landlock-Schritt.
    if let Some(CompletionsSubcommand::Completions(args)) = cli.command.as_ref() {
        return match run_completions(args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                // Kann Pfade der Shell-Konfiguration nennen: nur stderr, kein Log.
                write_stderr(&err);
                ExitCode::FAILURE
            }
        };
    }

    // `clap` erzwingt `--sentinel-socket` ohne Unterbefehl bereits
    // (`required = true`); dies ist nur die Entpackung des `Option`.
    let Some(sentinel_socket) = cli.sentinel_socket.as_deref() else {
        let err = <Cli as clap::CommandFactory>::command().error(
            clap::error::ErrorKind::MissingRequiredArgument,
            "the following required argument was not provided: --sentinel-socket <PATH>",
        );
        // Wie oben: scheitert die Ausgabe, bleibt nur der Exit-Code.
        let _ = err.print();
        return ExitCode::FAILURE;
    };

    match run(&cli, sentinel_socket) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!(error = %err, "harw-probe-netlink exiting");
            ExitCode::FAILURE
        }
    }
}

/// Initialisiert den globalen `tracing`-Subscriber auf stderr.
///
/// # Arguments
/// - `level` (`cli::LogLevel`): die über `--log` angeforderte Stufe.
fn init_tracing(level: LogLevel) {
    use tracing_subscriber::EnvFilter;

    let installed = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(level.as_filter_directive()))
        .with_target(false)
        .with_writer(std::io::stderr)
        .try_init();
    if installed.is_err() {
        // Nur möglich, wenn bereits ein Subscriber installiert ist; der
        // bestehende bleibt dann in Kraft.
        let mut stderr = std::io::stderr().lock();
        // Scheitert auch stderr, gibt es keinen Kanal mehr für den Hinweis.
        let _ = writeln!(
            stderr,
            "harw-probe-netlink: a tracing subscriber was already installed"
        );
    }
}

/// Schreibt einen Fehler auf stderr, ohne Umweg über `tracing`.
///
/// # Arguments
/// - `err` (`&ProbeError`): der auszugebende Fehler.
fn write_stderr(err: &ProbeError) {
    let mut stderr = std::io::stderr().lock();
    // Scheitert stderr, gibt es keinen Kanal mehr für die Meldung; der
    // Exit-Code trägt den Fehlschlag trotzdem.
    let _ = writeln!(stderr, "harw-probe-netlink: {err}");
}

/// Führt den `completions`-Unterbefehl aus.
///
/// # Arguments
/// - `args` (`&harw_completions::CompletionsArgs`): die geparsten
///   Unterbefehls-Argumente.
///
/// # Errors
/// [`ProbeError::Completions`], wenn Erzeugung, Installation oder das
/// Schreiben nach stdout scheitert.
fn run_completions(args: &harw_completions::CompletionsArgs) -> Result<(), ProbeError> {
    harw_completions::run_completions(
        &mut <Cli as clap::CommandFactory>::command(),
        "harw-probe-netlink",
        args,
        &harw_completions::HomeEnv::from_process(),
        &mut std::io::stdout().lock(),
    )?;
    Ok(())
}

/// Prüft `--sensor-id`.
///
/// # Arguments
/// - `raw` (`&str`): der Wert von `--sensor-id`.
///
/// # Errors
/// [`ProbeError::InvalidSensorId`] für eine leere oder nur aus Leerraum
/// bestehende Kennung.
fn parse_sensor_id(raw: &str) -> Result<SensorId, ProbeError> {
    SensorId::try_from_str(raw).map_err(|_| ProbeError::InvalidSensorId)
}

/// Öffnet den Audit-Netlink-Socket und bindet ihn an die Lesegruppe.
///
/// # Errors
/// [`ProbeError::AuditSourceUnavailable`] ohne `CAP_AUDIT_READ`, auf einem
/// Kernel ohne Audit oder wenn das Binden scheitert.
#[cfg(target_os = "linux")]
fn open_audit_source() -> Result<Box<dyn AuditSource>, ProbeError> {
    let source = harw_dod_netlink::socket::NetlinkAuditSource::open()
        .map_err(ProbeError::AuditSourceUnavailable)?;
    Ok(Box::new(source))
}

/// Außerhalb von Linux gibt es kein Audit-Netlink.
///
/// # Errors
/// Immer [`ProbeError::AuditSourceUnavailable`].
#[cfg(not(target_os = "linux"))]
fn open_audit_source() -> Result<Box<dyn AuditSource>, ProbeError> {
    Err(ProbeError::AuditSourceUnavailable(
        harw_dod_netlink::NetlinkError::from(harw_dod_cap::SensorError::SourceUnavailable),
    ))
}

/// Führt den eigentlichen Sondenbetrieb aus.
///
/// # Description
/// Reihenfolge siehe Moduldoku, „Reihenfolge in [`run`] und Landlock".
///
/// # Arguments
/// - `cli` (`&cli::Cli`): die geparste Kommandozeile.
/// - `sentinel_socket` (`&Path`): der bereits entpackte `--sentinel-socket`.
///
/// # Errors
/// [`ProbeError::InvalidSensorId`], [`ProbeError::AuditSourceUnavailable`],
/// [`ProbeError::SentinelConnectFailed`], [`ProbeError::LandlockUnavailable`]
/// beim Start; danach der Fehler, der die Sammelschleife beendet.
fn run(cli: &Cli, sentinel_socket: &Path) -> Result<(), ProbeError> {
    let sensor = parse_sensor_id(&cli.sensor_id)?;

    let source = open_audit_source()?;
    tracing::info!("joined the audit netlink read group");

    tracing::info!(path = %sentinel_socket.display(), "connecting to sentinel");
    let sink = sink::build_sentinel_sink(sentinel_socket)?;

    landlock::enforce_no_fs_access()?;

    tracing::info!(sensor_id = %sensor, "collection loop starting");
    Err(collect::run_forever(
        source.as_ref(),
        &sensor,
        sink.as_ref(),
    ))
}

#[cfg(test)]
mod tests {
    // `run` selbst wird nicht getestet: es öffnet einen echten Audit-Socket,
    // verbindet einen echten Sentinel-Socket und setzt ein echtes Landlock.
    // Die Bausteine sind einzeln geprüft (`cli`, `collect`, `sink`,
    // `landlock::handled_access`, `push_only_guard`).
    use super::parse_sensor_id;
    use crate::cli::DEFAULT_SENSOR_ID;
    use crate::error::ProbeError;
    use crate::test_support::{TestResult, ctx};

    #[test]
    fn test_parse_sensor_id_accepts_the_default() -> TestResult {
        let sensor = parse_sensor_id(DEFAULT_SENSOR_ID).map_err(ctx("Vorgabe ist gültig"))?;
        assert_eq!(sensor.as_str(), "probe-netlink-auth-0");
        Ok(())
    }

    #[test]
    fn test_parse_sensor_id_rejects_blank_ids() {
        assert!(matches!(
            parse_sensor_id(""),
            Err(ProbeError::InvalidSensorId)
        ));
        assert!(matches!(
            parse_sensor_id("   "),
            Err(ProbeError::InvalidSensorId)
        ));
    }
}
