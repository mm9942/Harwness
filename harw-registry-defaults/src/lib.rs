//! `harw-registry-defaults` — Shared coding-agent ExtensionRegistry assembly.
//!
//! Used by both `harw-cli` and `harw-tui` so the runtime picture is identical
//! regardless of whether the user runs one-shot commands, the interactive TUI,
//! or the local echo path.
//!
//! # Responsibility
//! Bundles the extension crates (fs, doc, shell, deps, web, instructions,
//! project-discovery) into a single `ExtensionRegistry`, plus discovers the
//! current project context so the model knows where it is. Behind the
//! optional `browser` Cargo feature it additionally offers the Harwness
//! browser tool surface (`harw-tool-browser` over a `harw-browser-thirtyfour`
//! `FirefoxHost`) — only through `profile::browser_tool_provider` with an
//! explicit `BrowserOpenGrant`, never as part of a profile (W5 RD).
//!
//! Welche dieser Provider tatsächlich registriert werden, entscheidet das
//! [`profile::RegistryProfile`]: `Full` ist der bisherige Coding-Satz,
//! `ReadOnlyExplore`/`Research`/`Planning` sind die Profile der eingebauten
//! Kind-Agenten, `WorkspaceEdit` ist das Lese-/Schreibprofil ohne Shell und
//! ohne Netz (Telegram mit Workspace). Ein read-only Profil sieht `fs.write`
//! und `shell.exec` nicht einmal im Inventar — die Beschränkung ist keine
//! Prompt-Bitte, sondern ein Filter über dem Provider (siehe [`profile::RestrictedToolProvider`]).
//!
//! # Key types
//! - [`AssembledRegistry`]: registry + discovered project context + identity.
//! - [`assemble_default_registry`]: builds the `Full` profile from a given `cwd`.
//! - [`profile::assemble_registry`]: builds any profile (runs discovery once).
//! - [`profile::assemble_registry_for_project`]: builds any profile über einem
//!   **bereits** erkannten Projektkontext — der Weg, auf dem eine Sitzung ihre
//!   Kind-Registries montiert, ohne die Projekterkennung je Kind zu wiederholen.
//! - [`profile::assemble_registry_for_sandbox`]: wie oben, registriert aber nur
//!   Werkzeuge, deren Recht der gewährte `PermissionSet` trägt (W5 RD).
//! - [`profile::assemble_registry_for_project_with_definition_access`] /
//!   [`profile::assemble_registry_for_sandbox_with_definition_access`]: wie
//!   die beiden vorherigen, nehmen aber zusätzlich eine optionale
//!   [`profile::AgentDefinitionAccess`] entgegen (Nachtrag K3) — nur für
//!   [`profile::RegistryProfile::AgentStewardship`] relevant; ohne sie bleibt
//!   `agent-steward` fail-closed bei `agents.validate`/`agents.list_proposals`.
//! - [`authority`]: Werkzeug→Recht, Rollen-Reducer (`reduce_to_read_only`,
//!   `reduce_to_read_registry`, `reduce_to_read_network`).
//! - [`research_web`]: Egress-Policy der Rolle `researcher-web` aus
//!   `[network].researcher_web_hosts`.
//! - [`embedded_agents`]: die eingebauten Agentendefinitionen als TOML und IR.
//!
//! # Feature `browser`
//! Disabled by default so the standard binary stays lean. When enabled,
//! `profile::browser_tool_provider(grant)` constructs a `FirefoxHost` and a
//! `HarwnessBrowserToolProvider` whose `browser.open` authority is exactly the
//! given grant. No profile — including [`assemble_default_registry`] — registers
//! browser tools on its own (F-073: previously `Full` did, without any grant).
//! `FirefoxHost::new` never starts Firefox or geckodriver at construction time;
//! a missing driver only surfaces as a normal tool execution error the first
//! time a `browser.*` tool actually dispatches.
//!
//! # Concurrency
//! `assemble_default_registry` is synchronous; all produced providers are
//! `Send + Sync`.

#![forbid(unsafe_code)]

mod error;

pub mod agent_definition_tools;
pub mod authority;
// #22 wave 2B: tool name → provider, capability class, runner feature.
pub mod capability_catalog;
// The agent compiler's built-in defaults (`harw_agent_compiler::builtins`).
pub mod compiler_defaults;
// Parsen/Senken der Agentendefinitionen einer Konfiguration: `harw-config`
// (Schicht I) reicht sie ungeparst weiter.
pub mod config_agents;
pub mod diary_tools;
pub mod embedded_agents;
pub mod kanban_tools;
pub mod palace_tools;
pub mod profile;
pub mod research_web;
pub mod roster;
pub mod skill_proposal_tools;
pub mod skill_tools;
pub mod tool_index;
pub mod workbench_tools;

#[cfg(test)]
mod test_support;

use std::path::PathBuf;
use std::sync::Arc;

use harw_extension_api::allow_rules::{AllowRuleSet, RuleDecision};
use harw_extension_api::approval_mode::ApprovalModeCell;
// Runde 5, Teil E: Naht zum Auto-Modus-Klassifizierer.
use harw_extension_api::auto_mode::AutoApprovalGate;
use harw_extension_api::{
    ApprovalDecision, ApprovalHandler, ApprovalMode, ExtFuture, ExtensionRegistry, ToolCall,
};
use harw_instructions::AgentIdentity;
use harw_project_discovery::ProjectContext;
// Remote-OCR-Status von `doc.read_pdf`; re-exportiert, damit die TUI den
// Freigabe-Hinweis ohne eigene `harw-tool-doc`-Kante testen kann.
pub use harw_tool_doc::{RemoteOcrApproval, RemoteOcrTarget, remote_ocr_target};

pub use agent_definition_tools::{
    AgentDefinitionToolProvider, DefinitionAuthorCeiling, DefinitionWriteMode,
    UiaSelfDocumentToolProvider,
};
pub use authority::{
    AuthorityReducer, authority_reducer_for_role, delegation_targets_for_role, tool_permission,
};
pub use config_agents::{AgentDefinitionMeta, ConfigAgents, discover_run_agent_definitions};
pub use diary_tools::DiaryToolProvider;
pub use error::{RegistryDefaultsError, RegistryDefaultsResult};
pub use kanban_tools::KanbanReadToolProvider;
pub use palace_tools::PalaceToolProvider;
pub use profile::{
    AgentDefinitionAccess, HostPermitWiring, IdentityOverrides, JobWiring, RegistryProfile,
    RestrictedToolProvider, agent_definition_tool_names_for_access, assemble_registry,
    assemble_registry_for_project, assemble_registry_for_project_with_definition_access,
    assemble_registry_for_sandbox, assemble_registry_for_sandbox_with_definition_access,
    profile_for_role, role_names,
};
pub use research_web::{
    OpenWebApprovalPolicy, install_web_tools, open_web_approval_notice,
    researcher_web_network_scope, researcher_web_policy, role_may_use_open_web,
};
pub use roster::{AgentRoster, CustomAgentWiring, RosterEntry, RosterSource};
pub use skill_proposal_tools::{SkillAuthorCeiling, SkillProposalStore, SkillProposalToolProvider};
pub use skill_tools::{SkillCatalogToolProvider, skill_catalog_hint};
pub use workbench_tools::{
    WorkbenchContextProvider, WorkbenchReadToolProvider, WorkbenchToolProvider,
};

/// Die Werkzeuge, die ohne Nutzerrückfrage ausgeführt werden dürfen.
///
/// # Beschreibung
/// Die Liste umfasst ausschließlich Werkzeuge, die **nachweislich** nur lesen
/// und an keiner Fläche eine Freigabe (`ApprovalPolicy != None`) deklarieren:
/// lesende Dateisystem-Werkzeuge, das lesende PDF-Werkzeug (`doc.read_pdf`,
/// `harw-tool-doc`), die Explorer-Werkzeuge (`explore.*`), die
/// Dependency-Werkzeuge, `lens.ask`, die Web-Recherche (deren Netzgrenze die
/// Host-Allowlist der Sandbox zieht, nicht die Freigabe) und die lesenden
/// Status-Operationen `status`/`ps`. `process.list`/`process.kill` gehören
/// nie dazu (`ExecuteProcess`, nur im Profil `Full`).
///
/// Alles Mutierende — `fs.write`, `shell.exec`, `stop`, `plan`, `goal` —, alles
/// mit Nebenwirkung über einen anderen Weg (`diff`, `explore`, `research_*`,
/// `analyze`) und jedes unbekannte Werkzeug bleibt freigabepflichtig
/// (fail-closed).
///
/// Die Liste ist eine **Obergrenze**, keine Garantie:
/// `harw_core::turn_loop::check_approval` befragt jeden registrierten
/// `ApprovalHandler` und
/// aggregiert `Deny` > `AskUser` > `Allow`. Ein hinter
/// [`DefaultApprovalPolicy`] angehängter Handler (etwa `ConfigApprovalPolicy`
/// aus `[policy].require_approval_for`) kann ein hier gelistetes Werkzeug
/// deshalb nur weiter einschränken, nie ein nicht gelistetes lockern — das gilt
/// erst mit der Aggregation (W1-05); vorher entschied der erste Nicht-`Allow`.
///
/// # Kopplung an die Deklarationen
/// - `harw-ops/tests/approval_declaration_gate.rs` prüft gegen die echte
///   Operations-Registry: keine Operation mit `Surface::ModelTool { approval
///   != None }` darf hier stehen.
/// - Die Tests `auto_approved_tools_are_a_subset_of_the_read_only_surface` und
///   `tools_with_a_declared_approval_are_never_auto_approved` (unten) prüfen
///   die Richtung *Allowlist ⊆ read-only*. Die frühere Richtung (jedes
///   beworbene Planning-Werkzeug muss auto-freigegeben sein) hatte
///   `plan`/`goal` in die Liste gezwungen (Befund F-014/G-003); die
///   Fan-out-Deckung gilt deshalb nur noch für Werkzeuge, die **dieses Crate**
///   selbst registriert (`read_only_profile_tools_registered_here_stay_auto_approved`).
pub const AUTO_APPROVED_TOOLS: &[&str] = &[
    // Lesende Dateisystem-Werkzeuge (`harw-tool-fs`).
    //
    // `fs.search` bleibt vorerst auto-freigegeben: der Symlink-Escape-Befund
    // (W3 A1) wird im Werkzeug selbst gehärtet (W1-02), nicht über die
    // Freigabe. Eine Rückfrage würde jeden read-only Fan-out blockieren
    // (Kinder laufen mit `allow_pause = false`), ohne den Escape im
    // `FullAccess`-Modus zu schließen.
    "fs.read",
    "fs.list",
    "fs.search",
    "fs.glob",
    "fs.grep",
    // Lesendes PDF-Werkzeug (`harw-tool-doc`, Anbindung W3): liest eine
    // PDF-Datei aus dem Workspace seitenweise als Text/Markdown — dieselbe
    // Berechtigung (`Permission::ReadWorkspace`) und dieselbe read-only
    // Eigenschaft wie `fs.read`, erscheint deshalb überall, wo ein lesender
    // FS-Provider registriert wird (siehe `profile::DOC_TOOLS`). Ausnahme:
    // würde der Aufruf die Datei an einen Remote-OCR-Dienst schicken und
    // steht `[tools.doc].remote_ocr` auf `ask`, fragt `review` unter
    // `ask`/`auto` trotzdem (siehe [`remote_ocr_requires_approval`]).
    "doc.read_pdf",
    // Workspace-Explorer (`harw-tool-explorer`) — Baum, Projekte, Relationen,
    // Suche ab der Workspace-Wurzel; rein lesend, `Permission::ReadWorkspace`
    // wie `fs.read` (siehe `profile::EXPLORER_TOOLS`).
    "explore.tree",
    "explore.projects",
    "explore.relations",
    "explore.find",
    // Dependency-Werkzeuge (`harw-tool-deps`) — ausnahmslos read-only.
    "deps.graph",
    "deps.locked",
    "deps.source_read",
    "deps.source_search",
    "deps.source_list",
    // Retrieval (`harw-tool-lens`) — rein lesend: eine Frage rein, verschmolzene
    // Treffer raus. Es gibt in dieser Crate kein schreibendes Lens-Werkzeug;
    // der Schreibpfad ist `harw-lens-source` und entsteht nie auf Zuruf eines
    // Agenten. Die Sichtbarkeitsgrenze zieht `ReadScope`, nicht die
    // Genehmigung — der Aufrufer kann seinen Bereich nicht selbst wählen.
    "lens.ask",
    // Web-Recherche (`harw-tool-web`) — alle vier im Profil `Research` (Rolle
    // `researcher-web`, ohne `fs.*`/`deps.*`); `web.fetch`/`web.search`
    // zusätzlich in den Erkundungsprofilen (`explorer`, `uia-explorer`,
    // Nutzerentscheidung) und in den UIA-Helferprofilen (`uia-worker`,
    // `uia-writer`, Nutzerentscheidung „kurz online recherchieren“). Die
    // Netzgrenze zieht jeweils der `NetworkScope` der Sandbox bzw. die
    // `EgressPolicy` (W5 RD), nicht die Freigabe — rein lesend.
    "web.fetch",
    "web.docs_rs",
    "web.crates_io",
    "web.search",
    // Lesende Status-Operationen (`harw-ops`, `model_tool(readonly, approval =
    // "none")`, ohne Seitenpfad in andere Executor).
    "status",
    "ps",
    // Fan-out-Operation der Orchestratoren (`harw-core-bridge::delegate_wave`,
    // Plan Punkt 1; `model_tool(readonly = false, approval = "none")`). Die
    // Operation selbst schreibt nichts: jedes gestartete Kind läuft mit seinem
    // eigenen, per `AuthorityReducer` gedeckelten Rechtesatz, seiner eigenen
    // Freigabe-Politik und unter dem Budget-Deckel der Welle
    // (`wave_budget_cap`). Ein Child-Orchestrator läuft mit
    // `allow_pause = false` und könnte eine Rückfrage nie beantworten — eine
    // Freigabepflicht hier würde jede verschachtelte Welle blockieren.
    // Registriert wird sie ausschließlich für Orchestrator-Rollen
    // (`profile::composition_tools_for_role`), nie über ein
    // `RegistryProfile`.
    "delegate_wave",
    // Lesende Wissenswerkzeuge (Plan Teil D, „Sicherheit“): nur lesend,
    // `Permission::ReadWorkspace`, ohne Freigabe-Deklaration und mit eigenem,
    // vom Modell nicht erweiterbaren Scope — `workbench.show` zeigt nur die
    // Workbench des gebundenen Projekts ([`WorkbenchReadToolProvider`]),
    // `diary.read` nur die eigenen Einträge des Agenten
    // ([`DiaryToolProvider`], Agent-Id fest beim Bau), `palace.search`/
    // `palace.recall` nur `established`-Knoten mit gedeckelten Hops
    // ([`PalaceToolProvider`]). Registriert werden sie nicht über ein
    // `RegistryProfile`, sondern von der Composition-Root (Wurzel) bzw.
    // für Kind-Rollen, deren Definition sie zulässt.
    "workbench.show",
    "diary.read",
    "palace.search",
    "palace.recall",
    // Runde 5, Teil H: `agent.result` (`harw-core-bridge::AgentResultOperation`,
    // `model_tool(readonly, approval = "none")`) liest nur den bereits
    // erzeugten Antworttext eines **eigenen** Kindes aus dem Ergebnisarchiv
    // des Spawners; die Eltern-Kind-Bindung prüft der Spawner, nicht das
    // Modell. Registriert über die Composition-Root
    // (`profile::CHILD_RESULT_TOOLS`), nie über ein `RegistryProfile`.
    "agent.result",
    // Runde 5, Teil K: `agent.status` (`harw-core-bridge::AgentStatusOperation`,
    // `model_tool(readonly, approval = "none")`) liest nur das Register der
    // eigenen Hintergrund-Agenten (Status, Fortschritt); die Eltern-Kind-
    // Bindung prüft der Spawner. Nur Wurzel, nur TUI (Composition-Root).
    // `agent.cancel` steht bewusst NICHT hier, sondern in `ALWAYS_ASK_TOOLS`.
    "agent.status",
    // Runde 5, Teil M: `agent.message`/`parent.message`
    // (`harw-core-bridge::agent_messaging`, `model_tool(readonly,
    // approval = "none")`) transportieren nur Text zwischen direkt
    // verbundenen Sitzungen (eigenes, laufendes Kind bzw. direkter
    // Elternteil; Bindung prüft der Spawner, 4-KiB-Deckel, begrenzte
    // Postfächer, Rate-Limit). Keine Schreibwirkung auf Workspace, Host oder
    // Netz, keine Rechte-Erweiterung — Nutzerentscheidung: keine
    // Freigabepflicht, nie in `ALWAYS_ASK_TOOLS`.
    "agent.message",
    "parent.message",
    // Plan R9, Teil A: `skills.search`/`skills.load`
    // (`crate::skill_tools::SkillCatalogToolProvider`) lesen nur den bei der
    // Montage eingefrorenen Skill-Katalog (vertraute Layer plus eingebettetes
    // Bündel) im Host-Prozess — kein Workspace, kein Netz, kein Prozess, keine
    // Rechteklasse. Ein Skill verleiht keine Rechte. Registriert von der
    // Composition-Root für jede Rolle aus
    // `profile::skill_catalog_tools_for_role`, nie über ein `RegistryProfile`.
    "skills.search",
    "skills.load",
    // Plan R9, Teil F: die lesenden Job-Werkzeuge (`harw-tool-job`,
    // `JOB_READ_TOOLS`): `job.status`/`job.logs`/`job.list` lesen nur
    // Zustand und Logdateien eigener Jobs bzw. der Jobs von Nachfahren
    // (Besitzprüfung über die Sitzung aus dem Ausführungskontext);
    // `job.wait` ist ein kurzes Polling (höchstens 60 s; das Jobende kommt
    // als Notiz, R18 F8). Keine
    // Schreibwirkung, kein Prozessstart. `job.start` fragt wie `shell.exec`,
    // `job.stop` wie jedes andere Werkzeug mit Wirkung (nicht in
    // `ALWAYS_ASK_TOOLS`, eine Allow-Regel greift).
    "job.status",
    "job.logs",
    "job.list",
    "job.wait",
    // harw-tool-tunnel-v1: die lesenden Tunnel-Werkzeuge (`TUNNEL_TOOLS`,
    // `profile::TUNNEL_TOOLS`): `tunnel.status`/`tunnel.list` lesen nur den
    // Zustand verwalteter Tunnels des Aufrufers, `tunnel.stop` beendet nur
    // einen eigenen Tunnel (Besitzprüfung analog `harw-tool-job`, Wiring
    // folgt im Plan-Knoten `job-lifecycle`). Keine Schreibwirkung, kein
    // Prozessstart. `tunnel.start` fragt wie `job.start`/`shell.exec` (nicht
    // in `ALWAYS_ASK_TOOLS`, eine Allow-Regel greift); bis zur Montage bleibt
    // der Eintrag die statische Vertrags-Obermenge und nichts wird beworben.
    "tunnel.status",
    "tunnel.list",
    // Runde 5, Teil F: `ask_user` (`harw-tool-plan`) liest nur die Antwort der
    // Nutzerin aus einem eigenen Auswahlfenster; es schreibt nichts, startet
    // nichts und geht nicht ins Netz. Nur Wurzel, nur TUI (sonst
    // fail-closed).
    "ask_user",
    // Runde 5 (Integration, Nutzerwunsch „wie Claude Code“): `plan.write`
    // schreibt ausschließlich die Plan-Datei unter `.harw/plans/<slug>.md`
    // (Slug-Grammatik, Symlink-Sperre, 256-KiB-Deckel, harw-tool-plan) und
    // lehnt außerhalb des Plan-Modus ab. Kein Workspace-Quellcode, kein
    // Prozess, kein Netz — deshalb ohne Rückfrage unter `Delegated`/`Full`
    // (die Plan-Stufe nutzt `Delegated`). Unter `AlwaysAsk` fragt es weiter.
    "plan.write",
    // Runde 7, Teil M: die beiden Game-Master-Werkzeuge ohne Freigabe-
    // Deklaration (`model_tool(approval = "none")`, `harw-ops/src/matrix/
    // game_master.rs`): `matrix.status` liest nur Laufstand bzw.
    // Beispielszenarien, `matrix.draft_scenario` validiert ein Szenario und
    // legt es ausschließlich im Matrix-Speicher des Profils ab
    // (`<profil>/knowledge/matrix/scenarios/<slug>.toml`, Slug-Grammatik,
    // kein Workspace, kein Prozess, kein Netz — dieselbe Klasse wie
    // `plan.write`). Registriert nur für `matrix-game-master`
    // (`profile::matrix_tools_for_role`). `matrix.start`/`matrix.run`/
    // `matrix.finish` fragen immer und stehen bewusst NICHT hier.
    "matrix.status",
    "matrix.draft_scenario",
    // Plan R9: `matrix.add_fact` trägt einen recherchierten Fakt mit Belegen
    // ins Journal des eigenen Laufs ein (nur Matrix-Speicher, dieselbe Klasse
    // wie `matrix.draft_scenario`; `model_tool(approval = "none")`).
    "matrix.add_fact",
    // R18 (D-E): `work_driver.report` schreibt nur den strukturierten Bericht
    // in den Slot des eigenen Worker-Turns (`profile::WORK_DRIVER_REPORT_TOOLS`)
    // — kein Workspace, kein Prozess, kein Netz. Der Worker läuft
    // unbeaufsichtigt; eine Rückfrage würde jede Runde als „blocked“ beenden.
    "work_driver.report",
    // R18 (D-B): die lesenden `gateway.*`-Werkzeuge
    // (`profile::GATEWAY_READ_TOOLS`, `model_tool(readonly, approval =
    // "none")`), nur an einer UIA-Wurzel registriert. Sie lesen Zustand des
    // Gateways bzw. des lokalen harw-Homes (Diagnose, Kanal-Konfiguration);
    // Ausgaben sind bereinigt, nie Tokens. Die Mutationen stehen in
    // `ALWAYS_ASK_TOOLS`.
    "gateway.status",
    "gateway.connections.list",
    "gateway.sessions.list",
    "gateway.listeners.list",
    "gateway.tools.list",
    "gateway.channels.list",
    "gateway.channels.connect_info",
    "gateway.health",
    "gateway.logs",
    // Bewusst entfernt (W1-05, Register F-014, G-003, G-004, F-043, G-068):
    // - `plan`, `goal`: deklarieren `model_tool(approval = "always")` und
    //   mutieren PlanStore bzw. Ziel; die Auto-Freigabe überstimmte die
    //   Deklaration (Approval-Bypass im Root, Modus `Delegated`).
    // - `explore`, `research_deps`, `research_web`, `analyze`: als `readonly`
    //   gelabelt, schreiben aber Findings/PlanStore und starten Kind-Agenten
    //   (Kosten, Budget) — keine read-only Oberfläche.
    // - `diff`: ruft den `shell.exec`-Executor direkt auf und umginge damit
    //   dessen Freigabe; `git diff` wertet zudem Repo-Konfiguration aus.
    // - `mode`: hat keine Modell-Tool-Fläche; der Eintrag war wirkungslos und
    //   hätte ein gleichnamiges Fremdwerkzeug (Plugin/MCP) freigeschaltet.
    // - `kanban.list`, `kanban.show` (Plan D2): zwar rein lesend, aber auf
    //   Wunsch der Nutzerin fragt **jeder** Aufruf — Kanban nur, wenn sie
    //   ausdrücklich darum bittet.
];

/// Werkzeuge, die unter `ask` und `auto` **immer** eine Rückfrage auslösen —
/// selbst dann, wenn eine passende [`AllowRuleSet`]-Regel sonst automatisch
/// freigeben würde (Welle FANIN-K, Nachtrag K3, „Freigabe-Härtung“).
///
/// # Ausnahme `FullAccess` (Nutzerentscheidung 2026-09-24)
/// Unter [`ApprovalMode::FullAccess`] fragt harw **nie** — auch nicht für
/// diese Werkzeuge. „Full Access" heißt: keine Bestätigung, auch nicht für
/// `process.kill`, `host.sudo_exec` oder `agent.cancel`. Wer Rückfragen will,
/// wählt `ask` oder `auto`.
///
/// # Description
/// `agents.write_uia` (`crate::agent_definition_tools`, Nachtrag K) legt ein
/// vollständiges neues UIA-Bündel an; `agents.commit_proposal`
/// (Nachtrag K2/K3) übernimmt einen zuvor abgelegten Agentendefinitions-
/// Vorschlag dauerhaft. Beide verlangen bei `review_level = "user_required"`
/// ein `user_confirmed: true` im Aufrufargument — aber dieses Feld ist keine
/// echte Freigabe, nur eine vom Modell behauptete Zeichenkette. Ohne diese
/// Liste könnte eine `/permissions`-Allow-Regel für den Werkzeugnamen den
/// vorgelagerten Freigabepfad umgehen, während das Modell `user_confirmed:
/// true` selbst setzt. (Unter `FullAccess` hat die Nutzerin genau das
/// ausdrücklich gewählt.)
///
/// [`DefaultApprovalPolicy::review`] prüft diese Liste deshalb unter
/// `ask`/`auto` **vor** jeder [`AllowRuleSet`]-Auswertung und vor der
/// Modus-Logik. Jedes
/// hier gelistete Werkzeug bleibt zusätzlich außerhalb von
/// [`AUTO_APPROVED_TOOLS`] (das gilt bereits, da beide Werkzeuge schreiben).
///
/// # Warum nur diese beiden Agenten-Werkzeuge
/// `agents.write_definition` legt lediglich einen Vorschlag ab (nie eine
/// aktive Definition) — dessen Prüfung ist Sache von `agents.commit_proposal`,
/// nicht des Ablegens selbst. `agents.reject_proposal` verwirft nur, verleiht
/// keine Rechte. Beide bleiben normal freigabepflichtig über
/// [`AUTO_APPROVED_TOOLS`]/die Modus-Logik, aber nicht zusätzlich über diese
/// Liste.
///
/// # `skills.commit_proposal`
/// Übernimmt einen Skill-Vorschlag dauerhaft in `<profil>/skills/`
/// ([`skill_proposal_tools`]); wie `agents.commit_proposal` ist ein
/// `user_confirmed: true` im Argument keine echte Freigabe. `skills.propose`
/// und `skills.reject_proposal` bleiben normal freigabepflichtig.
///
/// # `process.kill`
/// Schickt SIGKILL an Host-Prozesse — nicht umkehrbar. Deshalb fragt es wie
/// die beiden Agenten-Werkzeuge unter `ask`/`auto` immer, trotz passender
/// Allow-Regel; unter `FullAccess` nicht.
///
/// # `host.sudo_exec`
/// Führt einen Root-Befehl über `sudo` aus (Runde 5, Teil B). Neben dem
/// eigenen TUI-Freigabefenster (Passwort/„Freigeben“) fragt es unter
/// `ask`/`auto` wie `process.kill` immer auch über die normale Freigabe —
/// eine Allow-Regel überspringt das nie. Unter `FullAccess` fragt keine der
/// beiden Stufen; die TUI zeigt dann nur noch das Passwortfeld, wenn `sudo`
/// selbst ein Passwort verlangt.
pub const ALWAYS_ASK_TOOLS: &[&str] = &[
    "agents.write_uia",
    "agents.commit_proposal",
    "skills.commit_proposal",
    "process.kill",
    // Runde 5, Teil B: Root-Befehl über sudo — fragt zusätzlich zum eigenen
    // TUI-Fenster immer über die normale Freigabe.
    "host.sudo_exec",
    // Runde 5, Teil K: Abbruch eines eigenen Hintergrund-Agenten — nie
    // automatisch im Auto-Modus (im Voll-Modus fragt nichts).
    "agent.cancel",
    // R18 (D-B): die mutierenden `gateway.*`-Werkzeuge
    // (`profile::GATEWAY_MUTATION_TOOLS`, `model_tool(approval = "always")`)
    // — Widerruf, Draining, Listener, Werkzeug-Freigaben fragen unter
    // `ask`/`auto` immer, auch gegen eine Allow-Regel.
    "gateway.connections.revoke",
    "gateway.drain",
    "gateway.listeners.set",
    "gateway.tools.grant",
    "gateway.tools.narrow",
];

/// Werkzeug, dessen Remote-OCR-Versand eine eigene Freigabe braucht.
pub const REMOTE_OCR_TOOL: &str = "doc.read_pdf";

/// Quelle des Remote-OCR-Status für [`DefaultApprovalPolicy`].
///
/// Standard ist [`harw_tool_doc::remote_ocr_target`] (der prozessweite
/// Status aus `harw-cli`s `install_doc_ocr`); Tests setzen über
/// [`DefaultApprovalPolicy::with_remote_ocr_status`] eine eigene Funktion.
pub type RemoteOcrStatus = fn() -> Option<RemoteOcrTarget>;

/// Hinweis an das Modell, wenn ein Remote-OCR-Aufruf nicht freigegeben
/// wurde oder niemand ihn freigeben kann.
pub const REMOTE_OCR_DENIAL_HINT: &str =
    "remote OCR needs approval; retry with `backend: \"native\"` for local extraction";

/// Ob `call` unter dem Ziel `target` eine Remote-OCR-Freigabe braucht.
///
/// # Beschreibung
/// `true` genau dann, wenn `call` [`REMOTE_OCR_TOOL`] ist, ein
/// Remote-OCR-Client installiert ist (`target` ist `Some`), sein Modus
/// [`harw_tool_doc::RemoteOcrApproval::Ask`] ist und die Argumente nicht
/// `backend: "native"` verlangen. Das Prädikat selbst kennt keinen
/// Freigabemodus; [`DefaultApprovalPolicy::review`] wertet es nur unter
/// `ask`/`auto` aus — unter [`ApprovalMode::FullAccess`] fragt auch der
/// Remote-OCR-Versand nicht (Nutzerentscheidung 2026-09-24).
///
/// # Examples
/// ```rust
/// use harw_extension_api::{ToolCall, ToolName};
/// use harw_registry_defaults::remote_ocr_requires_approval;
/// use harw_tool_doc::{RemoteOcrApproval, RemoteOcrTarget};
/// use serde_json::json;
///
/// let target = RemoteOcrTarget {
///     host: "api.mistral.ai".to_owned(),
///     approval: RemoteOcrApproval::Ask,
/// };
/// let call = ToolCall {
///     id: Default::default(),
///     name: ToolName::new("doc.read_pdf"),
///     arguments: json!({ "path": "a.pdf" }),
/// };
/// assert!(remote_ocr_requires_approval(&call, Some(&target)));
/// assert!(!remote_ocr_requires_approval(&call, None));
/// ```
#[must_use]
pub fn remote_ocr_requires_approval(call: &ToolCall, target: Option<&RemoteOcrTarget>) -> bool {
    call.name.as_str() == REMOTE_OCR_TOOL
        && target.is_some_and(|target| target.approval == harw_tool_doc::RemoteOcrApproval::Ask)
        && harw_tool_doc::arguments_request_remote_ocr(&call.arguments)
}

/// Host, an den `call` die Datei schickt, wenn es ein `doc.read_pdf` mit
/// Remote-OCR-Pfad ist und ein Client installiert ist (Modus egal); sonst
/// `None`. Für die Verlaufsanzeige der TUI.
///
/// # Examples
/// ```rust
/// use harw_extension_api::{ToolCall, ToolName};
/// use harw_registry_defaults::{RemoteOcrApproval, RemoteOcrTarget, remote_ocr_host};
/// use serde_json::json;
///
/// let target = RemoteOcrTarget {
///     host: "api.mistral.ai".to_owned(),
///     approval: RemoteOcrApproval::Auto,
/// };
/// let call = ToolCall {
///     id: Default::default(),
///     name: ToolName::new("doc.read_pdf"),
///     arguments: json!({ "path": "a.pdf" }),
/// };
/// assert_eq!(remote_ocr_host(&call, Some(&target)), Some("api.mistral.ai"));
/// ```
#[must_use]
pub fn remote_ocr_host<'a>(
    call: &ToolCall,
    target: Option<&'a RemoteOcrTarget>,
) -> Option<&'a str> {
    if call.name.as_str() != REMOTE_OCR_TOOL
        || !harw_tool_doc::arguments_request_remote_ocr(&call.arguments)
    {
        return None;
    }
    target.map(|target| target.host.as_str())
}

/// Wie [`remote_ocr_requires_approval`], gegen den prozessweiten
/// Remote-OCR-Status ([`harw_tool_doc::remote_ocr_target`]). Für Aufrufer,
/// die die Rückfrage vorhersagen müssen (`harw-runtime`s
/// `AskResolutionPolicy`), ohne eine eigene Politik zu tragen.
#[must_use]
pub fn call_needs_remote_ocr_approval(call: &ToolCall) -> bool {
    remote_ocr_requires_approval(call, harw_tool_doc::remote_ocr_target().as_ref())
}

/// Ob `call` ein lesendes fs-Werkzeug ist, dessen `path` über einen Symlink
/// in ein noch nicht freigegebenes Verzeichnis **außerhalb** des Workspace
/// führt (siehe [`harw_tool_fs::symlink`]). Reine Prüfung ohne
/// Zustandsänderung — für die Vorhersage der Ask-Auflösung.
#[must_use]
pub fn call_needs_symlink_approval(call: &ToolCall) -> bool {
    harw_tool_fs::symlink::global()
        .review_call(call.name.as_str(), &call.arguments)
        .is_some()
}

/// Hinweistext für den Freigabe-Dialog, wenn `call` einem Symlink nach
/// außen folgen würde: nennt Link, Ziel und das Verzeichnis, für das die
/// Freigabe bis zum Sitzungsende gemerkt wird.
#[must_use]
pub fn symlink_approval_notice(call: &ToolCall) -> Option<String> {
    harw_tool_fs::symlink::global()
        .review_call(call.name.as_str(), &call.arguments)
        .map(|target| {
            format!(
                "{}. Eine Freigabe gilt für {} bis zum Sitzungsende.",
                target.describe(),
                target.grant_dir.display()
            )
        })
}

/// Vermerkt für `call` die Freigabe eines Symlink-Ziels außerhalb des
/// Workspace, die [`DefaultApprovalPolicy::review`] gerade erteilt (Full
/// Access) oder dem Menschen vorlegt: sie wirkt erst, wenn genau dieser
/// Aufruf ausgeführt wird — ein abgelehnter Aufruf läuft nie
/// ([`harw_tool_fs::symlink::SymlinkAccess::approve_call`]).
///
/// # Returns
/// `true`, wenn `call` ein solches Ziel betrifft.
fn note_symlink_approval(call: &ToolCall) -> bool {
    let access = harw_tool_fs::symlink::global();
    let Some(target) = access.review_call(call.name.as_str(), &call.arguments) else {
        return false;
    };
    access.approve_call(call.name.as_str(), &call.arguments, &target);
    true
}

/// Hinweistext für den Freigabe-Dialog, wenn `call` die Datei an einen
/// Remote-OCR-Dienst schicken würde und dafür gefragt wird.
///
/// # Returns
/// `Some(text)` mit dem Host aus dem prozessweiten Remote-OCR-Status
/// ([`harw_tool_doc::remote_ocr_target`]), wenn
/// [`remote_ocr_requires_approval`] für `call` gilt; sonst `None`.
#[must_use]
pub fn remote_ocr_approval_notice(call: &ToolCall) -> Option<String> {
    remote_ocr_approval_notice_with(call, harw_tool_doc::remote_ocr_target().as_ref())
}

/// Wie [`remote_ocr_approval_notice`], mit explizit übergebenem Ziel
/// (testbar ohne prozessweiten Status).
///
/// # Examples
/// ```rust
/// use harw_extension_api::{ToolCall, ToolName};
/// use harw_registry_defaults::remote_ocr_approval_notice_with;
/// use harw_tool_doc::{RemoteOcrApproval, RemoteOcrTarget};
/// use serde_json::json;
///
/// let target = RemoteOcrTarget {
///     host: "api.mistral.ai".to_owned(),
///     approval: RemoteOcrApproval::Ask,
/// };
/// let call = ToolCall {
///     id: Default::default(),
///     name: ToolName::new("doc.read_pdf"),
///     arguments: json!({ "path": "a.pdf" }),
/// };
/// let notice = remote_ocr_approval_notice_with(&call, Some(&target));
/// assert!(notice.is_some_and(|text| text.contains("api.mistral.ai")));
/// ```
#[must_use]
pub fn remote_ocr_approval_notice_with(
    call: &ToolCall,
    target: Option<&RemoteOcrTarget>,
) -> Option<String> {
    if !remote_ocr_requires_approval(call, target) {
        return None;
    }
    target.map(|target| {
        format!(
            "Sends the file contents to {} (remote OCR). Deny, or ask for backend=native, \
             to keep it local.",
            target.host
        )
    })
}

/// Begründung der harten Ablehnung, wenn unter [`ApprovalMode::FullAccess`]
/// eine ausdrückliche `/permissions`-Deny-Regel auf `call` passt.
///
/// # Beschreibung
/// Unter `FullAccess` fragt harw nie; eine Deny-Regel ist dort deshalb keine
/// Rückfrage (wie unter `ask`/`auto`), sondern eine Ablehnung mit diesem
/// Grund. Das Modell erfährt so, dass die Nutzerin den Aufruf selbst
/// gesperrt hat.
///
/// # Examples
/// ```rust
/// use harw_extension_api::{ToolCall, ToolName};
/// use harw_registry_defaults::full_access_deny_reason;
///
/// let call = ToolCall {
///     id: Default::default(),
///     name: ToolName::new("shell.exec"),
///     arguments: serde_json::json!({ "command": "git push" }),
/// };
/// assert!(full_access_deny_reason(&call).contains("shell.exec"));
/// ```
#[must_use]
pub fn full_access_deny_reason(call: &ToolCall) -> String {
    format!(
        "'{}' is blocked by a /permissions deny rule (full access never asks, so the rule refuses the call)",
        call.name.as_str()
    )
}

/// Default approval boundary for the built-in coding-agent tool set.
///
/// Only the known read-only tools and operations execute without a pause
/// ([`AUTO_APPROVED_TOOLS`]). Every other tool, including `fs.write`,
/// `shell.exec`, and any future tool registered by default, requires an
/// explicit approval decision. This default is deliberately fail-closed so
/// adding a tool cannot silently widen agent authority.
///
/// # Woher der Freigabemodus kommt
/// Die Politik trägt ihre [`ApprovalModeCell`] selbst (G-009): Es gibt keinen
/// prozessweiten Modus mehr, den Root, Kinder und Job-Worker gemeinsam sähen.
/// Wer eine Sitzung zusammenbaut, entscheidet mit der übergebenen Zelle, wer
/// den Modus mit wem teilt — ein Klon teilt ihn, [`ApprovalModeCell::detached`]
/// löst ihn. Die Zelle wird bei **jedem** [`ApprovalHandler::review`] frisch
/// gelesen, damit eine Umschaltung sofort und nicht erst im nächsten Turn wirkt.
///
/// # Freigaberegeln (`AllowRuleSet`)
/// Vor der Modus-Logik befragt [`Self::review`] die geteilte
/// [`AllowRuleSet`] dieser Politik (Contract §2/§4, Plan Schritt 4):
/// - `Some(`[`RuleDecision::Deny`]`)`: dieselbe „immer fragen“-Auskunft, die
///   die Politik auch für nicht in [`AUTO_APPROVED_TOOLS`] gelistete Aufrufe
///   liefert ([`ApprovalDecision::AskUser`]) — fail-closed, **niemals**
///   automatisch freigegeben. Unter [`ApprovalMode::FullAccess`], wo nie
///   gefragt wird, wird daraus eine harte Ablehnung
///   ([`ApprovalDecision::Deny`] mit [`full_access_deny_reason`]).
/// - `Some(`[`RuleDecision::Allow`]`)`: freigegeben, ohne dass die
///   Modus-Logik überhaupt befragt wird.
/// - `None`: keine Regel passt, die bisherige Modus-Logik entscheidet
///   unverändert.
///
/// Ohne ausdrücklich übergebene Regelmenge ([`Self::new`]) trägt die Politik
/// eine leere [`AllowRuleSet`] — bestehende Aufrufer ändern ihr Verhalten
/// damit nicht.
#[derive(Debug)]
pub struct DefaultApprovalPolicy {
    /// Der Freigabemodus dieser Politik; geteilt mit jedem Klon der Zelle.
    mode: ApprovalModeCell,
    /// Geteilte Freigaberegeln (`/permissions` „nicht mehr fragen“, Contract
    /// §2/§4); leer, wenn [`Self::new`] ohne eigene Regelmenge gebaut wurde.
    rules: AllowRuleSet,
    /// Runde 5, Teil E: das Auto-Modus-Gate (Vorfilter + Klassifizierer aus
    /// `harw-runtime`). `None` = bisheriges Verhalten (`auto` fragt bei
    /// allem außerhalb von [`AUTO_APPROVED_TOOLS`]).
    auto_gate: Option<Arc<dyn AutoApprovalGate>>,
    /// Quelle des Remote-OCR-Status (`doc.read_pdf` → Mistral); Standard
    /// [`harw_tool_doc::remote_ocr_target`], in Tests injizierbar.
    remote_ocr: RemoteOcrStatus,
}

impl Default for DefaultApprovalPolicy {
    /// Erzeugt eine Politik mit einer **eigenen**, nicht geteilten Zelle auf
    /// [`ApprovalMode::Delegated`] und einer leeren [`AllowRuleSet`].
    ///
    /// # Beschreibung
    /// Das ist kein stiller Rückfall auf mehr Rechte: `Delegated` ist die
    /// engste Stufe, die harw ohne jede Einstellung fährt (`AlwaysAsk` fragt
    /// mehr, `FullAccess` fragt nichts). Wer den Modus zur Laufzeit umschalten
    /// können muss, darf diesen Konstruktor **nicht** benutzen, sondern
    /// [`DefaultApprovalPolicy::new`] mit der Zelle der Sitzung — eine hier
    /// erzeugte Zelle hat außerhalb dieser Politik keinen Besitzer mehr.
    fn default() -> Self {
        Self::new(ApprovalModeCell::default())
    }
}

impl DefaultApprovalPolicy {
    /// Erzeugt die Politik über der Freigabemodus-Zelle `mode`, mit einer
    /// **leeren** [`AllowRuleSet`].
    ///
    /// # Arguments
    /// - `mode` ([`ApprovalModeCell`]): die Zelle, aus der jeder
    ///   [`ApprovalHandler::review`] den aktuellen Modus liest. Ein Klon
    ///   derselben Zelle beim Aufrufer bleibt der Schalter, mit dem sich der
    ///   Modus der Sitzung umstellen lässt.
    ///
    /// # Returns
    /// Die Politik; sie hält nur einen Zeiger auf die Zelle, keine Kopie des
    /// Modus. Bestehende Aufrufer, die keine Regeln kennen, bleiben
    /// unverändert: eine leere [`AllowRuleSet`] liefert für jeden Aufruf
    /// `None` aus [`AllowRuleSet::evaluate`].
    #[must_use]
    pub fn new(mode: ApprovalModeCell) -> Self {
        Self::with_rules(mode, AllowRuleSet::new())
    }

    /// Erzeugt die Politik über Freigabemodus **und** Freigaberegeln.
    ///
    /// # Arguments
    /// - `mode` ([`ApprovalModeCell`]): siehe [`Self::new`].
    /// - `rules` ([`AllowRuleSet`]): geteilte Regelmenge; ein Klon beim
    ///   Aufrufer (etwa `harw-runtime`, das sie zusätzlich in die
    ///   `ServiceMap` legt) bleibt der Schalter für `/permissions`.
    ///
    /// # Returns
    /// Die Politik; hält nur Zeiger auf beide geteilten Zellen.
    #[must_use]
    pub fn with_rules(mode: ApprovalModeCell, rules: AllowRuleSet) -> Self {
        Self {
            mode,
            rules,
            auto_gate: None,
            remote_ocr: harw_tool_doc::remote_ocr_target,
        }
    }

    /// Ersetzt die Quelle des Remote-OCR-Status (Standard:
    /// [`harw_tool_doc::remote_ocr_target`]).
    ///
    /// # Arguments
    /// - `status` ([`RemoteOcrStatus`]): liefert das aktuelle Remote-OCR-Ziel
    ///   oder `None`, wenn kein Client installiert ist.
    ///
    /// # Returns
    /// Die Politik mit der neuen Quelle.
    #[must_use]
    pub fn with_remote_ocr_status(mut self, status: RemoteOcrStatus) -> Self {
        self.remote_ocr = status;
        self
    }

    /// Runde 5, Teil E: hängt das Auto-Modus-Gate an.
    ///
    /// # Beschreibung
    /// Das Gate wird **nur** im Modus [`ApprovalMode::Delegated`] befragt und
    /// **nur** für Aufrufe, für die diese Politik sonst
    /// [`ApprovalDecision::AskUser`] liefern würde (nicht in
    /// [`AUTO_APPROVED_TOOLS`], nicht in [`ALWAYS_ASK_TOOLS`], keine
    /// passende Regel). Es kann also nie mehr erlauben als „diese eine
    /// Rückfrage entfällt"; `ALWAYS_ASK_TOOLS` erreichen es nie.
    ///
    /// # Arguments
    /// - `gate` (`Arc<dyn AutoApprovalGate>`): das Gate der Sitzung.
    ///
    /// # Returns
    /// Die Politik mit Gate.
    #[must_use]
    pub fn with_auto_gate(mut self, gate: Arc<dyn AutoApprovalGate>) -> Self {
        self.auto_gate = Some(gate);
        self
    }

    /// Returns whether `call` must be explicitly approved before dispatch.
    ///
    /// # Arguments
    /// - `call` (`&ToolCall`): der angefragte Werkzeugaufruf.
    ///
    /// # Returns
    /// `false`, wenn der Name in [`AUTO_APPROVED_TOOLS`] steht, sonst `true`.
    ///
    /// # Beschreibung
    /// Reines Modus-Prädikat, unabhängig von einer [`AllowRuleSet`] — passend
    /// zu seinen Aufrufern (`ApprovalChain`s Rückfrage-Vorhersage), die selbst
    /// keine Regelmenge kennen und darum nur die Modus-Logik nachrechnen.
    #[must_use]
    pub fn requires_explicit_approval(call: &ToolCall) -> bool {
        !AUTO_APPROVED_TOOLS.contains(&call.name.as_str())
    }
}

impl ApprovalHandler for DefaultApprovalPolicy {
    /// Entscheidet zuerst anhand von `FullAccess`, dann anhand von
    /// [`ALWAYS_ASK_TOOLS`], dann anhand der [`AllowRuleSet`], dann anhand
    /// des Freigabemodus in der eigenen [`ApprovalModeCell`].
    ///
    /// # Description
    /// -1. [`ApprovalMode::FullAccess`] → nie eine Rückfrage
    ///    (Nutzerentscheidung 2026-09-24): eine passende `Deny`-Regel wird zu
    ///    [`ApprovalDecision::Deny`] ([`full_access_deny_reason`]), alles
    ///    andere zu [`ApprovalDecision::Allow`] — auch [`ALWAYS_ASK_TOOLS`],
    ///    Remote-OCR und ohne das Auto-Modus-Gate zu befragen.
    /// 0. `call.name` ∈ [`ALWAYS_ASK_TOOLS`] → sofort
    ///    [`ApprovalDecision::AskUser`], ohne Regeln überhaupt zu befragen —
    ///    auch mit einer passenden `Allow`-Regel (Nachtrag K3,
    ///    „Freigabe-Härtung“). Ebenso ein `doc.read_pdf`, das die Datei an
    ///    einen Remote-OCR-Dienst schicken würde, solange
    ///    `[tools.doc].remote_ocr = "ask"` gilt
    ///    ([`remote_ocr_requires_approval`]).
    /// 1. [`AllowRuleSet::evaluate`] auf `call.name`/`call.arguments`:
    ///    - `Some(`[`RuleDecision::Deny`]`)` → [`ApprovalDecision::AskUser`]
    ///      (fail-closed, nie automatisch freigegeben).
    ///    - `Some(`[`RuleDecision::Allow`]`)` → [`ApprovalDecision::Allow`],
    ///      ohne die Modus-Logik zu befragen.
    ///    - `None` → weiter mit Schritt 2.
    /// 2. Modus-Logik (unverändert):
    ///    - [`ApprovalMode::AlwaysAsk`]: jeder Aufruf wird bestätigt, auch ein
    ///      lesender.
    ///    - [`ApprovalMode::Delegated`]: die Voreinstellung —
    ///      [`AUTO_APPROVED_TOOLS`] läuft durch, alles andere fragt. Runde 5,
    ///      Teil E: ist ein Auto-Modus-Gate angehängt
    ///      ([`Self::with_auto_gate`]), entscheidet statt der Rückfrage sein
    ///      Urteil (`allow` → `Allow`, `ask` → `AskUser`, `deny` → `Deny`).
    ///      Runde 6, Teil A1: das Gate liefert `deny` nur noch dort, wo
    ///      niemand gefragt werden kann (Kind ohne Freigabe-Kanal); sonst
    ///      wandelt es selbst in `ask` um — der Grund steht dann im
    ///      Entscheidungsprotokoll unter der Id des Aufrufs.
    ///    - [`ApprovalMode::FullAccess`]: siehe Schritt -1.
    ///
    /// Regeln und Modus werden bei **jedem** Aufruf frisch gelesen, damit eine
    /// Umschaltung sofort greift und nicht erst im nächsten Turn.
    fn review<'a>(&'a self, call: &'a ToolCall) -> ExtFuture<'a, ApprovalDecision> {
        let mode = self.mode.get();
        // Nutzerentscheidung 2026-09-24: `FullAccess` fragt NIE — auch nicht
        // für `ALWAYS_ASK_TOOLS`, Remote-OCR oder über das Auto-Modus-Gate.
        // Einzige Ausnahme ist eine ausdrückliche `/permissions`-Deny-Regel:
        // sie ist eine Anweisung der Nutzerin und wird hier zur harten
        // Ablehnung (nicht zur Rückfrage).
        if mode == ApprovalMode::FullAccess {
            let decision = match self.rules.evaluate(call.name.as_str(), &call.arguments) {
                Some(RuleDecision::Deny) => ApprovalDecision::Deny(full_access_deny_reason(call)),
                Some(RuleDecision::Allow) | None => ApprovalDecision::Allow,
            };
            // Symlink nach außen: unter Full Access ohne Rückfrage freigegeben.
            if matches!(decision, ApprovalDecision::Allow) {
                note_symlink_approval(call);
            }
            return Box::pin(async move { decision });
        }
        // Nachtrag K3, „Freigabe-Härtung“: gewinnt über jede Regel und die
        // Modi `ask`/`auto` — siehe die Begründung bei `ALWAYS_ASK_TOOLS`.
        if ALWAYS_ASK_TOOLS.contains(&call.name.as_str()) {
            return Box::pin(async { ApprovalDecision::AskUser(Default::default()) });
        }
        // `[tools.doc].remote_ocr = "ask"`: die Datei verließe die Maschine.
        // Das ist keine Ausführungsfrage, deshalb fragt es unter `ask`/`auto`
        // trotz Allow-Regel; unter `FullAccess` (oben) fragt es nicht.
        if remote_ocr_requires_approval(call, (self.remote_ocr)().as_ref()) {
            return Box::pin(async { ApprovalDecision::AskUser(Default::default()) });
        }
        // Ein lesendes fs-Werkzeug folgt einem Symlink nach außerhalb des
        // Workspace: fragt unter `ask`/`auto` trotz Allow-Regel (wie ein
        // Lesen außerhalb); die Freigabe gilt danach für das Zielverzeichnis.
        if note_symlink_approval(call) {
            return Box::pin(async { ApprovalDecision::AskUser(Default::default()) });
        }
        let rule_decision = self.rules.evaluate(call.name.as_str(), &call.arguments);
        let requires_approval = match mode {
            ApprovalMode::AlwaysAsk => true,
            ApprovalMode::Delegated => Self::requires_explicit_approval(call),
            ApprovalMode::FullAccess => false,
        };
        // Runde 5, Teil F: `plan.exit`, `plan.enter` und `ask_user` öffnen ein
        // eigenes Fenster; ihre Ausführung IST die Rückfrage an die Nutzerin
        // und wirkt ohne deren Antwort nicht. Eine zusätzliche Freigabe davor
        // wäre eine doppelte Frage — auch unter `AlwaysAsk` (Stufe `plan`).
        // Eine Deny-Regel fragt weiterhin (Regelpfad oben/unten).
        let requires_approval =
            requires_approval && !harw_tool_plan::USER_DIALOG_TOOLS.contains(&call.name.as_str());
        // Runde 5, Teil E: nur `auto`, nur ohne Regeltreffer, nur dort, wo
        // sonst gefragt würde — Deny vor Allow vor Klassifizierer.
        let gate = match (mode, &rule_decision) {
            (ApprovalMode::Delegated, None) if requires_approval => self.auto_gate.clone(),
            _ => None,
        };
        Box::pin(async move {
            match rule_decision {
                // Fail-closed: eine Deny-Regel darf niemals automatisch
                // freigegeben werden (unter `FullAccess` lehnt sie oben hart
                // ab).
                Some(RuleDecision::Deny) => ApprovalDecision::AskUser(Default::default()),
                Some(RuleDecision::Allow) => ApprovalDecision::Allow,
                None if requires_approval => match gate {
                    Some(gate) => gate.decide(call).await.into_approval(),
                    None => ApprovalDecision::AskUser(Default::default()),
                },
                None => ApprovalDecision::Allow,
            }
        })
    }
}

/// Bundles the assembled registry with the discovered project context so
/// callers can report the resolved root back to the user.
pub struct AssembledRegistry {
    /// The fully assembled extension registry ready for `AgentSession::new`.
    pub registry: ExtensionRegistry,
    /// The project context discovered from `cwd`.
    pub project: ProjectContext,
    /// The agent identity used to build `BaselineInstructionsProvider`.
    pub identity: AgentIdentity,
}

/// Assembles the default coding-agent `ExtensionRegistry` rooted at `cwd`.
///
/// # Description
/// Unveränderte Signatur und unverändertes Verhalten: delegiert an
/// [`profile::assemble_registry`] mit [`RegistryProfile::Full`] und leeren
/// [`IdentityOverrides`]. Bestehende Aufrufer (`harw-cli`, `harw-tui`)
/// brauchen keine Anpassung.
///
/// # Arguments
/// - `cwd` (`PathBuf`): Startpunkt der Projekterkennung; Eigentum geht über.
///
/// # Returns
/// `Ok(AssembledRegistry)` mit dem vollen Coding-Werkzeugsatz.
///
/// # Errors
/// Returns a typed error when project discovery fails. Browser tools are not
/// part of this set, with or without the `browser` feature (W5 RD).
///
/// # Examples
/// ```rust,no_run
/// use std::path::PathBuf;
/// use harw_registry_defaults::assemble_default_registry;
///
/// let assembled = assemble_default_registry(PathBuf::from("/workspace"))?;
/// assert_eq!(assembled.identity.role_description, "coding agent");
/// # Ok::<(), harw_registry_defaults::RegistryDefaultsError>(())
/// ```
pub fn assemble_default_registry(cwd: PathBuf) -> RegistryDefaultsResult<AssembledRegistry> {
    assemble_registry(RegistryProfile::Full, cwd, IdentityOverrides::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    fn registered_names(assembled: &AssembledRegistry) -> Vec<String> {
        assembled
            .registry
            .tool_providers()
            .iter()
            .flat_map(|provider| provider.tools())
            .map(|spec| spec.name().to_owned())
            .collect()
    }

    #[test]
    fn assemble_from_current_dir_smoke() -> TestResult {
        let cwd = std::env::current_dir().map_err(ctx("cwd"))?;
        let ar = assemble_default_registry(cwd).map_err(ctx("assemble"))?;
        let total_tools: usize = ar
            .registry
            .tool_providers()
            .iter()
            .map(|p| p.tools().len())
            .sum();
        // Plan R9, Teil F: `job.*` montiert nur eine Montage mit Job-Verdrahtung.
        let declared = RegistryProfile::Full
            .registered_tool_names()
            .into_iter()
            .filter(|name| !crate::profile::JOB_TOOLS.contains(name))
            .count();
        assert_eq!(
            total_tools, declared,
            "das Full-Profil muss genau seine deklarierten Werkzeuge registrieren"
        );
        assert_eq!(ar.registry.instructions_providers().len(), 1);
        assert_eq!(ar.registry.context_providers().len(), 1);
        assert_eq!(ar.registry.approval_handlers().len(), 1);
        Ok(())
    }

    #[test]
    fn assemble_default_registry_still_yields_the_full_coding_tool_set() -> TestResult {
        let cwd = std::env::current_dir().map_err(ctx("cwd"))?;
        let ar = assemble_default_registry(cwd).map_err(ctx("assemble"))?;

        // Ohne Browser — auch unter Feature `browser` (W5 RD: nur mit Grant).
        let expected_tools = vec![
            "fs.read".to_owned(),
            "fs.write".to_owned(),
            "fs.edit".to_owned(),
            "fs.list".to_owned(),
            "fs.search".to_owned(),
            "fs.glob".to_owned(),
            "fs.grep".to_owned(),
            "doc.read_pdf".to_owned(),
            "explore.tree".to_owned(),
            "explore.projects".to_owned(),
            "explore.relations".to_owned(),
            "explore.find".to_owned(),
            "shell.exec".to_owned(),
            // `process.kill` steht in `ALWAYS_ASK_TOOLS`, fragt also immer.
            "process.list".to_owned(),
            "process.kill".to_owned(),
        ];

        let advertised_tools = registered_names(&ar);
        assert_eq!(advertised_tools, expected_tools);
        assert_eq!(ar.identity.tools_available, advertised_tools);
        assert_eq!(ar.identity.role_description, "coding agent");
        assert_eq!(ar.identity.agent_name, "harw");
        assert_eq!(ar.identity.cwd, ar.project.cwd.display().to_string());
        assert_eq!(
            ar.identity.project_root,
            ar.project.project_root.display().to_string()
        );
        assert_eq!(ar.registry.context_providers().len(), 1);
        Ok(())
    }

    fn call(name: &str) -> ToolCall {
        ToolCall {
            id: Default::default(),
            name: harw_extension_api::ToolName::new(name),
            arguments: Default::default(),
        }
    }

    #[test]
    fn default_approval_policy_keeps_declared_read_only_tools_unblocked() {
        for name in AUTO_APPROVED_TOOLS {
            assert!(
                !DefaultApprovalPolicy::requires_explicit_approval(&call(name)),
                "{name} should remain available without approval"
            );
        }
    }

    #[test]
    fn read_only_profile_tools_registered_here_stay_auto_approved() {
        // Fan-out-Schutz: ein Kind mit `allow_pause = false` kann keine
        // Rückfrage beantworten. Gedeckt werden aber nur die Werkzeuge, die
        // **dieses Crate** registriert (`registered_tool_names`) — nie die
        // beworbenen Operationen der Composition-Root. Genau diese Kopplung
        // an `tool_names()` hatte `plan`/`goal` in die Allowlist gezwungen
        // (F-014/G-003).
        for profile in RegistryProfile::ALL.iter().filter(|p| p.is_read_only()) {
            for tool in profile.registered_tool_names() {
                assert!(
                    !DefaultApprovalPolicy::requires_explicit_approval(&call(tool)),
                    "{profile:?}: {tool} fehlt in AUTO_APPROVED_TOOLS"
                );
            }
        }
    }

    /// Lesende Operationen der Composition-Root (`harw-ops`), die
    /// `model_tool(readonly, approval = "none")` deklarieren und keinen
    /// fremden Executor aufrufen (`harw-ops/src/status.rs`, `ps.rs`).
    const READ_ONLY_ROOT_OPERATIONS: &[&str] = &["status", "ps"];

    /// Fan-out-Operationen der Composition-Root für Orchestratoren
    /// (`profile::ORCHESTRATION_TOOLS`, heute `delegate_wave`): nicht
    /// read-only im engeren Sinn, aber ohne eigene Schreibwirkung — jedes
    /// Kind bleibt an seine eigene Rechte- und Freigabegrenze gebunden
    /// (Begründung bei [`AUTO_APPROVED_TOOLS`]).
    const NON_PAUSING_ORCHESTRATION_OPERATIONS: &[&str] = crate::profile::ORCHESTRATION_TOOLS;

    #[test]
    fn auto_approved_tools_are_a_subset_of_the_read_only_surface() {
        // Umkehrung der früheren Deckungsprüfung: nicht „jedes Profil-Werkzeug
        // muss in die Allowlist“, sondern „jeder Allowlist-Eintrag muss
        // nachweislich read-only sein“.
        let mut read_only_surface: Vec<&str> = READ_ONLY_ROOT_OPERATIONS.to_vec();
        read_only_surface.extend_from_slice(NON_PAUSING_ORCHESTRATION_OPERATIONS);
        // Lesende Wissenswerkzeuge (Plan Teil D): von der Composition-Root
        // registriert, nicht über ein Profil — die Rechteklasse jedes Namens
        // ist `ReadWorkspace` (siehe die `TOOL_PERMISSIONS` der Provider).
        read_only_surface
            .extend_from_slice(crate::workbench_tools::WorkbenchReadToolProvider::TOOL_NAMES);
        read_only_surface.extend_from_slice(crate::diary_tools::DiaryToolProvider::TOOL_NAMES);
        read_only_surface.extend_from_slice(crate::palace_tools::PalaceToolProvider::TOOL_NAMES);
        // Runde 5, Teil H: `agent.result` liest nur eigene Kind-Ergebnisse.
        read_only_surface.extend_from_slice(crate::profile::CHILD_RESULT_TOOLS);
        // Runde 5, Teil F: `ask_user` liest nur die Antwort der Nutzerin.
        read_only_surface.push(harw_tool_plan::ASK_USER_TOOL);
        // Runde 5 (Integration): `plan.write` schreibt ausschließlich die
        // Plan-Datei unter `.harw/plans` (kein Workspace-Quellcode) und nur im
        // Plan-Modus — bewusste, dokumentierte Ausnahme (siehe
        // `AUTO_APPROVED_TOOLS`).
        read_only_surface.push(harw_tool_plan::PLAN_WRITE_TOOL);
        // Runde 5, Teil K: `agent.status` liest nur eigene Hintergrund-Läufe.
        read_only_surface.push("agent.status");
        // Runde 5, Teil M: Nachrichten zwischen Elternteil und eigenem Kind
        // (reiner Text, ohne Schreibwirkung).
        read_only_surface.extend_from_slice(crate::profile::CHILD_MESSAGE_TOOLS);
        read_only_surface.extend_from_slice(crate::profile::PARENT_MESSAGE_TOOLS);
        // Plan R9, Teil A: Skill-Katalog lesen (`skills.search`/`skills.load`).
        read_only_surface.extend_from_slice(crate::profile::SKILL_CATALOG_TOOLS);
        // Runde 7, Teil M: Laufstand lesen bzw. Entwurf in den Matrix-Speicher
        // (dokumentierte Ausnahme wie `plan.write`).
        read_only_surface.extend_from_slice(crate::profile::MATRIX_GAME_MASTER_READ_TOOLS);
        // Plan R9, Teil F: lesende Job-Werkzeuge (eigene Jobs, begrenztes
        // Warten), registriert neben `shell.exec` bzw. für Orchestratoren.
        read_only_surface.extend_from_slice(&harw_tool_job::JOB_READ_TOOLS);
        // R18: lesende Gateway-Werkzeuge (nur UIA-Wurzel) und der Bericht der
        // WorkDriver-Worker (schreibt nur den eigenen Berichts-Slot).
        read_only_surface.extend_from_slice(crate::profile::GATEWAY_READ_TOOLS);
        read_only_surface.extend_from_slice(crate::profile::WORK_DRIVER_REPORT_TOOLS);
        for profile in RegistryProfile::ALL.iter().filter(|p| p.is_read_only()) {
            read_only_surface.extend(profile.registered_tool_names());
        }
        for tool in AUTO_APPROVED_TOOLS {
            assert!(
                read_only_surface.contains(tool),
                "{tool} steht in AUTO_APPROVED_TOOLS, gehört aber zu keiner \
                 read-only Oberfläche"
            );
        }
        // Was nur `Full` registriert (fs.write, shell.exec, browser.*), ist
        // per Definition nicht read-only und darf nie auto-freigegeben sein.
        for tool in RegistryProfile::Full.registered_tool_names() {
            if !read_only_surface.contains(&tool) {
                assert!(
                    DefaultApprovalPolicy::requires_explicit_approval(&call(tool)),
                    "{tool} registriert nur Full und darf nicht auto-freigegeben sein"
                );
            }
        }
    }

    /// Plan R9, Teil F: die lesenden Job-Werkzeuge laufen ohne Rückfrage,
    /// `job.start` fragt wie `shell.exec`, `job.stop` fragt (Modus-Logik),
    /// ist aber nie „immer fragen“.
    #[test]
    fn job_read_tools_are_auto_approved_and_start_is_treated_like_shell_exec() {
        for tool in harw_tool_job::JOB_READ_TOOLS {
            assert!(AUTO_APPROVED_TOOLS.contains(&tool), "{tool}");
            assert!(!ALWAYS_ASK_TOOLS.contains(&tool), "{tool}");
            assert!(
                !DefaultApprovalPolicy::requires_explicit_approval(&call(tool)),
                "{tool} muss ohne Rückfrage laufen"
            );
        }
        for tool in [harw_tool_job::JOB_START_TOOL, harw_tool_job::JOB_STOP_TOOL] {
            assert!(!AUTO_APPROVED_TOOLS.contains(&tool), "{tool}");
            assert!(!ALWAYS_ASK_TOOLS.contains(&tool), "{tool}");
            assert_eq!(
                DefaultApprovalPolicy::requires_explicit_approval(&call(tool)),
                DefaultApprovalPolicy::requires_explicit_approval(&call("shell.exec")),
                "{tool}"
            );
        }
    }

    /// Plan D2, Entscheidung der Nutzerin „Kanban nur auf ausdrücklichen
    /// Wunsch“: die lesenden Kanban-Werkzeuge fragen bei jedem Aufruf.
    #[test]
    fn kanban_read_tools_always_require_an_approval() {
        for tool in crate::kanban_tools::KanbanReadToolProvider::TOOL_NAMES {
            assert!(!AUTO_APPROVED_TOOLS.contains(tool), "{tool}");
            assert!(
                DefaultApprovalPolicy::requires_explicit_approval(&call(tool)),
                "{tool} darf nicht auto-freigegeben sein"
            );
        }
    }

    #[test]
    fn tools_with_a_declared_approval_are_never_auto_approved() {
        // Operationen mit `model_tool(approval = "always")`
        // (`harw-ops/src/plan.rs`, `goal.rs`, `stop.rs`) sowie die in W1-05
        // entfernten Einträge mit Nebenwirkung. Die registry-getriebene,
        // vollständige Prüfung liegt in
        // `harw-ops/tests/approval_declaration_gate.rs` (dieses Crate darf
        // `harw-ops` nicht einmal als Dev-Dependency ziehen — Zyklus).
        for name in [
            "plan",
            "goal",
            "stop",
            "explore",
            "research_deps",
            "research_web",
            "analyze",
            "diff",
            "mode",
            // Runde 7, Teil M: Game-Master-Werkzeuge mit Wirkung.
            "matrix.start",
            "matrix.run",
            "matrix.finish",
        ] {
            assert!(
                DefaultApprovalPolicy::requires_explicit_approval(&call(name)),
                "{name} deklariert eine Freigabe bzw. hat Nebenwirkungen und \
                 darf nicht auto-freigegeben sein"
            );
        }
    }

    /// Runde 5, Teil H: `agent.result` ist rein lesend, auto-freigegeben und
    /// nie in der Immer-fragen-Liste.
    #[test]
    fn agent_result_is_auto_approved_and_never_always_ask() {
        for tool in crate::profile::CHILD_RESULT_TOOLS {
            assert!(AUTO_APPROVED_TOOLS.contains(tool), "{tool}");
            assert!(!ALWAYS_ASK_TOOLS.contains(tool), "{tool}");
            assert!(
                !DefaultApprovalPolicy::requires_explicit_approval(&call(tool)),
                "{tool} muss ohne Rückfrage laufen"
            );
        }
    }

    /// Runde 5, Teil K: `agent.status` ist lesend und auto-freigegeben,
    /// `agent.cancel` fragt immer — nie automatisch.
    #[test]
    fn agent_status_is_auto_approved_and_agent_cancel_always_asks() {
        assert!(AUTO_APPROVED_TOOLS.contains(&"agent.status"));
        assert!(!ALWAYS_ASK_TOOLS.contains(&"agent.status"));
        assert!(!DefaultApprovalPolicy::requires_explicit_approval(&call(
            "agent.status"
        )));
        assert!(!AUTO_APPROVED_TOOLS.contains(&"agent.cancel"));
        assert!(ALWAYS_ASK_TOOLS.contains(&"agent.cancel"));
        assert!(DefaultApprovalPolicy::requires_explicit_approval(&call(
            "agent.cancel"
        )));
    }

    /// R18 (D-B, D-E): lesende `gateway.*`-Werkzeuge und `work_driver.report`
    /// laufen ohne Rückfrage; die mutierenden `gateway.*`-Werkzeuge fragen
    /// immer (unter `ask`/`auto` auch gegen eine Allow-Regel).
    #[test]
    fn gateway_reads_and_work_driver_report_are_auto_approved_and_gateway_mutations_always_ask() {
        assert_eq!(crate::profile::GATEWAY_READ_TOOLS.len(), 9);
        assert_eq!(crate::profile::GATEWAY_MUTATION_TOOLS.len(), 5);
        for tool in crate::profile::GATEWAY_READ_TOOLS
            .iter()
            .chain(crate::profile::WORK_DRIVER_REPORT_TOOLS)
        {
            assert!(AUTO_APPROVED_TOOLS.contains(tool), "{tool}");
            assert!(!ALWAYS_ASK_TOOLS.contains(tool), "{tool}");
            assert!(
                !DefaultApprovalPolicy::requires_explicit_approval(&call(tool)),
                "{tool} muss ohne Rückfrage laufen"
            );
        }
        for tool in crate::profile::GATEWAY_MUTATION_TOOLS {
            assert!(!AUTO_APPROVED_TOOLS.contains(tool), "{tool}");
            assert!(ALWAYS_ASK_TOOLS.contains(tool), "{tool}");
            assert!(
                DefaultApprovalPolicy::requires_explicit_approval(&call(tool)),
                "{tool} darf nie auto-freigegeben sein"
            );
        }
    }

    /// Runde 5, Teil M: `agent.message`/`parent.message` brauchen keine
    /// Freigabe, stehen aber nie in `ALWAYS_ASK_TOOLS` und tragen keine
    /// Sandbox-Rechteklasse.
    #[test]
    fn agent_messaging_tools_are_auto_approved_without_permission_class() {
        for tool in crate::profile::CHILD_MESSAGE_TOOLS
            .iter()
            .chain(crate::profile::PARENT_MESSAGE_TOOLS)
        {
            assert!(AUTO_APPROVED_TOOLS.contains(tool), "{tool}");
            assert!(!ALWAYS_ASK_TOOLS.contains(tool), "{tool}");
            assert_eq!(crate::authority::tool_permission(tool), None, "{tool}");
        }
    }

    #[test]
    fn no_builtin_role_advertises_a_tool_outside_its_registered_set() -> TestResult {
        // Ohne `PLANNING_OPERATION_TOOLS` bewirbt keine Rolle mehr ein
        // Werkzeug, das ihre Registry nicht trägt — insbesondere nicht
        // `plan`/`goal` beim Planner, die im Kind nie einen Executor hatten.
        for role in role_names::ALL {
            let profile = profile_for_role(role)
                .ok_or(TestError::Missing("eingebaute Rolle braucht ein Profil"))?;
            assert_eq!(
                profile.tool_names(),
                profile.registered_tool_names(),
                "Rolle {role}: beworbene und registrierte Werkzeuge müssen übereinstimmen"
            );
            // Dieselbe berechnete Bedingung wie
            // `crate::authority::tests::test_authority_reducer_for_role_covers_every_role_and_bounds_its_profile`
            // (dort `exempt_from_subset_bound`), statt einer zweiten,
            // handgepflegten Rollenliste: `executor`, `memory-steward`,
            // `uia-worker`, `uia-writer`, `uia-shell-worker` und
            // `agent-steward` bekommen ihr freigabepflichtiges
            // Werkzeug (`shell.exec`/`fs.write`/die schreibenden
            // Agentendefinitions-Werkzeuge) über ihre feste Profilzuweisung bei
            // der Registry-Montage, nicht über den `AuthorityReducer` — siehe
            // `authority_reducer_for_role`. Für genau diese dokumentierten
            // Ausnahmen existiert der Kind-Freigabepfad: jedes Kind erbt einen
            // `ApprovalActor` aus `Principal::approval_actor`
            // (`harw-runtime/src/spec.rs`), der eine Rückfrage tatsächlich
            // beantworten kann, statt dass sie unbeantwortet hängen bleibt.
            let reducer = crate::authority::authority_reducer_for_role(role).ok_or_else(|| {
                TestError::Unexpected(format!("eingebaute Rolle {role} ohne Reducer"))
            })?;
            let is_documented_gated_exception = !profile
                .required_permissions()
                .is_subset_of(&reducer.ceiling());
            for tool in profile.tool_names() {
                let needs_approval = DefaultApprovalPolicy::requires_explicit_approval(&call(tool));
                if needs_approval && is_documented_gated_exception {
                    // Erwartete Rückfrage einer dokumentierten Ausnahmerolle —
                    // der Kind-Freigabepfad existiert, siehe oben.
                    continue;
                }
                assert!(
                    !needs_approval,
                    "Rolle {role}: {tool} würde im Kind an einer Rückfrage hängen"
                );
            }
        }
        Ok(())
    }

    // `review` liefert ein `ExtFuture` (`Pin<Box<dyn Future>>`), aber diese
    // Crate zieht keinen Async-Runtime als Dev-Dependency; die Zukunft hat
    // ohnehin keinen echten `.await`-Punkt und wird beim ersten `poll` fertig.
    // `Waker::noop` genügt darum, um sie synchron im Test auszulesen — die
    // Crate verbietet `unsafe`, ein handgebauter RawWaker wäre hier ohnehin
    // nicht erlaubt.
    fn block_on<T>(mut future: harw_extension_api::ExtFuture<'_, T>) -> TestResult<T> {
        use std::task::{Context, Poll, Waker};

        let mut cx = Context::from_waker(Waker::noop());
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(value) => Ok(value),
            Poll::Pending => Err(TestError::Unexpected(
                "review() future did not complete on first poll".to_owned(),
            )),
        }
    }

    // Der Freigabemodus lebt seit G-009 in einer `ApprovalModeCell` statt in
    // einem prozessweiten Static. Jeder der folgenden Tests baut sich deshalb
    // seine eigene Zelle: Sie teilen keinen Zustand, brauchen keine
    // Wiederherstellung am Ende und können in beliebiger Reihenfolge parallel
    // laufen.

    /// Runde 5, Teil F: die Dialog-Werkzeuge (`plan.exit`, `plan.enter`,
    /// `ask_user`) fragen in keinem Modus zusätzlich — ihr eigenes Fenster
    /// ist die Rückfrage. `plan.write` ist unter `Delegated` auto-freigegeben
    /// (nur `.harw/plans`), unter `AlwaysAsk` fragt es; eine Deny-Regel fragt
    /// auch bei Dialog-Werkzeugen.
    #[test]
    fn user_dialog_tools_never_ask_twice_but_plan_write_does() -> TestResult {
        use harw_extension_api::allow_rules::{ApprovalRule, RuleDecision, RuleScope};

        for mode in [
            ApprovalMode::AlwaysAsk,
            ApprovalMode::Delegated,
            ApprovalMode::FullAccess,
        ] {
            let policy = DefaultApprovalPolicy::new(ApprovalModeCell::new(mode));
            for tool in harw_tool_plan::USER_DIALOG_TOOLS {
                assert!(
                    matches!(
                        block_on(policy.review(&call(tool)))?,
                        ApprovalDecision::Allow
                    ),
                    "{tool} unter {mode:?}"
                );
            }
        }
        assert!(AUTO_APPROVED_TOOLS.contains(&harw_tool_plan::PLAN_WRITE_TOOL));
        assert!(AUTO_APPROVED_TOOLS.contains(&harw_tool_plan::ASK_USER_TOOL));
        assert!(!harw_tool_plan::USER_DIALOG_TOOLS.contains(&harw_tool_plan::PLAN_WRITE_TOOL));
        // `plan.write` läuft in der Plan-Stufe (`Delegated`) ohne Rückfrage …
        let policy = DefaultApprovalPolicy::new(ApprovalModeCell::new(ApprovalMode::Delegated));
        assert!(matches!(
            block_on(policy.review(&call(harw_tool_plan::PLAN_WRITE_TOOL)))?,
            ApprovalDecision::Allow
        ));
        // … fragt unter `AlwaysAsk` aber weiterhin.
        let policy = DefaultApprovalPolicy::new(ApprovalModeCell::new(ApprovalMode::AlwaysAsk));
        assert!(matches!(
            block_on(policy.review(&call(harw_tool_plan::PLAN_WRITE_TOOL)))?,
            ApprovalDecision::AskUser(_)
        ));

        let rules = AllowRuleSet::new();
        rules.add(ApprovalRule {
            tool: harw_tool_plan::ASK_USER_TOOL.to_owned(),
            pattern: None,
            decision: RuleDecision::Deny,
            scope: RuleScope::Session,
        });
        let policy = DefaultApprovalPolicy::with_rules(
            ApprovalModeCell::new(ApprovalMode::Delegated),
            rules.clone(),
        );
        assert!(matches!(
            block_on(policy.review(&call(harw_tool_plan::ASK_USER_TOOL)))?,
            ApprovalDecision::AskUser(_)
        ));
        // Unter `FullAccess` fragt nichts: die Deny-Regel lehnt hart ab.
        let policy = DefaultApprovalPolicy::with_rules(
            ApprovalModeCell::new(ApprovalMode::FullAccess),
            rules,
        );
        assert!(matches!(
            block_on(policy.review(&call(harw_tool_plan::ASK_USER_TOOL)))?,
            ApprovalDecision::Deny(_)
        ));
        Ok(())
    }

    fn remote_ocr_ask() -> Option<RemoteOcrTarget> {
        Some(RemoteOcrTarget {
            host: "api.mistral.ai".to_owned(),
            approval: harw_tool_doc::RemoteOcrApproval::Ask,
        })
    }

    fn remote_ocr_auto() -> Option<RemoteOcrTarget> {
        Some(RemoteOcrTarget {
            host: "api.mistral.ai".to_owned(),
            approval: harw_tool_doc::RemoteOcrApproval::Auto,
        })
    }

    fn read_pdf(arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            id: Default::default(),
            name: harw_extension_api::ToolName::new(REMOTE_OCR_TOOL),
            arguments,
        }
    }

    /// `[tools.doc].remote_ocr = "ask"`: `doc.read_pdf` fragt unter `ask`
    /// und `auto`, sobald die Datei an den Remote-Dienst ginge — auch trotz
    /// Allow-Regel. `backend: "native"`, Modus `on` und „kein Client
    /// installiert“ bleiben ohne Rückfrage. `FullAccess` fragt nie (siehe
    /// `review_allows_remote_ocr_under_full_access`).
    /// Symlink nach außerhalb des Workspace: `auto` fragt, Full Access fragt
    /// nicht, ein gemerktes Verzeichnis fragt nicht noch einmal.
    #[test]
    fn review_asks_for_symlinks_leaving_the_workspace_and_remembers_the_directory() -> TestResult {
        use std::os::unix::fs::symlink;

        let dir = tempfile::TempDir::new().map_err(ctx("tempdir"))?;
        let base = dir.path().canonicalize().map_err(ctx("canonicalize"))?;
        let ws = base.join("ws");
        for sub in ["a", "b"] {
            let target = base.join("outside").join(sub);
            std::fs::create_dir_all(&target).map_err(ctx("mkdir"))?;
            std::fs::write(target.join("f.txt"), "x").map_err(ctx("write"))?;
        }
        std::fs::create_dir_all(&ws).map_err(ctx("mkdir ws"))?;
        symlink(base.join("outside/a"), ws.join("share_a")).map_err(ctx("symlink a"))?;
        symlink(base.join("outside/b"), ws.join("share_b")).map_err(ctx("symlink b"))?;
        let access = harw_tool_fs::symlink::global();
        access.register_root(&ws);
        let fs_read = |path: &str| ToolCall {
            id: Default::default(),
            name: harw_extension_api::ToolName::new("fs.read"),
            arguments: serde_json::json!({ "path": path }),
        };

        // `auto`: fragt, und der Dialog nennt Link und Ziel.
        let auto = DefaultApprovalPolicy::new(ApprovalModeCell::new(ApprovalMode::Delegated));
        let call_a = fs_read("share_a/f.txt");
        assert!(call_needs_symlink_approval(&call_a));
        let notice = symlink_approval_notice(&call_a).ok_or(TestError::Missing("notice"))?;
        assert!(
            notice.contains("Symlink zeigt außerhalb des Arbeitsbereichs")
                && notice.contains(&base.join("outside/a").display().to_string()),
            "{notice}"
        );
        assert!(matches!(
            block_on(auto.review(&call_a))?,
            ApprovalDecision::AskUser(_)
        ));
        // Innerhalb des Workspace fragt `fs.read` unter `auto` nicht.
        assert!(matches!(
            block_on(auto.review(&fs_read("share_a_missing")))?,
            ApprovalDecision::Allow
        ));

        // Full Access: keine Rückfrage, die Freigabe ist für den Aufruf vermerkt.
        let full = DefaultApprovalPolicy::new(ApprovalModeCell::new(ApprovalMode::FullAccess));
        let call_b = fs_read("share_b/f.txt");
        assert!(matches!(
            block_on(full.review(&call_b))?,
            ApprovalDecision::Allow
        ));
        let target_b = access
            .review_call("fs.read", &call_b.arguments)
            .ok_or(TestError::Missing("share_b target"))?;
        assert!(access.allows(&target_b, "fs.read", "share_b/f.txt"));

        // Nach der Ausführung des freigegebenen Aufrufs (das Werkzeug verbraucht
        // die Freigabe) ist das Verzeichnis gemerkt: keine zweite Frage.
        let target_a = access
            .review_call("fs.read", &call_a.arguments)
            .ok_or(TestError::Missing("share_a target"))?;
        assert!(access.allows(&target_a, "fs.read", "share_a/f.txt"));
        assert!(!call_needs_symlink_approval(&call_a));
        assert!(matches!(
            block_on(auto.review(&fs_read("share_a")))?,
            ApprovalDecision::Allow
        ));
        Ok(())
    }

    #[test]
    fn review_asks_for_remote_ocr_in_ask_mode_outside_full_access() -> TestResult {
        use harw_extension_api::allow_rules::{ApprovalRule, RuleScope};
        use serde_json::json;

        let remote = read_pdf(json!({ "path": "scan.pdf" }));
        let native = read_pdf(json!({ "path": "scan.pdf", "backend": "native" }));
        let mode = ApprovalMode::Delegated;
        let rules = AllowRuleSet::new();
        rules.add(ApprovalRule {
            tool: REMOTE_OCR_TOOL.to_owned(),
            pattern: None,
            decision: RuleDecision::Allow,
            scope: RuleScope::Session,
        });
        let asking = DefaultApprovalPolicy::with_rules(ApprovalModeCell::new(mode), rules)
            .with_remote_ocr_status(remote_ocr_ask);
        assert!(
            matches!(
                block_on(asking.review(&remote))?,
                ApprovalDecision::AskUser(_)
            ),
            "remote OCR unter {mode:?} muss fragen"
        );
        assert!(
            matches!(block_on(asking.review(&native))?, ApprovalDecision::Allow),
            "backend native unter {mode:?} bleibt ohne Rückfrage"
        );

        let auto = DefaultApprovalPolicy::new(ApprovalModeCell::new(mode))
            .with_remote_ocr_status(remote_ocr_auto);
        assert!(matches!(
            block_on(auto.review(&remote))?,
            ApprovalDecision::Allow
        ));

        let none =
            DefaultApprovalPolicy::new(ApprovalModeCell::new(mode)).with_remote_ocr_status(|| None);
        assert!(matches!(
            block_on(none.review(&remote))?,
            ApprovalDecision::Allow
        ));
        // Weiterhin auto-freigegeben: die Rückfrage hängt nur am Remote-Pfad.
        assert!(AUTO_APPROVED_TOOLS.contains(&REMOTE_OCR_TOOL));
        Ok(())
    }

    /// Nutzerentscheidung 2026-09-24: unter `FullAccess` fragt auch der
    /// Remote-OCR-Versand unter `[tools.doc].remote_ocr = "ask"` nicht —
    /// mit und ohne Allow-Regel.
    #[test]
    fn review_allows_remote_ocr_under_full_access() -> TestResult {
        use harw_extension_api::allow_rules::{ApprovalRule, RuleScope};
        use serde_json::json;

        let remote = read_pdf(json!({ "path": "scan.pdf" }));
        let plain = DefaultApprovalPolicy::new(ApprovalModeCell::new(ApprovalMode::FullAccess))
            .with_remote_ocr_status(remote_ocr_ask);
        assert!(matches!(
            block_on(plain.review(&remote))?,
            ApprovalDecision::Allow
        ));

        let rules = AllowRuleSet::new();
        rules.add(ApprovalRule {
            tool: REMOTE_OCR_TOOL.to_owned(),
            pattern: None,
            decision: RuleDecision::Allow,
            scope: RuleScope::Session,
        });
        let with_rule = DefaultApprovalPolicy::with_rules(
            ApprovalModeCell::new(ApprovalMode::FullAccess),
            rules,
        )
        .with_remote_ocr_status(remote_ocr_ask);
        assert!(matches!(
            block_on(with_rule.review(&remote))?,
            ApprovalDecision::Allow
        ));
        Ok(())
    }

    #[test]
    fn remote_ocr_notice_names_host_only_when_asking() {
        use serde_json::json;

        let remote = read_pdf(json!({ "path": "scan.pdf" }));
        let native = read_pdf(json!({ "path": "scan.pdf", "backend": "native" }));
        let notice = remote_ocr_approval_notice_with(&remote, remote_ocr_ask().as_ref());
        assert!(
            notice
                .as_deref()
                .is_some_and(|text| text.contains("api.mistral.ai") && text.contains("native")),
            "{notice:?}"
        );
        assert_eq!(
            remote_ocr_approval_notice_with(&native, remote_ocr_ask().as_ref()),
            None
        );
        assert_eq!(
            remote_ocr_approval_notice_with(&remote, remote_ocr_auto().as_ref()),
            None
        );
        assert_eq!(
            remote_ocr_approval_notice_with(&call("fs.read"), remote_ocr_ask().as_ref()),
            None
        );
    }

    #[test]
    fn review_in_always_ask_mode_asks_even_for_read_only_tools() -> TestResult {
        let policy = DefaultApprovalPolicy::new(ApprovalModeCell::new(ApprovalMode::AlwaysAsk));

        assert!(matches!(
            block_on(policy.review(&call("fs.read")))?,
            ApprovalDecision::AskUser(_)
        ));
        assert!(matches!(
            block_on(policy.review(&call("shell.exec")))?,
            ApprovalDecision::AskUser(_)
        ));
        Ok(())
    }

    #[test]
    fn review_in_delegated_mode_allows_only_the_allowlist() -> TestResult {
        let policy = DefaultApprovalPolicy::new(ApprovalModeCell::new(ApprovalMode::Delegated));

        assert!(matches!(
            block_on(policy.review(&call("fs.read")))?,
            ApprovalDecision::Allow
        ));
        assert!(matches!(
            block_on(policy.review(&call("shell.exec")))?,
            ApprovalDecision::AskUser(_)
        ));
        Ok(())
    }

    #[test]
    fn review_in_full_access_mode_allows_everything() -> TestResult {
        let policy = DefaultApprovalPolicy::new(ApprovalModeCell::new(ApprovalMode::FullAccess));

        assert!(matches!(
            block_on(policy.review(&call("fs.read")))?,
            ApprovalDecision::Allow
        ));
        assert!(matches!(
            block_on(policy.review(&call("shell.exec")))?,
            ApprovalDecision::Allow
        ));
        Ok(())
    }

    /// Die Zusage „der Modus wird bei jedem Aufruf frisch gelesen“: Ein
    /// `set` auf einem Klon der Zelle wirkt auf die bereits gebaute Politik,
    /// ohne dass sie neu montiert werden müsste.
    #[test]
    fn review_reads_the_mode_cell_on_every_call() -> TestResult {
        let cell = ApprovalModeCell::new(ApprovalMode::Delegated);
        let policy = DefaultApprovalPolicy::new(cell.clone());
        let mutating = call("shell.exec");

        assert!(matches!(
            block_on(policy.review(&mutating))?,
            ApprovalDecision::AskUser(_)
        ));

        cell.set(ApprovalMode::FullAccess);
        assert!(
            matches!(block_on(policy.review(&mutating))?, ApprovalDecision::Allow),
            "die Umschaltung muss sofort wirken, nicht erst im nächsten Turn"
        );

        cell.set(ApprovalMode::AlwaysAsk);
        assert!(matches!(
            block_on(policy.review(&call("fs.read")))?,
            ApprovalDecision::AskUser(_)
        ));
        Ok(())
    }

    /// Eine Politik mit eigener Zelle darf von der Zelle einer anderen nichts
    /// mitbekommen — der eigentliche Grund, warum der prozessweite Schalter
    /// weg musste (G-009): Ein Kind im Modus `FullAccess` hätte sonst den
    /// Root-Modus mitverändert und umgekehrt.
    #[test]
    fn two_policies_with_separate_cells_do_not_influence_each_other() -> TestResult {
        let root_cell = ApprovalModeCell::new(ApprovalMode::Delegated);
        let child_cell = ApprovalModeCell::new(ApprovalMode::Delegated);
        let root = DefaultApprovalPolicy::new(root_cell.clone());
        let child = DefaultApprovalPolicy::new(child_cell);

        root_cell.set(ApprovalMode::FullAccess);

        assert!(matches!(
            block_on(root.review(&call("shell.exec")))?,
            ApprovalDecision::Allow
        ));
        assert!(matches!(
            block_on(child.review(&call("shell.exec")))?,
            ApprovalDecision::AskUser(_)
        ));
        Ok(())
    }

    /// Die zusammengebaute Registry muss genau die übergebene Zelle tragen —
    /// sonst wäre das Durchreichen durch [`assemble_registry_for_project`]
    /// wirkungslos.
    #[test]
    fn assembled_registry_uses_the_approval_mode_cell_it_was_given() -> TestResult {
        use harw_project_discovery::{DiscoveryConfig, discover_project};

        let cwd = std::env::current_dir().map_err(ctx("cwd"))?;
        let project = discover_project(&cwd, &DiscoveryConfig::default())
            .map_err(ctx("Discovery im Workspace"))?;

        let cell = ApprovalModeCell::new(ApprovalMode::AlwaysAsk);
        let assembled = assemble_registry_for_project(
            RegistryProfile::ReadOnlyExplore,
            &project,
            IdentityOverrides::default(),
            cell.clone(),
        )
        .map_err(ctx("assemble"))?;

        let handlers = assembled.registry.approval_handlers();
        assert_eq!(handlers.len(), 1);
        let handler = &handlers[0];

        assert!(
            matches!(
                block_on(handler.review(&call("fs.read")))?,
                ApprovalDecision::AskUser(_)
            ),
            "die Registry muss den Modus der übergebenen Zelle sehen"
        );

        cell.set(ApprovalMode::FullAccess);
        assert!(
            matches!(
                block_on(handler.review(&call("fs.read")))?,
                ApprovalDecision::Allow
            ),
            "ein `set` auf der übergebenen Zelle muss die montierte Registry erreichen"
        );
        Ok(())
    }

    /// Eine passende `Allow`-Regel gibt frei, ohne dass die Modus-Logik
    /// überhaupt gefragt würde — selbst wenn der Modus `AlwaysAsk` wäre.
    #[test]
    fn review_allows_a_call_matching_an_allow_rule_without_asking() -> TestResult {
        use harw_extension_api::allow_rules::{ApprovalRule, RuleDecision, RuleScope};

        let rules = AllowRuleSet::new();
        rules.add(ApprovalRule {
            tool: "shell.exec".to_owned(),
            pattern: Some("git status".to_owned()),
            decision: RuleDecision::Allow,
            scope: RuleScope::Project,
        });
        let policy = DefaultApprovalPolicy::with_rules(
            ApprovalModeCell::new(ApprovalMode::AlwaysAsk),
            rules,
        );

        let mut call = call("shell.exec");
        call.arguments = serde_json::json!({"command": "git status --short"});

        assert!(matches!(
            block_on(policy.review(&call))?,
            ApprovalDecision::Allow
        ));
        Ok(())
    }

    /// Eine `Deny`-Regel gewinnt über eine passende `Allow`-Regel und über
    /// den Modus `FullAccess` — beides würde ohne Regel automatisch
    /// freigeben. Unter `FullAccess` (fragt nie) wird sie zur harten
    /// Ablehnung, unter `auto` zur Rückfrage.
    #[test]
    fn review_deny_rule_beats_allow_rule_and_full_access_mode() -> TestResult {
        use harw_extension_api::allow_rules::{ApprovalRule, RuleDecision, RuleScope};

        let rules = AllowRuleSet::new();
        rules.add(ApprovalRule {
            tool: "shell.exec".to_owned(),
            pattern: Some("git push".to_owned()),
            decision: RuleDecision::Allow,
            scope: RuleScope::Global,
        });
        rules.add(ApprovalRule {
            tool: "shell.exec".to_owned(),
            pattern: Some("git push".to_owned()),
            decision: RuleDecision::Deny,
            scope: RuleScope::Session,
        });
        let cell = ApprovalModeCell::new(ApprovalMode::FullAccess);
        let policy = DefaultApprovalPolicy::with_rules(cell.clone(), rules);

        let mut call = call("shell.exec");
        call.arguments = serde_json::json!({"command": "git push origin main"});

        match block_on(policy.review(&call))? {
            ApprovalDecision::Deny(reason) => {
                assert_eq!(reason, full_access_deny_reason(&call));
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "Deny-Regel unter FullAccess muss hart ablehnen, war {other:?}"
                )));
            }
        }

        cell.set(ApprovalMode::Delegated);
        assert!(matches!(
            block_on(policy.review(&call))?,
            ApprovalDecision::AskUser(_)
        ));
        Ok(())
    }

    /// Nachtrag K3, „Freigabe-Härtung“: `ALWAYS_ASK_TOOLS` fragen unter
    /// `ask` und `auto` immer nach — selbst mit einer passenden
    /// `Allow`-Regel, die für jedes andere Werkzeug automatisch freigeben
    /// würde.
    #[test]
    fn review_always_asks_for_always_ask_tools_outside_full_access_even_with_an_allow_rule()
    -> TestResult {
        use harw_extension_api::allow_rules::{ApprovalRule, RuleDecision, RuleScope};

        for mode in [ApprovalMode::AlwaysAsk, ApprovalMode::Delegated] {
            for tool in ALWAYS_ASK_TOOLS {
                let rules = AllowRuleSet::new();
                rules.add(ApprovalRule {
                    tool: (*tool).to_owned(),
                    pattern: None,
                    decision: RuleDecision::Allow,
                    scope: RuleScope::Global,
                });
                let policy = DefaultApprovalPolicy::with_rules(ApprovalModeCell::new(mode), rules);

                assert!(
                    matches!(
                        block_on(policy.review(&call(tool)))?,
                        ApprovalDecision::AskUser(_)
                    ),
                    "{tool} muss unter {mode:?} trotz Allow-Regel nachfragen"
                );
            }
        }
        Ok(())
    }

    /// Nutzerentscheidung 2026-09-24: unter `FullAccess` fragt harw nie —
    /// jedes `ALWAYS_ASK_TOOLS`-Werkzeug (Kill, sudo, Agenten-Commits,
    /// `agent.cancel`) wird ohne Rückfrage freigegeben, mit und ohne
    /// Allow-Regel.
    #[test]
    fn review_allows_every_always_ask_tool_under_full_access() -> TestResult {
        use harw_extension_api::allow_rules::{ApprovalRule, RuleDecision, RuleScope};

        for tool in ALWAYS_ASK_TOOLS {
            let plain = DefaultApprovalPolicy::new(ApprovalModeCell::new(ApprovalMode::FullAccess));
            assert!(
                matches!(
                    block_on(plain.review(&call(tool)))?,
                    ApprovalDecision::Allow
                ),
                "{tool} muss unter FullAccess ohne Rückfrage laufen"
            );

            let rules = AllowRuleSet::new();
            rules.add(ApprovalRule {
                tool: (*tool).to_owned(),
                pattern: None,
                decision: RuleDecision::Allow,
                scope: RuleScope::Global,
            });
            let with_rule = DefaultApprovalPolicy::with_rules(
                ApprovalModeCell::new(ApprovalMode::FullAccess),
                rules,
            );
            assert!(
                matches!(
                    block_on(with_rule.review(&call(tool)))?,
                    ApprovalDecision::Allow
                ),
                "{tool} muss unter FullAccess mit Allow-Regel ohne Rückfrage laufen"
            );
        }
        Ok(())
    }

    /// Ohne passende Regel bleibt die bisherige Modus-Logik unverändert in
    /// Kraft — `AllowRuleSet::new()` (leer) ändert nichts am Verhalten von
    /// [`DefaultApprovalPolicy::new`].
    #[test]
    fn review_without_a_matching_rule_falls_back_to_mode_logic() -> TestResult {
        let policy = DefaultApprovalPolicy::new(ApprovalModeCell::new(ApprovalMode::Delegated));

        assert!(matches!(
            block_on(policy.review(&call("fs.read")))?,
            ApprovalDecision::Allow
        ));
        assert!(matches!(
            block_on(policy.review(&call("shell.exec")))?,
            ApprovalDecision::AskUser(_)
        ));
        Ok(())
    }

    #[test]
    fn default_approval_policy_requires_approval_for_mutating_removed_and_unknown_tools() {
        for name in [
            "fs.write",
            "fs.edit",
            "shell.exec",
            "stop",
            "memory",
            "plugins",
            "skills",
            "future.tool",
        ] {
            assert!(
                DefaultApprovalPolicy::requires_explicit_approval(&call(name)),
                "{name} must not receive automatic approval"
            );
        }
    }

    // ── Runde 5, Teil E: Auto-Modus-Gate ─────────────────────────────────────

    /// Ein Gate-Doppel mit festem Urteil, das seine Befragungen zählt.
    #[derive(Debug)]
    struct FixedGate {
        verdict: harw_extension_api::auto_mode::AutoVerdict,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl FixedGate {
        fn new(decision: harw_extension_api::auto_mode::AutoDecision) -> Arc<Self> {
            Self::with_verdict(harw_extension_api::auto_mode::AutoVerdict::new(
                decision,
                "test",
                "fester Testgrund",
                harw_extension_api::auto_mode::VerdictSource::Classifier,
            ))
        }

        fn with_verdict(verdict: harw_extension_api::auto_mode::AutoVerdict) -> Arc<Self> {
            Arc::new(Self {
                verdict,
                calls: std::sync::atomic::AtomicUsize::new(0),
            })
        }

        fn calls(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl AutoApprovalGate for FixedGate {
        fn decide<'a>(
            &'a self,
            _call: &'a ToolCall,
        ) -> ExtFuture<'a, harw_extension_api::auto_mode::AutoVerdict> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let verdict = self.verdict.clone();
            Box::pin(async move { verdict })
        }
    }

    fn gated_policy(
        mode: ApprovalMode,
        rules: AllowRuleSet,
        gate: &Arc<FixedGate>,
    ) -> DefaultApprovalPolicy {
        DefaultApprovalPolicy::with_rules(ApprovalModeCell::new(mode), rules)
            .with_auto_gate(Arc::clone(gate) as Arc<dyn AutoApprovalGate>)
    }

    #[test]
    fn auto_gate_allow_skips_the_question_in_delegated_mode() -> TestResult {
        use harw_extension_api::auto_mode::AutoDecision;
        let gate = FixedGate::new(AutoDecision::Allow);
        let policy = gated_policy(ApprovalMode::Delegated, AllowRuleSet::new(), &gate);

        assert!(matches!(
            block_on(policy.review(&call("shell.exec")))?,
            ApprovalDecision::Allow
        ));
        assert_eq!(gate.calls(), 1);
        Ok(())
    }

    /// Runde 6, Teil A1: ein `deny`, das nach der Umwandlung im Gate übrig
    /// bleibt (niemand kann gefragt werden: Kind ohne Kanal, Nicht-TUI),
    /// wird zum harten `Deny` mit Grund.
    #[test]
    fn auto_gate_final_deny_stays_a_deny_with_reason() -> TestResult {
        use harw_extension_api::auto_mode::{AUTO_DENIAL_PREFIX, AutoDecision};
        let gate = FixedGate::new(AutoDecision::Deny);
        let policy = gated_policy(ApprovalMode::Delegated, AllowRuleSet::new(), &gate);

        match block_on(policy.review(&call("fs.write")))? {
            ApprovalDecision::Deny(reason) => {
                assert!(reason.starts_with(AUTO_DENIAL_PREFIX), "{reason}");
                Ok(())
            }
            other => Err(TestError::Unexpected(format!(
                "erwartet Deny, war {other:?}"
            ))),
        }
    }

    /// Runde 6, Teil A1: ein vom Gate aus `deny` umgewandeltes `ask` (Wurzel
    /// bzw. Kind mit Kanal) wird zur Rückfrage, nicht zur Ablehnung.
    #[test]
    fn auto_gate_escalated_deny_becomes_a_question() -> TestResult {
        use harw_extension_api::auto_mode::{AutoDecision, AutoVerdict, VerdictSource};
        let gate = FixedGate::with_verdict(
            AutoVerdict::new(
                AutoDecision::Deny,
                "exfiltration",
                "verschiebt nach ~",
                VerdictSource::Classifier,
            )
            .escalate_to_ask(),
        );
        let policy = gated_policy(ApprovalMode::Delegated, AllowRuleSet::new(), &gate);

        assert!(matches!(
            block_on(policy.review(&call("shell.exec")))?,
            ApprovalDecision::AskUser(_)
        ));
        assert_eq!(gate.calls(), 1);
        Ok(())
    }

    /// Harte Regel: `ALWAYS_ASK_TOOLS` erreichen das Gate nie — auch nicht,
    /// wenn es `allow` sagen würde.
    #[test]
    fn auto_gate_never_sees_always_ask_tools() -> TestResult {
        use harw_extension_api::auto_mode::AutoDecision;
        let gate = FixedGate::new(AutoDecision::Allow);
        let policy = gated_policy(ApprovalMode::Delegated, AllowRuleSet::new(), &gate);

        for tool in ALWAYS_ASK_TOOLS {
            assert!(
                matches!(
                    block_on(policy.review(&call(tool)))?,
                    ApprovalDecision::AskUser(_)
                ),
                "{tool} muss trotz Auto-Gate fragen"
            );
        }
        assert_eq!(gate.calls(), 0);
        Ok(())
    }

    /// Nutzerentscheidung 2026-09-24: unter `FullAccess` wird der
    /// Auto-Modus-Klassifizierer (Vorfilter + Modell) nie befragt — auch ein
    /// Gate, das ablehnen würde (etwa `privilege-escalation` für `sudo`),
    /// kann dort keine Rückfrage erzwingen.
    #[test]
    fn auto_gate_is_never_consulted_under_full_access() -> TestResult {
        use harw_extension_api::auto_mode::{AutoDecision, AutoVerdict, VerdictSource};

        for verdict in [
            AutoVerdict::new(
                AutoDecision::Ask,
                "privilege-escalation",
                "`sudo` erhöht Rechte",
                VerdictSource::Prefilter,
            ),
            AutoVerdict::new(
                AutoDecision::Deny,
                "exfiltration",
                "verschiebt nach ~",
                VerdictSource::Classifier,
            ),
        ] {
            let gate = FixedGate::with_verdict(verdict);
            let policy = gated_policy(ApprovalMode::FullAccess, AllowRuleSet::new(), &gate);
            let mut sudo = call("shell.exec");
            sudo.arguments = serde_json::json!({ "command": "sudo apt-get install ripgrep" });
            for probe in [sudo, call("fs.write"), call("process.kill")] {
                assert!(
                    matches!(block_on(policy.review(&probe))?, ApprovalDecision::Allow),
                    "{} muss unter FullAccess ohne Klassifizierer freigegeben werden",
                    probe.name.as_str()
                );
            }
            assert_eq!(gate.calls(), 0, "FullAccess befragt den Klassifizierer nie");
        }
        Ok(())
    }

    /// Das Gate wird nur in `auto` befragt, nie für bereits freigegebene
    /// Lesewerkzeuge und nie, wenn eine Regel trifft (Deny vor Allow vor
    /// Klassifizierer).
    #[test]
    fn auto_gate_is_consulted_only_where_the_policy_would_ask() -> TestResult {
        use harw_extension_api::allow_rules::{ApprovalRule, RuleScope};
        use harw_extension_api::auto_mode::AutoDecision;

        let gate = FixedGate::new(AutoDecision::Allow);
        for mode in [ApprovalMode::AlwaysAsk, ApprovalMode::FullAccess] {
            let policy = gated_policy(mode, AllowRuleSet::new(), &gate);
            let _ = block_on(policy.review(&call("shell.exec")))?;
        }
        let delegated = gated_policy(ApprovalMode::Delegated, AllowRuleSet::new(), &gate);
        assert!(matches!(
            block_on(delegated.review(&call("fs.read")))?,
            ApprovalDecision::Allow
        ));
        assert_eq!(gate.calls(), 0, "weder ask/full noch Lesewerkzeuge");

        let rules = AllowRuleSet::new();
        rules.add(ApprovalRule {
            tool: "shell.exec".to_owned(),
            pattern: None,
            decision: RuleDecision::Deny,
            scope: RuleScope::Session,
        });
        let with_deny = gated_policy(ApprovalMode::Delegated, rules, &gate);
        assert!(
            matches!(
                block_on(with_deny.review(&call("shell.exec")))?,
                ApprovalDecision::AskUser(_)
            ),
            "Deny-Regel schlägt ein Gate-allow"
        );
        assert_eq!(gate.calls(), 0);
        Ok(())
    }
}
