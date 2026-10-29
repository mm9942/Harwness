//! Statischer Hilfe-Katalog für `/`-Befehle: TUI-lokale Befehle,
//! Unterkommando-Hinweise, Nutzungszeilen und geplante Befehle.
//!
//! # Verantwortung
//! - [`local_command_specs`]: Spezifikationen der rein TUI-lokal abgefangenen
//!   Befehle (`/tools`, `/exit`, …) plus Ersatz-Spezifikationen für Befehle,
//!   deren Operation (noch) nicht registriert ist (`/workbench`, `/kanban`,
//!   `/palace`, `/dream`, `/diary`, `/models`, `/mode`, `/matrix`). Beim Einmischen über
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

use crate::i18n::tr;

/// Owned metadata is translated by identity; syntax and supplied metadata are payload.
struct CatalogHint {
    name: &'static str,
    args: &'static str,
    key: &'static str,
    english: &'static str,
}

impl CatalogHint {
    const fn new(
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

    fn localized(&self) -> SubcommandHint {
        SubcommandHint::new(self.name, self.args, tr(self.key, self.english))
    }
}

/// Maximale Länge (Zeichen) einer Operations-Beschreibung, bevor [`enrich`]
/// sie auf den ersten Satz kürzt.
const MAX_SUMMARY_CHARS: usize = 110;

// ---------------------------------------------------------------------------
// Unterkommando-Tabellen
// ---------------------------------------------------------------------------

/// `/model`, Grammatik aus `harw-ops/src/model.rs` (`show|list|switch`).
const MODEL: &[CatalogHint] = &[
    CatalogHint::new(
        "show",
        "",
        "commands.subcommand.model.show",
        "Show the active model",
    ),
    CatalogHint::new(
        "list",
        "",
        "commands.subcommand.model.list",
        "List catalog models",
    ),
    CatalogHint::new(
        "switch",
        "<model-id>",
        "commands.subcommand.model.switch",
        "Switch provider and model immediately",
    ),
];

/// `/uia-model` (`show|list|switch`).
const UIA_MODEL: &[CatalogHint] = &[
    CatalogHint::new(
        "show",
        "",
        "commands.subcommand.uia_model.show",
        "Show the pinned UIA model",
    ),
    CatalogHint::new(
        "list",
        "",
        "commands.subcommand.uia_model.list",
        "List catalog models",
    ),
    CatalogHint::new(
        "switch",
        "<model-id>",
        "commands.subcommand.uia_model.switch",
        "Switch the UIA model starting next turn",
    ),
];

/// `/uia-worker-model` (`show|list|switch`).
const UIA_WORKER_MODEL: &[CatalogHint] = &[
    CatalogHint::new(
        "show",
        "",
        "commands.subcommand.uia_worker_model.show",
        "Show the pinned UIA worker model",
    ),
    CatalogHint::new(
        "list",
        "",
        "commands.subcommand.uia_worker_model.list",
        "List the UIA provider's models",
    ),
    CatalogHint::new(
        "switch",
        "<model-id>",
        "commands.subcommand.uia_worker_model.switch",
        "Pin the UIA worker model starting next session",
    ),
];

/// `/provider` (`show|list|test`); Wechsel läuft über `/model switch`.
const PROVIDER: &[CatalogHint] = &[
    CatalogHint::new(
        "show",
        "",
        "commands.subcommand.provider.show",
        "Show the active provider",
    ),
    CatalogHint::new(
        "list",
        "",
        "commands.subcommand.provider.list",
        "List configured providers",
    ),
    CatalogHint::new(
        "test",
        "",
        "commands.subcommand.provider.test",
        "Test the active provider's connection",
    ),
];

/// `/effort` — das Level ist selbst das Unterkommando.
const EFFORT: &[CatalogHint] = &[
    CatalogHint::new(
        "show",
        "",
        "commands.subcommand.effort.show",
        "Show the current reasoning effort",
    ),
    CatalogHint::new(
        "minimal",
        "",
        "commands.subcommand.effort.minimal",
        "Minimal reasoning effort",
    ),
    CatalogHint::new(
        "low",
        "",
        "commands.subcommand.effort.low",
        "Low reasoning effort",
    ),
    CatalogHint::new(
        "medium",
        "",
        "commands.subcommand.effort.medium",
        "Medium reasoning effort",
    ),
    CatalogHint::new(
        "high",
        "",
        "commands.subcommand.effort.high",
        "High reasoning effort",
    ),
    CatalogHint::new(
        "xhigh",
        "",
        "commands.subcommand.effort.xhigh",
        "Very high reasoning effort",
    ),
    CatalogHint::new(
        "max",
        "",
        "commands.subcommand.effort.max",
        "Maximum reasoning effort",
    ),
    CatalogHint::new(
        "clear",
        "",
        "commands.subcommand.effort.clear",
        "Restore the provider default",
    ),
];

/// `/uia-effort` — wie `/effort`, wirkt ab der nächsten Sitzung.
const UIA_EFFORT: &[CatalogHint] = &[
    CatalogHint::new(
        "show",
        "",
        "commands.subcommand.uia_effort.show",
        "Show the pinned UIA reasoning effort",
    ),
    CatalogHint::new(
        "minimal",
        "",
        "commands.subcommand.uia_effort.minimal",
        "Minimal effort starting next session",
    ),
    CatalogHint::new(
        "low",
        "",
        "commands.subcommand.uia_effort.low",
        "Low effort starting next session",
    ),
    CatalogHint::new(
        "medium",
        "",
        "commands.subcommand.uia_effort.medium",
        "Medium effort starting next session",
    ),
    CatalogHint::new(
        "high",
        "",
        "commands.subcommand.uia_effort.high",
        "High effort starting next session",
    ),
    CatalogHint::new(
        "xhigh",
        "",
        "commands.subcommand.uia_effort.xhigh",
        "Very high effort starting next session",
    ),
    CatalogHint::new(
        "max",
        "",
        "commands.subcommand.uia_effort.max",
        "Maximum effort starting next session",
    ),
    CatalogHint::new(
        "clear",
        "",
        "commands.subcommand.uia_effort.clear",
        "Remove the pin and use the role default",
    ),
];

/// `/mode` — `show`, ein Modusname oder `default <modus>`.
const MODE: &[CatalogHint] = &[
    CatalogHint::new(
        "show",
        "",
        "commands.subcommand.mode.show",
        "Show the current interaction mode",
    ),
    CatalogHint::new(
        "chat",
        "",
        "commands.subcommand.mode.chat",
        "Chat mode: converse without requiring tools",
    ),
    CatalogHint::new(
        "plan",
        "",
        "commands.subcommand.mode.plan",
        "Plan mode: plan without making changes",
    ),
    CatalogHint::new(
        "explore",
        "",
        "commands.subcommand.mode.explore",
        "Explore mode: investigate with read-only access",
    ),
    CatalogHint::new(
        "work",
        "",
        "commands.subcommand.mode.work",
        "Work mode: implement with tools",
    ),
    CatalogHint::new(
        "shell",
        "",
        "commands.subcommand.mode.shell",
        "Shell mode: execute commands",
    ),
    CatalogHint::new(
        "default",
        "<mode>",
        "commands.subcommand.mode.default",
        "Set the default mode in configuration",
    ),
];

/// `/plan` (Runde 5, Teil F): die lokalen Plan-Modus-Befehle vorn, dazu der
/// Einstieg in die `plan`-Operation (`inspect`). Bare `/plan` schaltet den
/// Plan-Modus ein.
const PLAN: &[CatalogHint] = &[
    CatalogHint::new(
        "show",
        "",
        "commands.subcommand.plan.show",
        "Show the current or pinned plan",
    ),
    CatalogHint::new(
        "edit",
        "",
        "commands.subcommand.plan.edit",
        "Open the current plan in $EDITOR",
    ),
    CatalogHint::new(
        "list",
        "",
        "commands.subcommand.plan.list",
        "List project plans (.harw/plans)",
    ),
    CatalogHint::new(
        "open",
        "<name>",
        "commands.subcommand.plan.open",
        "Load an earlier plan to continue planning",
    ),
    CatalogHint::new(
        "inspect",
        "[id]",
        "commands.subcommand.plan.inspect",
        "Show the plan operation's graph",
    ),
    // Runde 5, Teil P: Plan-Store (mehrere Pläne, Freigabe, Schritte). `list`
    // zeigt hier die Plan-Dateien; die Graphen listet `plans`.
    CatalogHint::new(
        "plans",
        "",
        "commands.subcommand.plan.plans",
        "List all graphs in the plan store",
    ),
    CatalogHint::new(
        "switch",
        "<id>",
        "commands.subcommand.plan.switch",
        "Activate another plan graph",
    ),
    CatalogHint::new(
        "archive",
        "<id>",
        "commands.subcommand.plan.archive",
        "Hide a plan graph without deleting it",
    ),
    CatalogHint::new(
        "submit",
        "[id]",
        "commands.subcommand.plan.submit",
        "Confirm the proposed plan",
    ),
    CatalogHint::new(
        "step",
        "<id> <open|running|done|blocked> [evidence]",
        "commands.subcommand.plan.step",
        "Report step progress (done requires evidence)",
    ),
];

/// `/models` — Modelle je Rolle.
const MODELS: &[CatalogHint] = &[
    CatalogHint::new(
        "show",
        "",
        "commands.subcommand.models.show",
        "Show models for all roles",
    ),
    CatalogHint::new(
        "set",
        "<role> <target>",
        "commands.subcommand.models.set",
        "Set a role's model (child roles apply to new agents immediately)",
    ),
    CatalogHint::new(
        "reset",
        "<role>",
        "commands.subcommand.models.reset",
        "Reset a role to its default",
    ),
    CatalogHint::new(
        "pick",
        "<role>",
        "commands.subcommand.models.pick",
        "Open the model picker for a role",
    ),
    // Runde 5, Teil G: eigene Modellwahl je UIA-Worker-Rolle.
    CatalogHint::new(
        "worker",
        "[<role|all> <uia|target>]",
        "commands.subcommand.models.worker",
        "Show or set UIA worker models (uia means same as UIA)",
    ),
];

/// `/permissions`, Grammatik aus `harw-ops/src/permissions.rs`.
const PERMISSIONS: &[CatalogHint] = &[
    CatalogHint::new(
        "show",
        "",
        "commands.subcommand.permissions.show",
        "Show approval mode, rules and directories",
    ),
    CatalogHint::new(
        "mode",
        "<ask|auto|full> [--session|--project|--global]",
        "commands.subcommand.permissions.mode",
        "Set the approval mode",
    ),
    // Runde 5, Teil E: `--user` (= `--global`), `rules`, `rm`, `log`.
    CatalogHint::new(
        "allow",
        "<tool> [pattern] [--session|--project|--user]",
        "commands.subcommand.permissions.allow",
        "Add an allow rule",
    ),
    CatalogHint::new(
        "deny",
        "<tool> [pattern] [--session|--project|--user]",
        "commands.subcommand.permissions.deny",
        "Add a deny rule (overrides allow)",
    ),
    CatalogHint::new(
        "rules",
        "",
        "commands.subcommand.permissions.rules",
        "List rules with their origins",
    ),
    CatalogHint::new(
        "remove",
        "<number>",
        "commands.subcommand.permissions.remove",
        "Remove a rule by number",
    ),
    CatalogHint::new(
        "rm",
        "<number>",
        "commands.subcommand.permissions.rm",
        "Remove a rule by number (short form)",
    ),
    CatalogHint::new(
        "log",
        "[count]",
        "commands.subcommand.permissions.log",
        "Show recent auto-mode decisions",
    ),
];

/// `/memory`, Grammatik aus `harw-ops/src/memory.rs`.
const MEMORY: &[CatalogHint] = &[
    CatalogHint::new(
        "list",
        "",
        "commands.subcommand.memory.list",
        "Show hot memories",
    ),
    CatalogHint::new(
        "stats",
        "",
        "commands.subcommand.memory.stats",
        "Show memory statistics",
    ),
    CatalogHint::new(
        "recall",
        "<keyword…>",
        "commands.subcommand.memory.recall",
        "Search facts",
    ),
    CatalogHint::new(
        "record",
        "<text…> [--project|--global]",
        "commands.subcommand.memory.record",
        "Record a fact",
    ),
    CatalogHint::new(
        "forget",
        "<name>",
        "commands.subcommand.memory.forget",
        "Delete a fact",
    ),
    CatalogHint::new(
        "maintain",
        "",
        "commands.subcommand.memory.maintain",
        "Clean up memory",
    ),
    CatalogHint::new(
        "consolidate",
        "[--project|--global]",
        "commands.subcommand.memory.consolidate",
        "Consolidate facts",
    ),
    CatalogHint::new(
        "promote",
        "<fact-id> [--project|--global] [--slug <slug>]",
        "commands.subcommand.memory.promote",
        "Promote a fact to a provisional topic",
    ),
    CatalogHint::new(
        "topics",
        "",
        "commands.subcommand.memory.topics",
        "List provisional topics",
    ),
];

/// `/agent`, Grammatik aus `harw-ops/src/agent.rs`.
const AGENT: &[CatalogHint] = &[
    CatalogHint::new(
        "list",
        "",
        "commands.subcommand.agent.list",
        "List active child agents",
    ),
    // Plan R9, Teil C: startbare Agenten (eingebaut und benutzerdefiniert).
    CatalogHint::new(
        "defs",
        "[query]",
        "commands.subcommand.agent.defs",
        "List launchable agents with their origins",
    ),
    CatalogHint::new(
        "stop",
        "<agent-id>",
        "commands.subcommand.agent.stop",
        "Cancel child agents",
    ),
    CatalogHint::new(
        "budget",
        "[agent-id]",
        "commands.subcommand.agent.budget",
        "Show budget and lease",
    ),
    // Runde 5, Teil I: TUI-lokal abgefangen (vor der Operation `/agent`).
    CatalogHint::new(
        "stream",
        "<orchestrators|all|none>",
        "commands.subcommand.agent.stream",
        "Stream child agents live in history (session)",
    ),
    // Runde 5, Teil K: TUI-lokal abgefangen (Hintergrund-Agenten).
    CatalogHint::new(
        "bg",
        "",
        "commands.subcommand.agent.bg",
        "List background agents with progress",
    ),
    CatalogHint::new(
        "cancel",
        "<agent-id>",
        "commands.subcommand.agent.cancel",
        "Cancel background agents",
    ),
];

/// `/export` — Optionen statt Unterkommandos (`harw-ops/src/export.rs`).
const EXPORT: &[CatalogHint] = &[
    CatalogHint::new(
        "--format",
        "<md|json>",
        "commands.subcommand.export.format",
        "Choose the output format",
    ),
    CatalogHint::new(
        "--datei",
        "<path>",
        "commands.subcommand.export.datei",
        "Write to a file",
    ),
    CatalogHint::new(
        "--tools",
        "",
        "commands.subcommand.export.tools",
        "Include tool calls",
    ),
    CatalogHint::new(
        "--no-tools",
        "",
        "commands.subcommand.export.no_tools",
        "Omit tool calls",
    ),
    CatalogHint::new(
        "--reasoning-summary",
        "",
        "commands.subcommand.export.reasoning_summary",
        "Include the reasoning summary",
    ),
    CatalogHint::new(
        "--max-chars",
        "<n>",
        "commands.subcommand.export.max_chars",
        "Limit the length",
    ),
];

/// `/sandbox-lease` — auf der Befehlsfläche nur `status`/`revoke`
/// (`request` braucht einen Grund und läuft über das Modell-Werkzeug).
const SANDBOX_LEASE: &[CatalogHint] = &[
    CatalogHint::new(
        "status",
        "",
        "commands.subcommand.sandbox_lease.status",
        "Show the active host permit",
    ),
    CatalogHint::new(
        "revoke",
        "",
        "commands.subcommand.sandbox_lease.revoke",
        "Revoke the host permit immediately",
    ),
];

/// `/workbench`, Grammatik aus `harw-ops/src/workbench.rs`; jeder
/// Subcommand akzeptiert `--scope=session|project|project:<slug>`.
const WORKBENCH: &[CatalogHint] = &[
    CatalogHint::new(
        "show",
        "[--scope=session|project]",
        "commands.subcommand.workbench.show",
        "Show the workbench",
    ),
    CatalogHint::new(
        "pin",
        "<path> [note]",
        "commands.subcommand.workbench.pin",
        "Pin a file",
    ),
    CatalogHint::new(
        "unpin",
        "<path>",
        "commands.subcommand.workbench.unpin",
        "Unpin a file",
    ),
    CatalogHint::new(
        "note",
        "<text>",
        "commands.subcommand.workbench.note",
        "Append a note",
    ),
    CatalogHint::new(
        "note",
        "edit <n> <text> | rm <n>",
        "commands.subcommand.workbench.note_edit",
        "Edit or delete a note",
    ),
    CatalogHint::new(
        "hypothesis",
        "add|confirm|reject <text>",
        "commands.subcommand.workbench.hypothesis",
        "Create or decide a hypothesis",
    ),
    CatalogHint::new(
        "retention",
        "[keep|<days>d] [--scope=project]",
        "commands.subcommand.workbench.retention",
        "Show or set retention",
    ),
];

/// `/kanban`, Grammatik aus `harw-ops/src/kanban.rs` (`--board=<id>` davor
/// möglich).
const KANBAN: &[CatalogHint] = &[
    CatalogHint::new(
        "show",
        "[card]",
        "commands.subcommand.kanban.show",
        "Show the board or a card",
    ),
    CatalogHint::new(
        "list",
        "",
        "commands.subcommand.kanban.list",
        "List board cards",
    ),
    CatalogHint::new(
        "boards",
        "",
        "commands.subcommand.kanban.boards",
        "List boards",
    ),
    CatalogHint::new(
        "add",
        "<title>",
        "commands.subcommand.kanban.add",
        "Create a card",
    ),
    CatalogHint::new(
        "move",
        "<card> <todo|ready|running|done|blocked|archived>",
        "commands.subcommand.kanban.move",
        "Move a card",
    ),
    CatalogHint::new(
        "todo",
        "<card>",
        "commands.subcommand.kanban.todo",
        "Set a card to todo",
    ),
    CatalogHint::new(
        "ready",
        "<card>",
        "commands.subcommand.kanban.ready",
        "Mark a card ready",
    ),
    CatalogHint::new(
        "claim",
        "<card>",
        "commands.subcommand.kanban.claim",
        "Claim a card",
    ),
    CatalogHint::new(
        "block",
        "<card> <reason>",
        "commands.subcommand.kanban.block",
        "Block a card",
    ),
    CatalogHint::new(
        "unblock",
        "<card>",
        "commands.subcommand.kanban.unblock",
        "Unblock a card",
    ),
    CatalogHint::new(
        "done",
        "<card>",
        "commands.subcommand.kanban.done",
        "Complete a card",
    ),
    CatalogHint::new(
        "archive",
        "<card>",
        "commands.subcommand.kanban.archive",
        "Archive a card",
    ),
    CatalogHint::new(
        "edit",
        "<card> <text>",
        "commands.subcommand.kanban.edit",
        "Replace card text",
    ),
    CatalogHint::new(
        "comment",
        "<card> <text>",
        "commands.subcommand.kanban.comment",
        "Append a comment",
    ),
    CatalogHint::new(
        "evidence",
        "<card> <path|url>",
        "commands.subcommand.kanban.evidence",
        "Link evidence",
    ),
    CatalogHint::new(
        "approve",
        "<card> [note]",
        "commands.subcommand.kanban.approve",
        "Approve a worker card",
    ),
    CatalogHint::new(
        "reject",
        "<card> [reason]",
        "commands.subcommand.kanban.reject",
        "Reject a worker card",
    ),
];

/// `/palace`, Grammatik aus `harw-ops/src/palace.rs` plus `list` (Vertrag).
const PALACE: &[CatalogHint] = &[
    CatalogHint::new("list", "", "commands.subcommand.palace.list", "List nodes"),
    CatalogHint::new(
        "show",
        "<id>",
        "commands.subcommand.palace.show",
        "Show a node",
    ),
    CatalogHint::new(
        "search",
        "<query> [--max-hops=n] [--max=n]",
        "commands.subcommand.palace.search",
        "Search the graph",
    ),
    CatalogHint::new(
        "promote",
        "<topic>",
        "commands.subcommand.palace.promote",
        "Promote a topic to the palace (established)",
    ),
    CatalogHint::new(
        "supersede",
        "<old> <new> [--confirm]",
        "commands.subcommand.palace.supersede",
        "Replace a node with its successor",
    ),
    CatalogHint::new(
        "edit",
        "<id> <text…> [--confirm]",
        "commands.subcommand.palace.edit",
        "Replace node text (new revision)",
    ),
    CatalogHint::new(
        "link",
        "<a> <b> [--confirm]",
        "commands.subcommand.palace.link",
        "Add a link from a to b",
    ),
];

/// `/dream`, Grammatik aus `harw-ops/src/dream.rs` (D5).
const DREAM: &[CatalogHint] = &[
    CatalogHint::new(
        "list",
        "",
        "commands.subcommand.dream.list",
        "List dream reports",
    ),
    CatalogHint::new(
        "show",
        "<id>",
        "commands.subcommand.dream.show",
        "Show a dream report",
    ),
    CatalogHint::new(
        "run",
        "",
        "commands.subcommand.dream.run",
        "Start a dream run",
    ),
    CatalogHint::new(
        "status",
        "",
        "commands.subcommand.dream.status",
        "Show dream run status",
    ),
    CatalogHint::new(
        "review",
        "[<id>] | <id> accept|reject <p-id> [reason]",
        "commands.subcommand.dream.review",
        "Review, accept or reject proposals",
    ),
];

/// `/learn`, Grammatik aus `harw-ops/src/learn.rs`: schlägt nur vor, jede
/// Übernahme braucht ein ausdrückliches `accept`.
const LEARN: &[CatalogHint] = &[
    CatalogHint::new(
        "scan",
        "",
        "commands.subcommand.learn.scan",
        "Scan the session for learning candidates",
    ),
    CatalogHint::new(
        "note",
        "<text> [--target memory|skill|agent]",
        "commands.subcommand.learn.note",
        "Create a proposal from your own text",
    ),
    CatalogHint::new(
        "list",
        "[--all]",
        "commands.subcommand.learn.list",
        "List proposals",
    ),
    CatalogHint::new(
        "show",
        "<id>",
        "commands.subcommand.learn.show",
        "Show a proposal",
    ),
    CatalogHint::new(
        "accept",
        "<id>",
        "commands.subcommand.learn.accept",
        "Accept a proposal",
    ),
    CatalogHint::new(
        "reject",
        "<id> [reason]",
        "commands.subcommand.learn.reject",
        "Reject a proposal",
    ),
];

/// `/diary`, Grammatik aus `harw-ops/src/diary.rs` plus `today` (Vertrag).
const DIARY: &[CatalogHint] = &[
    CatalogHint::new(
        "today",
        "",
        "commands.subcommand.diary.today",
        "Show today's entries",
    ),
    CatalogHint::new(
        "show",
        "[agent] [--date=YYYY-MM-DD | --from=… [--to=…]]",
        "commands.subcommand.diary.show",
        "Show entries for a day or date range",
    ),
    CatalogHint::new(
        "search",
        "<text> [--agent=<id>] [--from=…] [--to=…]",
        "commands.subcommand.diary.search",
        "Search diaries",
    ),
    CatalogHint::new(
        "agents",
        "",
        "commands.subcommand.diary.agents",
        "List agents with diaries",
    ),
    CatalogHint::new(
        "note",
        "<text>",
        "commands.subcommand.diary.note",
        "Write a diary note",
    ),
];

/// `/matrix`, Grammatik der Matrix-Game-Operation (`harw-ops/src/matrix`).
/// Runde 7, Teil M: nur noch Ansicht — Start und Steuerung übernimmt der
/// Game Master (`matrix-game-master`), den die UIA im Hintergrund startet.
const MATRIX: &[CatalogHint] = &[
    CatalogHint::new(
        "show",
        "",
        "commands.subcommand.matrix.show",
        "Show the running game (panel: F9)",
    ),
    CatalogHint::new(
        "list",
        "",
        "commands.subcommand.matrix.list",
        "List scenarios, drafts and runs",
    ),
    CatalogHint::new(
        "replay",
        "",
        "commands.subcommand.matrix.replay",
        "Verify the journal deterministically",
    ),
    CatalogHint::new(
        "compare",
        "<run> <run> …",
        "commands.subcommand.matrix.compare",
        "Compare runs for design lessons",
    ),
];

/// `/jobs`, Grammatik aus `harw-ops/src/jobs.rs` (Plan R9, Teil F).
const JOBS: &[CatalogHint] = &[
    CatalogHint::new(
        "list",
        "",
        "commands.subcommand.jobs.list",
        "List this session's background jobs",
    ),
    CatalogHint::new(
        "show",
        "<id>",
        "commands.subcommand.jobs.show",
        "Show status, command and recent lines",
    ),
    CatalogHint::new(
        "stop",
        "<id> [TERM|INT|HUP|KILL]",
        "commands.subcommand.jobs.stop",
        "Stop a job and its process group",
    ),
    CatalogHint::new(
        "logs",
        "<id> [n]",
        "commands.subcommand.jobs.logs",
        "Show recent log lines (stdout/stderr)",
    ),
];

/// `/tools`, Grammatik aus `crate::tools_command`.
const TOOLS: &[CatalogHint] = &[
    CatalogHint::new(
        "on",
        "<name>",
        "commands.subcommand.tools.on",
        "Enable a tool",
    ),
    CatalogHint::new(
        "off",
        "<name>",
        "commands.subcommand.tools.off",
        "Disable a tool",
    ),
    CatalogHint::new(
        "reset",
        "[name]",
        "commands.subcommand.tools.reset",
        "Reset overrides",
    ),
    CatalogHint::new(
        "profile",
        "<minimal|coding|full>",
        "commands.subcommand.tools.profile",
        "Choose a tool profile",
    ),
];

/// Befehl → (Nutzungszeile, Unterkommandos). Einzige Quelle für
/// [`subcommand_hints`] und die Nutzungszeilen in [`enrich`].
const HINT_TABLE: &[(&str, &str, &[CatalogHint])] = &[
    (
        "settings",
        "/settings [show|set <key> <value>|reset <key>]",
        &[],
    ),
    ("model", "/model [show|list|switch <model-id>]", MODEL),
    (
        "uia-model",
        "/uia-model [show|list|switch <model-id>]",
        UIA_MODEL,
    ),
    (
        "uia-worker-model",
        "/uia-worker-model [show|list|switch <model-id>]",
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
    ("mode", "/mode [show|<mode>|default <mode>]", MODE),
    // Runde 5, Teil F.
    (
        "plan",
        "/plan [show|edit|list|open <name>|inspect|plans|switch|submit|step …]",
        PLAN,
    ),
    (
        "models",
        "/models [show|set <role> <target>|reset <role>|pick <role>|worker [<role|all> <uia|target>]]",
        MODELS,
    ),
    (
        "permissions",
        "/permissions [show|mode <mode>|allow <tool>|deny <tool>|rules|rm <number>|log]",
        PERMISSIONS,
    ),
    (
        "memory",
        "/memory [list|stats|recall|record|forget|promote|topics|maintain|consolidate] …",
        MEMORY,
    ),
    (
        "agent",
        "/agent [list|defs [query]|stop <agent-id>|budget [agent-id]|stream <orchestrators|all|none>|bg|cancel <agent-id>]",
        AGENT,
    ),
    (
        "export",
        "/export [--format <md|json>] [--datei <path>] [--no-tools] …",
        EXPORT,
    ),
    (
        "sandbox-lease",
        "/sandbox-lease [status|revoke]",
        SANDBOX_LEASE,
    ),
    (
        "workbench",
        "/workbench [show|pin|unpin|note [edit|rm]|hypothesis|retention] [--scope=…] …",
        WORKBENCH,
    ),
    (
        "kanban",
        "/kanban [--board=<id>] [show|list|boards|add|move|block|done|…] …",
        KANBAN,
    ),
    (
        "palace",
        "/palace [list|show <id>|search <query>|promote <topic>|supersede|edit|link] …",
        PALACE,
    ),
    (
        "dream",
        "/dream [list|show <id>|run|status|review [<id>] [accept|reject <p-id>]]",
        DREAM,
    ),
    (
        "learn",
        "/learn [scan|note <text>|list [--all]|show <id>|accept <id>|reject <id>]",
        LEARN,
    ),
    (
        "diary",
        "/diary [today|show [agent] [--date|--from/--to]|search <text>|agents|note <text>]",
        DIARY,
    ),
    (
        "matrix",
        "/matrix [show|list|replay|compare <run> <run>] — Start via the UIA (Game Master)",
        MATRIX,
    ),
    (
        "jobs",
        "/jobs [list|show <id>|stop <id> [signal]|logs <id> [n]]",
        JOBS,
    ),
    (
        "tools",
        "/tools [on <name>|off <name>|reset [name]|profile <p>]",
        TOOLS,
    ),
];

/// Kurzbeschreibungen, die zu lange oder veraltete Operations-Texte ersetzen.
const SUMMARY_OVERRIDES: &[(&str, &str, &str)] = &[
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
const PLANNED_COMMANDS: &[(&str, &str, &str)] = &[
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

// ---------------------------------------------------------------------------
// Öffentliche Funktionen
// ---------------------------------------------------------------------------

/// Liefert die Unterkommando-Hinweise eines Befehls (ohne führendes `/`).
///
/// # Rückgabe
/// Die lokalisierte Hinweisliste oder eine leere Liste für unbekannte Befehle.
pub(crate) fn subcommand_hints(name: &str) -> Vec<SubcommandHint> {
    for (command, _, hints) in HINT_TABLE {
        if *command == name {
            return hints.iter().map(CatalogHint::localized).collect();
        }
    }
    Vec::new()
}

/// Localized planned summaries, keyed by owned command identity.
pub(crate) fn planned_commands() -> Vec<(&'static str, &'static str)> {
    PLANNED_COMMANDS
        .iter()
        .map(|(name, key, english)| (*name, tr(key, english)))
        .collect()
}

/// Nutzungszeile aus der Katalogtabelle, falls vorhanden.
fn usage_for(name: &str) -> Option<&'static str> {
    HINT_TABLE
        .iter()
        .find(|(command, _, _)| *command == name)
        .map(|(_, usage, _)| {
            if name == "matrix" {
                tr("commands.usage.matrix", "/matrix [show|list|replay|compare <run> <run>] — Start via the UIA (Game Master)")
            } else {
                *usage
            }
        })
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
/// - `summary`: eine Katalog-Überschreibung gewinnt; bekannte Operations-Texte
///   werden bei Bedarf gekürzt. Unbekannte und lokale Texte bleiben erhalten.
/// - `subcommands`: nur gesetzt, wenn noch leer.
/// - `usage`: nur gesetzt, wenn noch leer — aus der Katalogtabelle, sonst
///   `"/<name>"`.
///
/// Idempotent: ein zweiter Aufruf ändert nichts mehr.
pub(crate) fn enrich(spec: &mut CommandSpec) {
    let name = spec.name.as_str().to_owned();
    if let Some((_, key, english)) = SUMMARY_OVERRIDES.iter().find(|(n, _, _)| *n == name) {
        spec.summary = tr(key, english).to_owned();
    } else if spec.origin == crate::command::CommandOrigin::Operation
        && usage_for(&name).is_some()
        && !spec.summary.is_empty()
    {
        spec.summary = shorten_summary(&spec.summary);
    }
    if name == "settings" {
        spec.summary = crate::i18n::tr("settings.title", "Settings").to_owned();
        spec.subcommands = vec![
            SubcommandHint::new("show", "", crate::i18n::tr("settings.title", "Settings")),
            SubcommandHint::new(
                "set",
                "<key> <value>",
                crate::i18n::tr("settings.scope", "Active profile · applies immediately"),
            ),
            SubcommandHint::new(
                "reset",
                "<key>",
                crate::i18n::tr("settings.scope", "Active profile · applies immediately"),
            ),
        ];
    }
    if spec.subcommands.is_empty() {
        spec.subcommands = subcommand_hints(&name);
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
    .with_help(summary, usage, &subcommand_hints(name))
    .local();
    spec.busy = busy;
    enrich(&mut spec);
    Some(spec)
}

/// Namen der Ersatz-Spezifikationen für (noch) fehlende Operationen.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const FALLBACK_COMMANDS: &[&str] = &["mode", "matrix", "plan"];

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
    use BusyAvailability::{DeferredUntilTurnEnd as Deferred, Immediate, Staged};
    use CommandDomain::{Execution, Misc, SessionLifecycle};
    use PermissionTier::{Observer, Operator};

    [
        // ── Echte lokale Befehle ──────────────────────────────────────────
        local_spec(
            "tools",
            Execution,
            Operator,
            Deferred,
            tr("commands.local.tools", "Show tools, enable or disable them, choose a profile"),
            "",
        ),
        local_spec(
            "resume",
            SessionLifecycle,
            Operator,
            Deferred,
            tr("commands.local.resume", "Resume an earlier session (without an ID: choose one)"),
            "/resume [session-id]",
        ),
        local_spec(
            "sessions",
            SessionLifecycle,
            Observer,
            Deferred,
            tr("commands.local.sessions", "Open the session picker"),
            "/sessions",
        ),
        local_spec(
            "exit",
            SessionLifecycle,
            Observer,
            Deferred,
            tr("commands.local.exit", "Exit the TUI (same as /quit)"),
            "/exit",
        ),
        local_spec(
            "clear",
            SessionLifecycle,
            Observer,
            Deferred,
            tr("commands.local.clear", "Clear the display (keep the session)"),
            "/clear",
        ),
        local_spec(
            "verbose",
            Misc,
            Observer,
            Immediate,
            tr("commands.local.verbose", "Toggle detailed tool display"),
            "/verbose",
        ),
        local_spec(
            "keys",
            Misc,
            Observer,
            Immediate,
            tr("commands.local.keys", "Show key bindings"),
            "/keys",
        ),
        local_spec(
            "whoami",
            Misc,
            Observer,
            Immediate,
            tr("commands.local.whoami", "Show session, permission and active model"),
            "/whoami",
        ),
        local_spec(
            "rename",
            SessionLifecycle,
            Operator,
            Immediate,
            tr("commands.local.rename", "Rename the session"),
            "/rename <title>",
        ),
        // Runde 5, Teil I: `/agents` entfällt (nur noch `/agent`); die
        // Eingabe `/agents` zeigt einen Hinweis (`local_commands`).
        // Runde 5, Teil L: flüchtige Nebenfrage, jederzeit (auch im Turn).
        local_spec(
            "btw",
            Misc,
            Operator,
            Immediate,
            tr("commands.local.btw", "Ask a side question without interrupting the agent (not in history)"),
            "/btw <question>",
        ),
        // Bilder: der Agent sieht sie mit der nächsten Nachricht.
        local_spec(
            "image",
            Misc,
            Operator,
            Immediate,
            tr("commands.local.image", "Attach an image to the next message (PNG, JPEG, GIF, WebP)"),
            "/image <path>",
        ),
        // ── Ersatz, solange die Operation fehlt ───────────────────────────
        local_spec(
            "mode",
            SessionLifecycle,
            Operator,
            Staged,
            tr("commands.summary.mode", "Show or change the interaction mode"),
            "",
        ),
        local_spec(
            "matrix",
            Misc,
            Operator,
            Deferred,
            tr("commands.local.matrix", "View the Matrix game (UIA/Game Master starts and controls it; bare command opens panel)"),
            "",
        ),
        // Runde 5, Teil F: `/plan` (Plan-Modus an) und `/plan
        // show|edit|list|open` sind immer lokal; die `plan`-Operation (nur
        // mit Plan-Diensten) gewinnt als Spezifikation, sobald sie
        // registriert ist.
        local_spec(
            "plan",
            SessionLifecycle,
            Operator,
            Immediate,
            tr("commands.local.plan", "Enable plan mode; show, edit or open plans"),
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
        // Runde 5, Teil L:
        "btw", "image",
    ];

    /// Befehle, deren Operation fehlen darf. `matrix` behält nur seine
    /// Ersatz-Spezifikation. Runde 5, Teil F: die `plan`-Operation gibt es
    /// nur mit Plan-Diensten (`[tools.plan]`, TUI-Vorgabe), nie im
    /// eingebauten Katalog; `/plan` selbst ist lokal.
    const ALLOWED_MISSING: &[&str] = &["plan"];

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
    fn hint_table_names_and_enriched_hints_are_valid_unique_and_non_empty() -> TestResult {
        for (index, (name, usage, hints)) in HINT_TABLE.iter().enumerate() {
            CommandName::parse(*name).map_err(ctx("hinted name must be valid"))?;
            let mut spec = CommandSpec::new(
                *name,
                Vec::<String>::new(),
                CommandScope::TuiOnly,
                PermissionTier::Operator,
                OutputSurface::Inline,
                CommandDomain::Misc,
            )
            .map_err(ctx("hinted spec"))?;
            enrich(&mut spec);
            if *name == "settings" {
                assert!(
                    hints.is_empty(),
                    "settings hints must reuse dynamic settings metadata"
                );
                assert_eq!(
                    spec.subcommands
                        .iter()
                        .map(|hint| (hint.name, hint.args))
                        .collect::<Vec<_>>(),
                    [("show", ""), ("set", "<key> <value>"), ("reset", "<key>")]
                );
                assert_eq!(
                    spec.subcommands[0].summary,
                    tr("settings.title", "Settings")
                );
                assert_eq!(
                    spec.subcommands[1].summary,
                    tr("settings.scope", "Active profile · applies immediately")
                );
                assert_eq!(spec.subcommands[2].summary, spec.subcommands[1].summary);
            } else {
                assert!(!hints.is_empty(), "{name} has an empty static hint list");
                assert_eq!(spec.subcommands.len(), hints.len());
            }
            assert!(
                !spec.subcommands.is_empty(),
                "{name} has an empty enriched hint list"
            );
            assert!(usage.starts_with(&format!("/{name}")), "{name} usage");
            assert_eq!(Some(spec.usage.as_str()), usage_for(name));
            assert!(
                HINT_TABLE[index + 1..].iter().all(|(n, _, _)| n != name),
                "{name} appears twice in HINT_TABLE"
            );
            for hint in &spec.subcommands {
                assert!(!hint.name.is_empty() && !hint.summary.is_empty());
                assert!(!hint.name.contains(' '), "{name}: {:?}", hint.name);
            }
        }
        Ok(())
    }

    /// Drift: jeder Befehl mit Hinweisen existiert als Operation oder als
    /// echter lokaler Befehl (Ausnahmen nur über `ALLOWED_MISSING`).
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
        for (name, summary) in planned_commands() {
            CommandName::parse(name).map_err(ctx("planned name must be valid"))?;
            assert!(!summary.is_empty());
            assert!(
                built_in.find(name).is_none(),
                "'/{name}' is implemented as an operation; remove it from PLANNED_COMMANDS"
            );
            assert!(
                locals.iter().all(|s| s.name.as_str() != name),
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
        assert_eq!(names("models"), ["show", "set", "reset", "pick", "worker"]);
        assert_eq!(names("sandbox-lease"), ["status", "revoke"]);
        assert!(names("mode").contains(&"default"));
        assert!(names("tools").contains(&"profile"));
        assert_eq!(names("matrix"), ["show", "list", "replay", "compare"]);
        assert_eq!(names("jobs"), ["list", "show", "stop", "logs"]);
        assert_eq!(names("dream"), ["list", "show", "run", "status", "review"]);
        assert_eq!(
            names("palace"),
            [
                "list",
                "show",
                "search",
                "promote",
                "supersede",
                "edit",
                "link"
            ]
        );
        assert_eq!(
            names("diary"),
            ["today", "show", "search", "agents", "note"]
        );
        assert!(names("memory").contains(&"promote"));
        assert!(names("memory").contains(&"topics"));
        for sub in ["edit", "comment", "evidence", "approve", "reject"] {
            assert!(names("kanban").contains(&sub), "{sub}");
        }
        assert!(names("workbench").contains(&"retention"));
        // Runde 5, Teil E.
        for sub in ["allow", "deny", "rules", "rm", "log"] {
            assert!(names("permissions").contains(&sub), "{sub}");
        }
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
        assert_eq!(spec.usage, "/model [show|list|switch <model-id>]");
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
            "Show or revoke the host permit for shell.exec"
        );
        Ok(())
    }

    #[test]
    fn every_owned_hint_has_a_stable_key_and_translates_in_all_ten_locales() {
        use crate::i18n::{tr_for, with_language};
        use harw_config::UiLanguage;
        let mut keys = std::collections::BTreeSet::new();
        let mut count = 0;
        for (name, _, hints) in HINT_TABLE {
            for hint in *hints {
                count += 1;
                assert!(keys.insert(hint.key), "duplicate key: {}", hint.key);
            }
            for language in UiLanguage::ALL {
                with_language(language, || {
                    let localized = subcommand_hints(name);
                    assert_eq!(localized.len(), hints.len());
                    for (actual, owned) in localized.iter().zip(*hints) {
                        assert_eq!((actual.name, actual.args), (owned.name, owned.args));
                        assert_eq!(actual.summary, tr_for(language, owned.key, owned.english));
                        if language == UiLanguage::English {
                            assert_eq!(actual.summary, owned.english);
                        } else {
                            assert_ne!(
                                actual.summary, owned.english,
                                "{} / {language:?}",
                                owned.key
                            );
                        }
                    }
                });
            }
        }
        assert_eq!(
            count, 141,
            "all owned hints, including both workbench note rows"
        );
        let notes: Vec<_> = subcommand_hints("workbench")
            .into_iter()
            .filter(|hint| hint.name == "note")
            .collect();
        assert_eq!(notes.len(), 2);
        assert_ne!(notes[0].args, notes[1].args);
        assert_ne!(notes[0].summary, notes[1].summary);
    }

    #[test]
    fn local_planned_override_and_settings_metadata_rebuild_for_all_locales() -> TestResult {
        use crate::i18n::{tr_for, with_language};
        use harw_config::UiLanguage;
        let english = local_command_specs();
        let planned = planned_commands();
        assert_eq!(planned.len(), 15);
        assert_eq!(subcommand_hints("model")[2].args, "<model-id>");
        assert_eq!(subcommand_hints("export")[1].name, "--datei");
        assert_eq!(subcommand_hints("export")[1].args, "<path>");
        for language in UiLanguage::ALL {
            with_language(language, || -> TestResult {
                hint_table_names_and_enriched_hints_are_valid_unique_and_non_empty()?;
                let localized = local_command_specs();
                for (spec, original) in localized.iter().zip(&english) {
                    assert_eq!(spec.name, original.name);
                    assert_eq!(spec.aliases, original.aliases);
                    assert_eq!(spec.permission, original.permission);
                    assert_eq!(spec.busy, original.busy);
                    assert_eq!(spec.origin, original.origin);
                    assert_eq!(
                        spec.usage.split(" — ").next(),
                        original.usage.split(" — ").next()
                    );
                    if language != UiLanguage::English {
                        assert_ne!(
                            spec.summary, original.summary,
                            "{} / {language:?}",
                            spec.name
                        );
                    }
                }
                for ((name, summary), (original, english)) in
                    planned_commands().iter().zip(&planned)
                {
                    assert_eq!(name, original);
                    if language != UiLanguage::English {
                        assert_ne!(summary, english);
                    }
                }
                for (name, key, fallback) in SUMMARY_OVERRIDES {
                    let mut spec = CommandSpec::new(
                        *name,
                        Vec::<String>::new(),
                        CommandScope::TuiOnly,
                        PermissionTier::Operator,
                        OutputSurface::Inline,
                        CommandDomain::Misc,
                    )
                    .map_err(ctx("override spec"))?;
                    enrich(&mut spec);
                    assert_eq!(spec.summary, tr_for(language, key, fallback));
                }
                let syntax = "/matrix [show|list|replay|compare <run> <run>]";
                assert_eq!(
                    usage_for("matrix").and_then(|usage| usage.split(" — ").next()),
                    Some(syntax)
                );
                Ok(())
            })?;
        }
        assert_eq!(tr("settings.title", "Settings"), "Settings");
        Ok(())
    }

    #[test]
    fn enrich_preserves_supplied_syntax_hints_and_unknown_long_descriptions() -> TestResult {
        use crate::i18n::with_language;
        use harw_config::UiLanguage;
        for name in ["external", "model"] {
            let mut spec = CommandSpec::new(
                name,
                ["custom"],
                CommandScope::TuiOnly,
                PermissionTier::Operator,
                OutputSurface::Inline,
                CommandDomain::Misc,
            )
            .map_err(ctx("supplied spec"))?;
            spec.summary = if name == "external" {
                "Unbekannter unveränderter Text {value}. ".repeat(10)
            } else {
                "Supplied model description".to_owned()
            };
            spec.usage = format!("/{name} <modell-id> --datei <pfad>");
            spec.subcommands = vec![SubcommandHint::new(
                "switch",
                "<modell-id>",
                "Gelieferter Text {value}",
            )];
            let supplied = spec.clone();
            for language in UiLanguage::ALL {
                with_language(language, || enrich(&mut spec));
                assert_eq!(spec, supplied, "{name} / {language:?}");
            }
        }
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
