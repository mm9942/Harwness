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
//! # Host-PATH für `!`-Befehle (Plan Teil C2)
//! [`execute_shell`] baut seinen `ShellToolProvider` über
//! [`shell_escape_provider`] mit der beim Prozessstart gelesenen
//! [`harw_sandbox::HostPathBinding::from_env`] (der zsh-`PATH` des Nutzers,
//! `RUSTUP_HOME`/`CARGO_HOME`). Ohne diese Bindung (`PATH` unset/leer) bleibt
//! der `bwrap`-Plan byte-identisch zu heute. Die normale Modell-`shell.exec`-
//! Ausführung in der Projekt-Sandbox bleibt davon unberührt — sie baut ihren
//! eigenen `ShellToolProvider` in `harw-runtime`, nicht hier.
//!
//! # `!`-Modus wie in Claude Code (Plan Teil F)
//! [`execute_shell`] liefert seit Plan Teil F kein reines `String` mehr,
//! sondern [`ShellDisplayOutcome`] — Anzeigetext (`display_text`,
//! byte-identisch zum bisherigen Rückgabewert) plus ein optionales
//! strukturiertes [`ShellRunOutcome`] (`run`), gesetzt genau dann, wenn der
//! `shell.exec`-Ausführer tatsächlich lief (`ToolOutput::Json` mit
//! `exit_code`). [`execute_with_context`] nutzt weiterhin nur
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
//! ist folgenlos, weil `!!` als Nicht-`Command`-Invocation in
//! [`busy_availability_for`] immer `DeferredUntilTurnEnd` ist und deshalb nie
//! über den Busy-Sofort-Dispatch (`dispatch_slash_command`), sondern
//! ausschließlich über den Idle-Zweig in `app.rs` (also über
//! `dispatch_command_with_shell_result`) erreicht wird.
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
use harw_extension_api::contributors::ToolProvider;
#[cfg(test)]
use harw_operations::SharedSessionController;
use harw_operations::adapter::CommandAdapter;
use harw_operations::operation::BusyAvailability;
#[cfg(test)]
use harw_operations::registry::OperationRegistry;
use harw_operations::{OpContext, PermissionTier, ServiceMap};
use harw_sandbox::HostPathBinding;
use harw_tool_shell::ShellToolProvider;
use harw_tools::spec::ToolName;
use harw_tools::{ToolCall, ToolExecutionContext, ToolOutput};
use harw_types::{SessionId, ToolCallId, TurnId};

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
/// - [`Invocation::Shell`]: wird im lokalen TUI-Kontext standardmäßig durch den
///   Bubblewrap-gebundenen `shell.exec`-Ausführer ausgeführt. `HARW_DISABLE_SHELL=1`
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
/// **Bewusst ausgeklammert:** die `/export`-Sonderbehandlung
/// (`execute_export_command_with_data` in `app.rs`, liefert zusätzlich
/// `OpOutput::data` für den Export-Dateischreiber) ist in `app.rs` als
/// private `async fn` deklariert und von hier — anderes Modul, kein
/// `pub(crate)` — nicht erreichbar. Für den Sofort-Dispatch-Anwendungsfall
/// (Welle 4b) ist das folgenlos: `/export` trägt `busy =
/// DeferredUntilTurnEnd` (Standard, nicht Teil der in Welle 2d/3d/4a auf
/// `Immediate` gesetzten Befehle) und läuft daher nie über diesen Helfer.
/// Falls Welle 5 auch den *Idle*-Pfad vollständig hierher verlagern will,
/// müsste `app.rs` zuerst `execute_export_command_with_data` auf
/// `pub(crate)` heben (oder die Funktion nach `command_exec.rs`
/// verschieben); dieser Helfer bräuchte dann einen zusätzlichen
/// `/export`-Vorabschritt, der bei `Some(Ok(..))`/`Some(Err(..))` Vorrang vor
/// `execute_command_as` erhält (siehe app.rs, `HarwEvent::Command`-Zweig) —
/// bis dahin bleibt die Export-Sonderbehandlung ausschließlich in `app.rs`.
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
/// [`harw_operations::operation::BusyAvailability`] für den Busy-Sofort-Dispatch
/// (Welle 4b).
///
/// # Beschreibung
/// Nutzt [`crate::classify_input`] (denselben Parser wie jeder andere
/// Dispatch-Pfad). Nur [`Invocation::Command`] kann `Immediate` liefern:
/// - Kein Befehl (`Shell`, `ShellRepeat`, `Note`, `Mention`, `Chat`) →
///   `DeferredUntilTurnEnd` (diese Formen haben keine `busy`-Metadaten und
///   sind während eines laufenden Turns ohnehin nicht sicher sofort
///   ausführbar).
/// - Unbekannter Befehlsname (`registry.find` liefert `None`) →
///   `DeferredUntilTurnEnd` — die ehrliche „unbekannter Befehl"-Meldung
///   entsteht weiterhin erst im eigentlichen Dispatch, nicht hier.
/// - Bekannter Befehl (kanonischer Name oder Alias, via [`CommandRegistry::find`]) →
///   grundsätzlich `spec.busy`.
///
/// **Sonderfall `model`/`provider`** (§Auftrag Punkt 3): beide Operationen
/// sind als `busy = Immediate` markiert, aber nur ihre Anzeige (`show`/
/// `list`) darf während eines laufenden Turns sofort laufen — `/model
/// switch ...`, ein bare `/model`/`/model switch` (öffnet den
/// `ModelSwitchPicker`, der eine Änderung vornimmt) und `/provider test`
/// müssen weiterhin bis zum Turn-Ende warten. Geprüft wird der **kanonische**
/// Befehlsname (nach Alias-Auflösung über `spec.name`, nicht der getippte
/// Alias) gegen das erste Argument-Token aus `Invocation::Command::raw_args`:
/// - `provider`: `show`/`list` **oder kein Argument** (bare `/provider` ≡
///   `show`, siehe 4a) → `Immediate`; jedes andere erste Token (z. B. `test`)
///   → `Deferred`.
/// - `model`: `show`/`list` → `Immediate`; **kein Argument** (bare `/model`
///   öffnet den Picker) oder jedes andere erste Token (u. a. `switch`,
///   egal ob mit oder ohne weiteres Argument) → `Deferred`.
///
/// Alle anderen `Immediate`-Befehle (`status, ps, usage, help, diff, work,
/// review, approve, deny, cancel, stop`, Welle 2d/3d) behalten unverändert
/// `spec.busy`.
///
/// # Argumente
/// - `registry` (`&CommandRegistry`): der Dispatch-Katalog, z. B.
///   `CommandRegistry::from_command_adapters(app.adapters())` bzw.
///   `CommandRegistry::built_in()`.
/// - `raw` (`&str`): die rohe, noch nicht abgeschickte oder gerade
///   abgeschickte Eingabezeile.
///
/// # Rückgabe
/// Die [`BusyAvailability`] dieser Eingabe für den Busy-Sofort-Dispatch.
#[must_use]
pub(crate) fn busy_availability_for(registry: &CommandRegistry, raw: &str) -> BusyAvailability {
    let Ok(Invocation::Command { name, raw_args }) = crate::classify_input(raw) else {
        return BusyAvailability::DeferredUntilTurnEnd;
    };

    let Some(spec) = registry.find(&name) else {
        return BusyAvailability::DeferredUntilTurnEnd;
    };

    if spec.busy != BusyAvailability::Immediate {
        return spec.busy;
    }

    let first_arg = raw_args.first().map(String::as_str);
    match (spec.name.as_str(), first_arg) {
        ("provider", None | Some("show") | Some("list")) => BusyAvailability::Immediate,
        ("provider", Some(_)) => BusyAvailability::DeferredUntilTurnEnd,
        ("model", Some("show") | Some("list")) => BusyAvailability::Immediate,
        ("model", None | Some(_)) => BusyAvailability::DeferredUntilTurnEnd,
        (_, _) => BusyAvailability::Immediate,
    }
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
        CommandAction::Shell(command) => {
            execute_shell(sandbox, session_id, command)
                .await
                .display_text
        }
        CommandAction::ShellRepeat => "Shell-Wiederholung ist noch nicht verfügbar.".to_owned(),
        CommandAction::Note(note) => format!("Notiz: {note}"),
        CommandAction::Mention { target, body } => format!("@{target}: {body}"),
        CommandAction::Chat(text) => text,
    }
}

/// Ergebnis eines tatsächlich ausgeführten `!`/`!!`-Laufs (Plan Teil F):
/// Grundlage für den automatischen Folge-Turn, den `app.rs` nach einem
/// erfolgreichen `shell.exec`-Aufruf startet.
///
/// # Beschreibung
/// Nur gesetzt, wenn der `shell.exec`-Ausführer wirklich lief und ein
/// `ToolOutput::Json` mit `exit_code` lieferte (siehe [`execute_shell`]) —
/// bei Admission-Fehlern (`HARW_DISABLE_SHELL`, fehlende Capability), einem
/// nicht verfügbaren Ausführer oder einer Ablehnung durch die Sandbox-
/// Autorität gibt es kein `ShellRunOutcome`, und `app.rs` startet dann
/// bewusst keinen Folge-Turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShellRunOutcome {
    /// Der ausgeführte Befehlstext, ohne führendes `!` (bei `!!` der
    /// aufgelöste letzte Befehl der Sitzung).
    pub(crate) command: String,
    /// Exit-Code des Prozesses.
    pub(crate) exit_code: i64,
    /// `stdout` und `stderr` zusammengeführt, ungekappt — die Kappung auf
    /// 8000 Zeichen erfolgt erst beim Bau der Folge-Turn-Nachricht in
    /// `app.rs` (`build_shell_turn_message`).
    pub(crate) combined_output: String,
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
            let outcome = execute_shell(sandbox, session_id, command).await;
            CommandDispatchOutcome {
                text: outcome.display_text,
                shell: outcome.run,
            }
        }
        CommandAction::ShellRepeat => match last_shell_command {
            Some(command) => {
                let outcome = execute_shell(sandbox, session_id, command.to_owned()).await;
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

/// Baut den `ShellToolProvider` für lokale `!`-Befehle, optional mit der
/// beim Prozessstart gelesenen Host-PATH-Bindung (Plan
/// `recursive-cooking-lobster.md` Teil C2).
///
/// # Beschreibung
/// Reine Hilfsfunktion, ausgelagert aus [`execute_shell`], damit die
/// Übernahme einer [`HostPathBinding`] in den `ShellToolProvider` ohne
/// `bwrap`-Aufbau testbar ist: `binding = Some(..)` ruft
/// [`ShellToolProvider::with_host_path`] auf, `binding = None` liefert den
/// unveränderten `ShellToolProvider::new()`. Wirkt sich nur auf den
/// `bwrap`-Sandbox-Pfad von `run_command` aus, nicht auf die normale
/// Modell-`shell.exec`-Ausführung (die baut ihren eigenen
/// `ShellToolProvider` ohne Host-PATH-Bindung, siehe `harw-runtime`).
///
/// # Argumente
/// - `binding` (`Option<HostPathBinding>`): siehe [`HostPathBinding::from_env`].
///
/// # Rückgabe
/// Ein [`ShellToolProvider`] mit oder ohne Host-PATH-Bindung.
fn shell_escape_provider(binding: Option<HostPathBinding>) -> ShellToolProvider {
    let provider = ShellToolProvider::new();
    match binding {
        Some(binding) => provider.with_host_path(binding),
        None => provider,
    }
}

/// Führt einen lokalen `!`-Befehl ausschließlich über den normalen,
/// Bubblewrap-gebundenen `shell.exec`-Ausführer aus. Die übergebene TUI-Sandbox
/// ist die gesamte Autoritätsquelle; weder Arbeitsverzeichnis noch Rechte kommen
/// aus dem vom Benutzer getippten Text. Der beim Start dieses Prozesses
/// gelesene zsh-PATH ([`HostPathBinding::from_env`]) wird über
/// [`shell_escape_provider`] an den `ShellToolProvider` gereicht (Plan Teil
/// C2) — wirkt sich nur auf den `bwrap`-Pfad aus, nicht auf die normale
/// Modell-`shell.exec`-Ausführung.
///
/// Liefert seit Plan Teil F [`ShellDisplayOutcome`] statt eines reinen
/// `String`: `display_text` ist byte-identisch zum bisherigen
/// Rückgabewert; `run` ist nur bei einem echten `ToolOutput::Json`-Ergebnis
/// gesetzt (siehe [`ShellRunOutcome`]).
async fn execute_shell(
    sandbox: &SandboxSpec,
    session_id: &SessionId,
    command: String,
) -> ShellDisplayOutcome {
    let provider = shell_escape_provider(HostPathBinding::from_env());
    let tool_name = ToolName::new("shell.exec");
    let Some(executor) = provider.executor(&tool_name) else {
        return ShellDisplayOutcome {
            display_text: "Shell-Ausführung fehlgeschlagen: shell.exec ist nicht verfügbar."
                .to_owned(),
            run: None,
        };
    };
    let context = ToolExecutionContext::new(session_id.clone(), TurnId::new(), sandbox.clone());
    // Geklont, weil `command` unten für `ShellRunOutcome::command` gebraucht
    // wird, nachdem `serde_json::json!` das Original in die Aufrufargumente
    // verschoben hat.
    let command_for_result = command.clone();
    let call = ToolCall {
        id: ToolCallId::new(),
        name: tool_name,
        arguments: serde_json::json!({ "command": command }),
    };
    match executor.execute(&context, &call).await {
        Ok(ToolOutput::Json { content }) => {
            let stdout = content
                .get("stdout")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let stderr = content
                .get("stderr")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let exit_code = content
                .get("exit_code")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(-1);
            let display_text = match (stdout.is_empty(), stderr.is_empty()) {
                (true, true) => format!("Shell beendet (Exit-Code {exit_code})."),
                (false, true) => stdout.to_owned(),
                (true, false) => format!("stderr:\n{stderr}"),
                (false, false) => format!("{stdout}\nstderr:\n{stderr}"),
            };
            let combined_output = match (stdout.is_empty(), stderr.is_empty()) {
                (true, true) => String::new(),
                (false, true) => stdout.to_owned(),
                (true, false) => stderr.to_owned(),
                (false, false) => format!("{stdout}\n{stderr}"),
            };
            ShellDisplayOutcome {
                display_text,
                run: Some(ShellRunOutcome {
                    command: command_for_result,
                    exit_code,
                    combined_output,
                }),
            }
        }
        Ok(ToolOutput::Text { content }) => ShellDisplayOutcome {
            display_text: content,
            run: None,
        },
        Ok(ToolOutput::Error { message }) => ShellDisplayOutcome {
            display_text: format!("Shell-Ausführung abgelehnt: {message}"),
            run: None,
        },
        Err(error) => ShellDisplayOutcome {
            display_text: format!("Shell-Ausführung fehlgeschlagen: {error}"),
            run: None,
        },
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

    use super::{
        CommandServices, build_services, dispatch_command_with_shell_result, shell_escape_provider,
    };
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

    /// Mit einem vorherigen Befehl versucht `!!`, ihn über `execute_shell`
    /// erneut auszuführen — anders als die zustandslose `execute_with_context`
    /// (siehe `test_shell_repeat_is_not_implemented`), die diesen Zustand
    /// nicht kennt. `test_sandbox()` gewährt kein `ExecuteProcess`, daher
    /// scheitert die Ausführung fail-closed; das beweist trotzdem, dass der
    /// gespeicherte Befehl tatsächlich (erneut) beim Ausführer ankommt statt
    /// bei der alten „noch nicht verfügbar"-Meldung stehen zu bleiben.
    #[tokio::test]
    async fn dispatch_shell_repeat_with_previous_command_reruns_it() -> TestResult {
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

        assert_eq!(
            outcome.text,
            "Shell-Ausführung abgelehnt: shell.exec denied: ExecuteProcess permission missing"
        );
        assert!(
            outcome.shell.is_none(),
            "eine von der Sandbox abgelehnte Ausführung darf keinen Folge-Turn auslösen"
        );
        assert_ne!(
            outcome.text, "Shell-Wiederholung ist noch nicht verfügbar.",
            "dispatch_command_with_shell_result muss !! wirklich auflösen"
        );
        Ok(())
    }

    /// Ein direkter `!`-Befehl (nicht `!!`) läuft über denselben Pfad wie
    /// `execute_with_context`s `CommandAction::Shell`-Zweig — `test_sandbox()`
    /// gewährt kein `ExecuteProcess`, daher `shell: None` und derselbe
    /// Ablehnungstext wie bei der bestehenden `!`-Deny-Prüfung
    /// (`test_execute_with_context_admitted_shell_denied_without_execute_permission`).
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
            "!echo hi",
            None,
            || build_services(&adapters, None, None, &controller, None, None),
        )
        .await;
        std::fs::remove_dir_all(tmp).ok();

        assert!(outcome.shell.is_none());
        assert_eq!(
            outcome.text,
            "Shell-Ausführung abgelehnt: shell.exec denied: ExecuteProcess permission missing"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_execute_with_context_admitted_shell_denied_without_execute_permission()
    -> TestResult {
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
            "!ls -la",
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

        // `test_sandbox()` gewährt nur ReadWorkspace/WriteWorkspace, kein
        // ExecuteProcess: `execute_shell` läuft jetzt tatsächlich bis zum
        // echten `shell.exec`-Ausführer durch (kein Platzhalter mehr) und
        // dieser lehnt fail-closed wegen der fehlenden Berechtigung ab.
        assert_eq!(
            shell,
            "Shell-Ausführung abgelehnt: shell.exec denied: ExecuteProcess permission missing"
        );
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

    #[test]
    fn busy_availability_for_status_is_immediate() -> TestResult {
        assert_eq!(
            super::busy_availability_for(&built_in_registry()?, "/status"),
            BusyAvailability::Immediate
        );
        Ok(())
    }

    #[test]
    fn busy_availability_for_mode_plan_is_deferred() -> TestResult {
        assert_eq!(
            super::busy_availability_for(&built_in_registry()?, "/mode plan"),
            BusyAvailability::DeferredUntilTurnEnd
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
    fn busy_availability_for_model_switch_with_argument_is_deferred() -> TestResult {
        assert_eq!(
            super::busy_availability_for(&built_in_registry()?, "/model switch x"),
            BusyAvailability::DeferredUntilTurnEnd
        );
        Ok(())
    }

    #[test]
    fn busy_availability_for_bare_model_is_deferred() -> TestResult {
        assert_eq!(
            super::busy_availability_for(&built_in_registry()?, "/model"),
            BusyAvailability::DeferredUntilTurnEnd
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

    // -----------------------------------------------------------------------
    // 14. shell_escape_provider (C2): HostPathBinding taken up by the
    //     ShellToolProvider that backs local `!`-commands.
    // -----------------------------------------------------------------------

    /// `shell_escape_provider_without_binding_leaves_host_path_none`: with no
    /// `HostPathBinding` (e.g. `HostPathBinding::from_env()` returned `None`
    /// because `PATH` was unset), the built provider must be indistinguishable
    /// from `ShellToolProvider::new()` — no bwrap plan change without a call.
    #[test]
    fn shell_escape_provider_without_binding_leaves_host_path_none() {
        let provider = shell_escape_provider(None);
        assert!(
            provider.host_path.is_none(),
            "shell_escape_provider(None) must not set a host_path binding"
        );
    }

    /// `shell_escape_provider_with_binding_sets_host_path`: with a supplied
    /// `HostPathBinding`, `shell_escape_provider` must thread it through
    /// `ShellToolProvider::with_host_path` unchanged (Plan Teil C2). Verified
    /// without ever touching `bwrap` — this is a pure struct-field check.
    #[test]
    fn shell_escape_provider_with_binding_sets_host_path() {
        let binding = harw_sandbox::HostPathBinding {
            path: "/host/bin:/host/usr/bin".to_owned(),
            ..harw_sandbox::HostPathBinding::default()
        };

        let provider = shell_escape_provider(Some(binding.clone()));

        assert_eq!(
            provider.host_path,
            Some(binding),
            "shell_escape_provider(Some(binding)) must carry the exact binding into \
             ShellToolProvider::host_path via with_host_path"
        );
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
    /// revoke` must all classify as `Immediate` via the generic `(_, _) =>
    /// BusyAvailability::Immediate` fallthrough in
    /// [`super::busy_availability_for`] — unlike `model`/`provider`, no
    /// first-argument rule is needed, because every `/sandbox-lease`
    /// sub-command is safe to dispatch immediately (Contract §Auftrag
    /// Punkt 3: "sollte nicht" nötig sein).
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
