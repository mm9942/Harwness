//! Routender Sink: verteilt Messwerte nach ihrem Namensraum auf
//! verschiedene Ziel-Sinks.
//!
//! # Verantwortungsbereich
//! Trägt [`RoutingSink`], [`ExportApproval`], [`PROTECTED_NAMESPACES`] und
//! den Nullzähler [`SECURITY_METRIC_LEAKED`] (Knoten AW5-06). Kennt kein
//! Backend selbst — jedes Ziel ist ein beliebiger, bereits fertiger
//! `Arc<dyn TelemetrySink>` (z. B. `harw-observe-file::FileSink` oder
//! `harw-observe-prom::PromSink`); `RoutingSink` verteilt nur, es schreibt
//! nie selbst auf ein Backend.
//!
//! # Zweck des Routings
//! Ein einzelner Prozess trägt typischerweise Metriken aus mehreren
//! Namensräumen (`app.*`, `security.*`, `warden.*`, …), die nicht alle an
//! dasselbe Ziel gehören sollen: Betriebsmetriken dürfen nach außen
//! (Prometheus-Scrape), Sicherheitstelemetrie soll das nicht automatisch.
//! [`RoutingSink`] bindet Namensraum-Präfixe an Ziel-Sinks und fällt für
//! alles Unbenannte auf einen Standard-Sink zurück — eine einzige Stelle,
//! die entscheidet, *wohin* ein Messwert geht, statt dass jeder Emittent
//! selbst einen Sink auswählen müsste.
//!
//! # Warum `security.*` und `warden.*` geschützt sind
//! Sicherheitstelemetrie beschreibt, *was ein Angreifer ausgelöst hat* —
//! und damit indirekt, was die Sensorik sieht und was nicht. Ein
//! Metrikendpunkt, der `security.intrusion_attempt_total` nach außen gibt,
//! sagt einem Angreifer, ob er bemerkt wurde; das ist eine Rückmeldung, die
//! man nicht geben will. Deshalb gehen [`PROTECTED_NAMESPACES`] per
//! Voreinstellung ausschließlich in den Sink, der [`RoutingSink::new`] als
//! `default_sink` übergeben wird (im Betrieb der File-Sink — siehe
//! [`RoutingSink::new`]-Doku). Ein Export nach außen entsteht nur über
//! [`RoutingSink::approve_export`] mit einem echten [`ExportApproval`].
//! Entscheidend: **ein gewöhnlicher [`RoutingSink::route`]-Aufruf für einen
//! geschützten Präfix hat keine Wirkung auf die Zustellung** — die
//! Voreinstellung entscheidet hier, was in der Praxis passiert, nicht eine
//! Konfigurationszeile, die man vergessen oder falsch setzen könnte. Trifft
//! `record()` trotzdem auf einen solchen (wirkungslosen, aber vorhandenen)
//! Eintrag, ist das ein Zeichen einer Fehlkonfiguration, kein normaler
//! Betriebszustand — sichtbar gemacht über [`SECURITY_METRIC_LEAKED`]
//! (siehe unten).
//!
//! # Warum die Routing-Tabelle unveränderlich ist
//! [`RoutingSink`] wird über `Arc<dyn TelemetrySink>` geteilt und aus
//! beliebigen Threads gleichzeitig gerufen (Vertrag A.3:
//! `TelemetrySink::record` bekommt nur `&self`). [`RoutingSink::route`] und
//! [`RoutingSink::approve_export`] verlangen dagegen `&mut self` — sie sind
//! nur aufrufbar, solange der Aufrufer eine exklusive Referenz auf den noch
//! nicht geteilten `RoutingSink` hält, also ausschließlich in der
//! Aufbauphase, bevor er in ein `Arc<dyn TelemetrySink>` verpackt und
//! geteilt wird. Danach erzwingt der Rust-Ausleihchecker die
//! Unveränderlichkeit, ohne dass diese Crate selbst etwas dafür prüfen
//! müsste: `Arc::get_mut` gibt nur bei Referenzzähler `1` eine `&mut`-Sicht
//! zurück, und sobald der Sink über mehrere Threads geteilt ist, bleibt der
//! Zähler dauerhaft über `1`. Eine Route, die zur Laufzeit umgehängt werden
//! könnte, wäre ein zweiter Weg, den Schutz von `security.*`/`warden.*`
//! aufzuheben — ein Aufrufer, der `record()` beobachtet (nur `&self`),
//! könnte nie beweisen, dass die Tabelle zwischen zwei Aufrufen gleich
//! geblieben ist. Die Bauart macht das von vornherein unmöglich, statt es
//! nur zu versprechen.
//!
//! # Wie eine Bestätigung aussieht
//! [`ExportApproval`] ist nur über [`ExportApproval::granted`]
//! konstruierbar und trägt den freigegebenen Namensraum sowie den
//! Bezeichner des Operators, der freigegeben hat. Diese Crate kann einen
//! Operator-Beleg **nicht selbst prüfen**: der Baum trägt dafür bereits
//! `ApprovalActor`/`ApprovalStore` (`harw-session-store`) und
//! `PermissionTier` (`harw-operations`) — aber beide Crates hängen
//! (transitiv über `harw-session-store`, das `harw-observe` für
//! `TraceContext` referenziert) von `harw-observe` ab. Eine Kante von hier
//! auf eine der beiden wäre ein Zyklus (`harw-session-store` →
//! `harw-observe` → `harw-session-store`). [`ExportApproval::granted`]
//! prüft deshalb nichts selbst; es verlangt nur, dass der Aufrufer explizit
//! benennt, wer freigegeben hat. Die eigentliche Prüfung — Existenz einer
//! aufgelösten `ApprovalResolutionRecord` mit `ReviewDecision::Approved`
//! bzw. `ApprovedOnce`, ausgestellt an einen Akteur mit ausreichender
//! `PermissionTier` — muss an der Kompositionsstelle passieren, die sowohl
//! `harw-observe` als auch `harw-session-store`/`harw-operations` sieht
//! (z. B. `harw-cli`), bevor sie [`ExportApproval::granted`] überhaupt
//! aufruft. Siehe die Typ-Doku von [`ExportApproval`] für Details.
//!
//! # Warum die Punktgrenzen-Prüfung hier ein zweites Mal steht
//! `harw-sandbox::EgressTarget::matches_host` löst dasselbe Problem für
//! Hostnamen (`docs.rs` passt auf `api.docs.rs`, nicht auf `evildocs.rs`):
//! ein Präfix-/Suffix-Vergleich muss an einer Trennzeichen-Grenze
//! verankert sein, sonst registriert `security.` versehentlich auch
//! `securityx.foo`. `harw-observe` zieht dafür **keine** Abhängigkeit auf
//! `harw-sandbox`: diese Crate ist die Wurzel von rund vierzig
//! Konsumenten, und eine Kante für rund zehn Zeilen Textvergleich
//! ([`namespace_matches`]) wäre teurer als die Wiederholung. Das ist der
//! seltene Fall, in dem eine zweite, unabhängige Implementierung derselben
//! Regel die richtige Wahl ist — dieser Absatz hält fest, dass es Absicht
//! ist, kein Versehen.
//!
//! # Der Nullzähler `security_metric_leaked`
//! [`SECURITY_METRIC_LEAKED`] zählt, wie oft eine geschützte Metrik einen
//! anderen Sink als den `default_sink` erreicht **hätte** — nämlich immer
//! dann, wenn [`RoutingSink::record`] auf einen (wirkungslosen)
//! [`RoutingSink::route`]-Eintrag für einen geschützten Namensraum trifft.
//! Erwarteter Stand im Betrieb: null. Steigt er, hat jemand versucht,
//! `security.*`/`warden.*` über den gewöhnlichen `route()`-Pfad statt über
//! [`RoutingSink::approve_export`] zu leiten — der Versuch bleibt wirkungs-
//! los (die Metrik erreicht weiterhin nur `default_sink`, nie den
//! fälschlich gebundenen Sink), aber er wird sichtbar statt stillschweigend
//! ignoriert zu werden.
//!
//! # Nebenläufigkeit
//! [`RoutingSink`] ist `Send + Sync + Debug`: `record`/`flush` lesen nur
//! aus den beim Aufbau gefüllten `Vec`s, ohne Sperre — siehe „Warum die
//! Routing-Tabelle unveränderlich ist" oben. [`ExportApproval`] ist
//! `Send + Sync` (nur `&'static str`- und `String`-Felder) und trägt keine
//! innere Veränderlichkeit.
//!
//! # Fehler
//! Keine — `route()`, `approve_export()`, `record()` und `flush()` geben
//! `()` zurück (Vertrag A.3). Eine Fehlkonfiguration (geschützter Präfix
//! über `route()` statt `approve_export()`) wird nicht propagiert, sondern
//! über [`SECURITY_METRIC_LEAKED`] gezählt.
//!
//! # Examples
//! ```
//! use std::sync::Arc;
//! use harw_observe::{
//!     Cardinality, ExportApproval, MetricKey, MetricKind, MetricValue, NullSink, RoutingSink,
//!     TelemetrySink, Unit,
//! };
//!
//! const FINDING: MetricKey = MetricKey {
//!     name: "security.finding_total",
//!     kind: MetricKind::Counter,
//!     unit: Unit::Count,
//!     labels: &[],
//!     cardinality: Cardinality::Single,
//! };
//!
//! // `default_sink` sollte im Betrieb der File-Sink sein.
//! let mut routing = RoutingSink::new(Arc::new(NullSink));
//! routing.route("app.", Arc::new(NullSink));
//!
//! // Ohne Bestätigung geht die geschützte Metrik nur an `default_sink`.
//! routing.record(&FINDING, MetricValue::Count(1), &[]);
//!
//! // Erst mit einem echten Beleg entsteht ein zusätzliches Exportziel.
//! routing.approve_export(Arc::new(NullSink), ExportApproval::granted("security.", "alice"));
//! ```

use std::sync::Arc;

use crate::field::{FieldName, FieldValue};
use crate::metric::{Cardinality, MetricKey, MetricKind, MetricValue, Unit};
use crate::null_counter::NullCounter;
use crate::sink::TelemetrySink;

/// Namensräume, die ohne Bestätigung nur in den `default_sink` (im Betrieb
/// der File-Sink) gehen.
///
/// # Description
/// Ein neuer geschützter Namensraum ist ein Listeneintrag hier, keine über
/// den Code verstreute Fallunterscheidung. Jeder Eintrag trägt bewusst
/// einen abschließenden Punkt (`"security."`, nicht `"security"`): der
/// Punkt ist Teil des Präfix-Vergleichs selbst (siehe
/// [`namespace_matches`]) und macht `"securityx.foo"` schon durch einen
/// einfachen `starts_with`-Vergleich unangreifbar, ganz ohne
/// Sonderbehandlung. Siehe Moduldoc, Abschnitt „Warum `security.*` und
/// `warden.*` geschützt sind" für die Begründung.
///
/// # Examples
/// ```
/// use harw_observe::PROTECTED_NAMESPACES;
///
/// assert!(PROTECTED_NAMESPACES.contains(&"security."));
/// assert!(PROTECTED_NAMESPACES.contains(&"warden."));
/// ```
pub const PROTECTED_NAMESPACES: &[&str] = &["security.", "warden."];

const SECURITY_METRIC_LEAKED_KEY: MetricKey = MetricKey {
    name: "security_metric_leaked_total",
    kind: MetricKind::Counter,
    unit: Unit::Count,
    labels: &[],
    cardinality: Cardinality::Single,
};

/// Nullzähler: wie oft eine geschützte Metrik (`security.*`/`warden.*`)
/// über den gewöhnlichen `route()`-Pfad einen anderen Sink als
/// `default_sink` erreicht **hätte**.
///
/// # Description
/// Erwarteter Stand im Betrieb: null. Erhöht sich ausschließlich in
/// [`RoutingSink::record`], wenn ein [`RoutingSink::route`]-Eintrag für
/// einen geschützten Namensraum existiert — ein Zustand, der nur durch eine
/// Fehlkonfiguration entsteht (siehe Moduldoc, Abschnitt „Der Nullzähler
/// `security_metric_leaked`"). Muss von der Kompositionsstelle, die den
/// Prozess zusammensetzt, in ihre [`crate::NullCounterRegistry`]
/// aufgenommen werden, damit er in der Nullzähler-Leiste (Knoten UI-04)
/// erscheint — dieses Modul registriert ihn nicht selbst, dieselbe
/// Zurückhaltung wie bei jedem anderen Nullzähler dieser Crate.
///
/// # Examples
/// ```
/// use harw_observe::SECURITY_METRIC_LEAKED;
///
/// assert_eq!(SECURITY_METRIC_LEAKED.invariant(), "security.* und warden.* erreichen ohne \
/// Operator-Bestätigung ausschließlich den default_sink (im Betrieb den File-Sink)");
/// ```
pub static SECURITY_METRIC_LEAKED: NullCounter = NullCounter::new(
    &SECURITY_METRIC_LEAKED_KEY,
    "security.* und warden.* erreichen ohne Operator-Bestätigung ausschließlich den \
     default_sink (im Betrieb den File-Sink)",
);

/// Erlaubt den Export eines geschützten Namensraums.
///
/// Nur über einen Operator-Beleg konstruierbar — nicht durch eine
/// Konfigurationszeile und nicht durch ein Modell.
///
/// # Description
/// `harw-observe` kann einen Operator-Beleg nicht selbst prüfen (siehe
/// Moduldoc, Abschnitt „Wie eine Bestätigung aussieht": die Typen dafür —
/// `ApprovalActor`/`ApprovalStore` in `harw-session-store`, `PermissionTier`
/// in `harw-operations` — liegen in Crates, die (transitiv) von
/// `harw-observe` abhängen, eine Kante in die Gegenrichtung wäre ein
/// Zyklus). Dieser Typ prüft deshalb nichts; er verlangt nur, dass der
/// Aufrufer den freigegebenen Namensraum und den freigebenden Operator
/// explizit benennt, statt dass ein Exportziel aus einer beliebigen
/// Konfigurationszeile oder einem Modelltext entstehen könnte. Die
/// eigentliche Prüfung — dass wirklich ein Operator mit ausreichender
/// `PermissionTier` über den bestehenden Freigabeweg (`ApprovalStore`)
/// zugestimmt hat — muss **vor** dem Aufruf von [`ExportApproval::granted`]
/// an einer Stelle passieren, die beide Seiten sieht (typischerweise
/// `harw-cli`, beim Aufbau der Telemetrie-Sinks).
#[derive(Debug, Clone)]
pub struct ExportApproval {
    namespace: &'static str,
    actor: String,
}

impl ExportApproval {
    /// Baut einen Export-Beleg für einen geschützten Namensraum.
    ///
    /// # Description
    /// Der einzige Konstruktionsweg für [`ExportApproval`]. Muss erst
    /// aufgerufen werden, **nachdem** der Aufrufer die eigentliche
    /// Freigabe über den bestehenden Weg geprüft hat (siehe Typ-Doku) —
    /// diese Funktion selbst führt keine Prüfung durch, sie bündelt nur,
    /// was bereits feststeht.
    ///
    /// # Arguments
    /// - `namespace` (`&'static str`): der freigegebene geschützte
    ///   Namensraum, üblicherweise ein Eintrag aus
    ///   [`PROTECTED_NAMESPACES`] (z. B. `"security."`).
    /// - `actor` (`impl Into<String>`): Bezeichner des Operators, der die
    ///   Freigabe erteilt hat, z. B. die `id` eines
    ///   `ApprovalActor::Operator` am Aufrufort.
    ///
    /// # Returns
    /// Den fertigen Beleg, bereit für [`RoutingSink::approve_export`].
    ///
    /// # Concurrency
    /// Reine Werterzeugung ohne Nebenläufigkeitsaspekt.
    ///
    /// # Examples
    /// ```
    /// use harw_observe::ExportApproval;
    ///
    /// let approval = ExportApproval::granted("security.", "alice");
    /// assert_eq!(approval.namespace(), "security.");
    /// assert_eq!(approval.actor(), "alice");
    /// ```
    #[must_use]
    pub fn granted(namespace: &'static str, actor: impl Into<String>) -> Self {
        Self {
            namespace,
            actor: actor.into(),
        }
    }

    /// Der freigegebene Namensraum.
    ///
    /// # Returns
    /// Den bei [`ExportApproval::granted`] übergebenen `namespace`-Wert.
    ///
    /// # Concurrency
    /// Liest ein unveränderliches `&'static str`-Feld.
    ///
    /// # Examples
    /// ```
    /// use harw_observe::ExportApproval;
    ///
    /// let approval = ExportApproval::granted("warden.", "bob");
    /// assert_eq!(approval.namespace(), "warden.");
    /// ```
    #[must_use]
    pub fn namespace(&self) -> &'static str {
        self.namespace
    }

    /// Der freigebende Operator.
    ///
    /// # Returns
    /// Den bei [`ExportApproval::granted`] übergebenen `actor`-Wert.
    ///
    /// # Concurrency
    /// Liest ein unveränderliches `String`-Feld.
    ///
    /// # Examples
    /// ```
    /// use harw_observe::ExportApproval;
    ///
    /// let approval = ExportApproval::granted("warden.", "bob");
    /// assert_eq!(approval.actor(), "bob");
    /// ```
    #[must_use]
    pub fn actor(&self) -> &str {
        &self.actor
    }
}

/// Ein genehmigtes zusätzliches Ziel für einen geschützten Namensraum, mit
/// dem freigebenden Operator für die Nachvollziehbarkeit im `Debug`-Abdruck.
struct ProtectedExport {
    prefix: &'static str,
    sink: Arc<dyn TelemetrySink>,
    approved_by: String,
}

impl std::fmt::Debug for ProtectedExport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProtectedExport")
            .field("prefix", &self.prefix)
            .field("sink", &self.sink.name())
            .field("approved_by", &self.approved_by)
            .finish()
    }
}

/// Ein [`TelemetrySink`], der Messwerte nach ihrem Namensraum auf
/// verschiedene Ziel-Sinks verteilt.
///
/// Siehe Moduldoc für Zweck, Schutzregel und Unveränderlichkeit der
/// Routing-Tabelle.
#[derive(Debug)]
pub struct RoutingSink {
    default_sink: Arc<dyn TelemetrySink>,
    routes: Vec<(&'static str, Arc<dyn TelemetrySink>)>,
    protected_exports: Vec<ProtectedExport>,
}

impl RoutingSink {
    /// Baut einen `RoutingSink` mit leerer Routing-Tabelle.
    ///
    /// # Description
    /// `default_sink` ist das Ziel für jede Metrik, die keinem
    /// [`RoutingSink::route`]-Eintrag entspricht — **und** die
    /// Voreinstellung für jede geschützte Metrik
    /// ([`PROTECTED_NAMESPACES`]), solange keine passende
    /// [`RoutingSink::approve_export`]-Freigabe vorliegt. Damit die Zusage
    /// aus dem Moduldoc („`security.*`/`warden.*` gehen ohne Bestätigung
    /// ausschließlich in den File-Sink") in der Praxis gilt, **muss** der
    /// Aufrufer hier den File-Sink übergeben — der Typ selbst kann das
    /// nicht erzwingen (`TelemetrySink` kennt kein „ist ein File-Sink"-
    /// Merkmal), das ist eine Betriebsvoraussetzung, keine
    /// Compile-Zeit-Garantie.
    ///
    /// # Arguments
    /// - `default_sink` (`Arc<dyn TelemetrySink>`): Rückfallziel für
    ///   ungebundene und für geschützte, nicht freigegebene Metriken.
    ///
    /// # Returns
    /// Einen `RoutingSink` ohne registrierte Routen oder Freigaben.
    ///
    /// # Concurrency
    /// Reine Werterzeugung; der zurückgegebene Wert ist noch nicht geteilt
    /// und darf mit `&mut self`-Methoden ([`RoutingSink::route`],
    /// [`RoutingSink::approve_export`]) weiter aufgebaut werden, bevor er
    /// in ein `Arc<dyn TelemetrySink>` verpackt wird (siehe Moduldoc,
    /// Abschnitt „Warum die Routing-Tabelle unveränderlich ist").
    ///
    /// # Examples
    /// ```
    /// use std::sync::Arc;
    /// use harw_observe::{NullSink, RoutingSink};
    ///
    /// let routing = RoutingSink::new(Arc::new(NullSink));
    /// assert_eq!(harw_observe::TelemetrySink::name(&routing), "routing");
    /// ```
    #[must_use]
    pub fn new(default_sink: Arc<dyn TelemetrySink>) -> Self {
        Self {
            default_sink,
            routes: Vec::new(),
            protected_exports: Vec::new(),
        }
    }

    /// Bindet einen Namensraum an ein Ziel.
    ///
    /// # Description
    /// Gilt für gewöhnliche, ungeschützte Namensräume. Für einen Präfix aus
    /// [`PROTECTED_NAMESPACES`] hat dieser Aufruf **keine** Wirkung auf die
    /// Zustellung: [`RoutingSink::record`] prüft geschützte Namensräume vor
    /// dieser Tabelle und liest sie nie zur Zustellung aus (siehe Moduldoc).
    /// Ein hier trotzdem vorhandener Eintrag für einen geschützten Präfix
    /// macht einen künftigen `record()`-Aufruf für diesen Namensraum zu
    /// einem gezählten Verstoß ([`SECURITY_METRIC_LEAKED`]), liefert die
    /// Metrik aber nie an ihn aus.
    ///
    /// Trifft mehr als ein Eintrag auf denselben Metriknamen zu (z. B.
    /// `"app."` und `"app.jobs."`), gewinnt der längere (spezifischere)
    /// Präfix; bei gleicher Länge der zuletzt registrierte.
    ///
    /// # Arguments
    /// - `prefix` (`&'static str`): der Namensraum-Präfix, üblicherweise
    ///   mit abschließendem Punkt (`"app."`), siehe [`namespace_matches`]
    ///   für die genaue Vergleichsregel.
    /// - `sink` (`Arc<dyn TelemetrySink>`): das Ziel für Metriken unter
    ///   diesem Präfix.
    ///
    /// # Concurrency
    /// Verlangt `&mut self` — nur aufrufbar, solange dieser `RoutingSink`
    /// noch nicht über `Arc<dyn TelemetrySink>` geteilt ist (siehe
    /// Moduldoc, Abschnitt „Warum die Routing-Tabelle unveränderlich ist").
    ///
    /// # Examples
    /// ```
    /// use std::sync::Arc;
    /// use harw_observe::{NullSink, RoutingSink};
    ///
    /// let mut routing = RoutingSink::new(Arc::new(NullSink));
    /// routing.route("app.", Arc::new(NullSink));
    /// ```
    pub fn route(&mut self, prefix: &'static str, sink: Arc<dyn TelemetrySink>) {
        self.routes.push((prefix, sink));
    }

    /// Setzt ein genehmigtes zusätzliches Ziel für einen geschützten
    /// Namensraum.
    ///
    /// # Description
    /// Der einzige Weg, mit dem eine Metrik aus [`PROTECTED_NAMESPACES`]
    /// **zusätzlich** zu `default_sink` ein weiteres Ziel erreicht — die
    /// durable Kopie in `default_sink` bleibt in jedem Fall bestehen (siehe
    /// Moduldoc). Der freigegebene Namensraum kommt aus `approval.namespace()`,
    /// nicht aus einem separaten Parameter: so kann kein Aufrufer versehentlich
    /// einen Beleg für `"security."` an einen `sink` für `"warden."` binden.
    ///
    /// # Arguments
    /// - `sink` (`Arc<dyn TelemetrySink>`): das zusätzliche Exportziel.
    /// - `approval` (`ExportApproval`): der Beleg; bestimmt über
    ///   [`ExportApproval::namespace`], für welchen Präfix dieser `sink`
    ///   gilt, und wird für die Nachvollziehbarkeit im `Debug`-Abdruck
    ///   dieses `RoutingSink` gehalten.
    ///
    /// # Concurrency
    /// Verlangt `&mut self` — dieselbe Einschränkung wie
    /// [`RoutingSink::route`].
    ///
    /// # Examples
    /// ```
    /// use std::sync::Arc;
    /// use harw_observe::{ExportApproval, NullSink, RoutingSink};
    ///
    /// let mut routing = RoutingSink::new(Arc::new(NullSink));
    /// routing.approve_export(Arc::new(NullSink), ExportApproval::granted("security.", "alice"));
    /// ```
    pub fn approve_export(&mut self, sink: Arc<dyn TelemetrySink>, approval: ExportApproval) {
        self.protected_exports.push(ProtectedExport {
            prefix: approval.namespace,
            sink,
            approved_by: approval.actor,
        });
    }
}

// Flusht `sink` genau dann, wenn keiner der bereits geflushten `Arc`s
// dieselbe Zuteilung ist (Zeigergleichheit über `Arc::ptr_eq`, kein
// `unsafe` nötig) — derselbe `Arc` kann als `default_sink` und zusätzlich
// unter einem oder mehreren Präfixen registriert sein.
fn flush_once<'a>(sink: &'a Arc<dyn TelemetrySink>, already: &mut Vec<&'a Arc<dyn TelemetrySink>>) {
    if already.iter().any(|flushed| Arc::ptr_eq(flushed, sink)) {
        return;
    }
    sink.flush();
    already.push(sink);
}

impl TelemetrySink for RoutingSink {
    fn record(&self, key: &MetricKey, value: MetricValue, labels: &[(FieldName, FieldValue)]) {
        if protected_namespace_for(key.name).is_some() {
            // Immer durable: eine geschützte Metrik erreicht `default_sink`
            // (im Betrieb der File-Sink) unabhängig von jeder Freigabe.
            self.default_sink.record(key, value, labels);

            // Eine passende Freigabe liefert zusätzlich an ihr Exportziel.
            if let Some(export) = best_protected_export(&self.protected_exports, key.name) {
                export.sink.record(key, value, labels);
            }

            // Tiefenverteidigung: ein gewöhnlicher `route()`-Eintrag darf
            // einen geschützten Namensraum nie tatsächlich erreichen (siehe
            // Moduldoc). Existiert trotzdem einer, ist das eine
            // Fehlkonfiguration, keine normale Zustellung — sichtbar über
            // den Nullzähler, die Metrik selbst bleibt unversehrt bei
            // `default_sink` (oben) und wird an den fälschlich gebundenen
            // Sink nicht ausgeliefert.
            if best_route(&self.routes, key.name).is_some() {
                SECURITY_METRIC_LEAKED.violated(self.default_sink.as_ref(), &[]);
            }
            return;
        }

        match best_route(&self.routes, key.name) {
            Some(sink) => sink.record(key, value, labels),
            None => self.default_sink.record(key, value, labels),
        }
    }

    fn flush(&self) {
        let mut flushed: Vec<&Arc<dyn TelemetrySink>> = Vec::new();
        flush_once(&self.default_sink, &mut flushed);
        for (_, sink) in &self.routes {
            flush_once(sink, &mut flushed);
        }
        for export in &self.protected_exports {
            flush_once(&export.sink, &mut flushed);
        }
    }

    fn name(&self) -> &'static str {
        "routing"
    }
}

// Der Namensraum aus `PROTECTED_NAMESPACES`, unter den `name` fällt, falls
// einer passt. Nutzt `namespace_matches`, siehe dort für die Vergleichsregel.
fn protected_namespace_for(name: &str) -> Option<&'static str> {
    PROTECTED_NAMESPACES
        .iter()
        .copied()
        .find(|prefix| namespace_matches(prefix, name))
}

// Der spezifischste (längste) passende Eintrag aus `routes` für `name`, falls
// einer passt. Siehe `RoutingSink::route` für die Präzedenzregel.
fn best_route<'a>(
    routes: &'a [(&'static str, Arc<dyn TelemetrySink>)],
    name: &str,
) -> Option<&'a Arc<dyn TelemetrySink>> {
    routes
        .iter()
        .filter(|(prefix, _)| namespace_matches(prefix, name))
        .max_by_key(|(prefix, _)| prefix.len())
        .map(|(_, sink)| sink)
}

// Der spezifischste (längste) passende Eintrag aus `protected_exports` für
// `name`, falls einer passt.
fn best_protected_export<'a>(
    exports: &'a [ProtectedExport],
    name: &str,
) -> Option<&'a ProtectedExport> {
    exports
        .iter()
        .filter(|export| namespace_matches(export.prefix, name))
        .max_by_key(|export| export.prefix.len())
}

/// Prüft, ob `name` unter dem Namensraum `prefix` liegt — ein
/// Präfixvergleich an einer Punktgrenze.
///
/// # Description
/// Erlaubt ist ein exakter Treffer oder ein Präfix-Treffer, wobei die Grenze
/// direkt nach `prefix` entweder schon ein Punkt in `prefix` selbst ist
/// (`prefix` endet mit `'.'`, z. B. `"security."`) oder das nächste Byte in
/// `name` ein Punkt ist. `"security."` passt daher auf
/// `"security.finding_total"`, aber weder auf `"securityx.foo"` (das neunte
/// Byte ist `'x'`, kein `'.'` — der Vergleich scheitert schon an
/// `starts_with`) noch auf `"security_total"` (kein Punkt an der Grenze).
/// Spiegelbildlich zur Suffix-Grenzprüfung in
/// `harw-authority`s `EgressTarget::matches_host`/`host_matches` — siehe
/// Moduldoc, Abschnitt „Warum die Punktgrenzen-Prüfung hier ein zweites Mal
/// steht", für die Begründung, warum diese Crate keine Abhängigkeit auf
/// `harw-sandbox` zieht, sondern dieselbe Regel hier eigenständig umsetzt.
///
/// # Arguments
/// - `prefix` (`&str`): der zu prüfende Namensraum-Präfix.
/// - `name` (`&str`): der volle Metrikname.
///
/// # Returns
/// `true`, wenn `name` zum Namensraum `prefix` gehört. Ein leerer `prefix`
/// liefert immer `false` — ein leerer Präfix wäre sonst Präfix jedes
/// Namens.
///
/// # Examples
/// ```ignore
/// assert!(namespace_matches("security.", "security.finding_total"));
/// assert!(!namespace_matches("security.", "securityx.foo"));
/// assert!(!namespace_matches("security.", "security_total"));
/// ```
fn namespace_matches(prefix: &str, name: &str) -> bool {
    if prefix.is_empty() {
        return false;
    }
    if name == prefix {
        return true;
    }
    if name.len() <= prefix.len() || !name.starts_with(prefix) {
        return false;
    }
    // `name` ist echt länger als `prefix` und beginnt damit: die Grenze
    // ist entweder schon Teil von `prefix` (`prefix` endet mit `'.'`) oder
    // das Byte direkt danach in `name` muss `'.'` sein. Ein Byte einer
    // Mehrbyte-UTF8-Sequenz ist nie `b'.'`, der Grenzvergleich bleibt also
    // auch bei nicht-ASCII-Inhalt in `name` korrekt.
    prefix.ends_with('.') || name.as_bytes()[prefix.len()] == b'.'
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;

    use super::*;
    use crate::metric::{Cardinality, MetricKind, Unit};
    use crate::test_support::{TestError, TestResult};

    fn key(name: &'static str) -> MetricKey {
        MetricKey {
            name,
            kind: MetricKind::Counter,
            unit: Unit::Count,
            labels: &[],
            cardinality: Cardinality::Single,
        }
    }

    /// Aufzeichnender Test-Sink: hält jeden `record()`-Aufruf und zählt
    /// `flush()`-Aufrufe, statt sie zu verwerfen wie [`crate::sink::NullSink`].
    #[derive(Debug, Default)]
    struct RecordingSink {
        records: Mutex<Vec<(MetricKey, MetricValue)>>,
        flushes: AtomicU64,
    }

    impl RecordingSink {
        fn count(&self) -> usize {
            self.records.lock().unwrap_or_else(|p| p.into_inner()).len()
        }

        fn flush_count(&self) -> u64 {
            self.flushes.load(Ordering::Relaxed)
        }
    }

    impl TelemetrySink for RecordingSink {
        fn record(&self, key: &MetricKey, value: MetricValue, _labels: &[(FieldName, FieldValue)]) {
            self.records
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push((*key, value));
        }

        fn flush(&self) {
            self.flushes.fetch_add(1, Ordering::Relaxed);
        }

        fn name(&self) -> &'static str {
            "recording"
        }
    }

    fn sink() -> Arc<RecordingSink> {
        Arc::new(RecordingSink::default())
    }

    fn as_dyn(sink: &Arc<RecordingSink>) -> Arc<dyn TelemetrySink> {
        Arc::clone(sink) as Arc<dyn TelemetrySink>
    }

    // ── namespace_matches (Punktgrenze) ─────────────────────────────────

    #[test]
    fn test_namespace_matches_exact_prefix_with_trailing_dot() {
        assert!(namespace_matches("security.", "security.finding_total"));
    }

    #[test]
    fn test_namespace_matches_rejects_lookalike_without_dot_boundary() {
        assert!(!namespace_matches("security.", "securityx.foo"));
    }

    #[test]
    fn test_namespace_matches_rejects_prefix_without_any_dot() {
        assert!(!namespace_matches("security.", "security_total"));
    }

    #[test]
    fn test_namespace_matches_rejects_empty_prefix() {
        assert!(!namespace_matches("", "anything"));
    }

    #[test]
    fn test_namespace_matches_accepts_exact_equality() {
        assert!(namespace_matches("security_total", "security_total"));
    }

    // ── PROTECTED_NAMESPACES / ExportApproval ───────────────────────────

    #[test]
    fn test_protected_namespaces_contains_security_and_warden() {
        assert_eq!(PROTECTED_NAMESPACES.len(), 2);
        assert!(PROTECTED_NAMESPACES.contains(&"security."));
        assert!(PROTECTED_NAMESPACES.contains(&"warden."));
    }

    #[test]
    fn test_export_approval_accessors_roundtrip() {
        let approval = ExportApproval::granted("security.", "alice");
        assert_eq!(approval.namespace(), "security.");
        assert_eq!(approval.actor(), "alice");
    }

    // ── RoutingSink: gewöhnliches Routing ────────────────────────────────

    #[test]
    fn test_ordinary_metric_reaches_default_sink() {
        let default = sink();
        let routing = RoutingSink::new(as_dyn(&default));

        routing.record(&key("app.jobs_total"), MetricValue::Count(1), &[]);

        assert_eq!(default.count(), 1);
    }

    #[test]
    fn test_metric_with_bound_prefix_reaches_its_target() {
        let default = sink();
        let target = sink();
        let mut routing = RoutingSink::new(as_dyn(&default));
        routing.route("app.", as_dyn(&target));

        routing.record(&key("app.jobs_total"), MetricValue::Count(1), &[]);

        assert_eq!(target.count(), 1);
        assert_eq!(default.count(), 0);
    }

    #[test]
    fn test_more_specific_route_wins_over_shorter_prefix() {
        let default = sink();
        let broad = sink();
        let narrow = sink();
        let mut routing = RoutingSink::new(as_dyn(&default));
        routing.route("app.", as_dyn(&broad));
        routing.route("app.jobs.", as_dyn(&narrow));

        routing.record(&key("app.jobs.completed_total"), MetricValue::Count(1), &[]);

        assert_eq!(narrow.count(), 1);
        assert_eq!(broad.count(), 0);
    }

    // ── Geschützte Namensräume ───────────────────────────────────────────

    #[test]
    fn test_security_metric_reaches_only_default_sink_without_approval() {
        let file = sink();
        let other = sink();
        let mut routing = RoutingSink::new(as_dyn(&file));
        routing.route("app.", as_dyn(&other));

        routing.record(&key("security.finding_total"), MetricValue::Count(1), &[]);

        assert_eq!(file.count(), 1);
        assert_eq!(other.count(), 0);
    }

    #[test]
    fn test_warden_metric_reaches_only_default_sink_without_approval() {
        let file = sink();
        let other = sink();
        let mut routing = RoutingSink::new(as_dyn(&file));
        routing.route("app.", as_dyn(&other));

        routing.record(&key("warden.escalation_total"), MetricValue::Count(1), &[]);

        assert_eq!(file.count(), 1);
        assert_eq!(other.count(), 0);
    }

    #[test]
    fn test_securityx_prefix_is_not_protected_dot_boundary() {
        let file = sink();
        let other = sink();
        let mut routing = RoutingSink::new(as_dyn(&file));
        routing.route("securityx.", as_dyn(&other));

        routing.record(&key("securityx.foo"), MetricValue::Count(1), &[]);

        assert_eq!(
            other.count(),
            1,
            "securityx. is a distinct, unprotected namespace"
        );
        assert_eq!(file.count(), 0);
    }

    #[test]
    fn test_security_without_dot_is_not_protected() {
        let file = sink();
        let other = sink();
        let mut routing = RoutingSink::new(as_dyn(&file));
        routing.route("security_finding_total", as_dyn(&other));

        routing.record(&key("security_finding_total"), MetricValue::Count(1), &[]);

        assert_eq!(
            other.count(),
            1,
            "a metric literally named security_finding_total has no namespace dot"
        );
        assert_eq!(file.count(), 0);
    }

    #[test]
    fn test_export_approval_lets_protected_metric_reach_export_target() {
        let file = sink();
        let export = sink();
        let mut routing = RoutingSink::new(as_dyn(&file));
        routing.approve_export(
            as_dyn(&export),
            ExportApproval::granted("security.", "alice"),
        );

        routing.record(&key("security.finding_total"), MetricValue::Count(1), &[]);

        assert_eq!(export.count(), 1);
        assert_eq!(
            file.count(),
            1,
            "approval adds an export target, it does not remove the durable default_sink copy"
        );
    }

    #[test]
    fn test_export_approval_only_unlocks_its_own_namespace() {
        let file = sink();
        let export = sink();
        let mut routing = RoutingSink::new(as_dyn(&file));
        routing.approve_export(
            as_dyn(&export),
            ExportApproval::granted("security.", "alice"),
        );

        routing.record(&key("warden.escalation_total"), MetricValue::Count(1), &[]);

        assert_eq!(
            export.count(),
            0,
            "a security. approval must not unlock warden."
        );
        assert_eq!(file.count(), 1);
    }

    // ── Nullzähler ───────────────────────────────────────────────────────

    /// Serialisiert die Tests, die den prozessweiten Nullzähler
    /// [`SECURITY_METRIC_LEAKED`] lesen oder erhöhen — dieselbe Begründung,
    /// aus der `harw-core/src/context_budget.rs` seinen
    /// `TRUST_BLOCK_VIOLATION`-Test, `harw-secrets/src/audit/telemetry.rs`
    /// seinen `AUDIT_CHAIN_BREAK`-Test und
    /// `harw-knowledge/src/context_steward.rs` seine drei `STEWARD_*`-Tests
    /// hinter einem eigenen Lock serialisieren: ein `static NullCounter` ist
    /// prozessweit geteilt, und `cargo test` läuft standardmäßig mit
    /// mehreren Threads im selben Prozess.
    ///
    /// Eine reine Vorher/Nachher-Differenzmessung (`let before = …count();`)
    /// **genügt dafür nicht** — zwischen der Messung von `before` und der
    /// Zusicherung am Ende kann ein parallel laufender Test denselben
    /// Zähler erhöhen, und die Differenz stimmt dann nicht mehr. Ein
    /// Kommentar, der behauptet, kein anderer Test fasse diesen Zähler an,
    /// ist keine dauerhafte Garantie: genau das ist bei den drei
    /// `STEWARD_*`-Zählern in `harw-knowledge` eingetreten — sie waren
    /// lange grün, bis neue Tests hinzukamen, die denselben Zähler lasen.
    /// Heute berührt in dieser Crate nur der untenstehende Test
    /// [`SECURITY_METRIC_LEAKED`] — aber exakt diese Annahme war bei
    /// `STEWARD_*` zuvor auch wahr. Die Sperre steht **zusätzlich** zur
    /// Differenzmessung, nicht an ihrer Stelle.
    static SECURITY_COUNTER_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn test_null_counter_increments_on_violation_and_stays_zero_otherwise() {
        let _guard = SECURITY_COUNTER_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let before = SECURITY_METRIC_LEAKED.count();

        let file = sink();
        let mut routing = RoutingSink::new(as_dyn(&file));

        // Normalbetrieb: keine geschützte Route vorhanden, kein Verstoß.
        routing.record(&key("security.finding_total"), MetricValue::Count(1), &[]);
        assert_eq!(
            SECURITY_METRIC_LEAKED.count(),
            before,
            "no leaky route registered yet, the counter must not move"
        );

        // Fehlkonfiguration: ein geschützter Präfix über den gewöhnlichen
        // `route()`-Pfad statt über `approve_export()`.
        let leaky = sink();
        routing.route("security.", as_dyn(&leaky));
        routing.record(&key("security.finding_total"), MetricValue::Count(2), &[]);

        assert_eq!(
            leaky.count(),
            0,
            "a plain route() must never actually deliver a protected metric"
        );
        // 2 real "security.finding_total" records (one per call) plus the
        // null counter's own self-report (`NullCounter::violated` records
        // its new count through the sink it is handed, here `default_sink`
        // itself) — the violation is itself durably logged, not just held
        // in the in-memory atomic.
        assert_eq!(
            file.count(),
            3,
            "default_sink receives both real records plus the null counter's own report"
        );
        assert_eq!(
            SECURITY_METRIC_LEAKED.count(),
            before + 1,
            "the violation must be counted exactly once"
        );
    }

    // ── Nebenläufigkeit ──────────────────────────────────────────────────

    #[test]
    fn test_routing_sink_is_send_sync_debug() {
        fn assert_bounds<T: Send + Sync + std::fmt::Debug>() {}
        assert_bounds::<RoutingSink>();
    }

    #[test]
    fn test_routing_sink_usable_through_arc_dyn_telemetry_sink() {
        let routing: Arc<dyn TelemetrySink> = Arc::new(RoutingSink::new(as_dyn(&sink())));
        routing.record(&key("app.jobs_total"), MetricValue::Count(1), &[]);
        routing.flush();
        assert_eq!(routing.name(), "routing");
    }

    #[test]
    fn test_concurrent_threads_all_records_arrive_at_correct_sink() -> TestResult {
        let default = sink();
        let app_sink = sink();
        let export_sink = sink();

        let mut routing = RoutingSink::new(as_dyn(&default));
        routing.route("app.", as_dyn(&app_sink));
        routing.approve_export(
            as_dyn(&export_sink),
            ExportApproval::granted("security.", "alice"),
        );

        let routing: Arc<dyn TelemetrySink> = Arc::new(routing);

        const ITERATIONS: usize = 200;
        let handles: Vec<_> = (0..4)
            .map(|thread_id| {
                let routing = Arc::clone(&routing);
                thread::spawn(move || {
                    for _ in 0..ITERATIONS {
                        if thread_id % 2 == 0 {
                            routing.record(&key("app.jobs_total"), MetricValue::Count(1), &[]);
                        } else {
                            routing.record(
                                &key("security.finding_total"),
                                MetricValue::Count(1),
                                &[],
                            );
                        }
                    }
                })
            })
            .collect();

        for handle in handles {
            handle
                .join()
                .map_err(|_| TestError::Unexpected("thread panicked".to_owned()))?;
        }

        // Zwei Threads schreiben "app." (nur `app_sink`), zwei schreiben
        // "security." (immer `default`, zusätzlich `export_sink`); keiner
        // landet im jeweils falschen Sink.
        assert_eq!(app_sink.count(), 2 * ITERATIONS);
        assert_eq!(default.count(), 2 * ITERATIONS);
        assert_eq!(export_sink.count(), 2 * ITERATIONS);
        Ok(())
    }

    // ── flush() ──────────────────────────────────────────────────────────

    #[test]
    fn test_flush_forwards_to_default_and_every_route() {
        let default = sink();
        let a = sink();
        let b = sink();
        let mut routing = RoutingSink::new(as_dyn(&default));
        routing.route("app.", as_dyn(&a));
        routing.approve_export(as_dyn(&b), ExportApproval::granted("security.", "alice"));

        routing.flush();

        assert_eq!(default.flush_count(), 1);
        assert_eq!(a.flush_count(), 1);
        assert_eq!(b.flush_count(), 1);
    }

    #[test]
    fn test_flush_forwards_to_each_distinct_sink_exactly_once() {
        let shared = sink();
        let mut routing = RoutingSink::new(as_dyn(&shared));
        routing.route("app.", as_dyn(&shared));

        routing.flush();

        assert_eq!(
            shared.flush_count(),
            1,
            "the same underlying sink registered under multiple roles must flush once"
        );
    }
}
