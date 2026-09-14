//! `harw-netns-relay` — TCP-Loopback→Unix-Socket-Relay innerhalb einer bwrap-netns.
//!
//! # Aufruf
//! ```text
//! harw-netns-relay <tcp-port> <unix-socket-path> [-- <cmd> [args…]]
//! ```
//! Bindet `127.0.0.1:<tcp-port>` und leitet jede Verbindung 1:1 an den
//! Unix-Socket des Egress-Proxys weiter. Mit `--` wird nach erfolgreichem
//! Bind `<cmd>` ohne Shell gestartet (Umgebung und Standardströme geerbt); das
//! Relay endet mit dem Exit-Code des Kindes (Signal → `128 + Signal`). Ohne
//! `--` leitet es dauerhaft weiter. Semantik und Exit-Codes: Modul
//! `harw_egress::relay`.
//!
//! # Fehler
//! Meldungen gehen auf stderr (kein tracing-Subscriber in der netns):
//! Argumentfehler → 64, Bind-Fehler → 70, Kind nicht startbar → 126/127/71.
//!
//! # Nebenläufigkeit
//! Im Kindmodus läuft die Weiterleitung in einem Hintergrundthread; das Ende
//! von `main` beendet den Prozess samt aller Relay-Threads.

#![forbid(unsafe_code)]

use std::process::ExitCode;
use std::sync::Arc;
use std::thread;

use harw_egress::{
    DEFAULT_RELAY_MAX_CONNECTIONS, EXIT_CHILD_FAILED, RELAY_USAGE, RelayConfig, RelayError,
    RelayReporter, bind_relay, run_child, run_relay,
};

// Einstieg: Argumente parsen, binden, dann weiterleiten bzw. Kind abwarten.
fn main() -> ExitCode {
    let config = match RelayConfig::from_args(std::env::args_os().skip(1)) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("harw-netns-relay: {err}\n{RELAY_USAGE}");
            return ExitCode::from(err.exit_code());
        }
    };
    let listener = match bind_relay(config.port) {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("harw-netns-relay: {err}");
            return ExitCode::from(err.exit_code());
        }
    };
    let report: RelayReporter = Arc::new(|err: &RelayError| eprintln!("harw-netns-relay: {err}"));
    let RelayConfig { proxy_socket, command, .. } = config;

    let Some(command) = command else {
        match run_relay(listener, &proxy_socket, DEFAULT_RELAY_MAX_CONNECTIONS, report) {}
    };

    let forwarder = thread::Builder::new().name("harw-relay-accept".to_owned()).spawn(move || {
        run_relay(listener, &proxy_socket, DEFAULT_RELAY_MAX_CONNECTIONS, report)
    });
    if let Err(source) = forwarder {
        eprintln!("harw-netns-relay: {}", RelayError::Thread { source });
        return ExitCode::from(EXIT_CHILD_FAILED);
    }

    match run_child(&command) {
        Ok(code) => ExitCode::from(code),
        Err(err) => {
            eprintln!("harw-netns-relay: {err}");
            ExitCode::from(err.exit_code())
        }
    }
}
