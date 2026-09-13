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
use harw_operations::operation::{ApprovalPolicy, PermissionTier};
use harw_operations::registry::OperationRegistry;

use crate::authz::{PeerAuthorizer, tier_permits};
use crate::error::WebError;
use crate::peer::PeerCredentials;

/// Die für eine `harw-web`-Route zulässige HTTP-Methode.
///
/// # Description
/// Wird ausschließlich aus [`harw_operations::adapter::WebAdapter::readonly`]
/// abgeleitet ([`WebMethod::expected_for`]) — es gibt kein zusätzliches
/// Methodenfeld an der Operation, weil `readonly` die Methode bereits
/// eindeutig festlegt (siehe `Surface::Web`-Feldbegründung in
/// `harw-operations`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebMethod {
    /// Zulässig für `readonly == true`.
    Get,
    /// Zulässig für `readonly == false`.
    Post,
}

impl WebMethod {
    /// Liefert die für eine Route mit gegebenem `readonly`-Flag erwartete Methode.
    ///
    /// # Arguments
    /// - `readonly` (`bool`): [`WebAdapter::readonly`] der Route.
    ///
    /// # Returns
    /// [`WebMethod::Get`] für `readonly == true`, sonst [`WebMethod::Post`].
    ///
    /// # Examples
    /// ```rust
    /// use harw_web::router::WebMethod;
    ///
    /// assert_eq!(WebMethod::expected_for(true), WebMethod::Get);
    /// assert_eq!(WebMethod::expected_for(false), WebMethod::Post);
    /// ```
    #[must_use]
    pub fn expected_for(readonly: bool) -> Self {
        if readonly { Self::Get } else { Self::Post }
    }

    /// Der HTTP-Methodenname als `&'static str` (`"GET"`/`"POST"`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
        }
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
    /// Der Pfad existiert, aber `method` passt nicht zu
    /// [`WebMethod::expected_for`].
    MethodNotAllowed {
        /// Die für diese Route tatsächlich erwartete Methode.
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
///   nicht unterstützte HTTP-Methode (z. B. `DELETE`).
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
    let Some(route) = routes.find(path) else {
        return RouteDecision::NotFound;
    };

    let expected = WebMethod::expected_for(route.readonly());
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
/// registrierten Operation. Siehe Moduldoku für die Begründung, warum eine
/// Route ohne `OperationMeta` nicht konstruierbar ist.
pub struct WebRouteTable {
    routes: Vec<WebAdapter>,
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
    /// # Errors
    /// [`WebError::DuplicateRoute`], wenn zwei Operationen denselben Pfad
    /// beanspruchen.
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
        let mut routes: Vec<WebAdapter> = Vec::new();
        for op in registry.iter() {
            for adapter in WebAdapter::from_operation(Arc::clone(op)) {
                if let Some(existing) = routes.iter().find(|r| r.path() == adapter.path()) {
                    return Err(WebError::DuplicateRoute {
                        path: adapter.path().to_owned(),
                        first_owner: existing.operation_name().to_owned(),
                        second_owner: adapter.operation_name().to_owned(),
                    });
                }
                routes.push(adapter);
            }
        }
        Ok(Self { routes })
    }

    /// Sucht eine Route über ihren exakten Pfad.
    #[must_use]
    pub fn find(&self, path: &str) -> Option<&WebAdapter> {
        self.routes.iter().find(|route| route.path() == path)
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
        self.routes.iter()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use harw_operations::operation::{
        OpFuture, OpInput, OpOutput, Operation, OperationCategory, OperationDomain,
        OperationMeta, PermissionTier, Surface,
    };
    use harw_operations::registry::OperationRegistry;

    use super::{ForbiddenReason, RouteDecision, WebMethod, WebRouteTable, decide_route};
    use crate::authz::StaticUidTierMap;
    use crate::peer::PeerCredentials;

    /// Testoperation mit konfigurierbarer `Surface::Web`-Deklaration.
    struct TierOp {
        name: &'static str,
        path: &'static str,
        permission: PermissionTier,
        readonly: bool,
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
                    readonly: self.readonly,
                    approval: self.approval,
                }],
                aliases: &[],
                category: OperationCategory::Misc,
                args_schema: None,
            }))
        }

        fn run<'a>(&'a self, _ctx: &'a harw_operations::context::OpContext, _input: OpInput) -> OpFuture<'a> {
            Box::pin(async { Ok(OpOutput { text: "ok".to_owned() }) })
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
            })
        }
        fn run<'a>(&'a self, _ctx: &'a harw_operations::context::OpContext, _input: OpInput) -> OpFuture<'a> {
            Box::pin(async { Ok(OpOutput { text: "noop".to_owned() }) })
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
            readonly: true,
            approval: harw_operations::operation::ApprovalPolicy::None,
        })
    }

    /// Baut eine Registry mit je einer Route pro Berechtigungsstufe.
    fn four_tier_registry() -> OperationRegistry {
        let mut registry = OperationRegistry::new();
        registry.register(tier_op("observer-op", "/api/observer", PermissionTier::Observer));
        registry.register(tier_op("operator-op", "/api/operator", PermissionTier::Operator));
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
    fn test_route_table_has_no_way_to_add_a_route_without_an_operation() {
        let mut registry = OperationRegistry::new();
        registry.register(std::sync::Arc::new(NoWebSurfaceOp));
        let routes = WebRouteTable::from_registry(&registry).unwrap();
        assert!(
            routes.is_empty(),
            "eine Operation ohne Surface::Web darf keine Route erzeugen — \
             WebRouteTable kennt keinen anderen Weg, eine Route hinzuzufügen"
        );
    }

    #[test]
    fn test_empty_registry_yields_empty_route_table() {
        let registry = OperationRegistry::new();
        let routes = WebRouteTable::from_registry(&registry).unwrap();
        assert!(routes.is_empty());
        assert_eq!(routes.len(), 0);
    }

    #[test]
    fn test_duplicate_web_path_across_two_operations_is_rejected() {
        let mut registry = OperationRegistry::new();
        registry
            .try_register(tier_op("a", "/api/dup", PermissionTier::Observer))
            .expect("die erste Operation belegt den Pfad");
        // Die Abwehr sitzt inzwischen eine Ebene früher: nicht erst beim Bau
        // der Routentabelle, sondern schon bei der Registrierung. `register()`
        // panickt bei einer Kollision (wie bereits bei Namens- und
        // Aliaskollisionen); `try_register()` liefert den typisierten Fehler.
        //
        // Zwei Operationen auf demselben Pfad sind über HTTP
        // ununterscheidbar -- das muss ein Fehler sein, kein stiller Vorrang.
        let collision = registry
            .try_register(tier_op("b", "/api/dup", PermissionTier::Observer))
            .expect_err("ein doppelt belegter Web-Pfad muss abgelehnt werden");
        assert!(matches!(
            collision,
            harw_operations::registry::RegistryError::WebPathCollision { .. }
        ));
    }

    // ── kein Web-Weg erreicht eine nicht registrierte Operation ───────────────

    #[test]
    fn test_no_route_found_for_operation_never_registered() {
        let registry = four_tier_registry();
        let routes = WebRouteTable::from_registry(&registry).unwrap();
        assert!(routes.find("/api/never-registered").is_none());
    }

    // ── Tier-Ablehnungsmatrix ──────────────────────────────────────────────────

    #[test]
    fn test_observer_caller_allowed_on_observer_route() {
        let registry = four_tier_registry();
        let routes = WebRouteTable::from_registry(&registry).unwrap();
        let authz = StaticUidTierMap::new(vec![(1000, PermissionTier::Observer)]);
        let decision = decide_route(
            &routes,
            &authz,
            &peer_with_tier(),
            "/api/observer",
            Some(WebMethod::Get),
        );
        assert!(matches!(decision, RouteDecision::Execute { .. }));
    }

    #[test]
    fn test_observer_caller_denied_on_operator_route() {
        let registry = four_tier_registry();
        let routes = WebRouteTable::from_registry(&registry).unwrap();
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
    }

    #[test]
    fn test_operator_caller_allowed_on_operator_route() {
        let registry = four_tier_registry();
        let routes = WebRouteTable::from_registry(&registry).unwrap();
        let authz = StaticUidTierMap::new(vec![(1000, PermissionTier::Operator)]);
        let decision = decide_route(
            &routes,
            &authz,
            &peer_with_tier(),
            "/api/operator",
            Some(WebMethod::Get),
        );
        assert!(matches!(decision, RouteDecision::Execute { .. }));
    }

    #[test]
    fn test_operator_caller_denied_on_maintainer_route() {
        let registry = four_tier_registry();
        let routes = WebRouteTable::from_registry(&registry).unwrap();
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
    }

    #[test]
    fn test_maintainer_caller_allowed_on_maintainer_route() {
        let registry = four_tier_registry();
        let routes = WebRouteTable::from_registry(&registry).unwrap();
        let authz = StaticUidTierMap::new(vec![(1000, PermissionTier::Maintainer)]);
        let decision = decide_route(
            &routes,
            &authz,
            &peer_with_tier(),
            "/api/maintainer",
            Some(WebMethod::Get),
        );
        assert!(matches!(decision, RouteDecision::Execute { .. }));
    }

    #[test]
    fn test_maintainer_caller_denied_on_owner_route() {
        let registry = four_tier_registry();
        let routes = WebRouteTable::from_registry(&registry).unwrap();
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
    }

    #[test]
    fn test_owner_caller_allowed_on_owner_route() {
        let registry = four_tier_registry();
        let routes = WebRouteTable::from_registry(&registry).unwrap();
        let authz = StaticUidTierMap::new(vec![(1000, PermissionTier::Owner)]);
        let decision = decide_route(
            &routes,
            &authz,
            &peer_with_tier(),
            "/api/owner",
            Some(WebMethod::Get),
        );
        assert!(matches!(decision, RouteDecision::Execute { .. }));
    }

    /// Owner ist die höchste Stufe der Totalordnung — es gibt strukturell
    /// keine Route, die einen Owner-Aufrufer per Tier ablehnen könnte (siehe
    /// `harw_operations::operation::PermissionTier`-Dokumentation:
    /// `Observer < Operator < Maintainer < Owner`). Der „abgelehnte Fall" für
    /// Owner ist deshalb keine höhere Route, sondern ein unbekannter Peer —
    /// dieselbe Ablehnung, die jede andere Stufe ebenfalls treffen kann.
    #[test]
    fn test_owner_tier_route_denied_for_unknown_peer_instead_of_insufficient_tier() {
        let registry = four_tier_registry();
        let routes = WebRouteTable::from_registry(&registry).unwrap();
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
    }

    // ── NotFound / MethodNotAllowed / ApprovalRequired ────────────────────────

    #[test]
    fn test_unknown_path_yields_not_found() {
        let registry = four_tier_registry();
        let routes = WebRouteTable::from_registry(&registry).unwrap();
        let authz = StaticUidTierMap::with_default(vec![], PermissionTier::Owner);
        let decision = decide_route(
            &routes,
            &authz,
            &peer_with_tier(),
            "/api/does-not-exist",
            Some(WebMethod::Get),
        );
        assert!(matches!(decision, RouteDecision::NotFound));
    }

    #[test]
    fn test_wrong_method_yields_method_not_allowed_with_expected_method() {
        let registry = four_tier_registry();
        let routes = WebRouteTable::from_registry(&registry).unwrap();
        let authz = StaticUidTierMap::with_default(vec![], PermissionTier::Owner);
        // /api/observer ist readonly => GET erwartet; POST muss abgelehnt werden.
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
    }

    #[test]
    fn test_unsupported_http_method_yields_method_not_allowed() {
        let registry = four_tier_registry();
        let routes = WebRouteTable::from_registry(&registry).unwrap();
        let authz = StaticUidTierMap::with_default(vec![], PermissionTier::Owner);
        let decision = decide_route(&routes, &authz, &peer_with_tier(), "/api/observer", None);
        assert!(matches!(decision, RouteDecision::MethodNotAllowed { .. }));
    }

    #[test]
    fn test_approval_required_route_is_never_executed_directly() {
        let mut registry = OperationRegistry::new();
        registry.register(std::sync::Arc::new(TierOp {
            name: "irreversible",
            path: "/api/irreversible",
            permission: PermissionTier::Operator,
            readonly: false,
            approval: harw_operations::operation::ApprovalPolicy::Always,
        }));
        let routes = WebRouteTable::from_registry(&registry).unwrap();
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
    }

    #[test]
    fn test_web_method_expected_for_matches_readonly_flag() {
        assert_eq!(WebMethod::expected_for(true), WebMethod::Get);
        assert_eq!(WebMethod::expected_for(false), WebMethod::Post);
        assert_eq!(WebMethod::Get.as_str(), "GET");
        assert_eq!(WebMethod::Post.as_str(), "POST");
    }
}
