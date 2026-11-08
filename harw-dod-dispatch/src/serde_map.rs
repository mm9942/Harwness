//! Serde-Hilfe: Tabellen eines persistierten Zustands, beim Lesen mit
//! Eintragsgrenze.
//!
//! # Zweck
//! 1. **Paarlisten:** JSON-Objekte erlauben nur Zeichenketten als
//!    Schlüssel; die Zustände dieser Crate haben zusammengesetzte Schlüssel
//!    ([`crate::PartitionKey`]) und Zahlen (`cgroup_kid`). Als
//!    `[[key, value], …]` serialisiert (`serialize`), bleibt der Zustand in
//!    jedem Serde-Format (JSON, TOML, bincode …) darstellbar.
//! 2. **Eintragsgrenze beim Lesen** (PL-100 WP2.1, Review 4505196829): ein
//!    persistierter Zustand ist untrusted, und innerhalb der Bytegrenze
//!    ([`crate::MAX_STATE_BYTES`]) passen Millionen winziger Einträge
//!    (`[n,0],` sind 6 bis 13 Bytes). Deshalb liest jede Tabelle mit ihrem
//!    absoluten Höchstwert: `deserialize_pairs_at_most` (Paarliste →
//!    `BTreeMap`), `deserialize_object_at_most` (JSON-Objekt → `BTreeMap`)
//!    und `deserialize_vec_at_most` (Liste → `Vec`). Paare gehen ohne
//!    Zwischenliste direkt in die Tabelle, eine Liste wächst höchstens bis
//!    zu ihrer Grenze. Ist die Grenze erreicht und folgt noch ein Eintrag,
//!    bricht das Lesen mit der festen Meldung `TOO_MANY_ENTRIES` ab, bevor
//!    das Format ein Byte dieses Eintrags parst: ein `DeserializeSeed`
//!    (`RejectEntry`) scheitert, ohne den Deserializer anzufassen;
//!    `serde_json` hat davor nur das Trennzeichen gelesen. Der Eintrag
//!    kostet also weder Zeit noch Speicher (auch nicht den Stapel, mit dem
//!    `serde_json` einen übersprungenen Wert durchläuft, ein Byte je
//!    Verschachtelungsebene), und der Rest der Eingabe wird nie geparst.
//!    Ein unbegrenzter Leseweg existiert in diesem Modul nicht.
//!
//! Ein doppelter Schlüssel beim Lesen ist ein Fehler, kein „last wins".
//!
//! # Nebenläufigkeit und Fehler
//! Zustandslos. Lesefehler kommen als `D::Error` des Formats zurück; die
//! Meldungen sind feste Texte ohne Inhalt der Eingabe.
//!
//! # Examples
//! ```ignore
//! #[derive(serde::Serialize, serde::Deserialize)]
//! struct State {
//!     #[serde(
//!         serialize_with = "crate::serde_map::serialize",
//!         deserialize_with = "read_at_most_eight"
//!     )]
//!     by_key: std::collections::BTreeMap<u64, String>,
//! }
//!
//! fn read_at_most_eight<'de, D: serde::Deserializer<'de>>(
//!     deserializer: D,
//! ) -> Result<std::collections::BTreeMap<u64, String>, D::Error> {
//!     crate::serde_map::deserialize_pairs_at_most(deserializer, 8)
//! }
//! ```

use std::collections::BTreeMap;
use std::fmt;
use std::marker::PhantomData;

use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

// Feste Meldung, wenn eine Tabelle beim Lesen ihren Höchstwert
// überschreitet. `DispatchState::from_json_bounded` erkennt sie am Anfang
// der Fehlermeldung (kein anderer Lesefehler beginnt so: die Meldungen von
// serde und `serde_json` beginnen mit festen eigenen Wörtern, nie mit
// Eingabetext).
pub(crate) const TOO_MANY_ENTRIES: &str = "too many entries in a dispatch state table";

// Feste Meldung bei einem doppelten Schlüssel.
const DUPLICATE_KEY: &str = "duplicate map key in dispatch state";

// Schreibt die Paare in Schlüsselreihenfolge.
pub(crate) fn serialize<K, V, S>(map: &BTreeMap<K, V>, serializer: S) -> Result<S::Ok, S::Error>
where
    K: Serialize,
    V: Serialize,
    S: Serializer,
{
    serializer.collect_seq(map.iter())
}

// Liest eine Paarliste mit höchstens `limit` Paaren direkt in eine
// `BTreeMap`; ein doppelter Schlüssel und ein Paar über `limit` sind
// Fehler.
pub(crate) fn deserialize_pairs_at_most<'de, K, V, D>(
    deserializer: D,
    limit: usize,
) -> Result<BTreeMap<K, V>, D::Error>
where
    K: Ord + Deserialize<'de>,
    V: Deserialize<'de>,
    D: Deserializer<'de>,
{
    deserializer.deserialize_seq(PairsAtMost {
        limit,
        marker: PhantomData,
    })
}

// Liest ein Objekt (Map) mit höchstens `limit` Einträgen direkt in eine
// `BTreeMap`; ein doppelter Schlüssel und ein Eintrag über `limit` sind
// Fehler.
pub(crate) fn deserialize_object_at_most<'de, K, V, D>(
    deserializer: D,
    limit: usize,
) -> Result<BTreeMap<K, V>, D::Error>
where
    K: Ord + Deserialize<'de>,
    V: Deserialize<'de>,
    D: Deserializer<'de>,
{
    deserializer.deserialize_map(ObjectAtMost {
        limit,
        marker: PhantomData,
    })
}

// Liest eine Liste mit höchstens `limit` Elementen; ein Element über `limit`
// ist ein Fehler.
pub(crate) fn deserialize_vec_at_most<'de, T, D>(
    deserializer: D,
    limit: usize,
) -> Result<Vec<T>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    deserializer.deserialize_seq(VecAtMost {
        limit,
        marker: PhantomData,
    })
}

// `DeserializeSeed` für einen Eintrag über der Grenze: scheitert mit
// `TOO_MANY_ENTRIES`, ohne den Deserializer anzufassen. Das Format hat davor
// höchstens das Trennzeichen gelesen; vom Eintrag selbst (bei einem Objekt
// auch vom Schlüssel) parst es kein Byte.
struct RejectEntry;

impl<'de> DeserializeSeed<'de> for RejectEntry {
    type Value = ();

    fn deserialize<D>(self, _deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        Err(<D::Error as de::Error>::custom(TOO_MANY_ENTRIES))
    }
}

// Die Grenze ist erreicht: folgt noch ein Element, bricht das Lesen ab, bevor
// es geparst wird (`RejectEntry`).
fn reject_further_elements<'de, A>(seq: &mut A) -> Result<(), A::Error>
where
    A: SeqAccess<'de>,
{
    match seq.next_element_seed(RejectEntry)? {
        None => Ok(()),
        // `RejectEntry` scheitert schon selbst; fail-closed, falls ein Format
        // trotzdem einen Wert meldet.
        Some(()) => Err(<A::Error as de::Error>::custom(TOO_MANY_ENTRIES)),
    }
}

// Besucher einer Paarliste mit Grenze.
struct PairsAtMost<K, V> {
    // Höchstzahl der Paare.
    limit: usize,
    // Schlüssel- und Werttyp.
    marker: PhantomData<fn() -> (K, V)>,
}

impl<'de, K, V> Visitor<'de> for PairsAtMost<K, V>
where
    K: Ord + Deserialize<'de>,
    V: Deserialize<'de>,
{
    type Value = BTreeMap<K, V>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "a list of at most {} key-value pairs",
            self.limit
        )
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut map = BTreeMap::new();
        loop {
            if map.len() >= self.limit {
                reject_further_elements(&mut seq)?;
                return Ok(map);
            }
            let Some((key, value)) = seq.next_element::<(K, V)>()? else {
                return Ok(map);
            };
            if map.insert(key, value).is_some() {
                return Err(<A::Error as de::Error>::custom(DUPLICATE_KEY));
            }
        }
    }
}

// Besucher eines Objekts mit Grenze.
struct ObjectAtMost<K, V> {
    // Höchstzahl der Einträge.
    limit: usize,
    // Schlüssel- und Werttyp.
    marker: PhantomData<fn() -> (K, V)>,
}

impl<'de, K, V> Visitor<'de> for ObjectAtMost<K, V>
where
    K: Ord + Deserialize<'de>,
    V: Deserialize<'de>,
{
    type Value = BTreeMap<K, V>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "a map of at most {} entries", self.limit)
    }

    fn visit_map<A>(self, mut access: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut map = BTreeMap::new();
        loop {
            if map.len() >= self.limit {
                // Ein weiterer Eintrag wird nicht geparst, auch sein
                // Schlüssel nicht (`RejectEntry`).
                return match access.next_key_seed(RejectEntry)? {
                    None => Ok(map),
                    // Wie in `reject_further_elements`: fail-closed.
                    Some(()) => Err(<A::Error as de::Error>::custom(TOO_MANY_ENTRIES)),
                };
            }
            let Some((key, value)) = access.next_entry::<K, V>()? else {
                return Ok(map);
            };
            if map.insert(key, value).is_some() {
                return Err(<A::Error as de::Error>::custom(DUPLICATE_KEY));
            }
        }
    }
}

// Besucher einer Liste mit Grenze.
struct VecAtMost<T> {
    // Höchstzahl der Elemente.
    limit: usize,
    // Elementtyp.
    marker: PhantomData<fn() -> T>,
}

impl<'de, T> Visitor<'de> for VecAtMost<T>
where
    T: Deserialize<'de>,
{
    type Value = Vec<T>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "a list of at most {} elements", self.limit)
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut items = Vec::new();
        loop {
            if items.len() >= self.limit {
                reject_further_elements(&mut seq)?;
                return Ok(items);
            }
            let Some(item) = seq.next_element::<T>()? else {
                return Ok(items);
            };
            items.push(item);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde::{Deserialize, Deserializer, Serialize};

    use super::{
        TOO_MANY_ENTRIES, deserialize_object_at_most, deserialize_pairs_at_most,
        deserialize_vec_at_most,
    };
    use crate::test_support::{TestError, TestResult};

    // Grenze der Testtabellen.
    const LIMIT: usize = 2;

    // Paarliste mit höchstens `LIMIT` Paaren.
    fn read_pairs<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<BTreeMap<u64, String>, D::Error> {
        deserialize_pairs_at_most(deserializer, LIMIT)
    }

    // Objekt mit höchstens `LIMIT` Einträgen.
    fn read_object<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<BTreeMap<String, u64>, D::Error> {
        deserialize_object_at_most(deserializer, LIMIT)
    }

    // Liste mit höchstens `LIMIT` Elementen.
    fn read_list<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u64>, D::Error> {
        deserialize_vec_at_most(deserializer, LIMIT)
    }

    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Pairs {
        #[serde(
            serialize_with = "crate::serde_map::serialize",
            deserialize_with = "read_pairs"
        )]
        map: BTreeMap<u64, String>,
    }

    #[derive(Debug, PartialEq, Eq, Deserialize)]
    struct Object {
        #[serde(deserialize_with = "read_object")]
        map: BTreeMap<String, u64>,
    }

    #[derive(Debug, PartialEq, Eq, Deserialize)]
    struct List {
        #[serde(deserialize_with = "read_list")]
        items: Vec<u64>,
    }

    // Die Meldung eines erwarteten Lesefehlers.
    fn error_of<T: std::fmt::Debug>(result: Result<T, serde_json::Error>) -> TestResult<String> {
        match result {
            Ok(value) => Err(TestError::Unexpected(format!("decoded {value:?}"))),
            Err(error) => Ok(error.to_string()),
        }
    }

    #[test]
    fn test_round_trip_and_duplicate_rejection() -> TestResult {
        let pairs = Pairs {
            map: BTreeMap::from([(2, "b".to_owned()), (1, "a".to_owned())]),
        };
        let json = serde_json::to_string(&pairs)?;
        assert_eq!(json, r#"{"map":[[1,"a"],[2,"b"]]}"#);
        let back: Pairs = serde_json::from_str(&json)?;
        assert_eq!(back, pairs);
        let duplicate = error_of(serde_json::from_str::<Pairs>(
            r#"{"map":[[1,"a"],[1,"b"]]}"#,
        ))?;
        assert!(duplicate.starts_with("duplicate map key"), "{duplicate}");
        let duplicate = error_of(serde_json::from_str::<Object>(r#"{"map":{"a":1,"a":2}}"#))?;
        assert!(duplicate.starts_with("duplicate map key"), "{duplicate}");
        Ok(())
    }

    #[test]
    fn test_each_reader_accepts_its_limit_exactly() -> TestResult {
        let pairs: Pairs = serde_json::from_str(r#"{"map":[[1,"a"],[2,"b"]]}"#)?;
        assert_eq!(pairs.map.len(), LIMIT);
        let object: Object = serde_json::from_str(r#"{"map":{"a":1,"b":2}}"#)?;
        assert_eq!(object.map.len(), LIMIT);
        let list: List = serde_json::from_str(r#"{"items":[1,2]}"#)?;
        assert_eq!(list.items, [1, 2]);
        // Leer ist ebenso gültig.
        let empty: List = serde_json::from_str(r#"{"items":[]}"#)?;
        assert!(empty.items.is_empty());
        Ok(())
    }

    #[test]
    fn test_one_entry_over_the_limit_stops_reading_before_its_content() -> TestResult {
        // Der Eintrag über der Grenze hat den falschen Typ, und dahinter
        // steht kein JSON mehr. Würde er typisiert oder der Rest geparst,
        // käme ein Typ- bzw. Syntaxfehler; es kommt die Grenze.
        let cases = [
            error_of(serde_json::from_str::<Pairs>(
                r#"{"map":[[1,"a"],[2,"b"],"not a pair",!!!"#,
            ))?,
            error_of(serde_json::from_str::<Object>(
                r#"{"map":{"a":1,"b":2,"c":"not a number",!!!"#,
            ))?,
            error_of(serde_json::from_str::<List>(
                r#"{"items":[1,2,"not a number",!!!"#,
            ))?,
        ];
        for message in cases {
            assert!(message.starts_with(TOO_MANY_ENTRIES), "{message}");
        }
        // Ein gültiger Eintrag über der Grenze ebenso.
        let message = error_of(serde_json::from_str::<List>(r#"{"items":[1,2,3]}"#))?;
        assert!(message.starts_with(TOO_MANY_ENTRIES), "{message}");
        // Unter der Grenze bleibt ein falscher Typ ein Typfehler.
        let message = error_of(serde_json::from_str::<List>(r#"{"items":[1,"x"]}"#))?;
        assert!(message.starts_with("invalid type"), "{message}");
        Ok(())
    }

    #[test]
    fn test_the_entry_over_the_limit_is_never_parsed() -> TestResult {
        // Der Eintrag über der Grenze ist selbst kein JSON (bzw. sein
        // Schlüssel endet nie). Würde er auch nur übersprungen, käme ein
        // Syntaxfehler; es kommt die Grenze, also ist von ihm kein Byte
        // geparst.
        let cases = [
            error_of(serde_json::from_str::<Pairs>(
                r#"{"map":[[1,"a"],[2,"b"],!!!"#,
            ))?,
            error_of(serde_json::from_str::<Object>(r#"{"map":{"a":1,"b":2,"c"#))?,
            error_of(serde_json::from_str::<List>(r#"{"items":[1,2,!!!"#))?,
        ];
        for message in cases {
            assert!(message.starts_with(TOO_MANY_ENTRIES), "{message}");
        }
        // Eine tief verschachtelte Liste über der Grenze, nie geschlossen:
        // übersprungen kostete sie einen Stapel von einem Byte je Ebene und
        // endete mit „EOF"; so wird keine Ebene geöffnet.
        let deep = format!(r#"{{"items":[1,2,{}"#, "[".repeat(1 << 16));
        let message = error_of(serde_json::from_str::<List>(&deep))?;
        assert!(message.starts_with(TOO_MANY_ENTRIES), "{message}");
        // Unter der Grenze ist dieselbe Liste ein Syntax- bzw. Typfehler,
        // die Eingaben oben also wirklich kein gültiger Eintrag.
        let message = error_of(serde_json::from_str::<List>(r#"{"items":[1,!!!"#))?;
        assert!(!message.starts_with(TOO_MANY_ENTRIES), "{message}");
        let shallow = format!(r#"{{"items":[1,{}"#, "[".repeat(1 << 16));
        let message = error_of(serde_json::from_str::<List>(&shallow))?;
        assert!(!message.starts_with(TOO_MANY_ENTRIES), "{message}");
        Ok(())
    }
}
