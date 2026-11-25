//! Eingabe des Dispatchers: [`DispatchFinding`] und die Prüfgrammatiken.
//!
//! # Zweck
//! Der Dispatcher kennt weder `harw-dod-signals` noch `harw-dod-rules`
//! (Ring A ohne interne Abhängigkeiten). Er spiegelt deshalb die Namen von
//! [`Severity`], [`Hardness`] und [`FindingKind`] und nimmt einen Befund als
//! eigenes, besitzendes Struct entgegen. Der Aufrufer (DodWatch) bildet den
//! gelesenen Export-Record darauf ab.
//!
//! # Grammatiken
//! - `finding_id`: strikte UUIDv4, 8-4-4-4-12 Hex in Kleinbuchstaben,
//!   Versions-Nibble `4`, Varianten-Nibble `8`, `9`, `a` oder `b`
//!   ([`is_uuid_v4`]);
//! - `dedup_key`: genau 32 Hex-Zeichen in Kleinbuchstaben ([`is_dedup_key`]);
//! - `record_digest` und `fingerprint.exe_digest`: genau 64 Hex-Zeichen in
//!   Kleinbuchstaben ([`is_record_digest`]);
//! - `sensor`: 1 bis [`MAX_SENSOR_ID_LEN`] ASCII-Zeichen aus Buchstaben,
//!   Ziffern, `-`, `_` und `.` ([`is_sensor_id`]).
//!
//! Alles, was diese Grammatiken nicht erfüllt, verwirft der Dispatcher mit
//! einem Zähler ([`DropReason`]). Nur geprüfte Kennungen erreichen damit je
//! ein Triage-Template oder einen Hinweis.
//!
//! # Härte
//! [`Hardness`] trägt bewusst **keine** abgeleitete Ordnung. In
//! `harw-dod-signals` ist `Observed < Correlated < Inferred` (steigende
//! Indirektheit); die Dispatch-Priorität braucht die Gegenrichtung
//! („Observed vor Correlated vor Inferred"). Verglichen wird deshalb nur über
//! [`Hardness::strength`], damit keine Stelle versehentlich die falsche
//! Richtung erbt.
//!
//! # Nebenläufigkeit und Fehler
//! Reine Daten und totale Funktionen ohne Zustand; keine Fehlerfälle.
//!
//! # Examples
//! ```
//! use harw_dod_dispatch::{is_dedup_key, is_uuid_v4, Hardness, Severity};
//!
//! assert!(is_uuid_v4("3f2c9a10-7b1e-4c2d-9e8f-0a1b2c3d4e5f"));
//! assert!(!is_uuid_v4("3F2C9A10-7B1E-4C2D-9E8F-0A1B2C3D4E5F"));
//! assert!(is_dedup_key("00112233445566778899aabbccddeeff"));
//! assert_eq!(Severity::Medium.raised_for_obfuscation(), Severity::High);
//! assert_eq!(Severity::Critical.raised_for_obfuscation(), Severity::Critical);
//! assert!(Hardness::Observed.is_stronger_than(Hardness::Inferred));
//! ```

use serde::{Deserialize, Serialize};

/// Längste zulässige Sensor-Kennung in Bytes (siehe [`is_sensor_id`]).
pub const MAX_SENSOR_ID_LEN: usize = 64;

/// Ersatzkennung in [`crate::DispatchAction`]s für eine `finding_id`, die
/// keine strikte UUIDv4 ist.
///
/// # Description
/// Ein ungültiger Befund erzeugt trotzdem einen Hinweis, wenn er `High` oder
/// höher ist (I3). Der Hinweis trägt dann nie die fremde Zeichenkette,
/// sondern diese feste Marke.
pub const INVALID_FINDING_ID: &str = "invalid-finding-id";

/// Ersatzkennung in [`crate::DispatchAction::Notify`] für eine unbekannte
/// Regel (siehe [`INVALID_FINDING_ID`]).
pub const UNKNOWN_RULE: &str = "unknown-rule";

/// Wie schwer ein Befund wiegt; Spiegel von `harw_dod_signals::Severity`.
///
/// # Description
/// Gleiche Namen, gleiche Serde-Schreibweise (`kebab-case`) und gleiche
/// Ordnung wie das Original: `Info < Low < Medium < High < Critical`.
///
/// # Examples
/// ```
/// use harw_dod_dispatch::Severity;
///
/// assert!(Severity::Medium < Severity::High);
/// assert!(Severity::High.requires_notice());
/// assert!(!Severity::Medium.requires_notice());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Severity {
    /// Rein informativ.
    Info,
    /// Geringes Gewicht.
    Low,
    /// Mittleres Gewicht.
    Medium,
    /// Hohes Gewicht.
    High,
    /// Kritisch, höchstes Gewicht.
    Critical,
}

impl Severity {
    /// Hebt die Schwere wegen eines verschleierten Scopes um eine Stufe an.
    ///
    /// # Description
    /// Regel aus README §3.3: „Severity +1 (höchstens High)". Die Kappung
    /// senkt nie: `High` bleibt `High`, `Critical` bleibt `Critical`.
    ///
    /// # Returns
    /// Die angehobene Schwere.
    ///
    /// # Examples
    /// ```
    /// use harw_dod_dispatch::Severity;
    ///
    /// assert_eq!(Severity::Info.raised_for_obfuscation(), Severity::Low);
    /// assert_eq!(Severity::High.raised_for_obfuscation(), Severity::High);
    /// ```
    #[must_use]
    pub const fn raised_for_obfuscation(self) -> Self {
        match self {
            Self::Info => Self::Low,
            Self::Low => Self::Medium,
            Self::Medium | Self::High => Self::High,
            Self::Critical => Self::Critical,
        }
    }

    /// Ob diese Schwere immer einen deterministischen Hinweis erzeugt (I3).
    ///
    /// # Returns
    /// `true` für `High` und `Critical`.
    #[must_use]
    pub const fn requires_notice(self) -> bool {
        matches!(self, Self::High | Self::Critical)
    }

    /// Kanonische Schreibweise (wie die Serde-Form).
    ///
    /// # Returns
    /// `"info"`, `"low"`, `"medium"`, `"high"` oder `"critical"`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }
}

/// Wie direkt ein Befund belegt ist; Spiegel von `harw_dod_signals::Hardness`.
///
/// # Description
/// Gleiche Namen und Serde-Schreibweise wie das Original, aber **ohne**
/// `Ord` (siehe Moduldoku): verglichen wird nur über [`Hardness::strength`].
///
/// # Examples
/// ```
/// use harw_dod_dispatch::Hardness;
///
/// assert!(Hardness::Observed.strength() > Hardness::Correlated.strength());
/// assert!(Hardness::Correlated.is_stronger_than(Hardness::Inferred));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Hardness {
    /// Direkt beobachtet.
    Observed,
    /// Aus mehreren Beobachtungen verknüpft.
    Correlated,
    /// Über die Beobachtungen hinaus geschlossen.
    Inferred,
}

impl Hardness {
    /// Stärke des Belegs: `Observed` = 2, `Correlated` = 1, `Inferred` = 0.
    ///
    /// # Returns
    /// Je größer, desto direkter der Beleg und desto höher die Priorität.
    #[must_use]
    pub const fn strength(self) -> u8 {
        match self {
            Self::Observed => 2,
            Self::Correlated => 1,
            Self::Inferred => 0,
        }
    }

    /// Ob `self` direkter belegt ist als `other`.
    ///
    /// # Arguments
    /// - `other` (`Hardness`): Vergleichswert.
    ///
    /// # Returns
    /// `true`, wenn [`Hardness::strength`] von `self` größer ist.
    #[must_use]
    pub const fn is_stronger_than(self, other: Self) -> bool {
        self.strength() > other.strength()
    }

    /// Kanonische Schreibweise (wie die Serde-Form).
    ///
    /// # Returns
    /// `"observed"`, `"correlated"` oder `"inferred"`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Observed => "observed",
            Self::Correlated => "correlated",
            Self::Inferred => "inferred",
        }
    }
}

/// Art eines Befunds; Spiegel von `harw_dod_rules::FindingKind`.
///
/// # Description
/// Wird vom Dispatcher nur durchgereicht; das Routing hängt an der Regel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FindingKind {
    /// Eine Regel hat eindeutig ausgelöst.
    RuleTriggered,
    /// Eine bloße Abweichung.
    Anomaly,
}

/// Merkmale, an denen die [`crate::ReTriagePolicy`] eine echte Änderung
/// unter einem bekannten `dedup_key` erkennt.
///
/// # Description
/// Jedes Feld ist optional; ein fehlendes Feld ist nie eine Änderung.
/// `exe_digest` muss, wenn gesetzt, 64 Hex-Zeichen in Kleinbuchstaben haben
/// (sonst verwirft der Dispatcher den Befund mit
/// [`DropReason::InvalidFingerprint`]).
///
/// # Examples
/// ```
/// use harw_dod_dispatch::TriageFingerprint;
///
/// let fingerprint = TriageFingerprint { port: Some(443), ..TriageFingerprint::default() };
/// assert_eq!(fingerprint.port, Some(443));
/// assert!(fingerprint.exe_digest.is_none());
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TriageFingerprint {
    /// Ziel- bzw. Listener-Port.
    pub port: Option<u16>,
    /// Digest der ausführenden Datei (64 Hex-Zeichen).
    pub exe_digest: Option<String>,
    /// Beobachtete Rate pro Minute.
    pub rate_per_min: Option<u64>,
}

/// Ein Befund, wie ihn der Dispatcher entgegennimmt.
///
/// # Description
/// Besitzende Felder, abgebildet aus dem Export v3 (README §3). Alle
/// Zeichenketten gelten als untrusted, bis der Dispatcher sie gegen die
/// Grammatiken der Moduldoku geprüft hat; nur `finding_id`, `rule_id` und
/// `record_digest` erreichen je ein Triage-Template, und das erst nach der
/// Prüfung. `sensor` landet geprüft im [`PartitionKey`].
///
/// # Examples
/// ```
/// use harw_dod_dispatch::{
///     DispatchFinding, FindingKind, Hardness, Severity, TriageFingerprint, RULE_EGRESS_FLOW,
/// };
///
/// let finding = DispatchFinding {
///     finding_id: "3f2c9a10-7b1e-4c2d-9e8f-0a1b2c3d4e5f".to_owned(),
///     rule_id: RULE_EGRESS_FLOW.to_owned(),
///     severity: Severity::Medium,
///     hardness: Hardness::Observed,
///     kind: FindingKind::RuleTriggered,
///     dedup_key: "00112233445566778899aabbccddeeff".to_owned(),
///     cgroup_kid: Some(4242),
///     uid: Some(1000),
///     sensor: "probe-bpf-0".to_owned(),
///     occurrences: 1,
///     observed_at_unix: 1_700_000_000,
///     scope_obfuscated: true,
///     record_digest: Some("ab".repeat(32)),
///     loopback: false,
///     verified_source: true,
///     fingerprint: TriageFingerprint::default(),
/// };
/// assert_eq!(finding.effective_severity(), Severity::High);
/// assert!(!finding.freeze_proposal_allowed());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchFinding {
    /// Befund-Kennung (strikte UUIDv4).
    pub finding_id: String,
    /// Regel-Kennung (siehe [`crate::ALL_RULE_IDS`]).
    pub rule_id: String,
    /// Schwere laut Regel.
    pub severity: Severity,
    /// Härte des Belegs.
    pub hardness: Hardness,
    /// Art des Befunds.
    pub kind: FindingKind,
    /// Throttle-Schlüssel des Sentinels (32 Hex-Zeichen).
    pub dedup_key: String,
    /// Kernel-ID der Cgroup, falls bekannt.
    pub cgroup_kid: Option<u64>,
    /// uid des Verursachers, falls bekannt.
    pub uid: Option<u32>,
    /// Kennung des meldenden Sensors.
    pub sensor: String,
    /// Wie oft der Sentinel den Schlüssel gesehen hat.
    pub occurrences: u64,
    /// Ereigniszeit in Unix-Sekunden.
    pub observed_at_unix: i64,
    /// Der Scope enthielt verschleiernde Zeichen (WP0.9).
    pub scope_obfuscated: bool,
    /// Digest des Spool-Records (64 Hex-Zeichen); Pflicht für KI-Routen.
    pub record_digest: Option<String>,
    /// Der Befund betrifft nur Loopback (Entscheidung O9).
    pub loopback: bool,
    /// Der Leser hat die Herkunft des Records geprüft.
    pub verified_source: bool,
    /// Merkmale für die erneute Triage.
    pub fingerprint: TriageFingerprint,
}

impl DispatchFinding {
    /// Schwere nach der Anhebung für verschleierte Scopes.
    ///
    /// # Returns
    /// [`Severity::raised_for_obfuscation`] bei `scope_obfuscated`, sonst
    /// `severity` unverändert.
    #[must_use]
    pub fn effective_severity(&self) -> Severity {
        if self.scope_obfuscated {
            self.severity.raised_for_obfuscation()
        } else {
            self.severity
        }
    }

    /// Ob für diesen Befund ein Freeze-Vorschlag zulässig ist.
    ///
    /// # Returns
    /// `false` bei verschleiertem Scope (nur ein Mensch entscheidet).
    #[must_use]
    pub fn freeze_proposal_allowed(&self) -> bool {
        !self.scope_obfuscated
    }

    /// Partition für Budget und Breaker: `(uid, cgroup_kid, sensor)`.
    ///
    /// # Returns
    /// Einen neuen [`PartitionKey`]; `sensor` wird unverändert übernommen
    /// (der Dispatcher prüft ihn vorher mit [`is_sensor_id`]).
    #[must_use]
    pub fn partition(&self) -> PartitionKey {
        PartitionKey {
            uid: self.uid,
            cgroup_kid: self.cgroup_kid,
            sensor: self.sensor.clone(),
        }
    }
}

/// Partition von Budget und Breaker (I10): `(uid, cgroup_kid, sensor)`.
///
/// # Description
/// Ein Sturm in einer Partition darf Befunde anderer Partitionen nicht
/// verdrängen. `sensor` ist im Dispatcher immer eine geprüfte
/// Sensor-Kennung ([`is_sensor_id`]); ein Aufrufer, der den Schlüssel
/// anzeigt, escapt ihn trotzdem wie jeden Sensortext.
///
/// # Examples
/// ```
/// use harw_dod_dispatch::PartitionKey;
///
/// let key = PartitionKey { uid: Some(1000), cgroup_kid: Some(7), sensor: "probe-bpf-0".to_owned() };
/// assert_eq!(key.uid, Some(1000));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PartitionKey {
    /// uid des Verursachers.
    pub uid: Option<u32>,
    /// Kernel-ID der Cgroup.
    pub cgroup_kid: Option<u64>,
    /// Kennung des meldenden Sensors.
    pub sensor: String,
}

/// Warum ein Befund nicht (erneut) an ein Modell geht.
///
/// # Description
/// Jeder Grund hat einen eigenen Zähler ([`crate::DropCounters`]). Die
/// ersten acht Gründe sind Prüffehler der Eingabe, die übrigen folgen aus
/// Budget, Breaker, Warteschlange und Re-Triage. Ein verworfener `High`-
/// oder `Critical`-Befund erzeugt trotzdem seinen Hinweis (I3).
///
/// # Examples
/// ```
/// use harw_dod_dispatch::DropReason;
///
/// assert_eq!(DropReason::UnknownRule.as_str(), "unknown-rule");
/// assert_eq!(DropReason::ALL.len(), 12);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DropReason {
    /// `finding_id` ist keine strikte UUIDv4.
    InvalidFindingId,
    /// Der Leser hat die Herkunft nicht geprüft (`verified_source == false`).
    UnverifiedSource,
    /// Die Regel steht nicht in der [`crate::RouteTable`].
    UnknownRule,
    /// `dedup_key` hat nicht genau 32 Hex-Zeichen in Kleinbuchstaben.
    InvalidDedupKey,
    /// `sensor` verletzt [`is_sensor_id`].
    InvalidSensor,
    /// `fingerprint.exe_digest` hat nicht genau 64 Hex-Zeichen.
    InvalidFingerprint,
    /// Eine KI-Route verlangt `record_digest`, er fehlt.
    MissingRecordDigest,
    /// `record_digest` hat nicht genau 64 Hex-Zeichen in Kleinbuchstaben.
    InvalidRecordDigest,
    /// Bekannter `dedup_key` ohne echte Änderung ([`crate::ReTriagePolicy`]).
    Duplicate,
    /// Der Breaker der Partition ist offen.
    BreakerOpen,
    /// Partitions- oder Gesamtbudget erschöpft.
    BudgetExhausted,
    /// Die Warteschlange war voll und dieser Befund hatte die niedrigste
    /// Priorität.
    QueueOverflow,
}

impl DropReason {
    /// Alle Gründe in Prüfreihenfolge.
    pub const ALL: [Self; 12] = [
        Self::InvalidFindingId,
        Self::UnverifiedSource,
        Self::UnknownRule,
        Self::InvalidDedupKey,
        Self::InvalidSensor,
        Self::InvalidFingerprint,
        Self::MissingRecordDigest,
        Self::InvalidRecordDigest,
        Self::Duplicate,
        Self::BreakerOpen,
        Self::BudgetExhausted,
        Self::QueueOverflow,
    ];

    /// Kanonische Schreibweise (wie die Serde-Form).
    ///
    /// # Returns
    /// Eine feste ASCII-Kennung in `kebab-case`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidFindingId => "invalid-finding-id",
            Self::UnverifiedSource => "unverified-source",
            Self::UnknownRule => "unknown-rule",
            Self::InvalidDedupKey => "invalid-dedup-key",
            Self::InvalidSensor => "invalid-sensor",
            Self::InvalidFingerprint => "invalid-fingerprint",
            Self::MissingRecordDigest => "missing-record-digest",
            Self::InvalidRecordDigest => "invalid-record-digest",
            Self::Duplicate => "duplicate",
            Self::BreakerOpen => "breaker-open",
            Self::BudgetExhausted => "budget-exhausted",
            Self::QueueOverflow => "queue-overflow",
        }
    }
}

// Ein Byte ist eine Hex-Ziffer in Kleinbuchstaben (`0-9a-f`).
const fn is_lower_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')
}

// Genau `len` Hex-Ziffern in Kleinbuchstaben.
fn is_lower_hex_of_len(candidate: &str, len: usize) -> bool {
    candidate.len() == len && candidate.bytes().all(is_lower_hex)
}

/// Prüft die strikte UUIDv4-Grammatik.
///
/// # Description
/// 8-4-4-4-12 Hex-Ziffern in Kleinbuchstaben mit `-` an den Positionen 8,
/// 13, 18 und 23, Versions-Nibble `4` (Position 14) und Varianten-Nibble
/// `8`, `9`, `a` oder `b` (Position 19). Großbuchstaben, Klammern, URN-
/// Präfixe und Leerraum werden abgelehnt.
///
/// # Arguments
/// - `candidate` (`&str`): die zu prüfende Zeichenkette.
///
/// # Returns
/// `true` nur für eine Zeichenkette in genau dieser Form.
///
/// # Examples
/// ```
/// use harw_dod_dispatch::is_uuid_v4;
///
/// assert!(is_uuid_v4("3f2c9a10-7b1e-4c2d-9e8f-0a1b2c3d4e5f"));
/// assert!(!is_uuid_v4("3f2c9a10-7b1e-1c2d-9e8f-0a1b2c3d4e5f")); // Version 1
/// assert!(!is_uuid_v4("3f2c9a10-7b1e-4c2d-ce8f-0a1b2c3d4e5f")); // Variante c
/// ```
#[must_use]
pub fn is_uuid_v4(candidate: &str) -> bool {
    candidate.len() == 36
        && candidate
            .bytes()
            .enumerate()
            .all(|(position, byte)| match position {
                8 | 13 | 18 | 23 => byte == b'-',
                14 => byte == b'4',
                19 => matches!(byte, b'8' | b'9' | b'a' | b'b'),
                _ => is_lower_hex(byte),
            })
}

/// Prüft einen Throttle-Schlüssel: genau 32 Hex-Ziffern in Kleinbuchstaben.
///
/// # Arguments
/// - `candidate` (`&str`): die zu prüfende Zeichenkette.
///
/// # Returns
/// `true` nur für genau diese Form.
///
/// # Examples
/// ```
/// use harw_dod_dispatch::is_dedup_key;
///
/// assert!(is_dedup_key("00112233445566778899aabbccddeeff"));
/// assert!(!is_dedup_key("00112233445566778899AABBCCDDEEFF"));
/// ```
#[must_use]
pub fn is_dedup_key(candidate: &str) -> bool {
    is_lower_hex_of_len(candidate, 32)
}

/// Prüft einen Digest: genau 64 Hex-Ziffern in Kleinbuchstaben.
///
/// # Description
/// Die Form von `ContentDigest`'s `Display` (BLAKE3, `harw-digest`). Gilt
/// für `record_digest` und `fingerprint.exe_digest`.
///
/// # Arguments
/// - `candidate` (`&str`): die zu prüfende Zeichenkette.
///
/// # Returns
/// `true` nur für genau diese Form.
///
/// # Examples
/// ```
/// use harw_dod_dispatch::is_record_digest;
///
/// assert!(is_record_digest(&"ab".repeat(32)));
/// assert!(!is_record_digest(&"ab".repeat(31)));
/// ```
#[must_use]
pub fn is_record_digest(candidate: &str) -> bool {
    is_lower_hex_of_len(candidate, 64)
}

/// Prüft eine Sensor-Kennung.
///
/// # Description
/// 1 bis [`MAX_SENSOR_ID_LEN`] Bytes aus ASCII-Buchstaben, Ziffern, `-`,
/// `_` und `.` (etwa `thermal-0`, `workspace-drift-1`). Begrenzt Größe und
/// Zeichenvorrat der Partitionsschlüssel im persistierten Zustand.
///
/// # Arguments
/// - `candidate` (`&str`): die zu prüfende Zeichenkette.
///
/// # Returns
/// `true` nur für eine Kennung in dieser Form.
///
/// # Examples
/// ```
/// use harw_dod_dispatch::is_sensor_id;
///
/// assert!(is_sensor_id("probe-bpf-0"));
/// assert!(!is_sensor_id("probe bpf"));
/// assert!(!is_sensor_id(""));
/// ```
#[must_use]
pub fn is_sensor_id(candidate: &str) -> bool {
    !candidate.is_empty()
        && candidate.len() <= MAX_SENSOR_ID_LEN
        && candidate
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;

    const VALID_UUID: &str = "3f2c9a10-7b1e-4c2d-9e8f-0a1b2c3d4e5f";

    #[test]
    fn test_uuid_v4_accepts_every_variant_nibble() -> TestResult {
        for variant in ['8', '9', 'a', 'b'] {
            let candidate = format!("3f2c9a10-7b1e-4c2d-{variant}e8f-0a1b2c3d4e5f");
            assert!(is_uuid_v4(&candidate), "variant {variant}");
        }
        assert!(is_uuid_v4(VALID_UUID));
        Ok(())
    }

    #[test]
    fn test_uuid_v4_rejects_malformed_input() -> TestResult {
        let rejected = [
            "",
            "3F2C9A10-7B1E-4C2D-9E8F-0A1B2C3D4E5F",
            "3f2c9a10-7b1e-1c2d-9e8f-0a1b2c3d4e5f",
            "3f2c9a10-7b1e-4c2d-7e8f-0a1b2c3d4e5f",
            "3f2c9a10-7b1e-4c2d-ce8f-0a1b2c3d4e5f",
            "3f2c9a107b1e-4c2d-9e8f-0a1b2c3d4e5f0",
            "3f2c9a10-7b1e-4c2d-9e8f-0a1b2c3d4e5",
            "3f2c9a10-7b1e-4c2d-9e8f-0a1b2c3d4e5f0",
            "{3f2c9a10-7b1e-4c2d-9e8f-0a1b2c3d4e5}",
            "3f2c9a10-7b1e-4c2d-9e8f-0a1b2c3d4e5g",
            " 3f2c9a10-7b1e-4c2d-9e8f-0a1b2c3d4e5",
            "3f2c9a10_7b1e_4c2d_9e8f_0a1b2c3d4e5f",
            "3f2c9a10-7b1e-4c2d-9e8f-0a1b2c3d4e\u{e9}",
        ];
        for candidate in rejected {
            assert!(!is_uuid_v4(candidate), "accepted {candidate:?}");
        }
        Ok(())
    }

    #[test]
    fn test_dedup_key_and_digest_grammar() -> TestResult {
        assert!(is_dedup_key("00112233445566778899aabbccddeeff"));
        assert!(!is_dedup_key("00112233445566778899aabbccddeef"));
        assert!(!is_dedup_key("00112233445566778899aabbccddeeff0"));
        assert!(!is_dedup_key("00112233445566778899aabbccddeefg"));
        assert!(!is_dedup_key("00112233445566778899AABBCCDDEEFF"));
        assert!(is_record_digest(&"0f".repeat(32)));
        assert!(!is_record_digest(&"0F".repeat(32)));
        assert!(!is_record_digest(&"0f".repeat(33)));
        assert!(!is_record_digest(""));
        Ok(())
    }

    #[test]
    fn test_sensor_id_grammar() -> TestResult {
        assert!(is_sensor_id("thermal-0"));
        assert!(is_sensor_id("workspace_drift.1"));
        assert!(is_sensor_id(&"s".repeat(MAX_SENSOR_ID_LEN)));
        assert!(!is_sensor_id(
            &"s".repeat(MAX_SENSOR_ID_LEN.saturating_add(1))
        ));
        assert!(!is_sensor_id(""));
        assert!(!is_sensor_id("thermal 0"));
        assert!(!is_sensor_id("thermal\u{202e}0"));
        assert!(!is_sensor_id("thermal/0"));
        Ok(())
    }

    #[test]
    fn test_obfuscation_raises_one_level_and_never_lowers() -> TestResult {
        let expected = [
            (Severity::Info, Severity::Low),
            (Severity::Low, Severity::Medium),
            (Severity::Medium, Severity::High),
            (Severity::High, Severity::High),
            (Severity::Critical, Severity::Critical),
        ];
        for (input, raised) in expected {
            assert_eq!(input.raised_for_obfuscation(), raised);
            assert!(input.raised_for_obfuscation() >= input);
        }
        Ok(())
    }

    #[test]
    fn test_hardness_strength_orders_observed_first() -> TestResult {
        assert!(Hardness::Observed.is_stronger_than(Hardness::Correlated));
        assert!(Hardness::Correlated.is_stronger_than(Hardness::Inferred));
        assert!(!Hardness::Inferred.is_stronger_than(Hardness::Inferred));
        assert!(!Hardness::Inferred.is_stronger_than(Hardness::Observed));
        Ok(())
    }

    #[test]
    fn test_serde_names_mirror_the_signals_crate() -> TestResult {
        let json = serde_json::to_string(&(
            Severity::Critical,
            Hardness::Correlated,
            FindingKind::RuleTriggered,
            DropReason::MissingRecordDigest,
        ))?;
        assert_eq!(
            json,
            r#"["critical","correlated","rule-triggered","missing-record-digest"]"#
        );
        for reason in DropReason::ALL {
            assert_eq!(
                serde_json::to_string(&reason)?,
                format!("\"{}\"", reason.as_str())
            );
        }
        Ok(())
    }
}
