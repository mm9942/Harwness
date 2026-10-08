//! Erweiterungspunkt der Runtime-Montage (`AssemblyContributor`).
//!
//! # Beschreibung
//! Die Montage in [`crate::assembly`] ist bewusst geschlossen: sie kennt genau
//! die Bausteine, die `docs/design/runtime-contracts.md` §runtime-spec nennt, und
//! keine Erweiterungsliste. Teil B des Plans („Abgleich mit Teil A") braucht
//! aber eine Stelle, an der ein Subsystem (Netz-Politik W5, DoD-Kette,
//! Browser-Host, Web-UI) **zusätzliche** Werkzeuge, Operationen oder
//! Lebenszyklus-Haken beisteuert, ohne dass `assembly.rs` jedes dieser Crates
//! kennen muss — `harw-runtime` liegt auf L12 und darf sie gar nicht sehen.
//!
//! Ein Beitrag ist **additiv und verengbar, nie erweiternd auf der
//! Rechte-Achse**: [`AssemblyParts`] trägt keine Sandbox, keine
//! Berechtigungen, keine Kontext-Decke und keinen Freigabemodus. Alles, was
//! Rechte vergibt, entsteht ausschließlich aus [`crate::spec::EntryKind::profile`].
//! Ein Contributor kann damit Werkzeuge anbieten und Haken registrieren, aber
//! nicht die Rechte-Matrix umgehen.
//!
//! # Reihenfolge
//! Die Contributors laufen in genau der Reihenfolge, in der sie am Builder
//! registriert wurden ([`crate::assembly::RuntimeAssemblyBuilder::contributor`]),
//! nachdem Registry, Operationen und Spawner stehen, und **bevor** aus
//! [`AssemblyParts`] die endgültige `ExtensionRegistry` und die
//! [`crate::services::RuntimeServices`] gebaut werden. Ein Contributor sieht
//! also die Beiträge aller vor ihm registrierten.

use std::sync::Arc;

use harw_agent_dsl::roles::AgentRoleId;
use harw_authority::NetworkScope;
use harw_config::ResolvedConfig;
use harw_core::SpawnContext;
use harw_extension_api::ExtensionRegistryBuilder;
use harw_extension_api::approval_mode::ApprovalModeCell;
use harw_operations::registry::OperationRegistry;
use harw_project_discovery::ProjectContext;
use harw_protocol::GatewayPort;

use crate::assembly::{SessionLifecycleHook, TurnLimits};
use crate::config::ConfigTrustReport;
use crate::error::RuntimeResult;
use crate::spec::{EntryProfile, OperationSurface, RootBudget, RuntimeSpec};

/// Nur-lesende Sicht auf alles, was die Montage bereits entschieden hat.
///
/// # Beschreibung
/// Ein Contributor darf jede dieser Angaben **lesen**, um seinen Beitrag
/// zuzuschneiden (etwa: kein Netz-Werkzeug, wenn `profile.permissions` kein
/// Netz trägt), aber keine davon ändern. Deshalb ausschließlich geteilte
/// Referenzen und keine `&mut`-Felder.
#[derive(Debug)]
pub struct AssemblyInputs<'a> {
    /// Die Eingangsbeschreibung des Laufs.
    pub spec: &'a RuntimeSpec,
    /// Das aus [`crate::spec::EntryKind::profile`] abgeleitete Profil.
    pub profile: &'a EntryProfile,
    /// Die aufgelöste Konfiguration.
    pub config: &'a ResolvedConfig,
    /// Vertrauensbericht der Konfigurationsschichten.
    pub trust_report: &'a ConfigTrustReport,
    /// Der **einmalig** ermittelte Projektkontext (keine zweite Discovery).
    pub project: &'a ProjectContext,
    /// Der Spawn-Kontext der Wurzel (Sandbox, Decke, Trace, Akteur).
    pub spawn_context: &'a SpawnContext,
    /// Das Budget des Wurzel-Agenten.
    pub budget: &'a RootBudget,
    /// Die daraus abgeleiteten Turn-Grenzwerte.
    pub turn_limits: &'a TurnLimits,
    /// Die Freigabe-Zelle des Laufs (lesbar, umschaltbar durch ihren Besitzer).
    pub approval_mode: &'a ApprovalModeCell,
}

/// Die veränderlichen Teile der Montage.
///
/// # Beschreibung
/// Bewusst klein gehalten: vier Felder, von denen keines Rechte vergibt.
/// `registry` und `operations` sind noch nicht gebaut bzw. noch nicht geteilt,
/// wenn die Contributors laufen — ein Beitrag landet deshalb wirklich in der
/// Registry, die die Wurzelsitzung bekommt, und in der Operationsliste, die
/// jede [`crate::services::ServiceSurface`] sieht.
pub struct AssemblyParts {
    /// Der noch offene Registry-Bauer der Wurzel; hier kommen Tool-,
    /// Kontext- und Instruktions-Provider hinzu.
    pub registry: ExtensionRegistryBuilder,
    /// Die Operations-Registry der Wurzel (Slash-Befehle und Modell-Tools).
    pub operations: OperationRegistry,
    /// Haken, die [`crate::assembly::RuntimeAssembly::close_session`] aufruft.
    pub lifecycle_hooks: Vec<Arc<dyn SessionLifecycleHook>>,
    /// Der Netz-Scope des Laufs. Bis Welle W5 (P1.7) bleibt er leer; ein
    /// Contributor der Netz-Welle setzt ihn hier, und nur hier.
    pub network_scope: NetworkScope,
}

impl std::fmt::Debug for AssemblyParts {
    /// Zeigt Größen statt Inhalte: Provider und Haken sind Trait-Objekte ohne
    /// `Debug`, und ihre Namen gehören nicht in ein Montage-Log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AssemblyParts")
            .field("operations", &self.operations.len())
            .field("lifecycle_hooks", &self.lifecycle_hooks.len())
            .field("network_scope_is_empty", &self.network_scope.is_empty())
            .finish_non_exhaustive()
    }
}

/// Ein Subsystem, das der Montage etwas beisteuert.
///
/// # Beschreibung
/// Vertrag aus dem Plan-Abschnitt „Abgleich mit Teil A". Implementierungen
/// leben **nicht** in `harw-runtime`, sondern in den Einstiegs-Crates
/// (`harw-cli`, `harw-tui`) bzw. in den Subsystem-Crates, die diese kennen;
/// `harw-runtime` sieht sie nur als Trait-Objekt.
///
/// # Nebenläufigkeit
/// `Send + Sync`: die Liste wird als `Vec<Arc<dyn AssemblyContributor>>`
/// gehalten und kann zwischen Threads wandern. `contribute` bekommt `&self`
/// und darf keinen eigenen veränderlichen Zustand ohne innere
/// Synchronisation führen.
pub trait AssemblyContributor: Send + Sync {
    /// Steuert Werkzeuge, Operationen, Lebenszyklus-Haken oder Netz-Scope bei.
    ///
    /// # Argumente
    /// - `inputs` ([`AssemblyInputs`]): nur-lesende Sicht auf die Montage.
    /// - `parts` (`&mut `[`AssemblyParts`]): die veränderlichen Teile.
    ///
    /// # Fehler
    /// [`crate::error::RuntimeError`], wenn der Beitrag nicht erbracht werden
    /// kann. Die Montage bricht dann ab — fail-closed: ein Lauf mit halb
    /// beigesteuertem Subsystem entsteht nicht.
    fn contribute(
        &self,
        inputs: &AssemblyInputs<'_>,
        parts: &mut AssemblyParts,
    ) -> RuntimeResult<()>;
}

/// Die Contributors, die jeder Einstieg ohne weitere Angabe bekommt.
///
/// # Beschreibung
/// [`crate::assembly::RuntimeAssemblyBuilder`] startet mit genau dieser
/// Liste; `.contributor(..)` hängt weitere an. Enthalten:
/// - [`crate::mcp_wiring::McpContributor`]: konfigurierte MCP-Server als
///   Werkzeuge `mcp.<server>.<tool>` (No-op ohne aktive `[mcps.*]`).
/// - [`InfrastructureContributor`]: die `infra.*`-Operationen, wenn
///   `[infrastructure]` konfiguriert ist (No-op ohne die Sektion).
/// - [`GatewayDiagnosticsContributor`][]: `gateway.health`/`gateway.logs`/
/// - [`CloudOpsContributor`]: the `cloud.*` operations of the cloud home
///   stack (`cloud.status`, `cloud.enrollments.list`, `cloudctl.*`) for the
///   UIA root (no-op for any other root).
///   `gateway.channels.*` für eine UIA-Wurzel (No-op für jede andere Wurzel).
/// - [`BrowserRootContributor`] (Feature `browser`): `browser.*` für die
///   Wurzelsitzung, wenn `[browser].roles` `"root"` enthält.
#[must_use]
pub fn default_contributors() -> Vec<Arc<dyn AssemblyContributor>> {
    // `mut` wird nur mit Feature `browser` gebraucht.
    #[cfg_attr(not(feature = "browser"), allow(unused_mut))]
    let mut contributors: Vec<Arc<dyn AssemblyContributor>> = vec![
        Arc::new(crate::mcp_wiring::McpContributor),
        Arc::new(InfrastructureContributor),
        Arc::new(GatewayDiagnosticsContributor),
        Arc::new(CloudOpsContributor),
    ];
    #[cfg(feature = "browser")]
    contributors.push(Arc::new(BrowserRootContributor));
    contributors
}

/// Registriert die `infra.*`-Operationen (Crypto-Infrastruktur-Masterplan v2
/// §11.3, §34 H5), sofern `[infrastructure]` konfiguriert ist.
///
/// # Beschreibung
/// Muster: **Registrierung gated, Verfügbarkeit über den Dienst.** Ohne
/// `[infrastructure]` registriert der Contributor nichts — die Operationen
/// erscheinen dann weder in `/help` noch als Web-Route. Mit Sektion hängt er
/// die vier Operationen aus [`harw_ops::infra::register_infrastructure`] an
/// die Operations-Registry der Wurzel (`infra.status`, `infra.health`,
/// `infra.auth.keys.describe`, `infra.auth.keys.rotate`). Ob sie dann
/// tatsächlich arbeiten, entscheidet allein der Dienst
/// `Arc<InfrastructureAvailability>`, den die Montage aus derselben Sektion
/// baut ([`crate::infrastructure::build_infrastructure`]) und den
/// [`crate::services::RuntimeServices`] nur auf Slash und Web legt: ist die
/// Sektion ungültig, fehlt der Dienst und jede Operation meldet
/// `OpError::NotAvailable` — ehrlich statt still verschwunden.
///
/// Die Operationen tragen **keine** `ModelTool`-Fläche; der Contributor
/// erweitert die Modell-Werkzeugliste daher nicht. Für einen Einstieg mit
/// [`OperationSurface::None`] registriert er ebenfalls nichts — die Montage
/// hat für ihn bewusst eine leere Registry gebaut.
///
/// Ein Registrierungskonflikt (etwa ein zweites Mal registriert) wird als
/// Warnung gemeldet und bricht die Montage nicht ab: Infrastruktur ist
/// optional.
#[derive(Debug, Default)]
pub struct InfrastructureContributor;

impl AssemblyContributor for InfrastructureContributor {
    fn contribute(
        &self,
        inputs: &AssemblyInputs<'_>,
        parts: &mut AssemblyParts,
    ) -> RuntimeResult<()> {
        if inputs.config.infrastructure.is_none()
            || inputs.profile.operations == OperationSurface::None
        {
            return Ok(());
        }
        if let Err(error) = harw_ops::infra::register_infrastructure(&mut parts.operations) {
            tracing::warn!(%error, "runtime.infrastructure_operations_not_registered");
        }
        Ok(())
    }
}

/// Ob eine Wurzel die `gateway.*`-Operationen bekommt (R18 D-B).
///
/// # Beschreibung
/// Nur die User-Interface-Agent-Wurzel (`AgentRoleId::UserInterface`) — sie
/// ist nach R18 D-A die einzige Rolle mit eigenen Werkzeugrechten am Gateway;
/// Unteragenten und Worker erreichen das Gateway nie über diese Fläche. Ein
/// Einstieg mit [`OperationSurface::None`] bekommt, wie bei
/// [`InfrastructureContributor`], nichts.
fn gateway_ops_wanted(role: AgentRoleId, operations: OperationSurface) -> bool {
    role == AgentRoleId::UserInterface && operations != OperationSurface::None
}

/// Registriert die zehn port-gestützten `gateway.*`-Operationen des
/// R18-Vertrags (§6), wenn die Laufzeit mit einem Gateway verbunden ist.
///
/// # Beschreibung
/// Muster wie [`InfrastructureContributor`]: **Registrierung gated,
/// Verfügbarkeit über den Dienst.** Der Contributor existiert nur, wenn der
/// Einstieg einen `Arc<dyn GatewayPort>` hat — er ist deshalb **nicht** Teil
/// von [`default_contributors`], sondern wird vom verbundenen Einstieg über
/// [`crate::assembly::RuntimeAssemblyBuilder::gateway_port`] angehängt. Ohne
/// Gateway gibt es ihn nicht, und die Operationen erscheinen weder als
/// Modell-Werkzeug noch in `/help` oder als Web-Route.
///
/// Registriert wird nur für die UIA-Wurzel ([`gateway_ops_wanted`]). Lesende
/// Operationen sind freie Modell-Werkzeuge, mutierende tragen
/// `approval = "always"`; die Schlüsseloperationen (`infra.auth.keys.*`)
/// bleiben ohne Modellfläche (Regel in `harw_ops::infra`).
///
/// Der Port selbst liegt als `Arc<dyn GatewayPort>` in den Service-Maps der
/// Flächen Slash, Modell-Werkzeug und Web (nie Job,
/// [`crate::services::ServiceSurface::allows_gateway`]);
/// [`AssemblyParts`] trägt bewusst keine Dienste, deshalb setzt
/// [`crate::assembly::RuntimeAssemblyBuilder::gateway_port`] beides: diesen
/// Contributor und `RuntimeServicesParts::gateway`. Solange der Dienst fehlt,
/// melden alle zehn Operationen `OpError::NotAvailable` („gateway not
/// configured") — fail closed.
///
/// Ein Registrierungskonflikt wird als Warnung gemeldet und bricht die
/// Montage nicht ab.
pub struct GatewayContributor {
    port: Arc<dyn GatewayPort>,
}

impl GatewayContributor {
    /// Contributor für eine mit dem Gateway verbundene Laufzeit.
    #[must_use]
    pub fn new(port: Arc<dyn GatewayPort>) -> Self {
        Self { port }
    }

    /// Der Gateway-Port, den die Montage in die Service-Maps legt.
    #[must_use]
    pub fn port(&self) -> &Arc<dyn GatewayPort> {
        &self.port
    }
}

impl std::fmt::Debug for GatewayContributor {
    /// Der Port ist ein Trait-Objekt ohne `Debug`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GatewayContributor").finish_non_exhaustive()
    }
}

impl AssemblyContributor for GatewayContributor {
    fn contribute(
        &self,
        inputs: &AssemblyInputs<'_>,
        parts: &mut AssemblyParts,
    ) -> RuntimeResult<()> {
        if !gateway_ops_wanted(
            inputs.spawn_context.organizational_role,
            inputs.profile.operations,
        ) {
            return Ok(());
        }
        if let Err(error) = harw_ops::gateway_ops::register_gateway(&mut parts.operations) {
            tracing::warn!(%error, "runtime.gateway_operations_not_registered");
        }
        Ok(())
    }
}

/// Registriert die lokale Gateway-Diagnose (`gateway.health`,
/// `gateway.logs`, `gateway.channels.list`, `gateway.channels.connect_info`)
/// für die UIA-Wurzel.
///
/// # Beschreibung
/// Sie lesen nur das gebundene harw-Home (Daemon-Prozess, Sockets,
/// Logdateien) über den überall liegenden `ResolvedHomeContext` bzw. die
/// Kanal-Konfiguration (`Arc<ResolvedConfig>`) und brauchen keinen
/// Gateway-Port; deshalb Teil von [`default_contributors`] (F1: „wie binde ich
/// einen Kanal an" wird gerade ohne verbundenes Gateway gefragt). Nur die
/// UIA-Wurzel bekommt sie ([`gateway_ops_wanted`]) — sie beantworten dort die
/// Frage „wie steht es um die Gateway-Kommunikation", für die eine UIA
/// sonst `ps`/`ls`/`tail` über die Shell bemühen müsste.
#[derive(Debug, Default)]
pub struct GatewayDiagnosticsContributor;

impl AssemblyContributor for GatewayDiagnosticsContributor {
    fn contribute(
        &self,
        inputs: &AssemblyInputs<'_>,
        parts: &mut AssemblyParts,
    ) -> RuntimeResult<()> {
        if !gateway_ops_wanted(
            inputs.spawn_context.organizational_role,
            inputs.profile.operations,
        ) {
            return Ok(());
        }
        if let Err(error) =
            harw_ops::gateway_ops::register_gateway_diagnostics(&mut parts.operations)
        {
            tracing::warn!(%error, "runtime.gateway_diagnostics_not_registered");
        }
        Ok(())
    }
}

/// Attaches the browser tools (Firefox/geckodriver, WebDriver BiDi) to the
/// root registry if `[browser].enabled` and `[browser].roles` contain the
/// `root` role. Children get them per-role in `children.rs` through the same
/// configuration.
/// Registers the seven `cloud.*` operations of the cloud home stack
/// (`harw-cloud-ops::register_cloud`) for the UIA root.
///
/// # Description
/// Same pattern as [`GatewayDiagnosticsContributor`]: only the UIA root gets
/// them (``[`gateway_ops_wanted`]``), because they answer "how is the cloud
/// stack doing" (status, enrollments) and offer the controlled mutations
/// (up/down/restart/enroll, each with `approval = "always"` in the op
/// definition). The read operations are free model tools.
///
/// # Boundaries
/// No gateway port needed: `cloud.status` probes loopback ports and the
/// systemd unit locally; the mutations run through the `harw-cloudctl`
/// binary. Without a cloud stack they answer `OpError::NotAvailable` —
/// fail closed.
#[derive(Debug, Default)]
pub struct CloudOpsContributor;

impl AssemblyContributor for CloudOpsContributor {
    fn contribute(
        &self,
        inputs: &AssemblyInputs<'_>,
        parts: &mut AssemblyParts,
    ) -> RuntimeResult<()> {
        if !gateway_ops_wanted(
            inputs.spawn_context.organizational_role,
            inputs.profile.operations,
        ) {
            return Ok(());
        }
        // PL-90(H1) local build fix: the harw-cloud-ops crate sources are not
        // present in the repository, so the cloud.* operation registration is
        // disabled until the crate is restored.
    }
}

/// Attaches the browser tools (Firefox/geckodriver, WebDriver BiDi) to the
/// root registry if `[browser].enabled` and `[browser].roles` contain the
/// `root` role. Children get them per-role in `children.rs` through the same
/// configuration.

#[cfg(feature = "browser")]
#[derive(Debug, Default)]
pub struct BrowserRootContributor;

#[cfg(feature = "browser")]
impl AssemblyContributor for BrowserRootContributor {
    fn contribute(
        &self,
        inputs: &AssemblyInputs<'_>,
        parts: &mut AssemblyParts,
    ) -> RuntimeResult<()> {
        if !inputs.config.browser.grants_role("root") {
            return Ok(());
        }
        match harw_registry_defaults::profile::browser_tool_provider_for_config(
            &inputs.config.browser,
        ) {
            Ok(Some(provider)) => {
                let registry = std::mem::take(&mut parts.registry);
                parts.registry = registry.tool_provider(provider);
            }
            Ok(None) => {}
            Err(error) => tracing::warn!(%error, "runtime.browser_root_provider_unavailable"),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_contributors_include_mcp() {
        assert!(!default_contributors().is_empty());
    }

    #[test]
    fn default_contributors_include_infrastructure() {
        // MCP + Infrastruktur + Gateway-Diagnose, plus Browser mit Feature
        // `browser`.
        let expected = if cfg!(feature = "browser") { 4 } else { 3 };
        assert_eq!(default_contributors().len(), expected);
    }

    /// R18 D-B: `gateway.*` nur für die UIA-Wurzel und nie ohne
    /// Operationsfläche.
    #[test]
    fn gateway_ops_reach_only_the_uia_root() {
        for surface in [
            OperationSurface::AllWithModelTools,
            OperationSurface::CommandsOnly,
        ] {
            assert!(gateway_ops_wanted(AgentRoleId::UserInterface, surface));
            for role in [
                AgentRoleId::RootOrchestrator,
                AgentRoleId::ChildOrchestrator,
                AgentRoleId::Worker,
                AgentRoleId::UiaWorker,
                AgentRoleId::AgentSteward,
            ] {
                assert!(!gateway_ops_wanted(role, surface), "{role:?}");
            }
        }
        assert!(!gateway_ops_wanted(
            AgentRoleId::UserInterface,
            OperationSurface::None
        ));
    }
}
