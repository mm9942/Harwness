//! Routentabelle und Zugriffsentscheidung — die Tier-Ablehnungsmatrix.
//!
//! # Verantwortungsbereich
//! Dieses Modul baut [`WebRouteTable`] ausschließlich aus einer
//! [`harw_operations::registry::OperationRegistry`] und entscheidet für
//! jeden eingehenden Aufruf (Pfad, Methode, Peer-Identität), ob er
//! ausgeführt werden darf — [`decide_route`] ist die eine Funktion, die
//! diese Entscheidung trifft und damit die Tier-Ablehnungsmatrix trägt.
//!
//! # Warum eine Route ohne `OperationMeta` nicht konstruierbar ist
//! [`WebRouteTable`] hat **einen** öffentlichen Konstruktor:
//! [`WebRouteTable::from_registry`]. Er nimmt ausschließlich eine
//! [`OperationRegistry`] entgegen und baut Einträge intern nur über
//! [`harw_operations::adapter::WebAdapter::from_operation`] — und dieser
//! wiederum nur aus `Arc<dyn Operation>` (siehe dessen Moduldoku: der
//! `Operation`-Trait erzwingt `meta()`). Es gibt in diesem Modul **keine**
//! Methode, die einen rohen Pfad-String und eine Callback-Funktion entgegennimmt
//! und daraus eine Route baut — genau das macht „Route ohne Metadaten"
//! nicht nur ungeprüft, sondern unausdrückbar. [`test_route_table_has_no_way_to_add_a_route_without_an_operation`]
//! belegt das anhand einer Operation, die absichtlich **keine**
//! `Surface::Web` deklariert: sie erzeugt nachweisbar keine Route.
//!
//! # Kein zweiter Autoritätspfad
//! [`decide_route`] fragt für die Berechtigungsstufe ausschließlich
//! [`crate::authz::PeerAuthorizer`] und für die Route ausschließlich
//! [`WebRouteTable::find`] — beide sind ausschließlich von außen (Registry
//! bzw. serverseitig vertraute Konfiguration) gespeist. Es gibt keinen
//! Zweig, der eine Operation ausführt, die nicht über
//! `WebRouteTable::from_registry` in die Tabelle gelangt ist.

use std::sync::Arc;

use harw_operations::adapter::WebAdapter;
use harw_operations::operation::{ApprovalPolicy, Operation, PermissionTier, Surface};
use harw_operations::registry::OperationRegistry;

use crate::authz::{PeerAuthorizer, tier_permits};
use crate::error::WebError;
use crate::peer::PeerCredentials;

/// Die für eine `harw-web`-Route zulässige HTTP-Methode — der kanonische
/// Vertragstyp aus `harw-operations` (F-031), hier nur re-exportiert.
///
/// # Description
/// Es gibt in `harw-web` **keinen** eigenen Methoden-Enum und **keine**
/// Ableitung aus einem `readonly`-Flag mehr. Die Methode einer Route ist
/// genau das, was ihre Operation in `Surface::Web { method, .. }` deklariert
/// ([`WebRouteTable::from_registry`] übernimmt sie unverändert).
pub use harw_operations::operation::WebMethod;

/// Der HTTP-Methodenname einer [`WebMethod`] (`"GET"`/`"POST"`).
///
/// # Arguments
/// - `method` (`WebMethod`): die deklarierte Methode.
///
/// # Returns
/// Den großgeschriebenen Methodennamen als `&'static str` — derselbe Wert,
/// den `Allow`-Header und Serde-Form (`SCREAMING_SNAKE_CASE`) tragen.
///
/// # Concurrency
/// Rein.
///
/// # Examples
/// ```rust
/// use harw_web::router::{WebMethod, method_name};
///
/// assert_eq!(method_name(WebMethod::Get), "GET");
/// assert_eq!(method_name(WebMethod::Post), "POST");
/// ```
#[must_use]
pub const fn method_name(method: WebMethod) -> &'static str {
    match method {
        WebMethod::Get => "GET",
        WebMethod::Post => "POST",
    }
}

/// Übersetzt einen HTTP-Methodennamen in eine [`WebMethod`].
///
/// # Description
/// Nur exakt `"GET"` und `"POST"` werden abgebildet (HTTP-Methodennamen sind
/// nach RFC 9110 §9.1 case-sensitiv). Insbesondere werden `HEAD` und
/// `OPTIONS` **nicht** implizit auf `GET` abgebildet: sie liefern `None` und
/// damit in [`decide_route`] immer [`RouteDecision::MethodNotAllowed`] — auch
/// auf `GET`-Routen, sodass keine Methode außer der deklarierten je eine
/// Operation erreicht (F-031).
///
/// # Arguments
/// - `name` (`&str`): der Methodenname aus der Anfragezeile.
///
/// # Returns
/// `Some(WebMethod)` für `GET`/`POST`, sonst `None`.
///
/// # Concurrency
/// Rein.
///
/// # Examples
/// ```rust
/// use harw_web::router::{WebMethod, parse_web_method};
///
/// assert_eq!(parse_web_method("POST"), Some(WebMethod::Post));
/// assert_eq!(parse_web_method("HEAD"), None);
/// ```
#[must_use]
pub fn parse_web_method(name: &str) -> Option<WebMethod> {
    match name {
        "GET" => Some(WebMethod::Get),
        "POST" => Some(WebMethod::Post),
        _ => None,
    }
}

/// Warum [`decide_route`] eine Route abgelehnt hat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForbiddenReason {
    /// Der Peer ist keiner Berechtigungsstufe zugeordnet
    /// ([`PeerAuthorizer::tier_for`] lieferte `None`).
    UnknownPeer,
    /// Der Peer ist bekannt, aber seine Stufe reicht für diese Route nicht.
    InsufficientTier,
}

/// Das Ergebnis von [`decide_route`] — genau ein Ausgang, nie mehrere
/// gleichzeitig wahr.
#[derive(Debug)]
pub enum RouteDecision<'a> {
    /// Kein registrierter `Surface::Web`-Pfad passt zu `path`.
    NotFound,
    /// Der Pfad existiert, aber `method` ist nicht die in
    /// `Surface::Web { method, .. }` deklarierte Methode (oder gar keine
    /// unterstützte, z. B. `HEAD`/`OPTIONS`/`DELETE`).
    MethodNotAllowed {
        /// Die für diese Route deklarierte (einzig zulässige) Methode —
        /// Quelle des `Allow`-Headers.
        expected: WebMethod,
    },
    /// Der Aufrufer darf diese Route nicht erreichen.
    Forbidden {
        /// Warum abgelehnt wurde.
        reason: ForbiddenReason,
    },
    /// Die Route verlangt eine Genehmigung (`approval != ApprovalPolicy::None`).
    /// `harw-web` darf sie **nicht** ausführen — das ist UI-06s Sache; diese
    /// Fläche lässt es strukturell nicht anders zu.
    ApprovalRequired {
        /// Die betroffene Route.
        route: &'a WebAdapter,
    },
    /// Alle Prüfungen bestanden — die Route darf ausgeführt werden.
    Execute {
        /// Die auszuführende Route.
        route: &'a WebAdapter,
        /// Die aufgelöste Berechtigungsstufe des Aufrufers.
        caller_tier: PermissionTier,
    },
}

/// Entscheidet, was mit einem eingehenden Web-Aufruf geschehen darf.
///
/// # Description
/// Prüft in dieser Reihenfolge: Route existiert → Methode passt → Peer hat
/// eine bekannte Stufe → Stufe reicht → keine Genehmigung nötig. Der erste
/// nicht bestandene Schritt bestimmt das Ergebnis; besteht alles, liefert
/// sie [`RouteDecision::Execute`].
///
/// # Arguments
/// - `routes` (`&WebRouteTable`): die aus der Registry gebaute Routentabelle.
/// - `authorizer` (`&dyn PeerAuthorizer`): löst die Berechtigungsstufe des Peers auf.
/// - `peer` (`&PeerCredentials`): die über `SO_PEERCRED` gelesene Identität.
/// - `path` (`&str`): der angefragte HTTP-Pfad.
/// - `method` (`Option<WebMethod>`): die angefragte Methode, `None` für jede
///   nicht unterstützte HTTP-Methode (z. B. `HEAD`, `OPTIONS`, `DELETE`; siehe
///   [`parse_web_method`]).
///
/// # Security (F-031)
/// Die Methodenprüfung vergleicht ausschließlich mit der **deklarierten**
/// Methode der Route ([`WebRouteTable::method_for`]); sie steht vor jeder
/// Autorisierungs- und Ausführungsentscheidung. Ein `GET` erreicht damit nie
/// eine Operation, deren `Surface::Web` `method: WebMethod::Post` trägt —
/// Prefetch, `<img src>` oder Cross-Site-Navigation lösen keine Mutation aus.
///
/// # Returns
/// Ein [`RouteDecision`] mit exakt einem Ausgang.
///
/// # Concurrency
/// Rein; keine Sperren, kein gemeinsamer veränderlicher Zustand.
///
/// # Examples
/// ```rust
/// use harw_operations::operation::PermissionTier;
/// use harw_operations::registry::OperationRegistry;
/// use harw_web::authz::StaticUidTierMap;
/// use harw_web::peer::PeerCredentials;
/// use harw_web::router::{RouteDecision, WebRouteTable, decide_route};
///
/// let registry = OperationRegistry::new();
/// let routes = WebRouteTable::from_registry(&registry).unwrap();
/// let authz = StaticUidTierMap::with_default(vec![], PermissionTier::Observer);
/// let decision = decide_route(
///     &routes,
///     &authz,
///     &PeerCredentials::new(1, 1000, 1000),
///     "/api/unknown",
///     None,
/// );
/// assert!(matches!(decision, RouteDecision::NotFound));
/// ```
#[must_use]
pub fn decide_route<'a>(
    routes: &'a WebRouteTable,
    authorizer: &dyn PeerAuthorizer,
    peer: &PeerCredentials,
    path: &str,
    method: Option<WebMethod>,
) -> RouteDecision<'a> {
    let Some(entry) = routes.find_entry(path) else {
        return RouteDecision::NotFound;
    };
    let route = &entry.adapter;

    let expected = entry.method;
    if method != Some(expected) {
        return RouteDecision::MethodNotAllowed { expected };
    }

    let Some(tier) = authorizer.tier_for(peer) else {
        return RouteDecision::Forbidden {
            reason: ForbiddenReason::UnknownPeer,
        };
    };

    if !tier_permits(tier, route.permission()) {
        return RouteDecision::Forbidden {
            reason: ForbiddenReason::InsufficientTier,
        };
    }

    if route.approval() != ApprovalPolicy::None {
        return RouteDecision::ApprovalRequired { route };
    }

    RouteDecision::Execute {
        route,
        caller_tier: tier,
    }
}

/// Die aus einer [`OperationRegistry`] gebaute HTTP-Routentabelle.
///
/// # Description
/// Enthält genau einen Eintrag pro `Surface::Web`-Deklaration jeder
/// registrierten Operation, jeweils mit der dort deklarierten
/// [`WebMethod`]. Siehe Moduldoku für die Begründung, warum eine Route ohne
/// `OperationMeta` nicht konstruierbar ist.
pub struct WebRouteTable {
    routes: Vec<WebRoute>,
}

// Ein Tabelleneintrag: der Adapter plus die aus `Surface::Web { method, .. }`
// übernommene Methode. Privat — von außen nur über `from_registry` befüllbar.
struct WebRoute {
    adapter: WebAdapter,
    method: WebMethod,
}

// Liest die in `op` für `path` deklarierte `Surface::Web`-Methode.
// `None` nur, wenn der Adapter einen Pfad trägt, den die Operation nicht (mehr)
// deklariert — dann wird die Tabelle fail-closed nicht gebaut.
fn declared_method(op: &dyn Operation, path: &str) -> Option<WebMethod> {
    op.meta().surfaces.iter().find_map(|surface| match surface {
        Surface::Web {
            path: declared,
            method,
            ..
        } if *declared == path => Some(*method),
        _ => None,
    })
}

impl WebRouteTable {
    /// Baut die Routentabelle aus allen `Surface::Web`-Deklarationen der Registry.
    ///
    /// # Arguments
    /// - `registry` (`&OperationRegistry`): eine bereits befüllte Registry,
    ///   typischerweise via `harw_ops::register_all`.
    ///
    /// # Returns
    /// Eine `WebRouteTable` mit einem Eintrag pro `Surface::Web`-Deklaration.
    ///
    /// # Description
    /// Die HTTP-Methode jedes Eintrags wird **unverändert** aus
    /// `Surface::Web { method, .. }` der Operation übernommen — keine
    /// Ableitung aus `readonly`, `approval` oder dem Pfad (F-031).
    ///
    /// # Errors
    /// - [`WebError::DuplicateRoute`], wenn zwei Operationen denselben Pfad
    ///   beanspruchen.
    /// - [`WebError::RouteMethodUndeclared`], wenn sich für einen Adapter keine
    ///   `Surface::Web`-Deklaration mit dessen Pfad finden lässt (fail-closed:
    ///   eine Route ohne deklarierte Methode wird nie bedient).
    ///
    /// # Concurrency
    /// Erfordert nur einen shared borrow von `registry`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_operations::registry::OperationRegistry;
    /// use harw_web::router::WebRouteTable;
    ///
    /// let registry = OperationRegistry::new();
    /// let routes = WebRouteTable::from_registry(&registry).unwrap();
    /// assert!(routes.is_empty());
    /// ```
    pub fn from_registry(registry: &OperationRegistry) -> Result<Self, WebError> {
        let mut routes: Vec<WebRoute> = Vec::new();
        for op in registry.iter() {
            for adapter in WebAdapter::from_operation(Arc::clone(op)) {
                if let Some(existing) = routes.iter().find(|r| r.adapter.path() == adapter.path()) {
                    return Err(WebError::DuplicateRoute {
                        path: adapter.path().to_owned(),
                        first_owner: existing.adapter.operation_name().to_owned(),
                        second_owner: adapter.operation_name().to_owned(),
                    });
                }
                let method = declared_method(op.as_ref(), adapter.path()).ok_or_else(|| {
                    WebError::RouteMethodUndeclared {
                        path: adapter.path().to_owned(),
                        operation: adapter.operation_name().to_owned(),
                    }
                })?;
                tracing::debug!(
                    path = adapter.path(),
                    operation = adapter.operation_name(),
                    method = method_name(method),
                    "web route registered"
                );
                routes.push(WebRoute { adapter, method });
            }
        }
        Ok(Self { routes })
    }

    // Sucht den vollständigen Eintrag (Adapter + deklarierte Methode).
    fn find_entry(&self, path: &str) -> Option<&WebRoute> {
        self.routes
            .iter()
            .find(|route| route.adapter.path() == path)
    }

    /// Sucht eine Route über ihren exakten Pfad.
    #[must_use]
    pub fn find(&self, path: &str) -> Option<&WebAdapter> {
        self.find_entry(path).map(|route| &route.adapter)
    }

    /// Liefert die deklarierte HTTP-Methode der Route unter `path`.
    ///
    /// # Arguments
    /// - `path` (`&str`): der exakte Routenpfad.
    ///
    /// # Returns
    /// `Some(method)` aus `Surface::Web { method, .. }`, `None` für einen
    /// unbekannten Pfad.
    ///
    /// # Concurrency
    /// Nur lesend; beliebig nebenläufig nutzbar.
    ///
    /// # Examples
    /// ```rust
    /// use harw_operations::registry::OperationRegistry;
    /// use harw_web::router::WebRouteTable;
    ///
    /// let routes = WebRouteTable::from_registry(&OperationRegistry::new()).unwrap();
    /// assert_eq!(routes.method_for("/api/unknown"), None);
    /// ```
    #[must_use]
    pub fn method_for(&self, path: &str) -> Option<WebMethod> {
        self.find_entry(path).map(|route| route.method)
    }

    /// Anzahl registrierter Routen.
    #[must_use]
    pub fn len(&self) -> usize {
        self.routes.len()
    }

    /// `true`, wenn keine Route registriert ist.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }

    /// Iteriert über alle Routen in Registrierungsreihenfolge.
    pub fn iter(&self) -> impl Iterator<Item = &WebAdapter> {
        self.routes.iter().map(|route| &route.adapter)
    }

    /// Iteriert über alle Routen samt deklarierter Methode in
    /// Registrierungsreihenfolge.
    pub fn iter_with_methods(&self) -> impl Iterator<Item = (&WebAdapter, WebMethod)> {
        self.routes
            .iter()
            .map(|route| (&route.adapter, route.method))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use harw_operations::operation::{
        BusyAvailability, OpFuture, OpInput, OpOutput, Operation, OperationCategory,
        OperationDomain, OperationMeta, PermissionTier, Surface,
    };
    use harw_operations::registry::OperationRegistry;

    use super::{
        ForbiddenReason, RouteDecision, WebMethod, WebRouteTable, decide_route, method_name,
        parse_web_method,
    };
    use crate::authz::StaticUidTierMap;
    use crate::peer::PeerCredentials;
    use crate::test_support::{TestError, TestResult, ctx};

    /// Testoperation mit konfigurierbarer `Surface::Web`-Deklaration.
    struct TierOp {
        name: &'static str,
        path: &'static str,
        permission: PermissionTier,
        method: WebMethod,
        approval: harw_operations::operation::ApprovalPolicy,
    }

    impl Operation for TierOp {
        fn meta(&self) -> &OperationMeta {
            Box::leak(Box::new(OperationMeta {
                name: self.name,
                summary: "Tier-Testoperation.",
                domain: OperationDomain::Misc,
                permission: self.permission,
                surfaces: vec![Surface::Web {
                    path: self.path,
                    method: self.method,
                    approval: self.approval,
                }],
                aliases: &[],
                category: OperationCategory::Misc,
                args_schema: None,
                output_schema: None,
                busy: BusyAvailability::DeferredUntilTurnEnd,
            }))
        }

        fn run<'a>(
            &'a self,
            _ctx: &'a harw_operations::context::OpContext,
            _input: OpInput,
        ) -> OpFuture<'a> {
            Box::pin(async {
                Ok(OpOutput {
                    text: "ok".to_owned(),
                    data: None,
                })
            })
        }
    }

    struct NoWebSurfaceOp;
    impl Operation for NoWebSurfaceOp {
        fn meta(&self) -> &OperationMeta {
            static META: OnceLock<OperationMeta> = OnceLock::new();
            META.get_or_init(|| OperationMeta {
                name: "no-web",
                summary: "Keine Web-Fläche.",
                domain: OperationDomain::Misc,
                permission: PermissionTier::Observer,
                surfaces: vec![],
                aliases: &[],
                category: OperationCategory::Misc,
                args_schema: None,
                output_schema: None,
                busy: BusyAvailability::DeferredUntilTurnEnd,
            })
        }
        fn run<'a>(
            &'a self,
            _ctx: &'a harw_operations::context::OpContext,
            _input: OpInput,
        ) -> OpFuture<'a> {
            Box::pin(async {
                Ok(OpOutput {
                    text: "noop".to_owned(),
                    data: None,
                })
            })
        }
    }

    fn tier_op(
        name: &'static str,
        path: &'static str,
        permission: PermissionTier,
    ) -> std::sync::Arc<dyn Operation> {
        std::sync::Arc::new(TierOp {
            name,
            path,
            permission,
            method: WebMethod::Get,
            approval: harw_operations::operation::ApprovalPolicy::None,
        })
    }

    /// Wie [`tier_op`], aber mit frei wählbarer deklarierter Methode.
    fn method_op(
        name: &'static str,
        path: &'static str,
        method: WebMethod,
    ) -> std::sync::Arc<dyn Operation> {
        std::sync::Arc::new(TierOp {
            name,
            path,
            permission: PermissionTier::Observer,
            method,
            approval: harw_operations::operation::ApprovalPolicy::None,
        })
    }

    /// Registry mit einer `GET`- und einer `POST`-Route (beide Observer, ohne Approval).
    fn get_and_post_registry() -> OperationRegistry {
        let mut registry = OperationRegistry::new();
        registry.register(method_op("read-op", "/api/read", WebMethod::Get));
        registry.register(method_op("write-op", "/api/write", WebMethod::Post));
        registry
    }

    /// Baut eine Registry mit je einer Route pro Berechtigungsstufe.
    fn four_tier_registry() -> OperationRegistry {
        let mut registry = OperationRegistry::new();
        registry.register(tier_op(
            "observer-op",
            "/api/observer",
            PermissionTier::Observer,
        ));
        registry.register(tier_op(
            "operator-op",
            "/api/operator",
            PermissionTier::Operator,
        ));
        registry.register(tier_op(
            "maintainer-op",
            "/api/maintainer",
            PermissionTier::Maintainer,
        ));
        registry.register(tier_op("owner-op", "/api/owner", PermissionTier::Owner));
        registry
    }

    fn peer_with_tier() -> PeerCredentials {
        PeerCredentials::new(1, 1000, 1000)
    }

    // ── Route ohne OperationMeta ist nicht konstruierbar ──────────────────────

    #[test]
    fn test_route_table_has_no_way_to_add_a_route_without_an_operation() -> TestResult {
        let mut registry = OperationRegistry::new();
        registry.register(std::sync::Arc::new(NoWebSurfaceOp));
        let routes = WebRouteTable::from_registry(&registry).map_err(ctx("from_registry"))?;
        assert!(
            routes.is_empty(),
            "eine Operation ohne Surface::Web darf keine Route erzeugen — \
             WebRouteTable kennt keinen anderen Weg, eine Route hinzuzufügen"
        );
        Ok(())
    }

    #[test]
    fn test_empty_registry_yields_empty_route_table() -> TestResult {
        let registry = OperationRegistry::new();
        let routes = WebRouteTable::from_registry(&registry).map_err(ctx("from_registry"))?;
        assert!(routes.is_empty());
        assert_eq!(routes.len(), 0);
        Ok(())
    }

    #[test]
    fn test_duplicate_web_path_across_two_operations_is_rejected() -> TestResult {
        let mut registry = OperationRegistry::new();
        registry
            .try_register(tier_op("a", "/api/dup", PermissionTier::Observer))
            .map_err(ctx("die erste Operation belegt den Pfad"))?;
        // Die Abwehr sitzt inzwischen eine Ebene früher: nicht erst beim Bau
        // der Routentabelle, sondern schon bei der Registrierung. `register()`
        // panickt bei einer Kollision (wie bereits bei Namens- und
        // Aliaskollisionen); `try_register()` liefert den typisierten Fehler.
        //
        // Zwei Operationen auf demselben Pfad sind über HTTP
        // ununterscheidbar -- das muss ein Fehler sein, kein stiller Vorrang.
        let Err(collision) =
            registry.try_register(tier_op("b", "/api/dup", PermissionTier::Observer))
        else {
            return Err(TestError::Unexpected(
                "ein doppelt belegter Web-Pfad muss abgelehnt werden".into(),
            ));
        };
        assert!(matches!(
            collision,
            harw_operations::registry::RegistryError::WebPathCollision { .. }
        ));
        Ok(())
    }

    // ── kein Web-Weg erreicht eine nicht registrierte Operation ───────────────

    #[test]
    fn test_no_route_found_for_operation_never_registered() -> TestResult {
        let registry = four_tier_registry();
        let routes = WebRouteTable::from_registry(&registry).map_err(ctx("from_registry"))?;
        assert!(routes.find("/api/never-registered").is_none());
        Ok(())
    }

    // ── Tier-Ablehnungsmatrix ──────────────────────────────────────────────────

    #[test]
    fn test_observer_caller_allowed_on_observer_route() -> TestResult {
        let registry = four_tier_registry();
        let routes = WebRouteTable::from_registry(&registry).map_err(ctx("from_registry"))?;
        let authz = StaticUidTierMap::new(vec![(1000, PermissionTier::Observer)]);
        let decision = decide_route(
            &routes,
            &authz,
            &peer_with_tier(),
            "/api/observer",
            Some(WebMethod::Get),
        );
        assert!(matches!(decision, RouteDecision::Execute { .. }));
        Ok(())
    }

    #[test]
    fn test_observer_caller_denied_on_operator_route() -> TestResult {
        let registry = four_tier_registry();
        let routes = WebRouteTable::from_registry(&registry).map_err(ctx("from_registry"))?;
        let authz = StaticUidTierMap::new(vec![(1000, PermissionTier::Observer)]);
        let decision = decide_route(
            &routes,
            &authz,
            &peer_with_tier(),
            "/api/operator",
            Some(WebMethod::Get),
        );
        assert!(matches!(
            decision,
            RouteDecision::Forbidden {
                reason: ForbiddenReason::InsufficientTier
            }
        ));
        Ok(())
    }

    #[test]
    fn test_operator_caller_allowed_on_operator_route() -> TestResult {
        let registry = four_tier_registry();
        let routes = WebRouteTable::from_registry(&registry).map_err(ctx("from_registry"))?;
        let authz = StaticUidTierMap::new(vec![(1000, PermissionTier::Operator)]);
        let decision = decide_route(
            &routes,
            &authz,
            &peer_with_tier(),
            "/api/operator",
            Some(WebMethod::Get),
        );
        assert!(matches!(decision, RouteDecision::Execute { .. }));
        Ok(())
    }

    #[test]
    fn test_operator_caller_denied_on_maintainer_route() -> TestResult {
        let registry = four_tier_registry();
        let routes = WebRouteTable::from_registry(&registry).map_err(ctx("from_registry"))?;
        let authz = StaticUidTierMap::new(vec![(1000, PermissionTier::Operator)]);
        let decision = decide_route(
            &routes,
            &authz,
            &peer_with_tier(),
            "/api/maintainer",
            Some(WebMethod::Get),
        );
        assert!(matches!(
            decision,
            RouteDecision::Forbidden {
                reason: ForbiddenReason::InsufficientTier
            }
        ));
        Ok(())
    }

    #[test]
    fn test_maintainer_caller_allowed_on_maintainer_route() -> TestResult {
        let registry = four_tier_registry();
        let routes = WebRouteTable::from_registry(&registry).map_err(ctx("from_registry"))?;
        let authz = StaticUidTierMap::new(vec![(1000, PermissionTier::Maintainer)]);
        let decision = decide_route(
            &routes,
            &authz,
            &peer_with_tier(),
            "/api/maintainer",
            Some(WebMethod::Get),
        );
        assert!(matches!(decision, RouteDecision::Execute { .. }));
        Ok(())
    }

    #[test]
    fn test_maintainer_caller_denied_on_owner_route() -> TestResult {
        let registry = four_tier_registry();
        let routes = WebRouteTable::from_registry(&registry).map_err(ctx("from_registry"))?;
        let authz = StaticUidTierMap::new(vec![(1000, PermissionTier::Maintainer)]);
        let decision = decide_route(
            &routes,
            &authz,
            &peer_with_tier(),
            "/api/owner",
            Some(WebMethod::Get),
        );
        assert!(matches!(
            decision,
            RouteDecision::Forbidden {
                reason: ForbiddenReason::InsufficientTier
            }
        ));
        Ok(())
    }

    #[test]
    fn test_owner_caller_allowed_on_owner_route() -> TestResult {
        let registry = four_tier_registry();
        let routes = WebRouteTable::from_registry(&registry).map_err(ctx("from_registry"))?;
        let authz = StaticUidTierMap::new(vec![(1000, PermissionTier::Owner)]);
        let decision = decide_route(
            &routes,
            &authz,
            &peer_with_tier(),
            "/api/owner",
            Some(WebMethod::Get),
        );
        assert!(matches!(decision, RouteDecision::Execute { .. }));
        Ok(())
    }

    /// Owner ist die höchste Stufe der Totalordnung — es gibt strukturell
    /// keine Route, die einen Owner-Aufrufer per Tier ablehnen könnte (siehe
    /// `harw_operations::operation::PermissionTier`-Dokumentation:
    /// `Observer < Operator < Maintainer < Owner`). Der „abgelehnte Fall" für
    /// Owner ist deshalb keine höhere Route, sondern ein unbekannter Peer —
    /// dieselbe Ablehnung, die jede andere Stufe ebenfalls treffen kann.
    #[test]
    fn test_owner_tier_route_denied_for_unknown_peer_instead_of_insufficient_tier() -> TestResult {
        let registry = four_tier_registry();
        let routes = WebRouteTable::from_registry(&registry).map_err(ctx("from_registry"))?;
        let authz = StaticUidTierMap::new(vec![]); // kein Eintrag für irgendeine UID
        let decision = decide_route(
            &routes,
            &authz,
            &peer_with_tier(),
            "/api/owner",
            Some(WebMethod::Get),
        );
        assert!(matches!(
            decision,
            RouteDecision::Forbidden {
                reason: ForbiddenReason::UnknownPeer
            }
        ));
        Ok(())
    }

    // ── NotFound / MethodNotAllowed / ApprovalRequired ────────────────────────

    #[test]
    fn test_unknown_path_yields_not_found() -> TestResult {
        let registry = four_tier_registry();
        let routes = WebRouteTable::from_registry(&registry).map_err(ctx("from_registry"))?;
        let authz = StaticUidTierMap::with_default(vec![], PermissionTier::Owner);
        let decision = decide_route(
            &routes,
            &authz,
            &peer_with_tier(),
            "/api/does-not-exist",
            Some(WebMethod::Get),
        );
        assert!(matches!(decision, RouteDecision::NotFound));
        Ok(())
    }

    #[test]
    fn test_wrong_method_yields_method_not_allowed_with_expected_method() -> TestResult {
        let registry = four_tier_registry();
        let routes = WebRouteTable::from_registry(&registry).map_err(ctx("from_registry"))?;
        let authz = StaticUidTierMap::with_default(vec![], PermissionTier::Owner);
        // /api/observer deklariert method: Get; POST muss abgelehnt werden.
        let decision = decide_route(
            &routes,
            &authz,
            &peer_with_tier(),
            "/api/observer",
            Some(WebMethod::Post),
        );
        assert!(matches!(
            decision,
            RouteDecision::MethodNotAllowed {
                expected: WebMethod::Get
            }
        ));
        Ok(())
    }

    #[test]
    fn test_unsupported_http_method_yields_method_not_allowed() -> TestResult {
        let registry = four_tier_registry();
        let routes = WebRouteTable::from_registry(&registry).map_err(ctx("from_registry"))?;
        let authz = StaticUidTierMap::with_default(vec![], PermissionTier::Owner);
        let decision = decide_route(&routes, &authz, &peer_with_tier(), "/api/observer", None);
        assert!(matches!(decision, RouteDecision::MethodNotAllowed { .. }));
        Ok(())
    }

    #[test]
    fn test_approval_required_route_is_never_executed_directly() -> TestResult {
        let mut registry = OperationRegistry::new();
        registry.register(std::sync::Arc::new(TierOp {
            name: "irreversible",
            path: "/api/irreversible",
            permission: PermissionTier::Operator,
            method: WebMethod::Post,
            approval: harw_operations::operation::ApprovalPolicy::Always,
        }));
        let routes = WebRouteTable::from_registry(&registry).map_err(ctx("from_registry"))?;
        let authz = StaticUidTierMap::with_default(vec![], PermissionTier::Owner);
        let decision = decide_route(
            &routes,
            &authz,
            &peer_with_tier(),
            "/api/irreversible",
            Some(WebMethod::Post),
        );
        assert!(
            matches!(decision, RouteDecision::ApprovalRequired { .. }),
            "eine Route mit approval != None darf harw-web nie direkt ausführen"
        );
        Ok(())
    }

    // ── F-031: deklarierte Methode, keine Ableitung ───────────────────────────

    #[test]
    fn test_decide_route_get_on_post_route_yields_method_not_allowed() -> TestResult {
        let registry = get_and_post_registry();
        let routes = WebRouteTable::from_registry(&registry).map_err(ctx("from_registry"))?;
        let authz = StaticUidTierMap::with_default(vec![], PermissionTier::Owner);
        let decision = decide_route(
            &routes,
            &authz,
            &peer_with_tier(),
            "/api/write",
            Some(WebMethod::Get),
        );
        assert!(
            matches!(
                decision,
                RouteDecision::MethodNotAllowed {
                    expected: WebMethod::Post
                }
            ),
            "GET darf eine POST-Operation nie erreichen (F-031), erhalten: {decision:?}"
        );
        Ok(())
    }

    #[test]
    fn test_decide_route_post_on_get_route_yields_method_not_allowed() -> TestResult {
        let registry = get_and_post_registry();
        let routes = WebRouteTable::from_registry(&registry).map_err(ctx("from_registry"))?;
        let authz = StaticUidTierMap::with_default(vec![], PermissionTier::Owner);
        let decision = decide_route(
            &routes,
            &authz,
            &peer_with_tier(),
            "/api/read",
            Some(WebMethod::Post),
        );
        assert!(matches!(
            decision,
            RouteDecision::MethodNotAllowed {
                expected: WebMethod::Get
            }
        ));
        Ok(())
    }

    #[test]
    fn test_decide_route_correct_method_dispatches_to_declaring_operation() -> TestResult {
        let registry = get_and_post_registry();
        let routes = WebRouteTable::from_registry(&registry).map_err(ctx("from_registry"))?;
        let authz = StaticUidTierMap::with_default(vec![], PermissionTier::Observer);

        let post = decide_route(
            &routes,
            &authz,
            &peer_with_tier(),
            "/api/write",
            Some(WebMethod::Post),
        );
        match post {
            RouteDecision::Execute { route, caller_tier } => {
                assert_eq!(route.operation_name(), "write-op");
                assert_eq!(caller_tier, PermissionTier::Observer);
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "POST auf POST-Route muss Execute liefern, erhalten: {other:?}"
                )));
            }
        }

        let get = decide_route(
            &routes,
            &authz,
            &peer_with_tier(),
            "/api/read",
            Some(WebMethod::Get),
        );
        match get {
            RouteDecision::Execute { route, .. } => assert_eq!(route.operation_name(), "read-op"),
            other => {
                return Err(TestError::Unexpected(format!(
                    "GET auf GET-Route muss Execute liefern, erhalten: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_decide_route_unsupported_method_on_post_route_is_rejected_before_authorization()
    -> TestResult {
        let registry = get_and_post_registry();
        let routes = WebRouteTable::from_registry(&registry).map_err(ctx("from_registry"))?;
        // Unbekannter Peer: die Methodenprüfung muss trotzdem vorher greifen.
        let authz = StaticUidTierMap::new(vec![]);
        let decision = decide_route(&routes, &authz, &peer_with_tier(), "/api/write", None);
        assert!(matches!(
            decision,
            RouteDecision::MethodNotAllowed {
                expected: WebMethod::Post
            }
        ));
        Ok(())
    }

    #[test]
    fn test_from_registry_takes_method_from_surface_web_declaration() -> TestResult {
        let registry = get_and_post_registry();
        let routes = WebRouteTable::from_registry(&registry).map_err(ctx("from_registry"))?;
        assert_eq!(routes.len(), 2);
        assert_eq!(routes.method_for("/api/read"), Some(WebMethod::Get));
        assert_eq!(routes.method_for("/api/write"), Some(WebMethod::Post));
        assert_eq!(routes.method_for("/api/unknown"), None);
        let collected: Vec<(&str, WebMethod)> = routes
            .iter_with_methods()
            .map(|(adapter, method)| (adapter.path(), method))
            .collect();
        assert_eq!(
            collected,
            vec![
                ("/api/read", WebMethod::Get),
                ("/api/write", WebMethod::Post)
            ]
        );
        Ok(())
    }

    /// Eine mutierende Route mit `approval = None` und `method = Post` ist genau
    /// der F-031-Fall (`/api/analyze`): die Methode kommt aus der Deklaration,
    /// nicht aus einer `readonly`-Heuristik oder dem Approval-Wert.
    #[test]
    fn test_from_registry_post_without_approval_stays_post() -> TestResult {
        let mut registry = OperationRegistry::new();
        registry.register(method_op("analyze-like", "/api/analyze", WebMethod::Post));
        let routes = WebRouteTable::from_registry(&registry).map_err(ctx("from_registry"))?;
        assert_eq!(routes.method_for("/api/analyze"), Some(WebMethod::Post));
        assert_eq!(
            routes.find("/api/analyze").map(|r| r.operation_name()),
            Some("analyze-like")
        );
        Ok(())
    }

    #[test]
    fn test_parse_web_method_maps_only_exact_get_and_post() {
        assert_eq!(parse_web_method("GET"), Some(WebMethod::Get));
        assert_eq!(parse_web_method("POST"), Some(WebMethod::Post));
        assert_eq!(parse_web_method("HEAD"), None);
        assert_eq!(parse_web_method("OPTIONS"), None);
        assert_eq!(parse_web_method("DELETE"), None);
        assert_eq!(parse_web_method("get"), None);
        assert_eq!(parse_web_method("post"), None);
        assert_eq!(parse_web_method(""), None);
    }

    #[test]
    fn test_method_name_matches_http_and_serde_form() -> TestResult {
        assert_eq!(method_name(WebMethod::Get), "GET");
        assert_eq!(method_name(WebMethod::Post), "POST");
        for method in [WebMethod::Get, WebMethod::Post] {
            let serialized =
                serde_json::to_string(&method).map_err(ctx("serde_json::to_string"))?;
            assert_eq!(serialized, format!("\"{}\"", method_name(method)));
            assert_eq!(parse_web_method(method_name(method)), Some(method));
        }
        Ok(())
    }
}
