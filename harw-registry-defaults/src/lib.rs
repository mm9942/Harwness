//! `harw-registry-defaults` — Shared coding-agent ExtensionRegistry assembly.
//!
//! Used by both `harw-cli` and `harw-tui` so the runtime picture is identical
//! regardless of whether the user runs one-shot commands, the interactive TUI,
//! or the local echo path.
//!
//! # Responsibility
//! Bundles the extension crates (fs, shell, deps, web, instructions,
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
//! Kind-Agenten. Ein read-only Profil sieht `fs.write` und `shell.exec` nicht
//! einmal im Inventar — die Beschränkung ist keine Prompt-Bitte, sondern ein
//! Filter über dem Provider (siehe [`profile::RestrictedToolProvider`]).
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

pub mod authority;
pub mod embedded_agents;
pub mod profile;
pub mod research_web;

use std::path::PathBuf;

use harw_extension_api::allow_rules::{AllowRuleSet, RuleDecision};
use harw_extension_api::approval_mode::ApprovalModeCell;
use harw_extension_api::{
    ApprovalDecision, ApprovalHandler, ApprovalMode, ExtFuture, ExtensionRegistry, ToolCall,
};
use harw_instructions::AgentIdentity;
use harw_project_discovery::ProjectContext;

pub use error::{RegistryDefaultsError, RegistryDefaultsResult};
pub use authority::{AuthorityReducer, authority_reducer_for_role, tool_permission};
pub use profile::{
    IdentityOverrides, RegistryProfile, RestrictedToolProvider, assemble_registry,
    assemble_registry_for_project, assemble_registry_for_sandbox, profile_for_role, role_names,
};
pub use research_web::{researcher_web_network_scope, researcher_web_policy};

/// Die Werkzeuge, die ohne Nutzerrückfrage ausgeführt werden dürfen.
///
/// # Beschreibung
/// Die Liste umfasst ausschließlich Werkzeuge, die **nachweislich** nur lesen
/// und an keiner Fläche eine Freigabe (`ApprovalPolicy != None`) deklarieren:
/// lesende Dateisystem-Werkzeuge, die Dependency-Werkzeuge, `lens.ask`, die
/// Web-Recherche (deren Netzgrenze die Host-Allowlist der Sandbox zieht, nicht
/// die Freigabe) und die lesenden Status-Operationen `status`/`ps`.
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
    // Web-Recherche (`harw-tool-web`) — nur im Profil `Research` (Rolle
    // `researcher-web`, ohne `fs.*`/`deps.*`); die Netzgrenze zieht die
    // `EgressPolicy` aus `[network].researcher_web_hosts` (W5 RD).
    "web.fetch",
    "web.docs_rs",
    "web.crates_io",
    // Lesende Status-Operationen (`harw-ops`, `model_tool(readonly, approval =
    // "none")`, ohne Seitenpfad in andere Executor).
    "status",
    "ps",
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
];

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
///   automatisch freigegeben, unabhängig vom Modus (auch nicht bei
///   [`ApprovalMode::FullAccess`]).
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
        Self { mode, rules }
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
    /// Entscheidet zuerst anhand der [`AllowRuleSet`], dann anhand des
    /// Freigabemodus in der eigenen [`ApprovalModeCell`].
    ///
    /// # Description
    /// 1. [`AllowRuleSet::evaluate`] auf `call.name`/`call.arguments`:
    ///    - `Some(`[`RuleDecision::Deny`]`)` → [`ApprovalDecision::AskUser`],
    ///      unabhängig vom Modus (fail-closed, nie automatisch freigegeben).
    ///    - `Some(`[`RuleDecision::Allow`]`)` → [`ApprovalDecision::Allow`],
    ///      ohne die Modus-Logik zu befragen.
    ///    - `None` → weiter mit Schritt 2.
    /// 2. Modus-Logik (unverändert):
    ///    - [`ApprovalMode::AlwaysAsk`]: jeder Aufruf wird bestätigt, auch ein
    ///      lesender.
    ///    - [`ApprovalMode::Delegated`]: die Voreinstellung —
    ///      [`AUTO_APPROVED_TOOLS`] läuft durch, alles andere fragt.
    ///    - [`ApprovalMode::FullAccess`]: nichts fragt.
    ///
    /// Regeln und Modus werden bei **jedem** Aufruf frisch gelesen, damit eine
    /// Umschaltung sofort greift und nicht erst im nächsten Turn.
    fn review<'a>(&'a self, call: &'a ToolCall) -> ExtFuture<'a, ApprovalDecision> {
        let rule_decision = self.rules.evaluate(call.name.as_str(), &call.arguments);
        let requires_approval = match self.mode.get() {
            ApprovalMode::AlwaysAsk => true,
            ApprovalMode::Delegated => Self::requires_explicit_approval(call),
            ApprovalMode::FullAccess => false,
        };
        Box::pin(async move {
            match rule_decision {
                // Fail-closed: eine Deny-Regel darf niemals automatisch
                // freigegeben werden, auch nicht unter `FullAccess`.
                Some(RuleDecision::Deny) => ApprovalDecision::AskUser(Default::default()),
                Some(RuleDecision::Allow) => ApprovalDecision::Allow,
                None if requires_approval => ApprovalDecision::AskUser(Default::default()),
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
    fn assemble_from_current_dir_smoke() {
        let cwd = std::env::current_dir().expect("cwd");
        let ar = assemble_default_registry(cwd).expect("assemble");
        let total_tools: usize = ar
            .registry
            .tool_providers()
            .iter()
            .map(|p| p.tools().len())
            .sum();
        assert_eq!(
            total_tools,
            RegistryProfile::Full.registered_tool_names().len(),
            "das Full-Profil muss genau seine deklarierten Werkzeuge registrieren"
        );
        assert_eq!(ar.registry.instructions_providers().len(), 1);
        assert_eq!(ar.registry.context_providers().len(), 1);
        assert_eq!(ar.registry.approval_handlers().len(), 1);
    }

    #[test]
    fn assemble_default_registry_still_yields_the_full_coding_tool_set() {
        let cwd = std::env::current_dir().expect("cwd");
        let ar = assemble_default_registry(cwd).expect("assemble");

        // Ohne Browser — auch unter Feature `browser` (W5 RD: nur mit Grant).
        let expected_tools = vec![
            "fs.read".to_owned(),
            "fs.write".to_owned(),
            "fs.list".to_owned(),
            "fs.search".to_owned(),
            "fs.glob".to_owned(),
            "fs.grep".to_owned(),
            "shell.exec".to_owned(),
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

    #[test]
    fn auto_approved_tools_are_a_subset_of_the_read_only_surface() {
        // Umkehrung der früheren Deckungsprüfung: nicht „jedes Profil-Werkzeug
        // muss in die Allowlist“, sondern „jeder Allowlist-Eintrag muss
        // nachweislich read-only sein“.
        let mut read_only_surface: Vec<&str> = READ_ONLY_ROOT_OPERATIONS.to_vec();
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
        ] {
            assert!(
                DefaultApprovalPolicy::requires_explicit_approval(&call(name)),
                "{name} deklariert eine Freigabe bzw. hat Nebenwirkungen und \
                 darf nicht auto-freigegeben sein"
            );
        }
    }

    #[test]
    fn no_builtin_role_advertises_a_tool_outside_its_registered_set() {
        // Ohne `PLANNING_OPERATION_TOOLS` bewirbt keine Rolle mehr ein
        // Werkzeug, das ihre Registry nicht trägt — insbesondere nicht
        // `plan`/`goal` beim Planner, die im Kind nie einen Executor hatten.
        for role in role_names::ALL {
            let profile = profile_for_role(role).expect("eingebaute Rolle braucht ein Profil");
            assert_eq!(
                profile.tool_names(),
                profile.registered_tool_names(),
                "Rolle {role}: beworbene und registrierte Werkzeuge müssen übereinstimmen"
            );
            for tool in profile.tool_names() {
                assert!(
                    !DefaultApprovalPolicy::requires_explicit_approval(&call(tool)),
                    "Rolle {role}: {tool} würde im Kind an einer Rückfrage hängen"
                );
            }
        }
    }

    // `review` liefert ein `ExtFuture` (`Pin<Box<dyn Future>>`), aber diese
    // Crate zieht keinen Async-Runtime als Dev-Dependency; die Zukunft hat
    // ohnehin keinen echten `.await`-Punkt und wird beim ersten `poll` fertig.
    // `Waker::noop` genügt darum, um sie synchron im Test auszulesen — die
    // Crate verbietet `unsafe`, ein handgebauter RawWaker wäre hier ohnehin
    // nicht erlaubt.
    fn block_on<T>(mut future: harw_extension_api::ExtFuture<'_, T>) -> T {
        use std::task::{Context, Poll, Waker};

        let mut cx = Context::from_waker(Waker::noop());
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("review() future did not complete on first poll"),
        }
    }

    // Der Freigabemodus lebt seit G-009 in einer `ApprovalModeCell` statt in
    // einem prozessweiten Static. Jeder der folgenden Tests baut sich deshalb
    // seine eigene Zelle: Sie teilen keinen Zustand, brauchen keine
    // Wiederherstellung am Ende und können in beliebiger Reihenfolge parallel
    // laufen.

    #[test]
    fn review_in_always_ask_mode_asks_even_for_read_only_tools() {
        let policy = DefaultApprovalPolicy::new(ApprovalModeCell::new(ApprovalMode::AlwaysAsk));

        assert!(matches!(
            block_on(policy.review(&call("fs.read"))),
            ApprovalDecision::AskUser(_)
        ));
        assert!(matches!(
            block_on(policy.review(&call("shell.exec"))),
            ApprovalDecision::AskUser(_)
        ));
    }

    #[test]
    fn review_in_delegated_mode_allows_only_the_allowlist() {
        let policy = DefaultApprovalPolicy::new(ApprovalModeCell::new(ApprovalMode::Delegated));

        assert!(matches!(
            block_on(policy.review(&call("fs.read"))),
            ApprovalDecision::Allow
        ));
        assert!(matches!(
            block_on(policy.review(&call("shell.exec"))),
            ApprovalDecision::AskUser(_)
        ));
    }

    #[test]
    fn review_in_full_access_mode_allows_everything() {
        let policy = DefaultApprovalPolicy::new(ApprovalModeCell::new(ApprovalMode::FullAccess));

        assert!(matches!(
            block_on(policy.review(&call("fs.read"))),
            ApprovalDecision::Allow
        ));
        assert!(matches!(
            block_on(policy.review(&call("shell.exec"))),
            ApprovalDecision::Allow
        ));
    }

    /// Die Zusage „der Modus wird bei jedem Aufruf frisch gelesen“: Ein
    /// `set` auf einem Klon der Zelle wirkt auf die bereits gebaute Politik,
    /// ohne dass sie neu montiert werden müsste.
    #[test]
    fn review_reads_the_mode_cell_on_every_call() {
        let cell = ApprovalModeCell::new(ApprovalMode::Delegated);
        let policy = DefaultApprovalPolicy::new(cell.clone());
        let mutating = call("shell.exec");

        assert!(matches!(
            block_on(policy.review(&mutating)),
            ApprovalDecision::AskUser(_)
        ));

        cell.set(ApprovalMode::FullAccess);
        assert!(
            matches!(block_on(policy.review(&mutating)), ApprovalDecision::Allow),
            "die Umschaltung muss sofort wirken, nicht erst im nächsten Turn"
        );

        cell.set(ApprovalMode::AlwaysAsk);
        assert!(matches!(
            block_on(policy.review(&call("fs.read"))),
            ApprovalDecision::AskUser(_)
        ));
    }

    /// Eine Politik mit eigener Zelle darf von der Zelle einer anderen nichts
    /// mitbekommen — der eigentliche Grund, warum der prozessweite Schalter
    /// weg musste (G-009): Ein Kind im Modus `FullAccess` hätte sonst den
    /// Root-Modus mitverändert und umgekehrt.
    #[test]
    fn two_policies_with_separate_cells_do_not_influence_each_other() {
        let root_cell = ApprovalModeCell::new(ApprovalMode::Delegated);
        let child_cell = ApprovalModeCell::new(ApprovalMode::Delegated);
        let root = DefaultApprovalPolicy::new(root_cell.clone());
        let child = DefaultApprovalPolicy::new(child_cell);

        root_cell.set(ApprovalMode::FullAccess);

        assert!(matches!(
            block_on(root.review(&call("shell.exec"))),
            ApprovalDecision::Allow
        ));
        assert!(matches!(
            block_on(child.review(&call("shell.exec"))),
            ApprovalDecision::AskUser(_)
        ));
    }

    /// Die zusammengebaute Registry muss genau die übergebene Zelle tragen —
    /// sonst wäre das Durchreichen durch [`assemble_registry_for_project`]
    /// wirkungslos.
    #[test]
    fn assembled_registry_uses_the_approval_mode_cell_it_was_given() {
        use harw_project_discovery::{DiscoveryConfig, discover_project};

        let cwd = std::env::current_dir().expect("cwd");
        let project =
            discover_project(&cwd, &DiscoveryConfig::default()).expect("Discovery im Workspace");

        let cell = ApprovalModeCell::new(ApprovalMode::AlwaysAsk);
        let assembled = assemble_registry_for_project(
            RegistryProfile::ReadOnlyExplore,
            &project,
            IdentityOverrides::default(),
            cell.clone(),
        )
        .expect("assemble");

        let handlers = assembled.registry.approval_handlers();
        assert_eq!(handlers.len(), 1);
        let handler = &handlers[0];

        assert!(
            matches!(
                block_on(handler.review(&call("fs.read"))),
                ApprovalDecision::AskUser(_)
            ),
            "die Registry muss den Modus der übergebenen Zelle sehen"
        );

        cell.set(ApprovalMode::FullAccess);
        assert!(
            matches!(
                block_on(handler.review(&call("fs.read"))),
                ApprovalDecision::Allow
            ),
            "ein `set` auf der übergebenen Zelle muss die montierte Registry erreichen"
        );
    }

    /// Eine passende `Allow`-Regel gibt frei, ohne dass die Modus-Logik
    /// überhaupt gefragt würde — selbst wenn der Modus `AlwaysAsk` wäre.
    #[test]
    fn review_allows_a_call_matching_an_allow_rule_without_asking() {
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
            block_on(policy.review(&call)),
            ApprovalDecision::Allow
        ));
    }

    /// Eine `Deny`-Regel gewinnt über eine passende `Allow`-Regel und über
    /// den Modus `FullAccess` — beides würde ohne Regel automatisch
    /// freigeben.
    #[test]
    fn review_deny_rule_beats_allow_rule_and_full_access_mode() {
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
        let policy = DefaultApprovalPolicy::with_rules(
            ApprovalModeCell::new(ApprovalMode::FullAccess),
            rules,
        );

        let mut call = call("shell.exec");
        call.arguments = serde_json::json!({"command": "git push origin main"});

        assert!(matches!(
            block_on(policy.review(&call)),
            ApprovalDecision::AskUser(_)
        ));
    }

    /// Ohne passende Regel bleibt die bisherige Modus-Logik unverändert in
    /// Kraft — `AllowRuleSet::new()` (leer) ändert nichts am Verhalten von
    /// [`DefaultApprovalPolicy::new`].
    #[test]
    fn review_without_a_matching_rule_falls_back_to_mode_logic() {
        let policy = DefaultApprovalPolicy::new(ApprovalModeCell::new(ApprovalMode::Delegated));

        assert!(matches!(
            block_on(policy.review(&call("fs.read"))),
            ApprovalDecision::Allow
        ));
        assert!(matches!(
            block_on(policy.review(&call("shell.exec"))),
            ApprovalDecision::AskUser(_)
        ));
    }

    #[test]
    fn default_approval_policy_requires_approval_for_mutating_removed_and_unknown_tools() {
        for name in [
            "fs.write",
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
}
