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
//! Zeitpunkt, zu dem ein künftiger Dream-Job-Lauf beginnt), nie aus
//! `jiff::Timestamp::now()` gelesen. Begründung:
//!
//! - Ein durabler, periodischer Aufrufer (Dream-Job/Scheduler) existiert für
//!   diesen Teilbaum noch nicht — [`crate::model_behavior_proposal`]s eigene
//!   Moduldoku nennt das ausdrücklich als offenen Knoten ("Ein Scheduler, der
//!   sie periodisch mit echten Beobachtungen aufruft ... ist ein eigener,
//!   noch offener Knoten"), und `croner` (Cron-Parsing für Dream-Jobs) steht
//!   in `harw-knowledge/Cargo.toml` noch als `TODO(parent cargo add)`. Es
//!   gibt also keine etablierte Lauf-Kadenz, an der sich dieser Knoten
//!   orientieren könnte — die Fensterlänge ist eine eigenständige
//!   Entscheidung, kein Spiegel eines bereits fixierten Zeitplans.
//! - Sieben Tage sind lang genug, dass die Wiederholungsschwellen, die
//!   `ContextProposal`/`ModelBehaviorProposal` selbst voraussetzen
//!   ([`crate::context_proposal::OVER_BUDGET_PROMOTION_THRESHOLD`] `= 3`,
//!   [`crate::model_behavior_proposal::MIN_RELIABLE_SAMPLE_COUNT`] `= 5`),
//!   im normalen Betrieb überhaupt Gelegenheit haben, erreicht zu werden,
//!   bevor sie zusammengefasst würden.
//! - Sieben Tage sind kurz genug, dass ein `Pending`-Vorschlag nicht über
//!   Wochen unbemerkt liegen bleibt, bevor ihn ein Operator zu Gesicht
//!   bekommt — eine Kalenderwoche ist die kürzeste Spanne, die sich ohne
//!   Rückgriff auf eine noch nicht existierende Betriebs-Kadenz plausibel
//!   als "ein Mensch schaut mindestens so oft vorbei" begründen lässt.
//!
//! Das Fenster bleibt trotzdem immer ein expliziter Parameter, nie ein
//! interner Default, den eine Funktion selbst zieht: [`curate_context_proposals`]
//! und [`curate_model_behavior_proposals`] nehmen `&StewardWindow` entgegen,
//! [`DEFAULT_STEWARD_WINDOW_SPAN_DAYS`]/[`StewardWindow::trailing`] sind nur
//! die dokumentierte Empfehlung für den Aufrufer, der noch keine eigene
//! Spanne hat.
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
//! [`StewardWindow`], [`CuratedContextProposals`], [`CuratedModelBehaviorProposals`]
//! und [`StewardDigest`] sind reine `Send + Sync`-Werttypen ohne innere
//! Veränderlichkeit. Die drei `NullCounter` sind nach ihrer eigenen
//! Dokumentation `Sync` und beliebig nebenläufig erhöh-/lesbar
//! (`AtomicU64`, `Ordering::Relaxed`).
//!
//! # Fehler
//! [`crate::error::KnowledgeError::StewardWindowInvalid`], wenn
//! [`StewardWindow::new`]/[`StewardWindow::trailing`] mit einer
//! widersprüchlichen Spanne aufgerufen wird.

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
        Ok(Self { since, until: as_of })
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

    use harw_agent_dsl::ids::DefinitionId;
    use harw_model_catalog::descriptor::ToolCallingSupport;
    use harw_types::{ModelId, ProviderId};

    fn program_id() -> DefinitionId {
        DefinitionId::parse("harwness.context.security-triage@1").expect("valid definition id")
    }

    fn context_proposal_at(slug: &str, generated_at: jiff::Timestamp) -> ContextProposal {
        ContextProposal::new(
            ArtifactId::new(format!("context-proposal/{slug}")),
            "Testvorschlag",
            program_id(),
            "deadbeef".repeat(8),
            Vec::new(),
            Vec::new(),
            generated_at,
            "heuristic:must-include-promotion@1",
        )
    }

    fn model_behavior_proposal_at(slug: &str, generated_at: jiff::Timestamp) -> ModelBehaviorProposal {
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

    fn far_future() -> jiff::Timestamp {
        epoch()
            .checked_add(jiff::SignedDuration::from_secs(365 * 24 * 60 * 60))
            .expect("fixture timestamp stays within range")
    }

    /// `StewardWindow::new` rejects `since` after `until`.
    #[test]
    fn test_window_new_rejects_since_after_until() {
        let error = StewardWindow::new(far_future(), epoch())
            .expect_err("since after until must be rejected");
        assert!(matches!(error, KnowledgeError::StewardWindowInvalid { .. }));
    }

    /// `StewardWindow::trailing` builds `until = as_of` and `since` earlier,
    /// with `contains` behaving as documented.
    #[test]
    fn test_window_trailing_builds_since_before_until_and_contains_works() {
        let window = StewardWindow::trailing(far_future(), 7).expect("positive span builds a window");
        assert_eq!(window.until, far_future());
        assert!(window.since < window.until);
        assert!(window.contains(far_future()));
        assert!(!window.contains(epoch()), "epoch is well outside a 7-day trailing window ending a year later");
    }

    /// `StewardWindow::trailing` rejects a non-positive span.
    #[test]
    fn test_window_trailing_rejects_non_positive_span() {
        let error = StewardWindow::trailing(epoch(), 0).expect_err("zero span must be rejected");
        assert!(matches!(error, KnowledgeError::StewardWindowInvalid { .. }));
    }

    /// `StewardWindow::default_window` uses `DEFAULT_STEWARD_WINDOW_SPAN_DAYS`.
    #[test]
    fn test_default_window_uses_the_documented_default_span() {
        let window = StewardWindow::default_window(far_future()).expect("default span builds a window");
        let explicit =
            StewardWindow::trailing(far_future(), super::DEFAULT_STEWARD_WINDOW_SPAN_DAYS)
                .expect("explicit span builds a window");
        assert_eq!(window, explicit);
    }

    /// `curate_context_proposals` splits pending-in-window, pending-outside,
    /// and decided correctly.
    #[test]
    fn test_curate_context_proposals_splits_by_status_and_window() {
        let window = StewardWindow::new(epoch(), far_future()).expect("valid window");

        let in_window = context_proposal_at("in-window", epoch());
        let mut decided = context_proposal_at("decided", epoch());
        decided.accept().expect("pending proposal accepts");
        let outside_window = context_proposal_at(
            "outside-window",
            far_future()
                .checked_add(jiff::SignedDuration::from_secs(1))
                .expect("fixture timestamp stays within range"),
        );

        let curated = curate_context_proposals(&[in_window, decided, outside_window], &window);

        assert_eq!(curated.pending_in_window.len(), 1);
        assert_eq!(curated.pending_in_window[0].id, ArtifactId::new("context-proposal/in-window"));
        assert_eq!(curated.pending_outside_window, 1);
        assert_eq!(curated.decided, 1);
    }

    /// Determinism: same input, called twice, byte-identical (via `Debug`,
    /// since `CuratedContextProposals` carries no `Serialize`).
    #[test]
    fn test_curate_context_proposals_is_deterministic() {
        let window = StewardWindow::new(epoch(), far_future()).expect("valid window");
        let proposals = vec![
            context_proposal_at("zzz", epoch()),
            context_proposal_at("aaa", epoch()),
        ];

        let first = format!("{:?}", curate_context_proposals(&proposals, &window));
        let second = format!("{:?}", curate_context_proposals(&proposals, &window));
        assert_eq!(first, second);
    }

    /// Multiple qualifying proposals come back ordered by
    /// (`target_program`, `id`) — here identical `target_program`, so `id`
    /// alone decides.
    #[test]
    fn test_curate_context_proposals_orders_by_id_within_the_same_program() {
        let window = StewardWindow::new(epoch(), far_future()).expect("valid window");
        let proposals = vec![
            context_proposal_at("zzz", epoch()),
            context_proposal_at("aaa", epoch()),
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
    }

    /// `curate_model_behavior_proposals` splits by status/window and orders
    /// deterministically, mirroring the `ContextProposal` tests above.
    #[test]
    fn test_curate_model_behavior_proposals_splits_and_orders() {
        let window = StewardWindow::new(epoch(), far_future()).expect("valid window");

        let mut decided = model_behavior_proposal_at("decided", epoch());
        decided.reject().expect("pending proposal rejects");
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
    }

    /// `build_steward_digest` combines both domains under one window.
    #[test]
    fn test_build_steward_digest_combines_both_domains() {
        let window = StewardWindow::new(epoch(), far_future()).expect("valid window");
        let digest = build_steward_digest(
            &[context_proposal_at("example", epoch())],
            &[model_behavior_proposal_at("example", epoch())],
            &window,
        );
        assert_eq!(digest.context_proposals.pending_in_window.len(), 1);
        assert_eq!(digest.model_behavior_proposals.pending_in_window.len(), 1);
    }

    /// `render_digest_summary` reports the counts from both domains.
    #[test]
    fn test_render_digest_summary_reports_both_domain_counts() {
        let window = StewardWindow::new(epoch(), far_future()).expect("valid window");
        let digest = build_steward_digest(
            &[context_proposal_at("example", epoch())],
            &[],
            &window,
        );
        let summary = render_digest_summary(&digest);
        assert!(summary.contains("1 im Fenster offen"));
    }

    /// `steward_null_counter_registry` registers exactly the three
    /// documented counters, by name — independent of their current count,
    /// so this test stays stable regardless of what other tests in this
    /// crate's test binary have already driven over zero.
    #[test]
    fn test_steward_null_counter_registry_lists_exactly_three_counters_by_name() {
        let registry = steward_null_counter_registry();
        let names: Vec<&str> = registry.all().iter().map(|counter| counter.name()).collect();
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
    /// die beiden `curate_*`-Funktionen, `build_steward_digest` und
    /// `render_digest_summary` — alle nehmen Slices/Referenzen entgegen und
    /// geben neue Werte zurück, keine schreibt irgendwohin. Dieser Test
    /// belegt die eine Hälfte davon strukturell: dieselben Eingabe-Slices
    /// bleiben nach dem Aufruf unverändert (kein `&mut`-Parameter existiert
    /// in der Signatur, also ist das ohnehin durch den Typ erzwungen — dieser
    /// Test macht es trotzdem explizit nachvollziehbar).
    #[test]
    fn test_curate_functions_do_not_mutate_their_inputs() {
        let window = StewardWindow::new(epoch(), far_future()).expect("valid window");
        let proposals = vec![context_proposal_at("example", epoch())];
        let before = proposals.clone();

        let _curated: CuratedContextProposals = curate_context_proposals(&proposals, &window);

        assert_eq!(proposals.len(), before.len());
        assert_eq!(proposals[0].status, ProposalStatus::Pending);
        assert_eq!(proposals[0].id, before[0].id);
    }
}
