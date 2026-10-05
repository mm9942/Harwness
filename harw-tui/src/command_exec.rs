//! Ausführung von `/command`-Eingaben in der Chat-TUI über die
//! Operation-Adapter-Pipeline (`harw-operations` / `harw-ops`).
//!
//! # Verantwortung
//! Dieses Modul stellt [`execute_command_as`] bereit — eine async Funktion, die
//! eine abgeschickte `/command`-Zeile klassifiziert ([`crate::classify_input`])
//! und, im Fall einer `Invocation::Command`, den passenden
//! [`harw_operations::adapter::CommandAdapter`] aus der übergebenen Adapter-Liste
//! sucht und dispatcht. Die Zulassung erfolgt über
//! [`crate::CommandRegistry::dispatch`] (siehe „Berechtigungen“).
//!
//! # Verantwortungsgrenze
//! Das Modul führt selbst **keine** Operationslogik aus — jede `/command`-Zeile
//! wird 1:1 an `CommandAdapter::dispatch` (und damit an `Operation::run` in
//! `harw-ops`) delegiert. Für `/help` wird eine frische
//! [`harw_operations::registry::OperationRegistry`] aus der Adapter-Liste
//! rekonstruiert und in den [`ServiceMap`] des jeweiligen `OpContext` gelegt,
//! da `HelpOperation` die Registry als Service benötigt, um alle Ops
//! aufzulisten (`harw-ops/src/help.rs`). Alle anderen Ops benötigen keine
//! Services.
//!
//! # Berechtigungen
//! Jeder Dispatch führt eine explizite Caller-Stufe mit, die die Composition
//! Root wählt. Die Admission (unbekannter Command mit Tippfehler-Vorschlag,
//! Stufe gegen die deklarierte [`harw_operations::PermissionTier`] des Adapters,
//! Scope, Shell-Capability) läuft ausschließlich über
//! [`crate::CommandRegistry::dispatch`]; der Katalog wird pro Aufruf per
//! `CommandRegistry::from_command_adapters` aus der Adapter-Liste gebaut.
//! Services und `OpContext` entstehen erst nach erfolgreicher Admission.
//!
//! # Services-Closure (W2d-2/CE)
//! [`execute_command_as`] nimmt die Services nicht mehr als fertigen
//! [`ServiceMap`]-Wert oder als `CommandServices`-Bündel entgegen, sondern als
//! generische Closure `F: FnOnce() -> ServiceMap`. Composition Roots
//! (`app.rs`) übergeben `|| runtime_commands::slash_service_map(rt.services())`.
//! Die Closure wird **erst** aufgerufen, nachdem [`crate::CommandRegistry::dispatch`]
//! die Invocation admittiert und in `CommandAction::Command` aufgelöst hat —
//! ein abgelehnter Command (unbekannt, Berechtigung, Capability) baut nie eine
//! `ServiceMap`. `CommandServices` und [`build_services`] existieren nur noch
//! `#[cfg(test)]`, um die vorherigen Test-Erwartungen (feste Service-Bündel)
//! als Closures nachzubilden.
//!
//! # `!`-Befehle laufen auf dem Host (Runde 6, Teil B)
//! `!`/`!!` sind Befehle der Nutzerin selbst und laufen **immer auf dem
//! Host** über [`harw_tool_shell::OperatorCommand`]: echtes `HOME`, geerbte
//! Umgebung, `cwd` = Projektwurzel, `setsid`, rlimits, gekappte Ausgabe,
//! Zeitlimit; `sudo`/`doas`/`pkexec` bleiben abgelehnt; keine Freigabe,
//! aber ein Audit-Ereignis `shell.operator_exec`. Früher lief `!` in
//! Bubblewrap mit flüchtigem `HOME=/tmp/home` — `!cp x ~/` meldete Exit 0,
//! die Datei verschwand aber mit der Sandbox. Die TUI startet `!` über
//! [`admit_shell_line`] und `app/operator_shell.rs` asynchron (auch während
//! eines Turns); [`execute_shell`] bedient nur noch die synchronen Pfade.
//! Die Modell-`shell.exec`-Ausführung bleibt unverändert in der Sandbox.
//! Nicht-TUI-Kanäle (Telegram) kennen kein `!` (geprüft in Runde 6).
//!
//! # `!`-Modus wie in Claude Code (Plan Teil F)
//! [`execute_shell`] liefert seit Plan Teil F kein reines `String` mehr,
//! sondern [`ShellDisplayOutcome`] — Anzeigetext (`display_text`,
//! byte-identisch zum bisherigen Rückgabewert) plus ein optionales
//! strukturiertes [`ShellRunOutcome`] (`run`), gesetzt genau dann, wenn der
//! Befehl tatsächlich auf dem Host lief (seit Runde 6, Teil B).
//! [`execute_with_context`] nutzt weiterhin nur
//! `display_text` und bleibt dadurch für alle bisherigen Aufrufer
//! (`execute_command_as`, `dispatch_slash_command`, die Busy-Sofort-
//! Dispatch- und Test-Pfade) unverändert.
//!
//! Die neue [`dispatch_command_with_shell_result`] ist der einzige Aufrufer,
//! der `run` tatsächlich braucht: `app.rs`s `HarwEvent::Command`-Zweig ruft
//! sie für jede Rohzeile auf, reicht den zuletzt gelaufenen `!`-Befehl der
//! Sitzung (`ChatApp::last_shell_command`) als `last_shell_command` durch und
//! startet bei `Some(shell)` sofort einen Folge-Turn mit dem Befehl,
//! Exit-Code und der (auf 8000 Zeichen gekappten) Ausgabe als
//! Nutzereingabe. `!!` ([`Invocation::ShellRepeat`]) wird hier — und nur
//! hier — real aufgelöst: mit `last_shell_command = Some(cmd)` läuft `cmd`
//! erneut über [`execute_shell`]; ohne Vorgänger liefert es den Text
//! „Kein vorheriger !-Befehl“, `run: None` (kein Folge-Turn). Die alte,
//! zustandslose [`execute_with_context`] kennt keinen Sitzungszustand und
//! bleibt für `!!` bei „Shell-Wiederholung ist noch nicht verfügbar.“ — das
//! ist folgenlos, weil die TUI seit Runde 6, Teil B jede `!`/`!!`-Zeile mit
//! Runtime-Montage vorher abfängt (`app/operator_shell.rs`, im Leerlauf wie
//! während eines Turns) und asynchron auf dem Host ausführt; die hiesigen
//! Pfade sind nur noch der Rückfall ohne Montage bzw. für Tests.
//!
//! # Nebenläufigkeit
//! `execute_command_as` ist `async` und ruft `CommandAdapter::dispatch` (ebenfalls
//! `async`) auf. Der Aufrufer (`run_loop` in `app.rs`) awaitet die Funktion im
//! `current_thread`-Tokio-Runtime des Chat-Loops.
//!
//! # Fehler
//! Erzeugt keine eigenen Fehlertypen. Klassifizierungsfehler
//! ([`crate::CommandError`]) und Dispatch-Fehler ([`harw_operations::OpError`])
//! werden in menschenlesbaren Text umgesetzt.

// `HashSet`, `Arc`, `OperationRegistry`, `SharedSessionController` und
// `TuiSessionController` werden ab W2d-2/CE nur noch von den `#[cfg(test)]`-
// gebundenen Items `CommandServices` und `build_services` gebraucht — siehe
// Moduldoc „Services-Closure (W2d-2/CE)". Ohne `#[cfg(test)]` hier wären sie
// in einem Produktions-Build ungenutzte Importe (Verstoß gegen `-D warnings`).
#[cfg(test)]
use std::collections::HashSet;
#[cfg(test)]
use std::sync::Arc;

use harw_authority::SandboxSpec;
#[cfg(test)]
use harw_operations::SharedSessionController;
use harw_operations::adapter::CommandAdapter;
use harw_operations::operation::{BusyAvailability, BusySubcommand};
#[cfg(test)]
use harw_operations::registry::OperationRegistry;
use harw_operations::{OpContext, PermissionTier, ServiceMap};
// Runde 6, Teil B: `!`-Befehle laufen über den Operator-Weg auf dem Host.
use harw_tool_shell::{
    OPERATOR_DEFAULT_TIMEOUT_SECS, OperatorCommand, OperatorEnd, OperatorOutcome, ShellLimits,
};
use harw_types::{SessionId, TurnId};

#[cfg(test)]
use crate::session_controller::TuiSessionController;
use crate::{
    CapabilitySet, CommandAction, CommandError, CommandRegistry, DispatchContext, Invocation,
    InvocationSurface,
};

/// Bündelt die optionalen/langlebigen Services, die eine `/command`-Ausführung
/// in Tests benötigt.
///
/// # Beschreibung
/// Nur noch `#[cfg(test)]` (W2d-2/CE): Produktionsaufrufer bauen die
/// [`ServiceMap`] direkt über `runtime_commands::slash_service_map` und reichen
/// sie als `F: FnOnce() -> ServiceMap`-Closure an [`execute_command_as`].
/// `CommandServices` bildet dasselbe Feldbündel für Tests nach, die weiterhin
/// gezielt einzelne Services (Config, Memory, Controller, Job-Store) setzen
/// wollen; sie wandern 1:1 in [`build_services`], das ebenfalls nur unter Test
/// existiert.
///
/// # Felder
/// - `runtime_config` (`Option<&Arc<harw_config::ResolvedConfig>>`): Optionaler,
///   von der Composition Root aufgelöster Laufzeit-Config-Snapshot. Wenn
///   vorhanden, wird genau dieser `Arc` für `/model` und `/provider` als Service
///   bereitgestellt.
/// - `memory` (`Option<&Arc<dyn harw_memory::Memory>>`): Optionales Memory-Backend.
/// - `controller` (`&Arc<TuiSessionController>`): Der langlebige Session-Controller,
///   der in die `ServiceMap` eingetragen wird. Zustandsänderungen (z.B. über
///   `/effort` oder `/model`) überleben so den Aufruf und sind beim nächsten
///   Turn sichtbar.
/// - `job_store` (`Option<&Arc<harw_session_store::JobStore>>`): Optionaler
///   dauerhafter Job-Store.
/// - `host_permit_handles` (`Option<&Arc<harw_tool_shell::HostPermitHandles>>`):
///   Optionales Bündel aus Permit-Ledger, Host-Permit-Session-Registry und
///   Fragekanal-Sender (Plan Teil B3/C2). In Produktion aus den
///   `RuntimeAssembly`-Accessoren `host_permit_ledger()`/
///   `host_permit_session_registry()`/`host_permit_prompt_sender()`
///   gebaut (gleiche `Arc`-Instanzen, Sender geklont) — hier nur
///   durchgereicht, damit `/sandbox-lease` und die `/status`-Zeile den
///   Service in Tests auflösen können.
#[cfg(test)]
pub(crate) struct CommandServices<'a> {
    pub(crate) runtime_config: Option<&'a Arc<harw_config::ResolvedConfig>>,
    pub(crate) memory: Option<&'a Arc<dyn harw_memory::Memory>>,
    pub(crate) controller: &'a Arc<TuiSessionController>,
    pub(crate) job_store: Option<&'a Arc<harw_session_store::JobStore>>,
    pub(crate) host_permit_handles: Option<&'a Arc<harw_tool_shell::HostPermitHandles>>,
}

/// Führt eine abgeschickte `/command`-Zeile über die Operation-Adapter-Pipeline
/// aus und liefert das Ergebnis als Anzeigetext.
///
/// This owner-tier compatibility wrapper is compiled only for this module's
/// source-local tests. Production callers must use [`execute_command_as`] and
/// provide their explicit permission tier.
///
/// # Beschreibung
/// Klassifiziert `raw_line` via [`crate::classify_input`]:
/// - [`Invocation::Command`] → Pfad `/{name}` in `adapters` suchen. Gefunden:
///   [`OpContext`] bauen (Session-ID geklont, frische [`TurnId`], Sandbox
///   geklont, Services siehe unten) und [`CommandAdapter::dispatch`] awaiten.
///   `Ok(output)` liefert `output.text`; `Err(error)` wird als
///   `"Fehler: {error}"` gerendert. Nicht gefunden: `"Unbekannter Command: {path}"`.
/// - [`Invocation::Shell`]: läuft im lokalen TUI-Kontext standardmäßig auf dem
///   Host (Operator-Weg, Runde 6, Teil B). `HARW_DISABLE_SHELL=1`
///   schaltet die Capability für den Prozess aus.
/// - [`Invocation::ShellRepeat`]: ist noch nicht implementiert.
/// - [`Invocation::Note`]: wird vorab zu `/diary note …` bzw.
///   `/memory record …` umgeschrieben (siehe `rewrite_note_line`); ohne
///   passenden Adapter `"Notiz: {text}"`.
/// - [`Invocation::Mention`]: `"@{target}: {body}"` (die Chat-Weiterleitung
///   übernimmt `app.rs`).
/// - [`Invocation::Chat`]: unverändert durchgereicht.
///
/// # Argumente
/// - `adapters` (`&[CommandAdapter]`): alle `/`-Command-Adapter, gebaut aus der
///   `OperationRegistry` in `crate::runtime_root::run_tui` (`CommandAdapter::from_operation`).
/// - `sandbox` (`&SandboxSpec`): Authority-Boundary; wird pro Dispatch geklont
///   in den `OpContext` übernommen.
/// - `session_id` (`&SessionId`): Stabile Session-ID; wird pro Dispatch geklont.
/// - `raw_line` (`&str`): die abgeschickte Rohzeile (inkl. führendem `/`, `!`,
///   `#` bzw. `@`).
/// - `services` (`&CommandServices<'_>`): siehe [`CommandServices`]; wird pro
///   Aufruf in eine `|| build_services(..)`-Closure für [`execute_command_as`]
///   übersetzt.
///
/// # Rückgabe
/// Ein (ggf. mehrzeiliger, durch `\n` getrennter) Ausgabetext für die Historie.
///
/// # Nebenläufigkeit
/// `async`; awaitet genau einen `CommandAdapter::dispatch`-Aufruf, falls ein
/// Command gefunden wurde. Keine eigenen Locks oder geteilten Zustände.
#[cfg(test)]
async fn execute_command(
    adapters: &[CommandAdapter],
    sandbox: &SandboxSpec,
    session_id: &SessionId,
    raw_line: &str,
    services: &CommandServices<'_>,
) -> String {
    execute_command_as(
        adapters,
        sandbox,
        session_id,
        PermissionTier::Owner,
        raw_line,
        || {
            build_services(
                adapters,
                services.runtime_config,
                services.memory,
                services.controller,
                services.job_store,
                services.host_permit_handles,
            )
        },
    )
    .await
}

/// Executes a command as an explicitly bounded caller.
///
/// This is the permission-enforcing variant used by composition roots. The
/// test-only compatibility wrapper [`execute_command`] represents the local
/// owner console in source-local tests.
///
/// # Beschreibung
/// Die gesamte Admission (unbekannter Command mit Vorschlag, Stufe, Scope,
/// Shell-Capability) läuft ausschließlich über
/// [`crate::CommandRegistry::dispatch`] mit dem TUI-Kontext aus
/// [`tui_dispatch_context`]. Es gibt keine zweite Tier-Prüfung in diesem Modul.
///
/// `services` ist ab W2d-2/CE keine fertige [`ServiceMap`] und kein
/// [`CommandServices`]-Bündel mehr, sondern eine `F: FnOnce() -> ServiceMap`-
/// Closure. Sie wird **nur** aufgerufen, wenn [`crate::CommandRegistry::dispatch`]
/// die Invocation zu `CommandAction::Command` auflöst — jeder Admission-Fehler
/// (unbekannt, Berechtigung, Capability) gibt seinen Text zurück, ohne die
/// Closure je aufzurufen.
///
/// # Argumente
/// - `services` (`F`): liefert die [`ServiceMap`] für genau diesen Dispatch,
///   erst nach erfolgreicher Admission ausgewertet.
///
/// # Nebenläufigkeit
/// `async`; ruft `services()` synchron innerhalb des `async fn`-Bodys auf,
/// bevor `CommandAdapter::dispatch` awaitet wird.
pub(crate) async fn execute_command_as<F>(
    adapters: &[CommandAdapter],
    sandbox: &SandboxSpec,
    session_id: &SessionId,
    caller_permission: PermissionTier,
    raw_line: &str,
    services: F,
) -> String
where
    F: FnOnce() -> ServiceMap,
{
    execute_with_context(
        adapters,
        sandbox,
        session_id,
        tui_dispatch_context(caller_permission),
        raw_line,
        services,
    )
    .await
}

/// Kapselt den Idle-Dispatch-Ablauf aus `app.rs` (`HarwEvent::Command`-Zweig,
/// ca. Zeile 2955-2990: „keine Runtime"-Fehlerpfad + `execute_command_as`-
/// Aufruf über `runtime_commands::caller_tier`/`runtime_commands::slash_service_map`)
/// als wiederverwendbaren Helfer für den Welle-4b-Sofort-Dispatch
/// (`BusyAvailability::Immediate` während eines laufenden Turns) und für die
/// Welle-5-Konsolidierung, die diesen Codeblock zwischen Idle- und Busy-Pfad
/// nicht länger duplizieren soll.
///
/// # Beschreibung
/// Bei `runtime = None` liefert dieser Helfer wortgleich die heutige
/// „keine Runtime"-Meldung aus `app.rs`: `"Fehler: keine Runtime-Montage"`
/// (inkl. desselben `tracing::error!("tui.command.no_runtime_assembly")`).
/// Bei `Some(rt)` liest er die Aufrufer-Stufe über
/// [`crate::runtime_commands::caller_tier`] aus `rt.principal()` und
/// dispatcht über [`execute_command_as`] mit
/// `|| crate::runtime_commands::slash_service_map(rt.services())` als
/// Services-Closure — identisch zum bestehenden `execute_command_as`-Zweig in
/// `app.rs`.
///
/// **Bewusst ausgeklammert:** die `/export`-Sonderbehandlung. `app.rs`
/// führt `/export` über [`crate::command_data::execute_command_with_data`]
/// aus, weil es zusätzlich `OpOutput::data` für den Export-Dateischreiber
/// braucht. Für den Sofort-Dispatch (Welle 4b) ist das folgenlos: `/export`
/// trägt `busy = DeferredUntilTurnEnd` und läuft daher nie über diesen
/// Helfer.
///
/// # Argumente
/// - `runtime` (`Option<&std::sync::Arc<harw_runtime::RuntimeAssembly>>`):
///   `app.runtime()`. `None` → keine Runtime-Montage vorhanden.
/// - `adapters` (`&[CommandAdapter]`): `app.adapters()`.
/// - `sandbox` (`&SandboxSpec`): `app.sandbox()`.
/// - `session_id` (`&SessionId`): `app.session_id()`.
/// - `raw` (`&str`): die abgeschickte `/command`-Zeile.
///
/// # Rückgabe
/// Anzeigetext, identisch zum heutigen `HarwEvent::Command`-Idle-Zweig
/// (abzüglich der `/export`-`data`-Sonderbehandlung, siehe oben — deren
/// `Vec<Line<'static>>`-Aufteilung und `app.push_lines`/
/// `app.apply_pending_controller_state`-Nachbereitung bleiben ohnehin
/// Aufgabe des jeweiligen Aufrufers, nicht dieses Helfers).
///
/// # Nebenläufigkeit
/// `async`; ruft [`execute_command_as`] genau einmal auf. Keine eigenen
/// Locks oder geteilten Zustände.
pub(crate) async fn dispatch_slash_command(
    runtime: Option<&std::sync::Arc<harw_runtime::RuntimeAssembly>>,
    adapters: &[CommandAdapter],
    sandbox: &SandboxSpec,
    session_id: &SessionId,
    raw: &str,
) -> String {
    match runtime {
        Some(rt) => {
            let caller_tier = crate::runtime_commands::caller_tier(rt.principal());
            execute_command_as(adapters, sandbox, session_id, caller_tier, raw, || {
                crate::runtime_commands::slash_service_map(rt.services())
            })
            .await
        }
        None => {
            tracing::error!("tui.command.no_runtime_assembly");
            "Fehler: keine Runtime-Montage".to_owned()
        }
    }
}

/// Klassifiziert eine rohe Eingabezeile in ihre
/// [`harw_operations::operation::BusyAvailability`] für den Busy-Dispatch
/// (Runde 4, Teil H).
///
/// # Beschreibung
/// Nutzt [`crate::classify_input`] (denselben Parser wie jeder andere
/// Dispatch-Pfad). Nur [`Invocation::Command`] kann `Immediate`/`Staged`
/// liefern — und seit Runde 6, Teil B `!`/`!!`:
/// - `Shell`, `ShellRepeat` → `Immediate` (sofort, auf dem Host; das
///   Ergebnis geht erst nach dem Ende an den Agenten).
/// - Kein Befehl (`Note`, `Mention`, `Chat`) → `DeferredUntilTurnEnd`.
/// - Unbekannter Befehlsname (`registry.find` liefert `None`) →
///   `DeferredUntilTurnEnd` — die ehrliche „unbekannter Befehl"-Meldung
///   entsteht weiterhin erst im eigentlichen Dispatch, nicht hier.
/// - TUI-lokaler Befehl ohne Operation dahinter → `DeferredUntilTurnEnd`:
///   einen busy-sicheren lokalen Abfang behandelt der Aufrufer vorher
///   (`local_intercept_for` in `app.rs`); was dort nicht abgefangen wird, hat
///   keinen Adapter und kann nur nach dem Turn laufen.
/// - Bekannter Befehl (kanonischer Name oder Alias) →
///   [`BusySubcommand::resolve`] über `spec.busy` und die
///   Unterbefehls-Tabelle `spec.busy_subcommands` mit dem ersten
///   Argument-Token. Die früher hier fest verdrahteten Sonderfälle für
///   `model`/`provider` stehen jetzt als `busy_subcommands` an den
///   `#[operation]`-Deklarationen.
///
/// # Argumente
/// - `registry` (`&CommandRegistry`): der Dispatch-Katalog.
/// - `raw` (`&str`): die abgeschickte Eingabezeile.
///
/// # Rückgabe
/// Die [`BusyAvailability`] dieser Eingabe.
#[must_use]
pub(crate) fn busy_availability_for(registry: &CommandRegistry, raw: &str) -> BusyAvailability {
    let invocation = crate::classify_input(raw);
    // Runde 6, Teil B: `!`-Befehle der Nutzerin laufen sofort, auch während
    // eines Turns (asynchron auf dem Host, `app/operator_shell.rs`).
    if matches!(
        invocation,
        Ok(Invocation::Shell(_) | Invocation::ShellRepeat)
    ) {
        return BusyAvailability::Immediate;
    }
    let Ok(Invocation::Command { name, raw_args }) = invocation else {
        return BusyAvailability::DeferredUntilTurnEnd;
    };

    let Some(spec) = registry.find(&name) else {
        return BusyAvailability::DeferredUntilTurnEnd;
    };

    if spec.origin == crate::CommandOrigin::TuiLocal {
        return BusyAvailability::DeferredUntilTurnEnd;
    }

    BusySubcommand::resolve(
        spec.busy,
        spec.busy_subcommands,
        raw_args.first().map(String::as_str),
    )
}

/// Baut den [`DispatchContext`] der lokalen TUI.
///
/// # Beschreibung
/// Surface ist [`InvocationSurface::Tui`]. Lokale Shell-Befehle sind für die
/// Owner-Konsole standardmäßig verfügbar. `HARW_DISABLE_SHELL=1` deaktiviert sie
/// explizit für den gestarteten Prozess; Channel-Eingänge bleiben davon getrennt
/// und benötigen weiterhin ihre eigene `channel.allow_shell`-Freigabe.
fn tui_dispatch_context(caller_permission: PermissionTier) -> DispatchContext {
    let disabled = std::env::var("HARW_DISABLE_SHELL")
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes"
            )
        })
        .unwrap_or(false);
    DispatchContext {
        caller_tier: caller_permission,
        surface: InvocationSurface::Tui,
        capabilities: if disabled {
            CapabilitySet::default()
        } else {
            CapabilitySet::with(crate::ShellCapability::CommandsShell)
        },
    }
}

/// Formt eine `#notiz`-Zeile in die passende Slash-Zeile um — dieselbe
/// Regel wie `local_commands::intercept` in `app.rs`.
///
/// # Beschreibung
/// `#text` → `/diary note text`, wenn ein `/diary`-Adapter registriert ist,
/// sonst `/memory record text`, wenn ein `/memory`-Adapter existiert. Ohne
/// passenden Adapter, bei leerer Notiz oder für jede andere Zeile `None`:
/// die Zeile läuft dann unverändert weiter (Notiz-Echo).
///
/// `app.rs` fängt `#`/`@` im interaktiven Pfad bereits vor diesem Modul ab;
/// die Umschreibung hier deckt die übrigen Aufrufer ab (etwa synthetische
/// Befehlszeilen), damit eine Notiz nie nur als Echo verpufft. `@rolle`
/// braucht den Chat-Pfad und bleibt hier ein Echo.
fn rewrite_note_line(adapters: &[CommandAdapter], raw_line: &str) -> Option<String> {
    let note = raw_line.trim_start().strip_prefix('#')?.trim();
    if note.is_empty() {
        return None;
    }
    let has = |path: &str| adapters.iter().any(|adapter| adapter.path() == path);
    if has("/diary") {
        Some(format!("/diary note {note}"))
    } else if has("/memory") {
        Some(format!("/memory record {note}"))
    } else {
        None
    }
}

/// Klassifiziert eine Rohzeile und admittiert sie über
/// [`CommandRegistry::dispatch`], ohne sie auszuführen.
///
/// # Beschreibung
/// Aus [`execute_with_context`] herausgelöst (Plan Teil F), damit
/// [`dispatch_command_with_shell_result`] dieselbe Klassifizierungs- und
/// Admission-Logik verwendet, ohne sie zu duplizieren. Verhalten
/// unverändert zum vorherigen Anfang von `execute_with_context`: ein
/// Klassifizierungsfehler liefert `"Eingabe abgelehnt: {error}"`, ein
/// Admission-Fehler den Text aus [`render_admission_error`].
///
/// # Rückgabe
/// `Ok((typed, action))` bei erfolgreicher Admission — `typed` ist die vom
/// Nutzer getippte Form (`/{name}` oder `"!"`) für spätere Fehlermeldungen.
/// `Err(text)` mit dem fertigen Anzeigetext, wenn Klassifizierung oder
/// Admission scheitert.
fn classify_and_admit(
    adapters: &[CommandAdapter],
    context: DispatchContext,
    raw_line: &str,
) -> Result<(String, CommandAction), String> {
    let invocation = match crate::classify_input(raw_line) {
        Ok(invocation) => invocation,
        Err(error) => return Err(format!("Eingabe abgelehnt: {error}")),
    };
    // Die vom Nutzer getippte Form (inkl. Alias) für Fehlermeldungen festhalten,
    // bevor `dispatch` die Invocation konsumiert.
    let typed = match &invocation {
        Invocation::Command { name, .. } => format!("/{name}"),
        Invocation::Shell(_) | Invocation::ShellRepeat => "!".to_owned(),
        Invocation::Note(_) | Invocation::Mention { .. } | Invocation::Chat(_) => String::new(),
    };

    let registry = CommandRegistry::from_command_adapters(adapters);
    match registry.dispatch(context, invocation) {
        Ok(action) => Ok((typed, action)),
        Err(error) => Err(render_admission_error(&typed, &error)),
    }
}

/// Klassifiziert, admittiert (über [`CommandRegistry::dispatch`]) und führt aus.
///
/// # Beschreibung
/// Nutzt [`classify_and_admit`] für Klassifizierung und Admission. Nur
/// `Ok(CommandAction::Command(..))` führt zur Ausführung: der Adapter mit
/// Pfad `/{spec.name}` wird gesucht, erst danach wird `services()`
/// aufgerufen (baut die [`ServiceMap`]) und `CommandAdapter::dispatch`
/// awaitet. Jeder Admission-Fehler wird unverändert als Text
/// zurückgegeben, ohne `services()` aufzurufen oder die Operation
/// anzufassen.
///
/// `CommandAction::Shell` liefert nur noch `display_text` aus
/// [`ShellDisplayOutcome`] (siehe Moduldoc „`!`-Modus wie in Claude Code");
/// `CommandAction::ShellRepeat` kennt hier — mangels Sitzungszustand — keinen
/// vorherigen Befehl und bleibt bei der bisherigen Meldung (siehe Moduldoc).
async fn execute_with_context<F>(
    adapters: &[CommandAdapter],
    sandbox: &SandboxSpec,
    session_id: &SessionId,
    context: DispatchContext,
    raw_line: &str,
    services: F,
) -> String
where
    F: FnOnce() -> ServiceMap,
{
    let rewritten = rewrite_note_line(adapters, raw_line);
    let raw_line = rewritten.as_deref().unwrap_or(raw_line);
    let (typed, action) = match classify_and_admit(adapters, context, raw_line) {
        Ok(pair) => pair,
        Err(text) => return text,
    };

    match action {
        CommandAction::Command(spec, raw_args) => {
            let path = format!("/{}", spec.name.as_str());
            // `from_command_adapters` übernimmt pro Pfad den ersten Adapter; dieselbe
            // Suche hier trifft also genau den Adapter, dessen Stufe admittiert wurde.
            let Some(adapter) = adapters.iter().find(|adapter| adapter.path() == path) else {
                return format!("Unbekannter Command: {typed}");
            };
            // Closure erst hier aufrufen — nach erfolgreicher Admission (§1.2).
            let service_map = services();
            let ctx = OpContext::new(
                session_id.clone(),
                TurnId::new(),
                sandbox.clone(),
                service_map,
            );
            match adapter.dispatch(&ctx, raw_args).await {
                Ok(output) => output.text,
                Err(error) => format!("Fehler: {error}"),
            }
        }
        CommandAction::Shell(command) => execute_shell(sandbox, command).await.display_text,
        CommandAction::ShellRepeat => "Shell-Wiederholung ist noch nicht verfügbar.".to_owned(),
        CommandAction::Note(note) => format!("Notiz: {note}"),
        CommandAction::Mention { target, body } => format!("@{target}: {body}"),
        CommandAction::Chat(text) => text,
    }
}

/// Ergebnis eines tatsächlich ausgeführten `!`/`!!`-Laufs (Plan Teil F):
/// Grundlage für den automatischen Folge-Turn bzw. (während eines Turns)
/// den Kontext des nächsten Turns.
///
/// # Beschreibung
/// Nur gesetzt, wenn der Befehl wirklich auf dem Host gestartet wurde
/// ([`shell_run_outcome`], Runde 6, Teil B) — bei Admission-Fehlern
/// (`HARW_DISABLE_SHELL`, fehlende Capability), einer Ablehnung (`sudo`,
/// leerer Befehl) oder einem Startfehler gibt es kein `ShellRunOutcome`, und
/// es folgt bewusst kein Turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShellRunOutcome {
    /// Der ausgeführte Befehlstext, ohne führendes `!` (bei `!!` der
    /// aufgelöste letzte Befehl der Sitzung).
    pub(crate) command: String,
    /// Exit-Code des Prozesses.
    pub(crate) exit_code: i64,
    /// `stdout` und `stderr` zusammengeführt (auf das Byte-Budget des
    /// Operator-Wegs gekappt) — die Kappung auf 8000 Zeichen erfolgt erst
    /// beim Bau der Folge-Turn-Nachricht in `app.rs`
    /// (`build_shell_turn_message`).
    pub(crate) combined_output: String,
    /// Runde 6, Teil B: Arbeitsverzeichnis auf dem Host (Projektwurzel).
    pub(crate) cwd: std::path::PathBuf,
    /// Runde 6, Teil B: Hinweis zum Ende (Zeitlimit, Kappung), sonst `None`.
    pub(crate) note: Option<String>,
}

/// Rückgabe von [`execute_shell`]: Anzeigetext für die Verlaufszelle
/// (unverändert zum bisherigen `String`-Rückgabewert) plus optionales
/// strukturiertes Ergebnis (Plan Teil F).
struct ShellDisplayOutcome {
    /// Anzeigetext für die Verlaufszelle — identisch zum Rückgabewert vor
    /// Plan Teil F.
    display_text: String,
    /// Gesetzt, wenn der Ausführer tatsächlich ein `ToolOutput::Json` mit
    /// Exit-Code lieferte. Siehe [`ShellRunOutcome`].
    run: Option<ShellRunOutcome>,
}

/// Ergebnis von [`dispatch_command_with_shell_result`]: Anzeigetext für die
/// Verlaufszelle (identisch zu dem, was `execute_command_as` liefern würde)
/// plus optionales strukturiertes Shell-Ergebnis (Plan Teil F).
pub(crate) struct CommandDispatchOutcome {
    /// Anzeigetext für die Verlaufszelle.
    pub(crate) text: String,
    /// Gesetzt genau dann, wenn dieser Dispatch tatsächlich einen
    /// `!`/`!!`-Shell-Befehl ausgeführt hat (nicht bei Admission-Fehlern,
    /// Nicht-Shell-Commands oder `!!` ohne Vorgänger).
    pub(crate) shell: Option<ShellRunOutcome>,
}

/// Klassifiziert, admittiert und führt eine Rohzeile aus — wie
/// [`execute_command_as`], aber mit echter `!!`-Auflösung und
/// strukturiertem Shell-Ergebnis (Plan Teil F).
///
/// # Beschreibung
/// Nutzt dieselbe [`classify_and_admit`]-Admission wie
/// [`execute_with_context`]. `CommandAction::Command`, `Note`, `Mention` und
/// `Chat` verhalten sich identisch zu [`execute_with_context`] (`shell:
/// None`). `CommandAction::Shell(command)` läuft über [`execute_shell`] und
/// gibt dessen `run` unverändert weiter. `CommandAction::ShellRepeat` löst
/// `last_shell_command` auf: `Some(cmd)` führt `cmd` erneut über
/// [`execute_shell`] aus; `None` liefert `"Kein vorheriger !-Befehl"` mit
/// `shell: None` (kein Folge-Turn).
///
/// # Argumente
/// - `last_shell_command` (`Option<&str>`): der zuletzt in dieser Sitzung
///   gelaufene `!`-Befehl (`ChatApp::last_shell_command`), für `!!`.
///
/// # Nebenläufigkeit
/// `async`; ruft `services()` synchron, bevor `CommandAdapter::dispatch`
/// bzw. [`execute_shell`] awaitet wird — wie [`execute_command_as`].
pub(crate) async fn dispatch_command_with_shell_result<F>(
    adapters: &[CommandAdapter],
    sandbox: &SandboxSpec,
    session_id: &SessionId,
    caller_permission: PermissionTier,
    raw_line: &str,
    last_shell_command: Option<&str>,
    services: F,
) -> CommandDispatchOutcome
where
    F: FnOnce() -> ServiceMap,
{
    let context = tui_dispatch_context(caller_permission);
    let rewritten = rewrite_note_line(adapters, raw_line);
    let raw_line = rewritten.as_deref().unwrap_or(raw_line);
    let (typed, action) = match classify_and_admit(adapters, context, raw_line) {
        Ok(pair) => pair,
        Err(text) => return CommandDispatchOutcome { text, shell: None },
    };

    match action {
        CommandAction::Command(spec, raw_args) => {
            let path = format!("/{}", spec.name.as_str());
            let Some(adapter) = adapters.iter().find(|adapter| adapter.path() == path) else {
                return CommandDispatchOutcome {
                    text: format!("Unbekannter Command: {typed}"),
                    shell: None,
                };
            };
            let service_map = services();
            let ctx = OpContext::new(
                session_id.clone(),
                TurnId::new(),
                sandbox.clone(),
                service_map,
            );
            let text = match adapter.dispatch(&ctx, raw_args).await {
                Ok(output) => output.text,
                Err(error) => format!("Fehler: {error}"),
            };
            CommandDispatchOutcome { text, shell: None }
        }
        CommandAction::Shell(command) => {
            let outcome = execute_shell(sandbox, command).await;
            CommandDispatchOutcome {
                text: outcome.display_text,
                shell: outcome.run,
            }
        }
        CommandAction::ShellRepeat => match last_shell_command {
            Some(command) => {
                let outcome = execute_shell(sandbox, command.to_owned()).await;
                CommandDispatchOutcome {
                    text: outcome.display_text,
                    shell: outcome.run,
                }
            }
            None => CommandDispatchOutcome {
                text: "Kein vorheriger !-Befehl".to_owned(),
                shell: None,
            },
        },
        CommandAction::Note(note) => CommandDispatchOutcome {
            text: format!("Notiz: {note}"),
            shell: None,
        },
        CommandAction::Mention { target, body } => CommandDispatchOutcome {
            text: format!("@{target}: {body}"),
            shell: None,
        },
        CommandAction::Chat(text) => CommandDispatchOutcome { text, shell: None },
    }
}

/// Ergebnis der Zulassung einer `!`-Zeile (Runde 6, Teil B).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ShellAdmission {
    /// Keine `!`/`!!`-Zeile — der normale Befehlspfad ist zuständig.
    NotShell,
    /// Zugelassen; der (zugeschnittene, bei `!!` aufgelöste) Befehl soll
    /// auf dem Host laufen.
    Run(String),
    /// Abgelehnt (Capability, Berechtigung, Parser, leerer Befehl, `!!`
    /// ohne Vorgänger); der Text wird angezeigt, es läuft nichts.
    Rejected(String),
}

/// Lässt eine `!`/`!!`-Zeile zu, ohne sie auszuführen (Runde 6, Teil B).
///
/// # Beschreibung
/// Nutzt dieselbe Admission wie jeder andere Dispatch
/// ([`classify_and_admit`] mit [`tui_dispatch_context`], also
/// `HARW_DISABLE_SHELL` und die Operator-Stufe). `!cmd` und `! cmd` sind
/// gleichwertig; der Befehl wird an beiden Enden zugeschnitten. `!!` (auch
/// `! !`) löst gegen `last_shell_command` auf.
///
/// # Argumente
/// - `adapters`: Command-Adapter (für den Katalog).
/// - `caller_permission`: Stufe der Aufruferin.
/// - `raw_line`: die abgeschickte Zeile.
/// - `last_shell_command`: der zuletzt gestartete `!`-Befehl der Sitzung.
///
/// # Rückgabe
/// [`ShellAdmission`].
pub(crate) fn admit_shell_line(
    adapters: &[CommandAdapter],
    caller_permission: PermissionTier,
    raw_line: &str,
    last_shell_command: Option<&str>,
) -> ShellAdmission {
    if !raw_line.starts_with('!') {
        return ShellAdmission::NotShell;
    }
    let context = tui_dispatch_context(caller_permission);
    match classify_and_admit(adapters, context, raw_line) {
        Err(text) => ShellAdmission::Rejected(text),
        Ok((_, CommandAction::Shell(command))) => {
            let command = command.trim();
            if command.is_empty() {
                ShellAdmission::Rejected("Kein Befehl nach `!` angegeben.".to_owned())
            } else {
                ShellAdmission::Run(command.to_owned())
            }
        }
        Ok((_, CommandAction::ShellRepeat)) => match last_shell_command {
            Some(command) => ShellAdmission::Run(command.to_owned()),
            None => ShellAdmission::Rejected("Kein vorheriger !-Befehl".to_owned()),
        },
        Ok(_) => ShellAdmission::NotShell,
    }
}

/// Baut den Operator-Befehl für einen `!`-Befehl (Runde 6, Teil B).
///
/// # Beschreibung
/// `cwd` ist die kanonische Projektwurzel der TUI-Sandbox; die Umgebung wird
/// vollständig geerbt (echtes `HOME`). rlimits: Standard-[`ShellLimits`].
pub(crate) fn operator_command(
    sandbox: &SandboxSpec,
    command: &str,
    timeout: std::time::Duration,
) -> OperatorCommand {
    OperatorCommand::new(command, sandbox.workspace().canonical_root())
        .with_limits(operator_limits())
        .with_timeout(timeout)
}

/// Großzügige rlimits für `!`-Befehle der Nutzerin.
///
/// # Beschreibung
/// Die Vorgaben von [`ShellLimits`] sind für Modellbefehle gedacht; mit 2 GiB
/// Adressraum und 256 Dateideskriptoren scheitern eigene Befehle wie
/// `! cargo build`. Die Nutzerin tippt hier selbst, deshalb gelten nur
/// Schutzgrenzen gegen Ausreißer. Fehlt `prlimit`, läuft der Befehl trotzdem;
/// das Zeitlimit bleibt die harte Grenze.
fn operator_limits() -> ShellLimits {
    const GIB: u64 = 1024 * 1024 * 1024;
    ShellLimits {
        as_bytes: 64 * GIB,
        fsize_bytes: 64 * GIB,
        nofile: 8192,
        nproc: 8192,
        require_rlimits: false,
        ..ShellLimits::default()
    }
}

/// Anzeigetext eines beendeten `!`-Befehls (Runde 6, Teil B).
///
/// # Beschreibung
/// Ablehnung → `Shell-Ausführung abgelehnt: …`, Startfehler →
/// `Shell-Ausführung fehlgeschlagen: …`. Sonst die Ausgabe (stdout, danach
/// `stderr:` und stderr) und eine Abschlusszeile „Shell beendet (Exit-Code
/// N) – auf dem Host ausgeführt, cwd …“ mit Hinweisen auf Zeitlimit bzw.
/// Kappung.
#[must_use]
pub(crate) fn operator_display_text(outcome: &OperatorOutcome) -> String {
    match &outcome.end {
        OperatorEnd::Denied { message } => {
            return format!("Shell-Ausführung abgelehnt: {message}");
        }
        OperatorEnd::Failed { message } if !outcome.executed_on_host => {
            return format!("Shell-Ausführung fehlgeschlagen: {message}");
        }
        _ => {}
    }
    let body = match (outcome.stdout.is_empty(), outcome.stderr.is_empty()) {
        (true, true) => String::new(),
        (false, true) => outcome.stdout.clone(),
        (true, false) => format!("stderr:\n{}", outcome.stderr),
        (false, false) => format!("{}\nstderr:\n{}", outcome.stdout, outcome.stderr),
    };
    let footer = format!(
        "Shell beendet (Exit-Code {}) – auf dem Host ausgeführt, cwd {}{}.",
        outcome.exit_code,
        outcome.cwd.display(),
        operator_note(outcome)
            .map(|note| format!("; {note}"))
            .unwrap_or_default()
    );
    let body = body.trim_end_matches('\n');
    if body.is_empty() {
        footer
    } else {
        format!("{body}\n{footer}")
    }
}

/// Zusatzhinweis zum Ende eines `!`-Befehls (Zeitlimit, Kappung, Fehler).
fn operator_note(outcome: &OperatorOutcome) -> Option<String> {
    match &outcome.end {
        OperatorEnd::TimedOut { timeout_secs } => Some(format!(
            "Zeitlimit {timeout_secs} s erreicht, Prozess beendet"
        )),
        OperatorEnd::OutputLimit => Some("Ausgabe zu groß, gekappt und Prozess beendet".to_owned()),
        OperatorEnd::Cancelled => Some("mit Ctrl+C abgebrochen, Prozess beendet".to_owned()),
        OperatorEnd::Failed { message } => Some(format!("Fehler: {message}")),
        OperatorEnd::Exited if outcome.truncated => Some("Ausgabe gekappt".to_owned()),
        OperatorEnd::Exited | OperatorEnd::Denied { .. } => None,
    }
}

/// Strukturiertes Ergebnis für den Folge-Turn (Runde 6, Teil B).
///
/// # Rückgabe
/// `None`, wenn der Befehl nie gestartet wurde (Ablehnung, Startfehler) —
/// dann gibt es keinen Folge-Turn.
#[must_use]
pub(crate) fn shell_run_outcome(outcome: &OperatorOutcome) -> Option<ShellRunOutcome> {
    if !outcome.executed_on_host {
        return None;
    }
    Some(ShellRunOutcome {
        command: outcome.command.clone(),
        exit_code: outcome.exit_code,
        combined_output: outcome.combined_output(),
        cwd: outcome.cwd.clone(),
        note: operator_note(outcome),
    })
}

/// Führt einen lokalen `!`-Befehl der Nutzerin **auf dem Host** aus
/// (Runde 6, Teil B; vorher Bubblewrap mit flüchtigem `HOME`).
///
/// # Beschreibung
/// Läuft über [`harw_tool_shell::OperatorCommand`] (echtes `HOME`, geerbte
/// Umgebung, `setsid`, rlimits, gekapptes Ergebnis, Zeitlimit
/// [`OPERATOR_DEFAULT_TIMEOUT_SECS`]); `cwd` ist die Projektwurzel der
/// Sandbox. `sudo`/`doas`/`pkexec` bleiben abgelehnt. Die TUI selbst nutzt
/// den asynchronen Weg in `app/operator_shell.rs`; diese Funktion bedient
/// die synchronen Dispatch-Pfade ([`execute_with_context`],
/// [`dispatch_command_with_shell_result`]).
async fn execute_shell(sandbox: &SandboxSpec, command: String) -> ShellDisplayOutcome {
    let outcome = operator_command(
        sandbox,
        &command,
        std::time::Duration::from_secs(OPERATOR_DEFAULT_TIMEOUT_SECS),
    )
    .run()
    .await;
    ShellDisplayOutcome {
        display_text: operator_display_text(&outcome),
        run: shell_run_outcome(&outcome),
    }
}

/// Setzt einen Admission-Fehler aus [`CommandRegistry::dispatch`] in Anzeigetext um.
///
/// # Argumente
/// - `typed` (`&str`): vom Nutzer getippte Form (`/name`, Alias oder `!`).
/// - `error` (`&CommandError`): der Admission-Fehler.
fn render_admission_error(typed: &str, error: &CommandError) -> String {
    match error {
        CommandError::UnknownCommand {
            suggestion: Some(suggestion),
            ..
        } => format!("Unbekannter Command: {typed} (meinten Sie /{suggestion}?)"),
        CommandError::UnknownCommand {
            suggestion: None, ..
        } => format!("Unbekannter Command: {typed}"),
        CommandError::PermissionDenied {
            required, actual, ..
        } => format!(
            "Berechtigung verweigert: {typed} erfordert {required:?}; aktuelle Stufe ist {actual:?}"
        ),
        CommandError::CapabilityDenied { capability } => {
            format!("Shell-Ausführung abgelehnt: Capability '{capability}' ist nicht aktiviert")
        }
        other => format!("Eingabe abgelehnt: {other}"),
    }
}

/// Baut die [`ServiceMap`] für einen einzelnen `CommandAdapter::dispatch`-Aufruf.
///
/// # Beschreibung
/// Die meisten Ops in `harw-ops` benötigen keine Services (leere `ServiceMap`
/// genügt). `HelpOperation` (`harw-ops/src/help.rs`) liest jedoch eine
/// [`OperationRegistry`] aus `ctx.service::<OperationRegistry>()`, um alle
/// registrierten Ops aufzulisten. Da `OperationRegistry` weder `Clone` noch von
/// außen aus einer bestehenden Instanz kopierbar ist, wird hier pro Aufruf eine
/// frische Registry aus den `Arc<dyn Operation>`-Referenzen der übergebenen
/// Adapter rekonstruiert (Operationen werden dedupliziert nach
/// `operation_name()`, damit Ops mit mehreren `Surface::Command`-Einträgen
/// nicht doppelt in `/help` erscheinen).
///
/// Der übergebe `controller` wird als [`SharedSessionController`]
/// (`Arc<dyn SessionController>`) in die Map eingetragen. Da `TypeId` den
/// **statischen** Typ bestimmt, muss der Upcast explizit vor dem `insert`
/// erfolgen — so verwenden alle Calls in `harw-ops` (effort, model, provider),
/// die via `ctx.service::<SharedSessionController>()` auflösen, denselben
/// `TypeId`-Schlüssel.
///
/// # Argumente
/// - `adapters` (`&[CommandAdapter]`): Quelle der zu registrierenden
///   Operationen (via [`CommandAdapter::operation`]).
/// - `runtime_config` (`Option<&Arc<harw_config::ResolvedConfig>>`): Optionaler
///   aufgelöster Laufzeit-Config-Snapshot. Der vorhandene Arc wird für
///   `/model`- und `/provider`-Ops in die `ServiceMap` geklont.
/// - `memory` (`Option<&Arc<dyn harw_memory::Memory>>`): Optionales
///   Memory-Backend für `/memory`-Ops.
/// - `controller` (`&Arc<TuiSessionController>`): Langlebiger Controller aus
///   `ChatApp`; wird als `SharedSessionController` eingetragen, damit der Zustand
///   über mehrere `execute_command`-Aufrufe hinweg erhalten bleibt.
/// - `job_store` (`Option<&Arc<harw_session_store::JobStore>>`): Optionaler
///   dauerhafter Job-Store.
/// - `host_permit_handles` (`Option<&Arc<harw_tool_shell::HostPermitHandles>>`):
///   Optionales Bündel aus Permit-Ledger, Host-Permit-Session-Registry und
///   Fragekanal-Sender (Plan Teil B3/C2), z. B. gebaut aus den
///   `RuntimeAssembly`-Accessoren `host_permit_ledger()`/
///   `host_permit_session_registry()`/`host_permit_prompt_sender()` (gleiche
///   `Arc`-Instanzen, Sender geklont). Vorhanden: derselbe `Arc` wird 1:1 in
///   die `ServiceMap` übernommen, damit `/sandbox-lease` und die
///   `/status`-Zeile ihn über `ctx.service::<Arc<harw_tool_shell::HostPermitHandles>>()`
///   finden.
///
/// # Rückgabe
/// Eine [`ServiceMap`] mit [`OperationRegistry`], [`SharedSessionController`],
/// je einer leeren `AllowRuleSet` und `ExtraRootsCell` (gleiche Fläche wie
/// `RuntimeServices::service_map(ServiceSurface::Slash)`) sowie optionalem
/// `Arc<harw_config::ResolvedConfig>`, `Arc<dyn Memory>` und
/// `Arc<harw_tool_shell::HostPermitHandles>`.
///
/// # Spec
/// harw-tui Design §session_controller — build_services long-lived controller.
///
/// Nur noch `#[cfg(test)]` (W2d-2/CE): Produktionsaufrufer bauen die
/// `ServiceMap` direkt über `runtime_commands::slash_service_map`.
#[cfg(test)]
pub(crate) fn build_services(
    adapters: &[CommandAdapter],
    runtime_config: Option<&Arc<harw_config::ResolvedConfig>>,
    memory: Option<&Arc<dyn harw_memory::Memory>>,
    controller: &Arc<TuiSessionController>,
    job_store: Option<&Arc<harw_session_store::JobStore>>,
    host_permit_handles: Option<&Arc<harw_tool_shell::HostPermitHandles>>,
) -> ServiceMap {
    let mut registry = OperationRegistry::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for adapter in adapters {
        if seen.insert(adapter.operation_name()) {
            registry.register(Arc::clone(adapter.operation()));
        }
    }
    let mut services = ServiceMap::new();
    services.insert(registry);
    if let Some(config) = runtime_config {
        services.insert(Arc::clone(config));
    }
    if let Some(mem) = memory {
        // Als `Arc<dyn Memory>` (nicht als konkreten Typ) registrieren, damit die
        // `/memory`-Op über `ctx.service::<Arc<dyn Memory>>()` fündig wird.
        services.insert(Arc::clone(mem));
    }
    if let Some(store) = job_store {
        services.insert(Arc::clone(store));
    }
    if let Some(handles) = host_permit_handles {
        // Derselbe `Arc<HostPermitHandles>`-Zeiger wandert unverändert in die
        // ServiceMap — kein Neubau aus den einzelnen Feldern, damit `/status`
        // und `/sandbox-lease` exakt denselben Ledger/Registry-Zustand sehen
        // wie die `RuntimeAssembly`, aus der der Aufrufer sie gebaut hat.
        services.insert(Arc::clone(handles));
    }
    // Upcast zu Arc<dyn SessionController> VOR dem insert, damit TypeId::of::<SharedSessionController>()
    // mit dem Schlüssel übereinstimmt, den harw-ops-Handler-Code via
    // `ctx.service::<SharedSessionController>()` nachschlägt.
    // Der explizite `as`-Cast erzwingt den Fat-Pointer-Upcast von Arc<Concrete>
    // zu Arc<dyn SessionController> — Arc::clone würde den konkreten Typ beibehalten.
    let shared: SharedSessionController = Arc::clone(controller) as SharedSessionController;
    services.insert(shared);
    // Gleiche Fläche wie `RuntimeServices::service_map(ServiceSurface::Slash)`
    // (harw-runtime/src/services.rs, `assemble`): alle drei Zellen gehören
    // zur Produktionsmontage dazu — `assemble` trägt
    // `self.parts.approval_mode.clone()` unbedingt auf jeder Fläche ein.
    // Leere, frische Zellen genügen hier — Tests prüfen keinen Regelinhalt,
    // nur dass der Diensttyp auffindbar ist.
    services.insert(harw_extension_api::allow_rules::AllowRuleSet::new());
    services.insert(harw_sandbox::ExtraRootsCell::new());
    services.insert(harw_extension_api::approval_mode::ApprovalModeCell::default());
    services
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_operations::adapter::CommandAdapter;
    use harw_operations::operation::BusyAvailability;
    use harw_operations::registry::OperationRegistry;
    use harw_operations::{
        CommandVisibility, OpContext, OpFuture, OpInput, OpOutput, Operation, OperationCategory,
        OperationDomain, OperationMeta, PermissionTier, Surface,
    };
    use harw_types::{SessionId, TenantId, WorkspaceId};

    use crate::CommandRegistry;
    use crate::session_controller::TuiSessionController;
    use harw_operations::SessionController;

    use super::{CommandServices, build_services, dispatch_command_with_shell_result};
    use crate::test_support::{TestError, TestResult, ctx};

    /// Baut alle 16 `harw-ops`-Adapter über die echte Registrierungsfunktion.
    fn adapters() -> Vec<CommandAdapter> {
        let mut registry = OperationRegistry::new();
        harw_ops::register_all(&mut registry);
        registry
            .iter()
            .flat_map(|op| CommandAdapter::from_operation(Arc::clone(op)))
            .collect()
    }

    /// Baut eine gültige Test-`SandboxSpec` gegen ein eindeutiges Temp-Verzeichnis.
    fn test_sandbox() -> TestResult<(SandboxSpec, PathBuf)> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "harw-tui-command-exec-test-{}-{}",
            std::process::id(),
            id
        ));
        std::fs::create_dir_all(root.join("workspace")).map_err(ctx("temp workspace dir"))?;
        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("tui-test"),
                workspace: WorkspaceId::from_str("workspace"),
                root: PathBuf::from("workspace"),
            }],
        )
        .map_err(ctx("workspace registry build"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("tui-test"),
                &WorkspaceId::from_str("workspace"),
            )
            .map_err(ctx("resolve workspace binding"))?;
        let sandbox = SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy([Permission::ReadWorkspace, Permission::WriteWorkspace]),
        );
        Ok((sandbox, root))
    }

    /// Baut einen frischen langlebigen Test-Controller.
    fn test_controller() -> Arc<TuiSessionController> {
        Arc::new(TuiSessionController::new())
    }

    struct CountingOperation {
        meta: OperationMeta,
        meta_reads: AtomicUsize,
        dispatches: AtomicUsize,
    }

    impl CountingOperation {
        fn protected_alias() -> Self {
            Self {
                meta: OperationMeta {
                    name: "test.protected",
                    summary: "Test-only protected command.",
                    domain: OperationDomain::Misc,
                    permission: PermissionTier::Operator,
                    surfaces: vec![Surface::Command {
                        path: "/protected",
                        visibility: CommandVisibility::TuiOnly,
                    }],
                    aliases: &["guard"],
                    category: OperationCategory::Misc,
                    args_schema: None,
                    // Kein Ausgabeschema: Dieser Test-Helfer prüft nur Dispatch/Meta-Zugriffe,
                    // keine strukturierte Ausgabe (siehe harw-ops/src/lib.rs, help.rs: gleiches Muster).
                    output_schema: None,
                    busy: BusyAvailability::DeferredUntilTurnEnd,
                },
                meta_reads: AtomicUsize::new(0),
                dispatches: AtomicUsize::new(0),
            }
        }
    }

    impl Operation for CountingOperation {
        fn meta(&self) -> &OperationMeta {
            self.meta_reads.fetch_add(1, Ordering::Relaxed);
            &self.meta
        }

        fn run<'a>(&'a self, _ctx: &'a OpContext, _input: OpInput) -> OpFuture<'a> {
            self.dispatches.fetch_add(1, Ordering::Relaxed);
            Box::pin(async {
                Ok(OpOutput {
                    text: "dispatched".to_owned(),
                    data: None,
                })
            })
        }
    }

    // -----------------------------------------------------------------------
    // 1. /help lists all commands via the Operation-Adapter pipeline
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_help_lists_registered_operations() -> TestResult {
        let adapters = adapters();
        let (sandbox, tmp) = test_sandbox()?;
        let session_id = SessionId::new();

        let output = super::execute_command(
            &adapters,
            &sandbox,
            &session_id,
            "/help",
            &CommandServices {
                runtime_config: None,
                memory: None,
                controller: &test_controller(),
                job_store: None,
                host_permit_handles: None,
            },
        )
        .await;
        std::fs::remove_dir_all(tmp).ok();

        for name in &["help", "status", "ps", "stop", "model"] {
            assert!(
                output.contains(name),
                "missing op {name} in help output; got: {output}"
            );
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // 2. /status runs the real StatusOperation and embeds the session id
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_status_embeds_session_id() -> TestResult {
        let adapters = adapters();
        let (sandbox, tmp) = test_sandbox()?;
        let session_id = SessionId::new();

        let output = super::execute_command(
            &adapters,
            &sandbox,
            &session_id,
            "/status",
            &CommandServices {
                runtime_config: None,
                memory: None,
                controller: &test_controller(),
                job_store: None,
                host_permit_handles: None,
            },
        )
        .await;
        std::fs::remove_dir_all(tmp).ok();

        assert!(
            output.contains(session_id.as_str()),
            "expected session id {session_id} in status output; got: {output}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_status_embeds_provider_and_model() -> TestResult {
        let adapters = adapters();
        let (sandbox, tmp) = test_sandbox()?;
        let session_id = SessionId::new();
        let controller = test_controller();
        controller
            .set_active_provider("anthropic".to_owned())
            .map_err(ctx("set_active_provider must succeed"))?;
        controller
            .set_active_model("claude-sonnet".to_owned())
            .map_err(ctx("set_active_model must succeed"))?;

        let output = super::execute_command(
            &adapters,
            &sandbox,
            &session_id,
            "/status",
            &CommandServices {
                runtime_config: None,
                memory: None,
                controller: &controller,
                job_store: None,
                host_permit_handles: None,
            },
        )
        .await;
        std::fs::remove_dir_all(tmp).ok();

        assert!(
            output.contains("Provider: anthropic"),
            "expected provider in status output; got: {output}"
        );
        assert!(
            output.contains("Modell: claude-sonnet"),
            "expected model in status output; got: {output}"
        );
        Ok(())
    }

    // -----------------------------------------------------------------------
    // 3. Unknown command returns an honest "unknown" message
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_unknown_command_returns_honest_message() -> TestResult {
        let adapters = adapters();
        let (sandbox, tmp) = test_sandbox()?;
        let session_id = SessionId::new();

        let output = super::execute_command(
            &adapters,
            &sandbox,
            &session_id,
            "/gibtsnicht",
            &CommandServices {
                runtime_config: None,
                memory: None,
                controller: &test_controller(),
                job_store: None,
                host_permit_handles: None,
            },
        )
        .await;
        std::fs::remove_dir_all(tmp).ok();

        assert_eq!(output, "Unbekannter Command: /gibtsnicht");
        Ok(())
    }

    // -----------------------------------------------------------------------
    // 4. Shell invocation renders the "not yet available" message
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_shell_not_available() -> TestResult {
        let adapters = adapters();
        let (sandbox, tmp) = test_sandbox()?;
        let session_id = SessionId::new();

        let output = super::execute_command(
            &adapters,
            &sandbox,
            &session_id,
            "!ls -la",
            &CommandServices {
                runtime_config: None,
                memory: None,
                controller: &test_controller(),
                job_store: None,
                host_permit_handles: None,
            },
        )
        .await;
        std::fs::remove_dir_all(tmp).ok();

        assert!(
            !output.contains("Capability 'commands.shell' ist nicht aktiviert"),
            "die lokale TUI aktiviert commands.shell standardmäßig: {output}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_shell_repeat_is_not_implemented() -> TestResult {
        let adapters = adapters();
        let (sandbox, tmp) = test_sandbox()?;
        let session_id = SessionId::new();

        let output = super::execute_command(
            &adapters,
            &sandbox,
            &session_id,
            "!!",
            &CommandServices {
                runtime_config: None,
                memory: None,
                controller: &test_controller(),
                job_store: None,
                host_permit_handles: None,
            },
        )
        .await;
        std::fs::remove_dir_all(tmp).ok();

        assert_eq!(output, "Shell-Wiederholung ist noch nicht verfügbar.");
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Plan Teil F: `dispatch_command_with_shell_result` löst `!!` echt auf.
    // -----------------------------------------------------------------------

    /// Ohne einen vorherigen `!`-Befehl liefert `!!` den dokumentierten
    /// Hinweistext und kein `ShellRunOutcome` — `app.rs` darf dann keinen
    /// Folge-Turn starten.
    #[tokio::test]
    async fn dispatch_shell_repeat_without_previous_command_reports_hint() -> TestResult {
        let adapters = adapters();
        let (sandbox, tmp) = test_sandbox()?;
        let session_id = SessionId::new();
        let controller = test_controller();

        let outcome = dispatch_command_with_shell_result(
            &adapters,
            &sandbox,
            &session_id,
            harw_operations::PermissionTier::Owner,
            "!!",
            None,
            || build_services(&adapters, None, None, &controller, None, None),
        )
        .await;
        std::fs::remove_dir_all(tmp).ok();

        assert_eq!(outcome.text, "Kein vorheriger !-Befehl");
        assert!(outcome.shell.is_none());
        Ok(())
    }

    /// Mit einem vorherigen Befehl führt `!!` ihn erneut aus — anders als die
    /// zustandslose `execute_with_context` (siehe
    /// `test_shell_repeat_is_not_implemented`). Runde 6, Teil B: der Befehl
    /// läuft auf dem Host (Projektwurzel als cwd), unabhängig von
    /// `ExecuteProcess` der Sandbox, und liefert ein strukturiertes Ergebnis.
    #[tokio::test]
    async fn dispatch_shell_repeat_with_previous_command_reruns_it() -> TestResult {
        crate::test_support::install_host_port();
        let adapters = adapters();
        let (sandbox, tmp) = test_sandbox()?;
        let session_id = SessionId::new();
        let controller = test_controller();

        let outcome = dispatch_command_with_shell_result(
            &adapters,
            &sandbox,
            &session_id,
            harw_operations::PermissionTier::Owner,
            "!!",
            Some("echo hi"),
            || build_services(&adapters, None, None, &controller, None, None),
        )
        .await;
        std::fs::remove_dir_all(tmp).ok();

        let shell = outcome
            .shell
            .ok_or(TestError::Missing("!! muss erneut ausgeführt werden"))?;
        assert_eq!(shell.command, "echo hi");
        assert_eq!(shell.exit_code, 0, "{shell:?}");
        assert_eq!(shell.combined_output.trim(), "hi");
        assert!(
            outcome.text.contains("auf dem Host ausgeführt"),
            "{}",
            outcome.text
        );
        assert_ne!(
            outcome.text, "Shell-Wiederholung ist noch nicht verfügbar.",
            "dispatch_command_with_shell_result muss !! wirklich auflösen"
        );
        Ok(())
    }

    /// Runde 6, Teil B: `sudo` in einem `!`-Befehl wird vor dem Start
    /// abgelehnt (Hinweis auf das sudo-Fenster) — kein strukturiertes
    /// Ergebnis, also auch kein Folge-Turn.
    #[tokio::test]
    async fn dispatch_shell_command_reports_no_structured_result_when_denied() -> TestResult {
        let adapters: Vec<CommandAdapter> = Vec::new();
        let (sandbox, tmp) = test_sandbox()?;
        let session_id = SessionId::new();
        let controller = test_controller();

        let outcome = dispatch_command_with_shell_result(
            &adapters,
            &sandbox,
            &session_id,
            harw_operations::PermissionTier::Owner,
            "!sudo ls",
            None,
            || build_services(&adapters, None, None, &controller, None, None),
        )
        .await;
        std::fs::remove_dir_all(tmp).ok();

        assert!(outcome.shell.is_none());
        assert!(
            outcome.text.starts_with("Shell-Ausführung abgelehnt:"),
            "{}",
            outcome.text
        );
        assert!(outcome.text.contains("sudo-Fenster"), "{}", outcome.text);
        Ok(())
    }

    #[tokio::test]
    async fn test_execute_with_context_admitted_shell_rejects_sudo() -> TestResult {
        let adapters = adapters();
        let (sandbox, tmp) = test_sandbox()?;
        let session_id = SessionId::new();
        let controller = test_controller();
        let context = crate::DispatchContext {
            caller_tier: PermissionTier::Operator,
            surface: crate::InvocationSurface::Tui,
            capabilities: crate::CapabilitySet::with(crate::ShellCapability::CommandsShell),
        };
        let services = CommandServices {
            runtime_config: None,
            memory: None,
            controller: &controller,
            job_store: None,
            host_permit_handles: None,
        };

        let shell = super::execute_with_context(
            &adapters,
            &sandbox,
            &session_id,
            context,
            "!sudo ls -la",
            || {
                build_services(
                    &adapters,
                    services.runtime_config,
                    services.memory,
                    services.controller,
                    services.job_store,
                    services.host_permit_handles,
                )
            },
        )
        .await;
        let repeat =
            super::execute_with_context(&adapters, &sandbox, &session_id, context, "!!", || {
                build_services(
                    &adapters,
                    services.runtime_config,
                    services.memory,
                    services.controller,
                    services.job_store,
                    services.host_permit_handles,
                )
            })
            .await;
        std::fs::remove_dir_all(tmp).ok();

        // Runde 6, Teil B: `!` läuft auf dem Host; `sudo` bleibt dort vor
        // dem Start abgelehnt (Hinweis auf das sudo-Fenster).
        assert!(shell.starts_with("Shell-Ausführung abgelehnt:"), "{shell}");
        assert!(shell.contains("sudo-Fenster"), "{shell}");
        assert_eq!(repeat, "Shell-Wiederholung ist noch nicht verfügbar.");
        Ok(())
    }

    // -----------------------------------------------------------------------
    // 5. Plain text is passed through as chat
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_plain_text_is_chat() -> TestResult {
        let adapters = adapters();
        let (sandbox, tmp) = test_sandbox()?;
        let session_id = SessionId::new();

        let output = super::execute_command(
            &adapters,
            &sandbox,
            &session_id,
            "hallo welt",
            &CommandServices {
                runtime_config: None,
                memory: None,
                controller: &test_controller(),
                job_store: None,
                host_permit_handles: None,
            },
        )
        .await;
        std::fs::remove_dir_all(tmp).ok();

        assert_eq!(output, "hallo welt");
        Ok(())
    }

    // -----------------------------------------------------------------------
    // 6. Note and mention prefixes render as before
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_note_prefix_renders_note() -> TestResult {
        let adapters = adapters();
        let (sandbox, tmp) = test_sandbox()?;
        let session_id = SessionId::new();

        let output = super::execute_command(
            &adapters,
            &sandbox,
            &session_id,
            "#this is a note",
            &CommandServices {
                runtime_config: None,
                memory: None,
                controller: &test_controller(),
                job_store: None,
                host_permit_handles: None,
            },
        )
        .await;
        std::fs::remove_dir_all(tmp).ok();

        // `#` wird zu einer Gedächtnis-/Tagebuch-Befehlszeile umgeschrieben
        // und nicht mehr nur als Echo gezeigt.
        assert_ne!(output, "Notiz: this is a note");
        Ok(())
    }

    #[test]
    fn test_rewrite_note_line_prefers_diary_then_memory() {
        let adapters = adapters();
        let has_diary = adapters.iter().any(|adapter| adapter.path() == "/diary");
        let rewritten = super::rewrite_note_line(&adapters, "  # hallo welt ");
        let expected = if has_diary {
            "/diary note hallo welt"
        } else {
            "/memory record hallo welt"
        };
        assert_eq!(rewritten.as_deref(), Some(expected));
        assert_eq!(super::rewrite_note_line(&adapters, "#   "), None);
        assert_eq!(super::rewrite_note_line(&adapters, "/status"), None);
        assert_eq!(super::rewrite_note_line(&[], "#notiz"), None);
    }

    #[tokio::test]
    async fn test_mention_renders_correctly() -> TestResult {
        let adapters = adapters();
        let (sandbox, tmp) = test_sandbox()?;
        let session_id = SessionId::new();

        let output = super::execute_command(
            &adapters,
            &sandbox,
            &session_id,
            "@alice hello there",
            &CommandServices {
                runtime_config: None,
                memory: None,
                controller: &test_controller(),
                job_store: None,
                host_permit_handles: None,
            },
        )
        .await;
        std::fs::remove_dir_all(tmp).ok();

        assert_eq!(output, "@alice: hello there");
        Ok(())
    }

    // -----------------------------------------------------------------------
    // 7. /model list dispatches raw_args through to the operation
    //    The source-local owner wrapper still exercises its Owner tier.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn owner_wrapper_dispatches_operator_command_without_error() -> TestResult {
        let adapters = adapters();
        let (sandbox, tmp) = test_sandbox()?;
        let session_id = SessionId::new();

        let output = super::execute_command(
            &adapters,
            &sandbox,
            &session_id,
            "/model list",
            &CommandServices {
                runtime_config: None,
                memory: None,
                controller: &test_controller(),
                job_store: None,
                host_permit_handles: None,
            },
        )
        .await;
        std::fs::remove_dir_all(tmp).ok();

        assert!(
            !output.starts_with("Unbekannter Command"),
            "/model must be a known command; got: {output}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn observer_cannot_dispatch_an_operator_command() -> TestResult {
        let adapters = adapters();
        let (sandbox, tmp) = test_sandbox()?;
        let session_id = SessionId::new();

        let controller = test_controller();
        let output = super::execute_command_as(
            &adapters,
            &sandbox,
            &session_id,
            harw_operations::PermissionTier::Observer,
            "/model list",
            || build_services(&adapters, None, None, &controller, None, None),
        )
        .await;
        std::fs::remove_dir_all(tmp).ok();

        assert_eq!(
            output,
            "Berechtigung verweigert: /model erfordert Operator; aktuelle Stufe ist Observer"
        );
        Ok(())
    }

    #[tokio::test]
    async fn denied_alias_does_not_build_services_or_dispatch() -> TestResult {
        let operation = Arc::new(CountingOperation::protected_alias());
        let adapters = CommandAdapter::from_operation(operation.clone());
        operation.meta_reads.store(0, Ordering::Relaxed);

        let (sandbox, tmp) = test_sandbox()?;
        let controller = test_controller();
        let output = super::execute_command_as(
            &adapters,
            &sandbox,
            &SessionId::new(),
            PermissionTier::Observer,
            "/guard",
            || build_services(&adapters, None, None, &controller, None, None),
        )
        .await;
        std::fs::remove_dir_all(tmp).ok();

        assert_eq!(
            output,
            "Berechtigung verweigert: /guard erfordert Operator; aktuelle Stufe ist Observer"
        );
        assert_eq!(
            operation.meta_reads.load(Ordering::Relaxed),
            1,
            "denied alias lookup may inspect metadata once, but must not build services"
        );
        assert_eq!(
            operation.dispatches.load(Ordering::Relaxed),
            0,
            "a denied alias must not dispatch its operation"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_execute_command_as_observer_cannot_run_operator_command() -> TestResult {
        let operation = Arc::new(CountingOperation::protected_alias());
        let adapters = CommandAdapter::from_operation(operation.clone());
        let (sandbox, tmp) = test_sandbox()?;
        let session_id = SessionId::new();
        let controller = test_controller();

        let denied = super::execute_command_as(
            &adapters,
            &sandbox,
            &session_id,
            PermissionTier::Observer,
            "/protected",
            || build_services(&adapters, None, None, &controller, None, None),
        )
        .await;
        assert_eq!(
            denied,
            "Berechtigung verweigert: /protected erfordert Operator; aktuelle Stufe ist Observer"
        );
        assert_eq!(
            operation.dispatches.load(Ordering::Relaxed),
            0,
            "a denied canonical command must not dispatch its operation"
        );

        let admitted = super::execute_command_as(
            &adapters,
            &sandbox,
            &session_id,
            PermissionTier::Operator,
            "/protected",
            || build_services(&adapters, None, None, &controller, None, None),
        )
        .await;
        std::fs::remove_dir_all(tmp).ok();

        assert_eq!(admitted, "dispatched");
        assert_eq!(operation.dispatches.load(Ordering::Relaxed), 1);
        Ok(())
    }

    #[tokio::test]
    async fn test_execute_command_as_unknown_command_suggests_nearest() -> TestResult {
        let adapters = adapters();
        let (sandbox, tmp) = test_sandbox()?;
        let session_id = SessionId::new();

        let controller = test_controller();
        let output = super::execute_command_as(
            &adapters,
            &sandbox,
            &session_id,
            PermissionTier::Operator,
            "/stauts",
            || build_services(&adapters, None, None, &controller, None, None),
        )
        .await;
        std::fs::remove_dir_all(tmp).ok();

        // `stauts` (Länge 6) → gleiche Anfangsbuchstaben-Kandidaten mit minimaler
        // Längendifferenz; `status` ist in `register_all` vor `skills` registriert.
        assert_eq!(
            output,
            "Unbekannter Command: /stauts (meinten Sie /status?)"
        );
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Closure discipline: services() must not be evaluated on denied admission
    // -----------------------------------------------------------------------

    /// `test_execute_command_as_does_not_build_services_on_denied_admission`:
    /// Denied admission (`Observer` calling an `Operator`-tier command) must
    /// never evaluate the `services` closure. A counter incremented inside the
    /// closure stays at `0` after a denied call and only becomes `1` once the
    /// same call is admitted (Operator tier) — proving the closure runs
    /// exactly at, and only at, the point `execute_with_context` reaches
    /// `CommandAction::Command`.
    #[tokio::test]
    async fn test_execute_command_as_does_not_build_services_on_denied_admission() -> TestResult {
        let adapters = adapters();
        let (sandbox, tmp) = test_sandbox()?;
        let session_id = SessionId::new();
        let controller = test_controller();
        let calls = AtomicUsize::new(0);

        let denied = super::execute_command_as(
            &adapters,
            &sandbox,
            &session_id,
            PermissionTier::Observer,
            "/model list",
            || {
                calls.fetch_add(1, Ordering::Relaxed);
                build_services(&adapters, None, None, &controller, None, None)
            },
        )
        .await;

        assert_eq!(
            denied,
            "Berechtigung verweigert: /model erfordert Operator; aktuelle Stufe ist Observer"
        );
        assert_eq!(
            calls.load(Ordering::Relaxed),
            0,
            "services() must not be evaluated when admission is denied"
        );

        let admitted = super::execute_command_as(
            &adapters,
            &sandbox,
            &session_id,
            PermissionTier::Operator,
            "/model list",
            || {
                calls.fetch_add(1, Ordering::Relaxed);
                build_services(&adapters, None, None, &controller, None, None)
            },
        )
        .await;
        std::fs::remove_dir_all(tmp).ok();

        assert!(
            !admitted.starts_with("Unbekannter Command") && !admitted.starts_with("Berechtigung"),
            "admitted call must actually dispatch; got: {admitted}"
        );
        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "services() must be evaluated exactly once, after successful admission"
        );
        Ok(())
    }

    // -----------------------------------------------------------------------
    // 8. Empty adapter list is honest about missing commands
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_empty_adapters_reports_unknown_for_any_command() -> TestResult {
        let (sandbox, tmp) = test_sandbox()?;
        let session_id = SessionId::new();

        let output = super::execute_command(
            &[],
            &sandbox,
            &session_id,
            "/status",
            &CommandServices {
                runtime_config: None,
                memory: None,
                controller: &test_controller(),
                job_store: None,
                host_permit_handles: None,
            },
        )
        .await;
        std::fs::remove_dir_all(tmp).ok();

        assert_eq!(output, "Unbekannter Command: /status");
        Ok(())
    }

    // -----------------------------------------------------------------------
    // 9. Alias dispatches to the same operation as the canonical command
    //    Proves that the adapter list is the single source of truth:
    //    no separate alias table is consulted.
    // -----------------------------------------------------------------------

    /// `alias_dispatches_to_same_handler_as_canonical`:
    /// Executes `/m` (alias) and `/model` (canonical) and asserts both produce
    /// non-"unknown" output, meaning both routes reach the same Operation.
    /// The test also verifies that the `CommandRegistry` and the adapter
    /// resolve to the same canonical spec name, confirming a single truth source.
    #[tokio::test]
    async fn alias_dispatches_to_same_handler_as_canonical() -> TestResult {
        let adapters = adapters();
        let (sandbox, tmp) = test_sandbox()?;
        let session_id = SessionId::new();
        let controller = test_controller();

        // Both the alias and the canonical form must reach the model operation.
        let output_alias = super::execute_command(
            &adapters,
            &sandbox,
            &session_id,
            "/m",
            &CommandServices {
                runtime_config: None,
                memory: None,
                controller: &controller,
                job_store: None,
                host_permit_handles: None,
            },
        )
        .await;
        let output_canonical = super::execute_command(
            &adapters,
            &sandbox,
            &session_id,
            "/model",
            &CommandServices {
                runtime_config: None,
                memory: None,
                controller: &controller,
                job_store: None,
                host_permit_handles: None,
            },
        )
        .await;
        std::fs::remove_dir_all(tmp).ok();

        assert!(
            !output_alias.starts_with("Unbekannter Command"),
            "alias '/m' must resolve to the model operation, got: {output_alias}"
        );
        assert!(
            !output_canonical.starts_with("Unbekannter Command"),
            "canonical '/model' must resolve to the model operation, got: {output_canonical}"
        );

        // Verify that CommandRegistry (the discovery/autocomplete side) and the
        // adapter lookup (the execution side) agree on the same canonical name.
        let registry = CommandRegistry::built_in().map_err(ctx("built_in"))?;
        let spec_via_alias = registry.find("m").ok_or(TestError::Missing(
            "'m' must be findable in CommandRegistry",
        ))?;
        let spec_via_canonical = registry.find("model").ok_or(TestError::Missing(
            "'model' must be findable in CommandRegistry",
        ))?;
        assert_eq!(
            spec_via_alias.name.as_str(),
            spec_via_canonical.name.as_str(),
            "CommandRegistry.find('m') and CommandRegistry.find('model') \
             must map to the same canonical spec name"
        );

        // Cross-check: the adapter that handles the alias must be the model
        // operation — verify by canonical path.
        let model_adapter = adapters
            .iter()
            .find(|a| a.path() == "/model")
            .ok_or(TestError::Missing("a /model adapter must exist"))?;
        let alias_resolves_same_op = model_adapter.operation().meta().aliases.contains(&"m");
        assert!(
            alias_resolves_same_op,
            "the /model adapter's OperationMeta must declare 'm' as an alias"
        );
        Ok(())
    }

    // -----------------------------------------------------------------------
    // 10. Long-lived controller survives two build_services calls
    //     Proves that Arc + ServiceMap no longer clobbers state between commands.
    // -----------------------------------------------------------------------

    /// `long_lived_controller_survives_two_build_services_calls`:
    /// Baut einen `Arc<TuiSessionController>`, setzt `reasoning_effort` auf `High`,
    /// ruft `build_services` zweimal auf und prüft, dass der Wert nach beiden Aufrufen
    /// erhalten bleibt — d.h. kein frischer Controller mehr pro Aufruf erzeugt wird.
    #[test]
    fn test_long_lived_controller_survives_two_build_services_calls() -> TestResult {
        use harw_operations::{SessionController, SharedSessionController};
        use harw_types::ReasoningEffort;

        let adapters = adapters();
        let controller = Arc::new(TuiSessionController::new());

        // Setze einen Nicht-Default-Effort auf den langlebigen Controller.
        controller
            .set_reasoning_effort(Some(ReasoningEffort::High))
            .map_err(ctx("set_reasoning_effort must succeed"))?;

        // Erster build_services-Aufruf — klont den Controller-Arc in die ServiceMap.
        let services1 = super::build_services(&adapters, None, None, &controller, None, None);
        // Abruf via SharedSessionController-TypeId (Arc<dyn SessionController>).
        let retrieved1 = services1
            .get::<SharedSessionController>()
            .ok_or(TestError::Missing(
                "SharedSessionController must be in ServiceMap after first call",
            ))?;
        assert_eq!(
            retrieved1.snapshot().reasoning_effort,
            Some(ReasoningEffort::High),
            "first build_services call must expose the pre-set reasoning effort"
        );

        // Zweiter build_services-Aufruf — derselbe Arc, keine neue Allokation.
        let services2 = super::build_services(&adapters, None, None, &controller, None, None);
        let retrieved2 = services2
            .get::<SharedSessionController>()
            .ok_or(TestError::Missing(
                "SharedSessionController must be in ServiceMap after second call",
            ))?;
        assert_eq!(
            retrieved2.snapshot().reasoning_effort,
            Some(ReasoningEffort::High),
            "second build_services call must still see the original reasoning effort — \
             the long-lived Arc was not replaced by a fresh controller"
        );
        Ok(())
    }

    // -----------------------------------------------------------------------
    // 11. Resolved runtime config keeps its composition-root Arc identity
    // -----------------------------------------------------------------------

    #[test]
    fn test_build_services_preserves_resolved_config_arc() -> TestResult {
        let adapters = adapters();
        let controller = test_controller();
        let runtime_config = Arc::new(harw_config::ResolvedConfig::default());

        let services = super::build_services(
            &adapters,
            Some(&runtime_config),
            None,
            &controller,
            None,
            None,
        );
        let retrieved =
            services
                .get::<Arc<harw_config::ResolvedConfig>>()
                .ok_or(TestError::Missing(
                    "ResolvedConfig Arc must be present when supplied",
                ))?;

        assert!(
            Arc::ptr_eq(retrieved, &runtime_config),
            "ServiceMap must retain the exact Arc resolved by the composition root"
        );
        Ok(())
    }

    #[test]
    fn build_services_preserves_durable_job_store_arc() -> TestResult {
        let adapters = adapters();
        let controller = test_controller();
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = Arc::new(harw_session_store::JobStore::new(temp.path()));

        let services =
            super::build_services(&adapters, None, None, &controller, Some(&store), None);
        let resolved =
            services
                .get::<Arc<harw_session_store::JobStore>>()
                .ok_or(TestError::Missing(
                    "durable job store must be available to command operations",
                ))?;

        assert!(Arc::ptr_eq(resolved, &store));
        Ok(())
    }

    // -----------------------------------------------------------------------
    // 12. dispatch_slash_command: no-runtime message + happy path
    // -----------------------------------------------------------------------

    /// `dispatch_slash_command_reports_the_idle_no_runtime_message`: mirrors
    /// the `app.rs` `HarwEvent::Command`-Zweig's "no runtime" branch verbatim.
    #[tokio::test]
    async fn dispatch_slash_command_reports_the_idle_no_runtime_message() -> TestResult {
        let adapters = adapters();
        let (sandbox, tmp) = test_sandbox()?;
        let session_id = SessionId::new();

        let output =
            super::dispatch_slash_command(None, &adapters, &sandbox, &session_id, "/status").await;
        std::fs::remove_dir_all(tmp).ok();

        assert_eq!(output, "Fehler: keine Runtime-Montage");
        Ok(())
    }

    /// `dispatch_slash_command_with_empty_adapters_reports_unknown`: without a
    /// `RuntimeAssembly` in scope for this unit test, exercise the
    /// runtime-independent branches through `execute_command_as` directly to
    /// prove they still agree (an empty adapter list is honest about missing
    /// commands, same as the idle path).
    #[tokio::test]
    async fn dispatch_slash_command_and_execute_command_as_agree_on_unknown_command() -> TestResult
    {
        let (sandbox, tmp) = test_sandbox()?;
        let session_id = SessionId::new();
        let controller = test_controller();

        let via_execute_command_as = super::execute_command_as(
            &[],
            &sandbox,
            &session_id,
            PermissionTier::Operator,
            "/status",
            || build_services(&[], None, None, &controller, None, None),
        )
        .await;
        std::fs::remove_dir_all(tmp).ok();

        assert_eq!(via_execute_command_as, "Unbekannter Command: /status");
        Ok(())
    }

    // -----------------------------------------------------------------------
    // 13. busy_availability_for classifier
    // -----------------------------------------------------------------------

    fn built_in_registry() -> TestResult<CommandRegistry> {
        CommandRegistry::built_in().map_err(ctx("built_in"))
    }

    /// Runde 6, Teil B: `!`/`!!` laufen während eines Turns sofort.
    #[test]
    fn busy_availability_for_bang_is_immediate() -> TestResult {
        let registry = built_in_registry()?;
        for raw in ["!ls", "! ls", "!!", "! !"] {
            assert_eq!(
                super::busy_availability_for(&registry, raw),
                BusyAvailability::Immediate,
                "{raw}"
            );
        }
        Ok(())
    }

    /// Runde 6, Teil B: `!cmd` und `! cmd` werden gleich zugelassen und
    /// zugeschnitten; `! !` wiederholt; leer und `!!` ohne Vorgänger werden
    /// abgelehnt; Nicht-`!`-Zeilen bleiben beim normalen Pfad.
    #[test]
    fn admit_shell_line_normalizes_and_resolves_repeat() {
        use super::{ShellAdmission, admit_shell_line};
        let adapters: Vec<CommandAdapter> = Vec::new();
        let tier = PermissionTier::Operator;
        let run = |cmd: &str| ShellAdmission::Run(cmd.to_owned());
        assert_eq!(
            admit_shell_line(&adapters, tier, "!ls -la", None),
            run("ls -la")
        );
        assert_eq!(
            admit_shell_line(&adapters, tier, "! ls -la ", None),
            run("ls -la")
        );
        assert_eq!(
            admit_shell_line(&adapters, tier, "! !", Some("pwd")),
            run("pwd")
        );
        assert_eq!(
            admit_shell_line(&adapters, tier, "!!", Some("pwd")),
            run("pwd")
        );
        assert!(matches!(
            admit_shell_line(&adapters, tier, "!!", None),
            ShellAdmission::Rejected(_)
        ));
        assert!(matches!(
            admit_shell_line(&adapters, tier, "! ", None),
            ShellAdmission::Rejected(_)
        ));
        assert!(matches!(
            admit_shell_line(&adapters, PermissionTier::Observer, "!ls", None),
            ShellAdmission::Rejected(_)
        ));
        assert_eq!(
            admit_shell_line(&adapters, tier, "/status", None),
            ShellAdmission::NotShell
        );
        assert_eq!(
            admit_shell_line(&adapters, tier, "\\!ls", None),
            ShellAdmission::NotShell
        );
    }

    #[test]
    fn busy_availability_for_status_is_immediate() -> TestResult {
        assert_eq!(
            super::busy_availability_for(&built_in_registry()?, "/status"),
            BusyAvailability::Immediate
        );
        Ok(())
    }

    #[test]
    fn busy_availability_for_mode_plan_is_staged() -> TestResult {
        assert_eq!(
            super::busy_availability_for(&built_in_registry()?, "/mode plan"),
            BusyAvailability::Staged
        );
        Ok(())
    }

    #[test]
    fn busy_availability_for_model_show_is_immediate() -> TestResult {
        assert_eq!(
            super::busy_availability_for(&built_in_registry()?, "/model show"),
            BusyAvailability::Immediate
        );
        Ok(())
    }

    #[test]
    fn busy_availability_for_model_switch_with_argument_is_staged() -> TestResult {
        assert_eq!(
            super::busy_availability_for(&built_in_registry()?, "/model switch x"),
            BusyAvailability::Staged
        );
        Ok(())
    }

    #[test]
    fn busy_availability_for_bare_model_is_staged() -> TestResult {
        // Getippt öffnet bare `/model` den Picker (lokaler Abfang in `app.rs`);
        // die Klasse der Operation selbst ist `Staged`.
        assert_eq!(
            super::busy_availability_for(&built_in_registry()?, "/model"),
            BusyAvailability::Staged
        );
        Ok(())
    }

    #[test]
    fn busy_availability_for_bare_provider_is_immediate() -> TestResult {
        assert_eq!(
            super::busy_availability_for(&built_in_registry()?, "/provider"),
            BusyAvailability::Immediate
        );
        Ok(())
    }

    #[test]
    fn busy_availability_for_provider_list_is_immediate() -> TestResult {
        assert_eq!(
            super::busy_availability_for(&built_in_registry()?, "/provider list"),
            BusyAvailability::Immediate
        );
        Ok(())
    }

    #[test]
    fn busy_availability_for_provider_test_is_deferred() -> TestResult {
        assert_eq!(
            super::busy_availability_for(&built_in_registry()?, "/provider test"),
            BusyAvailability::DeferredUntilTurnEnd
        );
        Ok(())
    }

    #[test]
    fn busy_availability_for_unknown_command_is_deferred() -> TestResult {
        assert_eq!(
            super::busy_availability_for(&built_in_registry()?, "/gibtsnicht"),
            BusyAvailability::DeferredUntilTurnEnd
        );
        Ok(())
    }

    #[test]
    fn busy_availability_for_chat_text_is_deferred() -> TestResult {
        assert_eq!(
            super::busy_availability_for(&built_in_registry()?, "hallo welt"),
            BusyAvailability::DeferredUntilTurnEnd
        );
        Ok(())
    }

    /// `busy_availability_for_alias_of_an_immediate_command_is_immediate`: an
    /// alias must resolve to the same canonical spec (and thus the same
    /// `busy` flag plus the same `model`/`provider` sub-command rule) as the
    /// canonical command name. `/p` is `/provider`'s alias; typed bare it
    /// must classify identically to bare `/provider` (`Immediate`, defaults
    /// to `show`) because [`super::busy_availability_for`] matches on
    /// `spec.name` (the canonical name), not on the typed token.
    #[test]
    fn busy_availability_for_alias_of_an_immediate_command_is_immediate() -> TestResult {
        let registry = built_in_registry()?;
        let canonical = registry
            .find("provider")
            .ok_or(TestError::Missing("provider must be registered"))?;
        let alias = canonical
            .aliases
            .first()
            .cloned()
            .ok_or(TestError::Missing(
                "/provider must declare at least one alias for this test to be meaningful",
            ))?;

        assert_eq!(
            super::busy_availability_for(&registry, &format!("/{alias}")),
            BusyAvailability::Immediate
        );
        Ok(())
    }

    /// Kurzform einer Busy-Klasse für die Tabelle unten.
    fn busy_class_name(class: BusyAvailability) -> &'static str {
        match class {
            BusyAvailability::Immediate => "immediate",
            BusyAvailability::Staged => "staged",
            BusyAvailability::DeferredUntilTurnEnd => "deferred",
        }
    }

    /// Erwartete Busy-Klasse **jedes** Befehls im vollständigen TUI-Katalog
    /// (Operationen plus TUI-lokale Befehle): `(name, klasse, unterbefehle)`,
    /// `unterbefehle` im Format `sub=klasse` (`-` = bare Form). Jede
    /// Abweichung — auch ein neuer Befehl ohne Eintrag — bricht die CI
    /// (Runde 4, Teil H).
    const EXPECTED_BUSY_CLASSES: &[(&str, &str, &str)] = &[
        ("add-workdir", "deferred", ""),
        // Runde 5, Teil K: `/agent bg` und `/agent cancel <id>` (TUI-lokal,
        // Hintergrund-Agenten) erben `immediate`.
        ("agent", "immediate", ""),
        // Runde 5, Teil I: `/agents` entfällt (nur noch `/agent`; `/agent
        // stream <modus>` erbt dessen `immediate`).
        ("approve", "immediate", ""),
        ("attach", "immediate", ""),
        // Runde 5, Teil L: `/btw` läuft neben dem Turn (lokaler Befehl).
        ("btw", "immediate", ""),
        ("bug-report", "deferred", ""),
        ("cancel", "immediate", ""),
        ("clear", "deferred", ""),
        ("compact", "deferred", ""),
        ("context-proposal", "deferred", ""),
        ("deny", "immediate", ""),
        (
            "diary",
            "deferred",
            "-=immediate,show=immediate,today=immediate,search=immediate,agents=immediate",
        ),
        ("diff", "immediate", ""),
        (
            "dream",
            "deferred",
            "-=immediate,list=immediate,show=immediate,status=immediate",
        ),
        ("effort", "staged", "-=immediate,show=immediate"),
        ("exit", "deferred", ""),
        ("export", "deferred", ""),
        ("help", "immediate", ""),
        // Plan R9, Teil F: `/jobs` (lesen, stoppen) läuft sofort.
        ("jobs", "immediate", ""),
        (
            "kanban",
            "deferred",
            "-=immediate,list=immediate,show=immediate,boards=immediate",
        ),
        ("keys", "immediate", ""),
        ("learn", "deferred", ""),
        ("matrix", "deferred", "show=immediate,list=immediate"),
        ("memory", "deferred", ""),
        ("mode", "staged", "-=immediate,show=immediate"),
        ("model", "staged", "show=immediate,list=immediate"),
        ("models", "immediate", ""),
        ("new", "deferred", ""),
        (
            "palace",
            "deferred",
            "-=immediate,list=immediate,show=immediate,search=immediate",
        ),
        // Runde 5, Teil E: `rules`/`log` sind lesend und laufen sofort.
        (
            "permissions",
            "deferred",
            "-=immediate,show=immediate,mode=immediate,set=immediate,rules=immediate,log=immediate",
        ),
        // Runde 5, Teil F: lokale Ersatz-Spezifikation (Plan-Modus an,
        // `/plan show|list|open`; `/plan edit` wartet als lokaler Abfang).
        ("plan", "immediate", ""),
        (
            "plugins",
            "immediate",
            "install=deferred,activate=deferred,uninstall=deferred",
        ),
        ("provider", "immediate", "test=deferred"),
        ("provider-concurrency", "immediate", ""),
        ("ps", "immediate", ""),
        ("quit", "deferred", ""),
        ("rename", "immediate", ""),
        ("resume", "deferred", ""),
        ("retry", "deferred", ""),
        ("review", "immediate", ""),
        ("sandbox-lease", "immediate", ""),
        ("sessions", "deferred", ""),
        (
            "skills",
            "deferred",
            "-=immediate,list=immediate,show=immediate",
        ),
        ("status", "immediate", ""),
        ("stop", "immediate", ""),
        ("tools", "deferred", ""),
        ("uia-effort", "staged", "-=immediate,show=immediate"),
        (
            "uia-model",
            "staged",
            "-=immediate,show=immediate,list=immediate",
        ),
        (
            "uia-provider",
            "staged",
            "-=immediate,show=immediate,list=immediate,test=deferred",
        ),
        (
            "uia-worker-model",
            "staged",
            "-=immediate,show=immediate,list=immediate",
        ),
        ("usage", "immediate", ""),
        ("verbose", "immediate", ""),
        ("whoami", "immediate", ""),
        ("work", "immediate", ""),
        ("workbench", "deferred", "-=immediate,show=immediate"),
    ];

    #[test]
    fn busy_class_table_covers_every_registered_command() -> TestResult {
        let registry =
            built_in_registry()?.with_local_specs(crate::command_catalog::local_command_specs());
        let mut actual: Vec<(String, &'static str, String)> = registry
            .specs()
            .iter()
            .map(|spec| {
                let subs = spec
                    .busy_subcommands
                    .iter()
                    .map(|entry| {
                        format!(
                            "{}={}",
                            entry.subcommand.unwrap_or("-"),
                            busy_class_name(entry.busy)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(",");
                (
                    spec.name.as_str().to_owned(),
                    busy_class_name(spec.busy),
                    subs,
                )
            })
            .collect();
        actual.sort();
        let mut expected: Vec<(String, &'static str, String)> = EXPECTED_BUSY_CLASSES
            .iter()
            .map(|(name, class, subs)| ((*name).to_owned(), *class, (*subs).to_owned()))
            .collect();
        expected.sort();
        let rendered = actual
            .iter()
            .map(|(name, class, subs)| format!("        (\"{name}\", \"{class}\", \"{subs}\"),"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            actual, expected,
            "Busy-Klassen weichen ab; aktueller Stand:\n{rendered}"
        );
        Ok(())
    }

    /// Stichproben über den echten Klassifizierer (inkl. Unterbefehlen).
    #[test]
    fn busy_availability_for_resolves_subcommand_tables() -> TestResult {
        let registry =
            built_in_registry()?.with_local_specs(crate::command_catalog::local_command_specs());
        let cases: &[(&str, BusyAvailability)] = &[
            ("/permissions", BusyAvailability::Immediate),
            ("/permissions mode auto", BusyAvailability::Immediate),
            ("/permissions log", BusyAvailability::Immediate),
            ("/permissions rules", BusyAvailability::Immediate),
            (
                "/permissions allow shell",
                BusyAvailability::DeferredUntilTurnEnd,
            ),
            ("/skills list", BusyAvailability::Immediate),
            ("/skills activate x", BusyAvailability::DeferredUntilTurnEnd),
            ("/workbench show", BusyAvailability::Immediate),
            (
                "/workbench pin a.rs",
                BusyAvailability::DeferredUntilTurnEnd,
            ),
            ("/palace search x", BusyAvailability::Immediate),
            ("/palace promote x", BusyAvailability::DeferredUntilTurnEnd),
            ("/diary today", BusyAvailability::Immediate),
            ("/diary note x", BusyAvailability::DeferredUntilTurnEnd),
            ("/matrix show", BusyAvailability::Immediate),
            ("/matrix start", BusyAvailability::DeferredUntilTurnEnd),
            ("/effort show", BusyAvailability::Immediate),
            ("/effort high", BusyAvailability::Staged),
            ("/uia-model switch x", BusyAvailability::Staged),
            ("/plugins list", BusyAvailability::Immediate),
            ("/plugins install x", BusyAvailability::DeferredUntilTurnEnd),
            ("/tools", BusyAvailability::DeferredUntilTurnEnd),
            ("/whoami", BusyAvailability::DeferredUntilTurnEnd),
            ("/compact", BusyAvailability::DeferredUntilTurnEnd),
            ("/new", BusyAvailability::DeferredUntilTurnEnd),
            // Runde 5, Teil I: Live-Stream-Umschalter wirkt sofort.
            ("/agent stream none", BusyAvailability::Immediate),
            // Runde 5, Teil K: Hintergrund-Agenten auflisten/abbrechen wirkt sofort.
            ("/agent bg", BusyAvailability::Immediate),
            ("/agent cancel x", BusyAvailability::Immediate),
            ("/agent stream orchestrators", BusyAvailability::Immediate),
        ];
        for (raw, expected) in cases {
            assert_eq!(
                super::busy_availability_for(&registry, raw),
                *expected,
                "{raw}"
            );
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // 15. build_services (W2d/B3-C2): Arc<HostPermitHandles> service entry
    // -----------------------------------------------------------------------

    /// `build_services_inserts_host_permit_handles_from_assembly_accessors`:
    /// a caller building `Arc<harw_tool_shell::HostPermitHandles>` from the
    /// `RuntimeAssembly` accessors (`host_permit_ledger()`,
    /// `host_permit_session_registry()`, `host_permit_prompt_sender()` —
    /// stood in here by directly-constructed `ProcessPermitLedger`/
    /// `HostPermitSessionRegistry` `Arc`s, since building a full
    /// `RuntimeAssembly` is out of scope for this module) must see the exact
    /// same `Arc` threaded through `build_services` into the `ServiceMap` —
    /// no rebuild, no clone-of-contents. This is what makes `/sandbox-lease`
    /// and the `/status` line resolve the live ledger/registry state.
    #[test]
    fn build_services_inserts_host_permit_handles_from_assembly_accessors() -> TestResult {
        let adapters = adapters();
        let controller = test_controller();
        let ledger = Arc::new(harw_sandbox::ProcessPermitLedger::default());
        let registry = Arc::new(harw_sandbox::HostPermitSessionRegistry::default());
        let handles = Arc::new(harw_tool_shell::HostPermitHandles {
            ledger: Arc::clone(&ledger),
            registry: Arc::clone(&registry),
            prompts: None,
        });

        let services = build_services(&adapters, None, None, &controller, None, Some(&handles));

        let retrieved = services
            .get::<Arc<harw_tool_shell::HostPermitHandles>>()
            .ok_or(TestError::Missing(
                "HostPermitHandles must be present in the ServiceMap when supplied",
            ))?;

        assert!(
            Arc::ptr_eq(retrieved, &handles),
            "the exact Arc<HostPermitHandles> supplied by the caller must be threaded through \
             unchanged, not rebuilt"
        );
        assert!(
            Arc::ptr_eq(&retrieved.ledger, &ledger),
            "the ledger Arc inside HostPermitHandles must stay ptr-identical to the assembly's \
             ledger (Arc::clone of the pointer, never a fresh ledger)"
        );
        assert!(
            Arc::ptr_eq(&retrieved.registry, &registry),
            "the registry Arc inside HostPermitHandles must stay ptr-identical to the \
             assembly's host permit session registry"
        );
        Ok(())
    }

    /// `build_services_without_host_permit_handles_leaves_service_absent`:
    /// `None` must not insert any `Arc<HostPermitHandles>` into the
    /// `ServiceMap` — composition roots that never wire host permits (e.g. a
    /// headless run without the accessors available) must not accidentally
    /// expose a fabricated service.
    #[test]
    fn build_services_without_host_permit_handles_leaves_service_absent() {
        let adapters = adapters();
        let controller = test_controller();

        let services = build_services(&adapters, None, None, &controller, None, None);

        assert!(
            services
                .get::<Arc<harw_tool_shell::HostPermitHandles>>()
                .is_none(),
            "without a supplied HostPermitHandles, the ServiceMap must not contain one"
        );
    }

    // -----------------------------------------------------------------------
    // 16. busy_availability_for (Auftrag Punkt 3): the new `sandbox-lease`
    //     op needs no special-casing — the generic `Immediate` fallthrough
    //     must already cover it, exactly like `status`/`ps`/`usage`.
    // -----------------------------------------------------------------------

    /// Test-Doppel für `/sandbox-lease` (Plan B5, registriert von
    /// `harw-ops/src/sandbox_lease.rs`): eine reine `busy = Immediate`
    /// `OperationMeta` ohne Modell-Tool-Zusatzlogik, unabhängig davon, ob die
    /// echte `harw-ops`-Registrierung in dieser Welle bereits gelandet ist.
    impl CountingOperation {
        fn sandbox_lease_stub() -> Self {
            Self {
                meta: OperationMeta {
                    name: "sandbox-lease",
                    summary: "Test-only sandbox-lease stand-in.",
                    domain: OperationDomain::Misc,
                    permission: PermissionTier::Operator,
                    surfaces: vec![Surface::Command {
                        path: "/sandbox-lease",
                        visibility: CommandVisibility::TuiOnly,
                    }],
                    aliases: &[],
                    category: OperationCategory::Misc,
                    args_schema: None,
                    output_schema: None,
                    busy: BusyAvailability::Immediate,
                },
                meta_reads: AtomicUsize::new(0),
                dispatches: AtomicUsize::new(0),
            }
        }
    }

    /// `busy_availability_for_sandbox_lease_is_immediate_without_special_casing`:
    /// bare `/sandbox-lease` and `/sandbox-lease status`/`/sandbox-lease
    /// revoke` must all classify as `Immediate` via `spec.busy` — without a
    /// `busy_subcommands` table every sub-command inherits the operation's
    /// class.
    #[test]
    fn busy_availability_for_sandbox_lease_is_immediate_without_special_casing() {
        let operation = Arc::new(CountingOperation::sandbox_lease_stub());
        let adapters = CommandAdapter::from_operation(operation);
        let registry = CommandRegistry::from_command_adapters(&adapters);

        for raw in [
            "/sandbox-lease",
            "/sandbox-lease status",
            "/sandbox-lease revoke",
        ] {
            assert_eq!(
                super::busy_availability_for(&registry, raw),
                BusyAvailability::Immediate,
                "'{raw}' must classify as Immediate without any sandbox-lease-specific rule"
            );
        }
    }
}
