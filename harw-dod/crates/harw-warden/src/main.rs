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
//! [`isolation::NftNetworkIsolator`]), und legt die Sandbox an (Landlock,
//! seccomp).
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
//! 3. **PL-100 WP 3.1** ergänzt `jiff` als Laufzeit-Eintrag (die Uhr für
//!    die Proof-Prüfung und den Hold-Timer liest dieses Binary, nicht die
//!    Bibliothek). `jiff` liegt über `harw-dod-warden-proto` bereits in der
//!    Hülle — kein neues Paket. Für `warden.toml` wird bewusst keine
//!    `toml`-Crate gezogen (siehe `harw_dod_warden::config`). Der direkte
//!    Eintrag `serde` entfällt mit dem eigenen v1-Umschlag
//!    (`WardenRequestEnvelope`); `serde` bleibt über die Kette in der Hülle.
//! 4. **PL-100 Entscheidung O10** ergänzt `seccompiler` (seccomp-Filter,
//!    [`seccomp`]) und `libc` (nur `SYS_*`- und Fehlerkonstanten für dessen
//!    Allowlist). `libc` liegt über `landlock` bereits in der Hülle;
//!    `seccompiler` ist **ein** neues Paket ohne weitere eigene
//!    Abhängigkeit. Eine nftables-Crate kommt nicht hinzu (Bewertung in der
//!    [`isolation`]-Moduldoku).
//!
//! **Befund, nicht Stillschweigen:** Der genannte Ausgangsstand „8 von 12"
//! lässt sich mit dieser Zählmethode nicht rekonstruieren (die geerbte
//! Kette allein zählt nach derselben Methode bereits 4, nach der
//! blattgenauen Methode der Proto-Crate bereits 65) — ein Hinweis darauf,
//! dass das eigentliche Gate (laut `harw-dod-warden-proto`s eigener
//! Dokumentation zum Zeitpunkt jenes Knotens noch nicht lauffähig) eine
//! andere, hier nicht bekannte Zählmethode verwendet. Diese Crate vermeidet
//! trotzdem jede vermeidbare zusätzliche Abhängigkeit (`libc` nur für
//! Konstanten, kein Mehrzweck-Socket-Framework, keine eigene Zeitzonen- oder
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
//! # seccomp nach Landlock (PL-100 Entscheidung O10)
//! Nach Landlock bindet [`seccomp::install`] eine Allowlist für alle Threads:
//! Speicher, Threads, Uhr, Zustandsdateien, cgroup-Kontrolldateien und der
//! Austausch auf dem geerbten Socket. Programme starten und Netlink-Sockets
//! anlegen darf der Warden nur, wenn die Policy eine Netzaktion erlaubt
//! (dann für das `nft`-Kindprozess-Backend, siehe [`isolation`]). Ein nicht
//! installierbarer Filter ist ein harter Startfehler wie bei Landlock.
//!
//! # Warum `SO_PEERCRED`, nicht ein Token im Rumpf
//! Siehe [`ipc`]-Moduldoku, Abschnitt „Warum `SO_PEERCRED`, nicht ein Token
//! im Rumpf": `harw_dod_warden`s eigene Dokumentation benennt die
//! Vertrauensgrenze dieses Systems bereits als „ein
//! `SOCK_SEQPACKET`-Unix-Socket mit `SO_PEERCRED`, AW5-04b" — diese Crate
//! liest sie deshalb bei jeder angenommenen Verbindung, bevor irgendeine
//! Nutzlast gelesen wird, und glaubt keiner Behauptung im Nachrichtenrumpf.
//! Seit PL-100 WP 3.1 ist die Peer-uid eine Allowlist ([`peer`]): nur die
//! uid des Escalators wird bedient, jeder andere Peer erhält ungelesen die
//! Antwort `peer-rejected`.
//!
//! # Proof v2 und Warden-eigene Policy (PL-100 WP 3.1)
//! Akzeptiert wird nur die v2-Anfrage mit `SignedAuthorization`; ein
//! v1-Auftrag wird beim Dekodieren ausdrücklich abgelehnt ([`protocol`]).
//! Policy (`--policy`, Vorgabe `/etc/harw-dod/warden.toml`), Proof-Schlüssel
//! und Escalator-uid lädt dieses Binary vor Landlock selbst; Nonce-Ledger,
//! Aktions-Ledger und Hold-Timer liegen im StateDirectory (`--state-dir`,
//! Vorgabe `/var/lib/harw-warden`). Jede Anfrage bekommt eine Antwort.
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
//! nimmt keine Netz-Crate auf; der seccomp-Filter erlaubt `socket` gar
//! nicht bzw. (bei erlaubter Netzaktion) nur für `AF_NETLINK`.
//! [`isolation::NftNetworkIsolator`] isoliert cgroups über ein
//! [`isolation::NetfilterBackend`] — heute das lokale `nft`-Binary
//! (nftables) —, ohne selbst Netzverkehr zu erzeugen.
//!
//! # Audit vor jedem Fehlerpfad — auch hier durchgesetzt, nicht umgangen
//! `Warden::handle` schreibt einen dauerhaften Ledger-Eintrag und einen
//! Audit-Eintrag vor jedem Ausführungsversuch (siehe
//! `harw_dod_warden::warden`-Moduldoku). Dieses Binary entscheidet vor
//! diesem Aufruf nur über den Transport: Peer-Allowlist und Dekodieren
//! (v2 oder Ablehnung). Eine dekodierte Anfrage geht unverändert an
//! `Warden::handle` ([`ipc::respond`]).
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
mod keygen;
mod landlock;
mod peer;
mod protocol;
mod seccomp;
mod systemd;
mod timer;
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
/// `ExitCode::SUCCESS` nur bei `--help`/`--version` oder nach erfolgreichem
/// `completions` bzw. `keygen`. Jeder andere Pfad ist
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
/// Reihenfolge (PL-100 WP 3.1):
/// 1. den systemd-Socket erwerben ([`systemd::acquire_listen_socket`],
///    harter Fehler ohne ihn — siehe dessen Moduldoku für die
///    `LISTEN_PID`-Prüfung);
/// 2. die Warden-eigene Policy laden (`cli.policy`, root-eigen, nicht
///    gruppen- oder weltschreibbar, [`harw_dod_warden::load_warden_config`]);
/// 3. den Schlüsselring aus `proof_key_dir` laden (Dateien `0600` root,
///    [`harw_dod_warden::load_key_ring`]);
/// 4. die Escalator-uid für die Peer-Allowlist einmal auflösen
///    ([`peer::resolve_escalator_uid`]);
/// 5. das StateDirectory öffnen (Nonce-Ledger, Aktions-Ledger, Holds;
///    [`harw_dod_warden::WardenState::open`]);
/// 6. den produktiven Warden zusammenbauen
///    ([`warden_factory::build_production_warden`], reiner Zusammenbau);
///    er liefert auch den Sandbox-Bedarf des Netfilter-Backends (leer ohne
///    erlaubte Netzaktion);
/// 7. **dann** über Landlock auf `cli.cgroup_root`, `cli.state_dir` und
///    diesen Bedarf einschränken (harter Abbruch bei jedem Ausgang außer
///    vollständiger Durchsetzung, siehe [`landlock`]-Moduldoku) — Policy,
///    Schlüssel und `/etc/passwd` sind danach unerreichbar;
/// 8. die seccomp-Allowlist für alle Threads binden ([`seccomp::install`],
///    PL-100 O10; harter Abbruch, wenn das scheitert);
/// 9. den Hold-Timer starten ([`timer::spawn_hold_sweeper`]) und die
///    Annahmeschleife starten ([`ipc::serve_forever`]).
///
/// Der Socket selbst wird
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
/// `Ok(())` nur nach den Unterkommandos `completions` und `keygen`. Sonst kehrt diese
/// Funktion nie mit `Ok` zurück — [`ipc::serve_forever`] kehrt nur zurück,
/// wenn die Annahmeschleife endgültig endet, was diese Funktion als
/// [`WardenBinError::IpcAcceptLoopTerminated`] meldet.
///
/// # Errors
/// [`WardenBinError::ListenPidMissing`]/[`WardenBinError::ListenPidMalformed`]/
/// [`WardenBinError::ListenPidForeign`]/[`WardenBinError::ListenFdsMissing`]/
/// [`WardenBinError::ListenFdsMalformed`]/[`WardenBinError::UnexpectedListenFdCount`]/
/// [`WardenBinError::ListenFdsAcquisitionFailed`]: siehe [`systemd`].
/// [`WardenBinError::Setup`]: Policy, Schlüssel oder StateDirectory.
/// [`WardenBinError::PasswdUnreadable`]/[`WardenBinError::PasswdTooLarge`]/
/// [`WardenBinError::EscalatorUserUnknown`]/
/// [`WardenBinError::EscalatorUserAmbiguous`]/[`WardenBinError::EscalatorUidRoot`]:
/// siehe [`peer`].
/// [`WardenBinError::NetAdminMissing`]: Netzaktionen erlaubt, aber
/// `CAP_NET_ADMIN` fehlt in der Ambient-Menge (siehe [`require_net_admin`]).
/// [`WardenBinError::LandlockUnavailable`]: siehe [`landlock`].
/// [`WardenBinError::SeccompUnavailable`]: siehe [`seccomp`].
/// [`WardenBinError::HoldTimerSpawn`]: siehe [`timer`].
/// [`WardenBinError::IpcAcceptLoopTerminated`]: die Annahmeschleife hat sich
/// endgültig beendet.
/// [`WardenBinError::Completions`]: das Unterkommando `completions` ist
/// fehlgeschlagen.
/// [`WardenBinError::KeyGeneration`]/[`WardenBinError::Setup`]: das
/// Unterkommando `keygen` ist fehlgeschlagen (siehe [`keygen`]).
fn run(cli: Cli) -> Result<(), WardenBinError> {
    // `completions` und `keygen` laufen vor jedem systemd-, Landlock- oder
    // Socket-Schritt: sie schreiben ein Skript bzw. einen Schlüssel und
    // beenden sich.
    match &cli.command {
        Some(cli::Command::Completions(args)) => {
            harw_completions::run_completions(
                &mut <Cli as clap::CommandFactory>::command(),
                "harw-warden",
                args,
                &harw_completions::HomeEnv::from_process(),
                &mut std::io::stdout().lock(),
            )?;
            return Ok(());
        }
        Some(cli::Command::Keygen(args)) => {
            return keygen::run(args, &mut std::io::stdout().lock());
        }
        None => {}
    }

    let listen_fd = systemd::acquire_listen_socket()?;
    // systemd übergibt den Socket ohne `FD_CLOEXEC`; ein `nft`-Kindprozess
    // soll ihn nicht erben (angenommene Verbindungen sind ohnehin
    // `SOCK_CLOEXEC`, siehe `ipc::serve_forever`).
    if let Err(error) = rustix::io::fcntl_setfd(&listen_fd, rustix::io::FdFlags::CLOEXEC) {
        tracing::warn!(error = %error, "could not mark the listen socket close-on-exec");
    }
    tracing::info!("systemd listen socket acquired");

    let trust = harw_dod_warden::TrustRules::root();
    let config = harw_dod_warden::load_warden_config(&cli.policy, trust)?;
    let keys = harw_dod_warden::load_key_ring(&config.proof_key_dir, trust)?;
    let escalator_uid =
        peer::resolve_escalator_uid(&config.escalator, std::path::Path::new(peer::PASSWD_PATH))?;
    let state = harw_dod_warden::WardenState::open(&cli.state_dir, jiff::Timestamp::now())?;
    let key_ids: Vec<&str> = keys
        .key_ids()
        .into_iter()
        .map(harw_dod_warden_proto::KeyId::as_str)
        .collect();
    tracing::info!(
        policy = %cli.policy.display(),
        key_ids = ?key_ids,
        allowed_actions = ?config.policy.actions().kinds(),
        escalator_uid,
        "warden policy, proof keys and state loaded"
    );

    let network_actions = isolation::network_actions_enabled(config.policy.actions());
    // Vor Landlock und seccomp (`prctl`): ohne `CAP_NET_ADMIN` in der
    // Ambient-Menge scheiterte jede Isolation erst im Ernstfall.
    require_net_admin(network_actions, net_admin_in_ambient_set)?;
    let production =
        warden_factory::build_production_warden(&cli.cgroup_root, keys, config.policy, state);

    landlock::enforce_runtime_scope(&cli.cgroup_root, &cli.state_dir, &production.sandbox)?;
    let profile = seccomp::install(&production.sandbox.syscalls)?;
    tracing::info!(
        allowed_syscalls = profile.allowed_count(),
        network_actions,
        "seccomp allowlist installed for all threads"
    );

    let warden = Arc::new(production.warden);
    let gate = Arc::new(peer::PeerGate::new(escalator_uid));
    timer::spawn_hold_sweeper(Arc::clone(&warden))?;
    tracing::info!(root = %cli.cgroup_root.display(), "warden ready; serving ipc connections");

    ipc::serve_forever(listen_fd, warden, gate);
    Err(WardenBinError::IpcAcceptLoopTerminated)
}

/// Refuses to start when network actions are allowed but an `nft` child would lack `CAP_NET_ADMIN`.
///
/// # Description
/// Die Vorgabe-Unit (`deploy/systemd/harw-warden.service`) gibt nur
/// `CAP_DAC_OVERRIDE`; `CAP_NET_ADMIN` kommt erst mit dem Isolations-Drop-in,
/// das der Installer nur anlegt, wenn `warden.toml` `isolate-network` oder
/// `lift-isolation` erlaubt. Das `nft`-Kind erbt die Capability über die
/// Ambient-Menge. Fehlt sie trotz erlaubter Netzaktion, ist das eine
/// Fehlkonfiguration, die sonst erst beim ersten Eingriff auffiele; der
/// Warden startet dann nicht (wie bei jeder anderen unsicheren Policy).
///
/// # Arguments
/// - `network_actions` (`bool`): ob die Policy eine Netzaktion erlaubt.
/// - `probe` (`impl FnOnce() -> std::io::Result<bool>`): fragt die
///   Ambient-Menge ab; wird ohne Netzaktion nicht aufgerufen.
///
/// # Errors
/// [`WardenBinError::NetAdminMissing`]: Netzaktion erlaubt, Capability
/// fehlt oder lässt sich nicht abfragen (fail closed).
fn require_net_admin(
    network_actions: bool,
    probe: impl FnOnce() -> std::io::Result<bool>,
) -> Result<(), WardenBinError> {
    if !network_actions {
        return Ok(());
    }
    match probe() {
        Ok(true) => Ok(()),
        Ok(false) => Err(WardenBinError::NetAdminMissing),
        Err(error) => {
            tracing::debug!(error = %error, "ambient capability probe failed");
            Err(WardenBinError::NetAdminMissing)
        }
    }
}

// Ob `CAP_NET_ADMIN` in der Ambient-Menge dieses Prozesses liegt (genau das
// erbt ein `nft`-Kind ohne Datei-Capabilities über `execve`).
fn net_admin_in_ambient_set() -> std::io::Result<bool> {
    rustix::thread::capability_is_in_ambient_set(rustix::thread::CapabilitySet::NET_ADMIN)
        .map_err(std::io::Error::from)
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

    /// Die Unit muss zur Sandbox im Code passen: StateDirectory für Ledger
    /// und Holds (`0700`), der Aufruf `seccomp` für [`super::seccomp::install`]
    /// (nicht in `@system-service`), die Landlock-Aufrufe und eine
    /// Capability-Menge ohne `CAP_SYS_ADMIN`.
    #[test]
    fn test_systemd_unit_matches_the_sandbox() -> crate::test_support::TestResult {
        use crate::test_support::TestError;

        let unit = include_str!("../../../../deploy/systemd/harw-warden.service");
        let lines: Vec<&str> = unit.lines().map(str::trim).collect();
        for required in [
            "StateDirectory=harw-warden",
            "StateDirectoryMode=0700",
            "ReadWritePaths=/sys/fs/cgroup /var/lib/harw-warden",
            "SystemCallFilter=@system-service",
            "NoNewPrivileges=yes",
        ] {
            if !lines.contains(&required) {
                return Err(TestError::Unexpected(format!("unit lacks {required:?}")));
            }
        }
        let allowed_calls: Vec<&str> = lines
            .iter()
            .filter_map(|line| line.strip_prefix("SystemCallFilter="))
            .filter(|value| !value.starts_with('~'))
            .flat_map(|value| value.split_whitespace())
            .collect();
        for call in [
            "seccomp",
            "landlock_create_ruleset",
            "landlock_add_rule",
            "landlock_restrict_self",
        ] {
            if !allowed_calls.contains(&call) {
                return Err(TestError::Unexpected(format!(
                    "SystemCallFilter does not allow {call}"
                )));
            }
        }
        // Vorgabe: nur Freeze/Thaw, also kein `CAP_NET_ADMIN`; das
        // Isolations-Drop-in ergänzt es (siehe Kommentar der Unit).
        let bounding = lines
            .iter()
            .find_map(|line| line.strip_prefix("CapabilityBoundingSet="))
            .ok_or(TestError::Missing("CapabilityBoundingSet"))?;
        assert_eq!(bounding, "CAP_DAC_OVERRIDE");
        let ambient = lines
            .iter()
            .find_map(|line| line.strip_prefix("AmbientCapabilities="))
            .ok_or(TestError::Missing("AmbientCapabilities"))?;
        assert_eq!(ambient, "CAP_DAC_OVERRIDE");
        assert!(unit.contains("AmbientCapabilities=CAP_NET_ADMIN"));
        Ok(())
    }

    #[test]
    fn test_network_actions_require_net_admin_in_the_ambient_set() {
        use super::{WardenBinError, require_net_admin};

        // Ohne Netzaktion wird gar nicht erst gefragt.
        assert!(require_net_admin(false, || Ok(false)).is_ok());
        assert!(require_net_admin(true, || Ok(true)).is_ok());
        assert!(matches!(
            require_net_admin(true, || Ok(false)),
            Err(WardenBinError::NetAdminMissing)
        ));
        assert!(matches!(
            require_net_admin(true, || Err(std::io::Error::from(
                std::io::ErrorKind::Unsupported
            ))),
            Err(WardenBinError::NetAdminMissing)
        ));
    }

    // `main`/`run` selbst werden hier bewusst nicht getestet: `run` würde
    // einen echten systemd-Deskriptor erwerben, eine echte Landlock-Regel
    // und einen echten seccomp-Filter binden und eine echte Annahmeschleife
    // starten — alle nach Aufgabenstellung untersagt. Jede darin verkettete Teilfunktion ist
    // einzeln geprüft: `cli::Cli::try_parse_from` in `cli.rs`,
    // `systemd::verify_listen_pid`/`verify_listen_fds_count` in
    // `systemd.rs`, `warden_factory::build_production_warden` gegen den
    // echten `Warden` in `warden_factory.rs`, das Wire-Format (v1 abgelehnt)
    // in `protocol.rs`, `ipc::respond` (jede Ablehnung hat eine Antwort) in
    // `ipc.rs`, die Peer-Allowlist in `peer.rs`, der Hold-Timer in `timer.rs`,
    // `audit::TracingAuditSink`/`isolation::NftNetworkIsolator` (samt
    // `NetfilterBackend` mit einem aufzeichnenden Backend) in ihren eigenen
    // Modulen, die seccomp-Allowlist als Profil und BPF-Programm in
    // `seccomp.rs`. `landlock::enforce_runtime_scope` und `seccomp::install`
    // bleiben wie bei `harw-sentinel`/`harw-probe-fs` ungetestet (sie würden
    // den Testprozess selbst einschränken).
}

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
