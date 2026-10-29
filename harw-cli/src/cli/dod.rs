//! Grammar of `harw dod` (PL-97 D3): the Harw-DoD suite from the command
//! line — read-only status, findings, sensor health, doctor and config
//! check, plus the root-only service control that mirrors
//! `make -C dod enable|disable|restart|enable-warden`.
//!
//! The help texts are German like the rest of the `harw` grammar.

use std::path::PathBuf;
use std::time::Duration;

use clap::{Args, Subcommand, ValueEnum, ValueHint};

/// Aktionen des `harw dod`-Subcommands.
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum DodAction {
    /// Zeigt den Zustand der Suite: Installation, Konfiguration, Units,
    /// Warden, Sensoren und Befunde (funktioniert auch ohne installierte Suite).
    Status,
    /// Prüft lesend die Voraussetzungen (cgroup v2, BTF, tracefs, Landlock,
    /// systemd, Daemons, Units, Konfiguration, Exporte); ändert nichts.
    Doctor,
    /// Listet die jüngsten Befunde aus dem Export, neueste zuerst.
    Findings(DodFindingsArgs),
    /// Zeigt den Gesundheitsbericht des Sentinels (`health.json`).
    Sensors,
    /// Prüft die Systemkonfiguration oder gibt eine Profilvorlage aus.
    Config {
        /// Auszuführende Konfigurations-Aktion.
        #[command(subcommand)]
        action: DodConfigAction,
    },
    /// Prüft die Konfiguration und aktiviert die Beobachtung
    /// (`harw-dod.target`); der Warden bleibt aus. Nur als root.
    Enable,
    /// Beendet und deaktiviert die Beobachtung samt jeder Warden-Aktivierung.
    /// Nur als root.
    Disable,
    /// Prüft die Konfiguration und startet die Beobachtung neu. Nur als root.
    Restart,
    /// Zeigt oder schaltet den Warden (Durchsetzung); Schalten nur als root.
    Warden {
        /// Auszuführende Warden-Aktion.
        #[command(subcommand)]
        action: DodWardenAction,
    },
    /// Gibt die eingebetteten systemd-Units aus `deploy/` aus; installiert,
    /// aktiviert und startet nichts.
    Install {
        /// Eingebettete systemd-Unit ausgeben (z. B. `harw-warden.socket`); ohne UNIT alle.
        #[arg(long, value_name = "UNIT", num_args = 0..=1, value_hint = ValueHint::Other)]
        print_systemd: Option<Option<String>>,
    },
}

/// Filter von `harw dod findings`.
#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct DodFindingsArgs {
    /// Höchstzahl der Befunde (1 bis 200).
    #[arg(
        short = 'n',
        long = "limit",
        value_name = "N",
        default_value_t = 50,
        value_parser = clap::value_parser!(u32).range(1..=200)
    )]
    pub limit: u32,
    /// Mindestschwere der Befunde.
    #[arg(long, value_enum, value_name = "SCHWERE")]
    pub severity: Option<DodSeverity>,
    /// Nur Befunde dieser Regel (z. B. `structure-drift`).
    #[arg(long, value_name = "REGEL", value_hint = ValueHint::Other)]
    pub rule: Option<String>,
    /// Nur Befunde der letzten Zeitspanne, z. B. `90m`, `24h` oder `7d`.
    #[arg(long, value_name = "DAUER", value_parser = parse_since)]
    pub since: Option<Duration>,
}

/// Mindestschwere für `harw dod findings --severity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum DodSeverity {
    /// Alle Befunde.
    Info,
    /// Ab `low`.
    Low,
    /// Ab `medium`.
    Medium,
    /// Ab `high`.
    High,
    /// Nur `critical`.
    Critical,
}

/// Aktionen von `harw dod config`.
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum DodConfigAction {
    /// Prüft die Harw-DoD-Systemkonfiguration (Standard `/etc/harw-dod/config.toml`).
    Check {
        /// Statt der Systemkonfiguration genau diese Datei prüfen.
        #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
        config: Option<PathBuf>,
    },
    /// Gibt eine Profilvorlage für harws Job-cgroups aus (die delegierte
    /// cgroup-Wurzel trägt PL-97 E3 ein).
    Suggest,
}

/// Aktionen von `harw dod warden`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Subcommand)]
pub enum DodWardenAction {
    /// Zeigt, ob der Warden aktiviert ist (Socket und Freigabedatei).
    Status,
    /// Legt `/etc/harw-dod/warden.enable` an und aktiviert den Warden-Socket.
    /// Nur als root.
    Enable,
    /// Deaktiviert Warden-Socket und -Dienst und entfernt die Freigabedatei.
    /// Nur als root.
    Disable,
}

/// Longest accepted `--since` span (one year).
const MAX_SINCE: Duration = Duration::from_secs(366 * 24 * 3600);

/// Parses `--since`: a positive number followed by `s`, `m`, `h` or `d`.
fn parse_since(value: &str) -> Result<Duration, String> {
    let value = value.trim();
    let split = value
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(|| format!("`{value}`: Einheit fehlt (s, m, h oder d)"))?;
    let (number, unit) = value.split_at(split);
    let number: u64 = number
        .parse()
        .map_err(|_| format!("`{value}`: erwartet z. B. `90m`, `24h` oder `7d`"))?;
    let factor = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        _ => return Err(format!("`{value}`: Einheit muss s, m, h oder d sein")),
    };
    let span = Duration::from_secs(number.saturating_mul(factor));
    if span.is_zero() || span > MAX_SINCE {
        return Err(format!("`{value}`: erlaubt sind 1s bis 366d"));
    }
    Ok(span)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dod_since_accepts_the_four_units_and_rejects_the_rest() {
        assert_eq!(parse_since("90m"), Ok(Duration::from_secs(5400)));
        assert_eq!(parse_since("24h"), Ok(Duration::from_secs(86_400)));
        assert_eq!(parse_since("7d"), Ok(Duration::from_secs(7 * 86_400)));
        assert_eq!(parse_since("30s"), Ok(Duration::from_secs(30)));
        for bad in ["", "24", "h", "0h", "1y", "-1h", "400d", "1.5h"] {
            assert!(parse_since(bad).is_err(), "{bad}");
        }
    }
}
