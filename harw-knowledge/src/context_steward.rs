//! Der Context Steward (Knoten **AW6-08**) — pflegt Kontextprogramme, indem
//! er bereits abgerufene [`crate::context_proposal::ContextProposal`]- und
//! [`crate::model_behavior_proposal::ModelBehaviorProposal`]-Bestände
//! aufbereitet, ordnet und zusammenfasst. Trägt außerdem die drei
//! `steward_*`-Nullzähler, die der Plan (`docs/aw-plan.md`, K7) diesem
//! Knoten zuweist, ohne sie zu benennen.
//!
//! # Warum es keine `apply`-Funktion gibt
//! Dieselbe tragende Regel wie bei [`crate::context_proposal`] (AW5-09) und
//! [`crate::model_behavior_proposal`] (AW6-06): **schlägt vor, committet
//! nie.** Der Steward liest keinen Kontextprogramm- oder Katalog-Zustand
//! selbst, sondern ausschließlich schon vom Aufrufer materialisierte
//! `&[ContextProposal]`/`&[ModelBehaviorProposal]`-Batches — jede Funktion in
//! diesem Modul ist eine reine, totale Funktion über diese Slices und ein
//! [`StewardWindow`]. Es gibt hier keinen Aufruf von
//! `harw_agent_dsl::context_program::resolve_context_program`, keinen
//! Schreibzugriff auf eine `.toml`-Kontextprogrammdatei, keinen Aufruf in
//! `harw-model-catalog`, der einen `ModelDescriptor` fortschreibt — exakt
//! dieselbe strukturelle Garantie, die die Moduldoku von `context_proposal`
//! und `model_behavior_proposal` für ihre eigenen Typen bereits belegt.
//!
//! # Kein zweiter, ungeprüfter Lesepfad
//! Dieses Modul öffnet **keinen** Weg, `harw_knowledge::index::KnowledgeIndex`
//! direkt zu durchsuchen. Es nimmt ausschließlich bereits sichtbarkeitsgeprüfte
//! `Vec<ContextProposal>`/`Vec<ModelBehaviorProposal>` entgegen, die der
//! Aufrufer zuvor selbst über
//! [`crate::memory::recall::search_with`] (mit
//! `VisibilityScope::OperatorOnly`, wie es
//! `harw-ops/src/context_proposal.rs` bereits für `ContextProposal` tut) und
//! `ContextProposal::from_artifact`/`ModelBehaviorProposal::from_artifact`
//! gewonnen hat. Die Sichtbarkeitsprüfung selbst bleibt — wie überall in
//! dieser Crate — ausschließlich in `search_with` verankert, bei jedem
//! Rückverweis-Sprung erneut (K35); dieses Modul umgeht sie nicht, weil es
//! sie gar nicht erst erreichen kann: es sieht nur, was der Aufrufer bereits
//! als sichtbar bestätigt bekommen hat.
//!
//! # Die Fensterlänge — Entscheidung und Begründung
//! Der Plan überlässt die Fensterlänge ausdrücklich diesem Knoten. Die
//! Entscheidung ist [`DEFAULT_STEWARD_WINDOW_SPAN_DAYS`] `= 7` (eine
//! Kalenderwoche), umgesetzt über [`StewardWindow::trailing`] — **keine
//! Systemuhr**: `as_of` wird immer vom Aufrufer hereingereicht (z. B. der
//! Zeitpunkt, zu dem ein von [`DreamSchedule`] freigegebener Lauf beginnt), nie aus
//! `jiff::Timestamp::now()` gelesen. Begründung:
//!
//! - Die Lauf-Kadenz liefert dieses Modul selbst: [`DreamSchedule`] über
//!   einem abhängigkeitsfreien 5-Feld-Cron-Ausdruck ([`CronSchedule`], siehe
//!   Abschnitt "Der Dream-Job-Zeitplan" unten), mit
//!   [`DEFAULT_DREAM_CRON_EXPRESSION`] `= "0 3 * * 1"` (montags 03:00 UTC) als
//!   Empfehlung. Wöchentlicher Lauf und siebentägiges Fenster sind bewusst
//!   aufeinander abgestimmt: jeder planmäßige Lauf sieht genau die Woche seit
//!   dem vorigen Lauf, ohne Lücke und ohne Doppelzählung.
//! - Sieben Tage sind lang genug, dass die Wiederholungsschwellen, die
//!   `ContextProposal`/`ModelBehaviorProposal` selbst voraussetzen
//!   ([`crate::context_proposal::OVER_BUDGET_PROMOTION_THRESHOLD`] `= 3`,
//!   [`crate::model_behavior_proposal::MIN_RELIABLE_SAMPLE_COUNT`] `= 5`),
//!   im normalen Betrieb überhaupt Gelegenheit haben, erreicht zu werden,
//!   bevor sie zusammengefasst würden.
//! - Sieben Tage sind kurz genug, dass ein `Pending`-Vorschlag nicht über
//!   Wochen unbemerkt liegen bleibt, bevor ihn ein Operator zu Gesicht
//!   bekommt — eine Kalenderwoche ist die kürzeste Spanne, die sich
//!   unabhängig von der konkret konfigurierten Lauf-Kadenz plausibel als
//!   "ein Mensch schaut mindestens so oft vorbei" begründen lässt.
//!
//! Das Fenster bleibt trotzdem immer ein expliziter Parameter, nie ein
//! interner Default, den eine Funktion selbst zieht: [`curate_context_proposals`]
//! und [`curate_model_behavior_proposals`] nehmen `&StewardWindow` entgegen,
//! [`DEFAULT_STEWARD_WINDOW_SPAN_DAYS`]/[`StewardWindow::trailing`] sind nur
//! die dokumentierte Empfehlung für den Aufrufer, der noch keine eigene
//! Spanne hat.
//!
//! # Der Dream-Job-Zeitplan
//! [`CronSchedule`] ist ein kleiner, abhängigkeitsfreier Parser/Auswerter für
//! klassische 5-Feld-Cron-Ausdrücke (`Minute Stunde Monatstag Monat
//! Wochentag`), ausgewertet **immer in UTC** über `jiff::Timestamp` — keine
//! Zeitzonendatenbank, keine Sommerzeit-Sonderfälle. Unterstützt werden `*`,
//! Einzelwerte, Listen (`a,b`), Bereiche (`a-b`) und Schritte (`*/n`,
//! `a-b/n`, sowie `a/n` als Kurzform für `a-<max>/n`). Wochentag `0` und `7`
//! bedeuten beide Sonntag. Namen (`MON`, `JAN`) und Erweiterungen (`L`, `W`,
//! `#`, `?`, Sekundenfeld) werden bewusst **nicht** unterstützt und als
//! [`CronParseError`] abgewiesen.
//!
//! Monatstag/Wochentag folgen der Vixie-Cron-Semantik: sind **beide** Felder
//! eingeschränkt (beginnen nicht mit `*`), feuert der Ausdruck, wenn
//! **eines** der beiden passt (`0 0 1 * 1` = jeden Monatsersten *und* jeden
//! Montag); sonst müssen beide passen (ein mit `*` beginnendes Feld wie
//! `*/2` gilt dabei — wie bei Vixie-Cron — als nicht eingeschränkt).
//!
//! [`DreamSchedule`] entscheidet über einem solchen Ausdruck, ob ein
//! Dream-/Konsolidierungslauf fällig ist ([`DreamSchedule::decide`]) — rein
//! aus dem hereingereichten letzten Laufzeitpunkt und `now`, **nie** aus der
//! Systemuhr. Versäumte Feuerzeitpunkte werden zu **einem** Nachholauf
//! zusammengefasst (wie `anacron`), nie einzeln nachgespielt; ein nie
//! gelaufener Job ist sofort fällig. [`steward_digest_if_due`] verbindet
//! Zeitplan und Steward: ist der Lauf fällig, baut es den [`StewardDigest`]
//! über dem Standardfenster, sonst nichts. Der Aufrufer bleibt für das
//! dauerhafte Festhalten von `last_run` und das eigentliche Anstoßen des
//! Jobs (z. B. über `harw-job-runtime`) zuständig.
//!
//! # Die drei Nullzähler
//! Alle drei liegen — wie die Mechanik es verlangt (`harw-observe`,
//! AW1-07) — als `'static` [`harw_observe::NullCounter`] in diesem Modul, wo
//! die jeweilige Invariante eingeführt wird. Jeder erhöht sich an einer
//! Stelle **außerhalb** dieses Moduls (in `artifact.rs`, `memory/recall.rs`
//! bzw. `context_proposal.rs`), weil genau dort der einzige heute real
//! erreichte Produktionsaufrufer sitzt:
//! `harw-cli` → `harw_ops::register_all` → `harw-ops/src/context_proposal.rs`
//! (`/context-proposal list|view|accept|reject`) →
//! `crate::memory::recall::search_with` /
//! `crate::context_proposal::ContextProposal::{accept,reject}`.
//!
//! - [`STEWARD_CEILING_VIOLATION`] (`steward_ceiling_violation_total`) —
//!   Invariante: *eine über diese Crate laufende Recall-Anfrage überschreitet
//!   nie die harte Obergrenze* (`MAX_RECALL_ARTIFACTS`/`MAX_RECALL_HOPS`,
//!   §2.3). Hängt an [`crate::artifact::RecallQuery::validate`], dem ersten
//!   Aufruf in jedem [`crate::memory::recall::search_with`]. **Erreicht**:
//!   `validate()` läuft bei jedem `/context-proposal`-Aufruf; heute bauen
//!   alle registrierten Aufrufer ihre Anfrage über `RecallQuery::new` (Bounds
//!   exakt an der Obergrenze), der Zweig läuft also mit, löst aber
//!   (korrekt) nie aus.
//! - [`STEWARD_LEAK_VIOLATION`] (`steward_leak_violation_total`) —
//!   Invariante: *kein Artefakt vom Typ `ContextProposal` oder
//!   `ModelBehaviorProposal` erscheint in einem `RecallResult`, dessen
//!   eigene Sichtbarkeit den `caller_scope` nicht erlaubt* — dieselbe
//!   K35-Familie, hier als Verteidigung in der Tiefe statt als einzige
//!   Prüfung. Hängt an einer Nachprüfung am Ende von `search_with`
//!   (`memory/recall.rs::verify_no_steward_domain_leak`). **Erreicht,
//!   teilweise**: der `ContextProposal`-Zweig läuft bei jedem
//!   `/context-proposal`-Aufruf mit (dessen Anfrage immer nach
//!   `ArtifactKind::ContextProposal` filtert); der
//!   `ModelBehaviorProposal`-Zweig hat heute **keinen** registrierten
//!   Aufrufer (kein `/model-behavior-proposal`-Kommando existiert) — diese
//!   Lücke wird hier benannt, nicht verschwiegen.
//! - [`STEWARD_REDECISION_VIOLATION`] (`steward_redecision_violation_total`)
//!   — Invariante: *ein `ContextProposal` wird nie ein zweites Mal
//!   entschieden* (`accept`/`reject` sind terminal). Hängt an
//!   `ContextProposal::transition_to`s Fehlerzweig. **Erreicht**: jeder
//!   `/context-proposal accept|reject <id>`-Aufruf durchläuft
//!   `transition_to`; der Zweig löst nur aus, wenn ein Operator (oder ein
//!   zweiter, konkurrierender Aufruf) denselben Vorschlag doppelt entscheidet.
//!
//! Ein vierter, ursprünglich erwogener Zähler — auf `ModelBehaviorProposal`s
//! eigenem `transition_to` — wurde **bewusst nicht gebaut**: dafür gibt es
//! (siehe oben) heute keinen registrierten Aufrufer irgendeiner Art (weder
//! `/context-proposal` noch ein gleichwertiges Kommando), also wäre der Zähler
//! dauerhaft blind — exakt die Falle, die `scope_violation_rate` bereits einmal
//! traf. Zwei vollständig erreichte Zähler und ein teilweise erreichter sind
//! ehrlicher als vier, von denen einer nie liefe.
//!
//! # Sichtbarkeit
//! Dieses Modul erzeugt keine eigenen Artefakte und trifft keine
//! Sichtbarkeitsentscheidung — es liest nur, was der Aufrufer bereits über
//! `search_with` mit `VisibilityScope::OperatorOnly` bestätigt bekommen hat
//! (siehe `RECOMMENDED_VISIBILITY` in `context_proposal`/
//! `model_behavior_proposal`).
//!
//! # Nebenläufigkeit
//! [`StewardWindow`], [`CuratedContextProposals`], [`CuratedModelBehaviorProposals`],
//! [`StewardDigest`], [`CronSchedule`] und [`DreamSchedule`] sind reine
//! `Send + Sync`-Werttypen ohne innere Veränderlichkeit. Die drei `NullCounter` sind nach ihrer eigenen
//! Dokumentation `Sync` und beliebig nebenläufig erhöh-/lesbar
//! (`AtomicU64`, `Ordering::Relaxed`).
//!
//! # Fehler
//! [`crate::error::KnowledgeError::StewardWindowInvalid`], wenn
//! [`StewardWindow::new`]/[`StewardWindow::trailing`] mit einer
//! widersprüchlichen Spanne aufgerufen wird; [`CronParseError`] (ein eigener,
//! modullokaler Fehlertyp) für einen ungültigen Cron-Ausdruck.

use harw_observe::{Cardinality, MetricKey, MetricKind, NullCounter, NullCounterRegistry, Unit};

use crate::context_proposal::{ContextProposal, ProposalStatus};
use crate::error::{KnowledgeError, KnowledgeResult};
use crate::model_behavior_proposal::ModelBehaviorProposal;

/// Empfohlene Fensterlänge in Tagen für [`StewardWindow::trailing`] (siehe
/// Moduldoku, "Die Fensterlänge — Entscheidung und Begründung").
pub const DEFAULT_STEWARD_WINDOW_SPAN_DAYS: i64 = 7;

/// Metrikschlüssel für [`STEWARD_CEILING_VIOLATION`].
const STEWARD_CEILING_VIOLATION_KEY: MetricKey = MetricKey {
    name: "steward_ceiling_violation_total",
    kind: MetricKind::Counter,
    unit: Unit::Count,
    labels: &[],
    cardinality: Cardinality::Single,
};

/// Metrikschlüssel für [`STEWARD_LEAK_VIOLATION`].
const STEWARD_LEAK_VIOLATION_KEY: MetricKey = MetricKey {
    name: "steward_leak_violation_total",
    kind: MetricKind::Counter,
    unit: Unit::Count,
    labels: &[],
    cardinality: Cardinality::Single,
};

/// Metrikschlüssel für [`STEWARD_REDECISION_VIOLATION`].
const STEWARD_REDECISION_VIOLATION_KEY: MetricKey = MetricKey {
    name: "steward_redecision_violation_total",
    kind: MetricKind::Counter,
    unit: Unit::Count,
    labels: &[],
    cardinality: Cardinality::Single,
};

/// Nullzähler: eine Recall-Anfrage überschreitet nie die harte Obergrenze
/// (`MAX_RECALL_ARTIFACTS`/`MAX_RECALL_HOPS`, §2.3). Erhöht in
/// [`crate::artifact::RecallQuery::validate`]. Siehe Moduldoku für die
/// Invariante und den Erreichbarkeitsnachweis im Detail.
pub static STEWARD_CEILING_VIOLATION: NullCounter = NullCounter::new(
    &STEWARD_CEILING_VIOLATION_KEY,
    "eine über harw-knowledge laufende Recall-Anfrage (RecallQuery) überschreitet nie MAX_RECALL_ARTIFACTS/MAX_RECALL_HOPS",
);

/// Nullzähler: kein `ContextProposal`/`ModelBehaviorProposal`-Artefakt
/// erscheint in einem `RecallResult`, dessen Sichtbarkeit den anfragenden
/// `caller_scope` nicht erlaubt. Erhöht in
/// `crate::memory::recall::verify_no_steward_domain_leak`. Siehe Moduldoku
/// für die Invariante und den (teilweisen) Erreichbarkeitsnachweis.
pub static STEWARD_LEAK_VIOLATION: NullCounter = NullCounter::new(
    &STEWARD_LEAK_VIOLATION_KEY,
    "kein ContextProposal- oder ModelBehaviorProposal-Artefakt erscheint in einem RecallResult, dessen eigene Sichtbarkeit den anfragenden caller_scope nicht erlaubt",
);

/// Nullzähler: ein `ContextProposal` wird nie ein zweites Mal entschieden
/// (`accept`/`reject` sind terminal). Erhöht in
/// [`crate::context_proposal::ContextProposal`]s privater
/// `transition_to`-Übergangslogik. Siehe Moduldoku für den
/// Erreichbarkeitsnachweis.
pub static STEWARD_REDECISION_VIOLATION: NullCounter = NullCounter::new(
    &STEWARD_REDECISION_VIOLATION_KEY,
    "ein ContextProposal wird nie ein zweites Mal entschieden (accept/reject sind terminal)",
);

/// Baut ein [`NullCounterRegistry`] mit allen drei Steward-Nullzählern, in
/// der Reihenfolge, in der sie oben dokumentiert sind.
///
/// # Description
/// Für Betrieb (eine künftige "Leiste", die alle Nullzähler zeigt, auch die
/// auf null — siehe `harw_observe::null_counter`s Moduldoku) und für Tests,
/// die [`harw_observe::assert_all_zero`] gegen genau diese drei Zähler
/// laufen lassen wollen, ohne sie einzeln aufzuzählen.
///
/// # Returns
/// Ein [`NullCounterRegistry`] mit [`STEWARD_CEILING_VIOLATION`],
/// [`STEWARD_LEAK_VIOLATION`] und [`STEWARD_REDECISION_VIOLATION`]
/// registriert.
///
/// # Examples
/// ```rust
/// use harw_knowledge::context_steward::steward_null_counter_registry;
///
/// let registry = steward_null_counter_registry();
/// assert_eq!(registry.all().len(), 3);
/// ```
#[must_use]
pub fn steward_null_counter_registry() -> NullCounterRegistry {
    let mut registry = NullCounterRegistry::new();
    registry.register(&STEWARD_CEILING_VIOLATION);
    registry.register(&STEWARD_LEAK_VIOLATION);
    registry.register(&STEWARD_REDECISION_VIOLATION);
    registry
}

/// Serialisiert alle Tests (in diesem Modul, in `crate::artifact`, in
/// `crate::memory::recall` und in `crate::context_proposal`), die einen der
/// drei prozessweiten Steward-Nullzähler ([`STEWARD_CEILING_VIOLATION`],
/// [`STEWARD_LEAK_VIOLATION`], [`STEWARD_REDECISION_VIOLATION`]) lesen oder
/// erhöhen — dieselbe Begründung, aus der `harw-core/src/context_budget.rs`
/// seinen `TRUST_BLOCK_VIOLATION`-Test und
/// `harw-secrets/src/audit/telemetry.rs` seinen `AUDIT_CHAIN_BREAK`-Test
/// hinter einem eigenen Lock serialisieren: ein `static NullCounter` ist
/// prozessweit geteilt, und `cargo test` läuft standardmäßig mit mehreren
/// Threads im selben Binary.
///
/// Eine **gemeinsame** Sperre für alle drei Zähler statt dreier getrennter:
/// nur sechs Tests im gesamten Crate lesen einen dieser Zähler, verteilt
/// über vier Dateien, und keiner davon läuft lange. Der Durchsatzverlust
/// einer gemeinsamen Sperre ist damit vernachlässigbar, während drei
/// getrennte Sperren nur ein zusätzliches Risiko schüfen: ein künftiger Test
/// nimmt aus Versehen die falsche (oder gar keine) Sperre für "seinen"
/// Zähler, weil er die Zuordnung Zähler→Sperre erst nachschlagen muss.
///
/// Eine Sperre **allein** genügt hier nicht, wie das Vorbild in
/// `context_budget.rs` festhält: alle sechs Tests bilden bereits eine
/// **Differenz** (`before`/`after` statt eines absoluten Werts, denn keiner
/// der drei Zähler ist zu Testbeginn garantiert null), aber selbst eine
/// Differenz kann zwischen Messung und Zusicherung von einem parallel
/// laufenden Test verfälscht werden — genau der hier gemeldete Fehlschlag.
/// Erst Differenzmessung **und** diese Sperre zusammen machen die Aussage
/// "diese Operation erhöht den Zähler nicht" wieder zuverlässig prüfbar.
#[cfg(test)]
pub(crate) static STEWARD_COUNTER_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Ein explizites, vom Aufrufer gefülltes Beobachtungsfenster — **keine
/// Systemuhr** (siehe Moduldoku, "Die Fensterlänge").
///
/// # Examples
/// ```rust
/// use harw_knowledge::context_steward::StewardWindow;
///
/// let window = StewardWindow::new(jiff::Timestamp::UNIX_EPOCH, jiff::Timestamp::UNIX_EPOCH)
///     .expect("since == until is a valid (zero-width) window");
/// assert!(window.contains(jiff::Timestamp::UNIX_EPOCH));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StewardWindow {
    /// Fensteranfang (einschließlich).
    pub since: jiff::Timestamp,
    /// Fensterende (einschließlich).
    pub until: jiff::Timestamp,
}

impl StewardWindow {
    /// Baut ein Fenster aus expliziten Grenzen.
    ///
    /// # Arguments
    /// - `since` (`jiff::Timestamp`): Fensteranfang, hereingereicht.
    /// - `until` (`jiff::Timestamp`): Fensterende, hereingereicht.
    ///
    /// # Returns
    /// Ein gültiges [`StewardWindow`].
    ///
    /// # Errors
    /// [`KnowledgeError::StewardWindowInvalid`], wenn `since` nach `until`
    /// liegt.
    ///
    /// # Examples
    /// Siehe die Typdokumentation von [`StewardWindow`].
    pub fn new(since: jiff::Timestamp, until: jiff::Timestamp) -> KnowledgeResult<Self> {
        if since > until {
            return Err(KnowledgeError::StewardWindowInvalid {
                detail: format!("since ({since}) liegt nach until ({until})"),
            });
        }
        Ok(Self { since, until })
    }

    /// Baut ein auf `as_of` endendes Fenster mit `span_days` Tagen Breite
    /// (siehe [`DEFAULT_STEWARD_WINDOW_SPAN_DAYS`] für die empfohlene Spanne).
    ///
    /// # Description
    /// `as_of` wird vollständig vom Aufrufer bestimmt — diese Funktion liest
    /// nie `jiff::Timestamp::now()`. `span_days` muss positiv sein.
    ///
    /// # Arguments
    /// - `as_of` (`jiff::Timestamp`): Fensterende, hereingereicht.
    /// - `span_days` (`i64`): Fensterbreite in Tagen; muss `> 0` sein.
    ///
    /// # Returns
    /// Ein [`StewardWindow`] mit `until = as_of` und
    /// `since = as_of - span_days Tage`.
    ///
    /// # Errors
    /// - [`KnowledgeError::StewardWindowInvalid`], wenn `span_days <= 0`.
    /// - [`KnowledgeError::StewardWindowInvalid`], wenn die Subtraktion
    ///   überläuft (nur nahe den Grenzen von `jiff::Timestamp` erreichbar).
    ///
    /// # Examples
    /// ```rust
    /// use harw_knowledge::context_steward::StewardWindow;
    ///
    /// let as_of = jiff::Timestamp::UNIX_EPOCH
    ///     .checked_add(jiff::SignedDuration::from_secs(10 * 24 * 60 * 60))
    ///     .expect("fixture timestamp stays within range");
    /// let window = StewardWindow::trailing(as_of, 7).expect("positive span builds a window");
    /// assert_eq!(window.until, as_of);
    /// assert!(window.since < window.until);
    /// ```
    pub fn trailing(as_of: jiff::Timestamp, span_days: i64) -> KnowledgeResult<Self> {
        if span_days <= 0 {
            return Err(KnowledgeError::StewardWindowInvalid {
                detail: format!("span_days muss positiv sein, war {span_days}"),
            });
        }
        let span_seconds = span_days.saturating_mul(24 * 60 * 60);
        let since = as_of
            .checked_sub(jiff::SignedDuration::from_secs(span_seconds))
            .map_err(|error| KnowledgeError::StewardWindowInvalid {
                detail: format!("as_of - {span_days}d ist übergelaufen: {error}"),
            })?;
        Ok(Self {
            since,
            until: as_of,
        })
    }

    /// Baut das empfohlene Standardfenster (siehe
    /// [`DEFAULT_STEWARD_WINDOW_SPAN_DAYS`]), endend bei `as_of`.
    ///
    /// # Arguments
    /// - `as_of` (`jiff::Timestamp`): Fensterende, hereingereicht.
    ///
    /// # Returns
    /// Siehe [`StewardWindow::trailing`].
    ///
    /// # Errors
    /// Siehe [`StewardWindow::trailing`].
    pub fn default_window(as_of: jiff::Timestamp) -> KnowledgeResult<Self> {
        Self::trailing(as_of, DEFAULT_STEWARD_WINDOW_SPAN_DAYS)
    }

    /// Prüft, ob `timestamp` innerhalb dieses Fensters liegt (einschließlich
    /// beider Grenzen).
    #[must_use]
    pub fn contains(&self, timestamp: jiff::Timestamp) -> bool {
        timestamp >= self.since && timestamp <= self.until
    }
}

/// Ergebnis von [`curate_context_proposals`]: `ContextProposal`s, aufgeteilt
/// nach Entscheidungsstatus und Fensterzugehörigkeit.
#[derive(Debug, Clone, Default)]
pub struct CuratedContextProposals {
    /// Noch offene (`Pending`) Vorschläge innerhalb des Fensters, sortiert
    /// deterministisch nach (`target_program`, `id`).
    pub pending_in_window: Vec<ContextProposal>,
    /// Anzahl noch offener Vorschläge außerhalb des Fensters.
    pub pending_outside_window: usize,
    /// Anzahl bereits entschiedener Vorschläge (`Accepted`/`Rejected`),
    /// unabhängig vom Fenster.
    pub decided: usize,
}

/// Ordnet und fasst einen bereits abgerufenen `ContextProposal`-Bestand
/// zusammen — ordnet, committet nie.
///
/// # Description
/// Reine, totale Funktion: iteriert `proposals` einmal, teilt nach
/// [`ProposalStatus`] und Fensterzugehörigkeit auf, sortiert die
/// zurückgegebenen `pending_in_window`-Einträge deterministisch über einen
/// String-Vergleich auf `target_program` (das keinen `Ord`-Impl trägt —
/// siehe `context_proposal.rs`s eigene Begründung, warum
/// `target_snapshot_digest` schon ein `String` ist) mit `id` als
/// Tie-Breaker, nie über eine `HashMap`-Iterationsreihenfolge. Enthält weder
/// Systemuhr noch Zufallszahl noch Modellaufruf.
///
/// # Arguments
/// - `proposals` (`&[ContextProposal]`): bereits sichtbarkeitsgeprüft vom
///   Aufrufer bezogen (siehe Moduldoku).
/// - `window` (`&StewardWindow`): das Beobachtungsfenster, hereingereicht.
///
/// # Returns
/// Ein [`CuratedContextProposals`].
///
/// # Examples
/// ```rust
/// use harw_agent_dsl::ids::DefinitionId;
/// use harw_knowledge::context_proposal::ContextProposal;
/// use harw_knowledge::context_steward::{StewardWindow, curate_context_proposals};
/// use harw_knowledge::ArtifactId;
///
/// let proposal = ContextProposal::new(
///     ArtifactId::new("context-proposal/example"),
///     "Beispiel",
///     DefinitionId::parse("harwness.context.base@1").expect("valid id"),
///     "deadbeef".repeat(8),
///     Vec::new(),
///     Vec::new(),
///     jiff::Timestamp::UNIX_EPOCH,
///     "human:operator",
/// );
/// let window = StewardWindow::new(jiff::Timestamp::UNIX_EPOCH, jiff::Timestamp::UNIX_EPOCH)
///     .expect("zero-width window at the fixture timestamp");
/// let curated = curate_context_proposals(&[proposal], &window);
/// assert_eq!(curated.pending_in_window.len(), 1);
/// assert_eq!(curated.pending_outside_window, 0);
/// assert_eq!(curated.decided, 0);
/// ```
#[must_use]
pub fn curate_context_proposals(
    proposals: &[ContextProposal],
    window: &StewardWindow,
) -> CuratedContextProposals {
    let mut pending_in_window = Vec::new();
    let mut pending_outside_window = 0usize;
    let mut decided = 0usize;

    for proposal in proposals {
        if proposal.status != ProposalStatus::Pending {
            decided += 1;
            continue;
        }
        if window.contains(proposal.generated_at) {
            pending_in_window.push(proposal.clone());
        } else {
            pending_outside_window += 1;
        }
    }

    pending_in_window.sort_by(|a, b| {
        a.target_program
            .to_string()
            .cmp(&b.target_program.to_string())
            .then_with(|| a.id.cmp(&b.id))
    });

    CuratedContextProposals {
        pending_in_window,
        pending_outside_window,
        decided,
    }
}

/// Ergebnis von [`curate_model_behavior_proposals`]: `ModelBehaviorProposal`s,
/// aufgeteilt nach Entscheidungsstatus und Fensterzugehörigkeit.
#[derive(Debug, Clone, Default)]
pub struct CuratedModelBehaviorProposals {
    /// Noch offene (`Pending`) Vorschläge innerhalb des Fensters, sortiert
    /// deterministisch nach (`target_provider`, `target_model`, `id`).
    pub pending_in_window: Vec<ModelBehaviorProposal>,
    /// Anzahl noch offener Vorschläge außerhalb des Fensters.
    pub pending_outside_window: usize,
    /// Anzahl bereits entschiedener Vorschläge, unabhängig vom Fenster.
    pub decided: usize,
}

/// Ordnet und fasst einen bereits abgerufenen `ModelBehaviorProposal`-Bestand
/// zusammen — ordnet, committet nie. Spiegelbild von
/// [`curate_context_proposals`] für die Modellkatalog-Domäne.
///
/// # Description
/// Reine, totale Funktion; sortiert deterministisch über
/// (`target_provider.as_str()`, `target_model.as_str()`, `id`) — beide Typen
/// tragen einen eigenen `Ord`-Impl (`harw_types::provider_ids`), anders als
/// `DefinitionId` in [`curate_context_proposals`].
///
/// # Arguments
/// - `proposals` (`&[ModelBehaviorProposal]`): bereits sichtbarkeitsgeprüft
///   vom Aufrufer bezogen.
/// - `window` (`&StewardWindow`): das Beobachtungsfenster, hereingereicht.
///
/// # Returns
/// Ein [`CuratedModelBehaviorProposals`].
///
/// # Examples
/// ```rust
/// use harw_knowledge::context_steward::{StewardWindow, curate_model_behavior_proposals};
/// use harw_knowledge::model_behavior_proposal::{ModelBehaviorProposal, ProposedModelChange};
/// use harw_knowledge::ArtifactId;
/// use harw_model_catalog::descriptor::ToolCallingSupport;
/// use harw_types::{ModelId, ProviderId};
///
/// let proposal = ModelBehaviorProposal::new(
///     ArtifactId::new("model-behavior-proposal/example"),
///     "Beispiel",
///     ProviderId::from("openai"),
///     ModelId::from("gpt-5"),
///     "deadbeef".repeat(8),
///     vec![ProposedModelChange::DowngradeToolCalling { to: ToolCallingSupport::Basic }],
///     Vec::new(),
///     jiff::Timestamp::UNIX_EPOCH,
///     "human:operator",
/// );
/// let window = StewardWindow::new(jiff::Timestamp::UNIX_EPOCH, jiff::Timestamp::UNIX_EPOCH)
///     .expect("zero-width window at the fixture timestamp");
/// let curated = curate_model_behavior_proposals(&[proposal], &window);
/// assert_eq!(curated.pending_in_window.len(), 1);
/// ```
#[must_use]
pub fn curate_model_behavior_proposals(
    proposals: &[ModelBehaviorProposal],
    window: &StewardWindow,
) -> CuratedModelBehaviorProposals {
    let mut pending_in_window = Vec::new();
    let mut pending_outside_window = 0usize;
    let mut decided = 0usize;

    for proposal in proposals {
        if proposal.status != ProposalStatus::Pending {
            decided += 1;
            continue;
        }
        if window.contains(proposal.generated_at) {
            pending_in_window.push(proposal.clone());
        } else {
            pending_outside_window += 1;
        }
    }

    pending_in_window.sort_by(|a, b| {
        (
            a.target_provider.as_str(),
            a.target_model.as_str(),
            a.id.as_str(),
        )
            .cmp(&(
                b.target_provider.as_str(),
                b.target_model.as_str(),
                b.id.as_str(),
            ))
    });

    CuratedModelBehaviorProposals {
        pending_in_window,
        pending_outside_window,
        decided,
    }
}

/// Die zusammengeführte Übersicht des Context Stewards über beide
/// Vorschlagsdomänen für ein Fenster.
#[derive(Debug, Clone, Default)]
pub struct StewardDigest {
    /// Kuratierte Kontextprogramm-Vorschläge.
    pub context_proposals: CuratedContextProposals,
    /// Kuratierte Modellverhalten-Vorschläge.
    pub model_behavior_proposals: CuratedModelBehaviorProposals,
}

/// Baut die zusammengeführte Übersicht über beide Vorschlagsdomänen für ein
/// Fenster — ordnet und fasst zusammen, committet nichts.
///
/// # Arguments
/// - `context_proposals` (`&[ContextProposal]`): siehe
///   [`curate_context_proposals`].
/// - `model_behavior_proposals` (`&[ModelBehaviorProposal]`): siehe
///   [`curate_model_behavior_proposals`].
/// - `window` (`&StewardWindow`): dasselbe Fenster für beide Domänen.
///
/// # Returns
/// Ein [`StewardDigest`].
#[must_use]
pub fn build_steward_digest(
    context_proposals: &[ContextProposal],
    model_behavior_proposals: &[ModelBehaviorProposal],
    window: &StewardWindow,
) -> StewardDigest {
    StewardDigest {
        context_proposals: curate_context_proposals(context_proposals, window),
        model_behavior_proposals: curate_model_behavior_proposals(model_behavior_proposals, window),
    }
}

/// Rendert eine menschenlesbare Kurzfassung eines [`StewardDigest`] — reine
/// Textzusammenfassung, keine Aktion.
///
/// # Arguments
/// - `digest` (`&StewardDigest`): die zu rendernde Übersicht.
///
/// # Returns
/// Einen mehrzeiligen Bericht mit den Zählwerten beider Domänen.
#[must_use]
pub fn render_digest_summary(digest: &StewardDigest) -> String {
    format!(
        "Kontextprogramm-Vorschläge: {} im Fenster offen, {} außerhalb des Fensters offen, {} bereits entschieden.\n\
         Modellverhalten-Vorschläge: {} im Fenster offen, {} außerhalb des Fensters offen, {} bereits entschieden.\n",
        digest.context_proposals.pending_in_window.len(),
        digest.context_proposals.pending_outside_window,
        digest.context_proposals.decided,
        digest.model_behavior_proposals.pending_in_window.len(),
        digest.model_behavior_proposals.pending_outside_window,
        digest.model_behavior_proposals.decided,
    )
}

// ---------------------------------------------------------------------------
// Dream-Job-Zeitplan: abhängigkeitsfreier 5-Feld-Cron (siehe Moduldoku).
// ---------------------------------------------------------------------------

/// Empfohlener Cron-Ausdruck für den Dream-/Konsolidierungslauf: montags um
/// 03:00 UTC — abgestimmt auf [`DEFAULT_STEWARD_WINDOW_SPAN_DAYS`] (siehe
/// Moduldoku, "Die Fensterlänge — Entscheidung und Begründung").
pub const DEFAULT_DREAM_CRON_EXPRESSION: &str = "0 3 * * 1";

/// Wie viele Jahre [`CronSchedule::next_after`] höchstens vorausschaut: ein
/// voller gregorianischer Zyklus. Jeder überhaupt erfüllbare Ausdruck feuert
/// innerhalb dieses Horizonts; ein unerfüllbarer (z. B. `0 0 30 2 *`) liefert
/// danach `None`, statt endlos zu suchen.
const CRON_SEARCH_HORIZON_YEARS: i64 = 400;

const SECONDS_PER_MINUTE: i64 = 60;
const MINUTES_PER_DAY: i64 = 24 * 60;
/// Tage vom 0000-03-01 (proleptisch gregorianisch) bis 1970-01-01.
const DAYS_FROM_CIVIL_EPOCH: i64 = 719_468;
const DAYS_PER_400_YEARS: i64 = 146_097;

/// Eines der fünf Felder eines Cron-Ausdrucks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CronField {
    /// Minute, `0..=59`.
    Minute,
    /// Stunde, `0..=23`.
    Hour,
    /// Tag des Monats, `1..=31`.
    DayOfMonth,
    /// Monat, `1..=12`.
    Month,
    /// Wochentag, `0..=7` (`0` und `7` = Sonntag).
    DayOfWeek,
}

impl CronField {
    /// Zulässige Einzelwerte (einschließlich) dieses Felds.
    const fn bounds(self) -> (u32, u32) {
        match self {
            Self::Minute => (0, 59),
            Self::Hour => (0, 23),
            Self::DayOfMonth => (1, 31),
            Self::Month => (1, 12),
            Self::DayOfWeek => (0, 7),
        }
    }

    /// Bereich, für den `*` steht (beim Wochentag ohne die Sonntags-Dublette `7`).
    const fn star_bounds(self) -> (u32, u32) {
        match self {
            Self::DayOfWeek => (0, 6),
            other => other.bounds(),
        }
    }
}

impl std::fmt::Display for CronField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Minute => "minute",
            Self::Hour => "hour",
            Self::DayOfMonth => "day-of-month",
            Self::Month => "month",
            Self::DayOfWeek => "day-of-week",
        };
        f.write_str(name)
    }
}

/// Fehler beim Parsen eines Cron-Ausdrucks ([`CronSchedule::parse`]).
///
/// Modullokal statt einer neuen `KnowledgeError`-Variante, weil ein
/// ungültiger Ausdruck ein reiner Konfigurationsfehler des Aufrufers ist und
/// feldgenau geprüft werden können soll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CronParseError {
    /// Der Ausdruck hat nicht genau fünf durch Leerraum getrennte Felder.
    WrongFieldCount {
        /// Gefundene Feldanzahl.
        found: usize,
    },
    /// Ein Listenelement ist syntaktisch ungültig (leer, keine Zahl, Name, …).
    InvalidToken {
        /// Betroffenes Feld.
        field: CronField,
        /// Das ungültige Listenelement.
        token: String,
    },
    /// Ein Wert liegt außerhalb der Feldgrenzen.
    OutOfRange {
        /// Betroffenes Feld.
        field: CronField,
        /// Der gelesene Wert.
        value: u32,
        /// Kleinster zulässiger Wert.
        min: u32,
        /// Größter zulässiger Wert.
        max: u32,
    },
    /// Ein Bereich `a-b` mit `a > b` (kein Umlauf wie `22-2`).
    ReversedRange {
        /// Betroffenes Feld.
        field: CronField,
        /// Bereichsanfang.
        start: u32,
        /// Bereichsende.
        end: u32,
    },
    /// Eine Schrittweite `/0`.
    ZeroStep {
        /// Betroffenes Feld.
        field: CronField,
    },
}

impl std::fmt::Display for CronParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongFieldCount { found } => {
                write!(f, "cron expression needs exactly 5 fields, found {found}")
            }
            Self::InvalidToken { field, token } => {
                write!(f, "invalid cron {field} token {token:?}")
            }
            Self::OutOfRange {
                field,
                value,
                min,
                max,
            } => write!(f, "cron {field} value {value} outside {min}..={max}"),
            Self::ReversedRange { field, start, end } => {
                write!(f, "cron {field} range {start}-{end} is reversed")
            }
            Self::ZeroStep { field } => write!(f, "cron {field} step must be positive"),
        }
    }
}

impl std::error::Error for CronParseError {}

/// Ein geparster 5-Feld-Cron-Ausdruck, ausgewertet in UTC (siehe Moduldoku,
/// "Der Dream-Job-Zeitplan").
///
/// # Examples
/// ```rust
/// use harw_knowledge::context_steward::CronSchedule;
///
/// let schedule = CronSchedule::parse("30 4 * * *").expect("valid expression");
/// let after: jiff::Timestamp = "2026-01-01T05:00:00Z".parse().expect("valid timestamp");
/// let next = schedule.next_after(after).expect("fires daily");
/// assert_eq!(next.to_string(), "2026-01-02T04:30:00Z");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronSchedule {
    /// Normalisierter Quelltext (Felder durch genau ein Leerzeichen getrennt).
    expression: String,
    /// Bit `n` gesetzt ⇔ Minute `n` erlaubt.
    minutes: u64,
    /// Bit `n` gesetzt ⇔ Stunde `n` erlaubt.
    hours: u64,
    /// Bit `n` gesetzt ⇔ Monatstag `n` erlaubt.
    days_of_month: u64,
    /// Bit `n` gesetzt ⇔ Monat `n` erlaubt.
    months: u64,
    /// Bit `n` gesetzt ⇔ Wochentag `n` erlaubt (`0` = Sonntag; `7` ist auf `0` gefaltet).
    days_of_week: u64,
    /// Monatstag-Feld beginnt nicht mit `*`.
    day_of_month_restricted: bool,
    /// Wochentag-Feld beginnt nicht mit `*`.
    day_of_week_restricted: bool,
}

impl CronSchedule {
    /// Parst einen 5-Feld-Cron-Ausdruck.
    ///
    /// # Arguments
    /// - `expression` (`&str`): `Minute Stunde Monatstag Monat Wochentag`,
    ///   Felder durch beliebigen Leerraum getrennt.
    ///
    /// # Returns
    /// Ein [`CronSchedule`].
    ///
    /// # Errors
    /// [`CronParseError`], wenn der Ausdruck nicht genau fünf Felder hat oder
    /// ein Feld ungültig ist (siehe die einzelnen Varianten).
    pub fn parse(expression: &str) -> Result<Self, CronParseError> {
        let fields: Vec<&str> = expression.split_whitespace().collect();
        let [minute, hour, day_of_month, month, day_of_week] = fields.as_slice() else {
            return Err(CronParseError::WrongFieldCount {
                found: fields.len(),
            });
        };
        Ok(Self {
            expression: fields.join(" "),
            minutes: parse_cron_field(CronField::Minute, minute)?,
            hours: parse_cron_field(CronField::Hour, hour)?,
            days_of_month: parse_cron_field(CronField::DayOfMonth, day_of_month)?,
            months: parse_cron_field(CronField::Month, month)?,
            days_of_week: parse_cron_field(CronField::DayOfWeek, day_of_week)?,
            day_of_month_restricted: !day_of_month.starts_with('*'),
            day_of_week_restricted: !day_of_week.starts_with('*'),
        })
    }

    /// Der normalisierte Ausdruck (Felder durch genau ein Leerzeichen getrennt).
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.expression
    }

    /// Prüft, ob der Ausdruck zur UTC-Minute von `timestamp` feuert
    /// (Sekunden und Sekundenbruchteile werden ignoriert).
    #[must_use]
    pub fn matches(&self, timestamp: jiff::Timestamp) -> bool {
        let minute_index = floor_minutes(timestamp);
        let days = minute_index.div_euclid(MINUTES_PER_DAY);
        let minute_of_day = minute_index.rem_euclid(MINUTES_PER_DAY);
        let (year, month, day) = civil_from_days(days);
        let hour = u32::try_from(minute_of_day / 60).unwrap_or(u32::MAX);
        let minute = u32::try_from(minute_of_day % 60).unwrap_or(u32::MAX);
        has_bit(self.months, month)
            && self.day_matches(year, month, day)
            && has_bit(self.hours, hour)
            && has_bit(self.minutes, minute)
    }

    /// Berechnet den nächsten Feuerzeitpunkt **echt nach** `after`.
    ///
    /// # Description
    /// Rein und deterministisch, keine Systemuhr. Die Suche springt
    /// monats-, tages-, stunden- und minutenweise vorwärts und bricht nach
    /// einem vollen gregorianischen Zyklus (400 Jahre) ab.
    ///
    /// # Arguments
    /// - `after` (`jiff::Timestamp`): Bezugszeitpunkt, hereingereicht.
    ///
    /// # Returns
    /// `Some(t)` mit `t > after`, `t` auf eine volle UTC-Minute gerundet;
    /// `None`, wenn der Ausdruck nie feuert (z. B. `0 0 30 2 *`) oder der
    /// Zeitpunkt jenseits von `jiff::Timestamp::MAX` läge.
    #[must_use]
    pub fn next_after(&self, after: jiff::Timestamp) -> Option<jiff::Timestamp> {
        let start = floor_minutes(after).checked_add(1)?;
        let days = start.div_euclid(MINUTES_PER_DAY);
        let minute_of_day = start.rem_euclid(MINUTES_PER_DAY);
        let (mut year, mut month, mut day) = civil_from_days(days);
        let mut hour = u32::try_from(minute_of_day / 60).ok()?;
        let mut minute = u32::try_from(minute_of_day % 60).ok()?;
        let horizon = year.checked_add(CRON_SEARCH_HORIZON_YEARS)?;

        loop {
            if year > horizon {
                return None;
            }
            if !has_bit(self.months, month) {
                (year, month) = next_month(year, month);
                (day, hour, minute) = (1, 0, 0);
                continue;
            }
            if !self.day_matches(year, month, day) {
                (year, month, day) = next_day(year, month, day);
                (hour, minute) = (0, 0);
                continue;
            }
            let Some(next_hour) = next_bit_at_or_after(self.hours, hour, 23) else {
                // Heute keine passende Stunde mehr: nächster Tag.
                (year, month, day) = next_day(year, month, day);
                (hour, minute) = (0, 0);
                continue;
            };
            if next_hour != hour {
                (hour, minute) = (next_hour, 0);
            }
            let Some(next_minute) = next_bit_at_or_after(self.minutes, minute, 59) else {
                // In dieser Stunde keine passende Minute mehr: nächste Stunde.
                if hour >= 23 {
                    (year, month, day) = next_day(year, month, day);
                    hour = 0;
                } else {
                    hour += 1;
                }
                minute = 0;
                continue;
            };
            let seconds = days_from_civil(year, month, day)
                .checked_mul(MINUTES_PER_DAY)?
                .checked_add(i64::from(hour) * 60 + i64::from(next_minute))?
                .checked_mul(SECONDS_PER_MINUTE)?;
            return jiff::Timestamp::from_second(seconds).ok();
        }
    }

    /// Monatstag/Wochentag nach Vixie-Cron-Semantik (siehe Moduldoku).
    fn day_matches(&self, year: i64, month: u32, day: u32) -> bool {
        let day_of_month_ok = has_bit(self.days_of_month, day);
        let day_of_week_ok = has_bit(
            self.days_of_week,
            weekday_sunday_zero(days_from_civil(year, month, day)),
        );
        if self.day_of_month_restricted && self.day_of_week_restricted {
            day_of_month_ok || day_of_week_ok
        } else {
            day_of_month_ok && day_of_week_ok
        }
    }
}

impl std::str::FromStr for CronSchedule {
    type Err = CronParseError;

    fn from_str(expression: &str) -> Result<Self, Self::Err> {
        Self::parse(expression)
    }
}

impl std::fmt::Display for CronSchedule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.expression)
    }
}

/// Parst ein einzelnes Cron-Feld zu einer Bitmaske.
fn parse_cron_field(field: CronField, text: &str) -> Result<u64, CronParseError> {
    let (_, max) = field.bounds();
    let mut mask = 0u64;
    for item in text.split(',') {
        let invalid = || CronParseError::InvalidToken {
            field,
            token: item.to_owned(),
        };
        let (base, step) = match item.split_once('/') {
            Some((base, step)) => (base, Some(parse_cron_number(step).ok_or_else(invalid)?)),
            None => (item, None),
        };
        if step == Some(0) {
            return Err(CronParseError::ZeroStep { field });
        }
        let (start, end) = if base == "*" {
            field.star_bounds()
        } else if let Some((start, end)) = base.split_once('-') {
            let start = parse_cron_value(field, start).ok_or_else(invalid)??;
            let end = parse_cron_value(field, end).ok_or_else(invalid)??;
            if start > end {
                return Err(CronParseError::ReversedRange { field, start, end });
            }
            (start, end)
        } else {
            let value = parse_cron_value(field, base).ok_or_else(invalid)??;
            // `a/n` ist die Kurzform für `a-<max>/n`.
            if step.is_some() {
                (value, max)
            } else {
                (value, value)
            }
        };
        let step = step.unwrap_or(1);
        let mut value = start;
        while value <= end {
            mask |= 1u64 << value;
            value = match value.checked_add(step) {
                Some(next) => next,
                None => break,
            };
        }
    }
    if field == CronField::DayOfWeek && has_bit(mask, 7) {
        mask = (mask & !(1u64 << 7)) | 1;
    }
    Ok(mask)
}

/// Parst eine nicht-negative Dezimalzahl (nur ASCII-Ziffern, kein Vorzeichen).
fn parse_cron_number(token: &str) -> Option<u32> {
    if token.is_empty() || !token.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    token.parse().ok()
}

/// Parst einen Feldwert: `None` bei Syntaxfehler, `Some(Err)` außerhalb der Grenzen.
fn parse_cron_value(field: CronField, token: &str) -> Option<Result<u32, CronParseError>> {
    let value = parse_cron_number(token)?;
    let (min, max) = field.bounds();
    if value < min || value > max {
        return Some(Err(CronParseError::OutOfRange {
            field,
            value,
            min,
            max,
        }));
    }
    Some(Ok(value))
}

/// Ist Bit `index` in `mask` gesetzt? (`index >= 64` ⇒ `false`.)
fn has_bit(mask: u64, index: u32) -> bool {
    index < 64 && (mask >> index) & 1 == 1
}

/// Kleinstes gesetztes Bit in `from..=last`.
fn next_bit_at_or_after(mask: u64, from: u32, last: u32) -> Option<u32> {
    (from..=last).find(|&index| has_bit(mask, index))
}

/// Ganze UTC-Minuten seit der Unix-Epoche, abgerundet (auch vor 1970).
fn floor_minutes(timestamp: jiff::Timestamp) -> i64 {
    let mut seconds = timestamp.as_second();
    if timestamp.subsec_nanosecond() < 0 {
        // `as_second` schneidet Richtung null ab; für negative Zeitpunkte abrunden.
        seconds = seconds.saturating_sub(1);
    }
    seconds.div_euclid(SECONDS_PER_MINUTE)
}

/// Folgemonat (mit Jahreswechsel).
fn next_month(year: i64, month: u32) -> (i64, u32) {
    if month >= 12 {
        (year.saturating_add(1), 1)
    } else {
        (year, month + 1)
    }
}

/// Folgetag (mit Monats- und Jahreswechsel).
fn next_day(year: i64, month: u32, day: u32) -> (i64, u32, u32) {
    if day >= days_in_month(year, month) {
        let (year, month) = next_month(year, month);
        (year, month, 1)
    } else {
        (year, month, day + 1)
    }
}

/// Schaltjahr nach gregorianischer Regel.
fn is_leap_year(year: i64) -> bool {
    year.rem_euclid(4) == 0 && (year.rem_euclid(100) != 0 || year.rem_euclid(400) == 0)
}

/// Tage im Monat.
fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        2 if is_leap_year(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Tage seit 1970-01-01 für ein proleptisch-gregorianisches Datum
/// (Algorithmus `days_from_civil` nach Howard Hinnant).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let month = i64::from(month);
    let shifted_month = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * DAYS_PER_400_YEARS + day_of_era - DAYS_FROM_CIVIL_EPOCH
}

/// Umkehrung von [`days_from_civil`] (`civil_from_days` nach Howard Hinnant).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + DAYS_FROM_CIVIL_EPOCH;
    let era = shifted.div_euclid(DAYS_PER_400_YEARS);
    let day_of_era = shifted.rem_euclid(DAYS_PER_400_YEARS);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (
        year,
        u32::try_from(month).unwrap_or(1),
        u32::try_from(day).unwrap_or(1),
    )
}

/// Wochentag mit `0` = Sonntag (1970-01-01 war ein Donnerstag).
fn weekday_sunday_zero(days: i64) -> u32 {
    u32::try_from((days + 4).rem_euclid(7)).unwrap_or(0)
}

/// Entscheidung von [`DreamSchedule::decide`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DreamDecision {
    /// Der Lauf ist fällig.
    Due {
        /// Der früheste versäumte Feuerzeitpunkt nach dem letzten Lauf;
        /// `None`, wenn der Job noch nie gelaufen ist. Mehrere versäumte
        /// Zeitpunkte werden zu diesem einen Lauf zusammengefasst.
        scheduled_for: Option<jiff::Timestamp>,
    },
    /// Noch nicht fällig.
    NotDue {
        /// Der nächste Feuerzeitpunkt nach dem letzten Lauf (echt nach `now`,
        /// sofern `last_run <= now`), ab dem der Lauf fällig wird.
        next_fire: jiff::Timestamp,
    },
    /// Der Ausdruck feuert nach dem letzten Lauf nie wieder (unerfüllbar oder
    /// jenseits des Suchhorizonts).
    Never,
}

impl DreamDecision {
    /// `true` genau für [`DreamDecision::Due`].
    #[must_use]
    pub fn is_due(&self) -> bool {
        matches!(self, Self::Due { .. })
    }
}

/// Zeitplan eines Dream-/Konsolidierungslaufs über einem [`CronSchedule`] —
/// entscheidet nur, **ob** ein Lauf fällig ist; startet selbst nichts und
/// liest nie die Systemuhr (siehe Moduldoku, "Der Dream-Job-Zeitplan").
///
/// # Examples
/// ```rust
/// use harw_knowledge::context_steward::{DreamDecision, DreamSchedule};
///
/// let schedule = DreamSchedule::recommended().expect("default expression parses");
/// // 2026-01-05 ist ein Montag.
/// let last_run: jiff::Timestamp = "2026-01-05T03:00:00Z".parse().expect("valid");
/// let now: jiff::Timestamp = "2026-01-08T12:00:00Z".parse().expect("valid");
/// assert!(!schedule.decide(Some(last_run), now).is_due());
/// let later: jiff::Timestamp = "2026-01-12T03:00:00Z".parse().expect("valid");
/// assert!(matches!(schedule.decide(Some(last_run), later), DreamDecision::Due { .. }));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DreamSchedule {
    cron: CronSchedule,
}

impl DreamSchedule {
    /// Baut einen Zeitplan aus einem bereits geparsten Ausdruck.
    #[must_use]
    pub fn new(cron: CronSchedule) -> Self {
        Self { cron }
    }

    /// Parst `expression` und baut daraus einen Zeitplan.
    ///
    /// # Errors
    /// [`CronParseError`], siehe [`CronSchedule::parse`].
    pub fn parse(expression: &str) -> Result<Self, CronParseError> {
        CronSchedule::parse(expression).map(Self::new)
    }

    /// Der empfohlene Zeitplan ([`DEFAULT_DREAM_CRON_EXPRESSION`]).
    ///
    /// # Errors
    /// Nur theoretisch ([`CronParseError`]); der Ausdruck ist konstant und
    /// durch Tests abgedeckt.
    pub fn recommended() -> Result<Self, CronParseError> {
        Self::parse(DEFAULT_DREAM_CRON_EXPRESSION)
    }

    /// Der zugrundeliegende Cron-Ausdruck.
    #[must_use]
    pub fn cron(&self) -> &CronSchedule {
        &self.cron
    }

    /// Nächster planmäßiger Lauf nach `last_run` (siehe
    /// [`CronSchedule::next_after`]).
    #[must_use]
    pub fn next_run_after(&self, last_run: jiff::Timestamp) -> Option<jiff::Timestamp> {
        self.cron.next_after(last_run)
    }

    /// Entscheidet, ob ein Lauf zum Zeitpunkt `now` fällig ist.
    ///
    /// # Description
    /// - `last_run == None` (nie gelaufen): sofort fällig.
    /// - sonst: fällig, sobald der erste Feuerzeitpunkt echt nach `last_run`
    ///   erreicht ist (`<= now`); beliebig viele versäumte Zeitpunkte ergeben
    ///   genau **einen** fälligen Lauf. Liegt `last_run` nach `now`
    ///   (Uhrensprung beim Aufrufer), ist der Lauf nicht fällig.
    ///
    /// # Arguments
    /// - `last_run` (`Option<jiff::Timestamp>`): Beginn des letzten Laufs,
    ///   vom Aufrufer dauerhaft festgehalten.
    /// - `now` (`jiff::Timestamp`): Entscheidungszeitpunkt, hereingereicht.
    ///
    /// # Returns
    /// Eine [`DreamDecision`].
    #[must_use]
    pub fn decide(&self, last_run: Option<jiff::Timestamp>, now: jiff::Timestamp) -> DreamDecision {
        let Some(last_run) = last_run else {
            return DreamDecision::Due {
                scheduled_for: None,
            };
        };
        match self.cron.next_after(last_run) {
            Some(fire) if fire <= now => DreamDecision::Due {
                scheduled_for: Some(fire),
            },
            Some(next_fire) => DreamDecision::NotDue { next_fire },
            None => DreamDecision::Never,
        }
    }
}

/// Baut den [`StewardDigest`] für einen Dream-Lauf, **falls** er laut
/// `schedule` fällig ist — Verbindung von Zeitplan und Steward.
///
/// # Description
/// Reine Funktion ohne Systemuhr: bei [`DreamDecision::Due`] wird
/// [`build_steward_digest`] über [`StewardWindow::default_window`]`(now)`
/// aufgerufen, sonst nichts. Der Aufrufer hält danach `now` als neuen
/// `last_run` fest.
///
/// # Arguments
/// - `schedule` (`&DreamSchedule`): der Lauf-Zeitplan.
/// - `last_run` (`Option<jiff::Timestamp>`): Beginn des letzten Laufs.
/// - `now` (`jiff::Timestamp`): Entscheidungs- und Fensterendzeitpunkt.
/// - `context_proposals`/`model_behavior_proposals`: siehe
///   [`build_steward_digest`].
///
/// # Returns
/// `Some(digest)`, wenn der Lauf fällig war; `None` sonst.
///
/// # Errors
/// [`KnowledgeError::StewardWindowInvalid`], wenn das Standardfenster an
/// `now` nicht gebildet werden kann (siehe [`StewardWindow::trailing`]).
pub fn steward_digest_if_due(
    schedule: &DreamSchedule,
    last_run: Option<jiff::Timestamp>,
    now: jiff::Timestamp,
    context_proposals: &[ContextProposal],
    model_behavior_proposals: &[ModelBehaviorProposal],
) -> KnowledgeResult<Option<StewardDigest>> {
    if !schedule.decide(last_run, now).is_due() {
        return Ok(None);
    }
    let window = StewardWindow::default_window(now)?;
    Ok(Some(build_steward_digest(
        context_proposals,
        model_behavior_proposals,
        &window,
    )))
}

#[cfg(test)]
mod tests {
    use super::{
        CuratedContextProposals, StewardWindow, build_steward_digest, curate_context_proposals,
        curate_model_behavior_proposals, render_digest_summary, steward_null_counter_registry,
    };
    use crate::artifact::ArtifactId;
    use crate::context_proposal::{ContextProposal, ProposalStatus};
    use crate::error::KnowledgeError;
    use crate::model_behavior_proposal::{ModelBehaviorProposal, ProposedModelChange};
    use crate::test_support::{TestError, TestResult};

    use harw_agent_dsl::ids::DefinitionId;
    use harw_model_catalog::descriptor::ToolCallingSupport;
    use harw_types::{ModelId, ProviderId};

    fn program_id() -> TestResult<DefinitionId> {
        DefinitionId::parse("harwness.context.security-triage@1")
            .map_err(crate::test_support::ctx("valid definition id"))
    }

    fn context_proposal_at(
        slug: &str,
        generated_at: jiff::Timestamp,
    ) -> TestResult<ContextProposal> {
        Ok(ContextProposal::new(
            ArtifactId::new(format!("context-proposal/{slug}")),
            "Testvorschlag",
            program_id()?,
            "deadbeef".repeat(8),
            Vec::new(),
            Vec::new(),
            generated_at,
            "heuristic:must-include-promotion@1",
        ))
    }

    fn model_behavior_proposal_at(
        slug: &str,
        generated_at: jiff::Timestamp,
    ) -> ModelBehaviorProposal {
        ModelBehaviorProposal::new(
            ArtifactId::new(format!("model-behavior-proposal/{slug}")),
            "Testvorschlag",
            ProviderId::from("openai"),
            ModelId::from("gpt-5"),
            "deadbeef".repeat(8),
            vec![ProposedModelChange::DowngradeToolCalling {
                to: ToolCallingSupport::Basic,
            }],
            Vec::new(),
            generated_at,
            "heuristic:tool-calling-downgrade@1",
        )
    }

    fn epoch() -> jiff::Timestamp {
        jiff::Timestamp::UNIX_EPOCH
    }

    fn far_future() -> TestResult<jiff::Timestamp> {
        epoch()
            .checked_add(jiff::SignedDuration::from_secs(365 * 24 * 60 * 60))
            .map_err(crate::test_support::ctx(
                "fixture timestamp stays within range",
            ))
    }

    /// `StewardWindow::new` rejects `since` after `until`.
    #[test]
    fn test_window_new_rejects_since_after_until() -> TestResult {
        let Err(error) = StewardWindow::new(far_future()?, epoch()) else {
            return Err(TestError::Unexpected(
                "since after until must be rejected".to_owned(),
            ));
        };
        assert!(matches!(error, KnowledgeError::StewardWindowInvalid { .. }));
        Ok(())
    }

    /// `StewardWindow::trailing` builds `until = as_of` and `since` earlier,
    /// with `contains` behaving as documented.
    #[test]
    fn test_window_trailing_builds_since_before_until_and_contains_works() -> TestResult {
        let window = StewardWindow::trailing(far_future()?, 7)
            .map_err(crate::test_support::ctx("positive span builds a window"))?;
        assert_eq!(window.until, far_future()?);
        assert!(window.since < window.until);
        assert!(window.contains(far_future()?));
        assert!(
            !window.contains(epoch()),
            "epoch is well outside a 7-day trailing window ending a year later"
        );
        Ok(())
    }

    /// `StewardWindow::trailing` rejects a non-positive span.
    #[test]
    fn test_window_trailing_rejects_non_positive_span() -> TestResult {
        let Err(error) = StewardWindow::trailing(epoch(), 0) else {
            return Err(TestError::Unexpected(
                "zero span must be rejected".to_owned(),
            ));
        };
        assert!(matches!(error, KnowledgeError::StewardWindowInvalid { .. }));
        Ok(())
    }

    /// `StewardWindow::default_window` uses `DEFAULT_STEWARD_WINDOW_SPAN_DAYS`.
    #[test]
    fn test_default_window_uses_the_documented_default_span() -> TestResult {
        let window = StewardWindow::default_window(far_future()?)
            .map_err(crate::test_support::ctx("default span builds a window"))?;
        let explicit =
            StewardWindow::trailing(far_future()?, super::DEFAULT_STEWARD_WINDOW_SPAN_DAYS)
                .map_err(crate::test_support::ctx("explicit span builds a window"))?;
        assert_eq!(window, explicit);
        Ok(())
    }

    /// `curate_context_proposals` splits pending-in-window, pending-outside,
    /// and decided correctly.
    #[test]
    fn test_curate_context_proposals_splits_by_status_and_window() -> TestResult {
        let window = StewardWindow::new(epoch(), far_future()?)
            .map_err(crate::test_support::ctx("valid window"))?;

        let in_window = context_proposal_at("in-window", epoch())?;
        let mut decided = context_proposal_at("decided", epoch())?;
        decided
            .accept()
            .map_err(crate::test_support::ctx("pending proposal accepts"))?;
        let outside_window = context_proposal_at(
            "outside-window",
            far_future()?
                .checked_add(jiff::SignedDuration::from_secs(1))
                .map_err(crate::test_support::ctx(
                    "fixture timestamp stays within range",
                ))?,
        )?;

        let curated = curate_context_proposals(&[in_window, decided, outside_window], &window);

        assert_eq!(curated.pending_in_window.len(), 1);
        assert_eq!(
            curated.pending_in_window[0].id,
            ArtifactId::new("context-proposal/in-window")
        );
        assert_eq!(curated.pending_outside_window, 1);
        assert_eq!(curated.decided, 1);
        Ok(())
    }

    /// Determinism: same input, called twice, byte-identical (via `Debug`,
    /// since `CuratedContextProposals` carries no `Serialize`).
    #[test]
    fn test_curate_context_proposals_is_deterministic() -> TestResult {
        let window = StewardWindow::new(epoch(), far_future()?)
            .map_err(crate::test_support::ctx("valid window"))?;
        let proposals = vec![
            context_proposal_at("zzz", epoch())?,
            context_proposal_at("aaa", epoch())?,
        ];

        let first = format!("{:?}", curate_context_proposals(&proposals, &window));
        let second = format!("{:?}", curate_context_proposals(&proposals, &window));
        assert_eq!(first, second);
        Ok(())
    }

    /// Multiple qualifying proposals come back ordered by
    /// (`target_program`, `id`) — here identical `target_program`, so `id`
    /// alone decides.
    #[test]
    fn test_curate_context_proposals_orders_by_id_within_the_same_program() -> TestResult {
        let window = StewardWindow::new(epoch(), far_future()?)
            .map_err(crate::test_support::ctx("valid window"))?;
        let proposals = vec![
            context_proposal_at("zzz", epoch())?,
            context_proposal_at("aaa", epoch())?,
        ];

        let curated = curate_context_proposals(&proposals, &window);
        let ids: Vec<String> = curated
            .pending_in_window
            .iter()
            .map(|proposal| proposal.id.to_string())
            .collect();
        assert_eq!(
            ids,
            vec![
                "context-proposal/aaa".to_owned(),
                "context-proposal/zzz".to_owned(),
            ]
        );
        Ok(())
    }

    /// `curate_model_behavior_proposals` splits by status/window and orders
    /// deterministically, mirroring the `ContextProposal` tests above.
    #[test]
    fn test_curate_model_behavior_proposals_splits_and_orders() -> TestResult {
        let window = StewardWindow::new(epoch(), far_future()?)
            .map_err(crate::test_support::ctx("valid window"))?;

        let mut decided = model_behavior_proposal_at("decided", epoch());
        decided
            .reject()
            .map_err(crate::test_support::ctx("pending proposal rejects"))?;
        let proposals = vec![
            model_behavior_proposal_at("zzz", epoch()),
            model_behavior_proposal_at("aaa", epoch()),
            decided,
        ];

        let curated = curate_model_behavior_proposals(&proposals, &window);
        assert_eq!(curated.decided, 1);
        let ids: Vec<String> = curated
            .pending_in_window
            .iter()
            .map(|proposal| proposal.id.to_string())
            .collect();
        assert_eq!(
            ids,
            vec![
                "model-behavior-proposal/aaa".to_owned(),
                "model-behavior-proposal/zzz".to_owned(),
            ]
        );
        Ok(())
    }

    /// `build_steward_digest` combines both domains under one window.
    #[test]
    fn test_build_steward_digest_combines_both_domains() -> TestResult {
        let window = StewardWindow::new(epoch(), far_future()?)
            .map_err(crate::test_support::ctx("valid window"))?;
        let digest = build_steward_digest(
            &[context_proposal_at("example", epoch())?],
            &[model_behavior_proposal_at("example", epoch())],
            &window,
        );
        assert_eq!(digest.context_proposals.pending_in_window.len(), 1);
        assert_eq!(digest.model_behavior_proposals.pending_in_window.len(), 1);
        Ok(())
    }

    /// `render_digest_summary` reports the counts from both domains.
    #[test]
    fn test_render_digest_summary_reports_both_domain_counts() -> TestResult {
        let window = StewardWindow::new(epoch(), far_future()?)
            .map_err(crate::test_support::ctx("valid window"))?;
        let digest =
            build_steward_digest(&[context_proposal_at("example", epoch())?], &[], &window);
        let summary = render_digest_summary(&digest);
        assert!(summary.contains("1 im Fenster offen"));
        Ok(())
    }

    /// `steward_null_counter_registry` registers exactly the three
    /// documented counters, by name — independent of their current count,
    /// so this test stays stable regardless of what other tests in this
    /// crate's test binary have already driven over zero.
    #[test]
    fn test_steward_null_counter_registry_lists_exactly_three_counters_by_name() {
        let registry = steward_null_counter_registry();
        let names: Vec<&str> = registry
            .all()
            .iter()
            .map(|counter| counter.name())
            .collect();
        assert_eq!(
            names,
            vec![
                "steward_ceiling_violation_total",
                "steward_leak_violation_total",
                "steward_redecision_violation_total",
            ]
        );
    }

    /// Es gibt keinen Weg, aus diesem Modul heraus ein Kontextprogramm oder
    /// einen Modellkatalog-Eintrag zu ändern: außer `new`/`accept`/`reject`
    /// auf den importierten Vorschlagstypen (getestet in deren eigenen
    /// Modulen) exponiert dieses Modul nur `StewardWindow`-Konstruktoren,
    /// die beiden `curate_*`-Funktionen, `build_steward_digest`,
    /// `render_digest_summary` und den reinen Zeitplan (`CronSchedule`,
    /// `DreamSchedule`, `steward_digest_if_due`) — alle nehmen Slices/Referenzen entgegen und
    /// geben neue Werte zurück, keine schreibt irgendwohin. Dieser Test
    /// belegt die eine Hälfte davon strukturell: dieselben Eingabe-Slices
    /// bleiben nach dem Aufruf unverändert (kein `&mut`-Parameter existiert
    /// in der Signatur, also ist das ohnehin durch den Typ erzwungen — dieser
    /// Test macht es trotzdem explizit nachvollziehbar).
    #[test]
    fn test_curate_functions_do_not_mutate_their_inputs() -> TestResult {
        let window = StewardWindow::new(epoch(), far_future()?)
            .map_err(crate::test_support::ctx("valid window"))?;
        let proposals = vec![context_proposal_at("example", epoch())?];
        let before = proposals.clone();

        let _curated: CuratedContextProposals = curate_context_proposals(&proposals, &window);

        assert_eq!(proposals.len(), before.len());
        assert_eq!(proposals[0].status, ProposalStatus::Pending);
        assert_eq!(proposals[0].id, before[0].id);
        Ok(())
    }
}

#[cfg(test)]
mod schedule_tests {
    use super::{
        CronField, CronParseError, CronSchedule, DEFAULT_DREAM_CRON_EXPRESSION, DreamDecision,
        DreamSchedule, civil_from_days, days_from_civil, steward_digest_if_due,
        weekday_sunday_zero,
    };
    use crate::artifact::ArtifactId;
    use crate::context_proposal::ContextProposal;
    use crate::test_support::{TestError, TestResult};

    use harw_agent_dsl::ids::DefinitionId;

    fn ts(text: &str) -> TestResult<jiff::Timestamp> {
        Ok(text.parse::<jiff::Timestamp>()?)
    }

    fn cron(expression: &str) -> TestResult<CronSchedule> {
        CronSchedule::parse(expression).map_err(crate::test_support::ctx("valid cron expression"))
    }

    /// Asserts `next_after(after) == expected` (both RFC 3339 strings).
    fn assert_next(expression: &str, after: &str, expected: &str) -> TestResult {
        let next = cron(expression)?
            .next_after(ts(after)?)
            .ok_or(TestError::Missing("expression fires"))?;
        assert_eq!(
            next,
            ts(expected)?,
            "{expression:?} after {after} should fire at {expected}, got {next}"
        );
        Ok(())
    }

    fn parse_err(expression: &str) -> TestResult<CronParseError> {
        match CronSchedule::parse(expression) {
            Ok(schedule) => Err(TestError::Unexpected(format!(
                "{expression:?} must be rejected, parsed as {schedule}"
            ))),
            Err(error) => Ok(error),
        }
    }

    // --- Parsing ---------------------------------------------------------

    #[test]
    fn test_parse_normalizes_whitespace_and_round_trips_via_display_and_from_str() -> TestResult {
        let schedule = cron("  0\t3  *   * 1 ")?;
        assert_eq!(schedule.as_str(), "0 3 * * 1");
        assert_eq!(schedule.to_string(), "0 3 * * 1");
        let reparsed: CronSchedule = schedule
            .as_str()
            .parse()
            .map_err(crate::test_support::ctx("normalized form reparses"))?;
        assert_eq!(reparsed, schedule);
        Ok(())
    }

    #[test]
    fn test_parse_rejects_wrong_field_count() -> TestResult {
        assert_eq!(
            parse_err("* * * *")?,
            CronParseError::WrongFieldCount { found: 4 }
        );
        assert_eq!(
            parse_err("0 * * * * *")?,
            CronParseError::WrongFieldCount { found: 6 }
        );
        assert_eq!(parse_err("")?, CronParseError::WrongFieldCount { found: 0 });
        Ok(())
    }

    #[test]
    fn test_parse_rejects_out_of_range_values_per_field() -> TestResult {
        let cases = [
            ("60 * * * *", CronField::Minute, 60, 0, 59),
            ("* 24 * * *", CronField::Hour, 24, 0, 23),
            ("* * 0 * *", CronField::DayOfMonth, 0, 1, 31),
            ("* * 32 * *", CronField::DayOfMonth, 32, 1, 31),
            ("* * * 0 *", CronField::Month, 0, 1, 12),
            ("* * * 13 *", CronField::Month, 13, 1, 12),
            ("* * * * 8", CronField::DayOfWeek, 8, 0, 7),
            ("0-60 * * * *", CronField::Minute, 60, 0, 59),
        ];
        for (expression, field, value, min, max) in cases {
            assert_eq!(
                parse_err(expression)?,
                CronParseError::OutOfRange {
                    field,
                    value,
                    min,
                    max
                },
                "{expression}"
            );
        }
        Ok(())
    }

    #[test]
    fn test_parse_rejects_reversed_range_and_zero_step() -> TestResult {
        assert_eq!(
            parse_err("* 22-2 * * *")?,
            CronParseError::ReversedRange {
                field: CronField::Hour,
                start: 22,
                end: 2
            }
        );
        assert_eq!(
            parse_err("*/0 * * * *")?,
            CronParseError::ZeroStep {
                field: CronField::Minute
            }
        );
        Ok(())
    }

    #[test]
    fn test_parse_rejects_malformed_tokens() -> TestResult {
        for expression in [
            "a * * * *",
            "1,,2 * * * *",
            ", * * * *",
            "-1 * * * *",
            "+1 * * * *",
            "1- * * * *",
            "*/ * * * *",
            "*/x * * * *",
            "1-2-3 * * * *",
            "**/2 * * * *",
            "* * * * MON",
            "* * * JAN *",
            "* * ? * *",
            "* * L * *",
            "99999999999 * * * *",
        ] {
            let error = parse_err(expression)?;
            assert!(
                matches!(error, CronParseError::InvalidToken { .. }),
                "{expression}: {error}"
            );
        }
        Ok(())
    }

    #[test]
    fn test_day_of_week_seven_is_sunday_like_zero() -> TestResult {
        let seven = cron("0 0 * * 7")?;
        let zero = cron("0 0 * * 0")?;
        assert_eq!(seven.days_of_week, zero.days_of_week);
        assert_eq!(seven.days_of_week, 1);
        // `5-7` = Fri, Sat, Sun.
        assert_eq!(cron("0 0 * * 5-7")?.days_of_week, 0b110_0001);
        // `*` never sets the (folded-away) bit 7.
        assert_eq!(cron("0 0 * * *")?.days_of_week, 0b111_1111);
        Ok(())
    }

    #[test]
    fn test_parse_builds_expected_masks_for_lists_ranges_and_steps() -> TestResult {
        let schedule = cron("*/15 1,3-5,20-23/2 1/10 2-12/5 *")?;
        assert_eq!(
            schedule.minutes,
            (1 << 0) | (1 << 15) | (1 << 30) | (1 << 45)
        );
        assert_eq!(
            schedule.hours,
            (1 << 1) | (1 << 3) | (1 << 4) | (1 << 5) | (1 << 20) | (1 << 22)
        );
        // `1/10` = `1-31/10`.
        assert_eq!(
            schedule.days_of_month,
            (1 << 1) | (1 << 11) | (1 << 21) | (1 << 31)
        );
        assert_eq!(schedule.months, (1 << 2) | (1 << 7) | (1 << 12));
        // Step larger than the range keeps just the start.
        assert_eq!(cron("5/100 * * * *")?.minutes, 1 << 5);
        Ok(())
    }

    // --- next_after: basics ---------------------------------------------

    #[test]
    fn test_every_minute_is_strictly_after_and_minute_aligned() -> TestResult {
        assert_next("* * * * *", "2026-01-01T10:00:30Z", "2026-01-01T10:01:00Z")?;
        // Exactly on a fire time: the next one, never the same.
        assert_next("* * * * *", "2026-01-01T10:01:00Z", "2026-01-01T10:02:00Z")?;
        assert_next(
            "* * * * *",
            "2026-01-01T10:01:59.999999999Z",
            "2026-01-01T10:02:00Z",
        )?;
        Ok(())
    }

    #[test]
    fn test_lists_ranges_and_steps_in_minutes() -> TestResult {
        assert_next(
            "5,10 * * * *",
            "2026-01-01T10:05:00Z",
            "2026-01-01T10:10:00Z",
        )?;
        assert_next(
            "5,10 * * * *",
            "2026-01-01T10:10:00Z",
            "2026-01-01T11:05:00Z",
        )?;
        assert_next(
            "*/15 * * * *",
            "2026-01-01T10:46:00Z",
            "2026-01-01T11:00:00Z",
        )?;
        assert_next(
            "10-20/5 * * * *",
            "2026-01-01T10:12:00Z",
            "2026-01-01T10:15:00Z",
        )?;
        assert_next(
            "10-20/5 * * * *",
            "2026-01-01T10:20:00Z",
            "2026-01-01T11:10:00Z",
        )?;
        assert_next(
            "50/5 * * * *",
            "2026-01-01T10:56:00Z",
            "2026-01-01T11:50:00Z",
        )?;
        Ok(())
    }

    #[test]
    fn test_hours_roll_over_to_the_next_day_and_month() -> TestResult {
        assert_next("30 4 * * *", "2026-01-01T05:00:00Z", "2026-01-02T04:30:00Z")?;
        assert_next("30 4 * * *", "2026-01-01T04:29:59Z", "2026-01-01T04:30:00Z")?;
        assert_next("0 0 * * *", "2026-01-31T23:59:59Z", "2026-02-01T00:00:00Z")?;
        assert_next("0 0 * * *", "2026-12-31T00:00:00Z", "2027-01-01T00:00:00Z")?;
        assert_next(
            "15 22-23 * * *",
            "2026-02-28T23:15:00Z",
            "2026-03-01T22:15:00Z",
        )?;
        Ok(())
    }

    // --- next_after: month boundaries -----------------------------------

    #[test]
    fn test_day_31_skips_short_months() -> TestResult {
        assert_next(
            "0 12 31 * *",
            "2026-01-31T13:00:00Z",
            "2026-03-31T12:00:00Z",
        )?;
        assert_next(
            "0 12 31 * *",
            "2026-03-31T12:00:00Z",
            "2026-05-31T12:00:00Z",
        )?;
        assert_next(
            "0 12 31 * *",
            "2026-07-31T12:00:00Z",
            "2026-08-31T12:00:00Z",
        )?;
        assert_next("0 0 30 * *", "2026-01-30T00:00:00Z", "2026-03-30T00:00:00Z")?;
        Ok(())
    }

    #[test]
    fn test_february_29_only_in_leap_years_including_century_rule() -> TestResult {
        assert_next("0 0 29 2 *", "2026-03-01T00:00:00Z", "2028-02-29T00:00:00Z")?;
        assert_next("0 0 29 2 *", "2024-02-28T23:59:00Z", "2024-02-29T00:00:00Z")?;
        // 2100 is not a leap year; 2000 was.
        assert_next("0 0 29 2 *", "2096-03-01T00:00:00Z", "2104-02-29T00:00:00Z")?;
        assert_next("0 0 29 2 *", "1999-01-01T00:00:00Z", "2000-02-29T00:00:00Z")?;
        Ok(())
    }

    #[test]
    fn test_month_restriction_rolls_over_the_year() -> TestResult {
        assert_next("0 0 1 1 *", "2026-06-15T00:00:00Z", "2027-01-01T00:00:00Z")?;
        assert_next(
            "0 0 1 6-8 *",
            "2026-08-02T00:00:00Z",
            "2027-06-01T00:00:00Z",
        )?;
        assert_next(
            "0 0 1 6-8 *",
            "2026-06-01T00:00:00Z",
            "2026-07-01T00:00:00Z",
        )?;
        assert_next("0 0 * 2 *", "2026-02-28T00:00:00Z", "2027-02-01T00:00:00Z")?;
        Ok(())
    }

    #[test]
    fn test_impossible_expressions_return_none() -> TestResult {
        let after = ts("2026-01-01T00:00:00Z")?;
        assert_eq!(cron("0 0 30 2 *")?.next_after(after), None);
        assert_eq!(cron("0 0 31 4,6,9,11 *")?.next_after(after), None);
        // `*/2` in DOW counts as unrestricted → AND with an impossible DOM.
        assert_eq!(cron("0 0 31 2 */2")?.next_after(after), None);
        Ok(())
    }

    #[test]
    fn test_next_after_near_timestamp_max_returns_none() -> TestResult {
        assert_eq!(cron("0 0 1 1 *")?.next_after(jiff::Timestamp::MAX), None);
        Ok(())
    }

    #[test]
    fn test_next_after_before_the_unix_epoch_with_fractional_seconds() -> TestResult {
        assert_next(
            "* * * * *",
            "1969-12-31T23:59:59.5Z",
            "1970-01-01T00:00:00Z",
        )?;
        assert_next("0 0 * * *", "1969-12-30T12:00:00Z", "1969-12-31T00:00:00Z")?;
        Ok(())
    }

    // --- next_after: day-of-month / day-of-week semantics ---------------

    #[test]
    fn test_day_of_week_only() -> TestResult {
        // 2026-01-01 is a Thursday; the next Monday is 2026-01-05.
        assert_next("0 9 * * 1", "2026-01-01T00:00:00Z", "2026-01-05T09:00:00Z")?;
        assert_next("0 9 * * 1", "2026-01-05T09:00:00Z", "2026-01-12T09:00:00Z")?;
        // Sunday as 0 and as 7.
        assert_next("0 0 * * 0", "2026-01-01T00:00:00Z", "2026-01-04T00:00:00Z")?;
        assert_next("0 0 * * 7", "2026-01-01T00:00:00Z", "2026-01-04T00:00:00Z")?;
        // Weekdays only: Friday evening → Monday.
        assert_next(
            "0 8 * * 1-5",
            "2026-01-02T09:00:00Z",
            "2026-01-05T08:00:00Z",
        )?;
        Ok(())
    }

    #[test]
    fn test_day_of_month_only_ignores_weekday() -> TestResult {
        assert_next("0 0 13 * *", "2026-01-01T00:00:00Z", "2026-01-13T00:00:00Z")?;
        Ok(())
    }

    #[test]
    fn test_both_restricted_means_either_matches() -> TestResult {
        // "13th of the month OR Friday": Friday 2026-01-02 comes first …
        assert_next("0 0 13 * 5", "2026-01-01T00:00:00Z", "2026-01-02T00:00:00Z")?;
        // … and the 13th (a Tuesday) fires even though it is not a Friday.
        assert_next("0 0 13 * 5", "2026-01-10T00:00:00Z", "2026-01-13T00:00:00Z")?;
        assert_next("0 0 13 * 5", "2026-01-13T00:00:00Z", "2026-01-16T00:00:00Z")?;
        // "1st OR Monday": Feb 1 2026 is a Sunday and still fires.
        assert_next("0 0 1 * 1", "2026-01-26T00:00:00Z", "2026-02-01T00:00:00Z")?;
        assert_next("0 0 1 * 1", "2026-02-01T00:00:00Z", "2026-02-02T00:00:00Z")?;
        Ok(())
    }

    #[test]
    fn test_star_prefixed_day_of_week_step_means_both_must_match() -> TestResult {
        // `*/2` (Sun, Tue, Thu, Sat) starts with `*` → AND with DOM 13:
        // Jan 13 2026 = Tue (fires); Feb/Mar = Fri, Apr = Mon, May = Wed
        // (skipped); Jun 13 = Sat (fires).
        assert_next(
            "0 0 13 * */2",
            "2026-01-01T00:00:00Z",
            "2026-01-13T00:00:00Z",
        )?;
        assert_next(
            "0 0 13 * */2",
            "2026-01-14T00:00:00Z",
            "2026-06-13T00:00:00Z",
        )?;
        Ok(())
    }

    // --- Cross-checks -----------------------------------------------------

    /// The hand-rolled civil calendar agrees with jiff across ±~2200 years.
    #[test]
    fn test_civil_conversion_agrees_with_jiff() -> TestResult {
        let mut days: i64 = -800_000;
        while days <= 800_000 {
            let timestamp = jiff::Timestamp::from_second(days * 86_400)?;
            let civil = jiff::tz::Offset::UTC.to_datetime(timestamp);
            let (year, month, day) = civil_from_days(days);
            assert_eq!(year, i64::from(civil.year()), "year for day {days}");
            assert_eq!(
                i64::from(month),
                i64::from(civil.month()),
                "month for day {days}"
            );
            assert_eq!(i64::from(day), i64::from(civil.day()), "day for day {days}");
            assert_eq!(days_from_civil(year, month, day), days);
            assert_eq!(
                i64::from(weekday_sunday_zero(days)),
                i64::from(civil.weekday().to_sunday_zero_offset()),
                "weekday for day {days}"
            );
            days += 997;
        }
        Ok(())
    }

    /// `next_after` agrees with a brute-force minute-by-minute scan over
    /// `matches` (result matches, nothing in between matches).
    #[test]
    fn test_next_after_agrees_with_brute_force_scan() -> TestResult {
        let expressions = [
            "* * * * *",
            "*/7 */5 * * *",
            "0 12 * * 1-5",
            "30 0 1,15 * *",
            "0 0 13 * 5",
            "45 23 28-31 * *",
            "0 6 * 2 0",
            "59 23 31 12 *",
        ];
        let starts = [
            "2026-01-01T00:00:00Z",
            "2026-02-27T23:59:30Z",
            "2027-12-31T23:58:00Z",
            "2028-02-28T12:34:56Z",
        ];
        for expression in expressions {
            let schedule = cron(expression)?;
            for start in starts {
                let after = ts(start)?;
                let next = schedule
                    .next_after(after)
                    .ok_or(TestError::Missing("frequent expression fires"))?;
                assert!(next > after, "{expression} after {start}");
                assert!(schedule.matches(next), "{expression}: {next} must match");
                let mut probe = after.checked_add(jiff::SignedDuration::from_secs(
                    60 - after.as_second().rem_euclid(60),
                ))?;
                while probe < next {
                    assert!(
                        !schedule.matches(probe),
                        "{expression} after {start}: {probe} matches before {next}"
                    );
                    probe = probe.checked_add(jiff::SignedDuration::from_secs(60))?;
                }
            }
        }
        Ok(())
    }

    // --- DreamSchedule ------------------------------------------------------

    fn recommended() -> TestResult<DreamSchedule> {
        DreamSchedule::recommended().map_err(crate::test_support::ctx("default expression parses"))
    }

    #[test]
    fn test_recommended_schedule_is_weekly_monday_0300_utc() -> TestResult {
        let schedule = recommended()?;
        assert_eq!(schedule.cron().as_str(), DEFAULT_DREAM_CRON_EXPRESSION);
        assert_eq!(
            schedule.next_run_after(ts("2026-01-01T00:00:00Z")?),
            Some(ts("2026-01-05T03:00:00Z")?)
        );
        Ok(())
    }

    #[test]
    fn test_decide_never_run_is_due_immediately() -> TestResult {
        assert_eq!(
            recommended()?.decide(None, ts("2026-01-01T00:00:00Z")?),
            DreamDecision::Due {
                scheduled_for: None
            }
        );
        Ok(())
    }

    #[test]
    fn test_decide_not_due_reports_next_fire() -> TestResult {
        let decision = recommended()?.decide(
            Some(ts("2026-01-05T03:00:00Z")?),
            ts("2026-01-12T02:59:59Z")?,
        );
        assert_eq!(
            decision,
            DreamDecision::NotDue {
                next_fire: ts("2026-01-12T03:00:00Z")?
            }
        );
        assert!(!decision.is_due());
        Ok(())
    }

    #[test]
    fn test_decide_due_exactly_at_fire_time() -> TestResult {
        assert_eq!(
            recommended()?.decide(
                Some(ts("2026-01-05T03:00:00Z")?),
                ts("2026-01-12T03:00:00Z")?
            ),
            DreamDecision::Due {
                scheduled_for: Some(ts("2026-01-12T03:00:00Z")?)
            }
        );
        Ok(())
    }

    #[test]
    fn test_decide_coalesces_missed_runs_to_the_earliest() -> TestResult {
        // Three Mondays missed; one run is due, reporting the first missed one.
        assert_eq!(
            recommended()?.decide(
                Some(ts("2026-01-05T03:00:00Z")?),
                ts("2026-01-30T00:00:00Z")?
            ),
            DreamDecision::Due {
                scheduled_for: Some(ts("2026-01-12T03:00:00Z")?)
            }
        );
        Ok(())
    }

    #[test]
    fn test_decide_last_run_in_the_future_is_not_due() -> TestResult {
        let decision = recommended()?.decide(
            Some(ts("2026-02-02T03:00:00Z")?),
            ts("2026-01-20T00:00:00Z")?,
        );
        assert_eq!(
            decision,
            DreamDecision::NotDue {
                next_fire: ts("2026-02-09T03:00:00Z")?
            }
        );
        Ok(())
    }

    #[test]
    fn test_decide_impossible_schedule_is_never() -> TestResult {
        let schedule = DreamSchedule::parse("0 0 30 2 *")
            .map_err(crate::test_support::ctx("syntactically valid"))?;
        assert_eq!(
            schedule.decide(
                Some(ts("2026-01-01T00:00:00Z")?),
                ts("2030-01-01T00:00:00Z")?
            ),
            DreamDecision::Never
        );
        Ok(())
    }

    fn context_proposal_at(
        slug: &str,
        generated_at: jiff::Timestamp,
    ) -> TestResult<ContextProposal> {
        Ok(ContextProposal::new(
            ArtifactId::new(format!("context-proposal/{slug}")),
            "Testvorschlag",
            DefinitionId::parse("harwness.context.security-triage@1")
                .map_err(crate::test_support::ctx("valid definition id"))?,
            "deadbeef".repeat(8),
            Vec::new(),
            Vec::new(),
            generated_at,
            "heuristic:must-include-promotion@1",
        ))
    }

    #[test]
    fn test_steward_digest_if_due_skips_when_not_due() -> TestResult {
        let now = ts("2026-01-08T00:00:00Z")?;
        let digest = steward_digest_if_due(
            &recommended()?,
            Some(ts("2026-01-05T03:00:00Z")?),
            now,
            &[context_proposal_at("recent", now)?],
            &[],
        )?;
        assert!(digest.is_none());
        Ok(())
    }

    #[test]
    fn test_steward_digest_if_due_builds_digest_over_default_window() -> TestResult {
        let now = ts("2026-01-12T03:00:00Z")?;
        let digest = steward_digest_if_due(
            &recommended()?,
            Some(ts("2026-01-05T03:00:00Z")?),
            now,
            &[
                context_proposal_at("recent", ts("2026-01-06T00:00:00Z")?)?,
                context_proposal_at("stale", ts("2026-01-04T00:00:00Z")?)?,
            ],
            &[],
        )?
        .ok_or(TestError::Missing("due run yields a digest"))?;
        assert_eq!(digest.context_proposals.pending_in_window.len(), 1);
        assert_eq!(digest.context_proposals.pending_outside_window, 1);
        assert_eq!(
            digest.context_proposals.pending_in_window[0].id,
            ArtifactId::new("context-proposal/recent")
        );
        Ok(())
    }
}
