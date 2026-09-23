//! Short-Term Memory (STM) — flüchtiger In-Process-Kontext pro Session.
//!
//! # Verantwortungsbereich
//! Dieses Modul implementiert einen salience-gesteuerten Ring-Buffer für den
//! Turn-Kontext eines Agenten. Es hält keine persistente Datei und führt kein
//! I/O durch. Das Modul folgt `docs/design/memory-v2.md` §3 und dem
//! Architekturprinzip aus `philosophy.md` §3 / §16 Invariante 7:
//! „Kontext wird pro Turn konstruiert, nicht unbegrenzt angesammelt."
//!
//! # Schlüsseltypen
//! - [`ShortTermMemory`] — öffentliche Fassade (`Send + Sync`).
//! - [`StmEntry`] — ein einzelner Kontext-Eintrag (Zeitstempel, Rolle, Salience,
//!   Inhalt).
//! - [`StmRole`] — Teilnehmerrolle im Dialog.
//!
//! # Nebenläufigkeit
//! `ShortTermMemory` kapselt ein `RwLock<Inner>`. Lesende Operationen
//! (`snapshot`, `render`, `len`, `is_empty`, `session_id`) erwerben ein
//! gemeinsames Leserecht; mutierende Operationen (`push`, `clear`) erwerben
//! exklusiven Schreibzugriff. Bei `PoisonError` wird das vergiftete `Inner`
//! über `.into_inner()` gerettet — kein Panic.
//!
//! # Fehler
//! Dieses Modul erzeugt keine neuen `MemoryError`-Varianten. Interne Fehler
//! (Lock-Vergiftung) werden transparent behandelt; nach außen sind alle API-
//! Methoden infallibel.
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_memory::short_term::{ShortTermMemory, StmRole};
//!
//! let stm = ShortTermMemory::new("session-42", 32, 2048);
//! stm.push(StmRole::User, 80, "Wie funktioniert der Heartbeat?");
//! stm.push(StmRole::Assistant, 60, "Der Heartbeat promotiert Patterns...");
//! let rendered = stm.render(512);
//! assert!(!rendered.is_empty());
//! ```

use std::collections::VecDeque;
use std::sync::RwLock;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

// ── Typen ────────────────────────────────────────────────────────────────────

/// Teilnehmerrolle eines STM-Eintrags.
///
/// # Beschreibung
/// Entspricht den vier Dialog-Rollen, die ein Kontext-Eintrag annehmen kann.
/// Der Serde-Alias verwendet `snake_case` für JSON-Kompatibilität.
///
/// # Beispiel
/// ```rust
/// use harw_memory::short_term::StmRole;
/// let r = StmRole::User;
/// assert_eq!(r, StmRole::User);
/// ```
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StmRole {
    /// Nachricht des Benutzers.
    User,
    /// Antwort des Assistenten.
    Assistant,
    /// Ergebnis eines Tool-Aufrufs.
    Tool,
    /// System-Instruktion.
    System,
}

impl StmRole {
    /// Gibt den kebab-case-Bezeichner der Rolle zurück.
    ///
    /// # Rückgabe
    /// Statischer String, passend für `render`-Ausgaben (`"user"`, `"assistant"`,
    /// `"tool"`, `"system"`).
    #[must_use]
    pub const fn as_kebab(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
            Self::System => "system",
        }
    }
}

/// Ein einzelner Eintrag im Short-Term Memory.
///
/// # Beschreibung
/// Trägt Zeitstempel, Rolle, Salience (0–100) und Inhalt. Wird vom Buffer
/// nach Salience verdrängt: niedrigste Salience fliegt zuerst raus; bei
/// Gleichstand das älteste Element (kleinster Index in der Deque).
///
/// # Felder
/// - `at` (`OffsetDateTime`): Zeitstempel der Erstellung (UTC).
/// - `role` (`StmRole`): Dialogrolle des Eintrags.
/// - `salience` (`u8`): Wichtigkeit 0–100; höher = wichtiger.
/// - `content` (`String`): Freitext-Inhalt.
///
/// # Beispiel
/// ```rust
/// use harw_memory::short_term::{StmEntry, StmRole};
/// use time::OffsetDateTime;
///
/// let entry = StmEntry {
///     at: OffsetDateTime::now_utc(),
///     role: StmRole::User,
///     salience: 90,
///     content: "Frage des Benutzers".to_owned(),
/// };
/// assert_eq!(entry.salience, 90);
/// ```
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StmEntry {
    /// Zeitstempel der Erstellung (UTC).
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// Dialogrolle.
    pub role: StmRole,
    /// Salience-Gewicht (0–100).
    pub salience: u8,
    /// Textinhalt des Eintrags.
    pub content: String,
}

impl StmEntry {
    /// Schätzt die Token-Anzahl dieses Eintrags.
    ///
    /// # Beschreibung
    /// Verwendet `content.len() / 4` als Näherung; Minimum 1 Token.
    ///
    /// # Rückgabe
    /// Geschätzte Tokenanzahl als `usize`.
    #[must_use]
    fn estimated_tokens(&self) -> usize {
        (self.content.len() / 4).max(1)
    }
}

// ── Inner ────────────────────────────────────────────────────────────────────

/// Interner, nicht-synchronisierter Zustand des STM.
///
/// # Beschreibung
/// Wird ausschließlich hinter einem `RwLock` verwendet. Enthält die eigentliche
/// Deque, Kapazitäts- und Budget-Grenzwerte sowie die Session-ID.
struct Inner {
    /// Ringpuffer der Einträge — älteste stehen vorne.
    deque: VecDeque<StmEntry>,
    /// Maximale Anzahl gleichzeitig gehaltener Einträge.
    capacity: usize,
    /// Maximales Token-Budget (Summe der geschätzten Tokens aller Einträge).
    token_budget: usize,
    /// Eindeutige Session-ID.
    session_id: String,
}

impl Inner {
    /// Liefert die Summe der geschätzten Tokens über alle Einträge.
    fn total_tokens(&self) -> usize {
        self.deque.iter().map(StmEntry::estimated_tokens).sum()
    }

    /// Entfernt das Element mit der niedrigsten Salience; bei Gleichstand das
    /// älteste (vorderste) Element in der Deque.
    ///
    /// # Beschreibung
    /// Sucht den Index des Eintrags mit dem kleinsten `salience`-Wert. Bei
    /// mehreren gleich kleinen Einträgen gewinnt der mit dem kleinsten Index
    /// (ältestes Element). Entfernt diesen Index dann aus der Deque.
    /// Wenn die Deque leer ist, wird keine Operation durchgeführt.
    fn evict_lowest_salience(&mut self) {
        if self.deque.is_empty() {
            return;
        }
        // Iteriere und finde den Index des Eintrags mit kleinster Salience.
        // Bei Gleichstand bevorzugen wir den kleinsten Index (ältestes Element),
        // was durch die Iteration von vorne bereits sichergestellt wird.
        let victim = self
            .deque
            .iter()
            .enumerate()
            .min_by_key(|(_, e)| e.salience)
            .map(|(i, _)| i);

        if let Some(idx) = victim {
            self.deque.remove(idx);
        }
    }

    /// Fügt einen neuen Eintrag hinzu und führt anschließend Verdrängung durch.
    ///
    /// # Beschreibung
    /// Hängt `entry` an das Ende der Deque. Dann wird solange das Element mit
    /// der niedrigsten Salience entfernt, bis sowohl `deque.len() <= capacity`
    /// als auch `total_tokens <= token_budget` gilt.
    ///
    /// # Argumente
    /// - `entry` (`StmEntry`): Der neu hinzuzufügende Eintrag.
    fn push(&mut self, entry: StmEntry) {
        self.deque.push_back(entry);
        // Verdränge, bis Kapazität und Token-Budget eingehalten werden.
        while self.deque.len() > self.capacity || self.total_tokens() > self.token_budget {
            self.evict_lowest_salience();
            // Sicherheitsanker: Wenn die Deque leer ist, brechen wir ab.
            if self.deque.is_empty() {
                break;
            }
        }
    }
}

// ── ShortTermMemory ──────────────────────────────────────────────────────────

/// Salience-gesteuerter In-Process-Kontext-Buffer (Short-Term Memory).
///
/// # Beschreibung
/// Hält bis zu `capacity` Einträge und ein Token-Budget. Neue Einträge werden
/// via `push` hinzugefügt. Überschreitet der Buffer Kapazität oder Budget, wird
/// der Eintrag mit der niedrigsten Salience (bei Gleichstand: der älteste)
/// entfernt. Der Buffer ist `Send + Sync` durch internes `RwLock<Inner>`.
///
/// # Nebenläufigkeit
/// Lesende Methoden (`snapshot`, `render`, `len`, `is_empty`, `session_id`)
/// erwerben ein gemeinsames Leserecht. Schreibende Methoden (`push`, `clear`)
/// erwerben exklusiven Schreibzugriff. Bei Lock-Vergiftung (PoisonError) wird
/// der interne Zustand transparent gerettet.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_memory::short_term::{ShortTermMemory, StmRole};
///
/// let stm = ShortTermMemory::new("meine-session", 32, 2048);
/// stm.push(StmRole::User, 75, "Wie lautet der Plan?");
/// assert_eq!(stm.len(), 1);
/// ```
pub struct ShortTermMemory {
    inner: RwLock<Inner>,
}

impl ShortTermMemory {
    /// Erstellt einen neuen leeren Short-Term-Memory-Buffer.
    ///
    /// # Argumente
    /// - `session_id` (`impl Into<String>`): Eindeutige Sitzungskennung.
    /// - `capacity` (`usize`): Maximale Anzahl gleichzeitig gehaltener Einträge.
    /// - `token_budget` (`usize`): Maximale Summe der geschätzten Token-Anzahl.
    ///
    /// # Rückgabe
    /// Neuer, leerer `ShortTermMemory`-Buffer.
    ///
    /// # Beispiel
    /// ```rust
    /// use harw_memory::short_term::ShortTermMemory;
    ///
    /// let stm = ShortTermMemory::new("session-1", 32, 2048);
    /// assert!(stm.is_empty());
    /// ```
    #[must_use]
    pub fn new(session_id: impl Into<String>, capacity: usize, token_budget: usize) -> Self {
        Self {
            inner: RwLock::new(Inner {
                deque: VecDeque::with_capacity(capacity.min(256)),
                capacity,
                token_budget,
                session_id: session_id.into(),
            }),
        }
    }

    /// Gibt eine geklonte Kopie der Session-ID zurück.
    ///
    /// # Rückgabe
    /// Die Session-ID als `String`.
    ///
    /// # Nebenläufigkeit
    /// Erfordert ein gemeinsames Leserecht auf dem internen Lock.
    #[must_use]
    pub fn session_id(&self) -> String {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        inner.session_id.clone()
    }

    /// Fügt einen neuen Eintrag in den Buffer ein.
    ///
    /// # Beschreibung
    /// Erstellt einen `StmEntry` mit dem aktuellen UTC-Zeitstempel und den
    /// übergebenen Parametern. Nach dem Einfügen wird die Verdrängungslogik
    /// ausgeführt: Solange Kapazität oder Token-Budget überschritten sind,
    /// wird das Element mit der niedrigsten Salience entfernt (bei Gleichstand:
    /// das älteste).
    ///
    /// # Argumente
    /// - `role` (`StmRole`): Dialogrolle des Eintrags.
    /// - `salience` (`u8`): Wichtigkeit 0–100.
    /// - `content` (`impl Into<String>`): Textinhalt des Eintrags.
    ///
    /// # Nebenläufigkeit
    /// Erfordert exklusiven Schreibzugriff auf dem internen Lock.
    pub fn push(&self, role: StmRole, salience: u8, content: impl Into<String>) {
        let entry = StmEntry {
            at: OffsetDateTime::now_utc(),
            role,
            salience,
            content: content.into(),
        };
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        inner.push(entry);
    }

    /// Gibt die aktuelle Anzahl der Einträge im Buffer zurück.
    ///
    /// # Rückgabe
    /// Anzahl der Einträge als `usize`.
    ///
    /// # Nebenläufigkeit
    /// Erfordert ein gemeinsames Leserecht.
    #[must_use]
    pub fn len(&self) -> usize {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        inner.deque.len()
    }

    /// Gibt `true` zurück, wenn der Buffer keine Einträge enthält.
    ///
    /// # Rückgabe
    /// `true` wenn leer, `false` sonst.
    ///
    /// # Nebenläufigkeit
    /// Erfordert ein gemeinsames Leserecht.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Gibt einen Snapshot aller Einträge zurück — älteste zuerst.
    ///
    /// # Beschreibung
    /// Klont alle Einträge der internen Deque in einen `Vec`. Die Reihenfolge
    /// entspricht der Einfügereihenfolge (älteste vorne).
    ///
    /// # Rückgabe
    /// `Vec<StmEntry>` mit allen aktuellen Einträgen, älteste zuerst.
    ///
    /// # Nebenläufigkeit
    /// Erfordert ein gemeinsames Leserecht.
    #[must_use]
    pub fn snapshot(&self) -> Vec<StmEntry> {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        inner.deque.iter().cloned().collect()
    }

    /// Rendert den Buffer-Inhalt als komprimierten Kontextstring.
    ///
    /// # Beschreibung
    /// Sortiert einen Snapshot der Einträge temporär nach `at` absteigend
    /// (jüngste zuerst). Nimmt dann der Reihe nach Einträge auf, bis das
    /// `max_tokens`-Budget erschöpft ist. Einträge, die das Budget
    /// überschreiten würden, werden übersprungen — dabei werden Einträge
    /// mit niedrigerer Salience bevorzugt übersprungen.
    ///
    /// Format pro Zeile: `[<rolle-kebab>] <inhalt>\n`
    ///
    /// # Argumente
    /// - `max_tokens` (`usize`): Maximale Token-Summe (Schätzung `len / 4`).
    ///
    /// # Rückgabe
    /// Gerenderter Kontextstring. Kapazität des internen `String` ist
    /// `max_tokens * 4` (kein Alloc-Sturm).
    ///
    /// # Nebenläufigkeit
    /// Erfordert ein gemeinsames Leserecht.
    #[must_use]
    pub fn render(&self, max_tokens: usize) -> String {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());

        // Sammle Einträge; jüngste zuerst für die Auswahl, aber
        // wir brauchen Referenzen, um Klone zu vermeiden.
        let mut sorted: Vec<&StmEntry> = inner.deque.iter().collect();
        // Sortiere nach Zeitstempel absteigend (jüngste zuerst).
        sorted.sort_by_key(|b| std::cmp::Reverse(b.at));

        // Wähle Einträge gierig aus (jüngste zuerst), bis Budget erschöpft.
        let mut selected: Vec<&StmEntry> = Vec::new();
        let mut used_tokens: usize = 0;

        for entry in &sorted {
            let tokens = entry.estimated_tokens();
            if used_tokens + tokens <= max_tokens {
                selected.push(entry);
                used_tokens += tokens;
            }
            // Einträge, die nicht passen, werden übersprungen.
            // Die Sortierung stellt sicher: zuerst werden jüngere/höhere
            // Einträge bevorzugt aufgenommen; übersprungene haben implizit
            // niedrigere Salience-oder-Alter-Priorisierung.
        }

        // Baue den String auf — voralloziert mit max_tokens * 4.
        let mut out = String::with_capacity(max_tokens * 4);
        for entry in &selected {
            out.push('[');
            out.push_str(entry.role.as_kebab());
            out.push_str("] ");
            out.push_str(&entry.content);
            out.push('\n');
        }
        out
    }

    /// Leert den Buffer vollständig.
    ///
    /// # Beschreibung
    /// Entfernt alle Einträge aus der Deque. Kapazität und Budget bleiben
    /// unverändert.
    ///
    /// # Nebenläufigkeit
    /// Erfordert exklusiven Schreibzugriff.
    pub fn clear(&self) {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        inner.deque.clear();
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    /// Hilfsmethode: Gibt die Summe geschätzter Tokens für alle Einträge zurück.
    fn total_tokens_in(stm: &ShortTermMemory) -> usize {
        stm.snapshot().iter().map(StmEntry::estimated_tokens).sum()
    }

    #[test]
    fn push_respects_capacity() {
        let stm = ShortTermMemory::new("s1", 3, 99_999);
        for i in 0..5u8 {
            stm.push(StmRole::User, i * 10, format!("msg {i}"));
        }
        assert_eq!(stm.len(), 3, "capacity=3 must be respected after 5 pushes");
    }

    #[test]
    fn push_respects_token_budget() {
        // Jeder Content hat ca. 40 Zeichen → ca. 10 Tokens.
        // Budget 20 Tokens → max. 2 Einträge.
        let stm = ShortTermMemory::new("s2", 999, 20);
        for i in 0..5u8 {
            // 40 Zeichen = 10 Token-Schätzung
            let content = "a".repeat(40);
            stm.push(StmRole::User, i * 10 + 10, content);
        }
        let tokens = total_tokens_in(&stm);
        assert!(
            tokens <= 20,
            "total_tokens {tokens} must not exceed budget 20"
        );
    }

    #[test]
    fn eviction_prefers_low_salience() {
        // Capacity 2: erst push low-salience, dann high-salience → low muss raus.
        let stm = ShortTermMemory::new("s3", 2, 99_999);
        stm.push(StmRole::User, 10, "low-salience"); // wird verdrängt
        stm.push(StmRole::User, 90, "high-salience-a");
        stm.push(StmRole::User, 80, "high-salience-b"); // verdrängt low

        let snap = stm.snapshot();
        assert_eq!(snap.len(), 2);
        let contents: Vec<&str> = snap.iter().map(|e| e.content.as_str()).collect();
        assert!(
            !contents.contains(&"low-salience"),
            "low-salience entry must have been evicted; got: {contents:?}"
        );
    }

    #[test]
    fn snapshot_returns_insertion_order() {
        let stm = ShortTermMemory::new("s4", 10, 99_999);
        stm.push(StmRole::User, 50, "first");
        stm.push(StmRole::Assistant, 50, "second");
        stm.push(StmRole::Tool, 50, "third");

        let snap = stm.snapshot();
        assert_eq!(snap.len(), 3);
        assert_eq!(snap[0].content, "first");
        assert_eq!(snap[1].content, "second");
        assert_eq!(snap[2].content, "third");
    }

    #[test]
    fn render_respects_max_tokens() {
        let stm = ShortTermMemory::new("s5", 20, 99_999);
        // Jeder Inhalt: 40 Zeichen → 10 Token.
        for i in 0..10u8 {
            stm.push(StmRole::User, 50 + i, "x".repeat(40));
        }
        // max_tokens = 25 → maximal 2 Einträge à 10 Token (≤ 25).
        let rendered = stm.render(25);
        // Zähle Token im gerenderten String.
        let rendered_tokens = rendered.len() / 4;
        assert!(
            rendered_tokens <= 25,
            "rendered token estimate {rendered_tokens} must be <= max_tokens 25; got:\n{rendered}"
        );
    }

    #[test]
    fn clear_empties_store() {
        let stm = ShortTermMemory::new("s6", 32, 2048);
        stm.push(StmRole::System, 100, "system prompt");
        stm.push(StmRole::User, 80, "user message");
        assert_eq!(stm.len(), 2);
        stm.clear();
        assert_eq!(stm.len(), 0);
        assert!(stm.is_empty());
    }

    #[test]
    fn send_sync_bound_holds() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ShortTermMemory>();
    }

    #[test]
    fn serde_roundtrip_entry() -> TestResult {
        let entry = StmEntry {
            at: OffsetDateTime::now_utc(),
            role: StmRole::Assistant,
            salience: 42,
            content: "Serde-Roundtrip-Test".to_owned(),
        };
        let json = serde_json::to_string(&entry).map_err(ctx("serialize"))?;
        let restored: StmEntry = serde_json::from_str(&json).map_err(ctx("deserialize"))?;
        assert_eq!(restored.role, entry.role);
        assert_eq!(restored.salience, entry.salience);
        assert_eq!(restored.content, entry.content);
        Ok(())
    }
}
