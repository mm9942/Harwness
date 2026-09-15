//! Durchsetzer-Binary am systemd-Socket (Knoten **AW5-04b**, Ebene **L7**).
//!
//! # Zweck
//! `harw-warden` ist das eine privilegierte Binary, das
//! [`harw_dod_warden::Warden`] (Knoten AW5-04a, `harw-dod-warden`) zu einem
//! laufenden Prozess macht: einen von systemd übergebenen
//! `SOCK_SEQPACKET`-Socket entgegennehmen, jede Anfrage unverändert an
//! `Warden::handle` weiterreichen, inhaltsfrei antworten. Alles, was
//! tatsächlich entscheidet (Beleg-Nachprüfung, Zulässigkeitsmatrix,
//! Ausführungs-Dispatch, Audit-Reihenfolge), liegt bereits in
//! `harw-dod-warden` — dieses Binary trifft **keine eigene**
//! Autorisierungsentscheidung, es verdrahtet nur Transport, Startreihenfolge
//! und die beiden produktiven Implementierungen, die die Bibliothek
//! bewusst nicht selbst mitbringt ([`audit::TracingAuditSink`],
//! [`isolation::UnimplementedNetworkIsolator`]).
//!
//! # Abhängigkeitsbudget (K53)
//! Das Plan-Dokument setzt für den gesamten Warden-Teilbaum eine Obergrenze
//! von **zwölf** transitiven Laufzeit-Abhängigkeiten (ohne Dev-, Build- und
//! Proc-Macro-Abhängigkeiten). Der Auftrag dieses Knotens nennt den
//! Ausgangsstand mit „8 von 12". Diese Crate zählt ihren eigenen Beitrag
//! nach der einzigen Methode, die ohne einen laufenden `cargo`-Aufruf
//! nachprüfbar ist (dieser Knoten darf kein `cargo` ausführen): die Anzahl
//! der `[dependencies]`-Einträge im eigenen `Cargo.toml`. Das ergibt **12**
//! direkte Einträge, in zwei Gruppen:
//!
//! 1. **Die geerbte, bereits dokumentiert gesprengte Kette** (4 Einträge):
//!    `harw-dod-warden`, `harw-dod-warden-proto`, `harw-types`,
//!    `harw-macros`. `harw-dod-warden-proto`s eigene Moduldoku
//!    (`lib.rs`, Abschnitt „Abhängigkeitsbudget") hat bereits offengelegt,
//!    dass allein ihre mandatierte Grundausstattung bei vollständiger,
//!    blattgenauer transitiver Zählung auf **65** Crates kommt — weit über
//!    zwölf. Diese vier Einträge fügen dieser bereits gesprengten
//!    Grundausstattung kein neues Blatt-Crate hinzu (dieselbe Feststellung,
//!    die `harw-dod-warden`s eigene Moduldoku bereits für sich trifft).
//! 2. **Der eigene Beitrag dieser Binary-Schicht** (8 Einträge): `serde`,
//!    `serde_json`, `tracing`, `tracing-subscriber`, `clap`, `rustix`,
//!    `landlock`, `sd-listen-fds`. Von diesen sind `serde`/`serde_json`
//!    bereits transitiv durch `harw-dod-warden-proto` vorhanden (siehe
//!    dessen `Cargo.toml`) — kein neues Blatt. `tracing`,
//!    `tracing-subscriber`, `clap`, `rustix` und `landlock` sind
//!    namens- und versionsgleich zu den bereits im selben Cargo.lock
//!    vorhandenen Einträgen aus `harw-sentinel`/`harw-probe-fs` — sie fügen
//!    dem **Workspace**-weiten `Cargo.lock` kein einziges neues,
//!    unterscheidbares Paket hinzu, auch wenn sie für die eigene,
//!    isolierte transitive Hülle dieser Crate mitzählen. Einzig
//!    `sd-listen-fds` ist eine für den gesamten Workspace neue Crate — mit
//!    laut eigener Beschreibung **keiner einzigen** weiteren Abhängigkeit
//!    (siehe `src/systemd.rs`-Moduldoku), also dem kleinstmöglichen
//!    Zuwachs für das eine Problem, das sie löst.
//!
//! **Befund, nicht Stillschweigen:** Der genannte Ausgangsstand „8 von 12"
//! lässt sich mit dieser Zählmethode nicht rekonstruieren (die geerbte
//! Kette allein zählt nach derselben Methode bereits 4, nach der
//! blattgenauen Methode der Proto-Crate bereits 65) — ein Hinweis darauf,
//! dass das eigentliche Gate (laut `harw-dod-warden-proto`s eigener
//! Dokumentation zum Zeitpunkt jenes Knotens noch nicht lauffähig) eine
//! andere, hier nicht bekannte Zählmethode verwendet. Diese Crate vermeidet
//! trotzdem jede vermeidbare zusätzliche Abhängigkeit (kein `libc`, kein
//! Mehrzweck-Socket-Framework, keine eigene Zeitzonen- oder
//! Netz-Bibliothek) und macht ihre eigene Zählung und Methode hier
//! nachprüfbar, statt eine Zahl zu behaupten, die diese Crate selbst nicht
//! herleiten kann.
//!
//! # Landlock: harter Startfehler, keine Degradation
//! Anders als `harw-sentinel` (unprivilegiert, degradiert bei fehlendem
//! Landlock) ist `harw-warden` **privilegiert**: er schreibt in
//! cgroup-v2-Kontrolldateien, um Prozessbäume einzufrieren und zu beenden.
//! Für ein privilegiertes Binary ist eine nicht durchsetzbare
//! Selbstbeschränkung ein harter Startfehler, keine akzeptable
//! Degradation — Entscheidung Nr. 4 des Plans, dieselbe Asymmetrie, die
//! `harw-probe-fs` (`CAP_SYS_ADMIN`) bereits vorexerziert. Siehe
//! [`landlock`]-Moduldoku für die vollständige Begründung, einschließlich
//! der einen Abweichung von `harw-probe-fs`: Lese- **und** Schreibzugriff
//! (dieses Binary schreibt), und ein nicht öffenbares Wurzelverzeichnis ist
//! hier ein harter Fehlschlag, kein stiller Teilerfolg (es gibt nur ein
//! Wurzelverzeichnis, nicht mehrere).
//!
//! # Warum `SO_PEERCRED`, nicht ein Token im Rumpf
//! Siehe [`ipc`]-Moduldoku, Abschnitt „Warum `SO_PEERCRED`, nicht ein Token
//! im Rumpf": `harw_dod_warden`s eigene Dokumentation benennt die
//! Vertrauensgrenze dieses Systems bereits als „ein
//! `SOCK_SEQPACKET`-Unix-Socket mit `SO_PEERCRED`, AW5-04b" — diese Crate
//! liest sie deshalb bei jeder angenommenen Verbindung, bevor irgendeine
//! Nutzlast gelesen wird, und glaubt keiner Behauptung im Nachrichtenrumpf.
//!
//! # Die `LISTEN_PID`-Prüfung
//! Systemd übergibt Sockets über geerbte Umgebungsvariablen
//! (`LISTEN_FDS`/`LISTEN_PID`), beginnend bei Deskriptor 3. Ohne eine
//! Prüfung, dass `LISTEN_PID` tatsächlich der eigenen Prozess-ID entspricht,
//! würde ein Kindprozess dieses Prozesses (der dieselben Umgebungsvariablen
//! erbt, ohne dass systemd ihm selbst je einen Deskriptor zugewiesen hätte)
//! fremde Deskriptoren für seine eigenen halten und versuchen, auf ihnen zu
//! `accept()`en — ein Deskriptor-Verwechslungsfehler, kein Sicherheitsleck
//! im engeren Sinn, aber ein Startfehler, der ohne diese Prüfung nicht als
//! solcher erkannt würde. Siehe [`systemd`]-Moduldoku für die vollständige
//! Begründung, warum diese Prüfung hier **zusätzlich** zu der bereits in
//! `sd_listen_fds::get()` vorhandenen eigenen Prüfung von Hand geschrieben
//! ist (Testbarkeit ohne `unsafe`-Umgebungsmutation, siehe dort), und für
//! den Test, der genau diese Falle absichert.
//!
//! # Kein Netz
//! Dieses Binary öffnet keinen TCP-Socket, macht keine DNS-Auflösung und
//! nimmt keine Netz-Crate auf. [`isolation::UnimplementedNetworkIsolator`]
//! baut die von der Bibliothek bewusst ausgelassene `NetworkIsolator`-
//! Implementierung nicht nach (siehe dessen Moduldoku).
//!
//! # Audit vor jedem Fehlerpfad — auch hier durchgesetzt, nicht umgangen
//! `Warden::handle` schreibt einen Audit-Eintrag vor jedem Ausführungs-
//! versuch (siehe `harw_dod_warden::warden`-Moduldoku). Dieses Binary trifft
//! **keine eigene** Entscheidung vor diesem Aufruf — [`ipc::handle_connection`]
//! reicht die empfangene Anfrage unverändert an `Warden::handle` weiter,
//! ohne selbst zu filtern, zu validieren oder vorzuentscheiden.
//!
//! # Inhaltsfreie Antworten
//! Siehe [`ipc`]-Moduldoku, Abschnitt „Inhaltsfreie Antworten": was diesen
//! Prozess über den Socket verlässt, ist ausschließlich die bereits von
//! `harw-dod-warden-proto` inhaltlich begrenzte
//! [`harw_dod_warden_proto::WardenResponse`] — nie ein Ausführungs- oder
//! Nachprüfungsfehlerdetail.
//!
//! # `--log`
//! Wie `harw-sentinel`/`harw-probe-fs`: ein ungültiger Wert ist ein
//! **Parse-Fehler**, kein stiller Rückfall (siehe [`cli::LogLevel`]).
//!
//! # Fehler
//! [`error::WardenBinError`] deckt jeden Startfehler und das endgültige
//! Ende der Annahmeschleife ab. Ein Fehler einer einzelnen Verbindung ist
//! [`ipc::IpcError`] — geloggt, die Annahmeschleife läuft weiter (siehe
//! dessen Moduldoku).
//!
//! # Examples
//! ```text
//! $ harw-warden --log info --cgroup-root /sys/fs/cgroup
//! ```
//! (Nur über einen systemd-Socket-Unit sinnvoll startbar — siehe
//! [`systemd`]-Moduldoku.)

#![forbid(unsafe_code)]

mod audit;
mod cli;
mod error;
mod ipc;
mod isolation;
mod landlock;
mod protocol;
mod systemd;
mod warden_factory;

use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser as _;

use cli::Cli;
use error::WardenBinError;

/// Einstiegspunkt.
///
/// # Description
/// Parst die Kommandozeile ohne `clap::Parser::parse` (das bei einem Fehler
/// oder `--help`/`--version` selbst `std::process::exit` aufriefe und diese
/// Funktion nie zu ihrem eigenen `ExitCode` zurückkehren ließe),
/// initialisiert `tracing` und delegiert an [`run`].
///
/// # Returns
/// `ExitCode::SUCCESS` nur bei `--help`/`--version`. Jeder andere Pfad ist
/// `ExitCode::FAILURE` — `run` selbst kehrt nie mit `Ok` zurück (siehe
/// dessen Doku): ein Durchsetzer, dessen Annahmeschleife endet, hat seine
/// eine Aufgabe nicht mehr erfüllen können.
fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            if matches!(
                err.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) {
                print!("{err}");
                return ExitCode::SUCCESS;
            }
            eprint!("{err}");
            return ExitCode::FAILURE;
        }
    };

    init_tracing(cli.log);

    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!(error = %err, "harw-warden exiting");
            ExitCode::FAILURE
        }
    }
}

/// Initialisiert den globalen `tracing`-Subscriber.
///
/// # Description
/// Muss genau einmal aufgerufen werden, als erste Anweisung nach dem Parsen
/// der Kommandozeile. Die angeforderte Stufe wird unverändert verwendet,
/// nie stillschweigend reduziert — siehe [`cli::LogLevel`]-Moduldoku für die
/// Begründung, warum ein ungültiger Rohwert diese Funktion nie erreicht.
///
/// # Arguments
/// - `level` (`cli::LogLevel`): die über `--log` angeforderte Stufe.
///
/// # Panics
/// Panics, falls bereits ein globaler Subscriber installiert ist (nur
/// erreichbar, wenn diese Funktion zweimal aufgerufen wird — ein
/// Programmierfehler).
fn init_tracing(level: cli::LogLevel) {
    use tracing_subscriber::EnvFilter;

    let filter =
        EnvFilter::try_new(level.as_filter_directive()).unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();
}

/// Führt den eigentlichen Durchsetzerbetrieb aus.
///
/// # Description
/// Reihenfolge: den systemd-Socket erwerben ([`systemd::acquire_listen_socket`],
/// harter Fehler ohne ihn — siehe dessen Moduldoku für die `LISTEN_PID`-
/// Prüfung), **dann** über Landlock selbst auf `cli.cgroup_root`
/// einschränken (harter Abbruch bei jedem Ausgang außer vollständiger
/// Durchsetzung, siehe [`landlock`]-Moduldoku), den produktiven Warden
/// zusammenbauen ([`warden_factory::build_production_warden`]) und die
/// Annahmeschleife starten ([`ipc::serve_forever`]). Der Socket selbst wird
/// vor Landlock erworben, weil er ein bereits von systemd geöffneter,
/// geerbter Deskriptor ist — Landlock beschränkt zukünftige
/// Pfadauflösungen, nicht bereits offene Deskriptoren; die Reihenfolge ist
/// hier also anders als bei `harw-probe-fs` (das einen neuen Socket-Pfad
/// selbst öffnen muss) ohne praktische Konsequenz, folgt aber trotzdem der
/// gleichen Disziplin „alles, was noch neue Pfade öffnen könnte, zuerst".
///
/// # Arguments
/// - `cli` (`cli::Cli`): die geparste Kommandozeile.
///
/// # Returns
/// Kehrt nie mit `Ok` zurück — [`ipc::serve_forever`] kehrt nur zurück,
/// wenn die Annahmeschleife endgültig endet, was diese Funktion als
/// [`WardenBinError::IpcAcceptLoopTerminated`] meldet.
///
/// # Errors
/// [`WardenBinError::ListenPidMissing`]/[`WardenBinError::ListenPidMalformed`]/
/// [`WardenBinError::ListenPidForeign`]/[`WardenBinError::ListenFdsMissing`]/
/// [`WardenBinError::ListenFdsMalformed`]/[`WardenBinError::UnexpectedListenFdCount`]/
/// [`WardenBinError::ListenFdsAcquisitionFailed`]: siehe [`systemd`].
/// [`WardenBinError::LandlockUnavailable`]: siehe [`landlock`].
/// [`WardenBinError::IpcAcceptLoopTerminated`]: die Annahmeschleife hat sich
/// endgültig beendet.
fn run(cli: Cli) -> Result<(), WardenBinError> {
    let listen_fd = systemd::acquire_listen_socket()?;
    tracing::info!("systemd listen socket acquired");

    landlock::enforce_cgroup_root(&cli.cgroup_root)?;

    let warden = Arc::new(warden_factory::build_production_warden(&cli.cgroup_root));
    tracing::info!(root = %cli.cgroup_root.display(), "warden ready; serving ipc connections");

    ipc::serve_forever(listen_fd, warden);
    Err(WardenBinError::IpcAcceptLoopTerminated)
}

#[cfg(test)]
mod tests {
    /// Deckt den im Auftrag verlangten Test „die Abhängigkeitsliste enthält
    /// keine Netz-Crate" ab — Muster: `harw-dod-flow`s
    /// `test_cargo_toml_declares_no_aya_dependency`.
    #[test]
    fn test_cargo_toml_declares_no_network_crate() {
        let manifest = include_str!("../Cargo.toml");
        for forbidden in [
            "reqwest",
            "hyper",
            "tokio",
            "tonic",
            "quinn",
            "socket2",
            "ipnet",
            "rustables",
            "trust-dns",
            "hickory",
            "async-std",
            "mio",
        ] {
            assert!(
                !manifest.contains(forbidden),
                "harw-warden darf keine Netz-Crate aufnehmen, gefunden: {forbidden}"
            );
        }
    }

    // `main`/`run` selbst werden hier bewusst nicht getestet: `run` würde
    // einen echten systemd-Deskriptor erwerben, eine echte Landlock-Regel
    // binden und eine echte Annahmeschleife starten — alle drei nach
    // Aufgabenstellung untersagt. Jede darin verkettete Teilfunktion ist
    // einzeln geprüft: `cli::Cli::try_parse_from` in `cli.rs`,
    // `systemd::verify_listen_pid`/`verify_listen_fds_count` in
    // `systemd.rs`, `warden_factory::build_production_warden` gegen den
    // echten `Warden` in `warden_factory.rs`, `protocol::WardenRequestEnvelope`
    // in `protocol.rs`, `audit::TracingAuditSink`/`isolation::UnimplementedNetworkIsolator`
    // in ihren eigenen Modulen. `landlock::enforce_cgroup_root` bleibt wie
    // bei `harw-sentinel`/`harw-probe-fs` ungetestet (siehe dessen
    // Moduldoku).
}
