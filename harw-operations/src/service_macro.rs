//! Deklarative Makros gegen wiederkehrendes Boilerplate in Operation-Implementierungen.
//!
//! # Verantwortungsbereich
//! Dieses Modul definiert zwei Muster, die in Operation-Implementierungen
//! (`#[operation]`-annotierte Funktionen und ihre Registrierung) sonst
//! wortreich wiederholt werden müssten:
//!
//! - [`require_service!`]: Holt einen Service aus [`crate::context::OpContext`]
//!   (via [`crate::context::OpContext::service`]) oder liefert früh einen
//!   typisierten [`crate::error::OpError::NotAvailable`]-Fehler zurück.
//! - [`operations!`] / [`try_operations!`]: Registrieren eine Liste von
//!   Operationen in einer [`crate::registry::OperationRegistry`] — infallible
//!   (Panic bei Kollision, wie [`crate::registry::OperationRegistry::register`])
//!   bzw. fallible mit `?`-Propagation (wie
//!   [`crate::registry::OperationRegistry::try_register`]).
//!
//! # Schlüsseltypen
//! Keine neuen Typen — nur `macro_rules!`-Makros, die über `#[macro_export]`
//! am Crate-Root verfügbar sind (`harw_operations::require_service!` usw.).
//!
//! # Nebenläufigkeit
//! Reine Textexpansion zur Kompilierzeit; die Makros selbst haben kein
//! Laufzeitverhalten jenseits dessen, was der expandierte Code ausführt
//! (siehe [`crate::context::OpContext`] und [`crate::registry::OperationRegistry`]
//! für deren jeweilige Nebenläufigkeits-Eigenschaften).
//!
//! # Fehlertypen
//! - [`require_service!`] expandiert zu einem `?`-Ausdruck, der bei fehlendem
//!   Service [`crate::error::OpError::NotAvailable`] zurückgibt.
//! - [`try_operations!`] expandiert zu `?`-Ausdrücken, die
//!   [`crate::registry::RegistryError`] propagieren.
//! - [`operations!`] propagiert keinen Fehler — Kollisionen paniken (siehe
//!   [`crate::registry::OperationRegistry::register`]).
//!
//! # Design-Doc-Referenz
//! AP W1-26f, Teil 2 (`harw-operations/src/service_macro.rs`).

/// Holt einen Service aus der ServiceMap oder liefert einen typisierten
/// `NotAvailable`-Fehler.
///
/// # Grammatik
/// ```ignore
/// require_service!($ctx:expr, $ty:ty, $label:expr)
/// ```
///
/// # Erzeugter Code
/// ```ignore
/// $ctx.service::<$ty>().ok_or_else(|| {
///     $crate::OpError::NotAvailable(format!(
///         "kein {} in diesem Kontext konfiguriert",
///         $label
///     ))
/// })?
/// ```
///
/// `$ctx` muss eine Methode `service::<S>(&self) -> Option<&S>` besitzen (wie
/// [`crate::context::OpContext::service`]). Das Makro endet auf ein
/// `?`-Ausdruck und ist daher nur in Funktionen verwendbar, deren
/// Rückgabetyp `Result<_, E>` mit `E: From<`[`crate::error::OpError`]`>` ist
/// (i. d. R. direkt `E = `[`crate::error::OpError`]).
///
/// # Fehlerfälle
/// - Kein Service vom angeforderten Typ `$ty` registriert: gibt früh
///   `Err(OpError::NotAvailable(format!("kein {label} in diesem Kontext konfiguriert")))`
///   zurück, wobei `label` der ausgewertete `$label`-Ausdruck ist.
///
/// # Beispiel
/// ```ignore
/// use std::sync::Arc;
/// use harw_operations::{require_service, OpContext, OpError};
///
/// fn spawner<'a>(ctx: &'a OpContext) -> Result<&'a Arc<ManagedAgentSpawner>, OpError> {
///     let spawner = require_service!(ctx, Arc<ManagedAgentSpawner>, "Agent-Spawner");
///     Ok(spawner)
/// }
/// ```
#[macro_export]
macro_rules! require_service {
    // Form 1 — Zugriff über die untypisierte `ServiceMap`.
    ($ctx:expr, $ty:ty, $label:expr) => {
        $ctx.service::<$ty>().ok_or_else(|| {
            $crate::OpError::NotAvailable(::std::format!(
                "kein {} in diesem Kontext konfiguriert",
                $label
            ))
        })?
    };
    // Form 2 — Zugriff über einen bereits typisierten Getter.
    //
    // Sie existiert, weil die tatsächlichen Konsumenten nicht `service::<T>()`
    // benutzen, sondern Extension-Traits mit typisierten Gettern
    // (`OpContextPlanExt::plan_store()` und Geschwister). Das ist die bessere
    // Bauart: der Getter kennt den Typ, der Aufrufer muss ihn nicht nennen.
    //
    // Wiederholt wird dort nur noch der Fehlerzweig — dieselbe Meldung an über
    // einem Dutzend Stellen. Genau den nimmt diese Form ab:
    //
    // ```ignore
    // let store = require_service!(ctx.plan_store(), "Plan-Store");
    // ```
    ($option:expr, $label:expr) => {
        $option.ok_or_else(|| {
            $crate::OpError::NotAvailable(::std::format!(
                "kein {} in diesem Kontext konfiguriert",
                $label
            ))
        })?
    };
}

/// Registriert eine Liste von Operationen in einer `OperationRegistry`.
///
/// Infallible — panikt bei Namens-/Alias-Kollisionen, genau wie
/// [`crate::registry::OperationRegistry::register`], dessen `require_service!`-
/// artiges Gegenstück dieses Makro für die Registrierungsseite ist. Für
/// kollisionssichere Registrierung siehe [`try_operations!`].
///
/// # Wann es sich **nicht** lohnt
/// `harw_ops::register_all` und `register_plan_tools` benutzen bewusst ein
/// Array-Literal fester Länge (`[Arc<dyn Operation>; 19]`) statt dieses Makros.
/// Der Grund ist die Compile-Zeit-Prüfung: wer eine Operation ergänzt, ohne die
/// Länge anzupassen, bekommt einen Fehler. Dieses Makro nimmt diese Prüfung weg
/// — es akzeptiert jede Anzahl. Für eine Liste, deren Vollständigkeit zählt,
/// ist das Array also die bessere Wahl, und das Makro bleibt dort ungenutzt.
///
/// Sinnvoll ist es, wo die Anzahl offen ist: dynamisch zusammengestellte
/// Registrierungen, Erweiterungs-Crates, Tests mit wechselnden Op-Mengen.
///
/// # Grammatik
/// ```ignore
/// operations![$registry:expr; $($op:expr),* $(,)?]
/// ```
///
/// # Erzeugter Code
/// ```ignore
/// $( $registry.register(::std::sync::Arc::new($op)); )*
/// ```
///
/// Jedes `$op` muss ein Ausdruck sein, dessen Typ
/// [`crate::operation::Operation`] implementiert (üblicherweise eine von
/// `#[operation]` erzeugte Unit-Struct). `Arc::new($op)` wird implizit zu
/// `Arc<dyn Operation>` unsized-coerced — kein `as`-Cast nötig.
///
/// # Fehlerfälle
/// Keine — Kollisionen führen zum `panic!` in
/// [`crate::registry::OperationRegistry::register`].
///
/// # Beispiel
/// ```ignore
/// use harw_operations::{operations, OperationRegistry};
///
/// let mut registry = OperationRegistry::new();
/// operations![registry; HelpOperation, StatusOperation, QuitOperation];
/// assert_eq!(registry.len(), 3);
/// ```
#[macro_export]
macro_rules! operations {
    ($registry:expr; $( $op:expr ),* $(,)?) => {
        $( $registry.register(::std::sync::Arc::new($op)); )*
    };
}

/// Fallible Variante von [`operations!`]: propagiert Registrierungs-Kollisionen
/// über `?` statt zu paniken.
///
/// Spiegelt [`crate::registry::OperationRegistry::try_register`] — für jede
/// Op wird `try_register` aufgerufen und ein `Err(RegistryError)` sofort nach
/// oben weitergereicht.
///
/// # Grammatik
/// ```ignore
/// try_operations![$registry:expr; $($op:expr),* $(,)?]
/// ```
///
/// # Erzeugter Code
/// ```ignore
/// $( $registry.try_register(::std::sync::Arc::new($op))?; )*
/// ```
///
/// Wie [`require_service!`] endet jede Expansion auf einen `?`-Ausdruck; die
/// umschließende Funktion muss daher `Result<_, E>` mit
/// `E: From<`[`crate::registry::RegistryError`]`>` zurückgeben.
///
/// # Fehlerfälle
/// - [`crate::registry::RegistryError::DuplicateName`],
///   [`crate::registry::RegistryError::AliasCollision`] oder
///   [`crate::registry::RegistryError::SelfCollision`]: propagiert vom ersten
///   `$op`, dessen Registrierung kollidiert; nachfolgende `$op`-Einträge
///   werden dann nicht mehr registriert.
///
/// # Beispiel
/// ```ignore
/// use harw_operations::{try_operations, OperationRegistry};
/// use harw_operations::registry::RegistryError;
///
/// fn build_registry() -> Result<OperationRegistry, RegistryError> {
///     let mut registry = OperationRegistry::new();
///     try_operations![registry; HelpOperation, StatusOperation];
///     Ok(registry)
/// }
/// ```
#[macro_export]
macro_rules! try_operations {
    ($registry:expr; $( $op:expr ),* $(,)?) => {
        $( $registry.try_register(::std::sync::Arc::new($op))?; )*
    };
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use crate::context::ServiceMap;
    use crate::error::OpError;
    use crate::operation::{
        OpFuture, OpInput, Operation, OperationCategory, OperationDomain, OperationMeta,
        PermissionTier,
    };
    use crate::registry::{OperationRegistry, RegistryError};
    use std::any::Any;
    use std::sync::OnceLock;

    // ── Fixtures für `require_service!` ───────────────────────────────────────

    /// Dummy-Service mit einem Prüfwert.
    struct DummyService {
        value: u32,
    }

    /// Minimaler Stand-in für `OpContext`: bietet exakt die von
    /// `require_service!` benötigte `service::<S>()`-Methode, ohne die
    /// Dateisystem-/Sandbox-Maschinerie eines echten `OpContext` aufzubauen.
    struct DummyCtx {
        services: ServiceMap,
    }

    impl DummyCtx {
        fn service<S: Any + Send + Sync>(&self) -> Option<&S> {
            self.services.get::<S>()
        }
    }

    fn fetch_dummy_value(ctx: &DummyCtx) -> Result<u32, OpError> {
        let service = require_service!(ctx, DummyService, "Dummy-Service");
        Ok(service.value)
    }

    #[test]
    fn require_service_returns_value_when_registered() {
        let mut services = ServiceMap::new();
        services.insert(DummyService { value: 42 });
        let ctx = DummyCtx { services };

        let result = fetch_dummy_value(&ctx);
        assert_eq!(result, Ok(42));
    }

    #[test]
    fn require_service_returns_not_available_when_missing() {
        let ctx = DummyCtx {
            services: ServiceMap::new(),
        };

        let result = fetch_dummy_value(&ctx);
        match result {
            Err(OpError::NotAvailable(msg)) => {
                assert!(
                    msg.contains("Dummy-Service"),
                    "Fehlermeldung sollte das Label enthalten, war: {msg}"
                );
                assert!(
                    msg.contains("kein") && msg.contains("konfiguriert"),
                    "Fehlermeldung sollte das require_service!-Template enthalten, war: {msg}"
                );
            }
            other => panic!("erwartete Err(OpError::NotAvailable(_)), erhielt: {other:?}"),
        }
    }

    // ── Fixtures für `operations!` / `try_operations!` ────────────────────────

    /// Minimale Test-Operation mit konfigurierbarem Namen (keine Surfaces/Aliase).
    struct DummyOp {
        name: &'static str,
    }

    impl Operation for DummyOp {
        fn meta(&self) -> &OperationMeta {
            // Name ist zur Laufzeit konfigurierbar, daher kein `static OnceLock`
            // pro Instanz möglich — analog zum `TestOp`-Fixture in `registry.rs`.
            Box::leak(Box::new(OperationMeta {
                name: self.name,
                summary: "Dummy-Operation für service_macro-Tests.",
                domain: OperationDomain::Misc,
                permission: PermissionTier::Observer,
                surfaces: vec![],
                aliases: &[],
                category: OperationCategory::Misc,
                args_schema: None,
                output_schema: None,
            }))
        }

        fn run<'a>(&'a self, _ctx: &'a crate::context::OpContext, _input: OpInput) -> OpFuture<'a> {
            unimplemented!("DummyOp::run wird in service_macro-Tests nicht aufgerufen")
        }
    }

    /// Operation mit fest verdrahtetem Namen, damit `operations!`/`try_operations!`
    /// auch mit reinen Typ-Idents (statt Werten) durchgespielt werden können,
    /// exakt wie im Aufrufbeispiel `operations![registry; HelpOperation, ...]`.
    struct FixedNameOp;

    impl Operation for FixedNameOp {
        fn meta(&self) -> &OperationMeta {
            static META: OnceLock<OperationMeta> = OnceLock::new();
            META.get_or_init(|| OperationMeta {
                name: "fixed",
                summary: "Feste Dummy-Operation.",
                domain: OperationDomain::Misc,
                permission: PermissionTier::Observer,
                surfaces: vec![],
                aliases: &[],
                category: OperationCategory::Misc,
                args_schema: None,
                output_schema: None,
            })
        }

        fn run<'a>(&'a self, _ctx: &'a crate::context::OpContext, _input: OpInput) -> OpFuture<'a> {
            unimplemented!("FixedNameOp::run wird in service_macro-Tests nicht aufgerufen")
        }
    }

    #[test]
    fn operations_macro_registers_all_ops_in_order() {
        let mut registry = OperationRegistry::new();
        operations![registry; FixedNameOp, DummyOp { name: "second" }];

        assert_eq!(registry.len(), 2);
        let names: Vec<&str> = registry.iter().map(|op| op.meta().name).collect();
        assert_eq!(names, vec!["fixed", "second"]);
    }

    #[test]
    fn operations_macro_accepts_trailing_comma() {
        let mut registry = OperationRegistry::new();
        operations![registry; FixedNameOp,];
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn operations_macro_empty_list_registers_nothing() {
        let registry = OperationRegistry::new();
        operations![registry;];
        assert!(registry.is_empty());
    }

    #[test]
    fn try_operations_macro_registers_all_ops() {
        fn build() -> Result<OperationRegistry, RegistryError> {
            let mut registry = OperationRegistry::new();
            try_operations![registry; FixedNameOp, DummyOp { name: "second" }];
            Ok(registry)
        }

        let registry = build().expect("keine Kollision erwartet");
        assert_eq!(registry.len(), 2);
    }

    #[test]
    fn try_operations_macro_propagates_duplicate_name_error() {
        fn build() -> Result<OperationRegistry, RegistryError> {
            let mut registry = OperationRegistry::new();
            try_operations![registry; FixedNameOp, FixedNameOp];
            Ok(registry)
        }

        // `OperationRegistry` leitet kein `Debug` ab (sie hält `Arc<dyn Operation>`),
        // deshalb lässt sich das ganze `Result` nicht formatieren. Für die
        // Fehlermeldung genügt der Fehlerzweig — der Erfolgsfall wird ohnehin
        // nur daran erkannt, dass er kein Fehler ist.
        let outcome = build().err();
        assert!(
            matches!(outcome, Some(RegistryError::DuplicateName { ref name }) if name == "fixed"),
            "erwartete Err(RegistryError::DuplicateName {{ name: \"fixed\" }}), erhielt: {outcome:?}"
        );
    }
}
