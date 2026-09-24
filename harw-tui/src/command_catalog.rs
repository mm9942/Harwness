//! Statischer Hilfe-Katalog für `/`-Befehle: TUI-lokale Befehle,
//! Unterkommando-Hinweise, Nutzungszeilen und geplante Befehle.
//!
//! # Verantwortung
//! - [`local_command_specs`]: Spezifikationen der rein TUI-lokal abgefangenen
//!   Befehle (`/tools`, `/exit`, …) plus Ersatz-Spezifikationen für Befehle,
//!   deren Operation (noch) nicht registriert ist (`/workbench`, `/kanban`,
//!   `/palace`, `/dream`, `/diary`, `/models`, `/mode`). Beim Einmischen über
//!   `CommandRegistry::with_local_specs` gewinnt immer die Operation; der
//!   Ersatz entfällt dann.
//! - [`enrich`]: ergänzt eine aus einer Operation abgeleitete Spezifikation um
//!   Kurzbeschreibung, Nutzungszeile und Unterkommandos.
//! - [`subcommand_hints`]: Unterkommando-Grammatik je Befehl; die Einträge
//!   folgen den tatsächlichen Parsern in `harw-ops` bzw.
//!   `crate::tools_command`.
//! - [`PLANNED_COMMANDS`]: Befehle aus dem Interaktionsvertrag, die weder als
//!   Operation noch lokal existieren — für die Hilfe („geplant").
//!
//! # Nebenläufigkeit
//! Nur `const`-Tabellen und reine Funktionen; kein geteilter Zustand.
//!
//! # Fehlertypen
//! Keine — ungültige Namen in den Tabellen würden beim Bau still entfallen
//! und von den Tests erkannt.

use crate::command::{
    BusyAvailability, CommandDomain, CommandScope, CommandSpec, OutputSurface, PermissionTier,
    SubcommandHint,
};

/// Maximale Länge (Zeichen) einer Operations-Beschreibung, bevor [`enrich`]
/// sie auf den ersten Satz kürzt.
const MAX_SUMMARY_CHARS: usize = 110;

// ---------------------------------------------------------------------------
// Unterkommando-Tabellen
// ---------------------------------------------------------------------------

/// `/model`, Grammatik aus `harw-ops/src/model.rs` (`show|list|switch`).
const MODEL: &[SubcommandHint] = &[
    SubcommandHint::new("show", "", "Aktives Modell anzeigen"),
    SubcommandHint::new("list", "", "Modelle des Katalogs auflisten"),
    SubcommandHint::new(
        "switch",
        "<modell-id>",
        "Provider und Modell sofort wechseln",
    ),
];

/// `/uia-model` (`show|list|switch`).
const UIA_MODEL: &[SubcommandHint] = &[
    SubcommandHint::new("show", "", "Gepinntes UIA-Modell anzeigen"),
    SubcommandHint::new("list", "", "Modelle des Katalogs auflisten"),
    SubcommandHint::new(
        "switch",
        "<modell-id>",
        "UIA-Modell pinnen (ab nächster Sitzung)",
    ),
];

/// `/uia-worker-model` (`show|list|switch`).
const UIA_WORKER_MODEL: &[SubcommandHint] = &[
    SubcommandHint::new("show", "", "Gepinntes UIA-Worker-Modell anzeigen"),
    SubcommandHint::new("list", "", "Modelle des UIA-Providers auflisten"),
    SubcommandHint::new(
        "switch",
        "<modell-id>",
        "UIA-Worker-Modell pinnen (ab nächster Sitzung)",
    ),
];

/// `/provider` (`show|list|test`); Wechsel läuft über `/model switch`.
const PROVIDER: &[SubcommandHint] = &[
    SubcommandHint::new("show", "", "Aktiven Provider anzeigen"),
    SubcommandHint::new("list", "", "Konfigurierte Provider auflisten"),
    SubcommandHint::new("test", "", "Verbindung des aktiven Providers testen"),
];

/// `/effort` — das Level ist selbst das Unterkommando.
const EFFORT: &[SubcommandHint] = &[
    SubcommandHint::new("show", "", "Aktuellen Reasoning-Effort anzeigen"),
    SubcommandHint::new("minimal", "", "Minimaler Reasoning-Effort"),
    SubcommandHint::new("low", "", "Niedriger Reasoning-Effort"),
    SubcommandHint::new("medium", "", "Mittlerer Reasoning-Effort"),
    SubcommandHint::new("high", "", "Hoher Reasoning-Effort"),
    SubcommandHint::new("xhigh", "", "Sehr hoher Reasoning-Effort"),
    SubcommandHint::new("max", "", "Maximaler Reasoning-Effort"),
    SubcommandHint::new("clear", "", "Zurück auf Provider-Default"),
];

/// `/uia-effort` — wie `/effort`, wirkt ab der nächsten Sitzung.
const UIA_EFFORT: &[SubcommandHint] = &[
    SubcommandHint::new("show", "", "Gepinnten UIA-Effort anzeigen"),
    SubcommandHint::new("minimal", "", "Minimaler Effort (ab nächster Sitzung)"),
    SubcommandHint::new("low", "", "Niedriger Effort (ab nächster Sitzung)"),
    SubcommandHint::new("medium", "", "Mittlerer Effort (ab nächster Sitzung)"),
    SubcommandHint::new("high", "", "Hoher Effort (ab nächster Sitzung)"),
    SubcommandHint::new("xhigh", "", "Sehr hoher Effort (ab nächster Sitzung)"),
    SubcommandHint::new("max", "", "Maximaler Effort (ab nächster Sitzung)"),
    SubcommandHint::new("clear", "", "Pin entfernen (Rollen-Default)"),
];

/// `/mode` — `show`, ein Modusname oder `default <modus>`.
const MODE: &[SubcommandHint] = &[
    SubcommandHint::new("show", "", "Aktuellen Interaktionsmodus anzeigen"),
    SubcommandHint::new("chat", "", "Modus Chat: Gespräch ohne Werkzeugzwang"),
    SubcommandHint::new("plan", "", "Modus Plan: planen, nichts ändern"),
    SubcommandHint::new("explore", "", "Modus Explore: lesend erkunden"),
    SubcommandHint::new("work", "", "Modus Work: umsetzen mit Werkzeugen"),
    SubcommandHint::new("shell", "", "Modus Shell: Befehle ausführen"),
    SubcommandHint::new(
        "default",
        "<modus>",
        "Standardmodus in der Konfiguration setzen",
    ),
];

/// `/models` — Modelle je Rolle.
const MODELS: &[SubcommandHint] = &[
    SubcommandHint::new("show", "", "Modelle aller Rollen anzeigen"),
    SubcommandHint::new(
        "set",
        "<rolle> <ziel>",
        "Modell einer Rolle setzen (ab nächster Sitzung)",
    ),
    SubcommandHint::new("reset", "<rolle>", "Rolle auf Vorgabe zurücksetzen"),
    SubcommandHint::new("pick", "<rolle>", "Modellauswahl für eine Rolle öffnen"),
];

/// `/permissions`, Grammatik aus `harw-ops/src/permissions.rs`.
const PERMISSIONS: &[SubcommandHint] = &[
    SubcommandHint::new("show", "", "Freigabemodus, Regeln und Verzeichnisse"),
    SubcommandHint::new(
        "mode",
        "<ask|auto|full> [--session|--project|--global]",
        "Freigabemodus setzen",
    ),
    SubcommandHint::new(
        "allow",
        "<tool> [muster] [--session|--project|--global]",
        "Allow-Regel hinzufügen",
    ),
    SubcommandHint::new(
        "deny",
        "<tool> [muster] [--session|--project|--global]",
        "Deny-Regel hinzufügen",
    ),
    SubcommandHint::new("remove", "<nr>", "Regel nach Nummer entfernen"),
];

/// `/memory`, Grammatik aus `harw-ops/src/memory.rs`.
const MEMORY: &[SubcommandHint] = &[
    SubcommandHint::new("list", "", "Heiße Erinnerungen anzeigen"),
    SubcommandHint::new("stats", "", "Speicherstatistik anzeigen"),
    SubcommandHint::new("recall", "<stichwort…>", "Fakten durchsuchen"),
    SubcommandHint::new("record", "<text…> [--project|--global]", "Fakt festhalten"),
    SubcommandHint::new("forget", "<name>", "Fakt löschen"),
    SubcommandHint::new("maintain", "", "Speicher aufräumen"),
    SubcommandHint::new("consolidate", "[--project|--global]", "Fakten verdichten"),
];

/// `/agent`, Grammatik aus `harw-ops/src/agent.rs`.
const AGENT: &[SubcommandHint] = &[
    SubcommandHint::new("list", "", "Aktive Kind-Agenten auflisten"),
    SubcommandHint::new("stop", "<agent-id>", "Kind-Agenten abbrechen"),
    SubcommandHint::new("budget", "[agent-id]", "Budget und Lease anzeigen"),
];

/// `/export` — Optionen statt Unterkommandos (`harw-ops/src/export.rs`).
const EXPORT: &[SubcommandHint] = &[
    SubcommandHint::new("--format", "<md|json>", "Ausgabeformat wählen"),
    SubcommandHint::new("--datei", "<pfad>", "In Datei schreiben"),
    SubcommandHint::new("--tools", "", "Werkzeugaufrufe einschließen"),
    SubcommandHint::new("--no-tools", "", "Werkzeugaufrufe weglassen"),
    SubcommandHint::new(
        "--reasoning-summary",
        "",
        "Reasoning-Zusammenfassung einschließen",
    ),
    SubcommandHint::new("--max-chars", "<n>", "Länge begrenzen"),
];

/// `/sandbox-lease` — auf der Befehlsfläche nur `status`/`revoke`
/// (`request` braucht einen Grund und läuft über das Modell-Werkzeug).
const SANDBOX_LEASE: &[SubcommandHint] = &[
    SubcommandHint::new("status", "", "Aktive Host-Freigabe anzeigen"),
    SubcommandHint::new("revoke", "", "Host-Freigabe sofort widerrufen"),
];

/// `/workbench`, Grammatik aus `harw-ops/src/workbench.rs`.
const WORKBENCH: &[SubcommandHint] = &[
    SubcommandHint::new("show", "[--scope=…]", "Arbeitsfläche anzeigen"),
    SubcommandHint::new("pin", "<pfad> [notiz]", "Datei anheften"),
    SubcommandHint::new("unpin", "<pfad>", "Datei lösen"),
    SubcommandHint::new("note", "<text>", "Notiz anhängen"),
    SubcommandHint::new(
        "hypothesis",
        "add|confirm|reject <text>",
        "Hypothese anlegen oder entscheiden",
    ),
];

/// `/kanban`, Grammatik aus `harw-ops/src/kanban.rs` (`--board=<id>` davor
/// möglich).
const KANBAN: &[SubcommandHint] = &[
    SubcommandHint::new("show", "[karte]", "Board oder eine Karte anzeigen"),
    SubcommandHint::new("list", "", "Karten des Boards auflisten"),
    SubcommandHint::new("boards", "", "Boards auflisten"),
    SubcommandHint::new("add", "<titel>", "Karte anlegen"),
    SubcommandHint::new(
        "move",
        "<karte> <todo|ready|running|done|blocked|archived>",
        "Karte verschieben",
    ),
    SubcommandHint::new("todo", "<karte>", "Karte auf Todo setzen"),
    SubcommandHint::new("ready", "<karte>", "Karte bereitstellen"),
    SubcommandHint::new("claim", "<karte>", "Karte übernehmen"),
    SubcommandHint::new("block", "<karte> <grund>", "Karte blockieren"),
    SubcommandHint::new("unblock", "<karte>", "Blockade aufheben"),
    SubcommandHint::new("done", "<karte>", "Karte erledigen"),
    SubcommandHint::new("archive", "<karte>", "Karte archivieren"),
];

/// `/palace`, Grammatik aus `harw-ops/src/palace.rs` plus `list` (Vertrag).
const PALACE: &[SubcommandHint] = &[
    SubcommandHint::new("list", "", "Knoten auflisten"),
    SubcommandHint::new("show", "<id>", "Knoten anzeigen"),
    SubcommandHint::new(
        "search",
        "<anfrage> [--max-hops=n] [--max=n]",
        "Graph durchsuchen",
    ),
    SubcommandHint::new("promote", "<thema>", "Thema in den Palast übernehmen"),
];

/// `/dream` (Vertrag).
const DREAM: &[SubcommandHint] = &[
    SubcommandHint::new("list", "", "Traumberichte auflisten"),
    SubcommandHint::new("show", "<id>", "Traumbericht anzeigen"),
];

/// `/diary`, Grammatik aus `harw-ops/src/diary.rs` plus `today` (Vertrag).
const DIARY: &[SubcommandHint] = &[
    SubcommandHint::new("today", "", "Heutige Einträge anzeigen"),
    SubcommandHint::new(
        "show",
        "[agent] [--date=YYYY-MM-DD]",
        "Einträge eines Tages anzeigen",
    ),
    SubcommandHint::new("note", "<text>", "Tagebuchnotiz schreiben"),
];

/// `/tools`, Grammatik aus `crate::tools_command`.
const TOOLS: &[SubcommandHint] = &[
    SubcommandHint::new("on", "<name>", "Werkzeug einschalten"),
    SubcommandHint::new("off", "<name>", "Werkzeug ausschalten"),
    SubcommandHint::new("reset", "[name]", "Überschreibungen zurücksetzen"),
    SubcommandHint::new("profile", "<minimal|coding|full>", "Werkzeugprofil wählen"),
];

/// Befehl → (Nutzungszeile, Unterkommandos). Einzige Quelle für
/// [`subcommand_hints`] und die Nutzungszeilen in [`enrich`].
const HINT_TABLE: &[(&str, &str, &[SubcommandHint])] = &[
    ("model", "/model [show|list|switch <modell-id>]", MODEL),
    (
        "uia-model",
        "/uia-model [show|list|switch <modell-id>]",
        UIA_MODEL,
    ),
    (
        "uia-worker-model",
        "/uia-worker-model [show|list|switch <modell-id>]",
        UIA_WORKER_MODEL,
    ),
    ("provider", "/provider [show|list|test]", PROVIDER),
    (
        "effort",
        "/effort [show|minimal|low|medium|high|xhigh|max|clear]",
        EFFORT,
    ),
    (
        "uia-effort",
        "/uia-effort [show|minimal|low|medium|high|xhigh|max|clear]",
        UIA_EFFORT,
    ),
    ("mode", "/mode [show|<modus>|default <modus>]", MODE),
    (
        "models",
        "/models [show|set <rolle> <ziel>|reset <rolle>|pick <rolle>]",
        MODELS,
    ),
    (
        "permissions",
        "/permissions [show|mode <modus>|allow <tool>|deny <tool>|remove <nr>]",
        PERMISSIONS,
    ),
    (
        "memory",
        "/memory [list|stats|recall|record|forget|maintain|consolidate] …",
        MEMORY,
    ),
    (
        "agent",
        "/agent [list|stop <agent-id>|budget [agent-id]]",
        AGENT,
    ),
    (
        "export",
        "/export [--format <md|json>] [--datei <pfad>] [--no-tools] …",
        EXPORT,
    ),
    (
        "sandbox-lease",
        "/sandbox-lease [status|revoke]",
        SANDBOX_LEASE,
    ),
    (
        "workbench",
        "/workbench [show|pin|unpin|note|hypothesis] …",
        WORKBENCH,
    ),
    (
        "kanban",
        "/kanban [--board=<id>] [show|list|boards|add|move|block|done|…] …",
        KANBAN,
    ),
    (
        "palace",
        "/palace [list|show <id>|search <anfrage>|promote <thema>]",
        PALACE,
    ),
    ("dream", "/dream [list|show <id>]", DREAM),
    ("diary", "/diary [today|show [agent]|note <text>]", DIARY),
    (
        "tools",
        "/tools [on <name>|off <name>|reset [name]|profile <p>]",
        TOOLS,
    ),
];

/// Kurzbeschreibungen, die zu lange oder veraltete Operations-Texte ersetzen.
const SUMMARY_OVERRIDES: &[(&str, &str)] = &[
    (
        "sandbox-lease",
        "Host-Freigabe für shell.exec anzeigen oder widerrufen",
    ),
    ("mode", "Interaktionsmodus anzeigen oder wechseln"),
    (
        "memory",
        "Langzeitgedächtnis: auflisten, suchen, festhalten, vergessen",
    ),
    ("agent", "Kind-Agenten auflisten, stoppen, Budget anzeigen"),
];

/// Befehle aus dem Interaktionsvertrag, die (noch) weder als Operation noch
/// TUI-lokal existieren: `(name, deutsche Kurzbeschreibung)`.
// Wird von der Hilfe-Ansicht gelesen; bis zu deren Verdrahtung nur in Tests.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const PLANNED_COMMANDS: &[(&str, &str)] = &[
    ("rewind", "Sitzung auf einen früheren Stand zurücksetzen"),
    ("fork", "Sitzung ab hier abzweigen"),
    ("archive", "Sitzung archivieren"),
    ("spawn", "Kind-Agenten gezielt starten"),
    ("mention", "Agenten oder Datei erwähnen"),
    ("kill", "Kind-Agenten sofort beenden"),
    ("logs", "Protokolle anzeigen"),
    ("doctor", "Installation und Konfiguration prüfen"),
    ("config", "Konfiguration anzeigen und ändern"),
    ("mcp", "MCP-Server verwalten"),
    ("channels", "Kanäle verwalten"),
    ("lease", "Leases anzeigen und verwalten"),
    ("priority", "Priorität einer Aufgabe setzen"),
    ("depends", "Abhängigkeiten zwischen Aufgaben setzen"),
    ("theme", "Farbschema wechseln"),
];

// ---------------------------------------------------------------------------
// Öffentliche Funktionen
// ---------------------------------------------------------------------------

/// Liefert die Unterkommando-Hinweise eines Befehls (ohne führendes `/`).
///
/// # Rückgabe
/// Die statische Hinweisliste oder eine leere Liste für unbekannte Befehle.
pub(crate) fn subcommand_hints(name: &str) -> &'static [SubcommandHint] {
    for (command, _, hints) in HINT_TABLE {
        if *command == name {
            return *hints;
        }
    }
    &[]
}

/// Nutzungszeile aus der Katalogtabelle, falls vorhanden.
fn usage_for(name: &str) -> Option<&'static str> {
    HINT_TABLE
        .iter()
        .find(|(command, _, _)| *command == name)
        .map(|(_, usage, _)| *usage)
}

/// Kürzt eine lange Beschreibung auf ihren ersten Satz (oder hart auf
/// [`MAX_SUMMARY_CHARS`] Zeichen mit `…`).
fn shorten_summary(summary: &str) -> String {
    if summary.chars().count() <= MAX_SUMMARY_CHARS {
        return summary.to_owned();
    }
    if let Some(end) = summary.find(". ") {
        let first = &summary[..end];
        // Mindestlänge schützt vor Abkürzungen wie „z. B.".
        let len = first.chars().count();
        if (20..=MAX_SUMMARY_CHARS).contains(&len) {
            return first.to_owned();
        }
    }
    let mut short: String = summary.chars().take(MAX_SUMMARY_CHARS - 1).collect();
    short.push('…');
    short
}

/// Ergänzt eine Spezifikation um Katalogwissen.
///
/// # Beschreibung
/// - `summary`: eine Katalog-Überschreibung gewinnt; sonst wird eine zu lange
///   Beschreibung auf den ersten Satz gekürzt. Leer bleibt leer.
/// - `subcommands`: nur gesetzt, wenn noch leer.
/// - `usage`: nur gesetzt, wenn noch leer — aus der Katalogtabelle, sonst
///   `"/<name>"`.
///
/// Idempotent: ein zweiter Aufruf ändert nichts mehr.
pub(crate) fn enrich(spec: &mut CommandSpec) {
    let name = spec.name.as_str().to_owned();
    if let Some((_, summary)) = SUMMARY_OVERRIDES.iter().find(|(n, _)| *n == name) {
        spec.summary = (*summary).to_owned();
    } else if !spec.summary.is_empty() {
        spec.summary = shorten_summary(&spec.summary);
    }
    if spec.subcommands.is_empty() {
        spec.subcommands = subcommand_hints(&name).to_vec();
    }
    if spec.usage.is_empty() {
        spec.usage = usage_for(&name).map_or_else(|| format!("/{name}"), str::to_owned);
    }
}

/// Baut eine lokale Spezifikation; `None` nur bei ungültigem Namen (von den
/// Tests ausgeschlossen).
fn local_spec(
    name: &str,
    domain: CommandDomain,
    permission: PermissionTier,
    busy: BusyAvailability,
    summary: &str,
    usage: &str,
) -> Option<CommandSpec> {
    let mut spec = CommandSpec::new(
        name,
        Vec::<String>::new(),
        CommandScope::TuiOnly,
        permission,
        OutputSurface::Inline,
        domain,
    )
    .ok()?
    .with_help(summary, usage, subcommand_hints(name))
    .local();
    spec.busy = busy;
    enrich(&mut spec);
    Some(spec)
}

/// Namen der Ersatz-Spezifikationen für (noch) fehlende Operationen.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const FALLBACK_COMMANDS: &[&str] = &[
    "workbench",
    "kanban",
    "palace",
    "dream",
    "diary",
    "models",
    "mode",
];

/// Spezifikationen aller TUI-lokalen Befehle plus Ersatz-Spezifikationen
/// ([`FALLBACK_COMMANDS`]).
///
/// # Beschreibung
/// Alle Einträge tragen [`crate::command::CommandOrigin::TuiLocal`] und
/// [`CommandScope::TuiOnly`]. Beim Einmischen via
/// `CommandRegistry::with_local_specs` gewinnt bei Namens-/Alias-Kollision
/// die Operation — Ersatz-Spezifikationen verschwinden also automatisch,
/// sobald die Operation registriert ist.
pub(crate) fn local_command_specs() -> Vec<CommandSpec> {
    use BusyAvailability::{DeferredUntilTurnEnd as Deferred, Immediate};
    use CommandDomain::{
        AgentTopology, CatalogConfig, Execution, Knowledge, Misc, SessionLifecycle,
    };
    use PermissionTier::{Observer, Operator};

    [
        // ── Echte lokale Befehle ──────────────────────────────────────────
        local_spec(
            "tools",
            Execution,
            Operator,
            Immediate,
            "Werkzeuge anzeigen, ein-/ausschalten, Profil wählen",
            "",
        ),
        local_spec(
            "resume",
            SessionLifecycle,
            Operator,
            Deferred,
            "Frühere Sitzung fortsetzen (ohne ID: Auswahl)",
            "/resume [sitzungs-id]",
        ),
        local_spec(
            "sessions",
            SessionLifecycle,
            Observer,
            Deferred,
            "Sitzungsauswahl öffnen",
            "/sessions",
        ),
        local_spec(
            "exit",
            SessionLifecycle,
            Observer,
            Immediate,
            "TUI beenden (wie /quit)",
            "/exit",
        ),
        local_spec(
            "clear",
            SessionLifecycle,
            Observer,
            Deferred,
            "Anzeige leeren (Sitzung bleibt erhalten)",
            "/clear",
        ),
        local_spec(
            "verbose",
            Misc,
            Observer,
            Immediate,
            "Ausführliche Werkzeuganzeige umschalten",
            "/verbose",
        ),
        local_spec(
            "keys",
            Misc,
            Observer,
            Immediate,
            "Tastenbelegung anzeigen",
            "/keys",
        ),
        local_spec(
            "whoami",
            Misc,
            Observer,
            Immediate,
            "Sitzung, Berechtigung und aktives Modell anzeigen",
            "/whoami",
        ),
        local_spec(
            "rename",
            SessionLifecycle,
            Operator,
            Deferred,
            "Sitzung umbenennen",
            "/rename <titel>",
        ),
        local_spec(
            "agents",
            AgentTopology,
            Observer,
            Immediate,
            "Agenten-Panel ein-/ausblenden",
            "/agents",
        ),
        // ── Ersatz, solange die Operation fehlt ───────────────────────────
        local_spec(
            "workbench",
            Knowledge,
            Operator,
            Immediate,
            "Arbeitsfläche: angeheftete Dateien, Notizen, Hypothesen",
            "",
        ),
        local_spec(
            "kanban",
            Knowledge,
            Operator,
            Immediate,
            "Kanban-Board der Aufgaben",
            "",
        ),
        local_spec(
            "palace",
            Knowledge,
            Operator,
            Immediate,
            "Gedächtnispalast: Wissensknoten durchsuchen",
            "",
        ),
        local_spec(
            "dream",
            Knowledge,
            Operator,
            Immediate,
            "Traumberichte (nächtliche Reflexion) ansehen",
            "",
        ),
        local_spec(
            "diary",
            Knowledge,
            Operator,
            Immediate,
            "Tagebuch: Einträge ansehen, Notiz schreiben",
            "",
        ),
        local_spec(
            "models",
            CatalogConfig,
            Operator,
            Immediate,
            "Modelle je Rolle anzeigen und setzen",
            "",
        ),
        local_spec(
            "mode",
            SessionLifecycle,
            Operator,
            Immediate,
            "Interaktionsmodus anzeigen oder wechseln",
            "",
        ),
    ]
    .into_iter()
    .flatten()
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CommandRegistry;
    use crate::command::{CommandName, CommandOrigin};
    use crate::test_support::{TestResult, ctx};

    /// Echte lokale Befehle (keine Ersatz-Spezifikationen).
    const REAL_LOCAL: &[&str] = &[
        "tools", "resume", "sessions", "exit", "clear", "verbose", "keys", "whoami", "rename",
        "agents",
    ];

    /// Befehle, deren Operation fehlen darf (Wissens-Ops und `/models`).
    const ALLOWED_MISSING: &[&str] = &["workbench", "kanban", "palace", "dream", "diary", "models"];

    #[test]
    fn local_specs_cover_every_local_and_fallback_name_once() {
        let specs = local_command_specs();
        let mut expected: Vec<&str> = REAL_LOCAL.to_vec();
        expected.extend_from_slice(FALLBACK_COMMANDS);
        assert_eq!(specs.len(), expected.len(), "a local spec failed to build");
        for name in expected {
            let hits = specs.iter().filter(|s| s.name.as_str() == name).count();
            assert_eq!(hits, 1, "local spec '{name}' must appear exactly once");
        }
    }

    #[test]
    fn local_specs_are_tui_local_with_help() {
        for spec in local_command_specs() {
            assert_eq!(spec.origin, CommandOrigin::TuiLocal, "{}", spec.name);
            assert_eq!(spec.scope, CommandScope::TuiOnly, "{}", spec.name);
            assert!(!spec.summary.is_empty(), "{} needs a summary", spec.name);
            assert!(
                spec.usage.starts_with(&format!("/{}", spec.name)),
                "{} usage must start with its name, got {:?}",
                spec.name,
                spec.usage
            );
        }
    }

    #[test]
    fn hint_table_names_are_valid_unique_and_non_empty() -> TestResult {
        for (index, (name, usage, hints)) in HINT_TABLE.iter().enumerate() {
            CommandName::parse(*name).map_err(ctx("hinted name must be valid"))?;
            assert!(!hints.is_empty(), "{name} has an empty hint list");
            assert!(usage.starts_with(&format!("/{name}")), "{name} usage");
            assert!(
                HINT_TABLE[index + 1..].iter().all(|(n, _, _)| n != name),
                "{name} appears twice in HINT_TABLE"
            );
            for hint in *hints {
                assert!(!hint.name.is_empty() && !hint.summary.is_empty());
                assert!(!hint.name.contains(' '), "{name}: {:?}", hint.name);
            }
        }
        Ok(())
    }

    /// Drift: jeder Befehl mit Hinweisen existiert als Operation oder als
    /// echter lokaler Befehl; nur Wissens-Ops und `/models` dürfen fehlen.
    #[test]
    fn every_hinted_command_exists_as_operation_or_local() -> TestResult {
        let built_in = CommandRegistry::built_in().map_err(ctx("built_in"))?;
        for (name, _, _) in HINT_TABLE {
            let exists = built_in.find(name).is_some()
                || REAL_LOCAL.contains(name)
                || ALLOWED_MISSING.contains(name);
            assert!(
                exists,
                "hinted command '/{name}' is neither an operation nor a local command"
            );
        }
        Ok(())
    }

    /// Drift: geplante Befehle dürfen weder als Operation noch lokal existieren
    /// — sonst aus `PLANNED_COMMANDS` entfernen.
    #[test]
    fn planned_commands_are_not_yet_implemented() -> TestResult {
        let built_in = CommandRegistry::built_in().map_err(ctx("built_in"))?;
        let locals = local_command_specs();
        for (name, summary) in PLANNED_COMMANDS {
            CommandName::parse(*name).map_err(ctx("planned name must be valid"))?;
            assert!(!summary.is_empty());
            assert!(
                built_in.find(name).is_none(),
                "'/{name}' is implemented as an operation; remove it from PLANNED_COMMANDS"
            );
            assert!(
                locals.iter().all(|s| s.name.as_str() != *name),
                "'/{name}' is implemented locally; remove it from PLANNED_COMMANDS"
            );
        }
        Ok(())
    }

    #[test]
    fn subcommand_hints_follow_the_op_grammar() {
        let names =
            |cmd: &str| -> Vec<&str> { subcommand_hints(cmd).iter().map(|h| h.name).collect() };
        assert_eq!(names("model"), ["show", "list", "switch"]);
        assert_eq!(names("provider"), ["show", "list", "test"]);
        assert_eq!(names("models"), ["show", "set", "reset", "pick"]);
        assert_eq!(names("sandbox-lease"), ["status", "revoke"]);
        assert!(names("mode").contains(&"default"));
        assert!(names("tools").contains(&"profile"));
        assert!(subcommand_hints("no-such-command").is_empty());
    }

    #[test]
    fn enrich_fills_help_fields_and_is_idempotent() -> TestResult {
        let registry = CommandRegistry::built_in().map_err(ctx("built_in"))?;
        let Some(model) = registry.find("model") else {
            return Err(crate::test_support::TestError::Missing("model spec"));
        };
        let mut spec = model.clone();
        spec.summary = "x".repeat(200);
        spec.usage.clear();
        spec.subcommands.clear();
        enrich(&mut spec);
        assert!(spec.summary.chars().count() <= MAX_SUMMARY_CHARS);
        assert_eq!(spec.usage, "/model [show|list|switch <modell-id>]");
        assert_eq!(spec.subcommands.len(), 3);
        let once = spec.clone();
        enrich(&mut spec);
        assert_eq!(spec, once, "enrich must be idempotent");
        Ok(())
    }

    #[test]
    fn enrich_overrides_the_long_sandbox_lease_summary() -> TestResult {
        let mut spec = CommandSpec::new(
            "sandbox-lease",
            Vec::<String>::new(),
            CommandScope::TuiOnly,
            PermissionTier::Operator,
            OutputSurface::Inline,
            CommandDomain::Execution,
        )
        .map_err(ctx("valid spec"))?;
        spec.summary = "lang ".repeat(100);
        enrich(&mut spec);
        assert_eq!(
            spec.summary,
            "Host-Freigabe für shell.exec anzeigen oder widerrufen"
        );
        Ok(())
    }

    #[test]
    fn shorten_summary_prefers_the_first_sentence() {
        let long = format!("Das ist der erste Satz. {}", "y".repeat(200));
        assert_eq!(shorten_summary(&long), "Das ist der erste Satz");
        let abbreviation = format!("Etwa z. B. {}", "y".repeat(200));
        let cut = shorten_summary(&abbreviation);
        assert_eq!(cut.chars().count(), MAX_SUMMARY_CHARS);
        assert!(cut.ends_with('…'));
        assert_eq!(shorten_summary("kurz"), "kurz");
    }
}
