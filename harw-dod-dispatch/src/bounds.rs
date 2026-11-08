//! Absolute Grenzen eines persistierten Dispatch-Zustands (PL-100 WP2.1,
//! Review 4505196829): die Höchstwerte der Konfiguration und
//! [`MAX_STATE_BYTES`].
//!
//! # Zweck
//! Ein persistierter [`crate::DispatchState`] ist untrusted: wer die Datei
//! schreiben kann, wählt Größe und Inhalt. Laden und Prüfen dürfen deshalb
//! nie Arbeit oder Speicher proportional zu einer frei gewählten Größe
//! kosten. Jede Tabelle des Zustands hat einen **absoluten Höchstwert**: den
//! größten, den irgendeine gültige Konfiguration zulässt
//! ([`crate::DispatchConfig::validate`] prüft gegen genau diese Konstanten),
//! bzw. eine feste Grenze der Crate ([`crate::MAX_PENDING_ASKS`],
//! [`crate::MAX_STARVED_EPISODES`], [`crate::MAX_JOB_MEMBERS`],
//! [`crate::MAX_ENTRIES_PER_CGROUP`]). Mehr kann die Laufzeit nicht
//! schreiben; ein größerer Zustand ist manipuliert und wird abgelehnt.
//!
//! # Was wo begrenzt ist
//! [`crate::DispatchState`] hat kein `Deserialize`; der einzige Leseweg ist
//! [`crate::DispatchState::from_json_bounded`]. Ein Zustand durchläuft vier
//! Grenzen, jede vor der nächsten Stufe:
//! 1. **Bytes und Zeichenvorrat, vor dem Dekodieren:** `from_json_bounded`
//!    lehnt mehr als [`MAX_STATE_BYTES`] Bytes ab (`"too many bytes"`),
//!    danach jede Eingabe mit einem Byte außer druckbarem ASCII ohne
//!    Backslash (`0x20` bis `0x7E` außer `0x5C`) und JSON-Leerraum (Tab, LF,
//!    CR) (`"json encoding"`); beides bevor serde ein Byte liest, die zweite
//!    Prüfung in einem Durchlauf ohne Allokation. Eine Kodierung aus
//!    [`crate::DispatchState::to_json`] hat nur solche Bytes: jede
//!    Zeichenkette eines gültigen Zustands hat eine feste ASCII-Form ohne
//!    `"` und `\` (siehe „Herleitung"), und `serde_json` schreibt nur
//!    Steuerzeichen, `"` und `\` als Escape. So kommt beim Dekodieren kein
//!    Escape vor, jede Zeichenkette ist ein unverändertes Stück der Eingabe,
//!    und eine Fehlermeldung, die eine Zeichenkette der Eingabe in
//!    Debug-Form wiedergibt (`invalid type: string "…"`), ist nur um feste
//!    Wörter länger als diese (ohne die Prüfung schriebe Debug ein rohes
//!    `0x7F` als `\u{7f}`, sechsmal so lang). Das Lesen der Datei bleibt
//!    Sache des Aufrufers (Ring A: kein IO); er liest höchstens
//!    `MAX_STATE_BYTES + 1` Bytes und reicht sie unverändert weiter.
//! 2. **Einträge, während des Dekodierens:** jede Tabelle, und jede innere
//!    Liste (wartende Befunde je Cgroup, Mitglieder je Auftrag), nimmt
//!    höchstens ihren Höchstwert auf (unten). Paare gehen ohne Zwischenliste
//!    direkt in die Tabelle, Listen wachsen höchstens bis zu ihrer Grenze.
//!    Folgt dem Höchstwert noch ein Eintrag, bricht das Dekodieren ab
//!    (`"too many table entries"`), bevor serde ein Byte dieses Eintrags
//!    parst (gelesen ist nur das Trennzeichen davor); auch der Rest der
//!    Eingabe wird nie geparst.
//!    Ohne diese Stufe passten in die Bytegrenze zig Millionen winziger
//!    Einträge (`[n,0],` sind 6 bis 13 Bytes), die serde vor jeder Prüfung
//!    anlegte: über 1 GiB Speicher für eine Datei von 242 MiB.
//! 3. **Einträge, beim Wiederherstellen:** [`crate::Dispatcher::restore`]
//!    vergleicht zuerst die Länge jeder Tabelle mit ihrem Höchstwert (je
//!    Tabelle O(1)), dann in einem Durchlauf über die damit begrenzten
//!    Cgroups und Aufträge deren innere Längen, und zwar bevor ein einziger
//!    Eintrag geprüft, gerankt oder gekürzt wird. Für dekodierte Zustände
//!    ist das eine zweite Prüfung der Grenzen aus Stufe 2; sie deckt auch
//!    einen in der Crate gebauten Zustand ab. Prüfung, Ranking
//!    (`crate::prune`) und Kürzen kosten danach höchstens so viel wie ein
//!    Zustand an den Höchstwerten.
//! 4. **Aktive Grenzen:** was unter dem Höchstwert, aber über der Grenze der
//!    übergebenen Konfiguration liegt, kürzt `restore` deterministisch, mit
//!    Zusatzspeicher höchstens in Höhe der aktiven Grenze.
//!
//! # Speicher beim Dekodieren
//! Was `from_json_bounded` höchstens belegt (die Eingabe selbst hält der
//! Aufrufer, höchstens [`MAX_STATE_BYTES`]):
//! - **Strukturen:** höchstens so viele Einträge, wie ein Zustand an den
//!   Höchstwerten hat (Stufe 2), je Eintrag seine feste Größe, sein Anteil
//!   an den B-Baum-Knoten und je Zeichenkette eine Allokation. Überschlagen
//!   für x86_64 mit 32 Bytes Mindestallokation: rund 150 MiB, gut 40 %
//!   davon die 4 096 wartenden Aufträge mit je 64 Mitgliedern. Das ist
//!   dieselbe Größenordnung, die ein Dispatcher an den Höchstwerten
//!   ohnehin im Speicher hält.
//! - **Bytes aus der Eingabe:** die Inhalte der gelesenen Zeichenketten
//!   (unveränderte Stücke der Eingabe, Stufe 1), der Stapel, mit dem
//!   `serde_json` den Wert eines unbekannten Felds überspringt (ein Byte je
//!   offener Klammer; einen Eintrag über einer Grenze überspringt es nicht,
//!   Stufe 2), und die Meldung eines Lesefehlers, die höchstens eine
//!   Zeichenkette der Eingabe wiedergibt (dank Stufe 1 so lang wie diese,
//!   plus feste Wörter). Jedes Byte der Eingabe trägt zu höchstens einem davon bei:
//!   zusammen sind sie höchstens so lang wie die Eingabe, mit der
//!   Wachstumsreserve ihrer Puffer (Verdopplung) höchstens doppelt so lang
//!   (als Kapazität gezählt; ein Allokator, der beim Vergrößern umkopiert,
//!   statt den Puffer per `mremap` umzuhängen wie glibc, hält dabei kurz
//!   alten und neuen Puffer).
//!   `from_json_bounded` vergleicht die Fehlermeldung nur mit der Marke der
//!   Eintragsgrenze, ohne sie zu kopieren, und verwirft sie dann.
//!
//! Die Spitze liegt damit bei höchstens `3 × MAX_STATE_BYTES` (Eingabe und
//! Bytes daraus, rund 726 MiB) plus den Strukturen (rund 150 MiB), also
//! unter 1 GiB auch für eine feindliche Datei an allen Grenzen; ein
//! gültiger Zustand an den Höchstwerten bleibt darunter (seine
//! Zeichenketten haben feste, kurze Formen).
//! Eine Speichergrenze des Prozesses (DodWatch: `MemoryMax` der Unit) ist
//! die zweite Linie, nicht die erste.
//!
//! # Herleitung von [`MAX_STATE_BYTES`]
//! Je Tabelle eine obere Schranke für die kompakte JSON-Kodierung
//! ([`crate::DispatchState::to_json`]) eines **gültigen** Eintrags samt
//! Trennzeichen, mal ihrem Höchstwert, dazu ein fester Anteil für Skalare,
//! Zähler, Feldnamen und Klammern. Die Schranken gelten, weil in einem
//! gültigen Zustand jede Zeichenkette eine feste Form hat (UUIDv4 36
//! Zeichen, `dedup_key` 32, Digest 64, Sensor höchstens
//! [`crate::MAX_SENSOR_ID_LEN`] ASCII-Zeichen ohne Escapes, bekannte Regel
//! höchstens 18, sonst Enum-Namen) und jede Zahl höchstens 20 Zeichen hat.
//! Die Schranken unten sind von Hand gerechnet und mit Reserve aufgerundet;
//! die Tests kodieren je Tabelle einen Eintrag mit den längsten Werten und
//! prüfen seine Schranke.
//!
//! | Tabelle | Höchstwert | Bytes je Eintrag |
//! |---|---|---|
//! | fester Anteil | 1 | 8 192 |
//! | wartende Aufträge (je 64 Mitglieder) | [`QUEUE_CAPACITY_LIMIT`] = 4 096 | 25 600 |
//! | vergebene Aufträge (je 64 Mitglieder) | [`MAX_PARALLEL_LIMIT`] = 64 | 25 600 |
//! | bekannte Schlüssel | [`MAX_KNOWN_KEYS_LIMIT`] = 131 072 | 320 |
//! | Cgroups (je 32 wartende Befunde und ein Vorfall) | [`MAX_TRACKED_CGROUPS_LIMIT`] = 8 192 | 8 192 |
//! | Budget: Eimer, uid-Eimer, Erstsichtungen, Vorrechte | je [`BUDGET_MAX_PARTITIONS_LIMIT`] = 32 768 | 256 + 128 + 320 + 64 |
//! | Breaker | [`BREAKER_MAX_PARTITIONS_LIMIT`] = 32 768 | 384 |
//! | offene Rückfragen | [`crate::MAX_PENDING_ASKS`] = 256 | 1 024 |
//! | `Starved`-Episoden | [`crate::MAX_STARVED_EPISODES`] = 256 | 64 |
//!
//! Ergebnis: 253 583 360 Bytes (knapp 242 MiB). Die Höchstwerte der
//! Konfiguration sind so gewählt, dass diese Summe eine echte Grenze bleibt
//! (das Zwei- bis Achtfache der Vorgaben); unter den früheren Höchstwerten
//! (bis zu 2^22 Schlüsseln, 2^20 Cgroups und 65 536 wartenden Aufträgen)
//! wären es rund 12,8 GB gewesen.
//!
//! # Nebenläufigkeit und Fehler
//! Konstanten und reine Funktionen; Fehler sind inhaltsfreie
//! [`DispatchError::InvalidState`]. Byte- und Zeichengrenze (Stufe 1) prüft
//! dieses Modul, die Eintragsgrenzen beim Dekodieren setzt
//! `crate::serde_map` mit genau diesen Konstanten durch.
//!
//! # Examples
//! ```
//! use harw_dod_dispatch::{DispatchConfig, MAX_KNOWN_KEYS_LIMIT, MAX_STATE_BYTES};
//!
//! let widest = DispatchConfig {
//!     max_known_keys: MAX_KNOWN_KEYS_LIMIT,
//!     ..DispatchConfig::default()
//! };
//! assert!(widest.validate().is_ok());
//! let beyond = DispatchConfig {
//!     max_known_keys: MAX_KNOWN_KEYS_LIMIT.saturating_add(1),
//!     ..widest
//! };
//! assert!(beyond.validate().is_err());
//! assert!(MAX_STATE_BYTES <= 256 << 20);
//! ```

use crate::dispatcher::{MAX_PENDING_ASKS, MAX_STARVED_EPISODES};
use crate::error::DispatchError;

/// Höchstwert von [`crate::DispatchConfig::max_parallel`].
///
/// # Description
/// Mehr vergebene Aufträge kann kein Zustand tragen, den eine gültige
/// Konfiguration geschrieben hat; [`crate::Dispatcher::restore`] lehnt
/// einen solchen Zustand ab.
pub const MAX_PARALLEL_LIMIT: usize = 64;

/// Höchstwert von [`crate::DispatchConfig::queue_capacity`].
///
/// # Description
/// Mehr wartende Aufträge kann kein Zustand tragen, den eine gültige
/// Konfiguration geschrieben hat; [`crate::Dispatcher::restore`] lehnt
/// einen solchen Zustand ab.
pub const QUEUE_CAPACITY_LIMIT: usize = 4_096;

/// Höchstwert von [`crate::DispatchConfig::max_tracked_cgroups`].
///
/// # Description
/// Mehr Cgroups im [`crate::IncidentGrouper`] kann kein Zustand tragen, den
/// eine gültige Konfiguration geschrieben hat; [`crate::Dispatcher::restore`]
/// lehnt einen solchen Zustand ab.
pub const MAX_TRACKED_CGROUPS_LIMIT: usize = 8_192;

/// Höchstwert von [`crate::DispatchConfig::max_known_keys`].
///
/// # Description
/// Mehr bekannte Schlüssel kann kein Zustand tragen, den eine gültige
/// Konfiguration geschrieben hat; [`crate::Dispatcher::restore`] lehnt
/// einen solchen Zustand ab.
pub const MAX_KNOWN_KEYS_LIMIT: usize = 131_072;

/// Höchstwert von [`crate::BudgetConfig::max_partitions`].
///
/// # Description
/// Gilt für jede der vier Budget-Tabellen (Eimer, uid-Eimer, Erstsichtungen,
/// Vorrechte je uid). Mehr Einträge kann kein Zustand tragen, den eine
/// gültige Konfiguration geschrieben hat; [`crate::Dispatcher::restore`]
/// lehnt einen solchen Zustand ab.
pub const BUDGET_MAX_PARTITIONS_LIMIT: usize = 32_768;

/// Höchstwert von [`crate::BreakerConfig::max_partitions`].
///
/// # Description
/// Mehr Breaker kann kein Zustand tragen, den eine gültige Konfiguration
/// geschrieben hat; [`crate::Dispatcher::restore`] lehnt einen solchen
/// Zustand ab.
pub const BREAKER_MAX_PARTITIONS_LIMIT: usize = 32_768;

// Schranke der kompakten Kodierung eines Zustands ohne Tabelleneinträge:
// Version, Zähler, Skalare von Warteschlange, Grouper und Budget samt
// globalem Eimer, Feldnamen und Klammern (gerechnet gut 1,6 KiB).
pub(crate) const STATE_FIXED_BYTES: usize = 8_192;

// Schranke eines wartenden Auftrags samt Komma (`QueueEntry` mit Priorität,
// Nummer und `MAX_JOB_MEMBERS` Mitgliedern zu je höchstens 333 Bytes;
// gerechnet 21 599).
pub(crate) const QUEUED_JOB_BYTES: usize = 25_600;

// Schranke eines vergebenen Auftrags samt Komma (wie oben, mit
// Vergabezeit statt Priorität; gerechnet 21 500).
pub(crate) const IN_FLIGHT_JOB_BYTES: usize = 25_600;

// Schranke eines bekannten Schlüssels samt Komma (Schlüssel als
// Objektname, Fingerprint mit Digest; gerechnet 244).
pub(crate) const KNOWN_KEY_BYTES: usize = 320;

// Schranke einer Cgroup samt Komma (`MAX_ENTRIES_PER_CGROUP` wartende
// Befunde zu je höchstens 199 Bytes und ein offener Vorfall; gerechnet
// gut 6 700).
pub(crate) const TRACKED_CGROUP_BYTES: usize = 8_192;

// Schranke eines Partitions-Eimers samt Komma (Partition mit längstem
// Sensor; gerechnet 218).
pub(crate) const BUDGET_PARTITION_BYTES: usize = 256;

// Schranke eines uid-Eimers samt Komma (gerechnet 100).
pub(crate) const BUDGET_UID_RESERVE_BYTES: usize = 128;

// Schranke einer Erstsichtung samt Komma (gerechnet 254).
pub(crate) const BUDGET_ESTABLISHED_BYTES: usize = 320;

// Schranke eines Vorrechts je uid samt Komma (gerechnet 34).
pub(crate) const BUDGET_UID_PRIVILEGE_BYTES: usize = 64;

// Schranke eines Breakers samt Komma (gerechnet 320).
pub(crate) const BREAKER_BYTES: usize = 384;

// Schranke einer offenen Rückfrage samt Komma (vollständiger Befund;
// gerechnet 788).
pub(crate) const PENDING_ASK_BYTES: usize = 1_024;

// Schranke einer `Starved`-Episode samt Komma (gerechnet 56).
pub(crate) const STARVED_EPISODE_BYTES: usize = 64;

/// Höchstgröße der Kodierung eines Dispatch-Zustands in Bytes.
///
/// # Description
/// Die größte kompakte JSON-Kodierung ([`crate::DispatchState::to_json`]),
/// die ein Zustand unter irgendeiner gültigen Konfiguration haben kann:
/// Summe aus Höchstwert mal Schranke je Eintrag über alle Tabellen plus
/// einem festen Anteil (Herleitung in der Moduldoku, 253 583 360 Bytes).
/// [`crate::DispatchState::from_json_bounded`] lehnt längere Eingaben ab,
/// bevor serde läuft, [`crate::DispatchState::to_json`] schreibt keine
/// längeren.
///
/// Die Bytegrenze allein begrenzt den Speicher beim Dekodieren nicht
/// ausreichend (winzige Einträge vervielfachen ihn, Escapes in
/// Fehlermeldungen ebenso); dafür sorgen die Zeichengrenze, die
/// `from_json_bounded` vor dem Dekodieren prüft, und die Eintragsgrenzen,
/// die es während des Dekodierens durchsetzt. Alle drei und die Spitze beim
/// Dekodieren beschreibt die Moduldoku („Was wo begrenzt ist", „Speicher
/// beim Dekodieren").
///
/// # Examples
/// ```
/// use harw_dod_dispatch::MAX_STATE_BYTES;
///
/// assert_eq!(MAX_STATE_BYTES, 253_583_360);
/// ```
pub const MAX_STATE_BYTES: usize = STATE_FIXED_BYTES
    .saturating_add(QUEUE_CAPACITY_LIMIT.saturating_mul(QUEUED_JOB_BYTES))
    .saturating_add(MAX_PARALLEL_LIMIT.saturating_mul(IN_FLIGHT_JOB_BYTES))
    .saturating_add(MAX_KNOWN_KEYS_LIMIT.saturating_mul(KNOWN_KEY_BYTES))
    .saturating_add(MAX_TRACKED_CGROUPS_LIMIT.saturating_mul(TRACKED_CGROUP_BYTES))
    .saturating_add(
        BUDGET_MAX_PARTITIONS_LIMIT.saturating_mul(
            BUDGET_PARTITION_BYTES
                .saturating_add(BUDGET_UID_RESERVE_BYTES)
                .saturating_add(BUDGET_ESTABLISHED_BYTES)
                .saturating_add(BUDGET_UID_PRIVILEGE_BYTES),
        ),
    )
    .saturating_add(BREAKER_MAX_PARTITIONS_LIMIT.saturating_mul(BREAKER_BYTES))
    .saturating_add(MAX_PENDING_ASKS.saturating_mul(PENDING_ASK_BYTES))
    .saturating_add(MAX_STARVED_EPISODES.saturating_mul(STARVED_EPISODE_BYTES));

// Die Längen der Tabellen eines Zustands, oder ihre Obergrenzen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TableSizes {
    // Vergebene Aufträge.
    pub(crate) in_flight_jobs: usize,
    // Wartende Aufträge.
    pub(crate) queued_jobs: usize,
    // Bekannte Schlüssel.
    pub(crate) known_keys: usize,
    // Cgroups des Groupers.
    pub(crate) tracked_cgroups: usize,
    // Partitions-Eimer des Budgets.
    pub(crate) budget_partitions: usize,
    // uid-Eimer des Budgets.
    pub(crate) budget_uid_reserves: usize,
    // Erstsichtungen des Budgets.
    pub(crate) budget_established: usize,
    // Vorrechte je uid des Budgets.
    pub(crate) budget_uid_privileges: usize,
    // Breaker.
    pub(crate) breakers: usize,
    // Offene Rückfragen.
    pub(crate) pending_asks: usize,
    // `Starved`-Episoden.
    pub(crate) starved_episodes: usize,
}

impl TableSizes {
    // Die absoluten Höchstwerte: mehr kann keine gültige Konfiguration
    // schreiben (siehe Moduldoku).
    pub(crate) const LEGAL_MAX: Self = Self {
        in_flight_jobs: MAX_PARALLEL_LIMIT,
        queued_jobs: QUEUE_CAPACITY_LIMIT,
        known_keys: MAX_KNOWN_KEYS_LIMIT,
        tracked_cgroups: MAX_TRACKED_CGROUPS_LIMIT,
        budget_partitions: BUDGET_MAX_PARTITIONS_LIMIT,
        budget_uid_reserves: BUDGET_MAX_PARTITIONS_LIMIT,
        budget_established: BUDGET_MAX_PARTITIONS_LIMIT,
        budget_uid_privileges: BUDGET_MAX_PARTITIONS_LIMIT,
        breakers: BREAKER_MAX_PARTITIONS_LIMIT,
        pending_asks: MAX_PENDING_ASKS,
        starved_episodes: MAX_STARVED_EPISODES,
    };

    // Lehnt die erste Tabelle (in fester Reihenfolge) ab, deren Länge über
    // ihrer Obergrenze in `bounds` liegt. Reine Zahlenvergleiche: kein
    // Eintrag wird angesehen.
    pub(crate) fn check_within(&self, bounds: &Self) -> Result<(), DispatchError> {
        let rows = [
            (
                self.in_flight_jobs,
                bounds.in_flight_jobs,
                "too many in-flight jobs",
            ),
            (self.queued_jobs, bounds.queued_jobs, "too many queued jobs"),
            (self.known_keys, bounds.known_keys, "too many known keys"),
            (
                self.tracked_cgroups,
                bounds.tracked_cgroups,
                "too many tracked cgroups",
            ),
            (
                self.budget_partitions,
                bounds.budget_partitions,
                "too many budget partitions",
            ),
            (
                self.budget_uid_reserves,
                bounds.budget_uid_reserves,
                "too many budget uid reserves",
            ),
            (
                self.budget_established,
                bounds.budget_established,
                "too many established partitions",
            ),
            (
                self.budget_uid_privileges,
                bounds.budget_uid_privileges,
                "too many budget uid privileges",
            ),
            (self.breakers, bounds.breakers, "too many breakers"),
            (
                self.pending_asks,
                bounds.pending_asks,
                "too many pending asks",
            ),
            (
                self.starved_episodes,
                bounds.starved_episodes,
                "too many starved episodes",
            ),
        ];
        match rows.into_iter().find(|(len, bound, _)| len > bound) {
            Some((_, _, what)) => Err(DispatchError::InvalidState { what }),
            None => Ok(()),
        }
    }
}

// Lehnt eine Kodierung mit mehr als `max_bytes` Bytes ab; nur die Länge
// zählt, kein Byte wird gelesen.
pub(crate) fn check_state_bytes(len: usize, max_bytes: usize) -> Result<(), DispatchError> {
    if len > max_bytes {
        return Err(DispatchError::InvalidState {
            what: "too many bytes",
        });
    }
    Ok(())
}

// Lehnt eine Kodierung mit einem Byte ab, das `DispatchState::to_json` nie
// schreibt (Moduldoku, Stufe 1): alles außer druckbarem ASCII ohne
// Backslash und JSON-Leerraum. Ein Durchlauf ohne Allokation, vor serde;
// der Fehler nennt kein Byte.
pub(crate) fn check_state_encoding(bytes: &[u8]) -> Result<(), DispatchError> {
    if bytes.iter().copied().all(is_state_byte) {
        Ok(())
    } else {
        Err(DispatchError::InvalidState {
            what: "json encoding",
        })
    }
}

// Ein Byte, das eine Kodierung enthalten darf: druckbares ASCII ohne `\`
// (`0x5C` liegt zwischen `[` und `]`), also kein Escape, oder JSON-Leerraum
// außerhalb von Zeichenketten (in einer Zeichenkette lehnt `serde_json`
// ein rohes Steuerzeichen mit fester Meldung ab).
const fn is_state_byte(byte: u8) -> bool {
    matches!(byte, b' '..=b'[' | b']'..=b'~' | b'\t' | b'\n' | b'\r')
}
