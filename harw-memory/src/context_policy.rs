//! Kontextbudget-Steuerung und Turn-Kontext-Rendering.
//!
//! # Spezifikation
//! Implementiert gemäß `docs/design/memory-v2.md` §7 (Kostenbudget) und
//! `docs/design/model-catalog-v2.md` §4 (ContextPolicy-Enum). Das Kernprinzip
//! stammt aus `philosophy.md` §3: „Kontext muss konstruiert werden, nicht
//! akkumulieren."
//!
//! # Verantwortungsbereich
//! Dieses Modul verbindet STM und LTM zu einem gerenderten Turn-Paket, das vom
//! Harness pro Turn an das Modell übergeben wird. Die Policy-Enum ist eine
//! **lokale Kopie** der gleichnamigen Variante aus `harw-model-catalog`; kein
//! Cross-Crate-Import. Der Aufrufer mapt extern.
//!
//! # Schlüsseltypen
//! - [`ContextPolicy`] — Enum: `TightSelect`, `Balanced`, `BroadContext`.
//! - [`ContextBudget`] — Token-Grenzen pro Policy (const-berechenbar).
//! - [`RenderedContext`] — Ausgabe von [`render_turn_context`].
//! - [`WarmSlice`] — ein WARM-Recall-Treffer in der Ausgabe.
//!
//! # Nebenläufigkeit
//! `render_turn_context` ist zustandslos; die Nebenläufigkeitsgarantien der
//! übergebenen `store`- und `stm`-Referenzen gelten unverändert.
//!
//! # Fehler
//! Gibt [`crate::error::MemoryError`] weiter; eigene Varianten werden nicht
//! eingeführt (Design-Entscheidung: keine Änderung an `error.rs`).
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_memory::context_policy::{ContextPolicy, render_turn_context};
//! use harw_memory::file_store::FileMemoryStore;
//! use harw_memory::short_term::ShortTermMemory;
//!
//! let store = FileMemoryStore::open("/tmp/harw-cp-example").unwrap();
//! let stm = ShortTermMemory::new("session-1", 32, 2048);
//! let ctx = render_turn_context(&store, &stm, ContextPolicy::Balanced, None, &[]).unwrap();
//! assert_eq!(ctx.policy, ContextPolicy::Balanced);
//! ```

use serde::{Deserialize, Serialize};

use crate::error::MemoryResult;
use crate::types::RecallQuery;

// ── ContextPolicy ─────────────────────────────────────────────────────────────

/// Lokale Kopie des Policy-Enums (Spiegel von `harw-model-catalog::runtime::ContextPolicy`).
///
/// # Beschreibung
/// Der Aufrufer (harw-core oder harw-tui) mapped diesen Wert aus dem Modell-
/// Katalog extern auf die lokale Kopie, damit `harw-memory` provider-frei bleibt.
///
/// Werte:
/// - `TightSelect` — aggressive Selektion, kleine Turn-Pakete (≤2 000 Tokens).
/// - `Balanced` — ausgewogenes Fenster (≤6 000 Tokens).
/// - `BroadContext` — große Fenster für Modelle mit breitem Kontextfenster
///   (≤20 000 Tokens).
///
/// # Serde
/// `snake_case` — serialisiert als `"tight_select"`, `"balanced"`, `"broad_context"`.
///
/// # Beispiel
/// ```rust
/// use harw_memory::context_policy::ContextPolicy;
/// let p = ContextPolicy::Balanced;
/// let json = serde_json::to_string(&p).unwrap();
/// assert_eq!(json, r#""balanced""#);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextPolicy {
    /// Aggressive Selektion, häufige Kompaktierung, kleine Turn-Pakete.
    TightSelect,
    /// Ausgewogenes Fenster mit thematischem Bündeln.
    Balanced,
    /// Große kohärente Fenster, seltene Kompaktierung.
    BroadContext,
}

// ── ContextBudget ─────────────────────────────────────────────────────────────

/// Token-Grenzen für eine bestimmte [`ContextPolicy`].
///
/// # Beschreibung
/// Alle Felder sind `const`-berechenbar. Die drei vordefinierten Konstanten
/// (`tight`, `balanced`, `broad`) entsprechen den Werten aus
/// `docs/design/memory-v2.md` §7. `for_policy` wählt deterministisch anhand
/// der Policy.
///
/// # Felder
/// - `max_total_tokens` (`usize`): Harte Obergrenze für den gesamten Kontext.
/// - `hot_percent` (`u8`): Anteil in % für HOT (0..=100).
/// - `stm_percent` (`u8`): Anteil in % für STM (0..=100).
/// - `warm_limit` (`usize`): Maximale Anzahl WARM-Recall-Treffer.
///
/// # Beispiel
/// ```rust
/// use harw_memory::context_policy::{ContextBudget, ContextPolicy};
/// let b = ContextBudget::for_policy(ContextPolicy::TightSelect);
/// assert_eq!(b.max_total_tokens, 2_000);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextBudget {
    /// Harte Obergrenze für den gesamten Turn-Kontext (Tokens).
    pub max_total_tokens: usize,
    /// Reservierter Anteil für HOT-LTM (0..=100 Prozent von `max_total_tokens`).
    pub hot_percent: u8,
    /// Reservierter Anteil für STM (0..=100 Prozent von `max_total_tokens`).
    pub stm_percent: u8,
    /// Maximale Anzahl WARM-Recall-Treffer.
    pub warm_limit: usize,
}

impl ContextBudget {
    /// Budget für aggressive Selektion (≤2 000 Tokens gesamt).
    ///
    /// # Rückgabe
    /// `ContextBudget` mit `max_total_tokens = 2_000`, `hot_percent = 40`,
    /// `stm_percent = 30`, `warm_limit = 3`.
    ///
    /// # Beispiel
    /// ```rust
    /// use harw_memory::context_policy::ContextBudget;
    /// assert_eq!(ContextBudget::tight().max_total_tokens, 2_000);
    /// ```
    #[must_use]
    pub const fn tight() -> Self {
        Self {
            max_total_tokens: 2_000,
            hot_percent: 40,
            stm_percent: 30,
            warm_limit: 3,
        }
    }

    /// Budget für ausgewogenes Fenster (≤6 000 Tokens gesamt).
    ///
    /// # Rückgabe
    /// `ContextBudget` mit `max_total_tokens = 6_000`, `hot_percent = 30`,
    /// `stm_percent = 25`, `warm_limit = 6`.
    ///
    /// # Beispiel
    /// ```rust
    /// use harw_memory::context_policy::ContextBudget;
    /// assert_eq!(ContextBudget::balanced().max_total_tokens, 6_000);
    /// ```
    #[must_use]
    pub const fn balanced() -> Self {
        Self {
            max_total_tokens: 6_000,
            hot_percent: 30,
            stm_percent: 25,
            warm_limit: 6,
        }
    }

    /// Budget für große Kontextfenster (≤20 000 Tokens gesamt).
    ///
    /// # Rückgabe
    /// `ContextBudget` mit `max_total_tokens = 20_000`, `hot_percent = 20`,
    /// `stm_percent = 20`, `warm_limit = 12`.
    ///
    /// # Beispiel
    /// ```rust
    /// use harw_memory::context_policy::ContextBudget;
    /// assert_eq!(ContextBudget::broad().max_total_tokens, 20_000);
    /// ```
    #[must_use]
    pub const fn broad() -> Self {
        Self {
            max_total_tokens: 20_000,
            hot_percent: 20,
            stm_percent: 20,
            warm_limit: 12,
        }
    }

    /// Wählt das Budget deterministisch anhand der Policy.
    ///
    /// # Argumente
    /// - `policy` (`ContextPolicy`): Die gewünschte Policy.
    ///
    /// # Rückgabe
    /// Das passende `ContextBudget` (const-berechenbar).
    ///
    /// # Beispiel
    /// ```rust
    /// use harw_memory::context_policy::{ContextBudget, ContextPolicy};
    /// assert_eq!(
    ///     ContextBudget::for_policy(ContextPolicy::BroadContext),
    ///     ContextBudget::broad()
    /// );
    /// ```
    #[must_use]
    pub const fn for_policy(policy: ContextPolicy) -> Self {
        match policy {
            ContextPolicy::TightSelect => Self::tight(),
            ContextPolicy::Balanced => Self::balanced(),
            ContextPolicy::BroadContext => Self::broad(),
        }
    }
}

// ── WarmSlice ─────────────────────────────────────────────────────────────────

/// Ein WARM-Recall-Treffer im gerenderten Kontext.
///
/// # Beschreibung
/// Enthält den Namespace und den Markdown-Inhalt des WARM-Eintrags. Wird von
/// [`render_turn_context`] aus einem [`crate::types::Entry`] erzeugt.
///
/// # Felder
/// - `namespace` (`String`): Namespace des WARM-Eintrags (z. B. `"domain/rust"`).
/// - `content` (`String`): Markdown-Inhalt des WARM-Eintrags.
///
/// # Beispiel
/// ```rust
/// use harw_memory::context_policy::WarmSlice;
/// let s = WarmSlice { namespace: "domain/rust".to_owned(), content: "tip".to_owned() };
/// assert_eq!(s.namespace, "domain/rust");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WarmSlice {
    /// Namespace des WARM-Eintrags.
    pub namespace: String,
    /// Markdown-Inhalt des WARM-Eintrags.
    pub content: String,
}

// ── RenderedContext ───────────────────────────────────────────────────────────

/// Vollständig gerenderter Turn-Kontext — Ausgabe von [`render_turn_context`].
///
/// # Beschreibung
/// Vereint HOT-LTM, STM und WARM-Recall in ein kompaktes Turn-Paket. Der
/// Aufrufer übergiebt dieses Paket an den Modell-Provider. `token_estimate`
/// ist eine Schätzung via `bytes / 4`; keine exakte LLM-Zählung.
///
/// # Felder
/// - `policy` ([`ContextPolicy`]): Die verwendete Policy.
/// - `hot` (`String`): HOT-LTM-Inhalt (ggf. gekürzt).
/// - `stm` (`String`): Gerenderter STM-Inhalt.
/// - `warm` (`Vec<WarmSlice>`): WARM-Recall-Treffer.
/// - `token_estimate` (`usize`): Geschätzte Gesamttokenanzahl.
///
/// # Beispiel
/// ```rust
/// use harw_memory::context_policy::{ContextPolicy, RenderedContext, WarmSlice};
/// let ctx = RenderedContext {
///     policy: ContextPolicy::Balanced,
///     hot: String::new(),
///     stm: "Frage".to_owned(),
///     warm: vec![],
///     token_estimate: 1,
/// };
/// assert_eq!(ctx.token_estimate, 1);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderedContext {
    /// Verwendete Policy.
    pub policy: ContextPolicy,
    /// HOT-LTM-Inhalt (zeilenweise gekürzt wenn nötig).
    pub hot: String,
    /// Gerenderter STM-Inhalt.
    pub stm: String,
    /// WARM-Recall-Treffer.
    pub warm: Vec<WarmSlice>,
    /// Geschätzte Gesamttokenanzahl (bytes / 4).
    pub token_estimate: usize,
}

// ── render_turn_context ───────────────────────────────────────────────────────

/// Rendert einen vollständigen Turn-Kontext aus STM und LTM gemäß Policy.
///
/// # Beschreibung
/// Implementiert den Kontext-Konstruktions-Algorithmus aus `memory-v2.md` §7
/// und `philosophy.md` §3. Der Ablauf:
///
/// 1. Budget aus Policy ableiten.
/// 2. Token-Kontingente für HOT und STM berechnen.
/// 3. HOT-Tier lesen und zeilenweise vom Ende kürzen, wenn über Limit.
/// 4. STM rendern mit STM-Kontingent.
/// 5. WARM per Namespace/Keyword abrufen (kein COLD).
/// 6. Token-Schätzung berechnen (Summe aller Bytes / 4).
/// 7. [`RenderedContext`] zurückgeben.
///
/// # Argumente
/// - `store` (`&M`): LTM-Backend (implementiert [`crate::store::Memory`]).
/// - `stm` (`&ShortTermMemory`): In-Process Short-Term Memory.
/// - `policy` ([`ContextPolicy`]): Wählt Token-Budgets.
/// - `namespace` (`Option<&str>`): Optionaler Namespace-Filter für WARM-Recall.
/// - `keywords` (`&[&str]`): Keyword-Filter für WARM-Recall (alle müssen matchen).
///
/// # Rückgabe
/// `Ok(RenderedContext)` mit allen drei Abschnitten befüllt.
///
/// # Fehler
/// - [`crate::error::MemoryError::Io`]: Lesefehler in HOT oder WARM.
/// - [`crate::error::MemoryError::TierOverflow`]: HOT-Datei enthält >100 Zeilen
///   **vor** der Kürzung (tritt nicht auf — die Kürzung erfolgt lokal im Rendering,
///   nicht im Store; `store.hot()` gibt TierOverflow zurück wenn der Store selbst
///   kaputt ist).
///
/// # Nebenläufigkeit
/// Zustandslos; Thread-Sicherheit folgt aus `M: Send + Sync` und `ShortTermMemory: Sync`.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_memory::context_policy::{ContextPolicy, render_turn_context};
/// use harw_memory::file_store::FileMemoryStore;
/// use harw_memory::short_term::ShortTermMemory;
///
/// let store = FileMemoryStore::open("/tmp/harw-rend-example").unwrap();
/// let stm   = ShortTermMemory::new("s", 32, 2048);
/// let ctx   = render_turn_context(&store, &stm, ContextPolicy::TightSelect, None, &[]).unwrap();
/// assert!(ctx.token_estimate == 0 || ctx.token_estimate > 0);
/// ```
pub fn render_turn_context<M: crate::store::Memory>(
    store: &M,
    stm: &crate::short_term::ShortTermMemory,
    policy: ContextPolicy,
    namespace: Option<&str>,
    keywords: &[&str],
) -> MemoryResult<RenderedContext> {
    let budget = ContextBudget::for_policy(policy);

    // Schritt 1–2: Token-Kontingente berechnen.
    let hot_tokens_max = budget.max_total_tokens * (budget.hot_percent as usize) / 100;
    let stm_tokens_max = budget.max_total_tokens * (budget.stm_percent as usize) / 100;

    // Schritt 3: HOT laden und zeilenweise vom Ende kürzen.
    let raw_hot = store.hot()?;
    let hot = truncate_hot_to_tokens(&raw_hot, hot_tokens_max);

    // Schritt 4: STM rendern.
    let stm_rendered = stm.render(stm_tokens_max);

    // Schritt 5: WARM abrufen (kein COLD).
    let query = RecallQuery {
        namespace,
        keywords,
        include_cold: false,
        limit: budget.warm_limit,
    };
    let warm_entries = store.recall(query)?;
    let warm: Vec<WarmSlice> = warm_entries
        .into_iter()
        .map(|e| WarmSlice {
            namespace: e.namespace,
            content: e.content,
        })
        .collect();

    // Schritt 6: Token-Schätzung (Summe bytes / 4 über alle drei Bereiche).
    let hot_estimate = hot.len() / 4;
    let stm_estimate = stm_rendered.len() / 4;
    let warm_estimate: usize = warm.iter().map(|s| s.content.len() / 4).sum();
    let token_estimate = hot_estimate + stm_estimate + warm_estimate;

    Ok(RenderedContext {
        policy,
        hot,
        stm: stm_rendered,
        warm,
        token_estimate,
    })
}

// ── Hilfsfunktionen ───────────────────────────────────────────────────────────

/// Kürzt einen HOT-String zeilenweise vom Ende, bis er unter `max_tokens` fällt.
///
/// # Beschreibung
/// Token-Schätzung: `bytes / 4`. Entfernt solange die letzte Zeile, bis das
/// Ergebnis unter `max_tokens` liegt oder keine Zeilen mehr vorhanden sind.
/// Leerzeilen am Ende werden mitgezählt.
///
/// # Argumente
/// - `raw` (`&str`): Ungekürzter HOT-Inhalt.
/// - `max_tokens` (`usize`): Maximale Token-Anzahl (Schätzung).
///
/// # Rückgabe
/// Gekürzter String; behält abschließendes `\n` wenn Zeilen verbleiben.
fn truncate_hot_to_tokens(raw: &str, max_tokens: usize) -> String {
    if raw.len() / 4 <= max_tokens {
        return raw.to_owned();
    }
    // Sammle Zeilen und entferne von hinten, bis Budget eingehalten wird.
    let mut lines: Vec<&str> = raw.lines().collect();
    while !lines.is_empty() {
        // Baue Kandidaten-String aus verbleibenden Zeilen.
        let candidate_bytes: usize = lines.iter().map(|l| l.len() + 1).sum(); // +1 für '\n'
        if candidate_bytes / 4 <= max_tokens {
            break;
        }
        lines.pop();
    }
    if lines.is_empty() {
        return String::new();
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_store::FileMemoryStore;
    use crate::short_term::{ShortTermMemory, StmRole};
    use crate::test_support::{TestResult, ctx};

    /// Erzeugt ein eindeutiges temporäres Verzeichnis.
    fn tmp_root(tag: &str) -> std::path::PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("harw-cp-{}-{}-{}", tag, std::process::id(), id));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    /// Erzeugt einen `FileMemoryStore` in einem frischen Temp-Verzeichnis.
    fn open_store(tag: &str) -> TestResult<(FileMemoryStore, std::path::PathBuf)> {
        let root = tmp_root(tag);
        let store = FileMemoryStore::open(&root).map_err(ctx("open store"))?;
        Ok((store, root))
    }

    // ── Test 1: Budgets skalieren mit Policy ──────────────────────────────────

    /// Prüft, dass `max_total_tokens` monoton mit der Policy wächst:
    /// TightSelect < Balanced < BroadContext.
    ///
    /// Spezifikation: `memory-v2.md` §7.
    #[test]
    fn budgets_scale_with_policy() {
        let tight = ContextBudget::for_policy(ContextPolicy::TightSelect);
        let balanced = ContextBudget::for_policy(ContextPolicy::Balanced);
        let broad = ContextBudget::for_policy(ContextPolicy::BroadContext);
        assert!(
            tight.max_total_tokens < balanced.max_total_tokens,
            "TightSelect ({}) must be < Balanced ({})",
            tight.max_total_tokens,
            balanced.max_total_tokens
        );
        assert!(
            balanced.max_total_tokens < broad.max_total_tokens,
            "Balanced ({}) must be < BroadContext ({})",
            balanced.max_total_tokens,
            broad.max_total_tokens
        );
    }

    // ── Test 2: Render liefert alle drei Abschnitte ───────────────────────────

    /// Prüft, dass HOT, STM und WARM nach dem Rendern nicht-leer sind,
    /// wenn entsprechende Daten vorliegen.
    ///
    /// Spezifikation: `render_turn_context` Schritt 3–6.
    #[test]
    fn render_returns_all_three_sections() -> TestResult {
        let (store, root) = open_store("all-sections")?;

        // HOT: Datei schreiben.
        std::fs::write(root.join("HOT.md"), "Regel A: keine Emojis.\n")
            .map_err(ctx("HOT.md schreiben"))?;

        // WARM: Namespace-Datei schreiben.
        std::fs::create_dir_all(root.join("warm/project")).map_err(ctx("warm/project anlegen"))?;
        std::fs::write(
            root.join("warm/project/harwness.md"),
            "Harwness-Projektnotiz.\n",
        )
        .map_err(ctx("warm-Datei schreiben"))?;

        // STM: einen Eintrag pushen.
        let stm = ShortTermMemory::new("s-all", 32, 2048);
        stm.push(StmRole::User, 80, "Wie geht das?");

        let turn_ctx = render_turn_context(&store, &stm, ContextPolicy::Balanced, None, &[])
            .map_err(ctx("render"))?;

        assert!(!turn_ctx.hot.is_empty(), "hot must not be empty");
        assert!(!turn_ctx.stm.is_empty(), "stm must not be empty");
        assert!(!turn_ctx.warm.is_empty(), "warm must not be empty");

        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    // ── Test 3: WARM-Limit wird respektiert ───────────────────────────────────

    /// Prüft, dass bei TightSelect (warm_limit=3) maximal 3 WARM-Treffer
    /// zurückgegeben werden, auch wenn 5 Namespaces vorhanden sind.
    ///
    /// Spezifikation: `ContextBudget::tight().warm_limit == 3`.
    #[test]
    fn render_respects_warm_limit() -> TestResult {
        let (store, root) = open_store("warm-limit")?;

        // 5 WARM-Namespaces anlegen.
        for i in 0..5usize {
            std::fs::write(root.join(format!("warm/ns{i}.md")), format!("Inhalt {i}\n"))
                .map_err(ctx("warm-Datei schreiben"))?;
        }

        let stm = ShortTermMemory::new("s-wl", 32, 2048);

        let turn_ctx = render_turn_context(
            &store,
            &stm,
            ContextPolicy::TightSelect, // warm_limit = 3
            None,
            &[],
        )
        .map_err(ctx("render"))?;

        assert_eq!(
            turn_ctx.warm.len(),
            3,
            "TightSelect warm_limit=3 must cap warm at 3; got {}",
            turn_ctx.warm.len()
        );

        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    // ── Test 4: HOT wird gekürzt wenn zu groß ─────────────────────────────────

    /// Prüft, dass HOT-Inhalt bei TightSelect zeilenweise vom Ende gekürzt wird,
    /// wenn er das Token-Budget überschreitet.
    ///
    /// Der Store begrenzt HOT auf ≤100 Zeilen (TierOverflow). Das Test-Szenario
    /// schreibt genau 100 Zeilen à 40 Bytes (≈1 000 Tokens) bei einem Budget
    /// von 800 Tokens (TightSelect: 2 000 × 40 % = 800).
    ///
    /// Spezifikation: `truncate_hot_to_tokens`, Schritt 3.
    #[test]
    fn render_hot_truncates_when_too_large() -> TestResult {
        let (store, root) = open_store("hot-truncate")?;

        // 100 Zeilen à 40 Zeichen → 4 000 Bytes → ~1 000 Token-Schätzung.
        // TightSelect hot_tokens_max = 2 000 * 40 / 100 = 800 Tokens → Kürzung nötig.
        let content: String = (0..100)
            .map(|i| format!("Zeile{:03}: {}\n", i, "x".repeat(30)))
            .collect();
        std::fs::write(root.join("HOT.md"), &content).map_err(ctx("HOT.md schreiben"))?;

        let stm = ShortTermMemory::new("s-ht", 32, 2048);

        let turn_ctx = render_turn_context(&store, &stm, ContextPolicy::TightSelect, None, &[])
            .map_err(ctx("render"))?;

        let original_len = content.len();
        let rendered_len = turn_ctx.hot.len();

        assert!(
            rendered_len < original_len,
            "truncated hot ({rendered_len} bytes) must be smaller than original ({original_len} bytes)"
        );

        // Token-Schätzung muss unter hot_tokens_max liegen.
        let hot_tokens_max = 2_000 * 40 / 100; // = 800
        let rendered_tokens = turn_ctx.hot.len() / 4;
        assert!(
            rendered_tokens <= hot_tokens_max,
            "hot token estimate {rendered_tokens} must be <= {hot_tokens_max}"
        );

        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    // ── Test 5: Leerer Store → leerer Kontext ─────────────────────────────────

    /// Prüft, dass ein leerer Store + leere STM einen `token_estimate == 0`
    /// liefern.
    ///
    /// Spezifikation: Schritt 6.
    #[test]
    fn render_empty_store_produces_empty_context() -> TestResult {
        let (store, root) = open_store("empty")?;
        let stm = ShortTermMemory::new("s-empty", 32, 2048);

        let turn_ctx = render_turn_context(&store, &stm, ContextPolicy::Balanced, None, &[])
            .map_err(ctx("render"))?;

        assert_eq!(
            turn_ctx.token_estimate, 0,
            "empty store + empty STM must yield token_estimate 0"
        );
        assert!(turn_ctx.hot.is_empty());
        assert!(turn_ctx.stm.is_empty());
        assert!(turn_ctx.warm.is_empty());

        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    // ── Test 6: Serde snake_case ──────────────────────────────────────────────

    /// Prüft, dass `ContextPolicy::BroadContext` als `"broad_context"` serialisiert.
    ///
    /// Spezifikation: `#[serde(rename_all = "snake_case")]`.
    #[test]
    fn context_policy_serde_snake_case() -> TestResult {
        let json = serde_json::to_string(&ContextPolicy::BroadContext)
            .map_err(ctx("serialize BroadContext"))?;
        assert_eq!(json, r#""broad_context""#);

        let back: ContextPolicy =
            serde_json::from_str(&json).map_err(ctx("deserialize BroadContext"))?;
        assert_eq!(back, ContextPolicy::BroadContext);

        // Auch die anderen Varianten prüfen.
        assert_eq!(
            serde_json::to_string(&ContextPolicy::TightSelect)
                .map_err(ctx("serialize TightSelect"))?,
            r#""tight_select""#
        );
        assert_eq!(
            serde_json::to_string(&ContextPolicy::Balanced).map_err(ctx("serialize Balanced"))?,
            r#""balanced""#
        );
        Ok(())
    }

    // ── Test 7: for_policy stimmt mit Konstanten überein ─────────────────────

    /// Prüft, dass `ContextBudget::for_policy` exakt dieselben Werte liefert
    /// wie die jeweiligen Konstanten.
    ///
    /// Spezifikation: `ContextBudget::for_policy`.
    #[test]
    fn budget_for_policy_matches_constants() {
        assert_eq!(
            ContextBudget::for_policy(ContextPolicy::TightSelect),
            ContextBudget::tight()
        );
        assert_eq!(
            ContextBudget::for_policy(ContextPolicy::Balanced),
            ContextBudget::balanced()
        );
        assert_eq!(
            ContextBudget::for_policy(ContextPolicy::BroadContext),
            ContextBudget::broad()
        );
    }
}
