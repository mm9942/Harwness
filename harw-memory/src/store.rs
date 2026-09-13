//! Backend-agnostischer `Memory`-Trait.
//!
//! # Verantwortungsbereich
//! Definiert den Vertrag zwischen dem Rest der Harness und einer beliebigen
//! Memory-Implementierung. Der Standard-Backend ist [`crate::file_store::FileMemoryStore`].

use crate::error::MemoryResult;
use crate::types::{Entry, MaintenanceReport, RecallQuery, Signal, Stats};

/// Persistente, tokensparsame Memory-Schicht.
///
/// # Beschreibung
/// Die Schicht ist nach Tiers gestaffelt (HOT/WARM/COLD). `hot()` ist die
/// einzige Methode, die auf jedem Turn aufgerufen werden sollte — sie ist
/// garantiert kleiner als 100 Zeilen und darf synchron gelesen werden.
/// `recall()`, `record()` und `maintain()` sind Bedarfsoperationen.
///
/// # Nebenläufigkeit
/// Implementierungen müssen `Send + Sync` sein. Sie dürfen intern serialisieren,
/// müssen aber keine sitzungsübergreifende Konsistenz garantieren — außer dass
/// jeder Aufruf einzeln atomar ist.
///
/// # Fehler
/// Alle Methoden geben [`crate::error::MemoryError`] zurück, siehe dort.
pub trait Memory: Send + Sync {
    /// HOT-Tier als Rohtext (`§`-delimitiert, Markdown), ≤100 Zeilen.
    ///
    /// # Rückgabe
    /// - `Ok(String)` — kann leer sein, wenn kein HOT existiert.
    ///
    /// # Fehler
    /// - `MemoryError::Io` beim Lesen der `HOT.md`.
    /// - `MemoryError::TierOverflow`, wenn die Datei existiert, aber >100 Zeilen hat.
    fn hot(&self) -> MemoryResult<String>;

    /// WARM/COLD durchsuchen.
    ///
    /// # Argumente
    /// - `query` — Namespace-/Keyword-Filter (siehe [`RecallQuery`]).
    ///
    /// # Rückgabe
    /// Alle Treffer bis `query.limit`; leere Liste, wenn nichts matcht.
    fn recall<'a>(&self, query: RecallQuery<'a>) -> MemoryResult<Vec<Entry>>;

    /// Hängt ein Signal an das Signal-Log an (append-only).
    ///
    /// # Argumente
    /// - `signal` — das zu protokollierende Signal.
    ///
    /// # Fehler
    /// - `MemoryError::Io`, `MemoryError::Serde`.
    fn record(&self, signal: Signal) -> MemoryResult<()>;

    /// Wartungslauf: verarbeitet offene Signale, wertet Promotion/Decay aus.
    ///
    /// # Rückgabe
    /// Ein [`MaintenanceReport`] mit den ausgeführten Änderungen.
    ///
    /// # Nebenläufigkeit
    /// Läuft unter einem serialisierenden Lock. Bei Contention gibt die
    /// Implementierung `MemoryError::LockContention` zurück.
    fn maintain(&self) -> MemoryResult<MaintenanceReport>;

    /// Zähler-Statistik ohne Content-Load.
    fn stats(&self) -> MemoryResult<Stats>;
}
