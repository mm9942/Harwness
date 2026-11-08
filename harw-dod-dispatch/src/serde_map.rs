//! Serde-Hilfe: eine `BTreeMap` als Liste von Paaren.
//!
//! # Zweck
//! JSON-Objekte erlauben nur Zeichenketten als Schlüssel; die Zustände
//! dieser Crate haben zusammengesetzte Schlüssel ([`crate::PartitionKey`])
//! und Zahlen (`cgroup_kid`). Als `[[key, value], …]` serialisiert, bleibt
//! der Zustand in jedem Serde-Format (JSON, TOML, bincode …) darstellbar.
//! Ein doppelter Schlüssel beim Lesen ist ein Fehler, kein „last wins".
//!
//! # Nebenläufigkeit und Fehler
//! Zustandslos. Lesefehler kommen als `D::Error` des Formats zurück.
//!
//! # Examples
//! ```ignore
//! #[derive(serde::Serialize, serde::Deserialize)]
//! struct State {
//!     #[serde(with = "crate::serde_map")]
//!     by_key: std::collections::BTreeMap<u64, String>,
//! }
//! ```

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

// Schreibt die Paare in Schlüsselreihenfolge.
pub(crate) fn serialize<K, V, S>(map: &BTreeMap<K, V>, serializer: S) -> Result<S::Ok, S::Error>
where
    K: Serialize,
    V: Serialize,
    S: Serializer,
{
    serializer.collect_seq(map.iter())
}

// Liest die Paare; ein doppelter Schlüssel ist ein Fehler.
pub(crate) fn deserialize<'de, K, V, D>(deserializer: D) -> Result<BTreeMap<K, V>, D::Error>
where
    K: Ord + Deserialize<'de>,
    V: Deserialize<'de>,
    D: Deserializer<'de>,
{
    let pairs = Vec::<(K, V)>::deserialize(deserializer)?;
    let mut map = BTreeMap::new();
    for (key, value) in pairs {
        if map.insert(key, value).is_some() {
            return Err(<D::Error as serde::de::Error>::custom(
                "duplicate map key in dispatch state",
            ));
        }
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde::{Deserialize, Serialize};

    use crate::test_support::TestResult;

    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Wrapper {
        #[serde(with = "crate::serde_map")]
        map: BTreeMap<u64, String>,
    }

    #[test]
    fn test_round_trip_and_duplicate_rejection() -> TestResult {
        let wrapper = Wrapper {
            map: BTreeMap::from([(2, "b".to_owned()), (1, "a".to_owned())]),
        };
        let json = serde_json::to_string(&wrapper)?;
        assert_eq!(json, r#"{"map":[[1,"a"],[2,"b"]]}"#);
        let back: Wrapper = serde_json::from_str(&json)?;
        assert_eq!(back, wrapper);
        let duplicate = serde_json::from_str::<Wrapper>(r#"{"map":[[1,"a"],[1,"b"]]}"#);
        assert!(duplicate.is_err());
        Ok(())
    }
}
