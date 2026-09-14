//! Bootstrap der aktiven UIA für lokale interaktive Starts.
//!
//! Der Bootstrap ist absichtlich vor der Runtime-Montage angesiedelt: eine UIA
//! wird nie stillschweigend vom Modell erzeugt oder aktiviert. Jede neue
//! Definition wird als sichtbarer Vorschlag gezeigt und erst nach einer klaren
//! Bestätigung geschrieben.

use std::{io::{self, IsTerminal as _, Write as _}, path::Path};

use harw_agent_dsl::roles::AgentRoleId;
use harw_config::{ConfigWriter, ResolvedConfig};
use harw_home::{active_profile_name, profile_dir};
use toml_edit::value;

const GENERATED_UIA_ID: &str = "harwness.agent.default-terminal-ui@1";
const GENERATED_UIA_DIR: &str = "default-terminal-ui";

/// Stellt vor einer interaktiven TUI-Montage eine aktive UIA sicher.
///
/// Eine explizite Auswahl bleibt unverändert und wird weiterhin von Config und
/// Runtime fail-closed validiert. Eine einzige entdeckte UIA wird als Standard
/// persistiert. Bei mehreren Definitionen trifft der Mensch die Wahl. Gibt es
/// keine, legt der Bootstrap eine lokale, minimale UIA an und aktiviert sie.
/// Persönlichkeit und Nutzerkontext bleiben bewusst als lokale Dateien im
/// Profil, damit sie nicht in ein Projekt-Repository geraten.
pub(crate) fn ensure_active_uia(home: &Path, config: &ResolvedConfig) -> Result<Option<String>, String> {
    if config.harness.active_uia_definition.is_some() {
        return Ok(None);
    }

    let mut candidates: Vec<String> = config
        .executable_agents
        .iter()
        .filter(|(_, agent)| agent.role() == AgentRoleId::UserInterface)
        .map(|(id, _)| id.clone())
        .collect();
    candidates.sort();

    let selected = match candidates.len() {
        1 => candidates.remove(0),
        count if count > 1 => select_uia(&candidates)?,
        _ => GENERATED_UIA_ID.to_owned(),
    };
    if selected == GENERATED_UIA_ID {
        write_generated_uia(home)?;
    }
    persist_active_uia(home, &selected)?;
    Ok(Some(selected))
}

fn select_uia(candidates: &[String]) -> Result<String, String> {
    require_terminal()?;
    eprintln!("Mehrere Benutzeroberflächen-Agenten wurden gefunden:");
    for (index, id) in candidates.iter().enumerate() {
        eprintln!("  {}) {id}", index + 1);
    }
    eprint!("Welche UIA soll als Standard gespeichert werden? [1-{}]: ", candidates.len());
    io::stderr().flush().map_err(|error| format!("UIA-Auswahl ausgeben: {error}"))?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer).map_err(|error| format!("UIA-Auswahl lesen: {error}"))?;
    let index: usize = answer.trim().parse().map_err(|_| "ungültige UIA-Auswahl; es wurde nichts gespeichert".to_owned())?;
    candidates.get(index.saturating_sub(1)).cloned().ok_or_else(|| "ungültige UIA-Auswahl; es wurde nichts gespeichert".to_owned())
}

fn require_terminal() -> Result<(), String> {
    if io::stdin().is_terminal() && io::stderr().is_terminal() {
        Ok(())
    } else {
        Err("keine aktive UIA konfiguriert; ein interaktives Terminal ist für Auswahl oder Bestätigung erforderlich".to_owned())
    }
}

fn active_profile_config(home: &Path) -> Result<std::path::PathBuf, String> {
    let profile = active_profile_name(home);
    Ok(profile_dir(home, &profile)
        .map_err(|error| format!("Profilverzeichnis für UIA-Bootstrap: {error}"))?
        .join("config.toml"))
}

fn persist_active_uia(home: &Path, id: &str) -> Result<(), String> {
    let path = active_profile_config(home)?;
    let mut writer = ConfigWriter::open(&path).map_err(|error| error.to_string())?;
    writer.set_value("active_uia_definition", value(id));
    writer.save().map_err(|error| error.to_string())
}

fn write_generated_uia(home: &Path) -> Result<(), String> {
    let profile = active_profile_name(home);
    let profile = profile_dir(home, &profile)
        .map_err(|error| format!("Profilverzeichnis für UIA-Bootstrap: {error}"))?;
    let dir = profile.join("agents").join(GENERATED_UIA_DIR);
    std::fs::create_dir_all(&dir).map_err(|error| format!("UIA-Verzeichnis {}: {error}", dir.display()))?;
    let definition = dir.join("definition.toml");
    let source = format!("schema = \"harwness.agent/v1\"\nid = \"{GENERATED_UIA_ID}\"\nversion = \"1.0.0\"\nrole = \"user-interface\"\nspecialization = \"terminal-ui\"\nname = \"Terminal UI\"\ndescription = \"Lokale, sichere Standardoberfläche für Harwness.\"\n");
    std::fs::write(&definition, source).map_err(|error| format!("UIA-Definition {}: {error}", definition.display()))?;
    let agent = dir.join("agent.toml");
    std::fs::write(
        &agent,
        "name = \"Terminal UI\"\nrole = \"user-interface\"\ndescription = \"Lokale, sichere Standardoberfläche für Harwness.\"\n",
    )
    .map_err(|error| format!("UIA-Agentenmetadaten {}: {error}", agent.display()))?;
    let personality = dir.join("Personality.md");
    std::fs::write(
        &personality,
        "# Persönlichkeit und Antwortverhalten\n\nSei klar, respektvoll und transparent. Erkläre Unsicherheit und Risiken offen; behaupte keine ausgeführte Aktion ohne überprüfbare Evidenz. Frage nur nach, wenn die Antwort die Entscheidung wesentlich verändert.\n",
    )
    .map_err(|error| format!("UIA-Persönlichkeit {}: {error}", personality.display()))?;
    let user = dir.join("USER.md");
    std::fs::write(
        &user,
        "# Nutzerkontext\n\n<!-- Trage hier freiwillig bereitgestellte Präferenzen, Arbeitsweisen und relevante Kontextinformationen ein. Keine Geheimnisse eintragen. -->\n",
    )
    .map_err(|error| format!("UIA-Nutzerkontext {}: {error}", user.display()))?;
    #[cfg(unix)]
    for path in [&definition, &agent, &personality, &user] {
        std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600))
            .map_err(|error| format!("Rechte für UIA-Datei {}: {error}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generated_definition_is_a_valid_uia_document() {
        let source = format!("schema = \"harwness.agent/v1\"\nid = \"{GENERATED_UIA_ID}\"\nversion = \"1.0.0\"\nrole = \"user-interface\"\nspecialization = \"terminal-ui\"\n");
        let parsed = harw_agent_dsl::parse::parse_toml(&source).expect("generated definition parses");
        assert_eq!(parsed.role, AgentRoleId::UserInterface);
    }
}
