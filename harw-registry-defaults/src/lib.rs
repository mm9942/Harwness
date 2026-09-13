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
//! optional `browser` Cargo feature it additionally registers the Harwness
//! browser tool surface (`harw-tool-browser` over a `harw-browser-thirtyfour`
//! `FirefoxHost`).
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
//! - [`profile::assemble_registry`]: builds any profile.
//! - [`embedded_agents`]: die eingebauten Agentendefinitionen als TOML und IR.
//!
//! # Feature `browser`
//! Disabled by default so the standard binary stays lean. When enabled,
//! [`assemble_default_registry`] constructs a `FirefoxHost` and registers
//! `HarwnessBrowserToolProvider`. `FirefoxHost::new` never starts Firefox or
//! geckodriver at construction time, so enabling this feature cannot break
//! default startup even without a running WebDriver; a missing driver only
//! surfaces as a normal tool execution error the first time a `browser.*`
//! tool actually dispatches.
//!
//! # Concurrency
//! `assemble_default_registry` is synchronous; all produced providers are
//! `Send + Sync`.

#![forbid(unsafe_code)]

mod error;

pub mod embedded_agents;
pub mod profile;

use std::path::PathBuf;

use harw_extension_api::{
    ApprovalDecision, ApprovalHandler, ApprovalMode, ExtFuture, ExtensionRegistry, ToolCall,
};
use harw_instructions::AgentIdentity;
use harw_project_discovery::ProjectContext;

pub use error::{RegistryDefaultsError, RegistryDefaultsResult};
pub use profile::{
    IdentityOverrides, RegistryProfile, RestrictedToolProvider, assemble_registry,
    profile_for_role, role_names,
};

/// Die Werkzeuge, die ohne Nutzerrückfrage ausgeführt werden dürfen.
///
/// # Beschreibung
/// Die Liste umfasst **genau** die read-only Oberfläche: lesende
/// Dateisystem-Werkzeuge, die Dependency-Werkzeuge, die Web-Recherche (deren
/// Netzgrenze die Host-Allowlist der Sandbox zieht, nicht die Freigabe), die
/// lesenden Status-Operationen sowie die Plan-/Ziel-/Delegations-Operationen
/// der Composition-Root.
///
/// Sie ist bewusst vollständig: fehlte hier ein read-only Werkzeug, bliebe
/// jeder Explore-Fan-out an einer Rückfrage hängen, die ein Kind-Agent nie
/// beantworten kann ([`profile::RegistryProfile`]-Kinder laufen mit
/// `allow_pause = false`). Umgekehrt bleibt alles Mutierende — `fs.write`,
/// `shell.exec`, `stop` — und jedes unbekannte Werkzeug freigabepflichtig.
///
/// Der Test `approval_allow_list_covers_every_read_only_profile_tool` hält die
/// Liste an die Profil-Werkzeuglisten gekoppelt, damit sie nicht veralten kann.
pub const AUTO_APPROVED_TOOLS: &[&str] = &[
    // Lesende Dateisystem-Werkzeuge (`harw-tool-fs`).
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
    // Web-Recherche (`harw-tool-web`) — die Netzgrenze zieht die Sandbox-Allowlist.
    "web.fetch",
    "web.docs_rs",
    "web.crates_io",
    // Lesende Status-Operationen.
    "status",
    "ps",
    "diff",
    // Plan-/Ziel-/Delegations-Operationen der Composition-Root.
    "plan",
    "goal",
    "explore",
    "research_deps",
    "research_web",
    "analyze",
    "mode",
];

/// Default approval boundary for the built-in coding-agent tool set.
///
/// Only the known read-only tools and operations execute without a pause
/// ([`AUTO_APPROVED_TOOLS`]). Every other tool, including `fs.write`,
/// `shell.exec`, and any future tool registered by default, requires an
/// explicit approval decision. This default is deliberately fail-closed so
/// adding a tool cannot silently widen agent authority.
#[derive(Debug, Default)]
pub struct DefaultApprovalPolicy;

impl DefaultApprovalPolicy {
    /// Returns whether `call` must be explicitly approved before dispatch.
    ///
    /// # Arguments
    /// - `call` (`&ToolCall`): der angefragte Werkzeugaufruf.
    ///
    /// # Returns
    /// `false`, wenn der Name in [`AUTO_APPROVED_TOOLS`] steht, sonst `true`.
    #[must_use]
    pub fn requires_explicit_approval(call: &ToolCall) -> bool {
        !AUTO_APPROVED_TOOLS.contains(&call.name.as_str())
    }
}

impl ApprovalHandler for DefaultApprovalPolicy {
    /// Entscheidet anhand des aktiven Freigabemodus
    /// ([`harw_extension_api::approval_mode`]).
    ///
    /// # Description
    /// - [`ApprovalMode::AlwaysAsk`]: jeder Aufruf wird bestätigt, auch ein
    ///   lesender.
    /// - [`ApprovalMode::Delegated`]: die Voreinstellung — [`AUTO_APPROVED_TOOLS`]
    ///   läuft durch, alles andere fragt.
    /// - [`ApprovalMode::FullAccess`]: nichts fragt.
    ///
    /// Der Modus wird bei **jedem** Aufruf frisch gelesen, damit eine
    /// Umschaltung sofort greift und nicht erst im nächsten Turn.
    fn review<'a>(&'a self, call: &'a ToolCall) -> ExtFuture<'a, ApprovalDecision> {
        let requires_approval = match harw_extension_api::approval_mode::current() {
            ApprovalMode::AlwaysAsk => true,
            ApprovalMode::Delegated => Self::requires_explicit_approval(call),
            ApprovalMode::FullAccess => false,
        };
        Box::pin(async move {
            if requires_approval {
                ApprovalDecision::AskUser(Default::default())
            } else {
                ApprovalDecision::Allow
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
/// Returns a typed error when project discovery fails, or — under the
/// `browser` feature — when the Firefox host configuration is invalid.
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

        // `mut` wird nur unter Feature "browser" gebraucht (siehe `extend` unten).
        #[cfg_attr(not(feature = "browser"), allow(unused_mut))]
        let mut expected_tools = vec![
            "fs.read".to_owned(),
            "fs.write".to_owned(),
            "fs.list".to_owned(),
            "fs.search".to_owned(),
            "fs.glob".to_owned(),
            "fs.grep".to_owned(),
            "shell.exec".to_owned(),
        ];
        #[cfg(feature = "browser")]
        expected_tools.extend([
            "browser.open".to_owned(),
            "browser.observe".to_owned(),
            "browser.find".to_owned(),
            "browser.act".to_owned(),
            "browser.wait".to_owned(),
            "browser.events".to_owned(),
            "browser.close".to_owned(),
        ]);

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
    fn approval_allow_list_covers_every_read_only_profile_tool() {
        // Jede read-only Profil-Werkzeugliste muss vollständig freigegeben sein:
        // sonst blockiert ein Fan-out an einer Rückfrage, die ein Kind mit
        // `allow_pause = false` nie beantworten kann.
        for profile in RegistryProfile::ALL.iter().filter(|p| p.is_read_only()) {
            for tool in profile.tool_names() {
                assert!(
                    !DefaultApprovalPolicy::requires_explicit_approval(&call(tool)),
                    "{profile:?}: {tool} fehlt in AUTO_APPROVED_TOOLS"
                );
            }
        }
    }

    #[test]
    fn approval_allow_list_covers_every_builtin_role_tool() {
        for role in role_names::ALL {
            let profile = profile_for_role(role).expect("eingebaute Rolle braucht ein Profil");
            for tool in profile.tool_names() {
                assert!(
                    !DefaultApprovalPolicy::requires_explicit_approval(&call(tool)),
                    "Rolle {role}: {tool} fehlt in AUTO_APPROVED_TOOLS"
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

    // Der Freigabemodus ist ein prozessweiter Schalter
    // (`harw_extension_api::approval_mode`). Dieser eine Test bündelt alle
    // drei Stufen und stellt am Ende ausdrücklich `Delegated` wieder her,
    // damit andere Tests im selben Binary den Startwert vorfinden.
    #[test]
    fn review_reads_the_active_approval_mode_on_every_call() {
        use harw_extension_api::approval_mode;

        let policy = DefaultApprovalPolicy;
        let read_only = call("fs.read");
        let mutating = call("shell.exec");

        approval_mode::set(ApprovalMode::AlwaysAsk);
        assert!(matches!(
            block_on(policy.review(&read_only)),
            ApprovalDecision::AskUser(_)
        ));
        assert!(matches!(
            block_on(policy.review(&mutating)),
            ApprovalDecision::AskUser(_)
        ));

        approval_mode::set(ApprovalMode::Delegated);
        assert!(matches!(
            block_on(policy.review(&read_only)),
            ApprovalDecision::Allow
        ));
        assert!(matches!(
            block_on(policy.review(&mutating)),
            ApprovalDecision::AskUser(_)
        ));

        approval_mode::set(ApprovalMode::FullAccess);
        assert!(matches!(
            block_on(policy.review(&read_only)),
            ApprovalDecision::Allow
        ));
        assert!(matches!(
            block_on(policy.review(&mutating)),
            ApprovalDecision::Allow
        ));

        approval_mode::set(ApprovalMode::Delegated);
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
