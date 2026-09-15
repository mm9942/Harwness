//! `NetPlan`: der inspizierbare, unveränderliche Netzplan-Wert.
//!
//! Enthält [`NetPlan`] selbst und [`plan_for_scope`], die einzige Brücke von
//! `harw_sandbox::NetworkScope` zu dieser Crate. Siehe die Moduldoc von
//! `crate` für die Gesamt-Architektur und die Zusage, die dieser Knoten
//! trägt.

use std::net::IpAddr;

use harw_sandbox::{EgressTarget, NetworkScope};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::rule::{normalize_host, NetRule};

/// Ein inspizierbarer Netzplan.
///
/// # Description
/// Ein reiner Wert: eine geordnete Liste von [`NetRule`]s. `NetPlan` wendet
/// nichts an, öffnet keinen Socket, ruft keinen externen Prozess auf —
/// Anwenden ist Sache eines [`crate::NetBackend`]. `#[serde(deny_unknown_fields)]`
/// lässt ein zukünftiges, unbekanntes Feld beim Deserialisieren fehlschlagen
/// statt es stillschweigend zu verwerfen (fail-closed bei
/// Konfigurationsdrift).
///
/// # Vergleich
/// [`PartialEq`]/[`Eq`] vergleichen die volle, geordnete `rules`-Liste.
/// [`plan_for_scope`] liefert für gleiche Bereiche immer dieselbe
/// Reihenfolge (siehe dort), daher ist der Vergleich für aus demselben
/// Bereich gebaute Pläne aussagekräftig.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetPlan {
    /// Die Regeln dieses Plans, in stabiler Reihenfolge (siehe
    /// [`plan_for_scope`]).
    pub rules: Vec<NetRule>,
}

impl NetPlan {
    /// Prüft, ob dieser Plan den gegebenen Hostnamen zulässt.
    ///
    /// # Description
    /// Normalisiert `host` identisch zu
    /// `harw_sandbox::NetworkScope::allows` und delegiert dann an jede
    /// Regel. Für einen per [`plan_for_scope`] gebauten Plan gilt: dieses
    /// Ergebnis ist identisch zu `scope.allows(host)` des Quell-Bereichs —
    /// das ist die Grundlage der Eigenschaft, dass ein Plan nie mehr
    /// erlaubt als sein Bereich (siehe Moduldoc von `crate`).
    ///
    /// # Arguments
    /// - `host` (`&str`): Hostname ohne Port und Userinfo. Ein nicht
    ///   auswertbarer oder leerer Host liefert `false`.
    ///
    /// # Returns
    /// `true`, wenn mindestens eine Regel den Host zulässt.
    ///
    /// # Panics
    /// Keine: ausschließlich prüfende Slice-Zugriffe.
    ///
    /// # Examples
    /// ```rust
    /// use harw_sandbox::NetworkScope;
    /// use harw_dod_netpolicy::plan_for_scope;
    ///
    /// let scope = NetworkScope::from_hosts(["docs.rs".to_owned()]);
    /// let plan = plan_for_scope(&scope);
    /// assert!(plan.allows_host("static.docs.rs"));
    /// assert!(!plan.allows_host("evil.com"));
    /// ```
    #[must_use]
    pub fn allows_host(&self, host: &str) -> bool {
        let needle = normalize_host(host);
        if needle.is_empty() {
            return false;
        }
        self.rules.iter().any(|rule| rule.allows_host(&needle))
    }

    /// Prüft, ob dieser Plan die gegebene Adresse zulässt.
    ///
    /// # Arguments
    /// - `addr` (`std::net::IpAddr`): die zu prüfende Adresse, IPv4 oder
    ///   IPv6.
    ///
    /// # Returns
    /// `true`, wenn mindestens eine [`NetRule::AllowCidr`]-Regel `addr`
    /// enthält.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_netpolicy::{NetPlan, NetRule};
    ///
    /// let plan = NetPlan {
    ///     rules: vec![NetRule::AllowCidr { cidr: "10.0.0.0/8".parse().unwrap() }],
    /// };
    /// assert!(plan.allows_addr("10.1.2.3".parse().unwrap()));
    /// assert!(!plan.allows_addr("11.0.0.0".parse().unwrap()));
    /// ```
    #[must_use]
    pub fn allows_addr(&self, addr: IpAddr) -> bool {
        self.rules.iter().any(|rule| rule.allows_addr(addr))
    }
}

/// Erzeugt den Plan, der genau diesen Sandkasten-Bereich durchsetzt.
///
/// # Description
/// `NetworkScope` bietet absichtlich keine Methode, die seine
/// `EgressTarget`-Werte direkt herausgibt: `hosts()` verwirft `Cidr`-Ziele
/// und vermischt `Host`/`DnsSuffix` zu einfachen Namen, und es gibt keinen
/// öffentlichen Konstruktor für `Host`- oder `Cidr`-Ziele außerhalb von
/// `harw-sandbox` selbst. Der einzige verlustfreie, öffentliche Weg, den
/// Inhalt eines beliebigen `NetworkScope` zu lesen, ist sein eigener,
/// dokumentierter `serde`-Wire-Vertrag (siehe der Kommentar über
/// `impl Serialize for EgressTarget` in `harw-sandbox/src/lib.rs`): ein
/// führendes `=` kodiert `Host`, kanonische CIDR-Notation kodiert `Cidr`,
/// alles andere ist `DnsSuffix`. Diese Funktion serialisiert `scope`, liest
/// das `allow_hosts`-Array roh aus und deserialisiert jeden Eintrag über
/// `harw_sandbox`s eigenes, bereits getestetes `EgressTarget`-`Deserialize`
/// zurück — statt dessen `=`/CIDR/Suffix-Entscheidungslogik hier ein
/// zweites Mal (und potenziell abweichend) nachzubilden. Jeder Eintrag wird
/// anschließend über [`NetRule::from`] verlustfrei in eine `NetRule`
/// übersetzt (siehe dort für die `DnsSuffix`/`Host`-Entscheidung dieser
/// Crate).
///
/// Jede unerwartete Form — eine fehlgeschlagene Serialisierung, ein
/// fehlendes oder falsch geformtes `allow_hosts`-Feld, ein Eintrag, der sich
/// nicht als `EgressTarget` lesen lässt — führt zum Verwerfen genau dieses
/// einen Eintrags, nie zu einem Panic. Das ist fail-closed im Sinne dieses
/// Knotens: ein übersehener Eintrag macht den resultierenden Plan enger,
/// nie weiter, als der Bereich es erlauben würde.
///
/// # Arguments
/// - `scope` (`&NetworkScope`): der durchzusetzende Bereich.
///
/// # Returns
/// Einen [`NetPlan`], dessen Regeln den Zielen von `scope` eins-zu-eins
/// entsprechen, in der von der internen `BTreeSet`-Ordnung von `scope`
/// vorgegebenen, stabilen Reihenfolge. Ein leerer Bereich ergibt einen
/// leeren Plan, dessen [`NetPlan::allows_host`] und [`NetPlan::allows_addr`]
/// für jede Eingabe `false` liefern.
///
/// # Determinism
/// Zwei inhaltsgleiche Bereiche ergeben immer denselben Plan mit derselben
/// Regelreihenfolge: die Reihenfolge stammt aus `harw-sandbox`s
/// `BTreeSet<EgressTarget>`, die für gleiche Mengen unabhängig von der
/// Einfügereihenfolge gleich sortiert. Diese Funktion selbst führt keine
/// Netzwerk-I/O aus (kein DNS, kein Socket) und ist deshalb ohne Netz und
/// ohne Root testbar.
///
/// # Panics
/// Keine.
///
/// # Examples
/// ```rust
/// use harw_sandbox::NetworkScope;
/// use harw_dod_netpolicy::plan_for_scope;
///
/// let scope = NetworkScope::empty();
/// let plan = plan_for_scope(&scope);
/// assert!(plan.rules.is_empty());
/// ```
#[must_use]
pub fn plan_for_scope(scope: &NetworkScope) -> NetPlan {
    NetPlan {
        rules: wire_targets(scope).into_iter().map(NetRule::from).collect(),
    }
}

// Liest die Zielarten von `scope` über dessen öffentlichen `serde`-Vertrag
// aus (siehe `plan_for_scope`-Dokumentation für die Begründung dieses
// Zugriffswegs). Jede unerwartete Form ergibt eine leere oder verkürzte
// Liste statt eines Panics.
fn wire_targets(scope: &NetworkScope) -> Vec<EgressTarget> {
    let Ok(Value::Object(mut root)) = serde_json::to_value(scope) else {
        return Vec::new();
    };
    let Some(Value::Array(items)) = root.remove("allow_hosts") else {
        return Vec::new();
    };
    items
        .into_iter()
        .filter_map(|item| serde_json::from_value::<EgressTarget>(item).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Baut einen `NetworkScope` über dessen öffentlichen `serde`-Wire-Vertrag
    // (siehe `plan_for_scope`-Dokumentation): der einzige Weg, aus dieser
    // Crate einen Bereich mit `Host`- oder `Cidr`-Einträgen zu bauen, weil
    // `NetworkScope` dafür keine öffentliche Konstruktormethode anbietet.
    fn scope_of(entries: &[&str]) -> NetworkScope {
        serde_json::from_value(serde_json::json!({ "allow_hosts": entries }))
            .expect("valid scope wire fixture")
    }

    fn addr(literal: &str) -> IpAddr {
        literal.parse().expect("valid IP literal in test fixture")
    }

    #[test]
    fn test_plan_for_scope_empty_scope_allows_nothing() {
        let plan = plan_for_scope(&NetworkScope::empty());

        assert!(plan.rules.is_empty());
        assert!(!plan.allows_host("docs.rs"));
        assert!(!plan.allows_host("anything.at.all"));
        assert!(!plan.allows_addr(addr("10.0.0.1")));
    }

    #[test]
    fn test_plan_for_scope_cidr_target_becomes_allow_cidr_rule() {
        let scope = scope_of(&["10.0.0.0/8"]);
        let plan = plan_for_scope(&scope);

        assert_eq!(
            plan.rules,
            vec![NetRule::AllowCidr {
                cidr: "10.0.0.0/8".parse().expect("valid CIDR literal")
            }]
        );
        assert!(plan.allows_addr(addr("10.1.2.3")));
        assert!(!plan.allows_addr(addr("11.0.0.0")));
    }

    #[test]
    fn test_plan_for_scope_host_and_dns_suffix_targets_are_distinguished() {
        let scope = scope_of(&["=api.example.com", "docs.rs"]);
        let plan = plan_for_scope(&scope);

        assert_eq!(
            plan.rules,
            vec![
                NetRule::AllowHost {
                    host: "api.example.com".to_owned()
                },
                NetRule::AllowDnsSuffix {
                    suffix: "docs.rs".to_owned()
                },
            ]
        );

        // Host: nur der exakte Name, keine Subdomain.
        assert!(plan.allows_host("api.example.com"));
        assert!(!plan.allows_host("sub.api.example.com"));

        // DnsSuffix: der Name selbst und alles darunter an einer
        // Punktgrenze.
        assert!(plan.allows_host("docs.rs"));
        assert!(plan.allows_host("static.docs.rs"));
        assert!(!plan.allows_host("evildocs.rs"));
    }

    #[test]
    fn test_net_plan_serde_roundtrip() {
        let plan = NetPlan {
            rules: vec![
                NetRule::AllowHost {
                    host: "api.example.com".to_owned(),
                },
                NetRule::AllowCidr {
                    cidr: "10.0.0.0/8".parse().expect("valid CIDR literal"),
                },
            ],
        };

        let json = serde_json::to_string(&plan).expect("plan serializes");
        let restored: NetPlan = serde_json::from_str(&json).expect("plan deserializes");
        assert_eq!(plan, restored);
    }

    #[test]
    fn test_net_plan_rejects_unknown_field() {
        let json = r#"{"rules": [], "unexpected": true}"#;
        let result: Result<NetPlan, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_plan_for_scope_is_deterministic_for_equal_scopes() {
        let first = scope_of(&["docs.rs", "=api.example.com", "10.0.0.0/8"]);
        // Andere Eingabereihenfolge, gleicher Bereich (die zugrunde
        // liegende `BTreeSet` sortiert intern) — muss denselben Plan mit
        // derselben Regelreihenfolge ergeben.
        let second = scope_of(&["10.0.0.0/8", "=api.example.com", "docs.rs"]);

        let plan_a = plan_for_scope(&first);
        let plan_b = plan_for_scope(&second);

        assert_eq!(plan_a, plan_b);
        assert_eq!(plan_a.rules, plan_b.rules);
    }
}
