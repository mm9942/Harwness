//! Registrierungsprüfung für Fragment-Quellen: Namensraum und Trust-Deckel.
//!
//! # Verantwortungsbereich
//! Knoten **AW3-03** schließt `PlanContextProvider` (dieses Crate),
//! `harw_memory::context_provider::MemoryContextProvider` und
//! `harw_knowledge::context_provider::KnowledgeContextProvider` als Quellen
//! von [`harw_context::Fragment`] an. Jede dieser Quellen ist im Kern ein
//! `#[harw_macros::context_provider]`-artiger Baustein — aber das Attribut
//! selbst (in `harw-macros`) und die Registry, gegen die es sich meldet
//! (`harw_extension_api::registry::ExtensionRegistry`), liegen beide
//! außerhalb des Schreibbereichs dieses Knotens (`harw-extension-api` ist
//! laut Auftrag „alle gelandet"). Deshalb steht die von AW3-03 verlangte
//! Zwei-Prüfungen-Regel — **kein doppelt belegter Namensraum, keine zu hohe
//! Trust-Behauptung** — hier: die Stelle, an der dieser Knoten schreiben
//! *darf*.
//!
//! Diese Registry hält absichtlich nur **Deklarationsdaten** (Namensraum,
//! deklarierter `max_trust`, ob die Quelle nutzerbeeinflussten Inhalt tragen
//! kann, ein Kostenschätzer) statt `Arc<dyn ContextProvider>`-Trait-Objekte:
//! ein gemeinsames Provider-Trait über alle drei Quell-Crates hinweg würde
//! entweder `harw-memory` oder `harw-knowledge` dazu zwingen, von
//! `harw-plan-bridge` abhängig zu werden — und `harw-plan-bridge` hängt
//! bereits von `harw-knowledge` ab (siehe `Cargo.toml`), was sofort einen
//! Zyklus ergäbe. Reine Deklarationsdaten (`&str`, `harw_context::TrustClass`,
//! `bool`, `Arc<dyn harw_lens_types::CostEstimator>`) verlangen dagegen von
//! keiner der drei Quell-Crates eine Abhängigkeit auf diese hier — jede
//! Quelle exportiert ihre eigenen Konstanten (`NAMESPACE`, `MAX_TRUST`,
//! `MAY_CARRY_USER_CONTENT`) unter Verwendung von `harw-context`, das sie
//! ohnehin schon für ihre Fragmente brauchen.
//!
//! # Was am `#[context_provider]`-Attribut zu ergänzen wäre
//! Siehe den Abschlussbericht dieses Knotens: `harw-macros/src/contributor.rs`
//! (`ContributorArgs`, `expand_context_provider`) müsste um `namespace`,
//! `trust` und `cost` als zusätzliche Attribut-Schlüssel erweitert werden,
//! und `harw_extension_api::registry::ExtensionRegistryBuilder::context_provider`
//! müsste die hier implementierte Prüfung (oder eine äquivalente) beim
//! Registrieren aufrufen. Diese Datei kann diese Verdrahtung nicht selbst
//! vornehmen — `harw-macros` und `harw-extension-api` liegen außerhalb des
//! Schreibbereichs von AW3-03.
//!
//! # Exportierte Typen
//! [`FragmentProviderDeclaration`], [`FragmentProviderRegistry`],
//! [`FragmentRegistryError`], [`FragmentRegistryResult`].
//!
//! # Concurrency
//! [`FragmentProviderRegistry`] ist ein reiner, nicht geteilter Werttyp
//! (`Send + Sync`, solange sein `Arc<dyn CostEstimator>` es ist — das
//! verlangt bereits `CostEstimator: Send + Sync`). Registrierung geschieht
//! sequenziell über `&mut self`; es gibt keine innere Veränderlichkeit.
//!
//! # Fehler
//! [`FragmentRegistryError`] — Namensraum leer/doppelt belegt, oder eine
//! Trust-Behauptung, die eine nutzerbeeinflusste Quelle nie erheben darf.
//!
//! # Examples
//! ```rust
//! use std::sync::Arc;
//! use harw_context::TrustClass;
//! use harw_lens_types::BytesOverFour;
//! use harw_plan_bridge::fragment_registry::{FragmentProviderDeclaration, FragmentProviderRegistry};
//!
//! let mut registry = FragmentProviderRegistry::new();
//! registry
//!     .try_register(FragmentProviderDeclaration {
//!         provider_name: "PlanContextProvider",
//!         namespace: "plan".to_owned(),
//!         max_trust: TrustClass::Evidence,
//!         may_carry_user_content: true,
//!         cost_estimator: Arc::new(BytesOverFour),
//!     })
//!     .expect("first claim of 'plan' succeeds");
//!
//! // Ein zweiter Provider unter demselben Namensraum wird abgewiesen.
//! let rejected = registry.try_register(FragmentProviderDeclaration {
//!     provider_name: "RogueProvider",
//!     namespace: "plan".to_owned(),
//!     max_trust: TrustClass::Data,
//!     may_carry_user_content: true,
//!     cost_estimator: Arc::new(BytesOverFour),
//! });
//! assert!(rejected.is_err());
//! ```

use std::collections::BTreeMap;
use std::sync::Arc;

use harw_context::TrustClass;
use harw_lens_types::CostEstimator;

/// Alles, was die Registry über eine Fragment-Quelle wissen muss, bevor sie
/// registriert werden darf.
///
/// # Description
/// Entspricht den drei in Knoten AW3-03 §4 verlangten Ergänzungen des
/// `#[context_provider]`-Attributs: `namespace`, `trust` (hier `max_trust`,
/// die höchste Klasse, die die Quelle behaupten darf) und `cost` (hier
/// `cost_estimator`). `may_carry_user_content` ist die zusätzliche Angabe,
/// gegen die eine `Instruction`-Behauptung geprüft wird (siehe
/// [`FragmentProviderRegistry::try_register`]).
pub struct FragmentProviderDeclaration {
    /// Menschenlesbarer Name der Quelle, für Fehlermeldungen (z. B.
    /// `"PlanContextProvider"`).
    pub provider_name: &'static str,
    /// Sektionspräfix, unter dem diese Quelle liefert (z. B. `"plan"`).
    pub namespace: String,
    /// Höchste Vertrauensklasse, die diese Quelle behaupten darf. Einzelne
    /// gelieferte Fragmente dürfen darunter liegen, nie darüber.
    pub max_trust: TrustClass,
    /// `true`, wenn die zugrundeliegende Quelle Inhalt tragen kann, den ein
    /// Nutzer, ein Planungsteilnehmer oder ein externes Werkzeug beeinflusst
    /// hat. Eine solche Quelle darf niemals `TrustClass::Instruction`
    /// behaupten — sonst hebelt sie die Zwei-Block-Trennung aus (siehe
    /// `harw-extension-api/src/v1_compat.rs`, derselbe Grundsatz).
    pub may_carry_user_content: bool,
    /// Kostenschätzer dieser Quelle (typischerweise
    /// [`harw_lens_types::BytesOverFour`], „der einzige Kostenschätzer
    /// dieser Crate-Landschaft").
    pub cost_estimator: Arc<dyn CostEstimator>,
}

impl std::fmt::Debug for FragmentProviderDeclaration {
    /// Lässt `cost_estimator` als Platzhalter aus: `dyn CostEstimator`
    /// erzwingt kein `Debug` auf seinen Implementierungen, und ein
    /// künstliches `Debug`-Bound nur für diese eine Ausgabe wäre eine
    /// Anforderung an jeden künftigen Schätzer, die niemand sonst braucht.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FragmentProviderDeclaration")
            .field("provider_name", &self.provider_name)
            .field("namespace", &self.namespace)
            .field("max_trust", &self.max_trust)
            .field("may_carry_user_content", &self.may_carry_user_content)
            .field("cost_estimator", &"<dyn CostEstimator>")
            .finish()
    }
}

/// Fehler, die eine Registrierung ablehnen.
///
/// # Description
/// Genau die zwei in AW3-03 §4 verlangten Ablehnungen, plus eine dritte für
/// einen leeren Namensraum (derselbe Grundsatz wie
/// `harw_context::error::validate_name`: ein Muster, das nichts aussagt,
/// darf nicht stillschweigend „alles" bedeuten).
#[derive(Debug, Clone, PartialEq, Eq, harw_macros::HarwError)]
pub enum FragmentRegistryError {
    /// Der Namensraum ist nach dem Trimmen leer.
    #[msg("fragment provider namespace must not be empty or whitespace-only")]
    NamespaceBlank,

    /// Zwei Provider beanspruchen denselben Namensraum.
    #[msg(
        "namespace '{namespace}' is already claimed by provider '{existing_provider}'; \
         provider '{new_provider}' cannot also register under it"
    )]
    NamespaceAlreadyClaimed {
        /// Der doppelt beanspruchte Namensraum.
        namespace: String,
        /// Der Provider, der den Namensraum zuerst registriert hat.
        existing_provider: &'static str,
        /// Der Provider, dessen Registrierung abgelehnt wurde.
        new_provider: &'static str,
    },

    /// Eine nutzerbeeinflusste Quelle behauptet `TrustClass::Instruction`.
    #[msg(
        "provider '{provider}' claims trust {claimed:?}, but its source may carry \
         user-influenced content and must never claim instruction-level trust"
    )]
    TrustClaimTooHigh {
        /// Der Provider, dessen Behauptung abgelehnt wurde.
        provider: &'static str,
        /// Die abgelehnte Vertrauensklasse.
        claimed: TrustClass,
    },
}

/// Registry für v2-Fragment-Quellen: weist Namensraum-Kollisionen und zu
/// hohe Trust-Behauptungen ab.
///
/// # Description
/// Siehe Moduldokumentation für die Begründung, warum diese Registry nur
/// Deklarationsdaten hält statt Provider-Instanzen.
#[derive(Default)]
pub struct FragmentProviderRegistry {
    declarations: BTreeMap<String, FragmentProviderDeclaration>,
}

impl FragmentProviderRegistry {
    /// Erstellt eine leere Registry.
    ///
    /// # Returns
    /// Eine [`FragmentProviderRegistry`] ohne registrierte Namensräume.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registriert eine Fragment-Quelle, wenn beide Prüfungen bestehen.
    ///
    /// # Description
    /// Prüft in dieser Reihenfolge: (1) der Namensraum ist nach dem Trimmen
    /// nicht leer; (2) die Trust-Behauptung ist zulässig — eine Quelle mit
    /// `may_carry_user_content = true` darf niemals
    /// `TrustClass::Instruction` behaupten (`Instruction` ist die einzige
    /// Klasse, die dabei je zurückgewiesen wird — [`TrustClass::trust_rank`]
    /// kennt keine höhere); (3) der Namensraum ist noch nicht von einem
    /// anderen Provider beansprucht. Erst wenn alle drei Prüfungen bestehen,
    /// wird die Deklaration übernommen.
    ///
    /// # Arguments
    /// - `declaration` (`FragmentProviderDeclaration`): die zu registrierende
    ///   Quelle.
    ///
    /// # Returns
    /// `Ok(())`, wenn die Registrierung angenommen wurde.
    ///
    /// # Errors
    /// - [`FragmentRegistryError::NamespaceBlank`]: der Namensraum ist leer
    ///   oder nur Leerzeichen.
    /// - [`FragmentRegistryError::TrustClaimTooHigh`] — `may_carry_user_content
    ///   == true` und `max_trust == TrustClass::Instruction`.
    /// - [`FragmentRegistryError::NamespaceAlreadyClaimed`]: ein anderer
    ///   Provider hat diesen Namensraum bereits registriert.
    ///
    /// # Examples
    /// ```rust
    /// use std::sync::Arc;
    /// use harw_context::TrustClass;
    /// use harw_lens_types::BytesOverFour;
    /// use harw_plan_bridge::fragment_registry::{
    ///     FragmentProviderDeclaration, FragmentProviderRegistry, FragmentRegistryError,
    /// };
    ///
    /// let mut registry = FragmentProviderRegistry::new();
    /// let outcome = registry.try_register(FragmentProviderDeclaration {
    ///     provider_name: "UntrustedProvider",
    ///     namespace: "untrusted".to_owned(),
    ///     max_trust: TrustClass::Instruction,
    ///     may_carry_user_content: true,
    ///     cost_estimator: Arc::new(BytesOverFour),
    /// });
    /// assert!(matches!(outcome, Err(FragmentRegistryError::TrustClaimTooHigh { .. })));
    /// ```
    pub fn try_register(
        &mut self,
        declaration: FragmentProviderDeclaration,
    ) -> FragmentRegistryResult<()> {
        // Als eigenständiges `String` berechnet (statt als `&str`-Borrow auf
        // `declaration.namespace`), damit `declaration` weiter unten
        // vollständig in `self.declarations` verschoben werden kann, ohne
        // dass ein noch lebender Borrow auf eines seiner Felder das verbietet.
        let trimmed_namespace = declaration.namespace.trim().to_owned();
        if trimmed_namespace.is_empty() {
            return Err(FragmentRegistryError::NamespaceBlank);
        }

        if declaration.may_carry_user_content && declaration.max_trust == TrustClass::Instruction
        {
            return Err(FragmentRegistryError::TrustClaimTooHigh {
                provider: declaration.provider_name,
                claimed: declaration.max_trust,
            });
        }

        if let Some(existing) = self.declarations.get(&trimmed_namespace) {
            return Err(FragmentRegistryError::NamespaceAlreadyClaimed {
                namespace: trimmed_namespace,
                existing_provider: existing.provider_name,
                new_provider: declaration.provider_name,
            });
        }

        self.declarations.insert(trimmed_namespace, declaration);
        Ok(())
    }

    /// Die höchste registrierte Vertrauensklasse für einen Namensraum.
    ///
    /// # Arguments
    /// - `namespace` (`&str`): der abzufragende Namensraum.
    ///
    /// # Returns
    /// `Some(TrustClass)`, wenn der Namensraum registriert ist, sonst `None`.
    #[must_use]
    pub fn max_trust(&self, namespace: &str) -> Option<TrustClass> {
        self.declarations.get(namespace).map(|d| d.max_trust)
    }

    /// Der Kostenschätzer eines registrierten Namensraums.
    ///
    /// # Arguments
    /// - `namespace` (`&str`): der abzufragende Namensraum.
    ///
    /// # Returns
    /// `Some(&Arc<dyn CostEstimator>)`, wenn der Namensraum registriert ist,
    /// sonst `None`.
    #[must_use]
    pub fn cost_estimator(&self, namespace: &str) -> Option<&Arc<dyn CostEstimator>> {
        self.declarations.get(namespace).map(|d| &d.cost_estimator)
    }

    /// Anzahl der registrierten Namensräume.
    #[must_use]
    pub fn len(&self) -> usize {
        self.declarations.len()
    }

    /// Ob keine Quelle registriert ist.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.declarations.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::{FragmentProviderDeclaration, FragmentProviderRegistry, FragmentRegistryError};
    use harw_context::TrustClass;
    use harw_lens_types::{BytesOverFour, CostEstimator};
    use std::sync::Arc;

    fn declaration(
        provider_name: &'static str,
        namespace: &str,
        max_trust: TrustClass,
        may_carry_user_content: bool,
    ) -> FragmentProviderDeclaration {
        FragmentProviderDeclaration {
            provider_name,
            namespace: namespace.to_owned(),
            max_trust,
            may_carry_user_content,
            cost_estimator: Arc::new(BytesOverFour),
        }
    }

    #[test]
    fn test_try_register_accepts_distinct_namespaces() {
        let mut registry = FragmentProviderRegistry::new();
        registry
            .try_register(declaration("Plan", "plan", TrustClass::Evidence, true))
            .expect("plan namespace registers");
        registry
            .try_register(declaration("Memory", "memory", TrustClass::Data, true))
            .expect("memory namespace registers");
        assert_eq!(registry.len(), 2);
    }

    #[test]
    fn test_try_register_rejects_duplicate_namespace() {
        let mut registry = FragmentProviderRegistry::new();
        registry
            .try_register(declaration("Plan", "plan", TrustClass::Evidence, true))
            .expect("first claim succeeds");

        let error = registry
            .try_register(declaration("RoguePlan", "plan", TrustClass::Data, true))
            .expect_err("second claim of the same namespace must be rejected");

        match error {
            FragmentRegistryError::NamespaceAlreadyClaimed {
                namespace,
                existing_provider,
                new_provider,
            } => {
                assert_eq!(namespace, "plan");
                assert_eq!(existing_provider, "Plan");
                assert_eq!(new_provider, "RoguePlan");
            }
            other => panic!("unexpected error: {other:?}"),
        }
        // Die abgelehnte Registrierung darf die bestehende nicht überschreiben.
        assert_eq!(registry.max_trust("plan"), Some(TrustClass::Evidence));
    }

    #[test]
    fn test_try_register_rejects_instruction_claim_from_mixed_source() {
        let mut registry = FragmentProviderRegistry::new();
        let error = registry
            .try_register(declaration(
                "Untrusted",
                "untrusted",
                TrustClass::Instruction,
                true,
            ))
            .expect_err("a user-influenced source must never claim Instruction");

        assert!(matches!(
            error,
            FragmentRegistryError::TrustClaimTooHigh {
                provider: "Untrusted",
                claimed: TrustClass::Instruction,
            }
        ));
        assert!(registry.is_empty(), "rejected registration must not persist");
    }

    /// Positivkontrolle: die Prüfung lehnt nicht *jede* `Instruction`-Klasse
    /// ab, sondern nur die Kombination mit `may_carry_user_content = true`.
    /// Ohne diesen Test könnte die Prüfung versehentlich zu
    /// „`Instruction` ist nie erlaubt" verengt werden, was die Registry für
    /// jeden künftigen, wirklich systemeigenen Provider unbrauchbar machen
    /// würde.
    #[test]
    fn test_try_register_accepts_instruction_claim_from_system_source() {
        let mut registry = FragmentProviderRegistry::new();
        registry
            .try_register(declaration(
                "SystemPrompt",
                "system",
                TrustClass::Instruction,
                false,
            ))
            .expect("a system-only source may claim Instruction");
        assert_eq!(registry.max_trust("system"), Some(TrustClass::Instruction));
    }

    #[test]
    fn test_try_register_rejects_blank_namespace() {
        let mut registry = FragmentProviderRegistry::new();
        let error = registry
            .try_register(declaration("Plan", "   ", TrustClass::Evidence, true))
            .expect_err("blank namespace must be rejected");
        assert!(matches!(error, FragmentRegistryError::NamespaceBlank));
    }

    #[test]
    fn test_cost_estimator_is_retrievable_after_registration() {
        let mut registry = FragmentProviderRegistry::new();
        registry
            .try_register(declaration("Plan", "plan", TrustClass::Evidence, true))
            .expect("registration succeeds");
        let estimator = registry
            .cost_estimator("plan")
            .expect("estimator is stored");
        assert_eq!(estimator.estimate("abcd"), BytesOverFour.estimate("abcd"));
    }

    #[test]
    fn test_max_trust_and_cost_estimator_are_none_for_unknown_namespace() {
        let registry = FragmentProviderRegistry::new();
        assert_eq!(registry.max_trust("plan"), None);
        assert!(registry.cost_estimator("plan").is_none());
    }
}
