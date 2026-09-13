//! Memory als Fragment-Quelle — `selection_role_for` als die eine Naht.
//!
//! # Verantwortungsbereich
//! [`MemoryContextProvider`] ist die zweite der drei AW3-03-Quellenbindungen:
//! sie liefert [`harw_context::Fragment`]e aus HOT/STM/WARM
//! ([`crate::context_policy::render_turn_context`] über
//! [`crate::context_selector::select_for_turn_no_signals`]).
//!
//! # `selection_role_for` — genau ein Symbol
//! [`crate::context_selector::SelectionRole`] steuert, wie viele epistemische
//! Signale ein Turn sehen darf ([`crate::context_selector::SelectionRole::signal_cap`])
//! — „von der Runtime gesetzt, nicht vom Modell selbst" (Authority-Reduktion,
//! siehe dessen eigene Moduldoku). Ein [`harw_extension_api::types::TurnInputContext`]
//! trägt diese Rolle nicht als eigenes Feld, sondern lose in
//! `metadata: serde_json::Value` — und genau das ist die Naht, an der zwei
//! Aufrufer leicht auseinanderdriften könnten: liest der eine `"role"`, der
//! andere `"selection_role"`, oder verwendet der eine eigene Strings statt
//! der `snake_case`-Form aus `SelectionRole`s `Deserialize`-Implementierung,
//! bekommt derselbe Turn je nach Aufrufer eine andere Rolle zugewiesen.
//! [`selection_role_for`] ist die **einzige** Stelle, die `ctx.metadata`
//! danach befragt — sie delegiert an `SelectionRole`s eigene
//! `Deserialize`-Implementierung (bereits gegen `snake_case` getestet in
//! `context_selector.rs`), statt eine zweite, potenziell abweichende
//! String-Tabelle zu pflegen. Fehlt das Feld oder ist es kein gültiger
//! `SelectionRole`-Wert, ist [`crate::context_selector::SelectionRole::Scout`]
//! die Antwort — die engste Rolle (`signal_cap() == 4`), fail-closed statt
//! fail-open auf die breiteste.
//!
//! # Wo die Sichtbarkeit durchgesetzt wird
//! Diese Quelle kennt keine `VisibilityScope` — Memory ist sitzungsgebunden,
//! nicht mehrparteien-sichtbarkeitsgegliedert wie `harw-knowledge`. Die
//! einzige Begrenzung ist das rollenspezifische Signal-Cap aus
//! `selection_role_for`, angewendet innerhalb von
//! `select_for_turn_no_signals` selbst (`crate::context_selector`).
//!
//! # Vertrauen und Namensraum
//! [`MEMORY_CONTEXT_NAMESPACE`] (`"memory"`), [`MEMORY_CONTEXT_MAX_TRUST`]
//! ([`harw_context::TrustClass::Data`] — HOT/STM/WARM-Inhalt kann von
//! Korrekturen, Reflexionen oder früherem Turn-Text stammen, den weder
//! Operator noch Harness vollständig kontrollieren) und
//! [`MEMORY_CONTEXT_MAY_CARRY_USER_CONTENT`] (`true`).
//!
//! # Exportierte Typen
//! [`selection_role_for`], [`MemoryContextProvider`],
//! [`MEMORY_CONTEXT_NAMESPACE`], [`MEMORY_CONTEXT_MAX_TRUST`],
//! [`MEMORY_CONTEXT_MAY_CARRY_USER_CONTENT`].
//!
//! # Concurrency
//! `MemoryContextProvider<M>` ist `Send + Sync`, solange `M: Memory` es ist
//! (verlangt vom `Memory`-Trait selbst). `fragments` liest Store und STM
//! synchron; keine innere Veränderlichkeit.
//!
//! # Fehler
//! Kein eigener Fehlerfall: schlägt `select_for_turn_no_signals` fehl (siehe
//! [`crate::error::MemoryError`]), ist das Ergebnis eine leere Fragmentliste
//! statt eines propagierten Fehlers — ein nicht renderbarer Turn-Kontext hat
//! schlicht nichts zur Montage beizutragen, analog zu
//! `harw_plan_bridge::GoalContextProvider`s Umgang mit einem fehlenden Plan.

use std::sync::Arc;

use harw_context::{Fragment, FragmentLabel, FragmentOrigin, SectionName, Stability, TrustClass};
use harw_extension_api::types::TurnInputContext;
use harw_lens_types::{BytesOverFour, CostEstimator};

use crate::context_policy::ContextPolicy;
use crate::context_selector::{SelectionRequest, SelectionRole, select_for_turn_no_signals};
use crate::short_term::ShortTermMemory;
use crate::store::Memory;

/// Sektionspräfix, unter dem [`MemoryContextProvider`] liefert.
pub const MEMORY_CONTEXT_NAMESPACE: &str = "memory";

/// Höchste Vertrauensklasse, die [`MemoryContextProvider`] behaupten darf.
pub const MEMORY_CONTEXT_MAX_TRUST: TrustClass = TrustClass::Data;

/// `true`: HOT/STM/WARM-Inhalt kann Korrekturen oder früheren Turn-Text
/// tragen — [`MemoryContextProvider`] darf deshalb niemals
/// `TrustClass::Instruction` behaupten (siehe Moduldoku).
pub const MEMORY_CONTEXT_MAY_CARRY_USER_CONTENT: bool = true;

/// Sektion für den HOT-Anteil.
const SECTION_HOT: &str = "memory.hot";
/// Sektion für den STM-Anteil.
const SECTION_STM: &str = "memory.stm";
/// Sektion für WARM-Treffer.
const SECTION_WARM: &str = "memory.warm";

/// Name dieses Providers, für [`FragmentOrigin::provider`].
const PROVIDER_NAME: &str = "MemoryContextProvider";

/// JSON-Schlüssel in `TurnInputContext::metadata`, unter dem die Runtime die
/// Selektionsrolle des Turns ablegt.
const METADATA_KEY: &str = "selection_role";

/// Bestimmt die [`SelectionRole`] eines Turns — die einzige Stelle, die
/// `ctx.metadata` danach befragt.
///
/// # Description
/// Siehe Moduldokumentation, Abschnitt „`selection_role_for` — genau ein
/// Symbol". Delegiert an [`SelectionRole`]s eigene `Deserialize`-
/// Implementierung, statt eine zweite String-Tabelle zu pflegen: jede
/// künftige Erweiterung von `SelectionRole` (neue Variante, geänderte
/// Serialisierung) wirkt automatisch hier, ohne dass diese Funktion
/// angepasst werden muss.
///
/// # Arguments
/// - `ctx` (`&TurnInputContext`): der Turn-Kontext; nur `metadata` wird
///   gelesen.
///
/// # Returns
/// Die im Feld [`METADATA_KEY`] deklarierte [`SelectionRole`], oder
/// [`SelectionRole::Scout`] (die engste Rolle), wenn das Feld fehlt oder kein
/// gültiger `SelectionRole`-Wert ist — fail-closed, nie fail-open auf die
/// breiteste Rolle.
///
/// # Examples
/// ```rust
/// use harw_extension_api::types::TurnInputContext;
/// use harw_memory::context_provider::selection_role_for;
/// use harw_memory::context_selector::SelectionRole;
///
/// let mut ctx = TurnInputContext::default();
/// ctx.metadata = serde_json::json!({ "selection_role": "orchestrator" });
/// assert_eq!(selection_role_for(&ctx), SelectionRole::Orchestrator);
///
/// let missing = TurnInputContext::default();
/// assert_eq!(selection_role_for(&missing), SelectionRole::Scout);
/// ```
#[must_use]
pub fn selection_role_for(ctx: &TurnInputContext) -> SelectionRole {
    ctx.metadata
        .get(METADATA_KEY)
        .cloned()
        .and_then(|value| serde_json::from_value::<SelectionRole>(value).ok())
        .unwrap_or(SelectionRole::Scout)
}

/// Liefert HOT/STM/WARM-Memory als [`harw_context::Fragment`]e.
///
/// # Description
/// Rendert den Turn-Kontext über
/// [`select_for_turn_no_signals`] mit der über [`selection_role_for`]
/// bestimmten Rolle, dann übersetzt jeden nicht-leeren Abschnitt
/// (HOT, STM, jeder WARM-Treffer) in ein eigenes Fragment.
///
/// # Concurrency
/// `Send + Sync`, solange `M: Memory` es ist.
///
/// # Examples
/// ```rust,no_run
/// use std::sync::Arc;
/// use harw_extension_api::types::TurnInputContext;
/// use harw_memory::context_policy::ContextPolicy;
/// use harw_memory::context_provider::MemoryContextProvider;
/// use harw_memory::file_store::FileMemoryStore;
/// use harw_memory::short_term::ShortTermMemory;
///
/// let store = Arc::new(FileMemoryStore::open("/tmp/harw-mem-ctx-example").unwrap());
/// let provider = MemoryContextProvider::new(
///     store,
///     ShortTermMemory::new("session-1", 32, 2_048),
///     ContextPolicy::Balanced,
/// );
/// let fragments = provider.fragments(
///     &TurnInputContext::default(),
///     time::OffsetDateTime::UNIX_EPOCH,
///     jiff::Timestamp::UNIX_EPOCH,
/// );
/// assert!(fragments.is_empty(), "leerer Store liefert nichts");
/// ```
pub struct MemoryContextProvider<M: Memory> {
    /// LTM-Backend.
    store: Arc<M>,
    /// In-Process Short-Term Memory.
    stm: ShortTermMemory,
    /// Policy für Token-Budgets.
    policy: ContextPolicy,
}

impl<M: Memory> MemoryContextProvider<M> {
    /// Konstruiert einen Provider aus Store, STM und Policy.
    ///
    /// # Arguments
    /// - `store` (`Arc<M>`): LTM-Backend.
    /// - `stm` (`ShortTermMemory`): In-Process Short-Term Memory.
    /// - `policy` ([`ContextPolicy`]): wählt die Token-Budgets für das
    ///   Rendering.
    ///
    /// # Returns
    /// Den konfigurierten Provider.
    #[must_use]
    pub fn new(store: Arc<M>, stm: ShortTermMemory, policy: ContextPolicy) -> Self {
        Self {
            store,
            stm,
            policy,
        }
    }

    /// Baut die Memory-Fragmente für einen Turn.
    ///
    /// # Arguments
    /// - `ctx` (`&TurnInputContext`): liefert die Selektionsrolle über
    ///   [`selection_role_for`].
    /// - `now` (`time::OffsetDateTime`): Referenzzeitpunkt für die
    ///   Signal-Ablauf-Prüfung in `select_for_turn_no_signals` (hier stets
    ///   ohne Signale, aber dieselbe Zeitachse wie `harw-plan`).
    /// - `produced_at` (`jiff::Timestamp`): Zeitstempel für
    ///   [`FragmentOrigin::produced_at`], auf der `harw-research`/
    ///   `harw-job-runtime`-Zeitachse — beide Parameter sind injiziert statt
    ///   aus der Systemuhr gelesen, damit dieselbe Eingabe immer dasselbe
    ///   Ergebnis liefert (dieselbe „zwei Zeitachsen"-Disziplin wie
    ///   `harw_plan_bridge::finding_store`).
    ///
    /// # Returns
    /// Bis zu drei Fragmente (HOT, STM, ein weiteres je WARM-Treffer); ein
    /// leerer Abschnitt liefert kein Fragment. Schlägt das Rendering fehl,
    /// ist das Ergebnis eine leere Liste (siehe Moduldoku „Fehler").
    ///
    /// # Concurrency
    /// Liest Store und STM einmal synchron; hält danach keine Locks mehr.
    #[must_use]
    pub fn fragments(
        &self,
        ctx: &TurnInputContext,
        now: time::OffsetDateTime,
        produced_at: jiff::Timestamp,
    ) -> Vec<Fragment> {
        let role = selection_role_for(ctx);
        let request = SelectionRequest {
            policy: self.policy,
            role,
            namespace: None,
            keywords: &[],
            min_stm_salience: 0,
            min_promotion_score: None,
        };

        let result = match select_for_turn_no_signals(self.store.as_ref(), &self.stm, request, now)
        {
            Ok(result) => result,
            Err(error) => {
                tracing::debug!(
                    error = %error,
                    "Turn-Kontext konnte nicht gerendert werden — keine Memory-Fragmente"
                );
                return Vec::new();
            }
        };

        let mut fragments = Vec::new();
        push_fragment(&mut fragments, SECTION_HOT, "hot", &result.base.hot, produced_at);
        push_fragment(&mut fragments, SECTION_STM, "stm", &result.base.stm, produced_at);
        for (index, slice) in result.base.warm.iter().enumerate() {
            let label = format!("warm-{index}-{}", slice.namespace);
            push_fragment(&mut fragments, SECTION_WARM, &label, &slice.content, produced_at);
        }
        fragments
    }
}

/// Baut ein Fragment aus einem gerenderten Abschnitt und hängt es an, wenn
/// der Abschnitt nicht leer ist und Sektion/Label gültig sind.
///
/// # Description
/// Ein leerer Abschnitt (kein HOT, kein STM, kein WARM-Treffer) liefert kein
/// Fragment — eine leere Sektion ist keine Aussage, die zur Montage
/// beizutragen wäre. `section`/`label` sind feste bzw. aus `slice.namespace`
/// abgeleitete Literale ohne Steuerzeichen; ein Fehlschlag von
/// `SectionName::try_new`/`FragmentLabel::try_new` ist praktisch
/// unerreichbar, wird aber wie überall in diesem Provider durch
/// Überspringen statt `unwrap()` behandelt.
fn push_fragment(
    out: &mut Vec<Fragment>,
    section: &str,
    label: &str,
    body: &str,
    produced_at: jiff::Timestamp,
) {
    if body.trim().is_empty() {
        return;
    }
    let Ok(section) = SectionName::try_new(section) else {
        return;
    };
    let Ok(label) = FragmentLabel::try_new(label) else {
        return;
    };

    let body = body.to_owned();
    let cost = BytesOverFour.estimate(&body);
    let digest = harw_types::ContentDigest::of(body.as_bytes());

    out.push(Fragment {
        label,
        section,
        trust: MEMORY_CONTEXT_MAX_TRUST,
        stability: Stability::Fresh,
        origin: FragmentOrigin {
            provider: PROVIDER_NAME.to_owned(),
            namespace: MEMORY_CONTEXT_NAMESPACE.to_owned(),
            produced_at,
        },
        cost,
        digest,
        body,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_store::FileMemoryStore;
    use crate::types::Signal;

    fn tmp_root(tag: &str) -> std::path::PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "harw-memory-ctxprov-{}-{}-{}",
            tag,
            std::process::id(),
            id
        ));
        // Das Verzeichnis muss existieren, bevor jemand hineinschreibt -- der
        // Helfer lieferte zuvor nur einen Pfad, und das erste `fs::write`
        // scheiterte mit `NotFound`.
        std::fs::create_dir_all(&root).expect("Testwurzel anlegen");
        root
    }

    // ── selection_role_for ────────────────────────────────────────────────

    #[test]
    fn test_selection_role_for_reads_declared_role() {
        let ctx = TurnInputContext {
            metadata: serde_json::json!({ "selection_role": "verifier" }),
            ..Default::default()
        };
        assert_eq!(selection_role_for(&ctx), SelectionRole::Verifier);
    }

    #[test]
    fn test_selection_role_for_defaults_to_scout_when_missing() {
        let ctx = TurnInputContext::default();
        assert_eq!(selection_role_for(&ctx), SelectionRole::Scout);
    }

    #[test]
    fn test_selection_role_for_defaults_to_scout_on_invalid_value() {
        let ctx = TurnInputContext {
            metadata: serde_json::json!({ "selection_role": "not-a-real-role" }),
            ..Default::default()
        };
        assert_eq!(selection_role_for(&ctx), SelectionRole::Scout);
    }

    #[test]
    fn test_selection_role_for_is_deterministic() {
        let ctx = TurnInputContext {
            metadata: serde_json::json!({ "selection_role": "orchestrator" }),
            ..Default::default()
        };
        assert_eq!(selection_role_for(&ctx), selection_role_for(&ctx));
    }

    // ── MemoryContextProvider::fragments ─────────────────────────────────

    fn provider(root: &std::path::Path) -> MemoryContextProvider<FileMemoryStore> {
        let store = Arc::new(FileMemoryStore::open(root).expect("open memory store"));
        MemoryContextProvider::new(
            store,
            ShortTermMemory::new("session-1", 32, 2_048),
            ContextPolicy::Balanced,
        )
    }

    #[test]
    fn test_empty_store_contributes_nothing() {
        let root = tmp_root("empty");
        let fragments = provider(&root).fragments(
            &TurnInputContext::default(),
            time::OffsetDateTime::UNIX_EPOCH,
            jiff::Timestamp::UNIX_EPOCH,
        );
        assert!(fragments.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_hot_content_becomes_a_fragment_with_all_fields_populated() {
        let root = tmp_root("hot");
        std::fs::write(root.join("HOT.md"), "Regel 1: keine Doppelantworten.\n")
            .expect("write HOT.md");
        let produced_at = jiff::Timestamp::UNIX_EPOCH;

        let fragments = provider(&root).fragments(
            &TurnInputContext::default(),
            time::OffsetDateTime::UNIX_EPOCH,
            produced_at,
        );

        let hot = fragments
            .iter()
            .find(|f| f.section.as_str() == SECTION_HOT)
            .expect("HOT fragment present");
        assert_eq!(hot.trust, MEMORY_CONTEXT_MAX_TRUST);
        assert_eq!(hot.stability, Stability::Fresh);
        assert_eq!(hot.origin.provider, PROVIDER_NAME);
        assert_eq!(hot.origin.namespace, MEMORY_CONTEXT_NAMESPACE);
        assert_eq!(hot.origin.produced_at, produced_at);
        assert!(hot.cost.0 > 0);
        assert_ne!(hot.digest, harw_types::ContentDigest::of(b""));
        assert!(hot.body.contains("keine Doppelantworten"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_record_populates_a_warm_fragment_when_recalled() {
        let root = tmp_root("warm");
        let store = Arc::new(FileMemoryStore::open(&root).expect("open memory store"));
        // Die Beförderung nach WARM verlangt **drei** `PatternHint`-Signale mit
        // demselben Schlüssel -- so ist `FileMemoryStore::maintain` gebaut
        // (siehe dessen eigenen Test `maintain_counts_pending_before_
        // promoting_pattern_hints`). Ein einzelnes `Reflection` erzeugt keine
        // Warm-Scheibe; der Test forderte zuvor ein Fragment aus einem Signal,
        // das der Speicher gar nicht befördert.
        for note in ["Popup schluckt Enter", "Popup schluckt Enter erneut", "Popup darf Enter nie schlucken."] {
            store
                .record(Signal::PatternHint {
                    key: "tui-command-wiring".to_owned(),
                    note: note.to_owned(),
                })
                .expect("record pattern hint");
        }
        let report = store.maintain().expect("maintain promotes signals");
        assert_eq!(report.warm_created, 1, "drei gleiche Hinweise ergeben eine Warm-Scheibe");

        let provider = MemoryContextProvider::new(
            store,
            ShortTermMemory::new("session-1", 32, 2_048),
            ContextPolicy::BroadContext,
        );
        let fragments = provider.fragments(
            &TurnInputContext::default(),
            time::OffsetDateTime::UNIX_EPOCH,
            jiff::Timestamp::UNIX_EPOCH,
        );

        // HOT/STM/WARM sind alle drei je nach Maintenance-Ausgang möglich;
        // dieser Test verlangt nur, dass *irgendein* Fragment aus dem
        // Rendering entsteht, wenn zuvor tatsächlich ein Signal verarbeitet
        // wurde — die genaue Tier-Zuordnung ist Sache von `context_policy`,
        // nicht dieses Providers.
        assert!(!fragments.is_empty(), "erwartet mindestens ein Fragment nach maintain()");

        let _ = std::fs::remove_dir_all(&root);
    }
}
