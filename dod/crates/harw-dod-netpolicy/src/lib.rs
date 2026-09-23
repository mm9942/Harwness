//! Planwert für Netzregeln: beschreibt, wendet aber nichts an.
//!
//! # Zweck
//! [`NetPlan`] ist eine reine Datenstruktur, die beschreibt, welche
//! Egress-Regeln gelten sollen — eine geordnete Liste von [`NetRule`]s.
//! [`plan_for_scope`] baut den Plan, der genau einen
//! `harw_authority::NetworkScope` durchsetzt. Diese Crate erzeugt dabei
//! **keinen** Kernel-Zustand, öffnet **keinen** Netlink-Socket und ruft
//! **kein** `nft` auf.
//!
//! # Warum diese Crate nichts anwendet
//! Eine Regelmenge, die man nur durch Anwenden prüfen kann, ist auf einem
//! Entwicklungsrechner nicht prüfbar — man bräuchte Root und würde das
//! echte Netz umkonfigurieren. Ein Plan als Wert ist mit gewöhnlichen Tests
//! prüfbar, ohne Berechtigungen und ohne Nebenwirkung: siehe die
//! Eigenschaftstests in `tests/net_policy_property.rs` für die zentrale
//! Zusage, die dieser Knoten trägt. Das Anwenden gehört in den
//! Durchsetzer-Knoten (AW5-04a), im richtigen Prozess, mit den richtigen
//! Rechten.
//!
//! # Wie ein Backendwechsel aussieht
//! [`NetBackend`] ist die einzige Nahtstelle zwischen einem [`NetPlan`] und
//! echter Mechanik. [`NetPlan`] und [`NetRule`] enthalten ausschließlich
//! backend-neutrale Typen (`String`, `ipnet::IpNet`) — keinen
//! nftables-Handle, kein Netlink-Socket, nichts, das nur zu einem Backend
//! passt. Ein neues Backend implementiert `NetBackend` in einem eigenen
//! Crate; weder `NetPlan` noch `NetRule` müssen dafür ein Feld oder eine
//! Variante gewinnen. Woran man erkennt, dass ein Wechsel ein Austausch
//! bleibt: die bestehenden [`InspectBackend`]-Tests kompilieren und
//! bestehen unverändert, und das neue Backend lässt sich in denselben
//! Tests (Plan hinein, `apply` aufrufen, Ergebnis prüfen) einsetzen, ohne
//! dass Test-Fixtures (`NetPlan`-Werte) angepasst werden müssten. Sobald
//! ein Backend-Wechsel eine neue `NetRule`-Variante oder ein neues
//! `NetPlan`-Feld verlangt, ist er keine Austausch mehr, sondern eine
//! Umschreibung — dann ist der Trait falsch geschnitten.
//!
//! # Umgang mit `DnsSuffix` (und `Host`)
//! Ein `EgressTarget::DnsSuffix` (und ebenso `EgressTarget::Host`) lässt
//! sich nicht direkt in eine Paketfilterregel übersetzen: Namen sind zur
//! Filterzeit nicht bekannt, ein Paket trägt eine Zieladresse, keinen
//! Hostnamen. Drei naheliegende Antworten und warum diese Crate keine der
//! ersten beiden wählt:
//!
//! 1. **Namensauflösung zur Planzeit** (den Namen hier auflösen und als
//!    `AllowCidr` einbetten) — verworfen. Das würde Netzzugriff *in diese
//!    Crate* holen, genau das, was die Trennung von Plan und Anwendung
//!    vermeiden soll (siehe oben). Es würde außerdem die
//!    Determinismus-Zusage brechen (zwei gleiche Bereiche → derselbe Plan;
//!    eine Namensauflösung kann je nach Zeitpunkt, Resolver-Cache oder
//!    Lastverteilung unterschiedliche Adressen liefern). Am schwersten
//!    wiegt: eine im Plan eingefrorene Adresse kann durch DNS-Rotation
//!    veralten — entweder sperrt sie legitimen Verkehr aus
//!    (Verfügbarkeitsproblem) oder, schlimmer, sie erlaubt weiterhin eine
//!    Adresse, die der Name inzwischen nicht mehr benennt (genau die Art
//!    von stillem Mehr-Erlauben, die dieser Knoten verhindern soll).
//! 2. **Stillschweigend verwerfen** (den Eintrag einfach nicht in den Plan
//!    aufnehmen) — verworfen, weil eine stillschweigend engere
//!    Durchsetzung als der Sandkasten verspricht ein eigenes, nur schwerer
//!    zu findendes Problem ist (der Sandkasten meldet „Netz zu
//!    example.com erlaubt", tatsächlich blockt jede Firewall alles) — eine
//!    ehrlich benannte Lücke ist besser als eine stillschweigende.
//! 3. **Eigene Regelart, unverändert weitergereicht** — die gewählte
//!    Antwort. [`NetRule::AllowHost`] und [`NetRule::AllowDnsSuffix`]
//!    tragen den Namen unverändert in den Plan. Der Plan bleibt damit
//!    wahrheitsgemäß (er sagt genau das, was der Bereich erlaubt, nicht
//!    mehr und nicht weniger) und deterministisch (kein Netzzugriff
//!    nötig).
//!
//! **Die Grenze dieser Entscheidung:** ein Backend, das nur adressbasierte
//! Paketfilterregeln schreiben kann (z. B. reines nftables ohne
//! DNS-Anbindung), kann [`NetRule::AllowHost`]/[`NetRule::AllowDnsSuffix`]
//! *nicht* korrekt durchsetzen. Der Vertrag von [`NetBackend::apply`]
//! verlangt deshalb: ein solches Backend muss eine solche Regel entweder
//! mit [`NetPolicyError::PlanRejected`] ablehnen (fail-closed) oder mit
//! einem DNS-bewussten Mechanismus koppeln (ein Resolver-Daemon, der eine
//! benannte Menge/ipset synchron hält, oder ein lokaler Forward-Proxy mit
//! SNI-/Host-Header-Prüfung) — beides Sache des Durchsetzer-Knotens
//! (AW5-04a), nicht dieser Crate. Was ein Backend **nie** darf: eine
//! namensbasierte Regel stillschweigend ignorieren und trotzdem `Ok(())`
//! melden, oder sie in eine andere, weitere Regel übersetzen.
//!
//! # Ein zweites, unabhängiges Kopplungsrisiko
//! [`NetPlan::allows_host`]/[`NetPlan::allows_addr`] reimplementieren
//! notwendigerweise dieselbe Punktgrenzen-Vergleichsregel wie
//! `harw_authority::NetworkScope::allows` — `harw-sandbox` exportiert diese
//! Hilfsfunktionen nicht öffentlich. [`plan_for_scope`] selbst braucht
//! diese Kopie nicht: sie liest die Zielarten stattdessen über den
//! öffentlichen `serde`-Wire-Vertrag von `EgressTarget` aus (siehe
//! [`plan_for_scope`]s Dokumentation) und ruft `harw_sandbox`s eigenes
//! `Deserialize`-Impl auf, statt dessen `=`/CIDR/Suffix-Logik hier ein
//! zweites Mal zu schreiben. Nur die *Abfrage*-Semantik
//! (`allows_host`/`allows_addr`) ist dupliziert, weil ein `NetPlan` als
//! eigenständiger Wert ohne den Quell-`NetworkScope` auskommen muss — der
//! Durchsetzer-Knoten bekommt nur den Plan, nicht den Bereich. Sollte
//! `harw-sandbox` seine Punktgrenzen-Regel ändern, ohne dass diese Crate
//! angepasst wird, laufen beide Definitionen auseinander — es gibt keinen
//! gemeinsamen Codepfad, der das verhindert. Die Eigenschaftstests in
//! `tests/net_policy_property.rs` sind die einzige Absicherung gegen
//! dieses Auseinanderlaufen und sollten bei jeder Änderung an
//! `harw_authority::NetworkScope::allows` erneut ausgeführt werden.
//!
//! # Nebenläufigkeit
//! [`NetPlan`] und [`NetRule`] sind reine Werte ohne innere
//! Veränderlichkeit: `Send + Sync`, beliebig teilbar. [`InspectBackend`]
//! hält seine Aufzeichnung hinter einem `Mutex` und ist damit sicher aus
//! mehreren Threads gleichzeitig aufrufbar (siehe dessen Dokumentation).
//!
//! # Fehler
//! [`NetPolicyError`] ist der einzige Fehlertyp dieser Crate; er
//! beschreibt ausschließlich, warum [`NetBackend::apply`] fehlschlagen
//! kann. [`plan_for_scope`] selbst ist unfehlbar.
//!
//! # Examples
//! ```rust
//! use harw_authority::NetworkScope;
//! use harw_dod_netpolicy::{plan_for_scope, InspectBackend, NetBackend};
//!
//! let scope = NetworkScope::from_hosts(["docs.rs".to_owned()]);
//! let plan = plan_for_scope(&scope);
//!
//! let backend = InspectBackend::new();
//! backend.apply(&plan).expect("InspectBackend schlägt nie fehl");
//! assert_eq!(backend.recorded_plans(), vec![plan]);
//! ```

#![forbid(unsafe_code)]

mod backend;
mod error;
mod plan;
mod rule;

pub use backend::{InspectBackend, NetBackend};
pub use error::{NetPolicyError, NetPolicyResult};
pub use plan::{NetPlan, plan_for_scope};
pub use rule::NetRule;

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
