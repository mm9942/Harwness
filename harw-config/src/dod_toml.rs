//! `[dod]` — Grundsatz-Policy für die Harw-DoD-Kette (Detect · Orient ·
//! Defend: Sensorik, Eskalation, Warden) (Remediationsplan Teil B, Welle W3, AP `C-CFG`;
//! Konsumenten: `harw-dod-escalate`/`harw-escalator` `D-ESC`,
//! `harw-dod-warden`/`harw-warden` `D-WARDEN`, `harw-dod-sentinel` `D-SENTLIB`).
//!
//! Dieses Modul definiert ausschließlich die deklarative Policy, unter der
//! die DoD-Kette eskalieren darf: ob ein Kill-Kommando ausschließlich mit
//! menschlicher Freigabe ausgeführt werden darf
//! ([`DodSection::kill_requires_human`] — laut Plan-Entscheidung "Warden-
//! Automatik: Freeze/Release automatisch nach signiertem Proof; Kill nur mit
//! menschlicher Freigabe (Default aus)" **nicht verhandelbar**, siehe
//! [`DodSection::validate`]), ob Freeze/Release-Übergänge automatisch nach
//! einem signierten Proof erfolgen ([`DodSection::auto_freeze`]), wo die
//! Schlüssel für `SignedAuthorization`-Prüfung liegen
//! ([`DodSection::proof_key_dir`]), und welche cgroup-Pfadpräfixe die
//! Eskalationskette überhaupt adressieren darf
//! ([`DodSection::allowed_cgroup_prefixes`] — Design "Ziel-cgroup aus Record
//! + Präfix"/"cgroup-Präfix-Allowlist"). Durchsetzung liegt vollständig beim
//!   Consumer (Escalator/Warden-Binaries); dieses Modul beschreibt nur die
//!   Konfiguration und deren Invarianten.
//!
//! Two read-side keys (PL-97 D1) tell harw where to read the sentinel's
//! exports and how far back to summarize findings:
//! [`DodSection::export_dir`] (explicit export directory; when unset the
//! reader falls back to `/var/lib/harw-dod/export`, then to
//! `<HARW home>/dod/export`) and [`DodSection::findings_window_hours`]
//! (default [`DEFAULT_FINDINGS_WINDOW_HOURS`]). Both are profile-scoped:
//! only trusted layers (home, active profile) set them. An untrusted
//! repository layer can set neither — `export_dir` for the same reason as
//! `proof_key_dir` (redirecting harw to an attacker-chosen directory would
//! let the repository fabricate posture), `findings_window_hours` because
//! the restricted merge only copies keys it explicitly narrows.
//!
//! Defaults sind bewusst restriktiv (`Default = sicher/aus`):
//! `kill_requires_human = true`, `auto_freeze = true`, leere
//! cgroup-Präfixliste. Ein nicht vertrauter Repo-Layer darf diese Sektion
//! nur verengen
//! (`allowed_cgroup_prefixes` nur Schnittmenge, `auto_freeze` nur Richtung
//! `true`); `kill_requires_human` ist ohnehin in jeder Layer fest auf `true`
//! erzwungen (siehe unten) und `proof_key_dir` wird aus einem Repo-Layer
//! **nie** übernommen (Umlenkung auf einen fremden Schlüsselordner wäre eine
//! Rechteausweitung); ebenso `export_dir` und `findings_window_hours`. Die
//! Merge-Logik lebt in `harw_config::discovery`, nicht hier.
//!
//! Keine Nebenläufigkeit: reiner Datentyp, `Send + Sync` über `derive`.
//! Fehler: [`DodSection::validate`] liefert `Result<(), String>` (gleiches
//! Muster wie `research_toml::ResearchSection::validate`).
//!
//! Deserializing a `[dod]` table runs [`DodSection::validate`] (serde
//! `try_from` over a private raw shape), so the invariants hold for every
//! value that config discovery produces: a `[dod]` table in any layer —
//! trusted or not — with `kill_requires_human = false`, a blank cgroup
//! prefix, a relative `export_dir` or `findings_window_hours = 0` makes
//! `discover_config` fail instead of loading. A section built in code
//! (struct literal) bypasses that check; call [`DodSection::validate`]
//! yourself there.
//!
//! # Examples
//! ```rust
//! use harw_config::DodSection;
//!
//! let section: DodSection = toml::from_str("").unwrap();
//! assert!(section.kill_requires_human);
//! assert_eq!(section.export_dir, None);
//! assert_eq!(section.findings_window_hours, 24);
//! assert!(section.validate().is_ok());
//! ```

use crate::serde_defaults::default_true;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Default of [`DodSection::findings_window_hours`]: one day.
pub const DEFAULT_FINDINGS_WINDOW_HOURS: u32 = 24;

/// Serde default for [`DodSection::findings_window_hours`].
const fn default_findings_window_hours() -> u32 {
    DEFAULT_FINDINGS_WINDOW_HOURS
}

/// `[dod]` — Eskalations-Policy für die DoD-Kette: Kill-Freigabe,
/// Auto-Freeze, Schlüsselverzeichnis für Warden-Proofs und erlaubte
/// cgroup-Präfixe; dazu die beiden Leseschlüssel `export_dir` und
/// `findings_window_hours` (PL-97 D1).
///
/// Deserialization goes through a private raw shape and then
/// [`Self::validate`]; an invalid `[dod]` table never deserializes (see the
/// module docs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "DodSectionToml")]
pub struct DodSection {
    /// Muss laut Plan-Entscheidung ("Kill nur mit menschlicher Freigabe")
    /// immer `true` sein. Nicht verhandelbar: [`Self::validate`] lehnt
    /// `false` in jeder Layer ab, unabhängig davon, ob sie vertraut ist.
    pub kill_requires_human: bool,
    /// Erlaubt automatisches Freeze/Release, sobald ein gültiger,
    /// signierter `WardenRequest`-Proof vorliegt. `true` (Default) ist die
    /// sichere Voreinstellung (siehe Modul-Doku); Abschalten reduziert den
    /// Automatisierungsgrad der DoD-Kette, nicht ihre Sicherheit — daher darf
    /// ein Repo-Layer diesen Wert nur *anheben*, nie absenken.
    pub auto_freeze: bool,
    /// Verzeichnis, aus dem der Warden die `ProofKey`-Materialien liest.
    /// `None` (Default) heißt: Consumer nutzt seinen eigenen fest
    /// verdrahteten Standardpfad.
    pub proof_key_dir: Option<PathBuf>,
    /// cgroup-Pfadpräfixe, auf die Eskalationsaktionen (Freeze/Release/Kill)
    /// beschränkt sind. Leer (Default) heißt: keine cgroup adressierbar, bis
    /// der Consumer explizit Präfixe erhält.
    pub allowed_cgroup_prefixes: Vec<String>,
    /// Explicit directory holding the sentinel exports (`findings.jsonl`,
    /// `health.json`). `None` (default): the reader resolves
    /// `/var/lib/harw-dod/export` if readable, else `<HARW home>/dod/export`.
    /// Must be absolute ([`Self::validate`]). Profile scope; never taken from
    /// an untrusted repository layer (same rule as `proof_key_dir`).
    pub export_dir: Option<PathBuf>,
    /// Window in hours over which harw summarizes exported findings
    /// (status line, `harw dod status`). Default
    /// [`DEFAULT_FINDINGS_WINDOW_HOURS`]; `0` is rejected by
    /// [`Self::validate`]. Profile scope; not taken from an untrusted
    /// repository layer.
    pub findings_window_hours: u32,
}

/// Raw TOML shape of `[dod]`: the same keys, defaults and
/// `deny_unknown_fields` as [`DodSection`], but without its invariants.
/// Only used as the serde `try_from` source of [`DodSection`], so that
/// deserialization cannot skip [`DodSection::validate`].
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DodSectionToml {
    #[serde(default = "default_true")]
    kill_requires_human: bool,
    #[serde(default = "default_true")]
    auto_freeze: bool,
    #[serde(default)]
    proof_key_dir: Option<PathBuf>,
    #[serde(default)]
    allowed_cgroup_prefixes: Vec<String>,
    #[serde(default)]
    export_dir: Option<PathBuf>,
    #[serde(default = "default_findings_window_hours")]
    findings_window_hours: u32,
}

impl TryFrom<DodSectionToml> for DodSection {
    type Error = String;

    /// Builds the section from its raw TOML shape and checks
    /// [`DodSection::validate`].
    ///
    /// # Errors
    /// The reason from [`DodSection::validate`] when an invariant is
    /// violated.
    fn try_from(raw: DodSectionToml) -> Result<Self, Self::Error> {
        let DodSectionToml {
            kill_requires_human,
            auto_freeze,
            proof_key_dir,
            allowed_cgroup_prefixes,
            export_dir,
            findings_window_hours,
        } = raw;
        let section = Self {
            kill_requires_human,
            auto_freeze,
            proof_key_dir,
            allowed_cgroup_prefixes,
            export_dir,
            findings_window_hours,
        };
        section.validate()?;
        Ok(section)
    }
}

impl Default for DodSection {
    fn default() -> Self {
        Self {
            kill_requires_human: true,
            auto_freeze: true,
            proof_key_dir: None,
            allowed_cgroup_prefixes: Vec::new(),
            export_dir: None,
            findings_window_hours: DEFAULT_FINDINGS_WINDOW_HOURS,
        }
    }
}

impl DodSection {
    /// Prüft Invarianten, die reine Deserialisierung nicht ausdrücken kann.
    ///
    /// # Errors
    /// Liefert `Err(String)` mit einer menschenlesbaren Begründung, wenn
    /// `kill_requires_human == false` (in jeder Layer, unabhängig von
    /// Vertrauensstatus — Kill ohne menschliche Freigabe ist per
    /// Plan-Entscheidung nie zulässig), oder wenn `allowed_cgroup_prefixes`
    /// einen leeren Eintrag enthält; also when `export_dir` is not an
    /// absolute path or `findings_window_hours` is `0`.
    pub fn validate(&self) -> Result<(), String> {
        if !self.kill_requires_human {
            return Err(
                "dod.kill_requires_human muss immer true sein (Warden-Kill nur mit \
                 menschlicher Freigabe)"
                    .to_owned(),
            );
        }
        for prefix in &self.allowed_cgroup_prefixes {
            if prefix.trim().is_empty() {
                return Err("dod.allowed_cgroup_prefixes enthält einen leeren Eintrag".to_owned());
            }
        }
        if let Some(dir) = &self.export_dir
            && !dir.is_absolute()
        {
            return Err(format!(
                "dod.export_dir must be an absolute path, got {}",
                dir.display()
            ));
        }
        if self.findings_window_hours == 0 {
            return Err("dod.findings_window_hours must be at least 1".to_owned());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn test_dod_section_defaults_from_empty_toml() -> TestResult {
        let section: DodSection = toml::from_str("").map_err(ctx("parse toml"))?;
        assert!(section.kill_requires_human);
        assert!(section.auto_freeze);
        assert_eq!(section.proof_key_dir, None);
        assert!(section.allowed_cgroup_prefixes.is_empty());
        assert_eq!(section.export_dir, None);
        assert_eq!(section.findings_window_hours, DEFAULT_FINDINGS_WINDOW_HOURS);
        assert_eq!(section.findings_window_hours, 24);
        assert_eq!(section, DodSection::default());
        Ok(())
    }

    #[test]
    fn test_dod_section_parses_export_dir_and_window() -> TestResult {
        let src = r#"
            export_dir = "/var/lib/harw-dod/export"
            findings_window_hours = 72
        "#;
        let section: DodSection = toml::from_str(src).map_err(ctx("parse toml"))?;
        assert_eq!(
            section.export_dir,
            Some(PathBuf::from("/var/lib/harw-dod/export"))
        );
        assert_eq!(section.findings_window_hours, 72);
        assert!(section.validate().is_ok());
        Ok(())
    }

    #[test]
    fn test_validate_rejects_relative_export_dir() -> TestResult {
        let section = DodSection {
            export_dir: Some(PathBuf::from("dod/export")),
            ..DodSection::default()
        };
        let Err(error) = section.validate() else {
            return Err(TestError::Unexpected(
                "relative export_dir must be rejected".to_string(),
            ));
        };
        assert!(error.contains("export_dir"));
        Ok(())
    }

    #[test]
    fn test_validate_rejects_zero_findings_window() -> TestResult {
        let section = DodSection {
            findings_window_hours: 0,
            ..DodSection::default()
        };
        let Err(error) = section.validate() else {
            return Err(TestError::Unexpected(
                "findings_window_hours = 0 must be rejected".to_string(),
            ));
        };
        assert!(error.contains("findings_window_hours"));
        Ok(())
    }

    #[test]
    fn test_deserialization_runs_validate() -> TestResult {
        for (src, field) in [
            ("findings_window_hours = 0", "findings_window_hours"),
            ("export_dir = \"dod/export\"", "export_dir"),
            ("kill_requires_human = false", "kill_requires_human"),
            (
                "allowed_cgroup_prefixes = [\" \"]",
                "allowed_cgroup_prefixes",
            ),
        ] {
            let Err(error) = toml::from_str::<DodSection>(src) else {
                return Err(TestError::Unexpected(format!(
                    "`{src}` must not deserialize"
                )));
            };
            assert!(
                error.to_string().contains(field),
                "error for `{src}` names `{field}`: {error}"
            );
        }
        Ok(())
    }

    #[test]
    fn test_dod_section_full_toml_round_trip() -> TestResult {
        let src = r#"
            kill_requires_human = true
            auto_freeze = false
            proof_key_dir = "/var/lib/harw/dod/keys"
            allowed_cgroup_prefixes = ["/sys/fs/cgroup/harw.slice/"]
            export_dir = "/srv/harw-dod/export"
            findings_window_hours = 6
        "#;
        let section: DodSection = toml::from_str(src).map_err(ctx("parse toml"))?;
        assert!(section.kill_requires_human);
        assert!(!section.auto_freeze);
        assert_eq!(
            section.proof_key_dir,
            Some(PathBuf::from("/var/lib/harw/dod/keys"))
        );
        assert_eq!(
            section.allowed_cgroup_prefixes,
            vec!["/sys/fs/cgroup/harw.slice/".to_owned()]
        );

        let encoded = toml::to_string(&section).map_err(ctx("encode toml"))?;
        let decoded: DodSection = toml::from_str(&encoded).map_err(ctx("parse encoded toml"))?;
        assert_eq!(decoded, section);
        Ok(())
    }

    #[test]
    fn test_dod_section_rejects_unknown_field() -> TestResult {
        let src = r#"
            kill_requires_human = true
            kill_reqiures_human = true
        "#;
        let Err(error) = toml::from_str::<DodSection>(src) else {
            return Err(TestError::Unexpected(
                "unknown field must be rejected".to_string(),
            ));
        };
        assert!(error.to_string().contains("unknown field"));
        Ok(())
    }

    #[test]
    fn test_validate_accepts_default_section() {
        assert!(DodSection::default().validate().is_ok());
    }

    #[test]
    fn test_validate_rejects_kill_requires_human_false() -> TestResult {
        let section = DodSection {
            kill_requires_human: false,
            ..DodSection::default()
        };
        let Err(error) = section.validate() else {
            return Err(TestError::Unexpected(
                "kill_requires_human = false must be rejected".to_string(),
            ));
        };
        assert!(error.contains("kill_requires_human"));
        Ok(())
    }

    #[test]
    fn test_validate_rejects_blank_cgroup_prefix() -> TestResult {
        let section = DodSection {
            allowed_cgroup_prefixes: vec!["   ".to_owned()],
            ..DodSection::default()
        };
        let Err(error) = section.validate() else {
            return Err(TestError::Unexpected(
                "blank cgroup prefix must be rejected".to_string(),
            ));
        };
        assert!(error.contains("allowed_cgroup_prefixes"));
        Ok(())
    }

    #[test]
    fn test_validate_accepts_disabled_auto_freeze_with_human_kill() {
        let section = DodSection {
            auto_freeze: false,
            ..DodSection::default()
        };
        assert!(section.validate().is_ok());
    }
}
