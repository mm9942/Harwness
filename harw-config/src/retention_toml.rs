//! `[retention]` — Aufbewahrungsgrenzen für flüchtige Daten (Logs, Caches,
//! Spools).
//!
//! # Verantwortungsbereich
//! Die Struktur [`RetentionSection`] ist **nicht** von Hand geschrieben: sie
//! wird von `harw_macros::retention_classes!` aus der einen Klassendeklaration
//! in `harw-retention` (`src/classes.rs`) erzeugt — ein Unterabschnitt
//! `[retention.<klasse>]` je Klasse, `deny_unknown_fields`. Eine neue
//! Klasse erscheint hier ohne Änderung an diesem Modul; nur `scope.rs`
//! (`FIELD_TABLE`) und der Test `config_scope_exhaustive` verlangen die
//! zusätzlichen Pfade (ein Konsistenztest gegen `CLASSES` bricht sonst).
//!
//! Schlüssel je Klasse (alle optional, ungesetzt = Vorgabe der Klasse):
//! `enabled`, `max_age_secs`, `max_bytes`, `max_files`, `keep_newest`.
//!
//! # Opt-in
//! Sicherheitsrelevante Klassen (`dod_spool`, `sentinel_export`,
//! `freeze_resolved`, `session_transcripts`, `session_corrupt_backups`)
//! sind standardmäßig **aus**: nie gelöscht, im Dry-Run nur berichtet. Nur
//! ein ausdrückliches `enabled = true` aus einem vertrauten Layer schaltet
//! die Löschung ein.
//!
//! # Merge-Regel
//! Siehe `crate::merge::merge_retention`: Home und Profil ersetzen; ein
//! nicht vertrautes Projekt darf `max_age_secs`/`max_bytes`/`max_files` nur
//! **senken** (Min-Bound gegen den bisherigen Stand bzw. die Klassenvorgabe)
//! und weder `enabled` noch `keep_newest` setzen — es kann also keine
//! Grenze lockern und keine Löschung sicherheitsrelevanter Daten
//! einschalten.

pub use harw_retention::ClassConfig as RetentionClassToml;
pub use harw_retention::RetentionConfig as RetentionSection;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn test_retention_section_defaults_from_empty_toml() -> TestResult {
        let section: RetentionSection = toml::from_str("").map_err(ctx("leeres TOML parsen"))?;
        assert_eq!(section, RetentionSection::default());
        section.validate().map_err(TestError::Unexpected)?;
        Ok(())
    }

    #[test]
    fn test_retention_section_rejects_unknown_class_and_field() {
        assert!(toml::from_str::<RetentionSection>("[bogus]\nenabled = true").is_err());
        assert!(toml::from_str::<RetentionSection>("[tui_log]\nbogus = 1").is_err());
    }

    #[test]
    fn test_retention_section_parses_overrides() -> TestResult {
        let section: RetentionSection =
            toml::from_str("[dod_spool]\nenabled = true\nmax_files = 7\n")
                .map_err(ctx("TOML parsen"))?;
        assert_eq!(section.dod_spool.enabled, Some(true));
        assert_eq!(section.dod_spool.max_files, Some(7));
        assert_eq!(section.tui_log.enabled, None);
        Ok(())
    }
}
