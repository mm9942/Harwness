//! Integrationstest: die zentrale Zusage von `harw-dod-netpolicy` —
//! `plan_for_scope` erlaubt nie mehr als der Bereich, aus dem er stammt.
//!
//! Eigenständiger Testfall (kein Root, kein Netz, kein Kernel), siehe der
//! Auftrag für Knoten AW3-02.

use std::net::IpAddr;

use harw_dod_netpolicy::plan_for_scope;
use harw_authority::NetworkScope;

// Baut einen `NetworkScope` über dessen öffentlichen `serde`-Wire-Vertrag
// (siehe `harw_authority::EgressTarget`s `Serialize`/`Deserialize`-Kommentar
// in `harw-sandbox/src/lib.rs`): ein führendes `=` ergibt `Host`, ein
// gültiges CIDR-Literal ergibt `Cidr`, alles andere `DnsSuffix`. Das ist der
// einzige Weg, aus einer anderen Crate einen Bereich mit `Host`- oder
// `Cidr`-Einträgen zu bauen, weil `NetworkScope` dafür keine öffentliche
// Konstruktormethode anbietet.
fn scope_of(entries: &[&str]) -> NetworkScope {
    serde_json::from_value(serde_json::json!({ "allow_hosts": entries }))
        .expect("valid scope wire fixture")
}

// Repräsentative Sonden je Bereich: Namen, von denen einige vom jeweiligen
// Bereich zugelassen werden und einige nicht. Die Eigenschaft muss für
// beide Fälle halten; für den nicht-zugelassenen Fall ist sie trivial, für
// den zugelassenen Fall ist es die eigentliche Prüfung.
const PROBE_HOSTS: &[&str] = &[
    "docs.rs",
    "static.docs.rs",
    "a.b.docs.rs",
    "evildocs.rs",
    "docs.rs.evil.com",
    "crates.io",
    "api.example.com",
    "sub.api.example.com",
    "example.com",
    "other.example.com",
    "evil.com",
    "",
];

fn probe_addrs() -> Vec<IpAddr> {
    [
        "10.1.2.3",
        "10.255.255.255",
        "11.0.0.0",
        "192.168.1.42",
        "192.168.2.1",
        "127.0.0.1",
        "2001:db8::1",
        "2001:db9::1",
        "::1",
    ]
    .into_iter()
    .map(|literal| literal.parse().expect("valid IP literal in test fixture"))
    .collect()
}

// Die fünf konstruierten Bereiche, gegen die beide Eigenschaftstests unten
// laufen: leer, reine `DnsSuffix`-Liste, ein einzelnes `Host`-Ziel, reine
// `Cidr`-Bereiche, und eine Mischung aus allen drei Zielarten.
fn constructed_scopes() -> Vec<NetworkScope> {
    vec![
        NetworkScope::empty(),
        NetworkScope::from_hosts(["docs.rs".to_owned(), "crates.io".to_owned()]),
        scope_of(&["=api.example.com"]),
        scope_of(&["10.0.0.0/8", "192.168.1.0/24"]),
        scope_of(&["example.com", "=other.example.com", "2001:db8::/32"]),
    ]
}

#[test]
fn test_property_plan_never_allows_more_than_scope() {
    for (index, scope) in constructed_scopes().iter().enumerate() {
        let plan = plan_for_scope(scope);

        for host in PROBE_HOSTS {
            if plan.allows_host(host) {
                assert!(
                    scope.allows(host),
                    "scope #{index} denies host {host:?} that its own derived plan allows"
                );
            }
        }

        for addr in probe_addrs() {
            if plan.allows_addr(addr) {
                assert!(
                    scope.allows_addr(addr),
                    "scope #{index} denies address {addr} that its own derived plan allows"
                );
            }
        }
    }
}

#[test]
fn test_property_plan_and_scope_agree_exactly_on_probes() {
    // Stärkere Prüfung als die reine Teilmengen-Eigenschaft oben: für jede
    // Sonde stimmen Plan und Bereich exakt überein, nicht nur einseitig.
    // Das gilt, weil `plan_for_scope` jede Zielart verlustfrei kopiert
    // (siehe `NetRule::from`) und `NetPlan::allows_host`/`allows_addr`
    // dieselbe Vergleichsregel wie `NetworkScope` implementieren (siehe die
    // Moduldoc von `harw_dod_netpolicy`, Abschnitt „Ein zweites,
    // unabhängiges Kopplungsrisiko").
    for scope in &constructed_scopes() {
        let plan = plan_for_scope(scope);
        for host in PROBE_HOSTS {
            assert_eq!(plan.allows_host(host), scope.allows(host), "host {host:?}");
        }
        for addr in probe_addrs() {
            assert_eq!(plan.allows_addr(addr), scope.allows_addr(addr), "addr {addr}");
        }
    }
}
