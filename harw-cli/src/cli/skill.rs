//! Grammatik von `harw skill` (PL-97 B3): Skill-Katalog, Trigger-Vorschau
//! ohne Modellaufruf und Prüfung eines Skill-Verzeichnisses.

use std::path::PathBuf;

use clap::{Subcommand, ValueHint};

/// Aktionen des `harw skill`-Subcommands.
#[derive(Debug, Clone, Subcommand)]
pub enum SkillAction {
    /// Listet die konfigurierten Skills mit Status und Beschreibung.
    List,
    /// Zeigt Beschreibung, Werkzeuge und MCPs eines Skills.
    Show {
        /// Skill-Name.
        #[arg(value_name = "NAME", value_hint = ValueHint::Other)]
        name: String,
    },
    /// Zeigt ohne Modellaufruf, welche Skill-Abschnitte die erste Runde einer
    /// frischen Sitzung für diese Eingaben getriggert einspielen würde, samt
    /// verworfener Treffer mit Grund.
    ///
    /// Gewählt wird über denselben Index, dieselben `[skills]`-Einstellungen
    /// und denselben Auswahlweg wie im Chat. Mindestens ein Aufgabentext oder
    /// eine der Optionen ist nötig.
    Preview {
        /// Aufgabentext wie eine Nutzereingabe (Schlüsselwörter, Fehlercodes);
        /// mehrere Wörter werden mit Leerzeichen verbunden.
        #[arg(value_name = "TEXT", num_args = 0.., value_hint = ValueHint::Other)]
        task: Vec<String>,
        /// Erstmals benutztes Werkzeug (wiederholbar), z. B. `sys.ps`.
        #[arg(long = "tool", value_name = "NAME", value_hint = ValueHint::Other)]
        tools: Vec<String>,
        /// Text einer Werkzeugausgabe, typischerweise ein Fehlercode
        /// (wiederholbar), z. B. `E0505`.
        #[arg(long = "error", value_name = "TEXT", value_hint = ValueHint::Other)]
        errors: Vec<String>,
        /// Gelesener oder geänderter Pfad (wiederholbar), z. B. `src/lib.rs`.
        #[arg(long = "path", value_name = "PATH", value_hint = ValueHint::AnyPath)]
        paths: Vec<String>,
        /// Ausgeführte Befehlszeile (wiederholbar), z. B. `"cargo test"`.
        #[arg(
            long = "command",
            value_name = "COMMAND",
            value_hint = ValueHint::CommandString
        )]
        commands: Vec<String>,
        /// Pro-Runde-Budget in Bytes statt `[skills] per_turn_bytes`
        /// (256 bis 65536).
        #[arg(long, value_name = "BYTES", value_parser = parse_budget)]
        budget: Option<usize>,
    },
    /// Prüft `skill.toml` und `triggers.toml` eines Skill-Verzeichnisses so,
    /// wie der Skill-Index sie liest, und meldet jeden Fehler und jede
    /// Warnung wörtlich; Exit-Code 1, sobald etwas zu beanstanden ist.
    Validate {
        /// Das Skill-Verzeichnis (mit `skill.toml`).
        #[arg(value_name = "DIR", value_hint = ValueHint::DirPath)]
        dir: PathBuf,
    },
}

/// Prüft `--budget` gegen den erlaubten Bereich von `[skills] per_turn_bytes`.
///
/// # Errors
/// Ein deutscher Fehlertext für keine Zahl oder eine Zahl außerhalb des
/// Bereichs; clap meldet ihn als Bedienfehler (Exit-Code 2).
fn parse_budget(value: &str) -> Result<usize, String> {
    let (min, max) = harw_config::skills_toml::SKILLS_PER_TURN_BYTES_RANGE;
    value
        .trim()
        .parse::<usize>()
        .ok()
        .filter(|bytes| (min..=max).contains(bytes))
        .ok_or_else(|| format!("erwartet eine Byte-Zahl zwischen {min} und {max}"))
}
