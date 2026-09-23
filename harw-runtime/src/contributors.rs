//! Erweiterungspunkt der Runtime-Montage (`AssemblyContributor`).
//!
//! # Beschreibung
//! Die Montage in [`crate::assembly`] ist bewusst geschlossen: sie kennt genau
//! die Bausteine, die `docs/remediation/CONTRACTS.md` §runtime-spec nennt, und
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

use harw_authority::NetworkScope;
use harw_config::ResolvedConfig;
use harw_core::SpawnContext;
use harw_extension_api::ExtensionRegistryBuilder;
use harw_extension_api::approval_mode::ApprovalModeCell;
use harw_operations::registry::OperationRegistry;
use harw_project_discovery::ProjectContext;

use crate::assembly::{SessionLifecycleHook, TurnLimits};
use crate::config::ConfigTrustReport;
use crate::error::RuntimeResult;
use crate::spec::{EntryProfile, RootBudget, RuntimeSpec};

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
/// - [`BrowserRootContributor`] (Feature `browser`): `browser.*` für die
///   Wurzelsitzung, wenn `[browser].roles` `"root"` enthält.
#[must_use]
pub fn default_contributors() -> Vec<Arc<dyn AssemblyContributor>> {
    let mut contributors: Vec<Arc<dyn AssemblyContributor>> =
        vec![Arc::new(crate::mcp_wiring::McpContributor)];
    #[cfg(feature = "browser")]
    contributors.push(Arc::new(BrowserRootContributor));
    contributors
}

/// Hängt die Browser-Werkzeuge (Firefox/geckodriver, WebDriver BiDi) an die
/// Wurzel-Registry, sofern `[browser].enabled` und `[browser].roles` die
/// Rolle `root` enthält. Kinder bekommen sie rollenweise in
/// `children.rs` über dieselbe Konfiguration.
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
}
