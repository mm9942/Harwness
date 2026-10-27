//! Provider-Offering-Lebenszyklus: Overlay für Deprecation und Retirement.
//!
//! Spezifikation: `docs/planning/86-provider-model-lifecycle/README.md`
//! (D1/D3/D5) sowie `2026-10-14-audit.md` und `2026-10-14-migration-gates.md`.
//!
//! # Verantwortung
//! Das Modul beschreibt, welchen Lebenszyklus ein *Provider-Angebot*
//! (Provider plus Modell-ID) hat. Derselbe Modellname kann bei verschiedenen
//! Providern unterschiedliche Daten haben (z. B. `zai-glm-5-2` bei Mistral
//! gegenüber direktem Z.AI); der Schlüssel ist deshalb immer
//! `(provider, model)`.
//!
//! # Exportierte Typen
//! - [`RetirementDate`]: Zeitpunkt oder reines Kalenderdatum.
//! - [`ProviderLifecycle`]: Deprecation-/Retirement-Angaben samt Ersatzmodellen.
//! - [`LifecycleStatus`]: abgeleiteter Status zu einem Zeitpunkt.
//! - [`ExplicitSelection`] und [`RetiredModelError`]: Ergebnis einer expliziten Auswahl.
//! - [`LifecycleEntry`], [`bundled_lifecycles`], [`lookup`]: eingebettete Tabelle.
//!
//! # Fehlerquellen
//! Ausschließlich [`RetiredModelError`] bei expliziter Auswahl eines
//! abgekündigten (retired) Modells. Das Modul ist rein und offline; es gibt
//! keine I/O und keine Uhr — der Zeitpunkt wird stets übergeben.
//!
//! # Concurrency
//! Reine Werttypen ohne innere Veränderlichkeit; `Send + Sync`.
//!
//! # Hinweise zur Semantik
//! - Ein Modell wird nie stillschweigend ersetzt: [`ProviderLifecycle::check_explicit`]
//!   liefert bei Retirement einen Fehler und tauscht die Modell-ID nicht aus.
//! - `earliest_retirement_at` ist eine Support-Untergrenze („frühestens“), kein
//!   angekündigtes Retirement, und wird von [`ProviderLifecycle::status`] nie gelesen.

use std::error::Error as StdError;
use std::fmt;

use serde::{Deserialize, Serialize};
use time::{Date, Duration, Month, OffsetDateTime};

use crate::descriptor::ModelId;

/// Vorlauf des Retirement-Fensters für reine Datumsangaben.
///
/// Ein Retirement zum Kalenderdatum `d` beginnt frühestens am Vortag
/// `d - 1` um 10:00 UTC, also 14 Stunden vor `d` 00:00 UTC. Provider nennen
/// nur Kalendertage ohne Zeitzone; vorher gilt das Modell nicht als im Fenster.
pub const DATE_ONLY_WINDOW_LEAD: Duration = Duration::hours(14);

/// Versatz nach `d` 00:00 UTC, ab dem ein reines Datums-Retirement als vollzogen gilt.
///
/// Ein Retirement zum Kalenderdatum `d` ist ab `d` 12:00 UTC endgültig
/// [`LifecycleStatus::Retired`]; bis dahin gilt [`LifecycleStatus::RetirementWindow`].
pub const DATE_ONLY_RETIRED_AFTER: Duration = Duration::hours(12);

/// Zeitangabe eines Retirements oder einer Deprecation.
///
/// # Beschreibung
/// Provider nennen entweder einen exakten Zeitpunkt oder nur ein Kalenderdatum
/// ohne Zeitzone. Reine Daten werden nicht als UTC-Mitternacht interpretiert,
/// sondern über [`DATE_ONLY_WINDOW_LEAD`] und [`DATE_ONLY_RETIRED_AFTER`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RetirementDate {
    /// Exakter Zeitpunkt.
    Instant(OffsetDateTime),
    /// Nur ein Kalenderdatum ohne Uhrzeit und Zeitzone.
    DateOnly(Date),
}

impl RetirementDate {
    /// Beginn des Kalendertags in UTC (nur für `DateOnly` sinnvoll).
    fn midnight_utc(date: Date) -> OffsetDateTime {
        date.midnight().assume_utc()
    }

    /// Ab diesem Zeitpunkt beginnt das Retirement-Fenster (`None`: kein Fenster).
    fn window_start(&self) -> Option<OffsetDateTime> {
        match self {
            Self::Instant(_) => None,
            Self::DateOnly(date) => Self::midnight_utc(*date).checked_sub(DATE_ONLY_WINDOW_LEAD),
        }
    }

    /// Ab diesem Zeitpunkt gilt das Retirement als vollzogen (`None`: Überlauf).
    fn retired_from(&self) -> Option<OffsetDateTime> {
        match self {
            Self::Instant(instant) => Some(*instant),
            Self::DateOnly(date) => Self::midnight_utc(*date).checked_add(DATE_ONLY_RETIRED_AFTER),
        }
    }

    /// Ab diesem Zeitpunkt gilt eine Deprecation als wirksam.
    ///
    /// Reine Daten beginnen am Kalendertag um 00:00 UTC.
    fn started_at(&self) -> OffsetDateTime {
        match self {
            Self::Instant(instant) => *instant,
            Self::DateOnly(date) => Self::midnight_utc(*date),
        }
    }
}

/// Abgeleiteter Lebenszyklus-Status eines Provider-Angebots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LifecycleStatus {
    /// Uneingeschränkt nutzbar.
    Active,
    /// Abgekündigt, funktioniert aber weiter.
    Deprecated,
    /// Das Retirement steht unmittelbar bevor (nur bei reinen Datumsangaben).
    RetirementWindow,
    /// Abgeschaltet.
    Retired,
}

/// Lebenszyklus-Angaben eines Provider-Angebots.
///
/// # Beschreibung
/// Alle Felder sind optional. `earliest_retirement_at` dokumentiert nur eine
/// Support-Untergrenze und beeinflusst [`ProviderLifecycle::status`] nicht.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ProviderLifecycle {
    /// Zeitpunkt der Deprecation.
    pub deprecated_at: Option<RetirementDate>,
    /// Angekündigtes Retirement.
    pub retired_at: Option<RetirementDate>,
    /// Frühestmögliches Retirement (Support-Untergrenze, kein Retirement-Datum).
    pub earliest_retirement_at: Option<RetirementDate>,
    /// Vom Provider empfohlene Ersatzmodelle.
    pub replacements: Vec<ModelId>,
}

/// Ergebnis einer expliziten Modellauswahl, die nicht abgelehnt wurde.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExplicitSelection {
    /// Das Modell ist ohne Einschränkung nutzbar.
    Ok,
    /// Das Modell ist nutzbar, aber abgekündigt; der Aufrufer sollte warnen.
    Deprecated {
        /// Empfohlene Ersatzmodelle.
        replacements: Vec<ModelId>,
    },
}

/// Fehler: ein explizit gewähltes Modell ist abgeschaltet.
///
/// Die Modell-ID wird nie automatisch ersetzt; `replacements` sind nur Hinweise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetiredModelError {
    /// Provider des abgeschalteten Angebots.
    pub provider: String,
    /// Abgeschaltete Modell-ID.
    pub model: String,
    /// Empfohlene Ersatzmodelle (ggf. leer).
    pub replacements: Vec<ModelId>,
}

impl fmt::Display for RetiredModelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "model {} of provider {} is retired",
            self.model, self.provider
        )?;
        if self.replacements.is_empty() {
            formatter.write_str("; no replacement announced")
        } else {
            let names: Vec<&str> = self.replacements.iter().map(ModelId::as_str).collect();
            write!(formatter, "; suggested replacement: {}", names.join(", "))
        }
    }
}

impl StdError for RetiredModelError {}

impl ProviderLifecycle {
    /// Bestimmt den Status zum Zeitpunkt `now`.
    ///
    /// Retired hat Vorrang vor RetirementWindow, dieses vor Deprecated.
    /// `earliest_retirement_at` wird nie gelesen.
    #[must_use]
    pub fn status(&self, now: OffsetDateTime) -> LifecycleStatus {
        if let Some(retired) = &self.retired_at {
            if retired.retired_from().is_some_and(|from| now >= from) {
                return LifecycleStatus::Retired;
            }
            if retired.window_start().is_some_and(|start| now >= start) {
                return LifecycleStatus::RetirementWindow;
            }
        }
        let deprecated_now = self
            .deprecated_at
            .is_some_and(|deprecated| now >= deprecated.started_at());
        if deprecated_now {
            return LifecycleStatus::Deprecated;
        }
        LifecycleStatus::Active
    }

    /// Ob das Angebot ohne explizite Nutzerwahl automatisch gewählt werden darf.
    ///
    /// `false` bei `RetirementWindow` und `Retired`.
    #[must_use]
    pub fn implicitly_selectable(&self, now: OffsetDateTime) -> bool {
        !matches!(
            self.status(now),
            LifecycleStatus::RetirementWindow | LifecycleStatus::Retired
        )
    }

    /// Prüft eine explizite Auswahl von `provider`/`model` zum Zeitpunkt `now`.
    ///
    /// `RetirementWindow` wird wie `Deprecated` behandelt.
    ///
    /// # Errors
    /// [`RetiredModelError`], wenn das Angebot retired ist. Die Modell-ID wird
    /// dabei nicht ersetzt.
    pub fn check_explicit(
        &self,
        provider: &str,
        model: &str,
        now: OffsetDateTime,
    ) -> Result<ExplicitSelection, RetiredModelError> {
        match self.status(now) {
            LifecycleStatus::Active => Ok(ExplicitSelection::Ok),
            LifecycleStatus::Deprecated | LifecycleStatus::RetirementWindow => {
                Ok(ExplicitSelection::Deprecated {
                    replacements: self.replacements.clone(),
                })
            }
            LifecycleStatus::Retired => Err(RetiredModelError {
                provider: provider.to_owned(),
                model: model.to_owned(),
                replacements: self.replacements.clone(),
            }),
        }
    }
}

/// Eintrag der eingebetteten Lebenszyklus-Tabelle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleEntry {
    /// Provider-Kennung des Angebots, z. B. `"mistral"`.
    pub provider: &'static str,
    /// Modell-ID beim Provider.
    pub model: &'static str,
    /// Lebenszyklus-Angaben.
    pub lifecycle: ProviderLifecycle,
}

/// Baut ein Kalenderdatum; `None` bei ungültigen Werten.
fn date(year: i32, month: u8, day: u8) -> Option<Date> {
    let month = Month::try_from(month).ok()?;
    Date::from_calendar_date(year, month, day).ok()
}

/// Baut ein reines Datum als [`RetirementDate`].
fn date_only(year: i32, month: u8, day: u8) -> Option<RetirementDate> {
    date(year, month, day).map(RetirementDate::DateOnly)
}

/// Wandelt eine optionale Ersatz-ID in die Ersatzliste um.
fn replacement_list(replacement: Option<&str>) -> Vec<ModelId> {
    replacement.map(ModelId::from).into_iter().collect()
}

/// Eintrag mit reinem Datums-Retirement.
fn retired_entry(
    provider: &'static str,
    model: &'static str,
    retired: (i32, u8, u8),
    replacement: Option<&str>,
) -> Option<LifecycleEntry> {
    Some(LifecycleEntry {
        provider,
        model,
        lifecycle: ProviderLifecycle {
            retired_at: Some(date_only(retired.0, retired.1, retired.2)?),
            replacements: replacement_list(replacement),
            ..ProviderLifecycle::default()
        },
    })
}

/// Eingebettete Lebenszyklus-Tabelle (Stand: Audit 2026-10-14).
///
/// Quellen: `docs/research/models/*-2026-10-14.md` und
/// `anthropic-lifecycle-2026-09-30.md`. Es gibt bewusst keine Z.AI-Einträge:
/// die Mistral-Daten gelten nur für das Mistral-Angebot.
#[must_use]
pub fn bundled_lifecycles() -> Vec<LifecycleEntry> {
    let mut entries: Vec<Option<LifecycleEntry>> = vec![
        retired_entry(
            "mistral",
            "mistral-ocr-4-0",
            (2026, 9, 30),
            Some("mistral-ocr-4-1"),
        ),
        retired_entry("mistral", "labs-leanstral-1-5", (2026, 9, 30), None),
        retired_entry(
            "mistral",
            "zai-glm-5-2",
            (2026, 10, 31),
            Some("zai-glm-5-3"),
        ),
        anthropic_sonnet_4_5(),
        anthropic_mythos_preview(),
        anthropic_haiku_5_5(),
    ];
    for model in [
        "gpt-4o-2024-08-06",
        "gpt-4o-2024-11-20",
        "gpt-4o-mini-2024-07-18",
        "gpt-4.1-2025-04-14",
        "gpt-4.1-mini-2025-04-14",
    ] {
        entries.push(retired_entry(
            "openai",
            model,
            (2027, 1, 14),
            Some("gpt-6-luna"),
        ));
    }
    entries.into_iter().flatten().collect()
}

/// Claude Sonnet 4.5: deprecated 2026-09-30, retired 2026-11-30.
fn anthropic_sonnet_4_5() -> Option<LifecycleEntry> {
    Some(LifecycleEntry {
        provider: "anthropic",
        model: "claude-sonnet-4-5-20250929",
        lifecycle: ProviderLifecycle {
            deprecated_at: Some(date_only(2026, 9, 30)?),
            retired_at: Some(date_only(2026, 11, 30)?),
            earliest_retirement_at: None,
            replacements: replacement_list(Some("claude-sonnet-5-5")),
        },
    })
}

/// Claude Mythos Preview: deprecated seit 2026-06-09, Retirement nicht angekündigt.
fn anthropic_mythos_preview() -> Option<LifecycleEntry> {
    Some(LifecycleEntry {
        provider: "anthropic",
        model: "claude-mythos-preview",
        lifecycle: ProviderLifecycle {
            deprecated_at: Some(date_only(2026, 6, 9)?),
            retired_at: None,
            earliest_retirement_at: None,
            replacements: replacement_list(Some("claude-mythos-5")),
        },
    })
}

/// Claude Haiku 5.5: aktiv, Retirement frühestens 2027-10-07 (nur Untergrenze).
fn anthropic_haiku_5_5() -> Option<LifecycleEntry> {
    Some(LifecycleEntry {
        provider: "anthropic",
        model: "claude-haiku-5-5",
        lifecycle: ProviderLifecycle {
            deprecated_at: None,
            retired_at: None,
            earliest_retirement_at: Some(date_only(2027, 10, 7)?),
            replacements: Vec::new(),
        },
    })
}

/// Sucht den Eintrag für `provider`/`model` (exakter, providergebundener Treffer).
#[must_use]
pub fn lookup<'a>(
    entries: &'a [LifecycleEntry],
    provider: &str,
    model: &str,
) -> Option<&'a LifecycleEntry> {
    entries
        .iter()
        .find(|entry| entry.provider == provider && entry.model == model)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    fn utc(year: i32, month: u8, day: u8, hour: u8) -> TestResult<OffsetDateTime> {
        let day = date(year, month, day).ok_or(TestError::Missing("valid test date"))?;
        let moment = day.with_hms(hour, 0, 0).map_err(ctx("valid test time"))?;
        Ok(moment.assume_utc())
    }

    fn retired_on(year: i32, month: u8, day: u8) -> TestResult<ProviderLifecycle> {
        Ok(ProviderLifecycle {
            retired_at: Some(date_only(year, month, day).ok_or(TestError::Missing("date"))?),
            replacements: vec![ModelId::from("next-model")],
            ..ProviderLifecycle::default()
        })
    }

    fn entry<'a>(
        entries: &'a [LifecycleEntry],
        provider: &str,
        model: &str,
    ) -> TestResult<&'a LifecycleEntry> {
        lookup(entries, provider, model).ok_or(TestError::Missing("bundled lifecycle entry"))
    }

    #[test]
    fn deprecated_warns_on_explicit_and_stays_implicitly_selectable() -> TestResult {
        let lifecycle = ProviderLifecycle {
            deprecated_at: Some(RetirementDate::Instant(utc(2026, 9, 30, 0)?)),
            replacements: vec![ModelId::from("next-model")],
            ..ProviderLifecycle::default()
        };
        let now = utc(2026, 10, 14, 0)?;

        assert_eq!(lifecycle.status(now), LifecycleStatus::Deprecated);
        assert!(lifecycle.implicitly_selectable(now));
        let selection = lifecycle
            .check_explicit("p", "m", now)
            .map_err(ctx("deprecated model stays selectable"))?;
        assert_eq!(
            selection,
            ExplicitSelection::Deprecated {
                replacements: vec![ModelId::from("next-model")]
            }
        );
        assert_eq!(
            lifecycle.status(utc(2026, 9, 29, 23)?),
            LifecycleStatus::Active
        );
        Ok(())
    }

    #[test]
    fn retired_instant_is_an_error_with_replacements_and_no_substitution() -> TestResult {
        let at = utc(2026, 11, 30, 8)?;
        let lifecycle = ProviderLifecycle {
            deprecated_at: Some(RetirementDate::Instant(utc(2026, 9, 30, 0)?)),
            retired_at: Some(RetirementDate::Instant(at)),
            replacements: vec![ModelId::from("next-model")],
            ..ProviderLifecycle::default()
        };

        assert_eq!(
            lifecycle.status(at - Duration::seconds(1)),
            LifecycleStatus::Deprecated
        );
        assert_eq!(lifecycle.status(at), LifecycleStatus::Retired);
        assert!(!lifecycle.implicitly_selectable(at));
        let Err(error) = lifecycle.check_explicit("prov", "old-model", at) else {
            return Err(TestError::Unexpected(
                "retired model must be rejected".to_owned(),
            ));
        };
        assert_eq!(error.provider, "prov");
        assert_eq!(error.model, "old-model");
        assert_eq!(error.replacements, vec![ModelId::from("next-model")]);
        assert!(error.to_string().contains("old-model"));
        assert!(error.to_string().contains("next-model"));
        Ok(())
    }

    #[test]
    fn date_only_window_boundaries() -> TestResult {
        let lifecycle = retired_on(2026, 9, 30)?;
        let window_start = utc(2026, 9, 29, 10)?;
        let retired_from = utc(2026, 9, 30, 12)?;
        let second = Duration::seconds(1);

        assert_eq!(
            lifecycle.status(window_start - second),
            LifecycleStatus::Active
        );
        assert_eq!(
            lifecycle.status(window_start),
            LifecycleStatus::RetirementWindow
        );
        assert_eq!(
            lifecycle.status(retired_from - second),
            LifecycleStatus::RetirementWindow
        );
        assert_eq!(lifecycle.status(retired_from), LifecycleStatus::Retired);
        assert!(!lifecycle.implicitly_selectable(window_start));

        let selection = lifecycle
            .check_explicit("p", "m", window_start)
            .map_err(ctx("window is not yet retired"))?;
        assert!(matches!(selection, ExplicitSelection::Deprecated { .. }));
        assert!(lifecycle.check_explicit("p", "m", retired_from).is_err());
        Ok(())
    }

    #[test]
    fn support_floor_does_not_change_status() -> TestResult {
        let entries = bundled_lifecycles();
        let haiku = entry(&entries, "anthropic", "claude-haiku-5-5")?;
        assert!(haiku.lifecycle.earliest_retirement_at.is_some());

        for now in [
            utc(2026, 10, 16, 0)?,
            utc(2027, 10, 8, 0)?,
            utc(2030, 1, 1, 0)?,
        ] {
            assert_eq!(haiku.lifecycle.status(now), LifecycleStatus::Active);
            assert!(haiku.lifecycle.implicitly_selectable(now));
        }
        Ok(())
    }

    #[test]
    fn bundled_statuses_follow_the_audit() -> TestResult {
        let entries = bundled_lifecycles();
        let now = utc(2026, 10, 16, 0)?;

        let ocr = entry(&entries, "mistral", "mistral-ocr-4-0")?;
        assert_eq!(ocr.lifecycle.status(now), LifecycleStatus::Retired);
        let sonnet = entry(&entries, "anthropic", "claude-sonnet-4-5-20250929")?;
        assert_eq!(sonnet.lifecycle.status(now), LifecycleStatus::Deprecated);
        let mythos = entry(&entries, "anthropic", "claude-mythos-preview")?;
        assert_eq!(mythos.lifecycle.status(now), LifecycleStatus::Deprecated);
        let gpt = entry(&entries, "openai", "gpt-4.1-2025-04-14")?;
        assert_eq!(gpt.lifecycle.status(now), LifecycleStatus::Active);
        Ok(())
    }

    #[test]
    fn mistral_and_zai_keys_do_not_collide() -> TestResult {
        let entries = bundled_lifecycles();

        assert!(lookup(&entries, "mistral", "zai-glm-5-2").is_some());
        assert!(lookup(&entries, "zai", "zai-glm-5-2").is_none());
        assert!(entries.iter().all(|entry| entry.provider != "zai"));

        let mut keys = BTreeSet::new();
        for entry in &entries {
            assert!(
                keys.insert((entry.provider, entry.model)),
                "duplicate key {}/{}",
                entry.provider,
                entry.model
            );
        }
        Ok(())
    }

    #[test]
    fn every_expected_key_is_present() {
        let entries = bundled_lifecycles();
        let expected = [
            ("mistral", "mistral-ocr-4-0"),
            ("mistral", "labs-leanstral-1-5"),
            ("mistral", "zai-glm-5-2"),
            ("anthropic", "claude-sonnet-4-5-20250929"),
            ("anthropic", "claude-mythos-preview"),
            ("anthropic", "claude-haiku-5-5"),
            ("openai", "gpt-4o-2024-08-06"),
            ("openai", "gpt-4o-2024-11-20"),
            ("openai", "gpt-4o-mini-2024-07-18"),
            ("openai", "gpt-4.1-2025-04-14"),
            ("openai", "gpt-4.1-mini-2025-04-14"),
        ];
        for (provider, model) in expected {
            assert!(
                lookup(&entries, provider, model).is_some(),
                "missing {provider}/{model}"
            );
        }
        assert_eq!(entries.len(), expected.len());
    }

    #[test]
    fn retired_entries_except_leanstral_have_replacements() {
        let entries = bundled_lifecycles();
        for entry in entries
            .iter()
            .filter(|entry| entry.lifecycle.retired_at.is_some())
        {
            if entry.model == "labs-leanstral-1-5" {
                assert!(entry.lifecycle.replacements.is_empty());
            } else {
                assert!(
                    !entry.lifecycle.replacements.is_empty(),
                    "no replacement for {}/{}",
                    entry.provider,
                    entry.model
                );
            }
        }
    }

    #[test]
    fn bundled_replacements_exist_in_the_anthropic_catalog() -> TestResult {
        let catalog = crate::vendor_anthropic::anthropic_descriptors();
        let known: BTreeSet<&str> = catalog.iter().map(|d| d.model.as_str()).collect();
        // Ersatz-IDs anderer Provider werden hier nicht geprüft.
        let other_providers = crate::descriptor::bootstrap_descriptors();
        let foreign: BTreeSet<&str> = other_providers
            .iter()
            .filter(|d| d.provider != "anthropic")
            .map(|d| d.model.as_str())
            .collect();

        let entries = bundled_lifecycles();
        let mut checked = 0_usize;
        for entry in entries.iter().filter(|e| e.provider == "anthropic") {
            for replacement in &entry.lifecycle.replacements {
                let id = replacement.as_str();
                if !known.contains(id) && foreign.contains(id) {
                    continue;
                }
                assert!(
                    known.contains(id),
                    "replacement {id} of anthropic/{} fehlt in anthropic_descriptors()",
                    entry.model
                );
                checked += 1;
            }
        }
        assert!(checked > 0, "kein Anthropic-Ersatzmodell geprüft");
        Ok(())
    }

    #[test]
    fn lifecycle_serde_round_trip() -> TestResult {
        let lifecycle = ProviderLifecycle {
            deprecated_at: Some(RetirementDate::Instant(utc(2026, 9, 30, 6)?)),
            retired_at: date_only(2026, 11, 30),
            earliest_retirement_at: date_only(2027, 10, 7),
            replacements: vec![ModelId::from("claude-sonnet-5-5")],
        };

        let json = serde_json::to_string(&lifecycle)?;
        let parsed: ProviderLifecycle = serde_json::from_str(&json)?;
        assert_eq!(parsed, lifecycle);
        Ok(())
    }
}
