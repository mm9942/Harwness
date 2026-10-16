//! Abbildung von Tool-Call-IDs auf das Wire-Format eines Providers.
//!
//! Die Mistral-API (`api.mistral.ai`) lehnt jede `tool_call_id` ab, die nicht
//! aus genau 9 Zeichen `a-z A-Z 0-9` besteht (HTTP 400, "Tool call id was …
//! but must be a-z, A-Z, 0-9, with a length of 9"). IDs anderer Anbieter
//! (`call_1740241912345`, `toolu_…`) und unsere synthetischen
//! `call_text_<n>` erfüllen das nicht; sie landen im Verlauf, sobald eine
//! Sitzung den Anbieter wechselt oder ein Text-Aufruf wiederhergestellt wurde,
//! und brechen dann jede Folge-Anfrage.
//!
//! [`ToolCallIdCodec`] bildet **pro Anfrage** jede ID des Verlaufs ab:
//!
//! - Eine ID, die das Format schon erfüllt (etwa eine von Mistral selbst
//!   vergebene), bleibt bytegleich (Prompt-Cache, Nachvollziehbarkeit).
//! - Jede andere ID wird zu einem 9-stelligen Base62-Hash ihrer selbst. Aufruf
//!   und Ergebnis tragen dieselbe ID, weil beide durch dieselbe Abbildung gehen.
//! - Treffen zwei verschiedene IDs auf denselben Wert (oder auf eine gültige
//!   ID des Verlaufs), bekommt die spätere einen neuen Versuch mit anderem
//!   Salz. Die Reihenfolge ist die des Verlaufs: dieselbe Anfrage ergibt
//!   bytegleiche IDs.
//!
//! Nur die ausgehende Richtung wird abgebildet. Antworten tragen IDs des
//! Providers, die im Verlauf bleiben und bei der nächsten Anfrage wieder
//! unverändert durchgehen.

use harw_core::{ModelMessage, ModelRequest};
use std::collections::{BTreeMap, BTreeSet};

/// Länge einer Mistral-ID.
const WIRE_ID_LEN: usize = 9;

/// `62^9`: Anzahl der möglichen 9-stelligen Base62-Werte.
const ID_SPACE: u64 = 13_537_086_546_263_552;

const ALPHABET: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// Wie ein Provider Tool-Call-IDs erwartet.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum ToolCallIdFormat {
    /// Unverändert senden (OpenAI und kompatible Server).
    #[default]
    Verbatim,
    /// Genau 9 Zeichen `a-z A-Z 0-9` (Mistral-API).
    Alnum9,
}

/// Die Abbildung für genau eine Anfrage.
#[derive(Debug, Default)]
pub(crate) struct ToolCallIdCodec {
    /// Leer bei [`ToolCallIdFormat::Verbatim`]: `encode` gibt die ID zurück.
    wire: BTreeMap<String, String>,
}

impl ToolCallIdCodec {
    /// Baut die Abbildung aus allen IDs des Verlaufs.
    pub(crate) fn for_request(request: &ModelRequest, format: ToolCallIdFormat) -> Self {
        match format {
            ToolCallIdFormat::Verbatim => Self::default(),
            ToolCallIdFormat::Alnum9 => {
                let ids = request
                    .history
                    .to_model_messages()
                    .into_iter()
                    .filter_map(|message| match message {
                        ModelMessage::ToolCall { call_id, .. }
                        | ModelMessage::ToolResult { call_id, .. } => {
                            Some(call_id.as_str().to_owned())
                        }
                        _ => None,
                    });
                Self::alnum9(ids)
            }
        }
    }

    /// Die Abbildung für [`ToolCallIdFormat::Alnum9`] aus IDs in
    /// Verlaufsreihenfolge (Wiederholungen sind erlaubt).
    fn alnum9(ids: impl IntoIterator<Item = String>) -> Self {
        let ids: Vec<String> = ids.into_iter().collect();
        let mut wire = BTreeMap::new();
        let mut taken = BTreeSet::new();
        // Gültige IDs bleiben, wie sie sind, und sind damit vergeben.
        for id in ids.iter().filter(|id| is_valid(id)) {
            wire.insert(id.clone(), id.clone());
            taken.insert(id.clone());
        }
        for id in &ids {
            if wire.contains_key(id) {
                continue;
            }
            let mut salt = 0_u32;
            let chosen = loop {
                let candidate = hashed(id, salt);
                if taken.insert(candidate.clone()) {
                    break candidate;
                }
                salt += 1;
            };
            wire.insert(id.clone(), chosen);
        }
        Self { wire }
    }

    /// Die ID, wie sie auf den Draht geht.
    pub(crate) fn encode(&self, id: &str) -> String {
        self.wire.get(id).cloned().unwrap_or_else(|| id.to_owned())
    }
}

/// `true`, wenn `id` genau 9 Zeichen `a-z A-Z 0-9` hat.
fn is_valid(id: &str) -> bool {
    id.len() == WIRE_ID_LEN && id.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

/// 9 Base62-Zeichen aus FNV-1a 64 über `salt` und `id`. Der Hash dient nur der
/// Verteilung, nicht der Sicherheit; Kollisionen löst der Aufrufer auf.
fn hashed(id: &str, salt: u32) -> String {
    let mut state: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in salt.to_le_bytes().into_iter().chain(id.bytes()) {
        state ^= u64::from(byte);
        state = state.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let mut value = state % ID_SPACE;
    let mut out = [b'0'; WIRE_ID_LEN];
    for slot in out.iter_mut().rev() {
        *slot = ALPHABET[(value % 62) as usize];
        value /= 62;
    }
    out.iter().map(|byte| char::from(*byte)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;

    fn codec(ids: &[&str]) -> ToolCallIdCodec {
        ToolCallIdCodec::alnum9(ids.iter().map(|id| (*id).to_owned()))
    }

    #[test]
    fn test_foreign_ids_become_nine_alphanumeric_characters() {
        let codec = codec(&[
            "call_1740241912345",
            "toolu_01A09q90qw90lq917835lq9",
            "call_text_0",
        ]);
        for id in [
            "call_1740241912345",
            "toolu_01A09q90qw90lq917835lq9",
            "call_text_0",
        ] {
            let wire = codec.encode(id);
            assert!(is_valid(&wire), "{id} -> {wire}");
        }
    }

    #[test]
    fn test_a_valid_id_stays_byte_identical() {
        let codec = codec(&["D681PevKs", "call_1"]);
        assert_eq!(codec.encode("D681PevKs"), "D681PevKs");
    }

    #[test]
    fn test_call_and_result_share_the_same_wire_id() {
        // Aufruf und Ergebnis nennen dieselbe ID; die Abbildung ist eine Funktion.
        let codec = codec(&["call_a", "call_b", "call_a"]);
        assert_eq!(codec.encode("call_a"), codec.encode("call_a"));
        assert_ne!(codec.encode("call_a"), codec.encode("call_b"));
    }

    #[test]
    fn test_the_mapping_is_stable_across_requests() {
        let first = codec(&["call_a", "call_b"]);
        let second = codec(&["call_a", "call_b"]);
        assert_eq!(first.encode("call_b"), second.encode("call_b"));
    }

    #[test]
    fn test_an_id_that_hashes_onto_a_valid_one_is_moved_away() {
        // Die gültige ID ist genau der Hash von "call_a": die fremde ID darf
        // sie nicht mitbenutzen, sonst gäbe es zwei Aufrufe mit einer ID.
        let clash = hashed("call_a", 0);
        let codec = codec(&["call_a", clash.as_str()]);
        assert_eq!(codec.encode(&clash), clash);
        assert_ne!(codec.encode("call_a"), clash);
        assert!(is_valid(&codec.encode("call_a")));
    }

    #[test]
    fn test_many_ids_stay_distinct() {
        let ids: Vec<String> = (0..2000).map(|n| format!("call_{n}")).collect();
        let codec = ToolCallIdCodec::alnum9(ids.clone());
        let wired: BTreeSet<String> = ids.iter().map(|id| codec.encode(id)).collect();
        assert_eq!(wired.len(), ids.len());
    }

    #[test]
    fn test_verbatim_leaves_every_id_alone() -> TestResult {
        let codec = ToolCallIdCodec::default();
        assert_eq!(codec.encode("call_1740241912345"), "call_1740241912345");
        Ok(())
    }

    #[test]
    fn test_the_id_space_matches_nine_base62_digits() {
        assert_eq!(ID_SPACE, 62_u64.pow(9));
    }
}
