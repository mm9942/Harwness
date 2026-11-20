//! Erneute Triage bekannter Schlüssel: [`ReTriagePolicy`].
//!
//! # Zweck
//! Ein `dedup_key`, der schon triagiert wurde, geht nur bei einer **echten
//! Änderung** erneut an ein Modell (README §3.3, B11). Echt ist:
//! - ein neuer Port ([`MaterialChange::NewPort`]);
//! - ein neuer Digest der ausführenden Datei
//!   ([`MaterialChange::NewExecutable`]);
//! - eine direktere Härte, also `Inferred` → `Correlated` → `Observed`
//!   ([`MaterialChange::HigherHardness`]);
//! - eine Rate von mindestens dem Vierfachen der gespeicherten
//!   ([`MaterialChange::RateIncrease`]).
//!
//! Ein im aktuellen Befund fehlendes Merkmal ist nie eine Änderung. Eine
//! Rate ohne gespeicherten Vergleichswert ebenfalls nicht; eine gespeicherte
//! Rate `0` wird von jeder positiven Rate überschritten.
//!
//! # Gespeicherter Stand
//! Verglichen wird gegen den Stand der **letzten Triage**, nicht gegen das
//! letzte Auftreten: sonst käme eine langsam steigende Rate (×2, ×2, ×2)
//! nie über die Schwelle. Nach einer erneuten Triage wird der Stand
//! zusammengeführt (neue Werte gewinnen, fehlende bleiben, die Härte bleibt
//! die direktere).
//!
//! # Nebenläufigkeit und Fehler
//! Reine Funktionen bzw. `&mut self`; keine Fehler außer beim Laden eines
//! Zustands ([`crate::DispatchError::InvalidState`]).
//!
//! # Examples
//! ```
//! use harw_dod_dispatch::{Hardness, KnownFinding, MaterialChange, ReTriagePolicy, TriageFingerprint};
//!
//! let stored = KnownFinding {
//!     fingerprint: TriageFingerprint { port: Some(443), rate_per_min: Some(10), ..TriageFingerprint::default() },
//!     hardness: Hardness::Correlated,
//!     last_seen_unix: 0,
//! };
//! let policy = ReTriagePolicy::new();
//! let same = TriageFingerprint { port: Some(443), rate_per_min: Some(30), ..TriageFingerprint::default() };
//! assert_eq!(policy.material_change(&stored, Hardness::Correlated, &same), None);
//! let burst = TriageFingerprint { rate_per_min: Some(40), ..same };
//! assert_eq!(policy.material_change(&stored, Hardness::Correlated, &burst), Some(MaterialChange::RateIncrease));
//! ```

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};

use crate::bounds::MAX_KNOWN_KEYS_LIMIT;
use crate::error::DispatchError;
use crate::finding::{Hardness, TriageFingerprint, is_dedup_key, is_record_digest};
use crate::prune::retain_greatest;

/// Der gespeicherte Stand eines bekannten `dedup_key`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownFinding {
    /// Merkmale zum Zeitpunkt der letzten Triage.
    pub fingerprint: TriageFingerprint,
    /// Härte zum Zeitpunkt der letzten Triage.
    pub hardness: Hardness,
    /// Letztes Auftreten in Dispatcher-Zeit (Unix-Sekunden).
    pub last_seen_unix: i64,
}

/// Welche Änderung eine erneute Triage auslöst.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MaterialChange {
    /// Neuer Port.
    NewPort,
    /// Neuer Digest der ausführenden Datei.
    NewExecutable,
    /// Direktere Härte.
    HigherHardness,
    /// Rate mindestens vervierfacht.
    RateIncrease,
}

/// Entscheidet über die erneute Triage (siehe Moduldoku).
///
/// # Examples
/// Siehe Moduldoku.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReTriagePolicy {
    // Faktor, ab dem eine Rate als echte Änderung gilt.
    rate_factor: u64,
}

impl Default for ReTriagePolicy {
    fn default() -> Self {
        Self::new()
    }
}

impl ReTriagePolicy {
    /// Faktor der Raten-Regel (README §3.3: „Rate ×4").
    pub const RATE_FACTOR: u64 = 4;

    /// Die Regel aus README §3.3.
    ///
    /// # Returns
    /// Die Policy mit [`ReTriagePolicy::RATE_FACTOR`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            rate_factor: Self::RATE_FACTOR,
        }
    }

    /// Erste echte Änderung gegenüber dem gespeicherten Stand.
    ///
    /// # Arguments
    /// - `stored` (`&KnownFinding`): Stand der letzten Triage.
    /// - `hardness` (`Hardness`): Härte des aktuellen Befunds.
    /// - `current` (`&TriageFingerprint`): Merkmale des aktuellen Befunds.
    ///
    /// # Returns
    /// `None`, wenn sich nichts Wesentliches geändert hat; sonst die erste
    /// zutreffende Änderung in der Reihenfolge Port, Datei, Härte, Rate.
    #[must_use]
    pub fn material_change(
        &self,
        stored: &KnownFinding,
        hardness: Hardness,
        current: &TriageFingerprint,
    ) -> Option<MaterialChange> {
        if current
            .port
            .is_some_and(|port| stored.fingerprint.port != Some(port))
        {
            return Some(MaterialChange::NewPort);
        }
        if current
            .exe_digest
            .as_deref()
            .is_some_and(|digest| stored.fingerprint.exe_digest.as_deref() != Some(digest))
        {
            return Some(MaterialChange::NewExecutable);
        }
        if hardness.is_stronger_than(stored.hardness) {
            return Some(MaterialChange::HigherHardness);
        }
        if let (Some(before), Some(now)) = (stored.fingerprint.rate_per_min, current.rate_per_min)
            && now > before
            && now >= before.saturating_mul(self.rate_factor)
        {
            return Some(MaterialChange::RateIncrease);
        }
        None
    }

    /// Ob [`ReTriagePolicy::material_change`] etwas findet.
    ///
    /// # Arguments
    /// Wie [`ReTriagePolicy::material_change`].
    ///
    /// # Returns
    /// `true` bei einer echten Änderung.
    #[must_use]
    pub fn is_material_change(
        &self,
        stored: &KnownFinding,
        hardness: Hardness,
        current: &TriageFingerprint,
    ) -> bool {
        self.material_change(stored, hardness, current).is_some()
    }
}

// Bekannte Schlüssel mit Ablauf und Obergrenze.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KnownKeys {
    // Nach so vielen Sekunden ohne Auftreten gilt ein Schlüssel als neu.
    ttl_secs: u64,
    // Höchstzahl gespeicherter Schlüssel.
    max_keys: usize,
    // Zeit des letzten Aufräumens; je Sekunde höchstens ein Durchlauf.
    last_prune_unix: Option<i64>,
    // Stand je `dedup_key`.
    keys: BTreeMap<String, KnownFinding>,
}

// Persistierbarer Inhalt von `KnownKeys`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct KnownKeysState {
    // Stand je `dedup_key` (als JSON-Objekt); gelesen mit höchstens
    // `MAX_KNOWN_KEYS_LIMIT` Einträgen (`read_known_keys`).
    #[serde(deserialize_with = "read_known_keys")]
    keys: BTreeMap<String, KnownFinding>,
}

// Liest die bekannten Schlüssel mit höchstens `MAX_KNOWN_KEYS_LIMIT`
// Einträgen (mehr kann keine gültige Konfiguration schreiben); der Eintrag
// darüber bricht das Lesen ab, bevor sein Wert gelesen wird. Ein doppelter
// Schlüssel ist ein Fehler.
fn read_known_keys<'de, D>(deserializer: D) -> Result<BTreeMap<String, KnownFinding>, D::Error>
where
    D: Deserializer<'de>,
{
    crate::serde_map::deserialize_object_at_most(deserializer, MAX_KNOWN_KEYS_LIMIT)
}

impl KnownKeysState {
    // Zahl der Schlüssel in O(1), ohne einen anzusehen (Vorabprüfung in
    // `Dispatcher::restore` gegen `MAX_KNOWN_KEYS_LIMIT`).
    pub(crate) fn key_count(&self) -> usize {
        self.keys.len()
    }
}

impl KnownKeys {
    // Leere Menge.
    pub(crate) fn new(ttl_secs: u32, max_keys: usize) -> Self {
        Self {
            ttl_secs: u64::from(ttl_secs),
            max_keys,
            last_prune_unix: None,
            keys: BTreeMap::new(),
        }
    }

    // Gespeicherter Stand eines Schlüssels.
    pub(crate) fn get(&self, key: &str) -> Option<&KnownFinding> {
        self.keys.get(key)
    }

    // Merkt einen triagierten Stand; ein vorhandener wird zusammengeführt.
    pub(crate) fn remember(
        &mut self,
        key: &str,
        fingerprint: &TriageFingerprint,
        hardness: Hardness,
        now_unix: i64,
    ) {
        if let Some(stored) = self.keys.get_mut(key) {
            if fingerprint.port.is_some() {
                stored.fingerprint.port = fingerprint.port;
            }
            if fingerprint.exe_digest.is_some() {
                stored
                    .fingerprint
                    .exe_digest
                    .clone_from(&fingerprint.exe_digest);
            }
            if fingerprint.rate_per_min.is_some() {
                stored.fingerprint.rate_per_min = fingerprint.rate_per_min;
            }
            if hardness.is_stronger_than(stored.hardness) {
                stored.hardness = hardness;
            }
            stored.last_seen_unix = now_unix;
            return;
        }
        self.make_room();
        self.keys.insert(
            key.to_owned(),
            KnownFinding {
                fingerprint: fingerprint.clone(),
                hardness,
                last_seen_unix: now_unix,
            },
        );
    }

    // Aktualisiert nur das letzte Auftreten.
    pub(crate) fn touch(&mut self, key: &str, now_unix: i64) {
        if let Some(stored) = self.keys.get_mut(key) {
            stored.last_seen_unix = now_unix;
        }
    }

    // Vergisst einen Schlüssel (verdrängt oder fehlgeschlagen).
    pub(crate) fn forget(&mut self, key: &str) {
        self.keys.remove(key);
    }

    // Vergisst abgelaufene Schlüssel.
    pub(crate) fn prune(&mut self, now_unix: i64) {
        if self.last_prune_unix == Some(now_unix) {
            return;
        }
        self.last_prune_unix = Some(now_unix);
        let ttl = i64::try_from(self.ttl_secs).unwrap_or(i64::MAX);
        self.keys
            .retain(|_, stored| now_unix.saturating_sub(stored.last_seen_unix) <= ttl);
    }

    // Macht Platz: das älteste Achtel fällt, damit nicht jeder neue
    // Schlüssel einen vollen Durchlauf kostet.
    fn make_room(&mut self) {
        if self.keys.len() < self.max_keys {
            return;
        }
        let keep = self
            .max_keys
            .saturating_sub(self.max_keys.checked_div(8).unwrap_or(0))
            .saturating_sub(1);
        let mut by_age: Vec<(i64, String)> = self
            .keys
            .iter()
            .map(|(key, stored)| (stored.last_seen_unix, key.clone()))
            .collect();
        by_age.sort();
        let surplus = self.keys.len().saturating_sub(keep);
        for (_, key) in by_age.into_iter().take(surplus) {
            self.keys.remove(&key);
        }
    }

    // Persistierbarer Inhalt.
    pub(crate) fn export(&self) -> KnownKeysState {
        KnownKeysState {
            keys: self.keys.clone(),
        }
    }

    // Baut die Menge aus einem Zustand: prüft jeden Schlüssel (auch die, die
    // danach fallen; ein manipulierter Eintrag lehnt den ganzen Zustand ab)
    // und kürzt dann auf `max_keys` (Grenze der aktiven Konfiguration). Es
    // bleiben die zuletzt gesehenen, bei Gleichstand der größere Schlüssel
    // (dieselbe Ordnung wie `make_room`). Ein gefallener Schlüssel gilt wieder
    // als neu, wie nach einer Verdrängung zur Laufzeit: sein nächstes
    // Auftreten wird erneut triagiert (gehört er zu einem wartenden Auftrag
    // oder einer offenen Rückfrage, auch doppelt), nie unterdrückt. Mehr als
    // `MAX_KNOWN_KEYS_LIMIT` Schlüssel lehnt `Dispatcher::restore` vorher ab,
    // ohne einen anzusehen; Prüfung und Kürzen hier sind also begrenzt.
    pub(crate) fn import(
        ttl_secs: u32,
        max_keys: usize,
        state: KnownKeysState,
    ) -> Result<Self, DispatchError> {
        let KnownKeysState { mut keys } = state;
        let valid = keys.iter().all(|(key, stored)| {
            is_dedup_key(key)
                && stored
                    .fingerprint
                    .exe_digest
                    .as_deref()
                    .is_none_or(is_record_digest)
        });
        if !valid {
            return Err(DispatchError::InvalidState { what: "known keys" });
        }
        retain_greatest(&mut keys, max_keys, |stored| stored.last_seen_unix);
        Ok(Self {
            ttl_secs: u64::from(ttl_secs),
            max_keys,
            last_prune_unix: None,
            keys,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, missing};

    fn stored(
        port: Option<u16>,
        exe: Option<&str>,
        rate: Option<u64>,
        hardness: Hardness,
    ) -> KnownFinding {
        KnownFinding {
            fingerprint: TriageFingerprint {
                port,
                exe_digest: exe.map(str::to_owned),
                rate_per_min: rate,
            },
            hardness,
            last_seen_unix: 0,
        }
    }

    fn current(port: Option<u16>, exe: Option<&str>, rate: Option<u64>) -> TriageFingerprint {
        TriageFingerprint {
            port,
            exe_digest: exe.map(str::to_owned),
            rate_per_min: rate,
        }
    }

    #[test]
    fn test_each_material_change_is_detected() -> TestResult {
        let policy = ReTriagePolicy::new();
        let exe_a = "a".repeat(64);
        let exe_b = "b".repeat(64);
        let base = stored(Some(443), Some(&exe_a), Some(10), Hardness::Correlated);
        let cases = [
            (
                current(Some(8443), Some(&exe_a), Some(10)),
                Hardness::Correlated,
                Some(MaterialChange::NewPort),
            ),
            (
                current(Some(443), Some(&exe_b), Some(10)),
                Hardness::Correlated,
                Some(MaterialChange::NewExecutable),
            ),
            (
                current(Some(443), Some(&exe_a), Some(10)),
                Hardness::Observed,
                Some(MaterialChange::HigherHardness),
            ),
            (
                current(Some(443), Some(&exe_a), Some(40)),
                Hardness::Correlated,
                Some(MaterialChange::RateIncrease),
            ),
        ];
        for (fingerprint, hardness, expected) in cases {
            assert_eq!(
                policy.material_change(&base, hardness, &fingerprint),
                expected
            );
        }
        Ok(())
    }

    #[test]
    fn test_no_change_without_a_material_difference() -> TestResult {
        let policy = ReTriagePolicy::new();
        let exe_a = "a".repeat(64);
        let base = stored(Some(443), Some(&exe_a), Some(10), Hardness::Correlated);
        let cases = [
            (
                current(Some(443), Some(&exe_a), Some(10)),
                Hardness::Correlated,
            ),
            (current(None, None, None), Hardness::Correlated),
            (current(Some(443), None, Some(39)), Hardness::Correlated),
            (
                current(Some(443), Some(&exe_a), Some(5)),
                Hardness::Inferred,
            ),
        ];
        for (fingerprint, hardness) in cases {
            assert!(
                !policy.is_material_change(&base, hardness, &fingerprint),
                "{fingerprint:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn test_rate_edge_cases() -> TestResult {
        let policy = ReTriagePolicy::new();
        let zero = stored(None, None, Some(0), Hardness::Observed);
        assert!(policy.is_material_change(
            &zero,
            Hardness::Observed,
            &current(None, None, Some(1))
        ));
        assert!(!policy.is_material_change(
            &zero,
            Hardness::Observed,
            &current(None, None, Some(0))
        ));
        let unknown = stored(None, None, None, Hardness::Observed);
        assert!(!policy.is_material_change(
            &unknown,
            Hardness::Observed,
            &current(None, None, Some(1_000))
        ));
        let huge = stored(None, None, Some(u64::MAX), Hardness::Observed);
        assert!(!policy.is_material_change(
            &huge,
            Hardness::Observed,
            &current(None, None, Some(u64::MAX))
        ));
        // Ein neuer Port gegenüber „kein Port" ist eine Änderung.
        assert!(policy.is_material_change(
            &unknown,
            Hardness::Observed,
            &current(Some(22), None, None)
        ));
        Ok(())
    }

    #[test]
    fn test_known_keys_merge_expire_and_bound() -> TestResult {
        let mut keys = KnownKeys::new(100, 8);
        let key = "0".repeat(31) + "1";
        keys.remember(
            &key,
            &current(Some(80), None, Some(10)),
            Hardness::Inferred,
            0,
        );
        keys.remember(&key, &current(None, None, Some(50)), Hardness::Observed, 5);
        let merged = keys.get(&key).ok_or_else(missing("merged"))?;
        assert_eq!(merged.fingerprint.port, Some(80));
        assert_eq!(merged.fingerprint.rate_per_min, Some(50));
        assert_eq!(merged.hardness, Hardness::Observed);
        keys.prune(105);
        assert!(keys.get(&key).is_some());
        keys.prune(106);
        assert!(keys.get(&key).is_none());
        for n in 0..20_u32 {
            keys.remember(
                &format!("{n:032x}"),
                &TriageFingerprint::default(),
                Hardness::Observed,
                i64::from(n),
            );
        }
        assert!(keys.keys.len() <= 8);
        // Die jüngsten bleiben.
        assert!(keys.get(&format!("{:032x}", 19)).is_some());
        Ok(())
    }

    #[test]
    fn test_import_rejects_malformed_keys() -> TestResult {
        let mut keys = KnownKeys::new(100, 8);
        keys.remember(
            &"f".repeat(32),
            &TriageFingerprint::default(),
            Hardness::Observed,
            0,
        );
        assert!(KnownKeys::import(100, 8, keys.export()).is_ok());
        keys.keys.insert(
            "not-a-key".to_owned(),
            stored(None, None, None, Hardness::Observed),
        );
        assert!(KnownKeys::import(100, 8, keys.export()).is_err());
        Ok(())
    }

    #[test]
    fn test_import_prunes_to_the_active_bound_keeping_the_most_recent() -> TestResult {
        // Geschrieben mit Grenze 1000: 40 Schlüssel, Schlüssel n zuletzt bei
        // 100 + n gesehen.
        let mut keys = KnownKeys::new(100_000, 1_000);
        for n in 1..=40_u32 {
            keys.remember(
                &format!("{n:032x}"),
                &TriageFingerprint::default(),
                Hardness::Observed,
                i64::from(n).saturating_add(100),
            );
        }
        assert_eq!(keys.keys.len(), 40);
        // Unverändert unter derselben Grenze.
        let same = KnownKeys::import(100_000, 1_000, keys.export())?;
        assert_eq!(same.export(), keys.export());
        // Grenze 5: genau die fünf jüngsten.
        let five = KnownKeys::import(100_000, 5, keys.export())?;
        let kept: Vec<&str> = five.keys.keys().map(String::as_str).collect();
        let expected: Vec<String> = (36..=40_u32).map(|n| format!("{n:032x}")).collect();
        assert_eq!(kept, expected);
        // Konfigurationswechsel 1000 → 1: nur der jüngste.
        let one = KnownKeys::import(100_000, 1, keys.export())?;
        assert_eq!(one.keys.len(), 1);
        assert!(one.get(&format!("{:032x}", 40)).is_some());
        // Ein manipulierter Schlüssel lehnt ab, auch wenn er fallen würde.
        let mut tampered = keys.export();
        tampered.keys.insert(
            "not-a-key".to_owned(),
            stored(None, None, None, Hardness::Observed),
        );
        assert_eq!(
            KnownKeys::import(100_000, 1, tampered).err(),
            Some(DispatchError::InvalidState { what: "known keys" })
        );
        Ok(())
    }

    #[test]
    fn test_import_breaks_ties_by_the_greater_key() -> TestResult {
        let mut keys = KnownKeys::new(100_000, 1_000);
        for n in [7_u32, 3, 9, 1] {
            keys.remember(
                &format!("{n:032x}"),
                &TriageFingerprint::default(),
                Hardness::Observed,
                500,
            );
        }
        let two = KnownKeys::import(100_000, 2, keys.export())?;
        let kept: Vec<&str> = two.keys.keys().map(String::as_str).collect();
        assert_eq!(kept, [format!("{:032x}", 7), format!("{:032x}", 9)]);
        Ok(())
    }
}
