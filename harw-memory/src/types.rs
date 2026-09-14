//! Kern-Datentypen des Memory-Subsystems.
//!
//! # Verantwortungsbereich
//! Enum `Tier`, `Signal`, `Entry`, `RecallQuery`, `Stats`, `MaintenanceReport`.
//! Reine Datentypen — keine I/O-Logik.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Speicher-Tier eines Memory-Eintrags.
///
/// # Beschreibung
/// - `Hot` — immer geladen, harte Grenze ≤100 Zeilen.
/// - `Warm` — bei Namespace-/Keyword-Match geladen.
/// - `Cold` — nur auf explizite Anfrage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Immer geladen. Kleines Kontrakt-Budget.
    Hot,
    /// Bedarfsgeladen.
    Warm,
    /// Archiv.
    Cold,
}

impl Tier {
    /// Zeilen-Limit dieses Tiers, `None` für unbegrenzt.
    #[must_use]
    pub const fn max_lines(self) -> Option<usize> {
        match self {
            Self::Hot => Some(100),
            Self::Warm => Some(200),
            Self::Cold => None,
        }
    }

    /// Kurzform für Pfad-Segmente.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hot => "hot",
            Self::Warm => "warm",
            Self::Cold => "cold",
        }
    }
}

/// Ein Signal, das aus dem laufenden Turn gelernt werden soll.
///
/// # Beschreibung
/// Signale werden append-only nach `signals/*.jsonl` geschrieben. Sie werden
/// nicht sofort in die Tiers befördert — die Promotion läuft in `maintain()`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Signal {
    /// Explizite Korrektur des Users.
    Correction {
        /// Der Korrekturtext.
        text: String,
        /// Freier Kontext, z. B. Session-/Turn-ID.
        context: Option<String>,
    },
    /// Selbst-Reflexion nach Aufgabenabschluss.
    Reflection {
        /// Aufgabentyp/Kontext.
        context: String,
        /// Extrahierte Lektion.
        lesson: String,
    },
    /// Kandidat-Muster (wird erst nach ≥3× befördert).
    PatternHint {
        /// Stabiler Schlüssel — dient als Zähler-Anker.
        key: String,
        /// Freie Notiz.
        note: String,
    },
}

impl Signal {
    /// Kanonisches Erkennungslabel für Telemetrie.
    #[must_use]
    pub const fn kind_label(&self) -> &'static str {
        match self {
            Self::Correction { .. } => "correction",
            Self::Reflection { .. } => "reflection",
            Self::PatternHint { .. } => "pattern_hint",
        }
    }
}

/// Zeitstempel-Wrapper (UTC).
///
/// # Beschreibung
/// Alias auf [`OffsetDateTime`] — wir speichern konsistent UTC.
pub type Timestamp = OffsetDateTime;

/// Ein persistenter Memory-Eintrag.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    /// Namespace, z. B. `"project/harwness"` oder `"domain/rust"`.
    pub namespace: String,
    /// Zeilen-Inhalt (Markdown).
    pub content: String,
    /// Tier, aus dem dieser Eintrag stammt.
    pub tier: Tier,
    /// Zeitpunkt des letzten Zugriffs.
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_used: Option<Timestamp>,
    /// Anzahl der Recall-Treffer.
    pub usage_count: u64,
}

/// Recall-Anfrage an das Memory-Backend.
#[derive(Clone, Debug, Default)]
pub struct RecallQuery<'a> {
    /// Optionaler Namespace-Filter (z. B. `"project/harwness"`).
    pub namespace: Option<&'a str>,
    /// Fallende Suchbegriffe (alle müssen als Substring in `Entry.content` auftreten).
    pub keywords: &'a [&'a str],
    /// Wenn `true`, wird auch das COLD-Tier durchsucht.
    pub include_cold: bool,
    /// Maximale Trefferzahl.
    pub limit: usize,
}

/// Ergebnis eines `maintain()`-Laufs.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MaintenanceReport {
    /// Anzahl neu ausgewerteter Signale.
    pub signals_processed: usize,
    /// Anzahl WARM→HOT-Promotions in diesem Lauf.
    pub promoted_to_hot: usize,
    /// Anzahl WARM→COLD-Demotions.
    pub demoted_to_cold: usize,
    /// Anzahl neu erstellter WARM-Einträge.
    pub warm_created: usize,
    /// Ergebnis des Heartbeat-Ticks (Promotion/Demotion/Overflow-Bericht).
    ///
    /// `None`, wenn kein Heartbeat gelaufen ist (z. B. bei Feature-Off-Pfaden).
    /// Ansonsten die Delta-Zahlen aus [`crate::heartbeat::tick`].
    #[serde(default)]
    pub heartbeat: Option<crate::heartbeat::HeartbeatReport>,
    /// Anzahl Fakten, deren `confidence` in diesem Lauf per
    /// [`crate::facts::FactStore::decay`] halbiert wurde (Design
    /// `memory-v3-ltm.md` §5.4).
    ///
    /// `#[serde(default)]`, damit ältere `workflow.json`/Zustandsdateien ohne
    /// dieses Feld weiter lesbar bleiben.
    #[serde(default)]
    pub facts_decayed: usize,
    /// Namen der Fakten, deren `confidence` nach dem Verfall unter `0.2`
    /// gefallen ist. Werden nur gemeldet, nicht automatisch gelöscht (Design
    /// §5.4).
    ///
    /// `#[serde(default)]`, damit ältere Zustandsdateien ohne dieses Feld
    /// weiter lesbar bleiben.
    #[serde(default)]
    pub facts_below_threshold: Vec<String>,
}

/// Kompakte Zähler-Statistik.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Stats {
    /// Zeilenzahl in HOT (harte Obergrenze 100).
    pub hot_lines: usize,
    /// Anzahl WARM-Namespaces.
    pub warm_namespaces: usize,
    /// Gesamtzeilenzahl in WARM.
    pub warm_total_lines: usize,
    /// Anzahl COLD-Namespaces.
    pub cold_namespaces: usize,
    /// Anzahl offener Signale (noch nicht durch `maintain()` gesehen).
    pub pending_signals: usize,
}
