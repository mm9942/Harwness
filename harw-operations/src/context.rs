//! Ausführungs-Kontext für Harness-Operationen.
//!
//! Dieses Modul definiert [`OpContext`] — den unveränderlichen, server-seitig
//! erzeugten Ausführungskontext, der bei jedem [`Operation::run`]-Aufruf
//! übergeben wird — sowie [`ServiceMap`], einen getypten Service-Container.
//!
//! # Verantwortungsbereich
//! - Stellt Session-ID, Turn-ID und Sandbox-Spec (Authority-Boundary) bereit.
//! - Ermöglicht typsicheren Zugriff auf optionale Laufzeit-Services via [`ServiceMap`].
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
//! use harw_types::{SessionId, TurnId};
//!
//! // Kontext wird vom Executor erzeugt, nicht von der Operation selbst.
//! let _ = OpContext::new; // Konstruktor existiert; Sandbox erfordert echte Verzeichnisse.
//! ```

use std::any::{Any, TypeId};
use std::collections::HashMap;

use harw_sandbox::SandboxSpec;
use harw_types::{SessionId, TurnId};

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
        }
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
