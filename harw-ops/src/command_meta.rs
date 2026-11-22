//! Befehls-Metadaten fuer TUI und Remote-Clients (Handy): Hilfe-Tabellen, Remote-Allowlist, Listen-Projektion.
//!
//! # Verantwortung
//! - Hilfe-Daten ([`HINT_TABLE`], [`SETTINGS_HINTS`], [`SUMMARY_OVERRIDES`], [`PLANNED_COMMANDS`], [`LOCAL_COMMANDS`]):
//!   unveraendert aus `harw-tui/src/command_catalog.rs` hierher verschoben. Die Daten tragen `(key, english)`; die
//!   Uebersetzung (`tr()`) bleibt in der TUI. Dieses Modul haengt nicht an `harw-tui`.
//! - Remote-Allowlist ([`REMOTE_POLICIES`]): welche Befehlsformen ein Remote-Client in welcher Scheibe und mit
//!   welchem Recht ausfuehren darf. Fail-closed: kein Eintrag bedeutet nicht remote.
//! - Parser und Aufloesung der Remote-Zeile ([`parse_remote_line`], [`resolve_remote`]).
//! - Listen-Projektion fuer `commands.list` ([`remote_command_list`], [`command_list_rev`]).
//!
//! # Nebenlaeufigkeit
//! Nur `const`-Tabellen und reine Funktionen; kein geteilter Zustand.
//!
//! # Fehlertypen
//! [`RemoteRefusal`] aus `harw-operations`.

use harw_operations::adapter::CommandAdapter;
use harw_operations::operation::{
    BusyAvailability, OperationDomain, PermissionTier, RemoteEffect, RemoteGrant, RemotePolicy,
    RemoteRefusal, RemoteSlice, RemoteVerb,
};

// ---------------------------------------------------------------------------
// Hilfe-Daten
// ---------------------------------------------------------------------------

/// Ein Unterbefehls-Hinweis; `key`/`english` sind der i18n-Schluessel und der englische Text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HintEntry {
    pub name: &'static str,
    pub args: &'static str,
    pub key: &'static str,
    pub english: &'static str,
}

impl HintEntry {
    /// Baut einen Eintrag (`const`, fuer die Tabellen).
    #[must_use]
    pub const fn new(
        name: &'static str,
        args: &'static str,
        key: &'static str,
        english: &'static str,
    ) -> Self {
        Self {
            name,
            args,
            key,
            english,
        }
    }
}

/// Nutzungszeile und Unterbefehle eines Befehls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandHelp {
    /// Name ohne fuehrendes `/`.
    pub name: &'static str,
    /// Nutzungszeile (Englisch bzw. sprachneutral).
    pub usage: &'static str,
    /// Wenn gesetzt, uebersetzt die TUI die Nutzungszeile mit `tr(usage_key, usage)`.
    /// Heute nur `matrix` mit `Some("commands.usage.matrix")`.
    pub usage_key: Option<&'static str>,
    pub hints: &'static [HintEntry],
}

// ---------------------------------------------------------------------------
// Unterkommando-Tabellen
// ---------------------------------------------------------------------------

/// `/model`, Grammatik aus `harw-ops/src/model.rs` (`show|list|switch`).
const MODEL: &[HintEntry] = &[
    HintEntry::new(
        "show",
        "",
        "commands.subcommand.model.show",
        "Show the active model",
    ),
    HintEntry::new(
        "list",
        "",
        "commands.subcommand.model.list",
        "List catalog models",
    ),
    HintEntry::new(
        "switch",
        "<model-id>",
        "commands.subcommand.model.switch",
        "Switch provider and model immediately",
    ),
];

/// `/uia-model` (`show|list|switch`).
const UIA_MODEL: &[HintEntry] = &[
    HintEntry::new(
        "show",
        "",
        "commands.subcommand.uia_model.show",
        "Show the pinned UIA model",
    ),
    HintEntry::new(
        "list",
        "",
        "commands.subcommand.uia_model.list",
        "List catalog models",
    ),
    HintEntry::new(
        "switch",
        "<model-id>",
        "commands.subcommand.uia_model.switch",
        "Switch the UIA model starting next turn",
    ),
];

/// `/uia-worker-model` (`show|list|switch`).
const UIA_WORKER_MODEL: &[HintEntry] = &[
    HintEntry::new(
        "show",
        "",
        "commands.subcommand.uia_worker_model.show",
        "Show the pinned UIA worker model",
    ),
    HintEntry::new(
        "list",
        "",
        "commands.subcommand.uia_worker_model.list",
        "List the UIA provider's models",
    ),
    HintEntry::new(
        "switch",
        "<model-id>",
        "commands.subcommand.uia_worker_model.switch",
        "Pin the UIA worker model starting next session",
    ),
];

/// `/provider` (`show|list|test`); Wechsel läuft über `/model switch`.
const PROVIDER: &[HintEntry] = &[
    HintEntry::new(
        "show",
        "",
        "commands.subcommand.provider.show",
        "Show the active provider",
    ),
    HintEntry::new(
        "list",
        "",
        "commands.subcommand.provider.list",
        "List configured providers",
    ),
    HintEntry::new(
        "test",
        "",
        "commands.subcommand.provider.test",
        "Test the active provider's connection",
    ),
];

/// `/effort` — das Level ist selbst das Unterkommando.
const EFFORT: &[HintEntry] = &[
    HintEntry::new(
        "show",
        "",
        "commands.subcommand.effort.show",
        "Show the current reasoning effort",
    ),
    HintEntry::new(
        "minimal",
        "",
        "commands.subcommand.effort.minimal",
        "Minimal reasoning effort",
    ),
    HintEntry::new(
        "low",
        "",
        "commands.subcommand.effort.low",
        "Low reasoning effort",
    ),
    HintEntry::new(
        "medium",
        "",
        "commands.subcommand.effort.medium",
        "Medium reasoning effort",
    ),
    HintEntry::new(
        "high",
        "",
        "commands.subcommand.effort.high",
        "High reasoning effort",
    ),
    HintEntry::new(
        "xhigh",
        "",
        "commands.subcommand.effort.xhigh",
        "Very high reasoning effort",
    ),
    HintEntry::new(
        "max",
        "",
        "commands.subcommand.effort.max",
        "Maximum reasoning effort",
    ),
    HintEntry::new(
        "clear",
        "",
        "commands.subcommand.effort.clear",
        "Restore the provider default",
    ),
];

/// `/uia-effort` — wie `/effort`, wirkt ab der nächsten Sitzung.
const UIA_EFFORT: &[HintEntry] = &[
    HintEntry::new(
        "show",
        "",
        "commands.subcommand.uia_effort.show",
        "Show the pinned UIA reasoning effort",
    ),
    HintEntry::new(
        "minimal",
        "",
        "commands.subcommand.uia_effort.minimal",
        "Minimal effort starting next session",
    ),
    HintEntry::new(
        "low",
        "",
        "commands.subcommand.uia_effort.low",
        "Low effort starting next session",
    ),
    HintEntry::new(
        "medium",
        "",
        "commands.subcommand.uia_effort.medium",
        "Medium effort starting next session",
    ),
    HintEntry::new(
        "high",
        "",
        "commands.subcommand.uia_effort.high",
        "High effort starting next session",
    ),
    HintEntry::new(
        "xhigh",
        "",
        "commands.subcommand.uia_effort.xhigh",
        "Very high effort starting next session",
    ),
    HintEntry::new(
        "max",
        "",
        "commands.subcommand.uia_effort.max",
        "Maximum effort starting next session",
    ),
    HintEntry::new(
        "clear",
        "",
        "commands.subcommand.uia_effort.clear",
        "Remove the pin and use the role default",
    ),
];

/// `/mode` — `show`, ein Modusname oder `default <modus>`.
const MODE: &[HintEntry] = &[
    HintEntry::new(
        "show",
        "",
        "commands.subcommand.mode.show",
        "Show the current interaction mode",
    ),
    HintEntry::new(
        "chat",
        "",
        "commands.subcommand.mode.chat",
        "Chat mode: converse without requiring tools",
    ),
    HintEntry::new(
        "plan",
        "",
        "commands.subcommand.mode.plan",
        "Plan mode: plan without making changes",
    ),
    HintEntry::new(
        "explore",
        "",
        "commands.subcommand.mode.explore",
        "Explore mode: investigate with read-only access",
    ),
    HintEntry::new(
        "work",
        "",
        "commands.subcommand.mode.work",
        "Work mode: implement with tools",
    ),
    HintEntry::new(
        "shell",
        "",
        "commands.subcommand.mode.shell",
        "Shell mode: execute commands",
    ),
    HintEntry::new(
        "default",
        "<mode>",
        "commands.subcommand.mode.default",
        "Set the default mode in configuration",
    ),
];

/// `/plan` (Runde 5, Teil F): die lokalen Plan-Modus-Befehle vorn, dazu der
/// Einstieg in die `plan`-Operation (`inspect`). Bare `/plan` schaltet den
/// Plan-Modus ein.
const PLAN: &[HintEntry] = &[
    HintEntry::new(
        "show",
        "",
        "commands.subcommand.plan.show",
        "Show the current or pinned plan",
    ),
    HintEntry::new(
        "edit",
        "",
        "commands.subcommand.plan.edit",
        "Open the current plan in $EDITOR",
    ),
    HintEntry::new(
        "list",
        "",
        "commands.subcommand.plan.list",
        "List project plans (.harw/plans)",
    ),
    HintEntry::new(
        "open",
        "<name>",
        "commands.subcommand.plan.open",
        "Load an earlier plan to continue planning",
    ),
    HintEntry::new(
        "inspect",
        "[id]",
        "commands.subcommand.plan.inspect",
        "Show the plan operation's graph",
    ),
    // Runde 5, Teil P: Plan-Store (mehrere Pläne, Freigabe, Schritte). `list`
    // zeigt hier die Plan-Dateien; die Graphen listet `plans`.
    HintEntry::new(
        "plans",
        "",
        "commands.subcommand.plan.plans",
        "List all graphs in the plan store",
    ),
    HintEntry::new(
        "switch",
        "<id>",
        "commands.subcommand.plan.switch",
        "Activate another plan graph",
    ),
    HintEntry::new(
        "archive",
        "<id>",
        "commands.subcommand.plan.archive",
        "Hide a plan graph without deleting it",
    ),
    HintEntry::new(
        "submit",
        "[id]",
        "commands.subcommand.plan.submit",
        "Confirm the proposed plan",
    ),
    HintEntry::new(
        "step",
        "<id> <open|running|done|blocked> [evidence]",
        "commands.subcommand.plan.step",
        "Report step progress (done requires evidence)",
    ),
];

/// `/models` — Modelle je Rolle.
const MODELS: &[HintEntry] = &[
    HintEntry::new(
        "show",
        "",
        "commands.subcommand.models.show",
        "Show models for all roles",
    ),
    HintEntry::new(
        "set",
        "<role> <target>",
        "commands.subcommand.models.set",
        "Set a role's model (child roles apply to new agents immediately)",
    ),
    HintEntry::new(
        "reset",
        "<role>",
        "commands.subcommand.models.reset",
        "Reset a role to its default",
    ),
    HintEntry::new(
        "pick",
        "<role>",
        "commands.subcommand.models.pick",
        "Open the model picker for a role",
    ),
    // Runde 5, Teil G: eigene Modellwahl je UIA-Worker-Rolle.
    HintEntry::new(
        "worker",
        "[<role|all> <uia|target>]",
        "commands.subcommand.models.worker",
        "Show or set UIA worker models (uia means same as UIA)",
    ),
];

/// `/permissions`, Grammatik aus `harw-ops/src/permissions.rs`.
const PERMISSIONS: &[HintEntry] = &[
    HintEntry::new(
        "show",
        "",
        "commands.subcommand.permissions.show",
        "Show approval mode, rules and directories",
    ),
    HintEntry::new(
        "mode",
        "<ask|auto|full> [--session|--project|--global]",
        "commands.subcommand.permissions.mode",
        "Set the approval mode",
    ),
    // Runde 5, Teil E: `--user` (= `--global`), `rules`, `rm`, `log`.
    HintEntry::new(
        "allow",
        "<tool> [pattern] [--session|--project|--user]",
        "commands.subcommand.permissions.allow",
        "Add an allow rule",
    ),
    HintEntry::new(
        "deny",
        "<tool> [pattern] [--session|--project|--user]",
        "commands.subcommand.permissions.deny",
        "Add a deny rule (overrides allow)",
    ),
    HintEntry::new(
        "rules",
        "",
        "commands.subcommand.permissions.rules",
        "List rules with their origins",
    ),
    HintEntry::new(
        "remove",
        "<number>",
        "commands.subcommand.permissions.remove",
        "Remove a rule by number",
    ),
    HintEntry::new(
        "rm",
        "<number>",
        "commands.subcommand.permissions.rm",
        "Remove a rule by number (short form)",
    ),
    HintEntry::new(
        "log",
        "[count]",
        "commands.subcommand.permissions.log",
        "Show recent auto-mode decisions",
    ),
];

/// `/memory`, Grammatik aus `harw-ops/src/memory.rs`.
const MEMORY: &[HintEntry] = &[
    HintEntry::new(
        "list",
        "",
        "commands.subcommand.memory.list",
        "Show hot memories",
    ),
    HintEntry::new(
        "stats",
        "",
        "commands.subcommand.memory.stats",
        "Show memory statistics",
    ),
    HintEntry::new(
        "recall",
        "<keyword…>",
        "commands.subcommand.memory.recall",
        "Search facts",
    ),
    HintEntry::new(
        "record",
        "<text…> [--project|--global]",
        "commands.subcommand.memory.record",
        "Record a fact",
    ),
    HintEntry::new(
        "forget",
        "<name>",
        "commands.subcommand.memory.forget",
        "Delete a fact",
    ),
    HintEntry::new(
        "maintain",
        "",
        "commands.subcommand.memory.maintain",
        "Clean up memory",
    ),
    HintEntry::new(
        "consolidate",
        "[--project|--global]",
        "commands.subcommand.memory.consolidate",
        "Consolidate facts",
    ),
    HintEntry::new(
        "promote",
        "<fact-id> [--project|--global] [--slug <slug>]",
        "commands.subcommand.memory.promote",
        "Promote a fact to a provisional topic",
    ),
    HintEntry::new(
        "topics",
        "",
        "commands.subcommand.memory.topics",
        "List provisional topics",
    ),
];

/// `/agent`, Grammatik aus `harw-ops/src/agent.rs`.
const AGENT: &[HintEntry] = &[
    HintEntry::new(
        "list",
        "",
        "commands.subcommand.agent.list",
        "List active child agents",
    ),
    // Plan R9, Teil C: startbare Agenten (eingebaut und benutzerdefiniert).
    HintEntry::new(
        "defs",
        "[query]",
        "commands.subcommand.agent.defs",
        "List launchable agents with their origins",
    ),
    HintEntry::new(
        "stop",
        "<agent-id>",
        "commands.subcommand.agent.stop",
        "Cancel child agents",
    ),
    HintEntry::new(
        "budget",
        "[agent-id]",
        "commands.subcommand.agent.budget",
        "Show budget and lease",
    ),
    // Runde 5, Teil I: TUI-lokal abgefangen (vor der Operation `/agent`).
    HintEntry::new(
        "stream",
        "<orchestrators|all|none>",
        "commands.subcommand.agent.stream",
        "Stream child agents live in history (session)",
    ),
    // Runde 5, Teil K: TUI-lokal abgefangen (Hintergrund-Agenten).
    HintEntry::new(
        "bg",
        "",
        "commands.subcommand.agent.bg",
        "List background agents with progress",
    ),
    HintEntry::new(
        "cancel",
        "<agent-id>",
        "commands.subcommand.agent.cancel",
        "Cancel background agents",
    ),
];

/// `/export` — Optionen statt Unterkommandos (`harw-ops/src/export.rs`).
const EXPORT: &[HintEntry] = &[
    HintEntry::new(
        "--format",
        "<md|json>",
        "commands.subcommand.export.format",
        "Choose the output format",
    ),
    HintEntry::new(
        "--datei",
        "<path>",
        "commands.subcommand.export.datei",
        "Write to a file",
    ),
    HintEntry::new(
        "--tools",
        "",
        "commands.subcommand.export.tools",
        "Include tool calls",
    ),
    HintEntry::new(
        "--no-tools",
        "",
        "commands.subcommand.export.no_tools",
        "Omit tool calls",
    ),
    HintEntry::new(
        "--reasoning-summary",
        "",
        "commands.subcommand.export.reasoning_summary",
        "Include the reasoning summary",
    ),
    HintEntry::new(
        "--max-chars",
        "<n>",
        "commands.subcommand.export.max_chars",
        "Limit the length",
    ),
];

/// `/sandbox-lease` — auf der Befehlsfläche nur `status`/`revoke`
/// (`request` braucht einen Grund und läuft über das Modell-Werkzeug).
const SANDBOX_LEASE: &[HintEntry] = &[
    HintEntry::new(
        "status",
        "",
        "commands.subcommand.sandbox_lease.status",
        "Show the active host permit",
    ),
    HintEntry::new(
        "revoke",
        "",
        "commands.subcommand.sandbox_lease.revoke",
        "Revoke the host permit immediately",
    ),
];

/// `/workbench`, Grammatik aus `harw-ops/src/workbench.rs`; jeder
/// Subcommand akzeptiert `--scope=session|project|project:<slug>`.
const WORKBENCH: &[HintEntry] = &[
    HintEntry::new(
        "show",
        "[--scope=session|project]",
        "commands.subcommand.workbench.show",
        "Show the workbench",
    ),
    HintEntry::new(
        "pin",
        "<path> [note]",
        "commands.subcommand.workbench.pin",
        "Pin a file",
    ),
    HintEntry::new(
        "unpin",
        "<path>",
        "commands.subcommand.workbench.unpin",
        "Unpin a file",
    ),
    HintEntry::new(
        "note",
        "<text>",
        "commands.subcommand.workbench.note",
        "Append a note",
    ),
    HintEntry::new(
        "note",
        "edit <n> <text> | rm <n>",
        "commands.subcommand.workbench.note_edit",
        "Edit or delete a note",
    ),
    HintEntry::new(
        "hypothesis",
        "add|confirm|reject <text>",
        "commands.subcommand.workbench.hypothesis",
        "Create or decide a hypothesis",
    ),
    HintEntry::new(
        "retention",
        "[keep|<days>d] [--scope=project]",
        "commands.subcommand.workbench.retention",
        "Show or set retention",
    ),
];

/// `/kanban`, Grammatik aus `harw-ops/src/kanban.rs` (`--board=<id>` davor
/// möglich).
const KANBAN: &[HintEntry] = &[
    HintEntry::new(
        "show",
        "[card]",
        "commands.subcommand.kanban.show",
        "Show the board or a card",
    ),
    HintEntry::new(
        "list",
        "",
        "commands.subcommand.kanban.list",
        "List board cards",
    ),
    HintEntry::new(
        "boards",
        "",
        "commands.subcommand.kanban.boards",
        "List boards",
    ),
    HintEntry::new(
        "add",
        "<title>",
        "commands.subcommand.kanban.add",
        "Create a card",
    ),
    HintEntry::new(
        "move",
        "<card> <todo|ready|running|done|blocked|archived>",
        "commands.subcommand.kanban.move",
        "Move a card",
    ),
    HintEntry::new(
        "todo",
        "<card>",
        "commands.subcommand.kanban.todo",
        "Set a card to todo",
    ),
    HintEntry::new(
        "ready",
        "<card>",
        "commands.subcommand.kanban.ready",
        "Mark a card ready",
    ),
    HintEntry::new(
        "claim",
        "<card>",
        "commands.subcommand.kanban.claim",
        "Claim a card",
    ),
    HintEntry::new(
        "block",
        "<card> <reason>",
        "commands.subcommand.kanban.block",
        "Block a card",
    ),
    HintEntry::new(
        "unblock",
        "<card>",
        "commands.subcommand.kanban.unblock",
        "Unblock a card",
    ),
    HintEntry::new(
        "done",
        "<card>",
        "commands.subcommand.kanban.done",
        "Complete a card",
    ),
    HintEntry::new(
        "archive",
        "<card>",
        "commands.subcommand.kanban.archive",
        "Archive a card",
    ),
    HintEntry::new(
        "edit",
        "<card> <text>",
        "commands.subcommand.kanban.edit",
        "Replace card text",
    ),
    HintEntry::new(
        "comment",
        "<card> <text>",
        "commands.subcommand.kanban.comment",
        "Append a comment",
    ),
    HintEntry::new(
        "evidence",
        "<card> <path|url>",
        "commands.subcommand.kanban.evidence",
        "Link evidence",
    ),
    HintEntry::new(
        "approve",
        "<card> [note]",
        "commands.subcommand.kanban.approve",
        "Approve a worker card",
    ),
    HintEntry::new(
        "reject",
        "<card> [reason]",
        "commands.subcommand.kanban.reject",
        "Reject a worker card",
    ),
];

/// `/palace`, Grammatik aus `harw-ops/src/palace.rs` plus `list` (Vertrag).
const PALACE: &[HintEntry] = &[
    HintEntry::new("list", "", "commands.subcommand.palace.list", "List nodes"),
    HintEntry::new(
        "show",
        "<id>",
        "commands.subcommand.palace.show",
        "Show a node",
    ),
    HintEntry::new(
        "search",
        "<query> [--max-hops=n] [--max=n]",
        "commands.subcommand.palace.search",
        "Search the graph",
    ),
    HintEntry::new(
        "promote",
        "<topic>",
        "commands.subcommand.palace.promote",
        "Promote a topic to the palace (established)",
    ),
    HintEntry::new(
        "supersede",
        "<old> <new> [--confirm]",
        "commands.subcommand.palace.supersede",
        "Replace a node with its successor",
    ),
    HintEntry::new(
        "edit",
        "<id> <text…> [--confirm]",
        "commands.subcommand.palace.edit",
        "Replace node text (new revision)",
    ),
    HintEntry::new(
        "link",
        "<a> <b> [--confirm]",
        "commands.subcommand.palace.link",
        "Add a link from a to b",
    ),
];

/// `/dream`, Grammatik aus `harw-ops/src/dream.rs` (D5).
const DREAM: &[HintEntry] = &[
    HintEntry::new(
        "list",
        "",
        "commands.subcommand.dream.list",
        "List dream reports",
    ),
    HintEntry::new(
        "show",
        "<id>",
        "commands.subcommand.dream.show",
        "Show a dream report",
    ),
    HintEntry::new(
        "run",
        "",
        "commands.subcommand.dream.run",
        "Start a dream run",
    ),
    HintEntry::new(
        "status",
        "",
        "commands.subcommand.dream.status",
        "Show dream run status",
    ),
    HintEntry::new(
        "review",
        "[<id>] | <id> accept|reject <p-id> [reason]",
        "commands.subcommand.dream.review",
        "Review, accept or reject proposals",
    ),
];

/// `/learn`, Grammatik aus `harw-ops/src/learn.rs`: schlägt nur vor, jede
/// Übernahme braucht ein ausdrückliches `accept`.
const LEARN: &[HintEntry] = &[
    HintEntry::new(
        "scan",
        "",
        "commands.subcommand.learn.scan",
        "Scan the session for learning candidates",
    ),
    HintEntry::new(
        "note",
        "<text> [--target memory|skill|agent]",
        "commands.subcommand.learn.note",
        "Create a proposal from your own text",
    ),
    HintEntry::new(
        "list",
        "[--all]",
        "commands.subcommand.learn.list",
        "List proposals",
    ),
    HintEntry::new(
        "show",
        "<id>",
        "commands.subcommand.learn.show",
        "Show a proposal",
    ),
    HintEntry::new(
        "accept",
        "<id>",
        "commands.subcommand.learn.accept",
        "Accept a proposal",
    ),
    HintEntry::new(
        "reject",
        "<id> [reason]",
        "commands.subcommand.learn.reject",
        "Reject a proposal",
    ),
];

/// `/diary`, Grammatik aus `harw-ops/src/diary.rs` plus `today` (Vertrag).
const DIARY: &[HintEntry] = &[
    HintEntry::new(
        "today",
        "",
        "commands.subcommand.diary.today",
        "Show today's entries",
    ),
    HintEntry::new(
        "show",
        "[agent] [--date=YYYY-MM-DD | --from=… [--to=…]]",
        "commands.subcommand.diary.show",
        "Show entries for a day or date range",
    ),
    HintEntry::new(
        "search",
        "<text> [--agent=<id>] [--from=…] [--to=…]",
        "commands.subcommand.diary.search",
        "Search diaries",
    ),
    HintEntry::new(
        "agents",
        "",
        "commands.subcommand.diary.agents",
        "List agents with diaries",
    ),
    HintEntry::new(
        "note",
        "<text>",
        "commands.subcommand.diary.note",
        "Write a diary note",
    ),
];

/// `/matrix`, Grammatik der Matrix-Game-Operation (`harw-ops/src/matrix`).
/// Runde 7, Teil M: nur noch Ansicht — Start und Steuerung übernimmt der
/// Game Master (`matrix-game-master`), den die UIA im Hintergrund startet.
const MATRIX: &[HintEntry] = &[
    HintEntry::new(
        "show",
        "",
        "commands.subcommand.matrix.show",
        "Show the running game (panel: F9)",
    ),
    HintEntry::new(
        "list",
        "",
        "commands.subcommand.matrix.list",
        "List scenarios, drafts and runs",
    ),
    HintEntry::new(
        "replay",
        "",
        "commands.subcommand.matrix.replay",
        "Verify the journal deterministically",
    ),
    HintEntry::new(
        "compare",
        "<run> <run> …",
        "commands.subcommand.matrix.compare",
        "Compare runs for design lessons",
    ),
];

/// `/jobs`, Grammatik aus `harw-ops/src/jobs.rs` (Plan R9, Teil F).
const JOBS: &[HintEntry] = &[
    HintEntry::new(
        "list",
        "",
        "commands.subcommand.jobs.list",
        "List this session's background jobs",
    ),
    HintEntry::new(
        "show",
        "<id>",
        "commands.subcommand.jobs.show",
        "Show status, command and recent lines",
    ),
    HintEntry::new(
        "stop",
        "<id> [TERM|INT|HUP|KILL]",
        "commands.subcommand.jobs.stop",
        "Stop a job and its process group",
    ),
    HintEntry::new(
        "logs",
        "<id> [n]",
        "commands.subcommand.jobs.logs",
        "Show recent log lines (stdout/stderr)",
    ),
];

/// `/tools`, Grammatik aus `crate::tools_command`.
const TOOLS: &[HintEntry] = &[
    HintEntry::new(
        "on",
        "<name>",
        "commands.subcommand.tools.on",
        "Enable a tool",
    ),
    HintEntry::new(
        "off",
        "<name>",
        "commands.subcommand.tools.off",
        "Disable a tool",
    ),
    HintEntry::new(
        "reset",
        "[name]",
        "commands.subcommand.tools.reset",
        "Reset overrides",
    ),
    HintEntry::new(
        "profile",
        "<minimal|coding|full>",
        "commands.subcommand.tools.profile",
        "Choose a tool profile",
    ),
];

/// Befehl → (Nutzungszeile, Unterkommandos). Einzige Quelle fuer die Unterkommando-Hinweise
/// und die Nutzungszeilen (ehemals `HINT_TABLE` in `harw-tui`).
pub const HINT_TABLE: &[CommandHelp] = &[
    CommandHelp {
        name: "settings",
        usage: "/settings [show|set <key> <value>|reset <key>]",
        usage_key: None,
        hints: &[],
    },
    CommandHelp {
        name: "model",
        usage: "/model [show|list|switch <model-id>]",
        usage_key: None,
        hints: MODEL,
    },
    CommandHelp {
        name: "uia-model",
        usage: "/uia-model [show|list|switch <model-id>]",
        usage_key: None,
        hints: UIA_MODEL,
    },
    CommandHelp {
        name: "uia-worker-model",
        usage: "/uia-worker-model [show|list|switch <model-id>]",
        usage_key: None,
        hints: UIA_WORKER_MODEL,
    },
    CommandHelp {
        name: "provider",
        usage: "/provider [show|list|test]",
        usage_key: None,
        hints: PROVIDER,
    },
    CommandHelp {
        name: "effort",
        usage: "/effort [show|minimal|low|medium|high|xhigh|max|clear]",
        usage_key: None,
        hints: EFFORT,
    },
    CommandHelp {
        name: "uia-effort",
        usage: "/uia-effort [show|minimal|low|medium|high|xhigh|max|clear]",
        usage_key: None,
        hints: UIA_EFFORT,
    },
    CommandHelp {
        name: "mode",
        usage: "/mode [show|<mode>|default <mode>]",
        usage_key: None,
        hints: MODE,
    },
    CommandHelp {
        name: "plan",
        usage: "/plan [show|edit|list|open <name>|inspect|plans|switch|submit|step …]",
        usage_key: None,
        hints: PLAN,
    },
    CommandHelp {
        name: "models",
        usage: "/models [show|set <role> <target>|reset <role>|pick <role>|worker [<role|all> <uia|target>]]",
        usage_key: None,
        hints: MODELS,
    },
    CommandHelp {
        name: "permissions",
        usage: "/permissions [show|mode <mode>|allow <tool>|deny <tool>|rules|rm <number>|log]",
        usage_key: None,
        hints: PERMISSIONS,
    },
    CommandHelp {
        name: "memory",
        usage: "/memory [list|stats|recall|record|forget|promote|topics|maintain|consolidate] …",
        usage_key: None,
        hints: MEMORY,
    },
    CommandHelp {
        name: "agent",
        usage: "/agent [list|defs [query]|stop <agent-id>|budget [agent-id]|stream <orchestrators|all|none>|bg|cancel <agent-id>]",
        usage_key: None,
        hints: AGENT,
    },
    CommandHelp {
        name: "export",
        usage: "/export [--format <md|json>] [--datei <path>] [--no-tools] …",
        usage_key: None,
        hints: EXPORT,
    },
    CommandHelp {
        name: "sandbox-lease",
        usage: "/sandbox-lease [status|revoke]",
        usage_key: None,
        hints: SANDBOX_LEASE,
    },
    CommandHelp {
        name: "workbench",
        usage: "/workbench [show|pin|unpin|note [edit|rm]|hypothesis|retention] [--scope=…] …",
        usage_key: None,
        hints: WORKBENCH,
    },
    CommandHelp {
        name: "kanban",
        usage: "/kanban [--board=<id>] [show|list|boards|add|move|block|done|…] …",
        usage_key: None,
        hints: KANBAN,
    },
    CommandHelp {
        name: "palace",
        usage: "/palace [list|show <id>|search <query>|promote <topic>|supersede|edit|link] …",
        usage_key: None,
        hints: PALACE,
    },
    CommandHelp {
        name: "dream",
        usage: "/dream [list|show <id>|run|status|review [<id>] [accept|reject <p-id>]]",
        usage_key: None,
        hints: DREAM,
    },
    CommandHelp {
        name: "learn",
        usage: "/learn [scan|note <text>|list [--all]|show <id>|accept <id>|reject <id>]",
        usage_key: None,
        hints: LEARN,
    },
    CommandHelp {
        name: "diary",
        usage: "/diary [today|show [agent] [--date|--from/--to]|search <text>|agents|note <text>]",
        usage_key: None,
        hints: DIARY,
    },
    CommandHelp {
        name: "matrix",
        usage: "/matrix [show|list|replay|compare <run> <run>] — Start via the UIA (Game Master)",
        usage_key: Some("commands.usage.matrix"),
        hints: MATRIX,
    },
    CommandHelp {
        name: "jobs",
        usage: "/jobs [list|show <id>|stop <id> [signal]|logs <id> [n]]",
        usage_key: None,
        hints: JOBS,
    },
    CommandHelp {
        name: "tools",
        usage: "/tools [on <name>|off <name>|reset [name]|profile <p>]",
        usage_key: None,
        hints: TOOLS,
    },
];

/// Unterbefehle von `/settings` (ehemals hart im `enrich` der TUI).
pub const SETTINGS_HINTS: &[HintEntry] = &[
    HintEntry::new("show", "", "settings.title", "Settings"),
    HintEntry::new(
        "set",
        "<key> <value>",
        "settings.scope",
        "Active profile · applies immediately",
    ),
    HintEntry::new(
        "reset",
        "<key>",
        "settings.scope",
        "Active profile · applies immediately",
    ),
];

/// Zusammenfassung von `/settings`: `(key, english)`.
pub const SETTINGS_SUMMARY: (&str, &str) = ("settings.title", "Settings");

/// Kurzbeschreibungen, die zu lange oder veraltete Operations-Texte ersetzen.
pub const SUMMARY_OVERRIDES: &[(&str, &str, &str)] = &[
    (
        "sandbox-lease",
        "commands.summary.sandbox_lease",
        "Show or revoke the host permit for shell.exec",
    ),
    (
        "mode",
        "commands.summary.mode",
        "Show or change the interaction mode",
    ),
    (
        "memory",
        "commands.summary.memory",
        "Long-term memory: list, search, record, forget",
    ),
    (
        "agent",
        "commands.summary.agent",
        "Child agents: tree, list, stop, budget, stream",
    ),
];

/// Befehle aus dem Interaktionsvertrag, die (noch) weder als Operation noch
/// TUI-lokal existieren: `(name, message key, English summary)`.
pub const PLANNED_COMMANDS: &[(&str, &str, &str)] = &[
    (
        "rewind",
        "commands.planned.rewind",
        "Restore the session to an earlier state",
    ),
    (
        "fork",
        "commands.planned.fork",
        "Branch the session from here",
    ),
    ("archive", "commands.planned.archive", "Archive the session"),
    (
        "spawn",
        "commands.planned.spawn",
        "Launch specific child agents",
    ),
    (
        "mention",
        "commands.planned.mention",
        "Mention an agent or file",
    ),
    (
        "kill",
        "commands.planned.kill",
        "Terminate child agents immediately",
    ),
    ("logs", "commands.planned.logs", "Show logs"),
    (
        "doctor",
        "commands.planned.doctor",
        "Check installation and configuration",
    ),
    (
        "config",
        "commands.planned.config",
        "Show and change configuration",
    ),
    ("mcp", "commands.planned.mcp", "Manage MCP servers"),
    ("channels", "commands.planned.channels", "Manage channels"),
    ("lease", "commands.planned.lease", "Show and manage leases"),
    (
        "priority",
        "commands.planned.priority",
        "Set a task's priority",
    ),
    (
        "depends",
        "commands.planned.depends",
        "Set dependencies between tasks",
    ),
    ("theme", "commands.planned.theme", "Change the color scheme"),
];

/// Ein TUI-lokaler Befehl oder eine Ersatz-Spezifikation (ehemals die `local_spec(...)`-Aufrufe der TUI).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalCommandMeta {
    pub name: &'static str,
    /// `Session` steht fuer TUI-`CommandDomain::SessionLifecycle`; sonst `Execution` oder `Misc`.
    pub domain: OperationDomain,
    pub permission: PermissionTier,
    pub busy: BusyAvailability,
    pub key: &'static str,
    pub english: &'static str,
    /// Leer = aus [`HINT_TABLE`] bzw. `"/<name>"`.
    pub usage: &'static str,
    /// `true` fuer die Ersatz-Specs `mode`, `matrix`, `plan`.
    pub fallback: bool,
}

#[allow(clippy::too_many_arguments)]
const fn local(
    name: &'static str,
    domain: OperationDomain,
    permission: PermissionTier,
    busy: BusyAvailability,
    key: &'static str,
    english: &'static str,
    usage: &'static str,
    fallback: bool,
) -> LocalCommandMeta {
    LocalCommandMeta {
        name,
        domain,
        permission,
        busy,
        key,
        english,
        usage,
        fallback,
    }
}

/// TUI-lokale Befehle, danach die Ersatz-Spezifikationen (`mode`, `matrix`, `plan`).
pub const LOCAL_COMMANDS: &[LocalCommandMeta] = &[
    local(
        "tools",
        OperationDomain::Execution,
        PermissionTier::Operator,
        BusyAvailability::DeferredUntilTurnEnd,
        "commands.local.tools",
        "Show tools, enable or disable them, choose a profile",
        "",
        false,
    ),
    local(
        "resume",
        OperationDomain::Session,
        PermissionTier::Operator,
        BusyAvailability::DeferredUntilTurnEnd,
        "commands.local.resume",
        "Resume an earlier session (without an ID: choose one)",
        "/resume [session-id]",
        false,
    ),
    local(
        "sessions",
        OperationDomain::Session,
        PermissionTier::Observer,
        BusyAvailability::DeferredUntilTurnEnd,
        "commands.local.sessions",
        "Open the session picker",
        "/sessions",
        false,
    ),
    local(
        "exit",
        OperationDomain::Session,
        PermissionTier::Observer,
        BusyAvailability::DeferredUntilTurnEnd,
        "commands.local.exit",
        "Exit the TUI (same as /quit)",
        "/exit",
        false,
    ),
    local(
        "clear",
        OperationDomain::Session,
        PermissionTier::Observer,
        BusyAvailability::DeferredUntilTurnEnd,
        "commands.local.clear",
        "Clear the display (keep the session)",
        "/clear",
        false,
    ),
    local(
        "verbose",
        OperationDomain::Misc,
        PermissionTier::Observer,
        BusyAvailability::Immediate,
        "commands.local.verbose",
        "Toggle detailed tool display",
        "/verbose",
        false,
    ),
    local(
        "keys",
        OperationDomain::Misc,
        PermissionTier::Observer,
        BusyAvailability::Immediate,
        "commands.local.keys",
        "Show key bindings",
        "/keys",
        false,
    ),
    local(
        "whoami",
        OperationDomain::Misc,
        PermissionTier::Observer,
        BusyAvailability::Immediate,
        "commands.local.whoami",
        "Show session, permission and active model",
        "/whoami",
        false,
    ),
    local(
        "rename",
        OperationDomain::Session,
        PermissionTier::Operator,
        BusyAvailability::Immediate,
        "commands.local.rename",
        "Rename the session",
        "/rename <title>",
        false,
    ),
    local(
        "btw",
        OperationDomain::Misc,
        PermissionTier::Operator,
        BusyAvailability::Immediate,
        "commands.local.btw",
        "Ask a side question without interrupting the agent (not in history)",
        "/btw <question>",
        false,
    ),
    local(
        "image",
        OperationDomain::Misc,
        PermissionTier::Operator,
        BusyAvailability::Immediate,
        "commands.local.image",
        "Attach an image to the next message (PNG, JPEG, GIF, WebP)",
        "/image <path>",
        false,
    ),
    local(
        "mode",
        OperationDomain::Session,
        PermissionTier::Operator,
        BusyAvailability::Staged,
        "commands.summary.mode",
        "Show or change the interaction mode",
        "",
        true,
    ),
    local(
        "matrix",
        OperationDomain::Misc,
        PermissionTier::Operator,
        BusyAvailability::DeferredUntilTurnEnd,
        "commands.local.matrix",
        "View the Matrix game (UIA/Game Master starts and controls it; bare command opens panel)",
        "",
        true,
    ),
    local(
        "plan",
        OperationDomain::Session,
        PermissionTier::Operator,
        BusyAvailability::Immediate,
        "commands.local.plan",
        "Enable plan mode; show, edit or open plans",
        "",
        true,
    ),
];

/// Hilfe zu einem Namen (ohne `/`).
#[must_use]
pub fn help_for(name: &str) -> Option<&'static CommandHelp> {
    HINT_TABLE.iter().find(|help| help.name == name)
}

/// Englische Zusammenfassungs-Ueberschreibung (ohne `/`): `(key, english)`.
#[must_use]
pub fn summary_override(name: &str) -> Option<(&'static str, &'static str)> {
    SUMMARY_OVERRIDES
        .iter()
        .find(|(n, _, _)| *n == name)
        .map(|(_, key, english)| (*key, *english))
}

// ---------------------------------------------------------------------------
// Remote-Zeile
// ---------------------------------------------------------------------------

/// Freigeschaltete Scheibe. B4: Session-Steuerung; B5 setzt `RemoteSlice::Global`, sobald Step-up (C9a) gemergt ist.
pub const REMOTE_ENABLED_SLICE: RemoteSlice = RemoteSlice::Session;
/// Maximale Laenge einer Remote-Zeile in Byte.
pub const REMOTE_LINE_MAX_BYTES: usize = 512;
/// Maximale Zahl Tokens einer Remote-Zeile, Befehlsname eingeschlossen.
pub const REMOTE_LINE_MAX_TOKENS: usize = 32;

/// Geparste Remote-Zeile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteLine {
    /// Befehlsname ohne `/`, wie getippt (kann ein Alias sein).
    pub name: String,
    pub args: Vec<String>,
}

/// Parst eine Remote-Zeile.
///
/// Regeln: getrimmt beginnt sie mit `/`; hoechstens 512 Byte; kein Steuerzeichen (auch kein `\n`, `\t`);
/// Trennung an ASCII-Whitespace, keine Quotes, keine Escapes; Name nicht leer; hoechstens 32 Tokens
/// einschliesslich Name.
///
/// # Errors
/// [`RemoteRefusal::LineInvalid`], wenn eine Regel verletzt ist.
pub fn parse_remote_line(line: &str) -> Result<RemoteLine, RemoteRefusal> {
    if line.len() > REMOTE_LINE_MAX_BYTES {
        return Err(RemoteRefusal::LineInvalid);
    }
    let trimmed = line.trim();
    if !trimmed.starts_with('/') || trimmed.chars().any(char::is_control) {
        return Err(RemoteRefusal::LineInvalid);
    }
    let mut tokens = trimmed.split_ascii_whitespace();
    let name = tokens
        .next()
        .and_then(|t| t.strip_prefix('/'))
        .filter(|n| !n.is_empty())
        .ok_or(RemoteRefusal::LineInvalid)?
        .to_owned();
    let args: Vec<String> = tokens.map(str::to_owned).collect();
    if args.len() + 1 > REMOTE_LINE_MAX_TOKENS {
        return Err(RemoteRefusal::LineInvalid);
    }
    Ok(RemoteLine { name, args })
}

// ---------------------------------------------------------------------------
// Remote-Allowlist
// ---------------------------------------------------------------------------

const NO_DENIED: &[&str] = &[];

const STATUS: RemotePolicy = RemotePolicy {
    path: "/status",
    verbs: &[RemoteVerb::read(&["*"])],
    denied_tokens: NO_DENIED,
};
const USAGE: RemotePolicy = RemotePolicy {
    path: "/usage",
    verbs: &[RemoteVerb::read(&["*"])],
    denied_tokens: NO_DENIED,
};
const HELP: RemotePolicy = RemotePolicy {
    path: "/help",
    verbs: &[RemoteVerb::read(&["*"])],
    denied_tokens: NO_DENIED,
};
const WORK: RemotePolicy = RemotePolicy {
    path: "/work",
    verbs: &[RemoteVerb::read(&["*"])],
    denied_tokens: NO_DENIED,
};
const DIFF: RemotePolicy = RemotePolicy {
    path: "/diff",
    verbs: &[RemoteVerb::read(&["*"])],
    denied_tokens: NO_DENIED,
};
const GOAL: RemotePolicy = RemotePolicy {
    path: "/goal",
    verbs: &[RemoteVerb::read(&["-"]), RemoteVerb::read(&["show", "-"])],
    denied_tokens: NO_DENIED,
};
const PLAN_REMOTE: RemotePolicy = RemotePolicy {
    path: "/plan",
    verbs: &[RemoteVerb::read(&["show", "-"])],
    denied_tokens: NO_DENIED,
};
const MEMORY_REMOTE: RemotePolicy = RemotePolicy {
    path: "/memory",
    verbs: &[RemoteVerb::read(&["list", "*"])],
    denied_tokens: NO_DENIED,
};
const SKILLS: RemotePolicy = RemotePolicy {
    path: "/skills",
    verbs: &[
        RemoteVerb::read(&["-"]),
        RemoteVerb::read(&["list", "-"]),
        RemoteVerb::read(&["show", "*"]),
    ],
    denied_tokens: NO_DENIED,
};
const AGENT_REMOTE: RemotePolicy = RemotePolicy {
    path: "/agent",
    verbs: &[
        RemoteVerb::read(&["-"]),
        RemoteVerb::read(&["list", "-"]),
        RemoteVerb::steer(&["stop", "*"], RemoteEffect::Applied),
    ],
    denied_tokens: NO_DENIED,
};
const STOP: RemotePolicy = RemotePolicy {
    path: "/stop",
    verbs: &[RemoteVerb::steer(&["*"], RemoteEffect::Applied)],
    denied_tokens: NO_DENIED,
};
const CANCEL: RemotePolicy = RemotePolicy {
    path: "/cancel",
    verbs: &[RemoteVerb::steer(&["*"], RemoteEffect::Applied)],
    denied_tokens: NO_DENIED,
};
const MODE_REMOTE: RemotePolicy = RemotePolicy {
    path: "/mode",
    verbs: &[
        RemoteVerb::read(&["-"]),
        RemoteVerb::read(&["show", "-"]),
        RemoteVerb::global(&["default", "*"], RemoteEffect::NextSession),
        RemoteVerb::control(&["*"], RemoteEffect::Staged),
    ],
    denied_tokens: NO_DENIED,
};
const EFFORT_REMOTE: RemotePolicy = RemotePolicy {
    path: "/effort",
    verbs: &[
        RemoteVerb::read(&["-"]),
        RemoteVerb::read(&["show", "-"]),
        RemoteVerb::control(&["*"], RemoteEffect::Staged),
    ],
    denied_tokens: NO_DENIED,
};
const MODEL_REMOTE: RemotePolicy = RemotePolicy {
    path: "/model",
    verbs: &[
        RemoteVerb::read(&["-"]),
        RemoteVerb::read(&["show", "-"]),
        RemoteVerb::read(&["list", "-"]),
        RemoteVerb::control(&["switch", "*"], RemoteEffect::Staged),
    ],
    denied_tokens: NO_DENIED,
};

/// Alle Policies; Reihenfolge egal, Pfade eindeutig. `/models`, `/jobs`, `/settings`, `/provider` liegen bei ihren Ops;
/// `/provider-concurrency` und `/uia-provider` sind nicht remote.
pub static REMOTE_POLICIES: &[&RemotePolicy] = &[
    &STATUS,
    &USAGE,
    &HELP,
    &WORK,
    &DIFF,
    &GOAL,
    &PLAN_REMOTE,
    &MEMORY_REMOTE,
    &SKILLS,
    &AGENT_REMOTE,
    &STOP,
    &CANCEL,
    &MODE_REMOTE,
    &EFFORT_REMOTE,
    &MODEL_REMOTE,
    &crate::models::REMOTE,
    &crate::jobs::REMOTE,
    &crate::settings::REMOTE,
    &crate::provider::REMOTE,
];

/// Policy zum Pfad (z. B. `"/models"`), `None` wenn nicht remote.
#[must_use]
pub fn remote_policy(path: &str) -> Option<&'static RemotePolicy> {
    REMOTE_POLICIES.iter().copied().find(|p| p.path == path)
}

/// Ergebnis der Aufloesung fuer `command.run`.
pub struct RemoteResolved<'a> {
    pub adapter: &'a CommandAdapter,
    pub args: Vec<String>,
    pub verb: &'static RemoteVerb,
}

fn adapter_matches(adapter: &CommandAdapter, name: &str) -> bool {
    adapter.path().strip_prefix('/') == Some(name)
        || adapter
            .operation()
            .meta()
            .aliases
            .iter()
            .any(|alias| *alias == name)
}

/// Parst die Zeile, sucht den Adapter und prueft die Allowlist.
///
/// Der erste Adapter mit passendem Pfad oder Alias gewinnt (wie in der TUI-Registry). Kein Adapter, aber ein
/// Name aus [`LOCAL_COMMANDS`]: `NotRemote`; sonst `UnknownCommand`.
///
/// # Errors
/// Die jeweilige [`RemoteRefusal`].
pub fn resolve_remote<'a>(
    adapters: &'a [CommandAdapter],
    line: &str,
    grant: &RemoteGrant,
) -> Result<RemoteResolved<'a>, RemoteRefusal> {
    let RemoteLine { name, args } = parse_remote_line(line)?;
    let Some(adapter) = adapters.iter().find(|a| adapter_matches(a, &name)) else {
        return Err(if LOCAL_COMMANDS.iter().any(|l| l.name == name) {
            RemoteRefusal::NotRemote
        } else {
            RemoteRefusal::UnknownCommand { name }
        });
    };
    let verb = adapter.allows_remote_invocation(
        remote_policy(adapter.path()),
        grant,
        REMOTE_ENABLED_SLICE,
        &args,
    )?;
    Ok(RemoteResolved {
        adapter,
        args,
        verb,
    })
}

// ---------------------------------------------------------------------------
// Listen-Projektion
// ---------------------------------------------------------------------------

/// Herkunft wie TUI `CommandOrigin`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandOriginKind {
    Operation,
    TuiLocal,
}

/// Unterbefehl in der Liste.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct HintView {
    pub name: String,
    pub args: String,
    pub summary: String,
}

/// Ein Befehl in `commands.list`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct RemoteCommandView {
    /// Ohne `/`.
    pub name: String,
    pub aliases: Vec<String>,
    pub summary: String,
    pub usage: String,
    pub domain: OperationDomain,
    pub permission: PermissionTier,
    /// `immediate`, `staged` oder `deferred`.
    pub busy: &'static str,
    pub origin: CommandOriginKind,
    /// Es gibt eine `RemotePolicy` fuer den Pfad.
    pub remote: bool,
    /// `remote_availability` ist `Ok`.
    pub available: bool,
    pub reason: Option<String>,
    pub reason_code: Option<&'static str>,
    pub subcommands: Vec<HintView>,
}

/// `BusyAvailability` als Wire-Text.
#[must_use]
pub const fn busy_str(busy: BusyAvailability) -> &'static str {
    match busy {
        BusyAvailability::Immediate => "immediate",
        BusyAvailability::Staged => "staged",
        BusyAvailability::DeferredUntilTurnEnd => "deferred",
    }
}

fn hint_views(hints: &[HintEntry]) -> Vec<HintView> {
    hints
        .iter()
        .map(|h| HintView {
            name: h.name.to_owned(),
            args: h.args.to_owned(),
            summary: h.english.to_owned(),
        })
        .collect()
}

fn usage_of(name: &str) -> String {
    help_for(name).map_or_else(|| format!("/{name}"), |h| h.usage.to_owned())
}

/// Alle Befehle wie im TUI-Popup: je Adapter-Pfad einer (erster gewinnt), dazu die [`LOCAL_COMMANDS`], deren Name
/// weder Pfad noch Alias einer Operation ist. Sortiert nach `name`.
#[must_use]
pub fn remote_command_list(
    adapters: &[CommandAdapter],
    grant: &RemoteGrant,
) -> Vec<RemoteCommandView> {
    let mut views: Vec<RemoteCommandView> = Vec::new();
    for adapter in adapters {
        let name = adapter.path().trim_start_matches('/');
        if name.is_empty() || views.iter().any(|v| v.name == name) {
            continue;
        }
        let meta = adapter.operation().meta();
        let policy = remote_policy(adapter.path());
        let availability = adapter.remote_availability(policy, grant, REMOTE_ENABLED_SLICE);
        let (summary, subcommands) = if name == "settings" {
            (SETTINGS_SUMMARY.1, hint_views(SETTINGS_HINTS))
        } else {
            (
                summary_override(name).map_or(meta.summary, |(_, english)| english),
                help_for(name).map_or_else(Vec::new, |h| hint_views(h.hints)),
            )
        };
        let (reason, reason_code) = match &availability {
            Ok(()) => (None, None),
            Err(refusal) => (Some(refusal.reason_de()), Some(refusal.code())),
        };
        views.push(RemoteCommandView {
            name: name.to_owned(),
            aliases: meta.aliases.iter().map(|a| (*a).to_owned()).collect(),
            summary: summary.to_owned(),
            usage: usage_of(name),
            domain: meta.domain,
            permission: meta.permission,
            busy: busy_str(meta.busy),
            origin: CommandOriginKind::Operation,
            remote: policy.is_some(),
            available: availability.is_ok(),
            reason,
            reason_code,
            subcommands,
        });
    }
    for local in LOCAL_COMMANDS {
        if adapters.iter().any(|a| adapter_matches(a, local.name)) {
            continue;
        }
        let refusal = RemoteRefusal::NotRemote;
        views.push(RemoteCommandView {
            name: local.name.to_owned(),
            aliases: Vec::new(),
            summary: local.english.to_owned(),
            usage: if local.usage.is_empty() {
                usage_of(local.name)
            } else {
                local.usage.to_owned()
            },
            domain: local.domain,
            permission: local.permission,
            busy: busy_str(local.busy),
            origin: CommandOriginKind::TuiLocal,
            remote: false,
            available: false,
            reason: Some(refusal.reason_de()),
            reason_code: Some(refusal.code()),
            subcommands: help_for(local.name).map_or_else(Vec::new, |h| hint_views(h.hints)),
        });
    }
    views.sort_by(|a, b| a.name.cmp(&b.name));
    views
}

const FNV64_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV64_PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a-64 ueber die nach `name` sortierten Zeilen `"{name}\0{available as u8}\0{reason_code or ""}\n"`.
/// Aendert sich, sobald sich Liste oder Verfuegbarkeit aendert; Grundlage fuer `commands.list.rev`.
#[must_use]
pub fn command_list_rev(views: &[RemoteCommandView]) -> u64 {
    let mut sorted: Vec<&RemoteCommandView> = views.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    let mut hash = FNV64_OFFSET;
    let mut feed = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(FNV64_PRIME);
        }
    };
    for view in sorted {
        feed(view.name.as_bytes());
        feed(&[0, u8::from(view.available), 0]);
        feed(view.reason_code.unwrap_or("").as_bytes());
        feed(b"\n");
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};
    use harw_operations::operation::{RemoteCap, RemoteSlice};
    use harw_operations::registry::OperationRegistry;
    use std::collections::HashSet;
    use std::sync::Arc;

    const OBSERVER: RemoteGrant = RemoteGrant {
        observe: true,
        steer: false,
        control: false,
        tier: PermissionTier::Observer,
    };
    const OPERATOR: RemoteGrant = RemoteGrant {
        observe: true,
        steer: true,
        control: false,
        tier: PermissionTier::Operator,
    };
    const FULL: RemoteGrant = RemoteGrant {
        observe: true,
        steer: true,
        control: true,
        tier: PermissionTier::Owner,
    };

    fn adapters() -> Vec<CommandAdapter> {
        let mut registry = OperationRegistry::new();
        crate::register_all(&mut registry);
        registry
            .iter()
            .flat_map(|op| CommandAdapter::from_operation(Arc::clone(op)))
            .collect()
    }

    fn view<'a>(views: &'a [RemoteCommandView], name: &str) -> TestResult<&'a RemoteCommandView> {
        views
            .iter()
            .find(|v| v.name == name)
            .ok_or(TestError::Missing("Befehl fehlt in der Liste"))
    }

    #[test]
    fn hint_table_is_unique_and_valid() {
        let mut seen = HashSet::new();
        for help in HINT_TABLE {
            assert!(!help.name.is_empty() && !help.name.starts_with('/'));
            assert!(seen.insert(help.name), "doppelt: {}", help.name);
            assert!(!help.usage.is_empty());
            for hint in help.hints {
                assert!(
                    hint.key.starts_with("commands.") || hint.key.starts_with("settings."),
                    "Schluessel {} in {}",
                    hint.key,
                    help.name
                );
                assert!(!hint.english.is_empty());
            }
        }
        assert_eq!(HINT_TABLE.len(), 24);
        assert_eq!(SUMMARY_OVERRIDES.len(), 4);
        assert_eq!(PLANNED_COMMANDS.len(), 16);
        assert_eq!(
            help_for("matrix").and_then(|h| h.usage_key),
            Some("commands.usage.matrix")
        );
    }

    #[test]
    fn local_commands_are_unique_and_match_fallbacks() {
        assert_eq!(LOCAL_COMMANDS.len(), 14);
        let mut seen = HashSet::new();
        for local in LOCAL_COMMANDS {
            assert!(seen.insert(local.name), "doppelt: {}", local.name);
        }
        let fallbacks: Vec<&str> = LOCAL_COMMANDS
            .iter()
            .filter(|l| l.fallback)
            .map(|l| l.name)
            .collect();
        assert_eq!(fallbacks, ["mode", "matrix", "plan"]);
    }

    #[test]
    fn remote_policy_paths_unique_and_forms_well_formed() -> TestResult {
        let mut seen = HashSet::new();
        for policy in REMOTE_POLICIES {
            assert!(policy.path.starts_with('/'), "{}", policy.path);
            assert!(seen.insert(policy.path), "doppelt: {}", policy.path);
            for verb in policy.verbs {
                let (last, head) = verb
                    .form
                    .split_last()
                    .ok_or(TestError::Missing("form nicht leer"))?;
                assert!(
                    *last == "-" || *last == "*",
                    "{}: {:?}",
                    policy.path,
                    verb.form
                );
                assert!(
                    head.iter().all(|t| *t != "-" && *t != "*"),
                    "{}: {:?}",
                    policy.path,
                    verb.form
                );
            }
        }
        Ok(())
    }

    #[test]
    fn remote_reads_are_slice_read_and_writes_are_not() {
        for policy in REMOTE_POLICIES {
            for verb in policy.verbs {
                assert_eq!(
                    verb.cap == RemoteCap::Observe,
                    verb.slice == RemoteSlice::Read,
                    "{} {:?}",
                    policy.path,
                    verb.form
                );
                if verb.slice == RemoteSlice::Global {
                    assert!(verb.stepup, "{} {:?}", policy.path, verb.form);
                    assert_eq!(verb.cap, RemoteCap::Control);
                }
            }
        }
    }

    #[test]
    fn remote_non_read_tier_not_below_op_permission() {
        let all = adapters();
        for policy in REMOTE_POLICIES {
            let Some(adapter) = all.iter().find(|a| a.path() == policy.path) else {
                continue;
            };
            for verb in policy.verbs.iter().filter(|v| v.cap != RemoteCap::Observe) {
                assert!(
                    verb.tier >= adapter.permission(),
                    "{} {:?}: Stufe {:?} unter {:?}",
                    policy.path,
                    verb.form,
                    verb.tier,
                    adapter.permission()
                );
            }
        }
    }

    #[test]
    fn every_remote_policy_names_a_registered_command() {
        let all = adapters();
        for policy in REMOTE_POLICIES {
            if policy.path == "/plan" {
                continue; // nur mit Plan-Tools registriert
            }
            assert!(
                all.iter().any(|a| a.path() == policy.path),
                "kein Adapter fuer {}",
                policy.path
            );
        }
    }

    #[test]
    fn status_and_models_show_run_for_observer() -> TestResult {
        let all = adapters();
        for line in ["/status", "/models show"] {
            let resolved = resolve_remote(&all, line, &OBSERVER)
                .map_err(|e| TestError::Unexpected(format!("{line}: {}", e.reason_de())))?;
            assert_eq!(resolved.verb.cap, RemoteCap::Observe, "{line}");
        }
        Ok(())
    }

    #[test]
    fn observer_sees_commands_greyed_with_reason() -> TestResult {
        let all = adapters();
        let views = remote_command_list(&all, &OBSERVER);
        let stop = view(&views, "stop")?;
        assert!(!stop.available);
        assert_eq!(stop.reason.as_deref(), Some("braucht Recht Steuern"));
        let tools = view(&views, "tools")?;
        assert_eq!(tools.origin, CommandOriginKind::TuiLocal);
        assert!(!tools.remote);
        assert_eq!(tools.reason.as_deref(), Some("nur in der TUI"));
        let status = view(&views, "status")?;
        assert!(status.available);
        assert_eq!(status.reason, None);
        for local in LOCAL_COMMANDS {
            let collides = all.iter().any(|a| adapter_matches(a, local.name));
            assert!(
                views.iter().any(|v| v.name == local.name),
                "{} fehlt",
                local.name
            );
            if !collides {
                assert_eq!(
                    view(&views, local.name)?.origin,
                    CommandOriginKind::TuiLocal
                );
            }
        }
        let names: Vec<&str> = views.iter().map(|v| v.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
        Ok(())
    }

    #[test]
    fn operator_cannot_manage() -> TestResult {
        let all = adapters();
        let line = "/models set uia x/y";
        let err = resolve_remote(&all, line, &OPERATOR).err();
        if REMOTE_ENABLED_SLICE < RemoteSlice::Global {
            assert_eq!(
                err,
                Some(RemoteRefusal::SliceDisabled {
                    slice: RemoteSlice::Global
                })
            );
        } else {
            assert_eq!(
                err,
                Some(RemoteRefusal::MissingCap {
                    cap: RemoteCap::Control
                })
            );
        }
        let adapter = all
            .iter()
            .find(|a| a.path() == "/models")
            .ok_or(TestError::Missing("/models"))?;
        let args: Vec<String> = ["set", "uia", "x/y"].map(str::to_owned).to_vec();
        let refused = adapter
            .allows_remote_invocation(
                remote_policy("/models"),
                &OPERATOR,
                RemoteSlice::Global,
                &args,
            )
            .err();
        assert_eq!(
            refused,
            Some(RemoteRefusal::MissingCap {
                cap: RemoteCap::Control
            })
        );
        Ok(())
    }

    #[test]
    fn parse_remote_line_rules() {
        for bad in ["", "status", "/", "/a\tb", "  /\t"] {
            assert_eq!(
                parse_remote_line(bad),
                Err(RemoteRefusal::LineInvalid),
                "{bad:?}"
            );
        }
        let long = format!("/{}", "a".repeat(512));
        assert_eq!(long.len(), 513);
        assert_eq!(parse_remote_line(&long), Err(RemoteRefusal::LineInvalid));
        let many = format!("/x{}", " a".repeat(32));
        assert_eq!(parse_remote_line(&many), Err(RemoteRefusal::LineInvalid));
        let max = format!("/x{}", " a".repeat(31));
        assert!(parse_remote_line(&max).is_ok());
        assert_eq!(
            parse_remote_line("  /models   show "),
            Ok(RemoteLine {
                name: "models".to_owned(),
                args: vec!["show".to_owned()],
            })
        );
    }

    #[test]
    fn alias_resolves_to_operation() -> TestResult {
        let all = adapters();
        let resolved = resolve_remote(&all, "/reasoning show", &OBSERVER)
            .map_err(|e| TestError::Unexpected(e.reason_de()))?;
        assert_eq!(resolved.adapter.path(), "/effort");
        assert_eq!(resolved.verb.cap, RemoteCap::Observe);
        Ok(())
    }

    #[test]
    fn provider_remote_forms() -> TestResult {
        let all = adapters();
        let catalog = resolve_remote(&all, "/provider catalog", &OBSERVER)
            .map_err(|e| TestError::Unexpected(e.reason_de()))?;
        assert_eq!(catalog.verb.cap, RemoteCap::Observe);
        for line in ["/provider list", "/p show"] {
            assert!(resolve_remote(&all, line, &OBSERVER).is_ok(), "{line}");
        }
        assert_eq!(
            resolve_remote(&all, "/provider test", &OBSERVER).err(),
            Some(RemoteRefusal::VerbNotRemote)
        );
        assert_eq!(
            resolve_remote(&all, "/provider add-custom x --allow-insecure-lan", &FULL).err(),
            Some(RemoteRefusal::DeniedToken {
                token: "--allow-insecure-lan".into()
            })
        );
        let add = resolve_remote(&all, "/provider add anthropic", &FULL);
        if REMOTE_ENABLED_SLICE < RemoteSlice::Global {
            assert_eq!(
                add.err(),
                Some(RemoteRefusal::SliceDisabled {
                    slice: RemoteSlice::Global
                })
            );
        } else {
            let resolved = add.map_err(|e| TestError::Unexpected(e.reason_de()))?;
            assert!(resolved.verb.stepup);
        }
        let adapter = all
            .iter()
            .find(|a| a.path() == "/provider")
            .ok_or(TestError::Missing("/provider"))?;
        let args: Vec<String> = ["add", "anthropic"].map(str::to_owned).to_vec();
        let verb = adapter
            .allows_remote_invocation(
                remote_policy("/provider"),
                &FULL,
                RemoteSlice::Global,
                &args,
            )
            .map_err(|e| TestError::Unexpected(e.reason_de()))?;
        assert!(verb.stepup);
        let refused = adapter
            .allows_remote_invocation(
                remote_policy("/provider"),
                &OPERATOR,
                RemoteSlice::Global,
                &args,
            )
            .err();
        assert_eq!(
            refused,
            Some(RemoteRefusal::MissingCap {
                cap: RemoteCap::Control
            })
        );
        assert!(remote_policy("/provider-concurrency").is_none());
        assert!(remote_policy("/uia-provider").is_none());
        Ok(())
    }

    #[test]
    fn unknown_and_local_commands() {
        let all = adapters();
        assert_eq!(
            resolve_remote(&all, "/nope", &FULL).err(),
            Some(RemoteRefusal::UnknownCommand {
                name: "nope".to_owned()
            })
        );
        assert_eq!(
            resolve_remote(&all, "/tools", &FULL).err(),
            Some(RemoteRefusal::NotRemote)
        );
    }

    #[test]
    fn rev_changes_with_availability() {
        let all = adapters();
        let observer = remote_command_list(&all, &OBSERVER);
        let full = remote_command_list(&all, &FULL);
        assert_ne!(command_list_rev(&observer), command_list_rev(&full));
        assert_eq!(
            command_list_rev(&observer),
            command_list_rev(&remote_command_list(&all, &OBSERVER))
        );
    }

    #[test]
    fn settings_list_uses_settings_texts() {
        let all = adapters();
        let views = remote_command_list(&all, &OBSERVER);
        if let Some(settings) = views.iter().find(|v| v.name == "settings") {
            assert_eq!(settings.summary, SETTINGS_SUMMARY.1);
            assert_eq!(settings.subcommands.len(), SETTINGS_HINTS.len());
        }
    }

    #[test]
    fn busy_strings_are_stable() {
        assert_eq!(busy_str(BusyAvailability::Immediate), "immediate");
        assert_eq!(busy_str(BusyAvailability::Staged), "staged");
        assert_eq!(busy_str(BusyAvailability::DeferredUntilTurnEnd), "deferred");
    }
}
