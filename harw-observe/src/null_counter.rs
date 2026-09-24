//! Nullzähler: Zähler, deren erwarteter Wert im Betrieb null ist.
//!
//! # Verantwortungsbereich
//! Trägt [`NullCounter`], [`NullCounterRegistry`] und [`assert_all_zero`]
//! (Knoten AW1-07, Vertrag `docs/design/build-history.md`). Baut die
//! **Mechanik** eines Nullzählers — Registrierung, Erhöhung, Prüfung. Die
//! konkreten Zähler selbst (`trust_block_violation`,
//! `steward_ceiling_violation`, `lens_remote_embed_on_operator_only`)
//! entstehen bei den Knoten, die ihre jeweilige Invariante einführen
//! (AW4-01, AW6-08, AW7-05); dieses Modul kennt sie nicht.
//!
//! # Die Idee des Nullzählers
//! Ein Nullzähler zählt nicht Arbeit, sondern **Verstöße gegen eine
//! Invariante**. Sein erwarteter Wert im Betrieb ist null. Der Sinn: eine
//! Invariante, die nur im Code steht (ein `assert!`, ein Kommentar, eine
//! Designentscheidung), ist eine Behauptung. Ein Nullzähler macht sie
//! **beobachtbar** — wenn er jemals über null geht, ist die Invariante
//! verletzt, und man sieht es im Betrieb (Metrik, Dashboard, ein
//! fehlschlagender [`assert_all_zero`]-Check) statt erst in einem
//! Post-Mortem.
//!
//! **Die Falle, die diese Mechanik vermeiden muss:** ein Nullzähler, der
//! nie erhöht wird, weil die Stelle, die ihn erhöhen müsste, nicht
//! existiert oder nicht erreicht wird, ist von einem eingehaltenen
//! Invariant nicht zu unterscheiden — beides zeigt sich als „Stand null".
//! „Grün, weil es funktioniert" und „grün, weil es niemand aufruft" sehen
//! hier identisch aus. Diese Mechanik kann diese Verwechslung nicht
//! auflösen (das verlangt einen Konsumentennachweis am Aufrufort selbst);
//! sie hält aber fest, was ein Nullzähler *ist*: [`NullCounter::invariant`]
//! trägt die verletzte Invariante immer im Klartext, statt nur einen
//! kryptischen Metriknamen zu zeigen, und [`NullCounterRegistry::snapshot`]
//! zeigt **alle** Zähler, auch die auf null — ein Zähler, den man erst
//! sucht, wenn man ihn vermutet, hilft nicht.
//!
//! # Nebenläufigkeit
//! [`NullCounter`] hält seinen Stand in einem `AtomicU64` und erhöht ihn
//! mit [`std::sync::atomic::Ordering::Relaxed`]. Das genügt hier, weil der
//! Zähler keine anderen Speicherzugriffe ordnet (er ist keine Sperre und
//! kein Signal für ein anderes Datum) und weil die exakte Reihenfolge
//! zweier gleichzeitiger Verletzungen bedeutungslos ist — nur „mehr als
//! null" zählt, nicht „welche Verletzung zuerst sichtbar wurde". Ein
//! `AtomicU64` ohne Interior-Mutability-Sperre macht [`NullCounter`]
//! automatisch `Sync`, also als `&'static` über beliebig viele Threads
//! teilbar, ohne dass diese Mechanik ein `unsafe impl` bräuchte.
//! [`NullCounterRegistry`] trägt nur `&'static`-Referenzen und ohne
//! Interior Mutability; ihr Aufbau (`register`) ist nicht für
//! gleichzeitigen Zugriff aus mehreren Threads vorgesehen — üblicherweise
//! einmalig beim Programmstart.
//!
//! # Fehler
//! [`assert_all_zero`] liefert [`crate::error::ObserveError::NullCounterViolated`],
//! wenn ein Zähler über null steht.
//!
//! # Examples
//! ```
//! use harw_observe::{
//!     Cardinality, MetricKey, MetricKind, NullCounter, NullCounterRegistry, NullSink, Unit,
//!     assert_all_zero,
//! };
//!
//! const EXAMPLE_KEY: MetricKey = MetricKey {
//!     name: "example_invariant_violation_total",
//!     kind: MetricKind::Counter,
//!     unit: Unit::Count,
//!     labels: &[],
//!     cardinality: Cardinality::Single,
//! };
//!
//! static EXAMPLE_COUNTER: NullCounter = NullCounter::new(
//!     &EXAMPLE_KEY,
//!     "kein Fragment mit TrustClass::Data erscheint im Instruktionsblock",
//! );
//!
//! let mut registry = NullCounterRegistry::new();
//! registry.register(&EXAMPLE_COUNTER);
//! assert!(assert_all_zero(&registry).is_ok());
//!
//! let sink = NullSink;
//! EXAMPLE_COUNTER.violated(&sink, &[]);
//! assert_eq!(EXAMPLE_COUNTER.count(), 1);
//! assert!(assert_all_zero(&registry).is_err());
//! ```

use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::ObserveError;
use crate::field::{FieldName, FieldValue};
use crate::metric::{MetricKey, MetricValue};
use crate::sink::TelemetrySink;

/// Ein Zähler, dessen erwarteter Wert im Betrieb null ist.
///
/// # Description
/// Bündelt einen Metrikschlüssel, die Invariante, die er bewacht, und
/// einen atomaren Zählstand. Nur über [`NullCounter::new`] konstruierbar
/// und typischerweise als `static` gehalten (siehe Modul-Beispiel), damit
/// [`NullCounterRegistry`] ihn als `&'static` registrieren kann.
///
/// # Concurrency
/// `Sync` durch die Bauart seiner Felder (`&'static MetricKey`,
/// `&'static str`, `AtomicU64`) — kein `unsafe impl` nötig. Siehe die
/// Modul-Dokumentation für die Begründung von `Ordering::Relaxed`.
#[derive(Debug)]
pub struct NullCounter {
    key: &'static MetricKey,
    invariant: &'static str,
    count: AtomicU64,
}

impl NullCounter {
    /// Registriert einen Nullzähler unter seinem Metrikschlüssel.
    ///
    /// # Description
    /// Baut den Zähler mit Stand null. `const fn`, damit dieser Aufruf in
    /// einer `static`-Deklaration ausgewertet werden kann — der einzige
    /// vorgesehene Konstruktionsweg für einen `&'static NullCounter`, wie
    /// ihn [`NullCounterRegistry::register`] verlangt.
    ///
    /// # Arguments
    /// - `key` (`&'static MetricKey`): der Metrikschlüssel dieses Zählers.
    /// - `invariant` (`&'static str`): die Invariante, die dieser Zähler
    ///   bewacht, im Klartext (z. B. „kein Fragment mit `TrustClass::Data`
    ///   erscheint im Instruktionsblock").
    ///
    /// # Returns
    /// Einen `NullCounter` mit Stand null.
    ///
    /// # Concurrency
    /// `const fn`; reine Werterzeugung ohne Nebenläufigkeitsaspekt.
    ///
    /// # Examples
    /// ```
    /// use harw_observe::{Cardinality, MetricKey, MetricKind, NullCounter, Unit};
    ///
    /// const KEY: MetricKey = MetricKey {
    ///     name: "example_violation_total",
    ///     kind: MetricKind::Counter,
    ///     unit: Unit::Count,
    ///     labels: &[],
    ///     cardinality: Cardinality::Single,
    /// };
    /// static COUNTER: NullCounter = NullCounter::new(&KEY, "example invariant");
    /// assert_eq!(COUNTER.count(), 0);
    /// ```
    #[must_use]
    pub const fn new(key: &'static MetricKey, invariant: &'static str) -> Self {
        Self {
            key,
            invariant,
            count: AtomicU64::new(0),
        }
    }

    /// Erhöht den Zähler und meldet ihn an den Sink.
    ///
    /// # Description
    /// Der Aufruf ist selbst der Beleg, dass die Invariante verletzt
    /// wurde: es gibt keinen zweiten Aufrufer-seitigen Schritt, der eine
    /// Verletzung erst noch feststellen müsste. Erhöht den internen Stand
    /// um eins und meldet den neuen Stand über [`TelemetrySink::record`]
    /// unter dem Metrikschlüssel dieses Zählers.
    ///
    /// # Arguments
    /// - `sink` (`&dyn TelemetrySink`): das Ziel für den Messwert.
    /// - `labels` (`&[(FieldName, FieldValue)]`): konkrete
    ///   Label-Belegungen für diesen Aufruf, weitergereicht an
    ///   [`TelemetrySink::record`].
    ///
    /// # Concurrency
    /// Sicher aus beliebigen Threads gleichzeitig aufrufbar: die Erhöhung
    /// läuft über `AtomicU64::fetch_add` mit `Ordering::Relaxed` (Begründung
    /// in der Modul-Dokumentation); `sink.record` muss laut
    /// [`TelemetrySink`]-Vertrag ebenfalls nebenläufig sicher sein.
    ///
    /// # Examples
    /// ```
    /// use harw_observe::{Cardinality, MetricKey, MetricKind, NullCounter, NullSink, Unit};
    ///
    /// const KEY: MetricKey = MetricKey {
    ///     name: "example_violation_total",
    ///     kind: MetricKind::Counter,
    ///     unit: Unit::Count,
    ///     labels: &[],
    ///     cardinality: Cardinality::Single,
    /// };
    /// static COUNTER: NullCounter = NullCounter::new(&KEY, "example invariant");
    ///
    /// let sink = NullSink;
    /// COUNTER.violated(&sink, &[]);
    /// assert_eq!(COUNTER.count(), 1);
    /// ```
    pub fn violated(&self, sink: &dyn TelemetrySink, labels: &[(FieldName, FieldValue)]) {
        let updated = self.count.fetch_add(1, Ordering::Relaxed) + 1;
        sink.record(self.key, MetricValue::Count(updated), labels);
    }

    /// Der aktuelle Stand.
    ///
    /// # Returns
    /// Die Anzahl bisher gemeldeter Verletzungen; `0`, solange die
    /// Invariante eingehalten wurde.
    ///
    /// # Concurrency
    /// Liest den Stand mit `Ordering::Relaxed` (Begründung in der
    /// Modul-Dokumentation); sicher aus beliebigen Threads aufrufbar.
    ///
    /// # Examples
    /// ```
    /// use harw_observe::{Cardinality, MetricKey, MetricKind, NullCounter, Unit};
    ///
    /// const KEY: MetricKey = MetricKey {
    ///     name: "example_violation_total",
    ///     kind: MetricKind::Counter,
    ///     unit: Unit::Count,
    ///     labels: &[],
    ///     cardinality: Cardinality::Single,
    /// };
    /// static COUNTER: NullCounter = NullCounter::new(&KEY, "example invariant");
    /// assert_eq!(COUNTER.count(), 0);
    /// ```
    #[must_use]
    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    /// Die Invariante, die dieser Zähler bewacht — als Klartext.
    ///
    /// # Returns
    /// Den Text, der bei [`NullCounter::new`] übergeben wurde. Ein Zähler,
    /// der `trust_block_violation` heißt, sagt nicht, **was** verletzt
    /// wurde — dieser Text tut es und wandert in die Meldung von
    /// [`assert_all_zero`].
    ///
    /// # Concurrency
    /// `const fn`; liest ein unveränderliches `&'static str`-Feld.
    ///
    /// # Examples
    /// ```
    /// use harw_observe::{Cardinality, MetricKey, MetricKind, NullCounter, Unit};
    ///
    /// const KEY: MetricKey = MetricKey {
    ///     name: "example_violation_total",
    ///     kind: MetricKind::Counter,
    ///     unit: Unit::Count,
    ///     labels: &[],
    ///     cardinality: Cardinality::Single,
    /// };
    /// static COUNTER: NullCounter = NullCounter::new(&KEY, "example invariant");
    /// assert_eq!(COUNTER.invariant(), "example invariant");
    /// ```
    #[must_use]
    pub const fn invariant(&self) -> &'static str {
        self.invariant
    }

    /// Der Metrikname dieses Zählers.
    ///
    /// # Description
    /// Nicht Teil der ursprünglich skizzierten Signatur, aber nötig, damit
    /// [`NullCounterRegistry`] und [`assert_all_zero`] einen Zähler
    /// benennen können, ohne seinen Metrikschlüssel offenzulegen.
    ///
    /// # Returns
    /// `key.name` des bei [`NullCounter::new`] übergebenen
    /// [`MetricKey`].
    ///
    /// # Concurrency
    /// `const fn`; liest ein unveränderliches `&'static MetricKey`-Feld.
    ///
    /// # Examples
    /// ```
    /// use harw_observe::{Cardinality, MetricKey, MetricKind, NullCounter, Unit};
    ///
    /// const KEY: MetricKey = MetricKey {
    ///     name: "example_violation_total",
    ///     kind: MetricKind::Counter,
    ///     unit: Unit::Count,
    ///     labels: &[],
    ///     cardinality: Cardinality::Single,
    /// };
    /// static COUNTER: NullCounter = NullCounter::new(&KEY, "example invariant");
    /// assert_eq!(COUNTER.name(), "example_violation_total");
    /// ```
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.key.name
    }
}

/// Ein Verzeichnis aller Nullzähler.
///
/// # Description
/// Damit die Oberfläche (Knoten UI-04) sie **als Leiste** zeigen kann —
/// alle, immer sichtbar, auch die auf null. Trägt nur `&'static
/// NullCounter`-Referenzen; die Zähler selbst leben als `static`-Items bei
/// den Knoten, die ihre Invariante einführen.
///
/// # Concurrency
/// Kein `Sync`-Anspruch: der Aufbau (`register`) ist für einmaligen
/// Gebrauch beim Programmstart aus einem Thread vorgesehen, nicht für
/// gleichzeitige Mutation. Die registrierten [`NullCounter`] selbst sind
/// `Sync` und beliebig nebenläufig lesbar/erhöhbar, unabhängig davon, wer
/// die Registry hält.
#[derive(Debug, Default)]
pub struct NullCounterRegistry {
    counters: Vec<&'static NullCounter>,
}

impl NullCounterRegistry {
    /// Baut eine leere Registrierung.
    ///
    /// # Returns
    /// Eine `NullCounterRegistry` ohne registrierte Zähler.
    ///
    /// # Examples
    /// ```
    /// use harw_observe::NullCounterRegistry;
    ///
    /// let registry = NullCounterRegistry::new();
    /// assert!(registry.all().is_empty());
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self {
            counters: Vec::new(),
        }
    }

    /// Registriert einen Nullzähler.
    ///
    /// # Description
    /// Hängt `counter` ans Ende der Registrierung an; die Reihenfolge der
    /// `register`-Aufrufe bestimmt die Reihenfolge in [`NullCounterRegistry::all`],
    /// [`NullCounterRegistry::snapshot`] und der von der Leiste gezeigten
    /// Liste.
    ///
    /// # Arguments
    /// - `counter` (`&'static NullCounter`): der zu registrierende Zähler.
    ///
    /// # Examples
    /// ```
    /// use harw_observe::{Cardinality, MetricKey, MetricKind, NullCounter, NullCounterRegistry, Unit};
    ///
    /// const KEY: MetricKey = MetricKey {
    ///     name: "example_violation_total",
    ///     kind: MetricKind::Counter,
    ///     unit: Unit::Count,
    ///     labels: &[],
    ///     cardinality: Cardinality::Single,
    /// };
    /// static COUNTER: NullCounter = NullCounter::new(&KEY, "example invariant");
    ///
    /// let mut registry = NullCounterRegistry::new();
    /// registry.register(&COUNTER);
    /// assert_eq!(registry.all().len(), 1);
    /// ```
    pub fn register(&mut self, counter: &'static NullCounter) {
        self.counters.push(counter);
    }

    /// Alle registrierten Zähler, in Registrierungsreihenfolge.
    ///
    /// # Returns
    /// Einen Blick auf alle registrierten `&'static NullCounter`.
    #[must_use]
    pub fn all(&self) -> &[&'static NullCounter] {
        &self.counters
    }

    /// Alle Zähler mit ihrem aktuellen Stand — die Datenquelle der Leiste.
    ///
    /// # Description
    /// Enthält **alle** registrierten Zähler, auch die auf null: eine
    /// Leiste, die nur Verletzungen zeigt, ist eine Liste, die man erst
    /// aufruft, wenn man einen Verdacht hat — genau das soll diese Leiste
    /// nicht sein.
    ///
    /// # Returns
    /// Ein `Vec` aus `(Metrikname, Stand)` in Registrierungsreihenfolge.
    ///
    /// # Concurrency
    /// Liest jeden Zählstand über [`NullCounter::count`]
    /// (`Ordering::Relaxed`); sicher, während andere Threads gleichzeitig
    /// erhöhen — der Schnappschuss kann Zwischenwerte zeigen, was für eine
    /// Anzeige unschädlich ist.
    ///
    /// # Examples
    /// ```
    /// use harw_observe::{Cardinality, MetricKey, MetricKind, NullCounter, NullCounterRegistry, Unit};
    ///
    /// const KEY: MetricKey = MetricKey {
    ///     name: "example_violation_total",
    ///     kind: MetricKind::Counter,
    ///     unit: Unit::Count,
    ///     labels: &[],
    ///     cardinality: Cardinality::Single,
    /// };
    /// static COUNTER: NullCounter = NullCounter::new(&KEY, "example invariant");
    ///
    /// let mut registry = NullCounterRegistry::new();
    /// registry.register(&COUNTER);
    /// assert_eq!(registry.snapshot(), vec![("example_violation_total", 0)]);
    /// ```
    #[must_use]
    pub fn snapshot(&self) -> Vec<(&'static str, u64)> {
        self.counters
            .iter()
            .map(|c| (c.name(), c.count()))
            .collect()
    }

    /// Die Zähler, die über null stehen. Leer heißt: keine Verletzung.
    ///
    /// # Returns
    /// Ein `Vec` aus `(Metrikname, Stand)` für jeden registrierten Zähler
    /// mit `count() > 0`, in Registrierungsreihenfolge.
    ///
    /// # Concurrency
    /// Liest jeden Zählstand über [`NullCounter::count`]
    /// (`Ordering::Relaxed`); siehe [`NullCounterRegistry::snapshot`].
    ///
    /// # Examples
    /// ```
    /// use harw_observe::{Cardinality, MetricKey, MetricKind, NullCounter, NullCounterRegistry, NullSink, Unit};
    ///
    /// const KEY: MetricKey = MetricKey {
    ///     name: "example_violation_total",
    ///     kind: MetricKind::Counter,
    ///     unit: Unit::Count,
    ///     labels: &[],
    ///     cardinality: Cardinality::Single,
    /// };
    /// static COUNTER: NullCounter = NullCounter::new(&KEY, "example invariant");
    ///
    /// let mut registry = NullCounterRegistry::new();
    /// registry.register(&COUNTER);
    /// assert!(registry.violated().is_empty());
    ///
    /// COUNTER.violated(&NullSink, &[]);
    /// assert_eq!(registry.violated(), vec![("example_violation_total", 1)]);
    /// ```
    #[must_use]
    pub fn violated(&self) -> Vec<(&'static str, u64)> {
        self.counters
            .iter()
            .filter(|c| c.count() > 0)
            .map(|c| (c.name(), c.count()))
            .collect()
    }
}

/// Prüft, ob ein Nullzähler unerwartet über null steht.
///
/// # Description
/// Das, was ein Test oder ein Betriebs-Check aufruft. Findet den ersten
/// registrierten Zähler (in Registrierungsreihenfolge) mit `count() > 0`
/// und meldet ihn vollständig: Name, Stand und Invariante — eine Meldung
/// „ein Nullzähler ist nicht null" allein wäre wertlos.
///
/// # Arguments
/// - `registry` (`&NullCounterRegistry`): die zu prüfende Registrierung.
///
/// # Returns
/// `Ok(())`, wenn jeder registrierte Zähler auf null steht (leere
/// Registrierung eingeschlossen).
///
/// # Errors
/// - [`ObserveError::NullCounterViolated`]: mit Name, Stand und Invariante
///   des ersten gefundenen Zählers, der über null steht.
///
/// # Concurrency
/// Liest jeden Zählstand über [`NullCounter::count`] (`Ordering::Relaxed`);
/// sicher aufrufbar, während andere Threads gleichzeitig erhöhen.
///
/// # Examples
/// ```
/// use harw_observe::{
///     Cardinality, MetricKey, MetricKind, NullCounter, NullCounterRegistry, NullSink, Unit,
///     assert_all_zero,
/// };
///
/// const KEY: MetricKey = MetricKey {
///     name: "example_violation_total",
///     kind: MetricKind::Counter,
///     unit: Unit::Count,
///     labels: &[],
///     cardinality: Cardinality::Single,
/// };
/// static COUNTER: NullCounter = NullCounter::new(&KEY, "example invariant");
///
/// let mut registry = NullCounterRegistry::new();
/// registry.register(&COUNTER);
/// assert!(assert_all_zero(&registry).is_ok());
///
/// COUNTER.violated(&NullSink, &[]);
/// assert!(assert_all_zero(&registry).is_err());
/// ```
pub fn assert_all_zero(registry: &NullCounterRegistry) -> Result<(), ObserveError> {
    match registry.all().iter().find(|c| c.count() > 0) {
        Some(counter) => Err(ObserveError::NullCounterViolated {
            name: counter.name(),
            count: counter.count(),
            invariant: counter.invariant(),
        }),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::thread;

    use super::*;
    use crate::metric::{Cardinality, MetricKind, Unit};
    use crate::test_support::{TestError, TestResult};

    const EXAMPLE_KEY: MetricKey = MetricKey {
        name: "example_invariant_violation_total",
        kind: MetricKind::Counter,
        unit: Unit::Count,
        labels: &[],
        cardinality: Cardinality::Single,
    };

    const OTHER_KEY: MetricKey = MetricKey {
        name: "other_invariant_violation_total",
        kind: MetricKind::Counter,
        unit: Unit::Count,
        labels: &[],
        cardinality: Cardinality::Single,
    };

    const EXAMPLE_INVARIANT: &str =
        "kein Fragment mit TrustClass::Data erscheint im Instruktionsblock";

    /// Aufzeichnender Test-Sink: hält jeden `record()`-Aufruf fest, statt
    /// ihn zu verwerfen wie [`crate::sink::NullSink`].
    /// Ein aufgezeichneter Aufruf: der Wert und die Felder, mit denen er kam.
    type RecordedCall = (MetricValue, Vec<(FieldName, FieldValue)>);

    #[derive(Debug, Default)]
    struct RecordingSink {
        records: Mutex<Vec<RecordedCall>>,
    }

    impl TelemetrySink for RecordingSink {
        fn record(&self, _key: &MetricKey, value: MetricValue, labels: &[(FieldName, FieldValue)]) {
            self.records
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push((value, labels.to_vec()));
        }

        fn flush(&self) {}

        fn name(&self) -> &'static str {
            "recording"
        }
    }

    #[test]
    fn test_null_counter_new_starts_at_zero() {
        static COUNTER: NullCounter = NullCounter::new(&EXAMPLE_KEY, EXAMPLE_INVARIANT);
        assert_eq!(COUNTER.count(), 0);
        assert_eq!(COUNTER.invariant(), EXAMPLE_INVARIANT);
        assert_eq!(COUNTER.name(), "example_invariant_violation_total");
    }

    #[test]
    fn test_null_counter_violated_increments_and_records() {
        static COUNTER: NullCounter = NullCounter::new(&EXAMPLE_KEY, EXAMPLE_INVARIANT);
        let sink = RecordingSink::default();

        COUNTER.violated(&sink, &[]);

        assert_eq!(COUNTER.count(), 1);
        let records = sink.records.lock().unwrap_or_else(|p| p.into_inner());
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].0, MetricValue::Count(1));
        assert!(records[0].1.is_empty());
    }

    #[test]
    fn test_assert_all_zero_ok_when_all_zero() {
        static COUNTER: NullCounter = NullCounter::new(&EXAMPLE_KEY, EXAMPLE_INVARIANT);
        let mut registry = NullCounterRegistry::new();
        registry.register(&COUNTER);

        assert!(assert_all_zero(&registry).is_ok());
    }

    #[test]
    fn test_assert_all_zero_names_counter_count_and_invariant() -> TestResult {
        static COUNTER: NullCounter = NullCounter::new(&EXAMPLE_KEY, EXAMPLE_INVARIANT);
        let sink = RecordingSink::default();
        COUNTER.violated(&sink, &[]);

        let mut registry = NullCounterRegistry::new();
        registry.register(&COUNTER);

        let Err(err) = assert_all_zero(&registry) else {
            return Err(TestError::Unexpected(
                "expected assert_all_zero to report the violated counter".to_owned(),
            ));
        };
        let ObserveError::NullCounterViolated {
            name,
            count,
            invariant,
        } = err
        else {
            return Err(TestError::Unexpected(
                "expected ObserveError::NullCounterViolated".to_owned(),
            ));
        };
        assert_eq!(name, "example_invariant_violation_total");
        assert_eq!(count, 1);
        assert_eq!(invariant, EXAMPLE_INVARIANT);
        assert_eq!(
            err.to_string(),
            format!(
                "null counter 'example_invariant_violation_total' is at 1 (expected zero); \
                 invariant: {EXAMPLE_INVARIANT}"
            )
        );
        Ok(())
    }

    #[test]
    fn test_null_counter_registry_snapshot_includes_all_counters() {
        static ZERO_COUNTER: NullCounter = NullCounter::new(&EXAMPLE_KEY, EXAMPLE_INVARIANT);
        static NONZERO_COUNTER: NullCounter = NullCounter::new(&OTHER_KEY, "other invariant");
        let sink = RecordingSink::default();
        NONZERO_COUNTER.violated(&sink, &[]);

        let mut registry = NullCounterRegistry::new();
        registry.register(&ZERO_COUNTER);
        registry.register(&NONZERO_COUNTER);

        let snapshot = registry.snapshot();
        assert_eq!(
            snapshot,
            vec![
                ("example_invariant_violation_total", 0),
                ("other_invariant_violation_total", 1),
            ]
        );
    }

    #[test]
    fn test_null_counter_registry_violated_contains_only_nonzero() {
        static ZERO_COUNTER: NullCounter = NullCounter::new(&EXAMPLE_KEY, EXAMPLE_INVARIANT);
        static NONZERO_COUNTER: NullCounter = NullCounter::new(&OTHER_KEY, "other invariant");
        let sink = RecordingSink::default();
        NONZERO_COUNTER.violated(&sink, &[]);

        let mut registry = NullCounterRegistry::new();
        registry.register(&ZERO_COUNTER);
        registry.register(&NONZERO_COUNTER);

        assert_eq!(
            registry.violated(),
            vec![("other_invariant_violation_total", 1)]
        );
    }

    #[test]
    fn test_null_counter_violated_is_thread_safe_under_concurrent_increments() -> TestResult {
        static COUNTER: NullCounter = NullCounter::new(&EXAMPLE_KEY, EXAMPLE_INVARIANT);
        let sink: Arc<dyn TelemetrySink> = Arc::new(RecordingSink::default());

        let handles: Vec<_> = (0..2)
            .map(|_| {
                let sink = Arc::clone(&sink);
                thread::spawn(move || {
                    for _ in 0..100 {
                        COUNTER.violated(sink.as_ref(), &[]);
                    }
                })
            })
            .collect();

        for handle in handles {
            handle
                .join()
                .map_err(|_| TestError::Unexpected("worker thread panicked".to_owned()))?;
        }

        assert_eq!(COUNTER.count(), 200);
        Ok(())
    }
}
