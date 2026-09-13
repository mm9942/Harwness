//! Fehler der Runtime-Montage (`RuntimeError`).
//!
//! # Beschreibung
//! Eine Variante je Montagephase (Vertrag `docs/remediation/CONTRACTS.md`
//! §runtime-spec). Jede Variante trägt eine menschenlesbare `detail`-Angabe;
//! die Montage-Wellen (W2b/W2c) übersetzen die Fehler der jeweiligen
//! Fach-Crates an der Aufrufstelle in diese Form. `Display`,
//! `std::error::Error` und der Alias [`RuntimeResult`] entstehen über
//! `#[derive(harw_macros::HarwError)]`.

use harw_macros::HarwError;

/// Fehler beim Montieren eines Runtime-Laufs.
#[derive(Debug, HarwError)]
pub enum RuntimeError {
    /// Konfiguration konnte nicht geladen oder aufgelöst werden.
    #[msg("runtime config error: {detail}")]
    Config { detail: String },

    /// Vertrauensprüfung (z. B. nicht vertrauenswürdiges Repository) schlug fehl.
    #[msg("runtime trust error: {detail}")]
    Trust { detail: String },

    /// Projekt-/Kontext-Erkennung schlug fehl.
    #[msg("runtime discovery error: {detail}")]
    Discovery { detail: String },

    /// Werkzeug-/Operations-Registry konnte nicht montiert werden.
    #[msg("runtime registry error: {detail}")]
    Registry { detail: String },

    /// Sandbox konnte nicht aufgelöst oder verengt werden.
    #[msg("runtime sandbox error: {detail}")]
    Sandbox { detail: String },

    /// Provider konnte nicht aufgelöst oder gebaut werden.
    #[msg("runtime provider error: {detail}")]
    Provider { detail: String },

    /// Session-/Job-Store konnte nicht geöffnet werden.
    #[msg("runtime store error: {detail}")]
    Store { detail: String },

    /// Spawner konnte nicht montiert werden.
    #[msg("runtime spawner error: {detail}")]
    Spawner { detail: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Konstruktor je `RuntimeError`-Variante für
    /// `display_names_phase_and_detail` (vermeidet `clippy::type_complexity`
    /// auf dem rohen Funktionszeiger-Tupel-Array-Typ).
    type Make = fn(String) -> RuntimeError;

    #[test]
    fn display_names_phase_and_detail() {
        let cases: [(Make, &str); 8] = [
            (|detail| RuntimeError::Config { detail }, "config"),
            (|detail| RuntimeError::Trust { detail }, "trust"),
            (|detail| RuntimeError::Discovery { detail }, "discovery"),
            (|detail| RuntimeError::Registry { detail }, "registry"),
            (|detail| RuntimeError::Sandbox { detail }, "sandbox"),
            (|detail| RuntimeError::Provider { detail }, "provider"),
            (|detail| RuntimeError::Store { detail }, "store"),
            (|detail| RuntimeError::Spawner { detail }, "spawner"),
        ];
        for (make, phase) in cases {
            let err = make("x".to_owned());
            assert_eq!(err.to_string(), format!("runtime {phase} error: x"));
            assert!(std::error::Error::source(&err).is_none());
        }
    }

    #[test]
    fn result_alias_is_emitted() {
        fn fails() -> RuntimeResult<()> {
            Err(RuntimeError::Trust {
                detail: "untrusted".into(),
            })
        }
        assert!(matches!(fails(), Err(RuntimeError::Trust { .. })));
    }
}
