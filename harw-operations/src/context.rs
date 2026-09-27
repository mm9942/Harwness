//! Ausführungs-Kontext für Harness-Operationen.
//!
//! Dieses Modul definiert [`OpContext`] — den unveränderlichen, server-seitig
//! erzeugten Ausführungskontext, der bei jedem [`Operation::run`]-Aufruf
//! übergeben wird — sowie [`ServiceMap`], einen getypten Service-Container.
//!
//! # Verantwortungsbereich
//! - Stellt Session-ID, Turn-ID und Sandbox-Spec (Authority-Boundary) bereit.
//! - Ermöglicht typsicheren Zugriff auf optionale Laufzeit-Services via [`ServiceMap`].
//! - Trägt optional den [`harw_types::cancel::CancelToken`] des laufenden Turns
//!   ([`OpContext::with_cancel_token`]/[`OpContext::cancel_token`]), damit
//!   Operationen (z. B. ein wartendes Kind-Spawn in `harw-core-bridge`) einen
//!   Turn-Abbruch beobachten können, ohne selbst von `harw-core` abzuhängen.
//! - Trägt optional den **Mandanten-Scope** des Aufrufers
//!   ([`OpContext::with_tenant`]/[`OpContext::tenant`]) und eine
//!   nicht-autoritative [`SecurityContextSummary`]
//!   ([`OpContext::with_security_context_summary`]) — beide setzt
//!   ausschließlich der serverseitige Kontextbau (z. B. `harw-web` aus der
//!   aufgelösten Peer-Identität, Masterplan v2 §14/§15, H12), nie der
//!   Request-Rumpf. Operationen, die mandantengebundene Daten auflisten,
//!   filtern mit [`OpContext::tenant_admits`] bzw. [`tenant_admits`].
//! - Erzeugt selbst KEINE Authority — Authority stammt ausschließlich vom Executor.
//!
//! # Schlüsseltypen
//! - [`ServiceMap`] — getypter Service-Container (intern `HashMap<TypeId, Box<dyn Any + Send + Sync>>`)
//! - [`OpContext`] — unveränderlicher Ausführungs-Kontext einer Operation
//!
//! # Nebenläufigkeit
//! [`OpContext`] ist `Send + Sync` (alle Felder sind es). [`ServiceMap`] ist
//! `Send + Sync`, weil alle enthaltenen Services `Send + Sync` sein müssen.
//!
//! # Fehlertypen
//! Dieses Modul erzeugt keine Fehler. [`ServiceMap::get`] und [`OpContext::service`]
//! geben `Option<&S>` zurück — kein `Result`.
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_operations::context::{OpContext, ServiceMap};
//! use harw_types::{SecurityContextSummary, SessionId, TenantId, TurnId};
//!
//! // Kontext wird vom Executor erzeugt, nicht von der Operation selbst.
//! let _ = OpContext::new; // Konstruktor existiert; Sandbox erfordert echte Verzeichnisse.
//! ```

use std::any::{Any, TypeId};
use std::collections::HashMap;

use harw_authority::SandboxSpec;
use harw_types::cancel::CancelToken;
use harw_types::{SecurityContextSummary, SessionId, TenantId, TurnId};

// ── ServiceMap ────────────────────────────────────────────────────────────────

/// Getypter Service-Container für optionale Laufzeit-Services.
///
/// # Beschreibung
/// Intern eine `HashMap<TypeId, Box<dyn Any + Send + Sync>>`. Der Zugriff
/// erfolgt über den Rust-Typ des gespeicherten Wertes — kein String-Schlüssel,
/// kein Casting auf der Aufrufseite.
///
/// # Nebenläufigkeit
/// `ServiceMap` ist `Send + Sync`, weil alle enthaltenen Services
/// `Send + Sync` sein müssen (Anforderung von [`Self::insert`]).
///
/// # Beispiel
/// ```rust
/// use harw_operations::context::ServiceMap;
///
/// struct MyService { value: u32 }
///
/// let mut map = ServiceMap::new();
/// map.insert(MyService { value: 42 });
/// assert_eq!(map.get::<MyService>().unwrap().value, 42);
/// assert!(map.get::<String>().is_none());
/// ```
#[derive(Default)]
pub struct ServiceMap {
    inner: HashMap<TypeId, Box<dyn Any + Send + Sync>>,
}

impl ServiceMap {
    /// Erstellt eine leere `ServiceMap`.
    ///
    /// # Rückgabe
    /// Eine neue, leere `ServiceMap` ohne registrierte Services.
    ///
    /// # Beispiel
    /// ```rust
    /// use harw_operations::context::ServiceMap;
    /// let map = ServiceMap::new();
    /// assert!(map.get::<String>().is_none());
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: HashMap::new(),
        }
    }

    /// Fügt einen Service vom Typ `S` in die Map ein.
    ///
    /// # Argumente
    /// - `service` (`S`): Der einzufügende Service-Wert. Wird via `TypeId::of::<S>()` indiziert.
    ///   Ein zuvor eingefügter Service desselben Typs wird überschrieben.
    ///
    /// # Nebenläufigkeit
    /// `S` muss `Any + Send + Sync` sein, damit die Map selbst `Send + Sync` bleibt.
    ///
    /// # Beispiel
    /// ```rust
    /// use harw_operations::context::ServiceMap;
    /// let mut map = ServiceMap::new();
    /// map.insert(42_u32);
    /// assert_eq!(*map.get::<u32>().unwrap(), 42);
    /// ```
    pub fn insert<S: Any + Send + Sync>(&mut self, service: S) {
        self.inner.insert(TypeId::of::<S>(), Box::new(service));
    }

    /// Gibt eine Referenz auf den Service vom Typ `S` zurück, falls vorhanden.
    ///
    /// # Rückgabe
    /// - `Some(&S)`: Service vom Typ `S` ist registriert.
    /// - `None`: Kein Service dieses Typs registriert.
    ///
    /// # Beschreibung
    /// Führt einen Downcast via [`Any::downcast_ref`] durch — keine `unsafe`-Aufrufe.
    ///
    /// # Beispiel
    /// ```rust
    /// use harw_operations::context::ServiceMap;
    /// let mut map = ServiceMap::new();
    /// map.insert("hello".to_owned());
    /// assert_eq!(map.get::<String>().map(String::as_str), Some("hello"));
    /// assert!(map.get::<u32>().is_none());
    /// ```
    #[must_use]
    pub fn get<S: Any + Send + Sync>(&self) -> Option<&S> {
        self.inner
            .get(&TypeId::of::<S>())
            .and_then(|boxed| boxed.downcast_ref::<S>())
    }
}

// ── OpContext ─────────────────────────────────────────────────────────────────

/// Unveränderlicher Ausführungs-Kontext, der bei jedem [`Operation::run`]-Aufruf
/// übergeben wird.
///
/// # Beschreibung
/// `OpContext` bündelt alle server-seitig erzeugten Informationen, die eine
/// Operation zur Ausführung benötigt:
///
/// - **`session_id`**: Identifiziert die aktive Agent-Sitzung.
/// - **`turn_id`**: Identifiziert den aktuellen Request-Response-Zyklus.
/// - **`sandbox`**: Die Authority-Boundary (Dateisystem-Wurzel, Berechtigungen).
///   Executors (Datei, Prozess, MCP) MÜSSEN `sandbox()` als Syscall-Boundary
///   verwenden und dürfen Workspace- oder Berechtigungsdaten NICHT aus der
///   `OpInput` übernehmen.
/// - **`services`**: Optionale Laufzeit-Services (z. B. HTTP-Client, DB-Pool).
/// - **`cancel`**: Optionaler [`CancelToken`] des laufenden Turns
///   ([`Self::with_cancel_token`]/[`Self::cancel_token`]). `None`, solange
///   der Aufrufer keinen gesetzt hat (z. B. der One-Shot-CLI-Pfad ohne
///   `TurnControl`) — Operationen, die auf einen Abbruch reagieren wollen
///   (etwa ein wartendes Kind-Spawn), müssen dann selbst auf einen frischen,
///   nie abgebrochenen Token zurückfallen.
///
/// # Wichtiger Hinweis zur Authority
/// `OpContext` **erzeugt selbst KEINE Authority**. Die in `sandbox` codierten
/// Grenzen werden vom Executor vor dem `run`-Aufruf gesetzt. Services, die
/// über [`Self::service`] abgerufen werden, dürfen die durch `sandbox`
/// gesetzten Grenzen **nicht umgehen**. Jeder Service, der erweiterte
/// Berechtigungen benötigt, muss dies explizit über `sandbox` aushandeln.
///
/// # Nebenläufigkeit
/// `OpContext` ist `Send + Sync` — alle Felder implementieren `Send + Sync`.
/// Operationen dürfen den Kontext über `Arc`-Grenzen teilen, müssen ihn aber
/// niemals mutieren.
///
/// # Fehlertypen
/// Dieses Struct erzeugt keine Fehler. Service-Zugriff gibt `Option<&S>` zurück.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_operations::context::{OpContext, ServiceMap};
/// use harw_types::{SessionId, TurnId};
///
/// // SandboxSpec erfordert echte Verzeichnisse — nur in Integrationstest-Kontext nutzbar.
/// // let ctx = OpContext::new(SessionId::new(), TurnId::new(), sandbox, ServiceMap::new());
/// ```
pub struct OpContext {
    session_id: SessionId,
    turn_id: TurnId,
    sandbox: SandboxSpec,
    services: ServiceMap,
    cancel: Option<CancelToken>,
    tenant: Option<TenantId>,
    security_context: Option<SecurityContextSummary>,
}

impl OpContext {
    /// Erstellt einen neuen `OpContext` mit den angegebenen Werten.
    ///
    /// # Argumente
    /// - `session_id` (`SessionId`): Aktive Session-ID.
    /// - `turn_id` (`TurnId`): Aktueller Turn.
    /// - `sandbox` (`SandboxSpec`): Eingefrorene Authorisierung (Workspace + Berechtigungen).
    /// - `services` (`ServiceMap`): Optionale Laufzeit-Services.
    ///
    /// # Rückgabe
    /// Ein neuer, unveränderlicher `OpContext`.
    ///
    /// # Nebenläufigkeit
    /// Kann sicher in Multi-Thread-Kontexten verwendet werden (alle Felder `Send + Sync`).
    ///
    /// # Beispiel
    /// ```rust,no_run
    /// use harw_operations::context::{OpContext, ServiceMap};
    /// use harw_types::{SessionId, TurnId};
    /// // sandbox muss vom Executor geliefert werden
    /// ```
    #[must_use]
    pub fn new(
        session_id: SessionId,
        turn_id: TurnId,
        sandbox: SandboxSpec,
        services: ServiceMap,
    ) -> Self {
        Self {
            session_id,
            turn_id,
            sandbox,
            services,
            cancel: None,
            tenant: None,
            security_context: None,
        }
    }

    /// Bindet diesen Kontext an einen Mandanten (Builder, H12).
    ///
    /// # Beschreibung
    /// Der Mandant stammt ausschließlich aus serverseitig vertrauter
    /// Identitätsauflösung (in `harw-web`: `ResolvedPeer::tenant`, aus
    /// `SO_PEERCRED` + Konfiguration bzw. einem vom SecurityHub bestätigten
    /// Kontext) — **nie** aus `OpInput` oder HTTP-Rumpf/-Headern. Ohne
    /// diesen Aufruf bleibt [`Self::tenant`] `None` (Einzelnutzer-Betrieb,
    /// bisheriges Verhalten).
    ///
    /// # Argumente
    /// - `tenant` (`TenantId`): der aufgelöste Mandant des Aufrufers.
    ///
    /// # Rückgabe
    /// `Self` mit `tenant() == Some(&tenant)`.
    #[must_use]
    pub fn with_tenant(mut self, tenant: TenantId) -> Self {
        self.tenant = Some(tenant);
        self
    }

    /// Hängt die nicht-autoritative Zusammenfassung des vom SecurityHub
    /// bestätigten Sicherheitskontexts an (Builder, H12).
    ///
    /// # Beschreibung
    /// Nur zur Anzeige, Protokollierung und Korrelation. Eine Operation darf
    /// daraus **keine** Autorisierungsentscheidung ableiten (siehe
    /// `harw_types::security`-Moduldoku); der verbindliche Mandanten-Scope
    /// ist [`Self::tenant`].
    #[must_use]
    pub fn with_security_context_summary(mut self, summary: SecurityContextSummary) -> Self {
        self.security_context = Some(summary);
        self
    }

    /// Der Mandanten-Scope des Aufrufers, falls gesetzt.
    ///
    /// # Rückgabe
    /// - `Some(&TenantId)`: der Aufrufer ist an diesen Mandanten gebunden;
    ///   mandantengebundene Listen müssen darauf filtern
    ///   ([`Self::tenant_admits`]).
    /// - `None`: kein Mandanten-Scope (Einzelnutzer-Betrieb).
    #[must_use]
    pub fn tenant(&self) -> Option<&TenantId> {
        self.tenant.as_ref()
    }

    /// Die angehängte [`SecurityContextSummary`], falls vorhanden
    /// (nicht-autoritativ, siehe [`Self::with_security_context_summary`]).
    #[must_use]
    pub fn security_context_summary(&self) -> Option<&SecurityContextSummary> {
        self.security_context.as_ref()
    }

    /// Darf der Aufrufer dieses Kontexts ein Element mit Mandant
    /// `item_tenant` sehen? Siehe [`tenant_admits`] für die Regel.
    #[must_use]
    pub fn tenant_admits(&self, item_tenant: Option<&TenantId>) -> bool {
        tenant_admits(self.tenant(), item_tenant)
    }

    /// Hängt den [`CancelToken`] des laufenden Turns an diesen Kontext (Builder).
    ///
    /// # Beschreibung
    /// Konsumiert `self` und gibt es mit gesetztem Cancel-Token zurück, damit
    /// Aufrufer es direkt an `OpContext::new(...)` anhängen können. Ohne
    /// diesen Aufruf bleibt [`Self::cancel_token`] `None` — bestehende
    /// Konstruktions-Aufrufe bleiben also unverändert gültig.
    ///
    /// # Argumente
    /// - `cancel` (`CancelToken`): der Cancel-Token des Turns, dessen Abbruch
    ///   diese Operation beobachten soll (siehe `harw-core::turn_loop::TurnControl::cancel_token`).
    ///
    /// # Rückgabe
    /// `Self` mit `cancel_token() == Some(&cancel)`.
    #[must_use]
    pub fn with_cancel_token(mut self, cancel: CancelToken) -> Self {
        self.cancel = Some(cancel);
        self
    }

    /// Gibt die Session-ID zurück.
    ///
    /// # Rückgabe
    /// Referenz auf die [`SessionId`] dieser Session.
    #[must_use]
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// Gibt die Turn-ID zurück.
    ///
    /// # Rückgabe
    /// Referenz auf die [`TurnId`] dieses Request-Response-Zyklus.
    #[must_use]
    pub fn turn_id(&self) -> &TurnId {
        &self.turn_id
    }

    /// Gibt die Sandbox-Spec zurück.
    ///
    /// # Rückgabe
    /// Referenz auf die eingefrorene [`SandboxSpec`], die Workspace und Berechtigungen enthält.
    ///
    /// # Hinweis
    /// Executors MÜSSEN diese Boundary als Syscall-Grenze verwenden. Services aus
    /// [`Self::service`] dürfen diese Grenze nicht umgehen.
    #[must_use]
    pub fn sandbox(&self) -> &SandboxSpec {
        &self.sandbox
    }

    /// Gibt den [`CancelToken`] des laufenden Turns zurück, falls einer gesetzt ist.
    ///
    /// # Rückgabe
    /// - `Some(&CancelToken)`: der Aufrufer hat einen über
    ///   [`Self::with_cancel_token`] angehängt (der Regelfall während eines
    ///   Modell-Turns, siehe die Fabrik in
    ///   `harw-runtime::assembly::install_operation_model_tools`).
    /// - `None`: kein Turn-Cancel-Token verfügbar (z. B. der One-Shot-CLI-Pfad
    ///   ohne `TurnControl`, oder ein Test-Fixture). Operationen, die einen
    ///   Abbruchpfad brauchen, müssen dann selbst auf einen frischen, nie
    ///   abgebrochenen [`CancelToken`] zurückfallen — nicht fail-closed
    ///   ablehnen, da ein fehlender Cancel-Token kein Autoritätsfehler ist.
    #[must_use]
    pub fn cancel_token(&self) -> Option<&CancelToken> {
        self.cancel.as_ref()
    }

    /// Gibt eine Referenz auf den Service vom Typ `S` zurück, falls registriert.
    ///
    /// # Rückgabe
    /// - `Some(&S)`: Service vom Typ `S` ist in der `ServiceMap` registriert.
    /// - `None`: Kein Service dieses Typs vorhanden.
    ///
    /// # Argumente
    /// Der Typ `S` muss `Any + Send + Sync` implementieren.
    ///
    /// # Beispiel
    /// ```rust,no_run
    /// use harw_operations::context::{OpContext, ServiceMap};
    /// // let svc: Option<&MyService> = ctx.service::<MyService>();
    /// ```
    #[must_use]
    pub fn service<S: Any + Send + Sync>(&self) -> Option<&S> {
        self.services.get::<S>()
    }

    // Hinweis: `managed_spawner()` und `state_store()` sind in Strang 3 in das
    // `harw-core-bridge`-Crate ausgelagert (Extension-Trait `OpContextCoreExt`),
    // damit `harw-operations` core-frei bleibt.
}

/// Die eine Filterregel für mandantengebundene Listen (H12, Masterplan v2 §15).
///
/// # Beschreibung
/// - Aufrufer **ohne** Mandanten-Scope (`scope == None`, Einzelnutzer-Betrieb)
///   sieht alles — das bisherige Verhalten bleibt unverändert.
/// - Aufrufer **mit** Mandanten-Scope sieht nur Elemente genau seines
///   Mandanten. Elemente ohne Mandant (Altbestand) sind für ihn
///   **unsichtbar** (fail-closed): „gehört niemandem" ist nicht „gehört mir".
///
/// # Beispiel
/// ```rust
/// use harw_operations::context::tenant_admits;
/// use harw_types::TenantId;
///
/// let a = TenantId::from_str("tenant-a");
/// let b = TenantId::from_str("tenant-b");
/// assert!(tenant_admits(None, Some(&a)));
/// assert!(tenant_admits(Some(&a), Some(&a)));
/// assert!(!tenant_admits(Some(&a), Some(&b)));
/// assert!(!tenant_admits(Some(&a), None));
/// ```
#[must_use]
pub fn tenant_admits(scope: Option<&TenantId>, item_tenant: Option<&TenantId>) -> bool {
    match scope {
        None => true,
        Some(scope) => item_tenant == Some(scope),
    }
}

#[cfg(test)]
mod tenant_scope_tests {
    use super::tenant_admits;
    use harw_types::TenantId;

    #[test]
    fn test_unscoped_caller_sees_every_item() {
        let a = TenantId::from_str("tenant-a");
        assert!(tenant_admits(None, Some(&a)));
        assert!(tenant_admits(None, None));
    }

    #[test]
    fn test_scoped_caller_sees_only_own_tenant() {
        let a = TenantId::from_str("tenant-a");
        let b = TenantId::from_str("tenant-b");
        assert!(tenant_admits(Some(&a), Some(&a)));
        assert!(!tenant_admits(Some(&a), Some(&b)));
        assert!(!tenant_admits(Some(&b), Some(&a)));
    }

    #[test]
    fn test_scoped_caller_does_not_see_untenanted_items() {
        let a = TenantId::from_str("tenant-a");
        assert!(!tenant_admits(Some(&a), None));
    }
}
