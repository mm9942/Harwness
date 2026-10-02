//! `[memory]` — Projektgedächtnis: Schalter, Kontextbudget, Fakt-Obergrenzen
//! und Fristen der Wartungsjobs.
//!
//! # Verantwortungsbereich
//! Dieses Modul definiert ausschließlich die deklarative Konfiguration; die
//! Durchsetzung liegt bei den Verbrauchern:
//!
//! - `enabled` / `global_enabled` / `token_budget` — Kontext-Provider der
//!   Runtime-Montage (`harw-runtime`).
//! - `max_facts` / `max_body_bytes` — `harw_memory::FactStore::write`.
//! - `max_unused_days` — Verfall (`FactStore::decay`) in der Konsolidierung.
//! - `*_deadline_secs` — Fristen der `memory_maintenance`-Jobs (Konsolidieren,
//!   Vergessen, Promotion nach Global, Startup-Sweep).
//!
//! Alle Vorgaben entsprechen dem Verhalten vor Einführung dieses Abschnitts:
//! eine `config.toml` ohne `[memory]` ändert nichts. Obergrenzen, die es
//! bisher nicht gab (`token_budget`, `max_facts`, `max_body_bytes`), sind
//! `None` = unbegrenzt.

use crate::serde_defaults::default_true;
use serde::{Deserialize, Serialize};

/// Vorgabe von [`MemorySection::max_unused_days`] (wie
/// `harw_memory::capture::DECAY_MAX_UNUSED_DAYS`).
pub const DEFAULT_MEMORY_MAX_UNUSED_DAYS: u32 = 90;
/// Vorgabe der Fristen für Konsolidieren, Vergessen und Promotion (Sekunden).
pub const DEFAULT_MEMORY_JOB_DEADLINE_SECS: u64 = 30;
/// Vorgabe der Frist des Startup-Sweeps (Sekunden).
pub const DEFAULT_MEMORY_SWEEP_DEADLINE_SECS: u64 = 120;

/// `[memory]` — siehe Moduldoku. Jedes Feld hat eine Vorgabe, die dem
/// bisherigen Verhalten entspricht.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemorySection {
    /// Gedächtnis-Recall im Modellkontext und Startup-Sweep. `false`
    /// schaltet beides ab; gespeicherte Fakten bleiben unberührt.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Ob Fakten der globalen Wurzel in den Modellkontext gelangen. `false`
    /// liefert nur Projekt-Fakten.
    #[serde(default = "default_true")]
    pub global_enabled: bool,
    /// Obergrenze der Gedächtnis-Fragmente im Modellkontext in Tokens
    /// (geschätzt, 4 Zeichen je Token). `None` = nur die festen
    /// Zeilenobergrenzen wie bisher.
    #[serde(default)]
    pub token_budget: Option<usize>,
    /// Höchstzahl Fakten je Wurzel; ein *neuer* Fakt darüber wird von
    /// `FactStore::write` abgelehnt, Aktualisierungen bleiben möglich.
    /// `None` = unbegrenzt.
    #[serde(default)]
    pub max_facts: Option<usize>,
    /// Höchstgröße eines Fakt-Bodys in Bytes (nach Schwärzung). `None` =
    /// unbegrenzt.
    #[serde(default)]
    pub max_body_bytes: Option<usize>,
    /// Tage ohne Nutzung, nach denen die Konfidenz eines Fakts halbiert wird.
    #[serde(default = "default_max_unused_days")]
    pub max_unused_days: u32,
    /// Frist des Jobs `memory_maintenance` / Konsolidieren (Sekunden).
    #[serde(default = "default_job_deadline")]
    pub consolidate_deadline_secs: u64,
    /// Frist des Jobs `memory_maintenance` / Vergessen (Sekunden).
    #[serde(default = "default_job_deadline")]
    pub forget_deadline_secs: u64,
    /// Frist des Jobs `memory_maintenance` / Promotion nach Global
    /// (Sekunden).
    #[serde(default = "default_job_deadline")]
    pub promote_deadline_secs: u64,
    /// Frist des Jobs `memory_maintenance` / Startup-Sweep (Sekunden).
    #[serde(default = "default_sweep_deadline")]
    pub sweep_deadline_secs: u64,
    /// Schreibt den Kontext-Ledger (`harw-context-ledger`): je Turn und
    /// Fragment Label, Anbieter, Vertrauensklasse, Größe und Auslassungsgrund
    /// — nie Inhalt — nach `<home>/context-ledger`. Standard: aus.
    #[serde(default)]
    pub context_ledger: bool,
}

impl Default for MemorySection {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            global_enabled: default_true(),
            token_budget: None,
            max_facts: None,
            max_body_bytes: None,
            max_unused_days: default_max_unused_days(),
            consolidate_deadline_secs: default_job_deadline(),
            forget_deadline_secs: default_job_deadline(),
            promote_deadline_secs: default_job_deadline(),
            sweep_deadline_secs: default_sweep_deadline(),
            context_ledger: false,
        }
    }
}

impl MemorySection {
    /// Prüft Invarianten, die reine Deserialisierung nicht ausdrückt.
    ///
    /// # Errors
    /// `Err(String)` mit menschenlesbarer Begründung, wenn eine gesetzte
    /// Obergrenze (`token_budget`, `max_facts`, `max_body_bytes`) oder eine
    /// Frist `0` ist (`0` würde jeden Lauf sofort abbrechen bzw. alles
    /// ablehnen; „unbegrenzt“ heißt: Schlüssel weglassen).
    pub fn validate(&self) -> Result<(), String> {
        for (name, value) in [
            ("token_budget", self.token_budget),
            ("max_facts", self.max_facts),
            ("max_body_bytes", self.max_body_bytes),
        ] {
            if value == Some(0) {
                return Err(format!(
                    "memory.{name} muss größer als 0 sein (Schlüssel weglassen = unbegrenzt)"
                ));
            }
        }
        for (name, value) in [
            ("consolidate_deadline_secs", self.consolidate_deadline_secs),
            ("forget_deadline_secs", self.forget_deadline_secs),
            ("promote_deadline_secs", self.promote_deadline_secs),
            ("sweep_deadline_secs", self.sweep_deadline_secs),
        ] {
            if value == 0 {
                return Err(format!("memory.{name} muss größer als 0 sein"));
            }
        }
        Ok(())
    }
}

fn default_max_unused_days() -> u32 {
    DEFAULT_MEMORY_MAX_UNUSED_DAYS
}
fn default_job_deadline() -> u64 {
    DEFAULT_MEMORY_JOB_DEADLINE_SECS
}
fn default_sweep_deadline() -> u64 {
    DEFAULT_MEMORY_SWEEP_DEADLINE_SECS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn test_memory_section_defaults_from_empty_toml() -> TestResult {
        let section: MemorySection = toml::from_str("").map_err(ctx("leeres TOML parsen"))?;
        assert!(section.enabled);
        assert!(section.global_enabled);
        assert_eq!(section.token_budget, None);
        assert_eq!(section.max_facts, None);
        assert_eq!(section.max_body_bytes, None);
        assert_eq!(section.max_unused_days, 90);
        assert_eq!(section.consolidate_deadline_secs, 30);
        assert_eq!(section.forget_deadline_secs, 30);
        assert_eq!(section.promote_deadline_secs, 30);
        assert_eq!(section.sweep_deadline_secs, 120);
        assert_eq!(section, MemorySection::default());
        assert!(section.validate().is_ok());
        Ok(())
    }

    #[test]
    fn test_memory_section_full_toml_parses() -> TestResult {
        let src = r#"
            enabled = false
            global_enabled = false
            token_budget = 500
            max_facts = 200
            max_body_bytes = 4096
            max_unused_days = 30
            consolidate_deadline_secs = 10
            forget_deadline_secs = 11
            promote_deadline_secs = 12
            sweep_deadline_secs = 13
        "#;
        let section: MemorySection = toml::from_str(src).map_err(ctx("TOML parsen"))?;
        assert!(!section.enabled);
        assert!(!section.global_enabled);
        assert_eq!(section.token_budget, Some(500));
        assert_eq!(section.max_facts, Some(200));
        assert_eq!(section.max_body_bytes, Some(4096));
        assert_eq!(section.max_unused_days, 30);
        assert_eq!(section.consolidate_deadline_secs, 10);
        assert_eq!(section.forget_deadline_secs, 11);
        assert_eq!(section.promote_deadline_secs, 12);
        assert_eq!(section.sweep_deadline_secs, 13);
        assert!(section.validate().is_ok());
        Ok(())
    }

    #[test]
    fn test_memory_section_rejects_unknown_fields() {
        let parsed: Result<MemorySection, _> = toml::from_str("bogus = 1");
        assert!(parsed.is_err());
    }

    #[test]
    fn test_memory_section_validate_rejects_zero_values() -> TestResult {
        for src in [
            "token_budget = 0",
            "max_facts = 0",
            "max_body_bytes = 0",
            "consolidate_deadline_secs = 0",
            "forget_deadline_secs = 0",
            "promote_deadline_secs = 0",
            "sweep_deadline_secs = 0",
        ] {
            let section: MemorySection = toml::from_str(src).map_err(ctx("TOML parsen"))?;
            if section.validate().is_ok() {
                return Err(TestError::Unexpected(format!(
                    "{src} hätte scheitern müssen"
                )));
            }
        }
        Ok(())
    }
}
