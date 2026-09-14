//! ExtensionRegistry + Builder — der Core fragt nur diese Registry ab.
//!
//! # Namensraum-Prüfung für Context-Provider (AW3-03 → Makro-Knoten → dieser Knoten)
//!
//! Befund des Knotens AW3-03: `#[harw_macros::context_provider]` konnte
//! weder einen Namensraum noch eine Vertrauensklasse (`harw_context::TrustClass`)
//! deklarieren, und diese Registry prüfte beim Registrieren keines von
//! beidem — ein Fragmentanbieter durfte seine eigene `TrustClass` frei
//! behaupten, und zwei Anbieter konnten denselben Namensraum beanspruchen,
//! ohne dass das auffiel. AW3-03 hat eine Prüfung dafür schon einmal gebaut,
//! notgedrungen in `harw_plan_bridge::fragment_registry` (außerhalb des
//! eigenen Schreibbereichs) — eine Prüfung, die nur greift, wenn ein
//! Anbieter freiwillig durch diese eine Crate geht. Der Makro-Knoten hat die
//! Prüfung hierher gebracht, aber musste sie hinter zwei Registrierungswegen
//! belassen: [`ExtensionRegistryBuilder::context_provider_declared`] prüfte,
//! das bestehende [`ExtensionRegistryBuilder::context_provider`] nicht — weil
//! [`ContextProvider`] selbst weder `namespace()` noch `max_trust()` kannte
//! und ein bereits typgelöschter `Arc<dyn ContextProvider>` diese
//! Information deshalb nirgends mitführen konnte.
//!
//! **Dieser Knoten schließt genau diese Lücke:** [`ContextProvider::namespace`]
//! und [`ContextProvider::max_trust`] (siehe `contributors.rs`) sitzen jetzt
//! auf dem Trait selbst, mit Vorgabewerten. Jeder Wert dieses Typs — ob
//! frisch erzeugt oder bereits zu `Arc<dyn ContextProvider>` typgelöscht —
//! trägt sie deshalb mit sich. [`ExtensionRegistryBuilder::context_provider`]
//! liest sie jetzt direkt vom übergebenen Trait-Objekt und wendet dieselbe
//! Prüfung an, die zuvor nur `context_provider_declared` kannte. Es gibt
//! damit keinen Registrierungsweg mehr, der die Prüfung umgeht: jeder
//! sichtbare `ContextProvider` läuft entweder über `context_provider` oder
//! über `context_provider_declared`, und beide teilen sich dieselbe interne
//! Prüfung (`ExtensionRegistryBuilder::claim_context_provider_namespace`).
//!
//! ## Warum die Prüfung hier steht und nicht bei den Aufrufern
//!
//! Jeder Aufrufer (`harw-registry-defaults`, `harw-cli`, `harw-tui`) müsste
//! dieselbe Namensraum-Kollisionsprüfung sonst selbst mitbringen — und jeder
//! neue Aufrufer, der das vergisst, wäre wieder ein unbeaufsichtigter
//! Registrierungsweg. Eine Prüfung, die nur an *einer* Stelle existiert und
//! die *jeder* Registrierungsweg zwangsläufig durchläuft, ist die einzige,
//! die sich nicht durch einen neuen Aufrufer umgehen lässt. Genau dasselbe
//! Argument gilt jetzt auch dafür, warum `namespace()`/`max_trust()` auf dem
//! `ContextProvider`-Trait selbst sitzen und nicht an dieser Registry: eine
//! Prüfung, die nur greift, wenn der Aufrufer sie zusätzlich zur normalen
//! Registrierung durchläuft, ist keine Prüfung, sondern eine Bitte.
//!
//! ## Was tatsächlich geprüft wird — und was bewusst nicht
//!
//! Geprüft werden zwei Dinge, in dieser Reihenfolge:
//! 1. der (getrimmte) Namensraum ist nicht leer;
//! 2. der Namensraum ist innerhalb dieses Builders noch nicht von einem
//!    anderen Provider beansprucht.
//!
//! **Nicht** geprüft wird, ob die von einem Provider behauptete `TrustClass`
//! „zu hoch" für diesen Provider ist. Eine solche Prüfung bräuchte einen
//! Vergleichsmaßstab, der unabhängig von der Behauptung selbst ist — eine
//! zweite, unabhängige Instanz, die sagt „dieser Provider darf höchstens
//! Klasse X". Diese Frage wurde für diesen Knoten erneut geprüft (nicht nur
//! übernommen): `harw-context` besitzt mit [`harw_context::ContextCeiling`]
//! zwar eine unabhängige Obergrenze, aber sie ist an eine **Sitzung/ein
//! Kind** gebunden (siehe `harw_core::child_controller`), nicht an einen
//! **Namensraum** oder eine Provider-Identität — sie sagt „was darf dieser
//! Turn insgesamt sehen", nicht „was darf *dieser Provider* höchstens
//! behaupten". Es gibt also weiterhin keine Instanz, die unabhängig von der
//! `#[context_provider(trust = ...)]`-Deklaration selbst eine
//! Provider-eigene Obergrenze kennt. Eine Prüfung, deren Vergleichswert aus
//! derselben Quelle stammt wie der geprüfte Wert, ist tautologisch und
//! dauerhaft grün — genau der Fehler, der in diesem Programm bereits einmal
//! aufgetreten ist (K43, `harw_lens_query::query`). Die tatsächliche
//! Sicherheitsmaßnahme gegen eine selbstgewählte, zu hohe Vertrauensklasse
//! ist deshalb nicht diese Prüfung, sondern der Vorgabewert selbst: ein
//! Provider ohne eigene Angabe bekommt [`TrustClass::Data`], die niedrigste
//! Klasse (siehe [`ContextProvider::max_trust`]) — ein Provider muss eine
//! höhere Klasse *aktiv und sichtbar* im Quelltext behaupten, sie fällt ihm
//! nie zu. Sollte künftig eine unabhängige Autorität für Trust-Obergrenzen
//! je Namensraum entstehen (z. B. eine Konfigurationsdatei, die ein
//! Betreiber pflegt, oder eine an die Registrierung gebundene
//! `ContextCeiling`), ist das der Zeitpunkt, an dem diese Datei eine
//! `TrustClaimTooHigh`-Prüfung bekommen sollte — nicht vorher.
//!
//! ## Wieso zwei Registrierungsmethoden bestehen bleiben — und beide jetzt prüfen
//!
//! [`DeclaredContextProvider`] und [`ExtensionRegistryBuilder::context_provider_declared`]
//! bleiben erhalten, obwohl [`ContextProvider`] jetzt selbst `namespace()`/
//! `max_trust()` trägt und `context_provider_declared` damit fachlich
//! redundant geworden ist. Grund ist ausschließlich Rückwirkungsfreiheit
//! außerhalb dieses Schreibbereichs: `harw-macros/src/contributor.rs`
//! generiert für jeden `#[context_provider]`-Typ ein `impl
//! DeclaredContextProvider for #struct_ident { .. }`, aber **überschreibt
//! nicht** die neuen `ContextProvider::namespace()`/`max_trust()`-Methoden —
//! das zu ändern liegt in `harw-macros`, außerhalb des Schreibbereichs
//! dieses Knotens (siehe Abschlussbericht für die konkrete Nachzugszeile).
//! Bis dahin liefert `p.namespace()`/`p.max_trust()` für einen
//! makro-erzeugten Typ nur die Vorgabewerte (Typname, `TrustClass::Data`),
//! während `p.declared_namespace()`/`p.declared_max_trust()` weiterhin die
//! tatsächlich deklarierten Werte tragen. `context_provider_declared` bleibt
//! deshalb vorerst der einzige Weg, der für makro-erzeugte Provider die
//! *richtigen* Werte prüft; `context_provider` prüft ab sofort *irgendeinen*
//! Wert — niemals gar keinen. Beide Methoden rufen intern dieselbe
//! Prüfung auf (`ExtensionRegistryBuilder::claim_context_provider_namespace`),
//! tragen also identisches Verhalten für Kollisionen und leere Namensräume.
//! Sobald `harw-macros` `ContextProvider::namespace()`/`max_trust()` direkt
//! überschreibt, liefern beide Methoden für makro-erzeugte Typen dieselben
//! Werte und `context_provider_declared`/`DeclaredContextProvider` können
//! ersatzlos entfernt werden.
//!
//! # Exportierte Typen
//! [`ExtensionRegistry`], [`ExtensionRegistryBuilder`],
//! [`DeclaredContextProvider`], [`ContextProviderRegistrationError`].
//!
//! # Concurrency
//! [`ExtensionRegistryBuilder`] wird sequenziell über `&mut self`/`self`
//! aufgebaut (kein geteilter Zustand, keine innere Veränderlichkeit). Das
//! fertige [`ExtensionRegistry`] hält ausschließlich `Arc<dyn Trait>`-Werte
//! (`Send + Sync`, vorausgesetzt durch die Trait-Definitionen in
//! `contributors.rs`) und ist selbst unveränderlich.
//!
//! # Fehler
//! [`ContextProviderRegistrationError`] — leerer oder doppelt beanspruchter
//! Namensraum.

use crate::capabilities::*;
use crate::contributors::*;
use harw_context::TrustClass;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Registry die alle Extensions hält — der Core fragt nur diese ab.
pub struct ExtensionRegistry {
    tool_providers: Vec<Arc<dyn ToolProvider>>,
    context_providers: Vec<Arc<dyn ContextProvider>>,
    instructions_providers: Vec<Arc<dyn InstructionsProvider>>,
    approval_handlers: Vec<Arc<dyn ApprovalHandler>>,
    turn_observers: Vec<Arc<dyn TurnObserver>>,
    spawner: Option<Arc<dyn AgentSpawner>>,
    /// Namensraum -> (Providername, behauptete `TrustClass`) für jeden über
    /// [`ExtensionRegistryBuilder::context_provider`] oder
    /// [`ExtensionRegistryBuilder::context_provider_declared`] registrierten
    /// Provider — inzwischen also für **jeden** registrierten Provider,
    /// nicht nur für einen Teil davon. Reine Introspektionsdaten (siehe
    /// Moduldoku, Abschnitt „Was tatsächlich geprüft wird") — kein
    /// Vergleichsmaßstab wird hierauf aufgebaut.
    context_provider_namespaces: BTreeMap<String, (&'static str, TrustClass)>,
}

impl ExtensionRegistry {
    pub fn builder() -> ExtensionRegistryBuilder {
        ExtensionRegistryBuilder::default()
    }

    pub fn tool_providers(&self) -> &[Arc<dyn ToolProvider>] {
        &self.tool_providers
    }
    /// Appends a tool provider after the registry has been assembled.
    pub fn add_tool_provider(&mut self, provider: Arc<dyn ToolProvider>) {
        self.tool_providers.push(provider);
    }
    pub fn context_providers(&self) -> &[Arc<dyn ContextProvider>] {
        &self.context_providers
    }
    pub fn instructions_providers(&self) -> &[Arc<dyn InstructionsProvider>] {
        &self.instructions_providers
    }
    pub fn approval_handlers(&self) -> &[Arc<dyn ApprovalHandler>] {
        &self.approval_handlers
    }
    pub fn turn_observers(&self) -> &[Arc<dyn TurnObserver>] {
        &self.turn_observers
    }
    pub fn spawner(&self) -> Option<&Arc<dyn AgentSpawner>> {
        self.spawner.as_ref()
    }

    /// Providername und behauptete `TrustClass` eines registrierten
    /// Namensraums.
    ///
    /// # Description
    /// Reine Introspektion für Aufrufer, die wissen wollen, welcher Provider
    /// unter welcher `TrustClass` in einem Namensraum liefert (z. B. für
    /// Diagnose-Ausgaben). Diese Methode vergleicht nichts und trifft keine
    /// Zulässigkeitsentscheidung — siehe Moduldoku, Abschnitt „Was
    /// tatsächlich geprüft wird — und was bewusst nicht", für die Begründung,
    /// warum es hier keinen Ceiling-Vergleich gibt.
    ///
    /// # Arguments
    /// - `namespace` (`&str`): der abzufragende Namensraum.
    ///
    /// # Returns
    /// `Some((provider_name, max_trust))`, wenn der Namensraum registriert
    /// wurde (über `context_provider` oder `context_provider_declared` —
    /// beide tragen seit diesem Knoten in dieselbe Tabelle ein), sonst
    /// `None`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_extension_api::registry::ExtensionRegistry;
    /// let registry = ExtensionRegistry::builder().build();
    /// assert_eq!(registry.context_provider_namespace("plan"), None);
    /// ```
    #[must_use]
    pub fn context_provider_namespace(&self, namespace: &str) -> Option<(&'static str, TrustClass)> {
        self.context_provider_namespaces.get(namespace).copied()
    }

    /// Wandelt eine fertige Registry zurück in einen [`ExtensionRegistryBuilder`].
    ///
    /// # Description
    /// Die Umkehrung von [`ExtensionRegistryBuilder::build`]: alle Provider
    /// (Werkzeuge, Kontext, Instructions, Freigabe-Handler, Turn-Observer,
    /// Spawner) sowie die vollständige Namensraum-Tabelle
    /// (`context_provider_namespaces`, siehe Moduldoku) wandern unverändert in
    /// den zurückgegebenen Builder. Das erlaubt es, eine bestehende, bereits
    /// gebaute Registry weiterzukomponieren (z. B. um sie um zusätzliche
    /// Provider zu erweitern, bevor sie erneut gebaut wird) ohne die
    /// Namensraum-Prüfung für die bereits registrierten Context-Provider zu
    /// wiederholen oder zu umgehen: ein weiterer `context_provider(..)`-Aufruf
    /// auf dem zurückgegebenen Builder prüft neue Provider weiterhin gegen die
    /// hier übernommenen, bereits beanspruchten Namensräume.
    ///
    /// # Returns
    /// Einen neuen `ExtensionRegistryBuilder`, der exakt den Zustand von
    /// `self` trägt.
    #[must_use]
    pub fn into_builder(self) -> ExtensionRegistryBuilder {
        ExtensionRegistryBuilder {
            tool_providers: self.tool_providers,
            context_providers: self.context_providers,
            instructions_providers: self.instructions_providers,
            approval_handlers: self.approval_handlers,
            turn_observers: self.turn_observers,
            spawner: self.spawner,
            context_provider_namespaces: self.context_provider_namespaces,
        }
    }
}

/// Ein `#[harw_macros::context_provider]`-erzeugter Typ implementiert dies
/// automatisch — Übergangs-Weg, solange `harw-macros` die neuen
/// `ContextProvider::namespace()`/`max_trust()`-Methoden noch nicht selbst
/// überschreibt (siehe Moduldoku, Abschnitt „Wieso zwei Registrierungsmethoden
/// bestehen bleiben").
///
/// # Description
/// [`ContextProvider::namespace`]/[`ContextProvider::max_trust`]
/// (`harw-extension-api/src/contributors.rs`) tragen inzwischen
/// Vorgabewerte, die für einen makro-erzeugten Typ **nicht** die tatsächlich
/// deklarierten Werte sind, solange `harw-macros` sie nicht selbst
/// überschreibt (außerhalb des Schreibbereichs dieses Knotens). Dieses
/// zusätzliche Trait bleibt deshalb der Weg, über den die tatsächlich
/// deklarierten Werte einen typgelöschten `Arc<dyn ContextProvider>`
/// überleben, bis diese Lücke in `harw-macros` geschlossen ist.
pub trait DeclaredContextProvider: ContextProvider {
    /// Der deklarierte Namensraum dieses Providers.
    ///
    /// # Returns
    /// Denselben Wert wie die generierte `Self::NAMESPACE`-Konstante.
    fn declared_namespace(&self) -> &'static str;

    /// Die deklarierte, höchste Vertrauensklasse dieses Providers.
    ///
    /// # Returns
    /// Denselben Wert wie die generierte `Self::TRUST`-Konstante.
    fn declared_max_trust(&self) -> TrustClass;
}

/// Fehler, die eine Context-Provider-Registrierung ablehnen.
///
/// # Description
/// Genau die zwei Prüfungen aus der Moduldoku, Abschnitt „Was tatsächlich
/// geprüft wird" — kein Fehlerfall für eine „zu hohe" Trust-Behauptung, weil
/// es dafür heute keinen unabhängigen Vergleichsmaßstab gibt (siehe dort).
/// Gemeinsamer Fehlertyp für [`ExtensionRegistryBuilder::context_provider`]
/// und [`ExtensionRegistryBuilder::context_provider_declared`] — beide
/// prüfen über denselben internen Weg.
#[derive(Debug, Clone, PartialEq, Eq, harw_macros::HarwError)]
pub enum ContextProviderRegistrationError {
    /// Der deklarierte Namensraum ist nach dem Trimmen leer.
    #[msg("context provider '{provider_name}' declares an empty or whitespace-only namespace")]
    NamespaceBlank {
        /// Der Provider mit dem leeren Namensraum.
        provider_name: &'static str,
    },

    /// Zwei Provider beanspruchen innerhalb desselben Builders denselben
    /// Namensraum.
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
}

#[derive(Default)]
pub struct ExtensionRegistryBuilder {
    tool_providers: Vec<Arc<dyn ToolProvider>>,
    context_providers: Vec<Arc<dyn ContextProvider>>,
    instructions_providers: Vec<Arc<dyn InstructionsProvider>>,
    approval_handlers: Vec<Arc<dyn ApprovalHandler>>,
    turn_observers: Vec<Arc<dyn TurnObserver>>,
    spawner: Option<Arc<dyn AgentSpawner>>,
    context_provider_namespaces: BTreeMap<String, (&'static str, TrustClass)>,
}

/// Zählt, statt zu dumpen.
///
/// Ein abgeleitetes `Debug` ist nicht möglich: der Bauer hält
/// `Arc<dyn Trait>`-Objekte, und `Debug` als Supertrait zu verlangen würde
/// jeden Implementierer zwingen, seine Innereien druckbar zu machen. Die
/// Zahlen beantworten die Frage, die man an einen Bauer stellt -- was ist
/// schon drin -- ohne Inhalte zu zeigen.
impl std::fmt::Debug for ExtensionRegistryBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtensionRegistryBuilder")
            .field("tool_providers", &self.tool_providers.len())
            .field("context_providers", &self.context_providers.len())
            .field("instructions_providers", &self.instructions_providers.len())
            .field("approval_handlers", &self.approval_handlers.len())
            .field("turn_observers", &self.turn_observers.len())
            .field("spawner", &self.spawner.is_some())
            .field("claimed_namespaces", &self.context_provider_namespaces.len())
            .finish()
    }
}

impl ExtensionRegistryBuilder {
    pub fn tool_provider(mut self, p: Arc<dyn ToolProvider>) -> Self {
        self.tool_providers.push(p);
        self
    }

    /// Prüft und verbucht einen Namensraum — der einzige Ort, an dem
    /// `context_provider_namespaces` beschrieben wird.
    ///
    /// # Description
    /// Geteilte Prüfung für [`Self::context_provider`] und
    /// [`Self::context_provider_declared`]: (1) der getrimmte `namespace`
    /// ist nicht leer; (2) `namespace` ist innerhalb dieses Builders noch
    /// nicht von einem anderen Provider beansprucht. `identity` ist der
    /// Menschen lesbare Bezeichner, der in Fehlermeldungen und in der
    /// Introspektionstabelle erscheint — für `context_provider_declared` der
    /// vom Aufrufer übergebene Name, für `context_provider` derselbe Wert
    /// wie `namespace` selbst (siehe dortige Doku).
    ///
    /// # Arguments
    /// - `identity` (`&'static str`): Bezeichner für Fehlermeldungen/Introspektion.
    /// - `namespace` (`&str`): der zu beanspruchende Namensraum, geliehen.
    /// - `max_trust` (`TrustClass`): die für diesen Namensraum verbuchte,
    ///   behauptete Vertrauensklasse.
    ///
    /// # Returns
    /// `Ok(())`, wenn beide Prüfungen bestehen und der Namensraum verbucht wurde.
    ///
    /// # Errors
    /// - [`ContextProviderRegistrationError::NamespaceBlank`] — `namespace`
    ///   ist nach dem Trimmen leer.
    /// - [`ContextProviderRegistrationError::NamespaceAlreadyClaimed`] — ein
    ///   anderer Provider hat `namespace` in diesem Builder bereits
    ///   beansprucht.
    fn claim_context_provider_namespace(
        &mut self,
        identity: &'static str,
        namespace: &str,
        max_trust: TrustClass,
    ) -> Result<(), ContextProviderRegistrationError> {
        let trimmed = namespace.trim();

        if trimmed.is_empty() {
            return Err(ContextProviderRegistrationError::NamespaceBlank {
                provider_name: identity,
            });
        }

        if let Some((existing_provider, _)) = self.context_provider_namespaces.get(trimmed) {
            return Err(ContextProviderRegistrationError::NamespaceAlreadyClaimed {
                namespace: trimmed.to_owned(),
                existing_provider,
                new_provider: identity,
            });
        }

        self.context_provider_namespaces
            .insert(trimmed.to_owned(), (identity, max_trust));
        Ok(())
    }

    /// Registriert einen Context-Provider **mit** Namensraum-Prüfung.
    ///
    /// # Description
    /// Liest `namespace()`/`max_trust()` direkt vom übergebenen Trait-Objekt
    /// (siehe [`ContextProvider::namespace`]/[`ContextProvider::max_trust`])
    /// und prüft sie über [`Self::claim_context_provider_namespace`] — dieselbe
    /// Prüfung wie [`Self::context_provider_declared`]. Das war vor diesem
    /// Knoten nicht möglich: `ContextProvider` kannte diese Methoden nicht,
    /// und ein bereits zu `Arc<dyn ContextProvider>` typgelöschter Wert trug
    /// die Information deshalb nirgends mit sich. Diese Methode ist damit ab
    /// sofort für **jeden** `ContextProvider` geprüft — es gibt keinen
    /// zweiten, ungeprüften Weg mehr.
    ///
    /// Ein Provider, der `namespace()`/`max_trust()` nicht überschreibt,
    /// bekommt die Vorgabewerte des Traits (Rust-Typname, `TrustClass::Data`)
    /// — nicht leer, registriert sich also weiterhin erfolgreich (siehe
    /// Moduldoku für die Ausnahme bei mehreren Instanzen desselben konkreten
    /// Typs).
    ///
    /// # Arguments
    /// - `p` (`Arc<dyn ContextProvider>`): der zu registrierende Provider.
    ///
    /// # Returns
    /// `Ok(Self)` mit `p` angehängt, wenn beide Prüfungen bestehen.
    ///
    /// # Errors
    /// - [`ContextProviderRegistrationError::NamespaceBlank`] — `p.namespace()`
    ///   ist nach dem Trimmen leer.
    /// - [`ContextProviderRegistrationError::NamespaceAlreadyClaimed`] — ein
    ///   anderer Provider hat `p.namespace()` in diesem Builder bereits
    ///   registriert.
    ///
    /// # Examples
    /// ```rust,ignore
    /// let builder = ExtensionRegistry::builder()
    ///     .context_provider(Arc::new(ProjectContextProvider::new(project)))?;
    /// ```
    pub fn context_provider(
        mut self,
        p: Arc<dyn ContextProvider>,
    ) -> Result<Self, ContextProviderRegistrationError> {
        let namespace = p.namespace();
        let max_trust = p.max_trust();
        self.claim_context_provider_namespace(namespace, namespace, max_trust)?;
        self.context_providers.push(p);
        Ok(self)
    }

    /// Registriert einen `#[harw_macros::context_provider]`-erzeugten
    /// Context-Provider **mit** Namensraum-Prüfung, unter Verwendung der
    /// tatsächlich deklarierten Werte statt der `ContextProvider`-Vorgabewerte.
    ///
    /// # Description
    /// Liest `namespace`/`max_trust` über [`DeclaredContextProvider`] aus `p`
    /// selbst (also aus derselben `#[context_provider(...)]`-Deklaration, die
    /// den Typ erzeugt hat) und prüft sie über
    /// [`Self::claim_context_provider_namespace`] — dieselbe Prüfung wie
    /// [`Self::context_provider`]. Solange `harw-macros` die neuen
    /// `ContextProvider::namespace()`/`max_trust()`-Methoden noch nicht
    /// selbst überschreibt (siehe Moduldoku), ist dies für makro-erzeugte
    /// Typen der einzige Weg, der die *tatsächlich deklarierten* Werte
    /// prüft statt der Vorgabewerte.
    ///
    /// # Arguments
    /// - `provider_name` (`&'static str`): menschenlesbarer Name für
    ///   Fehlermeldungen (typischerweise `P::NAME` oder
    ///   `std::any::type_name::<P>()`).
    /// - `p` (`Arc<P>`): der zu registrierende Provider.
    ///
    /// # Returns
    /// `Ok(Self)` mit `p` angehängt, wenn beide Prüfungen bestehen.
    ///
    /// # Errors
    /// - [`ContextProviderRegistrationError::NamespaceBlank`] — `p`s
    ///   deklarierter Namensraum ist leer oder nur Leerzeichen.
    /// - [`ContextProviderRegistrationError::NamespaceAlreadyClaimed`] — ein
    ///   anderer Provider hat diesen Namensraum in diesem Builder bereits
    ///   registriert.
    ///
    /// # Examples
    /// ```rust,ignore
    /// let builder = ExtensionRegistry::builder()
    ///     .context_provider_declared("ProjectContextProvider", Arc::new(ProjectContextProvider::new(project)))?;
    /// ```
    pub fn context_provider_declared<P>(
        mut self,
        provider_name: &'static str,
        p: Arc<P>,
    ) -> Result<Self, ContextProviderRegistrationError>
    where
        P: DeclaredContextProvider + 'static,
    {
        let namespace = p.declared_namespace();
        let max_trust = p.declared_max_trust();
        self.claim_context_provider_namespace(provider_name, namespace, max_trust)?;
        self.context_providers.push(p);
        Ok(self)
    }

    pub fn instructions_provider(mut self, p: Arc<dyn InstructionsProvider>) -> Self {
        self.instructions_providers.push(p);
        self
    }
    pub fn approval_handler(mut self, h: Arc<dyn ApprovalHandler>) -> Self {
        self.approval_handlers.push(h);
        self
    }

    /// Entfernt alle bisher registrierten Freigabe-Handler.
    ///
    /// Das ist für einen bekannten Montagepunkt gedacht, an dem eine
    /// mitgebrachte Standardpolitik durch eine vollständig konfigurierte
    /// Freigabekette ersetzt werden muss. Andere Registry-Bestandteile
    /// bleiben unverändert erhalten.
    #[must_use]
    pub fn clear_approval_handlers(mut self) -> Self {
        self.approval_handlers.clear();
        self
    }
    pub fn turn_observer(mut self, o: Arc<dyn TurnObserver>) -> Self {
        self.turn_observers.push(o);
        self
    }
    pub fn spawner(mut self, s: Arc<dyn AgentSpawner>) -> Self {
        self.spawner = Some(s);
        self
    }
    pub fn build(self) -> ExtensionRegistry {
        ExtensionRegistry {
            tool_providers: self.tool_providers,
            context_providers: self.context_providers,
            instructions_providers: self.instructions_providers,
            approval_handlers: self.approval_handlers,
            turn_observers: self.turn_observers,
            spawner: self.spawner,
            context_provider_namespaces: self.context_provider_namespaces,
        }
    }
}

pub fn empty_extension_registry() -> ExtensionRegistry {
    ExtensionRegistryBuilder::default().build()
}

#[cfg(test)]
mod tests {
    use crate::{ContextFragment, TurnInputContext};
    use super::*;
    use harw_tools::{FunctionToolSpec, JsonSchema, ToolExecutor, ToolName, ToolSpec};
    use std::future::Future;
    use std::pin::Pin;

    struct NamedToolProvider {
        tool_name: &'static str,
    }

    impl ToolProvider for NamedToolProvider {
        fn tools(&self) -> Vec<ToolSpec> {
            vec![ToolSpec::Function(FunctionToolSpec {
                name: ToolName::new(self.tool_name),
                description: format!("{} description", self.tool_name),
                parameters: JsonSchema::default(),
                strict: false,
            })]
        }

        fn executor(&self, _name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
            None
        }
    }

    #[test]
    fn add_tool_provider_appends_visible_provider_in_order() {
        let mut registry = ExtensionRegistry::builder()
            .tool_provider(Arc::new(NamedToolProvider {
                tool_name: "tools.initial",
            }))
            .build();

        registry.add_tool_provider(Arc::new(NamedToolProvider {
            tool_name: "tools.appended",
        }));

        let tool_names: Vec<_> = registry
            .tool_providers()
            .iter()
            .flat_map(|provider| provider.tools())
            .map(|tool| tool.name().to_owned())
            .collect();

        assert_eq!(
            tool_names,
            vec!["tools.initial".to_owned(), "tools.appended".to_owned()]
        );
    }

    /// Ein minimaler `DeclaredContextProvider` für die Registrierungstests
    /// unten, mit fest verdrahtetem Namensraum/Trust statt über das Makro
    /// erzeugt — dieses Modul testet die Prüfung in `registry.rs`, nicht die
    /// Makro-Expansion (die hat ihre eigenen Tests in
    /// `harw-macros/src/contributor.rs`). Überschreibt bewusst **auch**
    /// `ContextProvider::namespace`/`max_trust`, damit `context_provider`
    /// (nicht nur `context_provider_declared`) gegen dieselben,
    /// nachvollziehbaren Werte getestet werden kann.
    struct FixedDeclaredProvider {
        namespace: &'static str,
        max_trust: TrustClass,
    }

    impl ContextProvider for FixedDeclaredProvider {
        fn contribute<'a>(
            &'a self,
            _ctx: &'a TurnInputContext,
        ) -> Pin<Box<dyn Future<Output = Vec<ContextFragment>> + Send + 'a>> {
            Box::pin(async { Vec::new() })
        }

        fn namespace(&self) -> &'static str {
            self.namespace
        }

        fn max_trust(&self) -> TrustClass {
            self.max_trust
        }
    }

    impl DeclaredContextProvider for FixedDeclaredProvider {
        fn declared_namespace(&self) -> &'static str {
            self.namespace
        }
        fn declared_max_trust(&self) -> TrustClass {
            self.max_trust
        }
    }

    /// Ein Provider, der `ContextProvider::namespace`/`max_trust` **nicht**
    /// überschreibt — steht für „ein bestehender Anbieter ohne eigene
    /// Angaben" in den Tests unten.
    struct UndeclaredProvider;

    impl ContextProvider for UndeclaredProvider {
        fn contribute<'a>(
            &'a self,
            _ctx: &'a TurnInputContext,
        ) -> Pin<Box<dyn Future<Output = Vec<ContextFragment>> + Send + 'a>> {
            Box::pin(async { Vec::new() })
        }
    }

    #[test]
    fn context_provider_declared_accepts_distinct_namespaces() {
        let registry = ExtensionRegistry::builder()
            .context_provider_declared(
                "Plan",
                Arc::new(FixedDeclaredProvider {
                    namespace: "plan",
                    max_trust: TrustClass::Evidence,
                }),
            )
            .expect("plan namespace registers")
            .context_provider_declared(
                "Memory",
                Arc::new(FixedDeclaredProvider {
                    namespace: "memory",
                    max_trust: TrustClass::Data,
                }),
            )
            .expect("memory namespace registers")
            .build();

        assert_eq!(registry.context_providers().len(), 2);
        assert_eq!(
            registry.context_provider_namespace("plan"),
            Some(("Plan", TrustClass::Evidence))
        );
    }

    /// Kern der Nachzugsprüfung: ein zweiter Provider unter demselben
    /// Namensraum wird zur Laufzeit abgelehnt — kein stiller Vorrang, keine
    /// stille Überschreibung.
    #[test]
    fn context_provider_declared_rejects_duplicate_namespace() {
        let builder = ExtensionRegistry::builder()
            .context_provider_declared(
                "Plan",
                Arc::new(FixedDeclaredProvider {
                    namespace: "plan",
                    max_trust: TrustClass::Evidence,
                }),
            )
            .expect("first claim succeeds");

        let error = builder
            .context_provider_declared(
                "RoguePlan",
                Arc::new(FixedDeclaredProvider {
                    namespace: "plan",
                    max_trust: TrustClass::Instruction,
                }),
            )
            .expect_err("second claim of the same namespace must be rejected");

        match error {
            ContextProviderRegistrationError::NamespaceAlreadyClaimed {
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
    }

    /// Die abgelehnte Registrierung darf die bestehende nicht überschreiben:
    /// derselbe Ausgangszustand, erneut aufgebaut und dann unangetastet
    /// direkt zu `build()` durchgereicht (statt über die fehlgeschlagene
    /// zweite Registrierung, die `self` konsumiert und verwirft), muss
    /// weiterhin den ursprünglichen Provider zeigen.
    #[test]
    fn context_provider_declared_rejected_duplicate_does_not_overwrite_existing() {
        let registry = ExtensionRegistry::builder()
            .context_provider_declared(
                "Plan",
                Arc::new(FixedDeclaredProvider {
                    namespace: "plan",
                    max_trust: TrustClass::Evidence,
                }),
            )
            .expect("first claim succeeds")
            .build();

        assert_eq!(
            registry.context_provider_namespace("plan"),
            Some(("Plan", TrustClass::Evidence)),
            "the accepted first registration must be exactly what a rejected \
             duplicate registration would have tried to overwrite"
        );
    }

    #[test]
    fn context_provider_declared_rejects_blank_namespace() {
        let error = ExtensionRegistry::builder()
            .context_provider_declared(
                "Blank",
                Arc::new(FixedDeclaredProvider {
                    namespace: "   ",
                    max_trust: TrustClass::Data,
                }),
            )
            .expect_err("blank namespace must be rejected");
        assert!(matches!(
            error,
            ContextProviderRegistrationError::NamespaceBlank {
                provider_name: "Blank"
            }
        ));
    }

    /// Positivkontrolle, passend zur Moduldoku: eine `Instruction`-Behauptung
    /// wird nicht abgelehnt, weil es dafür keinen Vergleichsmaßstab gibt —
    /// die Prüfung lehnt ausschließlich Namensraum-Probleme ab.
    #[test]
    fn context_provider_declared_accepts_instruction_trust_without_ceiling_check() {
        let registry = ExtensionRegistry::builder()
            .context_provider_declared(
                "System",
                Arc::new(FixedDeclaredProvider {
                    namespace: "system",
                    max_trust: TrustClass::Instruction,
                }),
            )
            .expect("no ceiling exists to reject this claim")
            .build();
        assert_eq!(
            registry.context_provider_namespace("system"),
            Some(("System", TrustClass::Instruction))
        );
    }

    #[test]
    fn context_provider_namespace_is_none_for_unregistered_namespace() {
        let registry = ExtensionRegistry::builder().build();
        assert_eq!(registry.context_provider_namespace("plan"), None);
    }

    /// Zentraler Test dieses Knotens: es gibt keinen Registrierungsweg mehr,
    /// der die Namensraum-Kollisionsprüfung umgeht. `context_provider` nahm
    /// vor diesem Knoten jeden Wert kommentarlos an; hier weist es einen
    /// doppelt beanspruchten Namensraum exakt so ab wie
    /// `context_provider_declared` — inklusive Ablehnung eines Konflikts,
    /// der über den jeweils *anderen* Weg zuerst beansprucht wurde.
    #[test]
    fn context_provider_no_registration_path_bypasses_the_namespace_check() {
        // Erst über den (früher ungeprüften) `context_provider`-Weg.
        let builder = ExtensionRegistry::builder()
            .context_provider(Arc::new(FixedDeclaredProvider {
                namespace: "plan",
                max_trust: TrustClass::Evidence,
            }))
            .expect("first claim via context_provider succeeds");

        // Ein zweiter Provider versucht denselben Namensraum über
        // `context_provider` erneut zu beanspruchen: muss scheitern.
        let error_same_path = FixedDeclaredProvider {
            namespace: "plan",
            max_trust: TrustClass::Data,
        };
        let rejected = builder
            .context_provider(Arc::new(error_same_path))
            .expect_err("context_provider must reject a duplicate namespace, not just context_provider_declared");
        assert!(matches!(
            rejected,
            ContextProviderRegistrationError::NamespaceAlreadyClaimed { .. }
        ));

        // Auch der zweite, unabhängige Weg (`context_provider_declared`) darf
        // denselben, bereits über `context_provider` beanspruchten
        // Namensraum nicht mehr bekommen — beide Wege teilen sich denselben
        // Zustand.
        let builder = ExtensionRegistry::builder()
            .context_provider(Arc::new(FixedDeclaredProvider {
                namespace: "plan",
                max_trust: TrustClass::Evidence,
            }))
            .expect("first claim via context_provider succeeds");
        let cross_path_rejected = builder
            .context_provider_declared(
                "RoguePlan",
                Arc::new(FixedDeclaredProvider {
                    namespace: "plan",
                    max_trust: TrustClass::Instruction,
                }),
            )
            .expect_err("context_provider_declared must see the namespace claimed via context_provider");
        assert!(matches!(
            cross_path_rejected,
            ContextProviderRegistrationError::NamespaceAlreadyClaimed { .. }
        ));
    }

    /// `context_provider` lehnt jetzt auch einen leeren Namensraum ab —
    /// dieselbe Prüfung wie `context_provider_declared`.
    #[test]
    fn context_provider_rejects_blank_namespace() {
        let error = ExtensionRegistry::builder()
            .context_provider(Arc::new(FixedDeclaredProvider {
                namespace: "   ",
                max_trust: TrustClass::Data,
            }))
            .expect_err("context_provider must reject a blank namespace");
        assert!(matches!(
            error,
            ContextProviderRegistrationError::NamespaceBlank { .. }
        ));
    }

    /// Der Vorgabewert von `ContextProvider::max_trust` ist nachweislich die
    /// niedrigste Klasse: ein Provider, der ihn nicht überschreibt, landet
    /// in der Introspektion mit `TrustClass::Data`.
    #[test]
    fn context_provider_default_max_trust_is_the_lowest_class() {
        let registry = ExtensionRegistry::builder()
            .context_provider(Arc::new(UndeclaredProvider))
            .expect("undeclared provider registers under its default namespace")
            .build();

        let (_, max_trust) = registry
            .context_provider_namespace(UndeclaredProvider.namespace())
            .expect("default namespace must be registered");
        assert_eq!(
            max_trust,
            TrustClass::Data,
            "an undeclared provider must never default to a higher trust class"
        );
        // Über `trust_rank`, nicht über `<`: die Deklarationsreihenfolge von
        // `TrustClass` ergäbe eine abgeleitete Ordnung, die der gemeinten
        // entgegengesetzt ist. Genau deshalb trägt der Typ inzwischen kein
        // `Ord` mehr -- diese Zusicherung war hier zuvor falsch herum.
        assert!(
            TrustClass::Data.trust_rank() < TrustClass::Evidence.trust_rank()
                && TrustClass::Data.trust_rank() < TrustClass::Instruction.trust_rank(),
            "TrustClass::Data must be the lowest class for this default to be meaningful"
        );
    }

    /// Ein bestehender Anbieter ohne eigene Angaben (weder `namespace()` noch
    /// `max_trust()` überschrieben) registriert sich über `context_provider`
    /// weiterhin erfolgreich — die neue Prüfung bricht keinen Provider, der
    /// von der Ergänzung nichts weiß.
    #[test]
    fn context_provider_undeclared_provider_still_registers() {
        let registry = ExtensionRegistry::builder()
            .context_provider(Arc::new(UndeclaredProvider))
            .expect("a provider without its own namespace()/max_trust() must still register")
            .build();
        assert_eq!(registry.context_providers().len(), 1);
        assert!(
            !UndeclaredProvider.namespace().is_empty(),
            "the default namespace (the Rust type name) must never be blank"
        );
    }

    /// Der bestehende, deklarierte Registrierungsweg bleibt unverändert
    /// nutzbar, jetzt über dieselbe gemeinsame Prüfung wie `context_provider`.
    #[test]
    fn context_provider_declared_path_still_works() {
        let registry = ExtensionRegistry::builder()
            .context_provider_declared(
                "Fixed",
                Arc::new(FixedDeclaredProvider {
                    namespace: "fixed",
                    max_trust: TrustClass::Instruction,
                }),
            )
            .expect("declared registration succeeds")
            .build();
        assert_eq!(registry.context_providers().len(), 1);
        assert_eq!(
            registry.context_provider_namespace("fixed"),
            Some(("Fixed", TrustClass::Instruction))
        );
    }

    /// `into_builder` muss Provider und Namensräume unverändert erhalten --
    /// ein Roundtrip `build().into_builder().build()` darf keine registrierte
    /// Angabe verlieren oder verändern.
    #[test]
    fn into_builder_roundtrip_preserves_providers_and_namespaces() {
        let registry = ExtensionRegistry::builder()
            .tool_provider(Arc::new(NamedToolProvider {
                tool_name: "tools.roundtrip",
            }))
            .context_provider_declared(
                "Plan",
                Arc::new(FixedDeclaredProvider {
                    namespace: "plan",
                    max_trust: TrustClass::Evidence,
                }),
            )
            .expect("plan namespace registers")
            .build();

        let rebuilt = registry.into_builder().build();

        let tool_names: Vec<_> = rebuilt
            .tool_providers()
            .iter()
            .flat_map(|provider| provider.tools())
            .map(|tool| tool.name().to_owned())
            .collect();
        assert_eq!(tool_names, vec!["tools.roundtrip".to_owned()]);
        assert_eq!(rebuilt.context_providers().len(), 1);
        assert_eq!(
            rebuilt.context_provider_namespace("plan"),
            Some(("Plan", TrustClass::Evidence)),
            "into_builder must carry the namespace claim table through unchanged"
        );
    }

    /// `into_builder` erlaubt es, weitere Provider anzuhängen, bevor erneut
    /// gebaut wird -- und die Namensraum-Prüfung wirkt dabei weiterhin gegen
    /// die aus der ursprünglichen Registry übernommenen Namensräume.
    #[test]
    fn into_builder_still_enforces_namespace_check_for_new_providers() {
        let registry = ExtensionRegistry::builder()
            .context_provider_declared(
                "Plan",
                Arc::new(FixedDeclaredProvider {
                    namespace: "plan",
                    max_trust: TrustClass::Evidence,
                }),
            )
            .expect("plan namespace registers")
            .build();

        let error = registry
            .into_builder()
            .context_provider_declared(
                "RoguePlan",
                Arc::new(FixedDeclaredProvider {
                    namespace: "plan",
                    max_trust: TrustClass::Instruction,
                }),
            )
            .expect_err("the namespace claim carried over from the built registry must still be enforced");

        assert!(matches!(
            error,
            ContextProviderRegistrationError::NamespaceAlreadyClaimed { .. }
        ));
    }
}
