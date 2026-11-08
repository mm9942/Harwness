//! Wire-Protokoll zwischen Eskalationsleiter und Warden (Knoten **AW5-02**):
//! der Signierteil.
//!
//! # Aufteilung (PL-100 WP 3.0, Leitprinzip I8)
//! Seit WP 3.0 liegt das signierfreie Vokabular in `harw-dod-warden-types`:
//! Aktionstaxonomie (`action`), Eskalationsstufen (`stage`), Ziel-Tripel
//! (`target`), Ablehnungskategorien (`denial`), Ergebnis einer Aktion
//! (`outcome`) und die kanonischen Kodierungsbausteine. Diese Crate behält
//! alles, was signiert oder prüft — [`SignedAuthorization`], [`KeyRing`],
//! [`ProofKey`], die kanonische MAC-Eingabe ([`canonical::PROOF_DOMAIN`]),
//! [`NonceLedger`], [`ProofPolicy`], die Anfrage- und Antworthüllen
//! ([`WardenRequest`]/[`WardenReply`]) und die v1-Altlast. Sie re-exportiert
//! die verschobenen Module und Typen unter ihren bisherigen Pfaden
//! (`harw_dod_warden_proto::WardenAction`,
//! `harw_dod_warden_proto::action::WardenAction`, …), damit `harw-dod-warden`
//! und `harw-warden` unverändert kompilieren. Prozesse, die mit Modellen
//! sprechen, hängen an `harw-dod-warden-types`, nie an dieser Crate.
//!
//! # Zweck
//! Diese Crate definiert, was über die Leitung zwischen dem
//! Eskalationsleiter (`harw-dod-escalate`, AW5-03) und dem Durchsetzer
//! (`harw-dod-warden`, AW5-04a) geht: die Aktionstaxonomie
//! ([`ProposedAction`]/[`WardenAction`], re-exportiert), die signierte
//! Autorisierung ([`SignedAuthorization`], `signed.rs`) mit ihrem Ziel-Tripel
//! ([`CgroupTarget`], re-exportiert), die Anfrage- und Antworthülle
//! ([`WardenRequest`]/[`WardenReply`], `response.rs`; [`WardenResponse`]
//! re-exportiert), die inhaltsfreie Ablehnungskategorie ([`Denial`],
//! re-exportiert) und die Eskalationsstufen ([`EscalationStage`],
//! re-exportiert; „re-exportiert" heißt: definiert in
//! `harw-dod-warden-types`, siehe oben). Sie führt selbst
//! keine Durchsetzung aus und trifft keine Autorisierungsentscheidung — beides
//! ist Sache der beiden Nachbarknoten.
//!
//! # Proof v2 (C-WPROTO, W3) — maßgeblich
//! Seit W3 ist die einzige akzeptierte Autorisierung
//! [`SignedAuthorization`] (Modul [`signed`]): keyed BLAKE3 über kanonische,
//! längenpräfixierte Bytes (Domain `harw:warden-proof:v2\0`), mit Key-ID
//! ([`KeyRing`], Rotation), Nonce ([`NonceLedger`]), Ablauf (TTL ≤ 120 s,
//! [`ProofPolicy`]) und cgroup-Präfix-Allowlist; Anfragehülle
//! [`WardenRequest`], Antworthülle [`WardenReply`] (beide versioniert, F-088).
//! Prüfreihenfolge und MAC-Eingabe: siehe [`signed`]-Moduldoku.
//!
//! **PL-100 WP 3.1 (W01 erweitert):** Der MAC deckt zusätzlich Befund,
//! Freigebenden, optionale Haltedauer (`None` = bis zur Aufhebung, O3), die
//! Bestätigung „nicht umkehrbar“ (`irreversible_ack`, O4), das Ziel-Tripel aus
//! cgroup-Pfad, Kernel-ID und erster Beobachtung ([`CgroupTarget`], B21) und
//! den Ablaufzeitpunkt ab ([`AuthorizationBody`]). Die Aktionsmenge trennt
//! Auftauen ([`WardenAction::ThawCgroup`]) und Aufheben der Isolation
//! ([`WardenAction::LiftIsolation`]); `ReleaseCgroup` ist stillgelegt.
//! [`WardenRequest::decode`] lehnt jeden v1-Auftrag ausdrücklich ab.
//!
//! **v1-Altlast:** [`AuthorizationProof`], [`WardenActionRequest`],
//! [`ActionContentDigest::content_digest`] (bis WP 3.0 die inhärente Methode
//! `WardenAction::content_digest`) und die Variante `ReleaseCgroup` sind
//! `#[deprecated]`. Seit PL-100 WP 3.0 (Schritt 2) nennt kein Workspace-Crate
//! sie mehr außerhalb von Ablehnungstests: `harw-dod-escalate` liefert nur
//! noch eine unsignierte Freigabe und hängt nicht an dieser Crate. Sie
//! bleiben bis zu ihrer Entfernung nur für die Tests der v1-Ablehnung
//! ([`WardenRequest::decode`]) stehen, sind fälschbar (F-001) und werden von
//! keinem Warden akzeptiert. Die Abschnitte unten, die „Beleg“ sagen,
//! beschreiben diesen v1-Stand.
//!
//! # Zwei Vorprüfungen, bevor hier etwas geschrieben wurde
//!
//! ## Prüfung 1: Nimmt `harw_macros::warden_actions!` `crate::…`-Pfade an?
//! **Ja.** `authorization_proof = <Pfad>;` parst als `syn::Path`
//! (`harw-macros/src/warden_actions.rs:393`), und `syn::Path`s
//! `Parse`-Implementierung behandelt `crate`/`self`/`super` als reguläre,
//! wenn auch reservierte Pfadsegmente (`syn-2.0.117/src/path.rs`,
//! `PathSegment::parse_helper`, Zeilen 514–522: `crate` wird über
//! `Ident::parse_any` gelesen wie `self`/`super`). `crate::AuthorizationProof`
//! wäre also syntaktisch angenommen worden — kein Makrofehler. Das Makro
//! wurde trotzdem **nicht** verwendet, aus einem anderen, in Prüfung 2
//! gefundenen Grund.
//!
//! ## Prüfung 2: `harw-tools`
//! `warden_actions!` erzeugt unbedingt
//! `impl ProposedAction { pub fn tool_schema() -> ::harw_tools::ToolSpec }`
//! (`warden_actions.rs:767–784`) — es gibt in der Deklarationsgrammatik
//! keinen Schalter, der dieses Impl wegließe; jede Crate, die das Makro
//! aufruft, braucht deshalb zwingend `harw-tools` als echte
//! Compile-Zeit-Abhängigkeit, unabhängig davon, ob `tool_schema()` je
//! aufgerufen wird. Eine transitive Zählung von `harw-tools` (Cargo.lock,
//! ohne die Dev-Dependencies der beteiligten Crates, die für einen
//! Konsumenten irrelevant sind) kommt auf 59 eindeutige Crates; oben auf die
//! ohnehin verbindliche Grundausstattung dieser Crate (`harw-types`,
//! `harw-macros`, `serde`, `serde_json`, `jiff` — selbst schon 65 Crates
//! transitiv) käme `harw-tools` noch mit 8 zusätzlichen, heute nicht
//! vorhandenen Crates obendrauf: `harw-sandbox`, `async-trait`, `ipnet`,
//! `tracing`, `tracing-attributes`, `tracing-core`, `valuable`, sowie
//! `harw-tools` selbst. Die Kette `harw-warden` → `harw-dod-warden` →
//! `harw-dod-warden-proto` würde diese acht zusätzlichen Crates vollständig
//! erben, ohne dass der Warden selbst `tool_schema()` je aufruft. Diese
//! Zahl ist zudem eine Momentaufnahme, keine feste Untergrenze:
//! `harw-tools/Cargo.toml` erhielt während dieses Knotens (parallel
//! landender AW5-07-Knoten) zwei weitere Abhängigkeiten
//! (`harw-context`, `harw-lens-types`) — ein Beleg dafür, dass `harw-tools`
//! als geteiltes Werkzeug-Crate strukturell weiterwächst und damit als
//! Warden-Abhängigkeit nur schwerer wird, nie leichter.
//!
//! **Von den drei im Brief genannten Auswegen ist (a) — `tool_schema()`
//! hinter ein Cargo-Feature legen, das der Warden-Pfad nicht einschaltet —
//! nicht verfügbar, ohne das Makro selbst zu ändern**: das generierte Impl
//! hat kein `#[cfg(...)]`, und ein Feature auf *dieser* Crate änderte daran
//! nichts, weil der Typfehler beim Kompilieren entstünde, nicht erst beim
//! Aufruf — `::harw_tools::ToolSpec` müsste als Rückgabetyp auflösen,
//! unabhängig davon, ob irgendein Aufrufer `tool_schema()` je erreicht.
//! **Entschieden: (b) — die Typen von Hand schreiben, ohne das Makro.**
//! `tool_schema()` gehört an eine andere Stelle (voraussichtlich
//! `harw-dod-escalate`, das als einziger der beiden Nachbarn tatsächlich mit
//! einem Modell spricht) — nicht hierher. `harw-macros` bleibt trotzdem
//! Abhängigkeit dieser Crate, aber ausschließlich für
//! `#[derive(harw_macros::HarwError)]` in `error.rs`, nie für
//! `warden_actions!`.
//!
//! **Eine zweite, hier nicht lösbare Beobachtung:** schon die *mandatierte*
//! Grundausstattung dieser Crate (`harw-types` allein zieht `blake3`, `uuid`,
//! `serde`, `serde_json`; `harw-macros` zieht `syn`, `quote`, `proc-macro2`;
//! `jiff` zieht seine eigene Zeitzonen-Kette) kommt bei einer vollständigen,
//! blattgenauen transitiven Zählung auf 65 Crates — weit über der in
//! `docs/design/build-history.md` („Jetzt entschieden", Nr. 1) festgelegten Obergrenze
//! von 12. Das eigentliche Gate (`xtask`, Gate „Warden-Abhängigkeitszahl")
//! existiert zum Zeitpunkt dieses Knotens noch nicht als lauffähiger Code
//! (`xtask/src/gate_privileges.rs` — das strukturell nächstliegende Gate —
//! ist noch ein Platzhalter, der `Err("noch nicht gebaut")` zurückgibt); wie
//! es tatsächlich zählen wird (jeder Blattknoten von crates.io, oder eine
//! gröbere Einheit), ist deshalb offen. Diese Crate hält sich an die vom
//! Brief mandatierte Grundausstattung und vermeidet jede vermeidbare
//! zusätzliche Abhängigkeit (`harw-tools` allen voran) — ob die Obergrenze
//! von 12 damit erreichbar ist, hängt von einer Zählmethode ab, die dieser
//! Knoten nicht festlegt.
//!
//! # Warum keine freien Argumente
//! `ProposedAction`/`WardenAction` (`action.rs`) werden von einem Modell
//! vorgeschlagen (über den künftigen `tool_schema()`-Aufrufer). Ein Feld,
//! in das ein Modell freien Text schreiben kann, ist der Weg, auf dem eine
//! Modellausgabe zu einem Befehl wird — dieselbe Begründung, aus der
//! `warden_actions!` `String` als Feldtyp ablehnt
//! (`harw-macros/src/warden_actions.rs`-Moduldoku, Abschnitt „Warum keine
//! erzeugte Variante freien Text trägt"). Diese Crate hält dieselbe
//! Disziplin von Hand: jedes Aktionsfeld ist eine `harw-types`-Kennung
//! (hier: `CgroupId`), nie ein roher `String`/`str`.
//!
//! # Die Zulässigkeitsmatrix in Worten
//! `FreezeCgroup` und `ThawCgroup` sind ab [`EscalationStage::RuleTriggered`]
//! zulässig; `IsolateNetwork`, `LiftIsolation` und `KillProcessTree` erst ab
//! [`EscalationStage::Escalated`]. Begründung je Aktion in `action.rs`,
//! Abschnitt „Die Zulässigkeitsmatrix in Worten". Reversibilität:
//! `FreezeCgroup` (über `ThawCgroup`) und `IsolateNetwork` (über
//! `LiftIsolation`) sind reversibel; `KillProcessTree` ist es ausdrücklich
//! nicht (`action.rs`, [`Reversibility`]). Kill und Isolation verlangen
//! trotzdem beide das signierte `irreversible_ack`
//! ([`WardenAction::requires_irreversible_ack`]).
//!
//! # Warum Ablehnungen inhaltsfrei sind
//! Der Durchsetzer läuft privilegiert; was er meldet, verlässt seinen
//! Vertrauensbereich. [`Denial`] (`denial.rs`) ist deshalb strukturell
//! feldlos — keine Variante *kann* ein Detail tragen, unabhängig von
//! Konvention oder Disziplin an der Aufrufstelle. Der feinkörnige Grund
//! bleibt in [`error::WardenProtoError`], einem Typ, der nie serialisiert
//! wird und den Prozess nie verlässt; [`error::WardenProtoError::as_denial`]
//! ist die einzige, bewusst verlustbehaftete Brücke zwischen beiden (siehe
//! `error.rs`-Moduldoku).
//!
//! # Wer einen Beleg ausstellen darf
//! Diese Crate definiert [`AuthorizationProof`], sie stellt keinen aus
//! (Invariante S1: Autorisierung entsteht an genau einem Ort). Es gibt hier
//! keine Funktion, die eine Autorisierungs*entscheidung* trifft —
//! [`AuthorizationProof::new`] setzt fünf bereits getroffene Fakten
//! zusammen, ohne selbst eine Leiter zu befragen oder eine Systemuhr zu
//! lesen. Bis PL-100 WP 3.0 rief ihn ausschließlich
//! `harw_dod_escalate::authorize` (AW5-03, dort `pub(crate)`) auf; seitdem
//! ruft ihn kein produktiver Code mehr auf. Den gültigen v2-Auftrag
//! ([`SignedAuthorization`]) stellt nur der Halter eines [`ProofKey`] aus
//! (`harw-escalator`, WP 3.2) — siehe `signed.rs`-Moduldoku.
//!
//! # Nebenläufigkeit
//! Reine, unveränderliche Werttypen. Kein internes Locking, keine geteilten
//! Ressourcen, keine Systemuhr (`jiff::Timestamp`-Werte werden immer als
//! Parameter entgegengenommen, nie über `Timestamp::now()` gelesen). Alle
//! Typen sind `Send + Sync` (jedes Feld ist es).
//!
//! # Fehler
//! [`error::WardenProtoError`] — siehe dessen Moduldoku.

pub mod canonical;
pub mod error;
pub mod proof;
pub mod response;
pub mod signed;

// Signierfreie Module aus `harw-dod-warden-types` (PL-100 WP 3.0), unter den
// bisherigen Modulpfaden re-exportiert; `crate::action::…` usw. lösen
// innerhalb dieser Crate weiterhin auf.
pub use harw_dod_warden_types::{action, denial, outcome, stage, target};

pub use error::{ProofError, ProofResult, WardenProtoError, WardenProtoResult};
pub use harw_dod_warden_types::{
    CgroupKernelId, CgroupTarget, Denial, EscalationStage, ProposedAction, Reversibility,
    WardenAction, WardenTypesError,
};
// v1-Altlast: bleibt erreichbar, die Warnung trifft jeden Aufrufer.
#[allow(deprecated)]
pub use proof::{ActionContentDigest, AuthorizationProof};
// v1-Altlast: bleibt erreichbar, die Warnung trifft jeden Aufrufer.
#[allow(deprecated)]
pub use response::WardenActionRequest;
pub use response::{WardenActionAudit, WardenReply, WardenRequest, WardenResponse};
pub use signed::{
    AuthorizationBody, KeyId, KeyRing, MAX_CLOCK_SKEW_SECS_CAP, MAX_TTL_SECS_CAP,
    MemoryNonceLedger, NonceLedger, ProofKey, ProofPolicy, SignedAuthorization,
    VerifiedAuthorization, WARDEN_PROTOCOL_VERSION,
};

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
