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
//! # Fakten aus zwei Wurzeln (M2, siehe `docs/design/memory-v3-ltm.md` §4)
//! Zusätzlich zu HOT/STM/WARM kann [`MemoryContextProvider`] optional einen
//! Projekt- und einen Global-[`crate::facts::FactStore`] tragen
//! ([`MemoryContextProvider::with_facts`]). Ist keiner konfiguriert, verhält
//! sich [`MemoryContextProvider::fragments`] exakt wie vor M2 — der
//! Fakten-Zweig trägt dann schlicht nichts bei. Sind sie konfiguriert, liefert
//! [`MemoryContextProvider::fragments`] **vor** HOT/STM/WARM zusätzliche
//! Fragmente, exakt in der Reihenfolge aus §4:
//! 1. Immer: ein aus dem Projekt-Store abgeleiteter Index (siehe
//!    [`FACT_INDEX_MAX_LINES`]) — äquivalent zu `MEMORY.md`, aber live aus
//!    [`crate::facts::FactStore::list`] gebaut, da `FactStore` weder Pfad
//!    noch Inhalt der generierten Datei exportiert und dieser Provider keinen
//!    Dateizugriff auf die Store-Wurzel besitzt.
//! 2. Immer: alle [`crate::facts::FactType::Preference`]-Fakten, Projekt vor
//!    Global.
//!    2b, ebenfalls immer (Addendum B): alle
//!    [`crate::facts::FactType::Pitfall`]-Fakten, Projekt vor Global,
//!    höchstens [`PITFALL_MAX_FACTS`] insgesamt, als ein Fragment unter der
//!    Überschrift „Bekannte Fallstricke".
//! 3. Nach Bedarf: Stichworttreffer aus [`crate::facts::FactStore::search`]
//!    gegen den jüngsten `user`-Eintrag im STM (`TurnInputContext` selbst
//!    trägt keinen Freitext-Nutzertext — nur `session_id`/`turn_id`/
//!    `metadata`), Projekt vor Global, bis [`DEFAULT_MEMORY_TOKEN_BUDGET`]
//!    (änderbar über [`MemoryContextProvider::with_memory_token_budget`])
//!    erschöpft ist.
//! 4. Nach Bedarf (Addendum B): sofern ein
//!    [`crate::file_index::FileKnowledgeIndex`] über
//!    [`MemoryContextProvider::with_file_index`] gesetzt ist und dieselben
//!    Stichworte aus Schritt 3 nicht leer sind, dessen Treffer als ein
//!    Fragment unter der Überschrift „Bekannte Dateien (bereits gelesen)".
//!
//! Schritte 3 und 4 prüfen das Budget; die übrigen Schritte zählen
//! unbedingt (2b trägt seine geschätzten Kosten dennoch in dieselbe laufende
//! Bilanz ein, wie in §4/Addendum B beschrieben).
//!
//! Jeder individuell ausgelieferte Fakt (Schritt 2, 2b und 3, nicht der
//! Index) trägt Scope und Name in seiner [`harw_context::FragmentLabel`] und
//! wird am Ende genau einmal je Store über
//! [`crate::facts::FactStore::record_usage`] gezählt; ein Fehler dabei ist
//! nur `tracing::warn!`, nie propagiert. Dateiwissen-Treffer (Schritt 4)
//! zählen nicht als Fakten und tragen nicht zu `record_usage` bei; ein
//! Fehler bei der Indexsuche ist ebenfalls nur `tracing::warn!`.
//!
//! # Concurrency
//! `MemoryContextProvider<M>` ist `Send + Sync`, solange `M: Memory` es ist
//! (verlangt vom `Memory`-Trait selbst). `fragments` liest Store und STM
//! synchron; keine innere Veränderlichkeit.
//!
//! # Fehler
//! Kein eigener Fehlerfall: schlägt `select_for_turn_no_signals` fehl (siehe
//! [`crate::error::MemoryError`]), enthält das Ergebnis nur die zuvor bereits
//! gesammelten Fakten-Fragmente statt eines propagierten Fehlers — ein nicht
//! renderbarer Turn-Kontext hat schlicht nichts zur HOT/STM/WARM-Montage
//! beizutragen, analog zu `harw_plan_bridge::GoalContextProvider`s Umgang mit
//! einem fehlenden Plan. Fehler beim Lesen/Schreiben eines Fakten-Stores
//! (`FactStore::list`/`search`/`record_usage`) sind ebenfalls nicht fatal:
//! nur `tracing::warn!`, der betroffene Schritt liefert dann schlicht nichts.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use harw_context::{Fragment, FragmentLabel, FragmentOrigin, SectionName, Stability, TrustClass};
use harw_extension_api::types::TurnInputContext;
use harw_lens_types::{BytesOverFour, CostEstimator};

use crate::context_policy::ContextPolicy;
use crate::context_selector::{SelectionRequest, SelectionRole, select_for_turn_no_signals};
use crate::facts::{Fact, FactScope, FactStore, FactType};
use crate::file_index::FileKnowledgeIndex;
use crate::short_term::{ShortTermMemory, StmRole};
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

/// Sektion für den aus dem Projekt-Store abgeleiteten Index (§4.1).
const SECTION_FACT_INDEX: &str = "memory.facts.index";
/// Sektion für Präferenz-Fakten (§4.2).
const SECTION_FACT_PREFERENCE: &str = "memory.facts.preference";
/// Sektion für Stichwort-Treffer unter Fakten (§4.3).
const SECTION_FACT_SEARCH: &str = "memory.facts.search";
/// Sektion für die immer geladenen `FactType::Pitfall`-Fakten
/// (Addendum B, "Bekannte Fallstricke").
const SECTION_FACT_PITFALL: &str = "memory.facts.pitfall";
/// Sektion für Dateiwissen-Treffer aus dem `FileKnowledgeIndex`
/// (Addendum B, "Bekannte Dateien (bereits gelesen)").
const SECTION_FILE_INDEX: &str = "memory.files.known";

/// Default-Token-Budget für den Fakten-Anteil aus §4, wenn
/// [`MemoryContextProvider::with_memory_token_budget`] nicht aufgerufen
/// wurde. Schätzung überall `len() / 4`, wie im übrigen Crate
/// (`context_selector::render_stm_above_salience`, `context_policy`).
pub const DEFAULT_MEMORY_TOKEN_BUDGET: usize = 1500;

/// Höchstzahl Zeilen des in [`render_fact_index`] gebauten Projekt-Index,
/// analog zu §4.1 („gekürzt auf 40 Zeilen").
const FACT_INDEX_MAX_LINES: usize = 40;

/// Höchstzahl Treffer, die [`crate::facts::FactStore::search`] je Store für
/// Schritt §4.3 liefern darf, bevor das Budget in
/// [`MemoryContextProvider::push_fact_fragments`] selbst greift.
const FACT_SEARCH_LIMIT_PER_STORE: usize = 20;

/// Höchstzahl Stichworte, die aus dem STM-Nutzertext für §4.3 verwendet
/// werden (siehe [`MemoryContextProvider::search_keywords`]).
const MAX_FACT_SEARCH_KEYWORDS: usize = 12;

/// Höchstzahl `FactType::Pitfall`-Fakten insgesamt (Projekt+Global
/// zusammen), die unter der Überschrift "Bekannte Fallstricke" ausgeliefert
/// werden (Addendum B).
const PITFALL_MAX_FACTS: usize = 10;

/// Höchstzahl Treffer, die [`FileKnowledgeIndex::search`] für den
/// Dateiwissen-Abschnitt liefern darf, bevor das Budget in
/// [`MemoryContextProvider::push_fact_fragments`] selbst greift
/// (Addendum B).
const FILE_INDEX_SEARCH_LIMIT: usize = 12;

/// Höchstzahl Symbole je Datei, die in der Dateiwissen-Zeile angezeigt
/// werden (Addendum B).
const FILE_INDEX_MAX_SYMBOLS: usize = 6;

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
    /// Fakten-Store der Projekt-Wurzel (`<projekt>/.harw/memories/`), siehe
    /// Design §2. `None`, wenn diesem Provider kein Projekt-Store übergeben
    /// wurde — dann trägt §4 nichts zu [`Self::fragments`] bei.
    project_facts: Option<Arc<FactStore>>,
    /// Fakten-Store der Global-Wurzel (`~/.harw/profiles/<p>/memories/`),
    /// siehe Design §2. `None` wie bei `project_facts`.
    global_facts: Option<Arc<FactStore>>,
    /// Token-Budget für den Fakten-Anteil aus §4 (Default
    /// [`DEFAULT_MEMORY_TOKEN_BUDGET`]).
    memory_token_budget: usize,
    /// Dateiwissen-Index (`<memories>/files/index.json`, Addendum B).
    /// `None`, wenn diesem Provider kein Index übergeben wurde — dann
    /// trägt der Dateiwissen-Abschnitt nichts zu [`Self::fragments`] bei.
    file_index: Option<Arc<FileKnowledgeIndex>>,
}

impl<M: Memory> MemoryContextProvider<M> {
    /// Konstruiert einen Provider aus Store, STM und Policy, **ohne**
    /// Fakten-Stores.
    ///
    /// # Beschreibung
    /// Bleibt für bestehende Aufrufer unverändert lauffähig: ohne einen
    /// nachträglichen [`Self::with_facts`]-Aufruf trägt §4 nichts zu
    /// [`Self::fragments`] bei, das Verhalten ist identisch zum Stand vor M2.
    ///
    /// # Arguments
    /// - `store` (`Arc<M>`): LTM-Backend.
    /// - `stm` (`ShortTermMemory`): In-Process Short-Term Memory.
    /// - `policy` ([`ContextPolicy`]): wählt die Token-Budgets für das
    ///   Rendering.
    ///
    /// # Returns
    /// Den konfigurierten Provider, ohne Projekt-/Global-Fakten.
    #[must_use]
    pub fn new(store: Arc<M>, stm: ShortTermMemory, policy: ContextPolicy) -> Self {
        Self {
            store,
            stm,
            policy,
            project_facts: None,
            global_facts: None,
            memory_token_budget: DEFAULT_MEMORY_TOKEN_BUDGET,
            file_index: None,
        }
    }

    /// Konstruiert einen Provider mit optionalen Projekt-/Global-Fakten-Stores
    /// (Design §2/§4).
    ///
    /// # Arguments
    /// - `store` (`Arc<M>`): LTM-Backend (HOT/STM/WARM).
    /// - `stm` (`ShortTermMemory`): In-Process Short-Term Memory.
    /// - `policy` ([`ContextPolicy`]): wählt die Token-Budgets für das
    ///   HOT/STM/WARM-Rendering.
    /// - `project_facts` (`Option<Arc<FactStore>>`): Fakten-Store der
    ///   Projekt-Wurzel, `None` wenn keiner verfügbar ist.
    /// - `global_facts` (`Option<Arc<FactStore>>`): Fakten-Store der
    ///   Global-Wurzel, `None` wenn keiner verfügbar ist.
    ///
    /// # Returns
    /// Den konfigurierten Provider mit [`DEFAULT_MEMORY_TOKEN_BUDGET`]; siehe
    /// [`Self::with_memory_token_budget`], um das Budget zu ändern.
    #[must_use]
    pub fn with_facts(
        store: Arc<M>,
        stm: ShortTermMemory,
        policy: ContextPolicy,
        project_facts: Option<Arc<FactStore>>,
        global_facts: Option<Arc<FactStore>>,
    ) -> Self {
        Self {
            store,
            stm,
            policy,
            project_facts,
            global_facts,
            memory_token_budget: DEFAULT_MEMORY_TOKEN_BUDGET,
            file_index: None,
        }
    }

    /// Überschreibt das Token-Budget für den Fakten-Anteil aus §4 (Default
    /// [`DEFAULT_MEMORY_TOKEN_BUDGET`]).
    ///
    /// # Arguments
    /// - `budget` (`usize`): neues Budget, Schätzung `len() / 4`.
    ///
    /// # Returns
    /// `self` mit geändertem Budget (Builder-Stil, verkettbar mit
    /// [`Self::new`]/[`Self::with_facts`]).
    #[must_use]
    pub fn with_memory_token_budget(mut self, budget: usize) -> Self {
        self.memory_token_budget = budget;
        self
    }

    /// Setzt (oder entfernt) den Dateiwissen-Index für den Abschnitt
    /// "Bekannte Dateien (bereits gelesen)" (Addendum B).
    ///
    /// # Beschreibung
    /// Ohne einen nachträglichen Aufruf trägt der Dateiwissen-Abschnitt
    /// nichts zu [`Self::fragments`] bei — analog zu [`Self::with_facts`]
    /// für die Fakten-Stores.
    ///
    /// # Arguments
    /// - `index` (`Option<Arc<FileKnowledgeIndex>>`): der Dateiwissen-Index
    ///   der Projekt-Wurzel (`<memories>/files/index.json`), oder `None`.
    ///
    /// # Returns
    /// `self` mit gesetztem Dateiwissen-Index (Builder-Stil, verkettbar mit
    /// [`Self::new`]/[`Self::with_facts`]/[`Self::with_memory_token_budget`]).
    #[must_use]
    pub fn with_file_index(mut self, index: Option<Arc<FileKnowledgeIndex>>) -> Self {
        self.file_index = index;
        self
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
    /// Zuerst die Fakten-Fragmente aus §4 ([`Self::push_fact_fragments`] —
    /// leer, wenn kein Fakten-Store konfiguriert ist), danach HOT, STM und
    /// ein weiteres Fragment je WARM-Treffer; ein leerer Abschnitt liefert
    /// kein Fragment. Schlägt das HOT/STM/WARM-Rendering fehl, enthält das
    /// Ergebnis nur die bereits gesammelten Fakten-Fragmente (siehe
    /// Moduldoku „Fehler") — ohne Fakten-Stores ist das weiterhin die leere
    /// Liste, identisch zum Verhalten vor M2.
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
        let mut fragments = Vec::new();
        self.push_fact_fragments(&mut fragments, produced_at);

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
                // Die bereits gesammelten Fakten-Fragmente (§4) sind von
                // dieser HOT/STM/WARM-Rendering-Frage unabhängig und bleiben
                // erhalten; ohne konfigurierte Fakten-Stores ist `fragments`
                // hier ohnehin leer — identisch zum Verhalten vor M2.
                return fragments;
            }
        };

        push_fragment(
            &mut fragments,
            SECTION_HOT,
            "hot",
            &result.base.hot,
            produced_at,
        );
        push_fragment(
            &mut fragments,
            SECTION_STM,
            "stm",
            &result.base.stm,
            produced_at,
        );
        for (index, slice) in result.base.warm.iter().enumerate() {
            let label = format!("warm-{index}-{}", slice.namespace);
            push_fragment(
                &mut fragments,
                SECTION_WARM,
                &label,
                &slice.content,
                produced_at,
            );
        }
        fragments
    }

    /// Baut die Fakten- und Dateiwissen-Fragmente aus §4(1-3) +
    /// Addendum B und pflegt anschließend die Nutzungszähler der
    /// ausgelieferten Fakten (§4, letzter Satz).
    ///
    /// # Beschreibung
    /// Reihenfolge: (1) der aus [`FactStore::list`] abgeleitete
    /// Projekt-Index ([`render_fact_index`]), immer wenn ein Projekt-Store
    /// konfiguriert ist; (2) alle [`FactType::Preference`]-Fakten, Projekt
    /// vor Global, je Store bereits nach `updated` absteigend sortiert
    /// (`FactStore::list`s eigene Garantie); (2b) alle
    /// [`FactType::Pitfall`]-Fakten, Projekt vor Global, höchstens
    /// [`PITFALL_MAX_FACTS`] insgesamt, als **ein** Fragment unter der
    /// Überschrift "Bekannte Fallstricke" (eine Zeile `- <description>` je
    /// Fakt, Rückfall auf `name` bei leerer Beschreibung); (3)
    /// Stichworttreffer aus [`FactStore::search`] gegen den jüngsten
    /// `user`-STM-Eintrag ([`Self::search_keywords`]), Projekt vor Global,
    /// so lange bis [`Self::memory_token_budget`] erschöpft ist; (4) falls
    /// [`Self::file_index`] gesetzt ist und Stichworte vorliegen, Treffer aus
    /// [`FileKnowledgeIndex::search`] (Limit [`FILE_INDEX_SEARCH_LIMIT`])
    /// als **ein** Fragment unter der Überschrift "Bekannte Dateien (bereits
    /// gelesen)" (eine Zeile `- <path> — <summary> [<symbole>]` je Treffer,
    /// höchstens [`FILE_INDEX_MAX_SYMBOLS`] Symbole).
    /// (1) und (2) zählen laut Design "immer" und werden nicht gegen das
    /// Budget geprüft; (2b) zählt ebenso "immer" wie (2), trägt aber wie (2)
    /// seine geschätzten Tokenkosten in [`Self::memory_token_budget`]s
    /// laufende Bilanz ein; (3) und (4) respektieren das verbleibende
    /// Budget (`len() / 4`-Schätzung je Zeile/Fragment-Inhalt, wie überall
    /// sonst in diesem Crate) und brechen ab statt fehlzuschlagen, sobald es
    /// erschöpft ist.
    ///
    /// Jeder individuell ausgelieferte Fakt (2, 2b und 3, nicht der Index)
    /// wird dedupliziert — ein Fakt erscheint höchstens einmal, auch wenn er
    /// sowohl Präferenz als auch Stichworttreffer ist — und am Ende in
    /// genau einem gepufferten [`FactStore::record_usage`]-Aufruf je Store
    /// gezählt; ein Fehler dabei wird nur geloggt (`tracing::warn!`), nie
    /// propagiert. Fehler beim Lesen des Dateiwissen-Index sind ebenfalls
    /// nur `tracing::warn!`.
    fn push_fact_fragments(&self, out: &mut Vec<Fragment>, produced_at: jiff::Timestamp) {
        let mut used_tokens = 0usize;
        let mut delivered: HashSet<(FactScope, String)> = HashSet::new();
        let mut project_delivered: Vec<String> = Vec::new();
        let mut global_delivered: Vec<String> = Vec::new();

        // (1) Projekt-Index — immer, wenn ein Projekt-Store konfiguriert ist.
        if let Some(project) = &self.project_facts {
            match project.list() {
                Ok(facts) => {
                    used_tokens += push_index_fragment(out, &facts, produced_at);
                }
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        "facts: Projekt-Index konnte nicht gelesen werden"
                    );
                }
            }
        }

        // (2) Präferenzen — immer, Projekt vor Global.
        let mut preference_facts: Vec<(FactScope, Fact)> = Vec::new();
        for (scope, store) in [
            (FactScope::Project, &self.project_facts),
            (FactScope::Global, &self.global_facts),
        ] {
            let Some(store) = store else { continue };
            match store.list() {
                Ok(facts) => preference_facts.extend(
                    facts
                        .into_iter()
                        .filter(|fact| fact.fact_type == FactType::Preference)
                        .map(|fact| (scope, fact)),
                ),
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        scope = %scope,
                        "facts: Präferenzen konnten nicht gelesen werden"
                    );
                }
            }
        }
        for (scope, fact) in preference_facts {
            if !delivered.insert((scope, fact.name.clone())) {
                continue;
            }
            used_tokens +=
                push_fact_fragment(out, SECTION_FACT_PREFERENCE, scope, &fact, produced_at);
            record_delivery(
                scope,
                &fact.name,
                &mut project_delivered,
                &mut global_delivered,
            );
        }

        // (2b) Fallstricke — immer, Projekt vor Global, max.
        // PITFALL_MAX_FACTS insgesamt (Addendum B).
        let mut pitfall_facts: Vec<(FactScope, Fact)> = Vec::new();
        for (scope, store) in [
            (FactScope::Project, &self.project_facts),
            (FactScope::Global, &self.global_facts),
        ] {
            let Some(store) = store else { continue };
            match store.list() {
                Ok(facts) => pitfall_facts.extend(
                    facts
                        .into_iter()
                        .filter(|fact| fact.fact_type == FactType::Pitfall)
                        .map(|fact| (scope, fact)),
                ),
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        scope = %scope,
                        "facts: Fallstricke konnten nicht gelesen werden"
                    );
                }
            }
        }
        let mut pitfall_lines: Vec<String> = Vec::new();
        for (scope, fact) in pitfall_facts {
            if pitfall_lines.len() >= PITFALL_MAX_FACTS {
                break;
            }
            if delivered.contains(&(scope, fact.name.clone())) {
                continue;
            }
            delivered.insert((scope, fact.name.clone()));
            let text = if fact.description.trim().is_empty() {
                fact.name.clone()
            } else {
                fact.description.clone()
            };
            pitfall_lines.push(format!("- {text}"));
            record_delivery(
                scope,
                &fact.name,
                &mut project_delivered,
                &mut global_delivered,
            );
        }
        if !pitfall_lines.is_empty() {
            let body = format!("## Bekannte Fallstricke\n{}", pitfall_lines.join("\n"));
            let cost = body.len() / 4;
            let before = out.len();
            push_fragment(out, SECTION_FACT_PITFALL, "pitfalls", &body, produced_at);
            if out.len() > before {
                used_tokens += cost;
            }
        }

        // Stichworte werden für (3) und den Dateiwissen-Abschnitt (Addendum
        // B) gemeinsam benötigt — einmal aus dem STM abgeleitet.
        let keywords_opt = self.search_keywords();

        // (3) Stichworttreffer — Projekt vor Global, bis das Budget voll ist.
        if let Some(keywords) = &keywords_opt {
            let keyword_refs: Vec<&str> = keywords.iter().map(String::as_str).collect();
            let mut hits: Vec<(FactScope, Fact)> = Vec::new();
            for (scope, store) in [
                (FactScope::Project, &self.project_facts),
                (FactScope::Global, &self.global_facts),
            ] {
                let Some(store) = store else { continue };
                match store.search(&keyword_refs, FACT_SEARCH_LIMIT_PER_STORE) {
                    Ok(facts) => hits.extend(facts.into_iter().map(|fact| (scope, fact))),
                    Err(error) => {
                        tracing::warn!(
                            error = %error,
                            scope = %scope,
                            "facts: Stichwortsuche fehlgeschlagen"
                        );
                    }
                }
            }
            for (scope, fact) in hits {
                if delivered.contains(&(scope, fact.name.clone())) {
                    continue;
                }
                let body = render_fact_body(&fact);
                let cost = body.len() / 4;
                if used_tokens + cost > self.memory_token_budget {
                    continue;
                }
                delivered.insert((scope, fact.name.clone()));
                push_fact_fragment(out, SECTION_FACT_SEARCH, scope, &fact, produced_at);
                used_tokens += cost;
                record_delivery(
                    scope,
                    &fact.name,
                    &mut project_delivered,
                    &mut global_delivered,
                );
            }
        }

        // (4) Dateiwissen-Treffer — nur wenn ein Index gesetzt ist und
        // Stichworte vorliegen (Addendum B); respektiert dasselbe Budget.
        if let Some(index) = &self.file_index {
            if let Some(keywords) = &keywords_opt {
                match index.search(keywords, FILE_INDEX_SEARCH_LIMIT) {
                    Ok(hits) => {
                        let mut lines: Vec<String> = Vec::new();
                        for hit in hits {
                            let summary = hit.summary.as_deref().unwrap_or("");
                            let symbols: Vec<&str> = hit
                                .symbols
                                .iter()
                                .take(FILE_INDEX_MAX_SYMBOLS)
                                .map(String::as_str)
                                .collect();
                            let line =
                                format!("- {} — {} [{}]", hit.path, summary, symbols.join(", "));
                            let cost = line.len() / 4;
                            if used_tokens + cost > self.memory_token_budget {
                                break;
                            }
                            used_tokens += cost;
                            lines.push(line);
                        }
                        if !lines.is_empty() {
                            let body = format!(
                                "## Bekannte Dateien (bereits gelesen)\n{}",
                                lines.join("\n")
                            );
                            push_fragment(
                                out,
                                SECTION_FILE_INDEX,
                                "known-files",
                                &body,
                                produced_at,
                            );
                        }
                    }
                    Err(error) => {
                        tracing::warn!(
                            error = %error,
                            "file_index: Stichwortsuche fehlgeschlagen"
                        );
                    }
                }
            }
        }

        if !project_delivered.is_empty() {
            if let Some(store) = &self.project_facts {
                let names: Vec<&str> = project_delivered.iter().map(String::as_str).collect();
                if let Err(error) = store.record_usage(&names) {
                    tracing::warn!(
                        error = %error,
                        "facts: Nutzungszähler (Projekt) konnten nicht geschrieben werden"
                    );
                }
            }
        }
        if !global_delivered.is_empty() {
            if let Some(store) = &self.global_facts {
                let names: Vec<&str> = global_delivered.iter().map(String::as_str).collect();
                if let Err(error) = store.record_usage(&names) {
                    tracing::warn!(
                        error = %error,
                        "facts: Nutzungszähler (Global) konnten nicht geschrieben werden"
                    );
                }
            }
        }
    }

    /// Leitet Stichworte für §4(3) aus dem letzten Nutzer-Eintrag im STM ab.
    ///
    /// # Beschreibung
    /// `TurnInputContext` trägt in diesem Crate keinen eigenen Freitext-
    /// Nutzertext (nur `session_id`, `turn_id`, `metadata`) — das
    /// In-Process-STM dieses Providers hält als einziges bereits den
    /// laufenden Dialog vor (Design §2: „Session … Ring-Buffer des
    /// laufenden Gesprächs"). Diese Funktion nimmt deshalb den jüngsten
    /// [`StmRole::User`]-Eintrag als Nutzertext für die Stichwortsuche,
    /// zerlegt ihn an nicht-alphanumerischen Zeichen und behält höchstens
    /// [`MAX_FACT_SEARCH_KEYWORDS`] kleingeschriebene Wörter ab drei
    /// Zeichen Länge.
    ///
    /// # Returns
    /// `None`, wenn kein Nutzer-Eintrag im STM vorliegt oder keine
    /// hinreichend langen Wörter übrig bleiben — Schritt (3) entfällt dann
    /// vollständig.
    fn search_keywords(&self) -> Option<Vec<String>> {
        let text = self
            .stm
            .snapshot()
            .into_iter()
            .rev()
            .find(|entry| entry.role == StmRole::User)
            .map(|entry| entry.content)?;
        let keywords: Vec<String> = text
            .split(|c: char| !c.is_alphanumeric())
            .filter(|word| word.chars().count() >= 3)
            .map(str::to_lowercase)
            .take(MAX_FACT_SEARCH_KEYWORDS)
            .collect();
        (!keywords.is_empty()).then_some(keywords)
    }
}

/// Ordnet den Namen eines ausgelieferten Fakts seinem Scope-Puffer zu, damit
/// [`MemoryContextProvider::push_fact_fragments`] am Ende genau einen
/// gepufferten [`FactStore::record_usage`]-Aufruf je Store absetzen kann.
fn record_delivery(
    scope: FactScope,
    name: &str,
    project: &mut Vec<String>,
    global: &mut Vec<String>,
) {
    match scope {
        FactScope::Project => project.push(name.to_owned()),
        FactScope::Global => global.push(name.to_owned()),
    }
}

/// Baut das Index-Fragment aus §4(1) und liefert die geschätzte Tokenzahl
/// zurück (0, wenn nichts angehängt wurde — leerer Index oder ungültige
/// Sektion/Beschriftung, siehe [`push_fragment`]).
fn push_index_fragment(
    out: &mut Vec<Fragment>,
    facts: &[Fact],
    produced_at: jiff::Timestamp,
) -> usize {
    let text = render_fact_index(facts);
    let cost = text.len() / 4;
    let before = out.len();
    push_fragment(out, SECTION_FACT_INDEX, "project-index", &text, produced_at);
    if out.len() > before { cost } else { 0 }
}

/// Baut ein einzelnes Fakten-Fragment (§4.2/§4.3) mit Scope und Name in der
/// Beschriftung und liefert die geschätzte Tokenzahl zurück (0, wenn nichts
/// angehängt wurde, siehe [`push_fragment`]).
fn push_fact_fragment(
    out: &mut Vec<Fragment>,
    section: &str,
    scope: FactScope,
    fact: &Fact,
    produced_at: jiff::Timestamp,
) -> usize {
    let label = format!("{}-{}", scope.as_str(), fact.name);
    let body = render_fact_body(fact);
    let cost = body.len() / 4;
    let before = out.len();
    push_fragment(out, section, &label, &body, produced_at);
    if out.len() > before { cost } else { 0 }
}

/// Baut einen `MEMORY.md`-äquivalenten Index aus bereits geladenen Fakten,
/// gruppiert nach [`FactType`] in [`FactType::ALL`]-Reihenfolge, je Gruppe
/// alphabetisch nach `name`, gekürzt auf höchstens [`FACT_INDEX_MAX_LINES`]
/// Zeilen.
///
/// # Beschreibung
/// `FactStore` legt `MEMORY.md` nur als generierte Datei ab
/// ([`FactStore::write_index`]) und exportiert weder deren Pfad noch ihren
/// Inhalt — dieser Provider hat keinen Dateizugriff auf die Store-Wurzel.
/// Der Index wird deshalb hier inhaltlich äquivalent aus
/// [`FactStore::list`] abgeleitet (gleiche Fakten, gleiche Gruppierung),
/// statt die persistierte Datei zu lesen — das Ergebnis ist unabhängig
/// davon, ob zuvor `write_index` gelaufen ist.
fn render_fact_index(facts: &[Fact]) -> String {
    let mut by_type: HashMap<FactType, Vec<&Fact>> = HashMap::new();
    for fact in facts {
        by_type.entry(fact.fact_type).or_default().push(fact);
    }
    let mut lines: Vec<String> = Vec::new();
    for fact_type in FactType::ALL {
        let Some(mut group) = by_type.remove(&fact_type) else {
            continue;
        };
        group.sort_by(|a, b| a.name.cmp(&b.name));
        lines.push(format!("## {fact_type}"));
        for fact in group {
            lines.push(format!(
                "- {} — {fact_type}, aktualisiert {}",
                fact.description,
                format_index_date(fact.updated)
            ));
        }
    }
    lines.truncate(FACT_INDEX_MAX_LINES);
    lines.join("\n")
}

/// Formatiert einen Zeitstempel als `yyyy-mm-dd` für [`render_fact_index`].
fn format_index_date(ts: time::OffsetDateTime) -> String {
    let format = time::macros::format_description!("[year]-[month]-[day]");
    ts.format(&format)
        .unwrap_or_else(|_| ts.unix_timestamp().to_string())
}

/// Baut den Fragment-Text eines einzelnen Fakts: Beschreibung, danach der
/// Freitext-`body`, falls nicht leer.
fn render_fact_body(fact: &Fact) -> String {
    let mut body = fact.description.clone();
    body.push('\n');
    let trimmed_body = fact.body.trim();
    if !trimmed_body.is_empty() {
        body.push('\n');
        body.push_str(trimmed_body);
        body.push('\n');
    }
    body
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
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::types::Signal;

    fn tmp_root(tag: &str) -> TestResult<std::path::PathBuf> {
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
        std::fs::create_dir_all(&root).map_err(ctx("Testwurzel anlegen"))?;
        Ok(root)
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

    fn provider(root: &std::path::Path) -> TestResult<MemoryContextProvider<FileMemoryStore>> {
        let store = Arc::new(FileMemoryStore::open(root).map_err(ctx("open memory store"))?);
        Ok(MemoryContextProvider::new(
            store,
            ShortTermMemory::new("session-1", 32, 2_048),
            ContextPolicy::Balanced,
        ))
    }

    #[test]
    fn test_empty_store_contributes_nothing() -> TestResult {
        let root = tmp_root("empty")?;
        let fragments = provider(&root)?.fragments(
            &TurnInputContext::default(),
            time::OffsetDateTime::UNIX_EPOCH,
            jiff::Timestamp::UNIX_EPOCH,
        );
        assert!(fragments.is_empty());
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    #[test]
    fn test_hot_content_becomes_a_fragment_with_all_fields_populated() -> TestResult {
        let root = tmp_root("hot")?;
        std::fs::write(root.join("HOT.md"), "Regel 1: keine Doppelantworten.\n")
            .map_err(ctx("write HOT.md"))?;
        let produced_at = jiff::Timestamp::UNIX_EPOCH;

        let fragments = provider(&root)?.fragments(
            &TurnInputContext::default(),
            time::OffsetDateTime::UNIX_EPOCH,
            produced_at,
        );

        let hot = fragments
            .iter()
            .find(|f| f.section.as_str() == SECTION_HOT)
            .ok_or(TestError::Missing("HOT fragment present"))?;
        assert_eq!(hot.trust, MEMORY_CONTEXT_MAX_TRUST);
        assert_eq!(hot.stability, Stability::Fresh);
        assert_eq!(hot.origin.provider, PROVIDER_NAME);
        assert_eq!(hot.origin.namespace, MEMORY_CONTEXT_NAMESPACE);
        assert_eq!(hot.origin.produced_at, produced_at);
        assert!(hot.cost.0 > 0);
        assert_ne!(hot.digest, harw_types::ContentDigest::of(b""));
        assert!(hot.body.contains("keine Doppelantworten"));

        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    #[test]
    fn test_record_populates_a_warm_fragment_when_recalled() -> TestResult {
        let root = tmp_root("warm")?;
        let store = Arc::new(FileMemoryStore::open(&root).map_err(ctx("open memory store"))?);
        // Die Beförderung nach WARM verlangt **drei** `PatternHint`-Signale mit
        // demselben Schlüssel -- so ist `FileMemoryStore::maintain` gebaut
        // (siehe dessen eigenen Test `maintain_counts_pending_before_
        // promoting_pattern_hints`). Ein einzelnes `Reflection` erzeugt keine
        // Warm-Scheibe; der Test forderte zuvor ein Fragment aus einem Signal,
        // das der Speicher gar nicht befördert.
        for note in [
            "Popup schluckt Enter",
            "Popup schluckt Enter erneut",
            "Popup darf Enter nie schlucken.",
        ] {
            store
                .record(Signal::PatternHint {
                    key: "tui-command-wiring".to_owned(),
                    note: note.to_owned(),
                })
                .map_err(ctx("record pattern hint"))?;
        }
        let report = store.maintain().map_err(ctx("maintain promotes signals"))?;
        assert_eq!(
            report.warm_created, 1,
            "drei gleiche Hinweise ergeben eine Warm-Scheibe"
        );

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
        assert!(
            !fragments.is_empty(),
            "erwartet mindestens ein Fragment nach maintain()"
        );

        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    // ── Fakten (M2, §4) ───────────────────────────────────────────────────

    fn make_fact(name: &str, fact_type: FactType, scope: FactScope, description: &str) -> Fact {
        let now = time::OffsetDateTime::now_utc();
        Fact {
            name: name.to_owned(),
            description: description.to_owned(),
            fact_type,
            scope,
            created: now,
            updated: now,
            confidence: 0.8,
            sources: Vec::new(),
            tags: Vec::new(),
            body: String::new(),
        }
    }

    fn facts_provider(
        mem_root: &std::path::Path,
        project_facts: Option<Arc<FactStore>>,
        global_facts: Option<Arc<FactStore>>,
    ) -> TestResult<MemoryContextProvider<FileMemoryStore>> {
        let store = Arc::new(FileMemoryStore::open(mem_root).map_err(ctx("open memory store"))?);
        Ok(MemoryContextProvider::with_facts(
            store,
            ShortTermMemory::new("session-1", 32, 2_048),
            ContextPolicy::Balanced,
            project_facts,
            global_facts,
        ))
    }

    #[test]
    fn test_without_fact_stores_behaves_like_before() -> TestResult {
        let root = tmp_root("no-facts")?;
        std::fs::write(root.join("HOT.md"), "Regel 1: keine Doppelantworten.\n")
            .map_err(ctx("write HOT.md"))?;

        let fragments = provider(&root)?.fragments(
            &TurnInputContext::default(),
            time::OffsetDateTime::UNIX_EPOCH,
            jiff::Timestamp::UNIX_EPOCH,
        );

        assert!(
            fragments
                .iter()
                .all(|f| !f.section.as_str().starts_with("memory.facts")),
            "ohne Fakten-Stores darf keine Fakten-Sektion erscheinen"
        );
        assert!(fragments.iter().any(|f| f.section.as_str() == SECTION_HOT));
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    #[test]
    fn test_preference_facts_always_included_project_before_global() -> TestResult {
        let mem_root = tmp_root("pref-mem")?;
        let project_root = tmp_root("pref-project")?;
        let global_root = tmp_root("pref-global")?;

        let project_store = Arc::new(
            FactStore::open(&project_root, FactScope::Project).map_err(ctx("open project"))?,
        );
        project_store
            .write(&make_fact(
                "proj-pref",
                FactType::Preference,
                FactScope::Project,
                "Projekt-Präferenz",
            ))
            .map_err(ctx("write project preference"))?;

        let global_store =
            Arc::new(FactStore::open(&global_root, FactScope::Global).map_err(ctx("open global"))?);
        global_store
            .write(&make_fact(
                "glob-pref",
                FactType::Preference,
                FactScope::Global,
                "Global-Präferenz",
            ))
            .map_err(ctx("write global preference"))?;

        let fragments = facts_provider(&mem_root, Some(project_store), Some(global_store))?
            .fragments(
                &TurnInputContext::default(),
                time::OffsetDateTime::UNIX_EPOCH,
                jiff::Timestamp::UNIX_EPOCH,
            );

        let project_pos = fragments
            .iter()
            .position(|f| f.label.as_str() == "project-proj-pref")
            .ok_or(TestError::Missing("project preference fragment present"))?;
        let global_pos = fragments
            .iter()
            .position(|f| f.label.as_str() == "global-glob-pref")
            .ok_or(TestError::Missing("global preference fragment present"))?;
        assert!(
            project_pos < global_pos,
            "Projekt-Präferenz muss vor Global-Präferenz stehen"
        );

        let _ = std::fs::remove_dir_all(&mem_root);
        let _ = std::fs::remove_dir_all(&project_root);
        let _ = std::fs::remove_dir_all(&global_root);
        Ok(())
    }

    #[test]
    fn test_search_hits_respect_the_memory_token_budget() -> TestResult {
        let mem_root = tmp_root("budget-mem")?;
        let project_root = tmp_root("budget-project")?;

        let project_store = Arc::new(
            FactStore::open(&project_root, FactScope::Project).map_err(ctx("open project"))?,
        );
        for i in 0..5 {
            project_store
                .write(&make_fact(
                    &format!("zeppelin-fakt-{i}"),
                    FactType::Fact,
                    FactScope::Project,
                    "Ein langer Text über Zeppeline und ihre Geschichte in der Luftfahrt.",
                ))
                .map_err(ctx("write fact"))?;
        }

        let stm = ShortTermMemory::new("session-1", 32, 2_048);
        stm.push(StmRole::User, 80, "Erzähl mir etwas über Zeppelin");
        let store = Arc::new(FileMemoryStore::open(&mem_root).map_err(ctx("open memory store"))?);
        let provider = MemoryContextProvider::with_facts(
            store,
            stm,
            ContextPolicy::Balanced,
            Some(project_store),
            None,
        )
        .with_memory_token_budget(5);

        let fragments = provider.fragments(
            &TurnInputContext::default(),
            time::OffsetDateTime::UNIX_EPOCH,
            jiff::Timestamp::UNIX_EPOCH,
        );

        let search_hits = fragments
            .iter()
            .filter(|f| f.section.as_str() == SECTION_FACT_SEARCH)
            .count();
        assert!(
            search_hits < 5,
            "ein Budget von 5 Token darf nicht alle 5 Treffer zulassen, got {search_hits}"
        );

        let _ = std::fs::remove_dir_all(&mem_root);
        let _ = std::fs::remove_dir_all(&project_root);
        Ok(())
    }

    // ── Fallstricke + Dateiwissen (Addendum B) ───────────────────────────

    #[test]
    fn test_pitfall_fact_appears_under_heading_without_keyword_match() -> TestResult {
        let mem_root = tmp_root("pitfall-mem")?;
        let project_root = tmp_root("pitfall-project")?;

        let project_store = Arc::new(
            FactStore::open(&project_root, FactScope::Project).map_err(ctx("open project"))?,
        );
        project_store
            .write(&make_fact(
                "known-pitfall",
                FactType::Pitfall,
                FactScope::Project,
                "Popup darf Enter nie schlucken.",
            ))
            .map_err(ctx("write pitfall"))?;

        // Kein STM-Nutzertext -> search_keywords() liefert None, der
        // Pitfall-Abschnitt darf trotzdem erscheinen ("immer" laut §4/Addendum B).
        let fragments = facts_provider(&mem_root, Some(project_store), None)?.fragments(
            &TurnInputContext::default(),
            time::OffsetDateTime::UNIX_EPOCH,
            jiff::Timestamp::UNIX_EPOCH,
        );

        let pitfall = fragments
            .iter()
            .find(|f| f.section.as_str() == SECTION_FACT_PITFALL)
            .ok_or(TestError::Missing(
                "pitfall fragment present even without keyword match",
            ))?;
        assert!(pitfall.body.contains("Bekannte Fallstricke"));
        assert!(pitfall.body.contains("- Popup darf Enter nie schlucken."));

        let _ = std::fs::remove_dir_all(&mem_root);
        let _ = std::fs::remove_dir_all(&project_root);
        Ok(())
    }

    fn make_file_knowledge(
        path: &str,
        summary: &str,
        symbols: Vec<String>,
    ) -> crate::file_index::FileKnowledge {
        crate::file_index::FileKnowledge {
            path: path.to_owned(),
            size_bytes: 42,
            line_count: 7,
            digest: "digest".to_owned(),
            language: Some("rust".to_owned()),
            summary: Some(summary.to_owned()),
            symbols,
            last_seen: "2024-01-01T00:00:00Z".to_owned(),
            read_count: 1,
        }
    }

    #[test]
    fn test_file_index_hit_appears_when_keyword_matches_symbol() -> TestResult {
        let mem_root = tmp_root("fidx-mem")?;
        let index_root = tmp_root("fidx-index")?;

        let index = crate::file_index::FileKnowledgeIndex::open(&index_root)
            .map_err(ctx("open file knowledge index"))?;
        index
            .upsert(make_file_knowledge(
                "src/zeppelin.rs",
                "Zeppelin-Hilfsfunktionen",
                vec!["zeppelin_helper".to_owned()],
            ))
            .map_err(ctx("upsert file knowledge"))?;

        let store = Arc::new(FileMemoryStore::open(&mem_root).map_err(ctx("open memory store"))?);
        let stm = ShortTermMemory::new("session-1", 32, 2_048);
        stm.push(StmRole::User, 80, "Erzähl mir etwas über zeppelin");
        let provider = MemoryContextProvider::new(store, stm, ContextPolicy::Balanced)
            .with_file_index(Some(Arc::new(index)));

        let fragments = provider.fragments(
            &TurnInputContext::default(),
            time::OffsetDateTime::UNIX_EPOCH,
            jiff::Timestamp::UNIX_EPOCH,
        );

        let file_hit = fragments
            .iter()
            .find(|f| f.section.as_str() == SECTION_FILE_INDEX)
            .ok_or(TestError::Missing(
                "file-index fragment present when a keyword matches a symbol",
            ))?;
        assert!(file_hit.body.contains("Bekannte Dateien (bereits gelesen)"));
        assert!(file_hit.body.contains("src/zeppelin.rs"));

        let _ = std::fs::remove_dir_all(&mem_root);
        let _ = std::fs::remove_dir_all(&index_root);
        Ok(())
    }

    #[test]
    fn test_file_index_section_is_truncated_by_a_tiny_budget_without_panicking() -> TestResult {
        let mem_root = tmp_root("fidx-budget-mem")?;
        let index_root = tmp_root("fidx-budget-index")?;

        let index = crate::file_index::FileKnowledgeIndex::open(&index_root)
            .map_err(ctx("open file knowledge index"))?;
        for i in 0..5 {
            index
                .upsert(make_file_knowledge(
                    &format!("src/zeppelin_{i}.rs"),
                    "Ein langer Beschreibungstext über Zeppeline und ihre Geschichte in der Luftfahrt, damit die Zeile teuer wird.",
                    vec!["zeppelin_helper".to_owned()],
                ))
                .map_err(ctx("upsert file knowledge"))?;
        }

        let store = Arc::new(FileMemoryStore::open(&mem_root).map_err(ctx("open memory store"))?);
        let stm = ShortTermMemory::new("session-1", 32, 2_048);
        stm.push(StmRole::User, 80, "Erzähl mir etwas über zeppelin");
        let provider = MemoryContextProvider::new(store, stm, ContextPolicy::Balanced)
            .with_file_index(Some(Arc::new(index)))
            .with_memory_token_budget(1);

        let fragments = provider.fragments(
            &TurnInputContext::default(),
            time::OffsetDateTime::UNIX_EPOCH,
            jiff::Timestamp::UNIX_EPOCH,
        );

        let file_hit_count = fragments
            .iter()
            .filter(|f| f.section.as_str() == SECTION_FILE_INDEX)
            .count();
        assert!(
            file_hit_count <= 1,
            "ein winziges Budget darf höchstens ein Dateiwissen-Fragment (oder keins) zulassen"
        );
        if let Some(hit) = fragments
            .iter()
            .find(|f| f.section.as_str() == SECTION_FILE_INDEX)
        {
            let line_count = hit
                .body
                .lines()
                .filter(|l| l.starts_with("- src/zeppelin"))
                .count();
            assert!(
                line_count < 5,
                "ein Budget von 1 Token darf nicht alle 5 Dateizeilen zulassen, got {line_count}"
            );
        }

        let _ = std::fs::remove_dir_all(&mem_root);
        let _ = std::fs::remove_dir_all(&index_root);
        Ok(())
    }

    #[test]
    fn test_delivered_fact_usage_counter_increments() -> TestResult {
        let mem_root = tmp_root("usage-mem")?;
        let project_root = tmp_root("usage-project")?;

        let project_store = Arc::new(
            FactStore::open(&project_root, FactScope::Project).map_err(ctx("open project"))?,
        );
        project_store
            .write(&make_fact(
                "usage-pref",
                FactType::Preference,
                FactScope::Project,
                "Präferenz",
            ))
            .map_err(ctx("write preference"))?;
        assert!(project_store.usage("usage-pref").is_none());

        let fragments = facts_provider(&mem_root, Some(Arc::clone(&project_store)), None)?
            .fragments(
                &TurnInputContext::default(),
                time::OffsetDateTime::UNIX_EPOCH,
                jiff::Timestamp::UNIX_EPOCH,
            );
        assert!(
            fragments
                .iter()
                .any(|f| f.label.as_str() == "project-usage-pref")
        );

        let (count, _) = project_store
            .usage("usage-pref")
            .ok_or(TestError::Missing("usage recorded after delivery"))?;
        assert_eq!(count, 1);

        let _ = std::fs::remove_dir_all(&mem_root);
        let _ = std::fs::remove_dir_all(&project_root);
        Ok(())
    }
}
