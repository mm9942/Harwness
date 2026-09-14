//! Die inhaltsfreie Ablehnungskategorie: [`Denial`].
//!
//! # Verantwortungsbereich
//! `Denial` ist der einzige Werttyp, den der Durchsetzer bei einer
//! Ablehnung über die Leitung zurückgibt (eingebettet in
//! [`crate::response::WardenResponse::Denied`]). Er ist bewusst **feldlos**:
//! jede Variante trägt null Nutzdaten, sodass eine Ablehnung strukturell —
//! nicht nur durch Konvention — nichts über den genauen Grund verraten
//! *kann*. Der Durchsetzer läuft privilegiert; was er meldet, verlässt
//! seinen Vertrauensbereich (siehe `lib.rs`-Moduldoku). Der feinkörnige
//! Grund (welches Feld genau nicht passte) bleibt in
//! [`crate::error::WardenProtoError`] — einem separaten, nie serialisierten
//! Typ, der das Prozessinnere nie verlässt (siehe dortige Moduldoku).
//!
//! [`crate::error::WardenProtoError::as_denial`] bildet die interne,
//! detailreichere Fehlersicht auf genau diese groben Kategorien ab — das ist
//! die einzige Stelle, an der Detail bewusst fallen gelassen wird, bevor
//! eine Aussage über die Leitung geht.
//!
//! # Warum kein `#[derive(harw_macros::HarwError)]`
//! `Denial` ist kein `std::error::Error` dieser Crate im Sinne von
//! Contract-Master §H.1 („ein Fehlertyp je Crate") — dieser Platz ist
//! [`crate::error::WardenProtoError`] vorbehalten. `Denial` ist ein
//! Wire-Datentyp wie
//! [`crate::action::WardenAction`], zufällig mit einer `Display`-Impl für
//! Logging-Bequemlichkeit auf der vertrauenswürdigen Seite.
//!
//! # Nebenläufigkeit
//! `Copy`, keine interne Veränderlichkeit.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Warum eine vorgeschlagene Aktion nicht ausgeführt wurde — ohne zu sagen,
/// *was* genau nicht passte.
///
/// # Description
/// Ursprünglich zwei Kategorien (Bindung, Zulässigkeit); seit Proof v2
/// (C-WPROTO, F-088) zusätzlich Versions-, Dekodier-, Nicht-unterstützt-,
/// Ausführungs- und Verfügbarkeitskategorien, damit der Warden jede
/// Ablehnung beantworten kann, statt die Verbindung kommentarlos zu schließen.
/// Alle Varianten bleiben feldlos — ein zusätzlicher Wert wäre nur über eine
/// sichtbare Erweiterung dieser Positivliste möglich.
///
/// # Wire-Format
/// Kebab-case-String, keine weiteren Felder.
///
/// # Examples
/// ```rust
/// use harw_dod_warden_proto::Denial;
///
/// let denial = Denial::ProofMismatch;
/// assert_eq!(
///     denial.to_string(),
///     "authorization proof does not authorize the requested action"
/// );
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Denial {
    /// Der Beleg ist nicht an den angeforderten Befund oder die angeforderte
    /// Aktion gebunden (siehe [`crate::proof::AuthorizationProof::authorizes`]).
    ProofMismatch,
    /// Die Aktion ist ab der im Beleg genannten Eskalationsstufe nicht
    /// zulässig (siehe [`crate::action::WardenAction::is_admissible_from`]).
    NotAdmissibleAtStage,
    /// Die Anfrage nennt eine nicht (mehr) unterstützte Protokoll-/Proof-Version
    /// (F-088; v1 wird nicht akzeptiert).
    UnsupportedVersion,
    /// Die Anfrage war nicht dekodierbar oder zu groß (F-088).
    Malformed,
    /// Die Aktion ist in dieser Warden-Ausprägung nicht implementiert
    /// (z. B. `IsolateNetwork`, Plan Teil B Annahme A2).
    Unsupported,
    /// Die Prüfung war erfolgreich, die Durchsetzung selbst ist fehlgeschlagen (F-088).
    ExecutionFailed,
    /// Der Warden kann gerade nicht sicher prüfen (z. B. Nonce-Ledger nicht
    /// schreibbar) — fail closed.
    Unavailable,
}

/// Formatiert die Ablehnung als feste, inhaltsfreie Meldung.
///
/// # Description
/// Jede Variante hat genau eine feste Meldung ohne interpolierte Werte —
/// `Denial` trägt ohnehin keine Felder, also gibt es nichts zu interpolieren.
///
/// # Examples
/// ```rust
/// use harw_dod_warden_proto::Denial;
///
/// assert_eq!(
///     Denial::NotAdmissibleAtStage.to_string(),
///     "action is not admissible at the proof's escalation stage"
/// );
/// ```
impl fmt::Display for Denial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::ProofMismatch => "authorization proof does not authorize the requested action",
            Self::NotAdmissibleAtStage => {
                "action is not admissible at the proof's escalation stage"
            }
            Self::UnsupportedVersion => "unsupported warden protocol version",
            Self::Malformed => "malformed warden request",
            Self::Unsupported => "action is not supported by this warden",
            Self::ExecutionFailed => "action could not be executed",
            Self::Unavailable => "warden is temporarily unable to verify requests",
        };
        f.write_str(text)
    }
}

#[cfg(test)]
mod tests {
    use super::Denial;

    const ALL: [Denial; 7] = [
        Denial::ProofMismatch,
        Denial::NotAdmissibleAtStage,
        Denial::UnsupportedVersion,
        Denial::Malformed,
        Denial::Unsupported,
        Denial::ExecutionFailed,
        Denial::Unavailable,
    ];

    #[test]
    fn test_display_contains_no_embedded_identifiers() {
        // "Inhaltsfrei" heißt hier konkret: die Meldung ist eine der zwei
        // festen Zeichenketten und enthält insbesondere keine der
        // Kennungs-Präfixe, die andere Wire-Typen dieser Crate verwenden
        // (`finding-`, `cgroup-`), und keine Ziffern (eine ID-artige
        // Zeichenkette hier wäre ein Leck).
        for denial in ALL {
            let text = denial.to_string();
            assert!(!text.chars().any(|c| c.is_ascii_digit()));
            assert!(!text.contains("finding-"));
            assert!(!text.contains("cgroup-"));
        }
    }

    #[test]
    fn test_serde_roundtrip_for_each_variant() {
        for denial in ALL {
            let json = serde_json::to_string(&denial).expect("serializes");
            let round_tripped: Denial = serde_json::from_str(&json).expect("deserializes");
            assert_eq!(round_tripped, denial);
        }
    }

    #[test]
    fn test_wire_names_are_kebab_case() {
        assert_eq!(
            serde_json::to_string(&Denial::ProofMismatch).unwrap(),
            "\"proof-mismatch\""
        );
        assert_eq!(
            serde_json::to_string(&Denial::NotAdmissibleAtStage).unwrap(),
            "\"not-admissible-at-stage\""
        );
        assert_eq!(
            serde_json::to_string(&Denial::UnsupportedVersion).unwrap(),
            "\"unsupported-version\""
        );
        assert_eq!(
            serde_json::to_string(&Denial::ExecutionFailed).unwrap(),
            "\"execution-failed\""
        );
    }
}
