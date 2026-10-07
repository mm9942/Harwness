//! Bootstrap der aktiven UIA für lokale interaktive Starts.
//!
//! Der Bootstrap ist absichtlich vor der Runtime-Montage angesiedelt: eine UIA
//! wird nie stillschweigend vom Modell erzeugt oder aktiviert. Fehlt jede
//! UIA-Definition, führt [`ensure_active_uia`] stattdessen einen kurzen
//! interaktiven Einrichtungsdialog ([`run_uia_setup_dialog`]): Name,
//! Persönlichkeit/Ton und Nutzerkontext werden erfragt, als Vorschau gezeigt
//! und erst nach expliziter Bestätigung geschrieben. Ohne verfügbares
//! Terminal (One-shot-Aufruf ohne TTY, Gateway-/Daemon-Kontext) liefert der
//! Bootstrap stattdessen einen klaren Fehler statt eines hängenden
//! Leseversuchs oder einer stillen Platzhalter-Anlage.
//!
//! `harw uia new` ([`run_new_uia_command`]) ruft denselben Dialog gezielt
//! erneut auf, auch wenn bereits eine UIA aktiv ist, und aktiviert die neu
//! angelegte UIA anschließend.
//!
//! ## Key types exported
//! - [`ensure_active_uia`] — Bootstrap-Einstiegspunkt vor der Runtime-Montage.
//! - [`run_new_uia_command`] — Dispatch-Ziel von `harw uia new`.
//!
//! ## Concurrency
//! Einzelthreadig und rein synchron: liest blockierend von `stdin`/`stderr`
//! (`io::stdin().read_line`) und schreibt Dateien sequentiell über
//! [`std::fs`]. Es werden keine Threads gespawnt und kein Zustand über
//! `Arc`/`Mutex` geteilt; der Modul ist nicht für parallele Aufrufe aus
//! mehreren Threads auf demselben Profilverzeichnis ausgelegt.
//!
//! ## Errors
//! Alle Fehler werden als `String` mit einer für den Menschen verständlichen
//! Meldung zurückgegeben (kein eigener `Error`-Typ in diesem Modul):
//! - kein interaktives Terminal verfügbar ([`require_terminal`]),
//! - Einrichtung bei der Bestätigung abgelehnt oder Eingabe endet vorzeitig
//!   (EOF) ([`run_uia_setup_dialog`], [`prompt_line`]),
//! - Verzeichnis-/Dateifehler beim Schreiben der Definition
//!   ([`write_uia_files`]),
//! - Konfigurationsfehler beim Persistieren der aktiven UIA
//!   ([`persist_active_uia`]).
//!
//! ## Examples
//! ```rust,ignore
//! # use std::path::Path;
//! # use harw_config::ResolvedConfig;
//! # use harw_registry_defaults::ConfigAgents;
//! # fn example(home: &Path, config: &ResolvedConfig, agents: &ConfigAgents) -> Result<(), String> {
//! // Vor der TUI-Montage aufrufen; startet ggf. den Einrichtungsdialog.
//! let activated = crate::uia_bootstrap::ensure_active_uia(home, config, agents)?;
//! # let _ = activated;
//! # Ok(())
//! # }
//! ```

use std::{
    io::{self, IsTerminal as _, Write as _},
    path::{Path, PathBuf},
};

use harw_agent_dsl::roles::AgentRoleId;
use harw_config::{ConfigWriter, ResolvedConfig};
use harw_home::{active_profile_name, profile_dir};
use harw_registry_defaults::ConfigAgents;
use toml_edit::value;

/// Neutraler Vorgabetext für `Personality.md`, falls die Persönlichkeitsfrage
/// im Einrichtungsdialog leer beantwortet wird.
const DEFAULT_PERSONALITY_TEXT: &str = "Sei klar, respektvoll und transparent. Erkläre Unsicherheit und Risiken offen; behaupte keine ausgeführte Aktion ohne überprüfbare Evidenz. Frage nur nach, wenn die Antwort die Entscheidung wesentlich verändert.";

/// Stellt vor einer interaktiven TUI-Montage eine aktive UIA sicher.
///
/// # Description
/// Eine explizite Auswahl bleibt unverändert und wird weiterhin von Config und
/// Runtime fail-closed validiert. Eine einzige entdeckte UIA wird als Standard
/// persistiert. Bei mehreren Definitionen trifft der Mensch die Wahl
/// ([`select_uia`]). Gibt es keine, führt der Bootstrap einen interaktiven
/// Einrichtungsdialog ([`run_uia_setup_dialog`]) durch und aktiviert das
/// Ergebnis. Persönlichkeit und Nutzerkontext bleiben bewusst als lokale
/// Dateien im Profil, damit sie nicht in ein Projekt-Repository geraten.
///
/// # Arguments
/// - `home` (`&Path`): geliehener Pfad zum Harwness-Home-Verzeichnis, aus dem
///   Profilverzeichnis und Konfigurationsdatei abgeleitet werden.
/// - `config` (`&ResolvedConfig`): geliehene, bereits aufgelöste Konfiguration
///   des aktiven Profils; wird nur gelesen, nie verändert.
/// - `agents` (`&ConfigAgents`): die gesenkten Agentendefinitionen derselben
///   Konfiguration (`harw_runtime::load_config_with_agents`); aus ihnen
///   stammen die UIA-Kandidaten.
///
/// # Returns
/// `Ok(None)`, wenn bereits eine `active_uia_definition` gesetzt ist (keine
/// Änderung). `Ok(Some(id))` mit der neu aktivierten oder bestätigten UIA-ID,
/// wenn eine einzige gefunden, ausgewählt oder frisch angelegt wurde.
///
/// # Errors
/// - Kein Terminal verfügbar, wenn mehrere Kandidaten eine Auswahl oder keine
///   Kandidaten eine Einrichtung erfordern (siehe [`require_terminal`]).
/// - Die Auswahl oder Einrichtung wird abgebrochen oder liefert eine
///   ungültige Eingabe.
/// - Schreib-/Konfigurationsfehler beim Persistieren der aktiven UIA
///   ([`persist_active_uia`]).
///
/// # Concurrency
/// Einzelthreadig; blockiert synchron auf `stdin`, falls eine Auswahl oder
/// Einrichtung nötig ist.
///
/// # Examples
/// ```rust,ignore
/// # use std::path::Path;
/// # use harw_config::ResolvedConfig;
/// # use harw_registry_defaults::ConfigAgents;
/// # fn example(home: &Path, config: &ResolvedConfig, agents: &ConfigAgents) -> Result<(), String> {
/// if let Some(activated) = crate::uia_bootstrap::ensure_active_uia(home, config, agents)? {
///     eprintln!("UIA {activated} aktiviert");
/// }
/// # Ok(())
/// # }
/// ```
pub(crate) fn ensure_active_uia(
    home: &Path,
    config: &ResolvedConfig,
    agents: &ConfigAgents,
) -> Result<Option<String>, String> {
    if config.harness.active_uia_definition.is_some() {
        return Ok(None);
    }

    let mut candidates: Vec<String> = agents
        .executable_agents
        .iter()
        .filter(|(_, agent)| agent.role() == AgentRoleId::UserInterface)
        .map(|(id, _)| id.clone())
        .collect();
    candidates.sort();

    let selected = match candidates.len() {
        1 => candidates.remove(0),
        count if count > 1 => select_uia(&candidates)?,
        _ => run_uia_setup_dialog(home, DialogContext::NoUiaConfigured)?,
    };
    persist_active_uia(home, &selected)?;
    Ok(Some(selected))
}

/// Führt `harw uia new` aus: ruft den Einrichtungsdialog gezielt erneut auf
/// (auch wenn bereits eine UIA aktiv ist) und aktiviert die neu angelegte UIA.
///
/// # Description
/// Löst zunächst das Home-Verzeichnis auf (`--home`-Override oder Standard)
/// und stellt sicher, dass es existiert. Führt dann unabhängig vom
/// bisherigen Zustand von `active_uia_definition` den vollständigen
/// Einrichtungsdialog ([`run_uia_setup_dialog`]) im Kontext
/// [`DialogContext::AdditionalUia`] aus, persistiert die neue UIA als aktiv
/// und gibt eine Bestätigung auf `stderr` aus.
///
/// # Arguments
/// - `home_override` (`Option<PathBuf>`): optionaler, übernommener Pfad, der
///   die automatische Home-Auflösung ersetzt (z. B. aus `--home`).
///
/// # Returns
/// `Ok(())`, sobald die neue UIA angelegt, aktiviert und die
/// Erfolgsmeldung ausgegeben wurde.
///
/// # Errors
/// - Fehler bei der Home-Auflösung oder -Anlage
///   (`crate::home::resolve_home`, `harw_home::ensure_home`).
/// - Kein Terminal verfügbar, Einrichtung abgebrochen oder Eingabe endet
///   vorzeitig (EOF) (siehe [`run_uia_setup_dialog`]).
/// - Schreib-/Konfigurationsfehler beim Persistieren der aktiven UIA
///   ([`persist_active_uia`]).
///
/// # Concurrency
/// Einzelthreadig; blockiert synchron auf `stdin` während des Dialogs.
///
/// # Examples
/// ```rust,ignore
/// # fn example() -> Result<(), String> {
/// crate::uia_bootstrap::run_new_uia_command(None)
/// # }
/// ```
pub(crate) fn run_new_uia_command(home_override: Option<PathBuf>) -> Result<(), String> {
    let home = crate::home::resolve_home(home_override)?;
    crate::home::ensure_home(&home).map_err(|error| error.to_string())?;
    let id = run_uia_setup_dialog(&home, DialogContext::AdditionalUia)?;
    persist_active_uia(&home, &id)?;
    eprintln!("UIA \"{id}\" angelegt und aktiviert.");
    Ok(())
}

/// Unterscheidet die Einleitung des Einrichtungsdialogs je Aufrufkontext.
enum DialogContext {
    /// Automatischer Bootstrap, weil noch keine UIA existiert.
    NoUiaConfigured,
    /// Gezielter Zusatzaufruf über `harw uia new`, ggf. neben einer
    /// bereits aktiven UIA.
    AdditionalUia,
}

// INTERNAL: Fragt bei mehreren gefundenen UIA-Kandidaten (Rollen-Filter aus
// `ensure_active_uia`) per numeriertem Menü auf `stderr` eine Auswahl ab.
// Erfordert ein Terminal (`require_terminal`); eine nicht parsbare oder
// außerhalb `[1..=candidates.len()]` liegende Antwort ist ein Fehler statt
// eines stillen Defaults, damit nie eine falsche UIA aktiviert wird.
fn select_uia(candidates: &[String]) -> Result<String, String> {
    require_terminal()?;
    eprintln!("Mehrere Benutzeroberflächen-Agenten wurden gefunden:");
    for (index, id) in candidates.iter().enumerate() {
        eprintln!("  {}) {id}", index + 1);
    }
    eprint!(
        "Welche UIA soll als Standard gespeichert werden? [1-{}]: ",
        candidates.len()
    );
    io::stderr()
        .flush()
        .map_err(|error| format!("UIA-Auswahl ausgeben: {error}"))?;
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map_err(|error| format!("UIA-Auswahl lesen: {error}"))?;
    let index: usize = answer
        .trim()
        .parse()
        .map_err(|_| "ungültige UIA-Auswahl; es wurde nichts gespeichert".to_owned())?;
    candidates
        .get(index.saturating_sub(1))
        .cloned()
        .ok_or_else(|| "ungültige UIA-Auswahl; es wurde nichts gespeichert".to_owned())
}

/// Führt den interaktiven Einrichtungsdialog für eine neue UIA aus.
///
/// # Description
/// Erfragt Name/Identität, Ton/Persönlichkeit (freitextig, mit Beispielen)
/// sowie freiwilligen Nutzerkontext für `USER.md`, zeigt vor dem Schreiben
/// eine Vorschau und holt eine explizite Bestätigung ein. Erst danach werden
/// `definition.toml`, `agent.toml`, `Personality.md` und `USER.md` in einem
/// neuen, kollisionsfreien Agentenverzeichnis unter `<profil>/agents/`
/// angelegt ([`write_uia_files`]).
///
/// # Arguments
/// - `home` (`&Path`): geliehener Pfad zum Harwness-Home-Verzeichnis, aus dem
///   das aktive Profilverzeichnis abgeleitet wird.
/// - `context` (`DialogContext`): steuert nur die Einleitungstexte
///   (Erstanlage vs. gezielter Zusatzaufruf über `harw uia new`), übernimmt
///   ansonsten keine weitere Logik.
///
/// # Returns
/// Die kanonische ID (`harwness.agent.<slug>@1`) der neu angelegten UIA.
///
/// # Errors
/// - Kein Terminal verfügbar (One-shot ohne TTY, Gateway-/Daemon-Kontext).
/// - Die Einrichtung wird bei der Bestätigung abgelehnt oder die Eingabe
///   endet vorzeitig (EOF).
/// - Verzeichnis-/Dateifehler beim Schreiben der Definition.
///
/// # Concurrency
/// Einzelthreadig; blockiert wiederholt synchron auf `stdin`, bis alle
/// Prompts beantwortet und die Bestätigung eingeholt wurden.
fn run_uia_setup_dialog(home: &Path, context: DialogContext) -> Result<String, String> {
    require_terminal()?;

    match context {
        DialogContext::NoUiaConfigured => {
            eprintln!("Keine Benutzeroberflächen-Agentin (UIA) konfiguriert.");
            eprintln!("Kurze Einrichtung, bevor der Chat startet:");
        }
        DialogContext::AdditionalUia => {
            eprintln!("Einrichtung einer zusätzlichen Benutzeroberflächen-Agentin (UIA):");
        }
    }
    eprintln!();

    let name = prompt_required("Name/Identität der UIA (z. B. \"Ada\", \"Terminal-Assistent\")")?;

    eprintln!();
    eprintln!("Selbstverständnis — frei formuliert, z. B.:");
    eprintln!("  - \"Ich bin Ada. Ich koordiniere Coding-Aufträge als eigenständige Assistenz.\"");
    let identity_input =
        prompt_optional("Wer/was ist diese UIA selbst? (optional, leer = neutraler Standard)")?;

    eprintln!();
    eprintln!("Ton/Persönlichkeit — freitextig, z. B.:");
    eprintln!("  - \"Ruhig, direkt, sicherheitsbewusst; erklärt Risiken offen.\"");
    eprintln!("  - \"Warmherzig, sorgfältig, fragt gezielt nach, wenn etwas unklar ist.\"");
    let personality_input = prompt_optional("Persönlichkeit (leer = neutraler Standard)")?;
    let personality = if personality_input.is_empty() {
        DEFAULT_PERSONALITY_TEXT.to_owned()
    } else {
        personality_input
    };

    eprintln!();
    eprintln!("Nutzerkontext für USER.md — was soll die UIA über dich wissen?");
    let user_name = prompt_optional("Dein Name (optional, leer = überspringen)")?;
    let user_context =
        prompt_optional("Weiterer Kontext (optional, eine Zeile, leer = überspringen)")?;

    let profile = profile_directory(home)?;
    let agents_dir = profile.join("agents");
    let slug = unique_agent_slug(&agents_dir, &slugify(&name));
    let id = format!("harwness.agent.{slug}@1");

    let identity_md = if identity_input.is_empty() {
        format!("# UIA-Identität\n\nIch bin {name}, eine lokale Benutzeroberflächen-Agentin.\n")
    } else {
        format!("# UIA-Identität\n\n{identity_input}\n")
    };
    let personality_md = format!("# Persönlichkeit und Antwortverhalten\n\n{personality}\n");
    let mut user_md = String::from(
        "# Nutzerkontext\n\n<!-- Trage hier freiwillig bereitgestellte Präferenzen, Arbeitsweisen und relevante Kontextinformationen ein. Keine Geheimnisse eintragen. -->\n",
    );
    if !user_name.is_empty() {
        user_md.push_str(&format!("\n- Name: {user_name}\n"));
    }
    if !user_context.is_empty() {
        user_md.push_str(&format!("- Kontext: {user_context}\n"));
    }

    eprintln!();
    eprintln!("--- Vorschau ---");
    eprintln!("ID:           {id}");
    eprintln!("Name:         {name}");
    eprintln!("Verzeichnis:  agents/{slug}/");
    eprintln!("Identität:");
    eprintln!("{identity_md}");
    eprintln!("Persönlichkeit:");
    eprintln!("{personality}");
    eprintln!("USER.md:");
    eprintln!("{user_md}");
    eprintln!("----------------");

    if !confirm("Diese UIA jetzt anlegen und aktivieren?")? {
        return Err("UIA-Einrichtung abgebrochen".to_owned());
    }

    write_uia_files(
        &agents_dir.join(&slug),
        &id,
        &name,
        &identity_md,
        &personality_md,
        &user_md,
    )?;
    Ok(id)
}

/// Schreibt eine neue UIA-Definition (`definition.toml`, `agent.toml`,
/// `identity.md`, `Personality.md`, `USER.md`) in `dir` und setzt unter Unix
/// `0o600`.
///
/// # Description
/// Legt `dir` rekursiv an, escaped `name` für den TOML-Basic-String-Kontext
/// ([`escape_toml_basic_string`]) und schreibt anschließend alle fünf
/// Dateien sequentiell. `identity.md` wird — anders als bei
/// `agents.write_uia` — **immer** geschrieben (mit neutralem Standardtext,
/// falls der Dialog leer beantwortet wurde), damit jede über den
/// Einrichtungsdialog angelegte UIA von Anfang an eine eigene Identität
/// besitzt (siehe `harw_config::loader::load_uia_identity`). Nur unter
/// `#[cfg(unix)]` werden die Zugriffsrechte aller fünf Dateien auf `0o600`
/// gesetzt, damit Identität, Persönlichkeit und Nutzerkontext nicht für
/// andere lokale Benutzer lesbar sind.
///
/// # Arguments
/// - `dir` (`&Path`): geliehener Zielpfad des neuen Agentenverzeichnisses;
///   wird bei Bedarf inklusive Elternverzeichnissen angelegt.
/// - `id` (`&str`): kanonische UIA-ID (`harwness.agent.<slug>@1`), die
///   unverändert in `definition.toml` eingebettet wird.
/// - `name` (`&str`): Anzeigename der UIA; wird vor dem Einbetten escaped.
/// - `identity_md` (`&str`): vollständiger Inhalt für `identity.md`.
/// - `personality_md` (`&str`): vollständiger Inhalt für `Personality.md`.
/// - `user_md` (`&str`): vollständiger Inhalt für `USER.md`.
///
/// # Returns
/// `Ok(())`, sobald alle fünf Dateien geschrieben (und unter Unix mit
/// `0o600` versehen) wurden.
///
/// # Errors
/// - Verzeichnis kann nicht angelegt werden.
/// - Eine der fünf Dateien kann nicht geschrieben werden.
/// - Unter Unix: die Zugriffsrechte einer Datei können nicht gesetzt werden.
///
/// # Concurrency
/// Einzelthreadig, rein synchrone Dateisystemoperationen ohne Locking; nicht
/// für parallele Aufrufe auf demselben `dir` aus mehreren Threads gedacht.
fn write_uia_files(
    dir: &Path,
    id: &str,
    name: &str,
    identity_md: &str,
    personality_md: &str,
    user_md: &str,
) -> Result<(), String> {
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("UIA-Verzeichnis {}: {error}", dir.display()))?;

    let escaped_name = escape_toml_basic_string(name);
    let definition = dir.join("definition.toml");
    let source = format!(
        "schema = \"harwness.agent/v1\"\nid = \"{id}\"\nversion = \"1.0.0\"\nrole = \"user-interface\"\nspecialization = \"terminal-ui\"\nname = \"{escaped_name}\"\ndescription = \"Lokale Benutzeroberflächen-Agentin, interaktiv eingerichtet.\"\n"
    );
    std::fs::write(&definition, source)
        .map_err(|error| format!("UIA-Definition {}: {error}", definition.display()))?;

    let agent = dir.join("agent.toml");
    std::fs::write(
        &agent,
        format!(
            "name = \"{escaped_name}\"\nrole = \"user-interface\"\ndescription = \"Lokale Benutzeroberflächen-Agentin, interaktiv eingerichtet.\"\n"
        ),
    )
    .map_err(|error| format!("UIA-Agentenmetadaten {}: {error}", agent.display()))?;

    let identity = dir.join("identity.md");
    std::fs::write(&identity, identity_md)
        .map_err(|error| format!("UIA-Identität {}: {error}", identity.display()))?;

    let personality = dir.join("Personality.md");
    std::fs::write(&personality, personality_md)
        .map_err(|error| format!("UIA-Persönlichkeit {}: {error}", personality.display()))?;

    let user = dir.join("USER.md");
    std::fs::write(&user, user_md)
        .map_err(|error| format!("UIA-Nutzerkontext {}: {error}", user.display()))?;

    #[cfg(unix)]
    for path in [&definition, &agent, &identity, &personality, &user] {
        std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600))
            .map_err(|error| format!("Rechte für UIA-Datei {}: {error}", path.display()))?;
    }
    Ok(())
}

/// Erzeugt einen ASCII-Kleinbuchstaben-Bindestrich-Slug aus einer freien
/// Texteingabe. Nicht-alphanumerische Zeichen werden zu einzelnen `-`
/// zusammengefasst; ein leerer Slug fällt auf `"uia"` zurück.
fn slugify(input: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = false;
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !slug.is_empty() {
            slug.push('-');
            last_dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        "uia".to_owned()
    } else {
        slug
    }
}

/// Liefert `base_slug` unverändert, falls noch kein Agentenverzeichnis dieses
/// Namens existiert; sonst einen nummerierten Ausweich-Slug (`-2`, `-3`, …).
fn unique_agent_slug(agents_dir: &Path, base_slug: &str) -> String {
    if !agents_dir.join(base_slug).exists() {
        return base_slug.to_owned();
    }
    let mut suffix = 2u32;
    loop {
        let candidate = format!("{base_slug}-{suffix}");
        if !agents_dir.join(&candidate).exists() {
            return candidate;
        }
        suffix += 1;
    }
}

/// Escaped einen Wert für die Verwendung als TOML-Basic-String: Kontrollzeichen
/// werden entfernt, Backslashes und Anführungszeichen werden escaped.
fn escape_toml_basic_string(input: &str) -> String {
    input
        .chars()
        .filter(|ch| !ch.is_control())
        .collect::<String>()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

// INTERNAL: Gate vor jedem blockierenden Lesevorgang von `stdin` in diesem
// Modul (Auswahl, Einrichtungsdialog, Bestätigung). Prüft `stdin` UND
// `stderr` auf TTY, weil beide für Prompt-Ausgabe bzw. -Eingabe gebraucht
// werden; fehlt eines, würde ein `read_line` in einem One-shot-/Gateway-
// Kontext ohne TTY sonst unbegrenzt blockieren statt sauber fehlzuschlagen.
fn require_terminal() -> Result<(), String> {
    if io::stdin().is_terminal() && io::stderr().is_terminal() {
        Ok(())
    } else {
        Err("keine aktive UIA konfiguriert; ein interaktives Terminal ist für Einrichtung, Auswahl oder Bestätigung erforderlich".to_owned())
    }
}

/// Liest eine Zeile von `stdin` für den Einrichtungsdialog; ein leeres Ergebnis
/// bei EOF wird als Abbruch behandelt statt als leere Eingabe missverstanden.
fn prompt_line(label: &str) -> Result<String, String> {
    eprint!("{label}: ");
    io::stderr()
        .flush()
        .map_err(|error| format!("UIA-Einrichtung ausgeben: {error}"))?;
    let mut answer = String::new();
    let bytes_read = io::stdin()
        .read_line(&mut answer)
        .map_err(|error| format!("UIA-Einrichtung lesen: {error}"))?;
    if bytes_read == 0 {
        return Err("UIA-Einrichtung abgebrochen (Eingabe beendet)".to_owned());
    }
    Ok(answer.trim().to_owned())
}

/// Fragt wiederholt nach, bis eine nicht-leere Antwort vorliegt.
fn prompt_required(label: &str) -> Result<String, String> {
    loop {
        let value = prompt_line(label)?;
        if !value.is_empty() {
            return Ok(value);
        }
        eprintln!("  (Eingabe darf nicht leer sein.)");
    }
}

/// Fragt einmalig ab; eine leere Antwort ist gültig (überspringen/Default).
fn prompt_optional(label: &str) -> Result<String, String> {
    prompt_line(label)
}

/// Fragt eine Ja/Nein-Bestätigung ab (Vorgabe bei leerer Antwort: Nein).
fn confirm(label: &str) -> Result<bool, String> {
    let answer = prompt_line(&format!("{label} [j/N]"))?;
    Ok(matches!(
        answer.to_ascii_lowercase().as_str(),
        "j" | "ja" | "y" | "yes"
    ))
}

// INTERNAL: Pfad zur `config.toml` des aktuell aktiven Profils unter `home`,
// abgeleitet aus [`profile_directory`]. Nur ein dünner Join-Helfer.
fn active_profile_config(home: &Path) -> Result<PathBuf, String> {
    Ok(profile_directory(home)?.join("config.toml"))
}

// INTERNAL: Löst das Verzeichnis des aktiven Profils unter `home` auf
// (`harw_home::active_profile_name` + `harw_home::profile_dir`). Liefert
// einen für Menschen lesbaren Fehler, falls das Profilverzeichnis nicht
// bestimmt werden kann (z. B. ungültiges `home`).
fn profile_directory(home: &Path) -> Result<PathBuf, String> {
    let profile = active_profile_name(home);
    profile_dir(home, &profile)
        .map_err(|error| format!("Profilverzeichnis für UIA-Bootstrap: {error}"))
}

// INTERNAL: Schreibt `id` als `active_uia_definition` in die `config.toml`
// des aktiven Profils via [`ConfigWriter`], erzeugt/öffnet die Datei bei
// Bedarf und speichert sie sofort. Aufgerufen sowohl vom stillen
// Ein-Kandidaten-Pfad als auch nach jedem erfolgreichen Einrichtungsdialog.
fn persist_active_uia(home: &Path, id: &str) -> Result<(), String> {
    let path = active_profile_config(home)?;
    let mut writer = ConfigWriter::open(&path).map_err(|error| error.to_string())?;
    writer
        .set_value("active_uia_definition", value(id))
        .map_err(|error| error.to_string())?;
    writer.save().map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    #[test]
    fn generated_definition_is_a_valid_uia_document() -> TestResult {
        let source = "schema = \"harwness.agent/v1\"\nid = \"harwness.agent.test-uia@1\"\nversion = \"1.0.0\"\nrole = \"user-interface\"\nspecialization = \"terminal-ui\"\n";
        let parsed = harw_agent_dsl::parse::parse_toml(source)
            .map_err(ctx("generated definition parses"))?;
        assert_eq!(parsed.role, AgentRoleId::UserInterface);
        Ok(())
    }

    // --- slugify ---

    #[test]
    fn test_slugify_simple_name() {
        assert_eq!(slugify("Ada"), "ada");
    }

    #[test]
    fn test_slugify_spaces_become_single_dash() {
        assert_eq!(slugify("Terminal Assistent"), "terminal-assistent");
    }

    #[test]
    fn test_slugify_special_characters_collapse_to_dash() {
        assert_eq!(slugify("Ada!!!Bot??"), "ada-bot");
    }

    #[test]
    fn test_slugify_leading_and_trailing_punctuation_trimmed() {
        assert_eq!(slugify("  -Ada-  "), "ada");
    }

    #[test]
    fn test_slugify_unicode_letters_are_dropped_as_non_ascii_alphanumeric() {
        // Non-ASCII letters (e.g. German umlauts) are not ASCII alphanumeric,
        // so they act as separators; only the ASCII letters survive.
        assert_eq!(slugify("Ä Übersetzerin"), "bersetzerin");
    }

    #[test]
    fn test_slugify_empty_string_falls_back_to_uia() {
        assert_eq!(slugify(""), "uia");
    }

    #[test]
    fn test_slugify_only_punctuation_falls_back_to_uia() {
        assert_eq!(slugify("!!!---???"), "uia");
    }

    // --- unique_agent_slug ---

    #[test]
    fn test_unique_agent_slug_no_collision_returns_base() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("create temp dir"))?;
        let result = unique_agent_slug(dir.path(), "ada");
        assert_eq!(result, "ada");
        Ok(())
    }

    #[test]
    fn test_unique_agent_slug_single_collision_returns_suffix_2() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("create temp dir"))?;
        std::fs::create_dir_all(dir.path().join("ada"))
            .map_err(ctx("create existing agent dir"))?;
        let result = unique_agent_slug(dir.path(), "ada");
        assert_eq!(result, "ada-2");
        Ok(())
    }

    #[test]
    fn test_unique_agent_slug_multiple_collisions_returns_suffix_3() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("create temp dir"))?;
        std::fs::create_dir_all(dir.path().join("ada"))
            .map_err(ctx("create existing agent dir"))?;
        std::fs::create_dir_all(dir.path().join("ada-2"))
            .map_err(ctx("create existing agent dir"))?;
        let result = unique_agent_slug(dir.path(), "ada");
        assert_eq!(result, "ada-3");
        Ok(())
    }

    // --- escape_toml_basic_string ---

    #[test]
    fn test_escape_toml_basic_string_quotes_are_escaped() {
        assert_eq!(escape_toml_basic_string("say \"hi\""), "say \\\"hi\\\"");
    }

    #[test]
    fn test_escape_toml_basic_string_backslashes_are_escaped() {
        assert_eq!(escape_toml_basic_string("C:\\path"), "C:\\\\path");
    }

    #[test]
    fn test_escape_toml_basic_string_control_characters_are_removed() {
        assert_eq!(
            escape_toml_basic_string("line1\nline2\ttab"),
            "line1line2tab"
        );
    }

    #[test]
    fn test_escape_toml_basic_string_plain_text_is_unchanged() {
        assert_eq!(escape_toml_basic_string("Ada"), "Ada");
    }

    // --- write_uia_files ---

    #[test]
    fn test_write_uia_files_writes_all_five_files_with_content() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("create temp dir"))?;
        let target = dir.path().join("ada");
        let result = write_uia_files(
            &target,
            "harwness.agent.ada@1",
            "Ada",
            "# UIA-Identität\n\nIch bin Ada.\n",
            "# Persönlichkeit und Antwortverhalten\n\nRuhig und direkt.\n",
            "# Nutzerkontext\n\n- Name: Test\n",
        );
        assert!(result.is_ok());

        let definition = std::fs::read_to_string(target.join("definition.toml"))
            .map_err(ctx("read definition.toml"))?;
        assert!(definition.contains("id = \"harwness.agent.ada@1\""));
        assert!(definition.contains("role = \"user-interface\""));

        let agent =
            std::fs::read_to_string(target.join("agent.toml")).map_err(ctx("read agent.toml"))?;
        assert!(agent.contains("name = \"Ada\""));

        let identity =
            std::fs::read_to_string(target.join("identity.md")).map_err(ctx("read identity.md"))?;
        assert!(identity.contains("Ich bin Ada."));

        let personality = std::fs::read_to_string(target.join("Personality.md"))
            .map_err(ctx("read Personality.md"))?;
        assert!(personality.contains("Ruhig und direkt."));

        let user = std::fs::read_to_string(target.join("USER.md")).map_err(ctx("read USER.md"))?;
        assert!(user.contains("Name: Test"));
        Ok(())
    }

    #[test]
    fn test_write_generated_uia_includes_identity_md() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("create temp dir"))?;
        let target = dir.path().join("ada");
        write_uia_files(
            &target,
            "harwness.agent.ada@1",
            "Ada",
            "# UIA-Identität\n\nIch bin Ada, eine lokale Benutzeroberflächen-Agentin.\n",
            "personality\n",
            "user\n",
        )
        .map_err(ctx("write uia files"))?;
        assert!(target.join("identity.md").exists());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn test_write_uia_files_sets_unix_permissions_to_0600() -> TestResult {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().map_err(ctx("create temp dir"))?;
        let target = dir.path().join("ada");
        let result = write_uia_files(
            &target,
            "harwness.agent.ada@1",
            "Ada",
            "identity\n",
            "personality\n",
            "user\n",
        );
        assert!(result.is_ok());

        for file_name in [
            "definition.toml",
            "agent.toml",
            "identity.md",
            "Personality.md",
            "USER.md",
        ] {
            let path = target.join(file_name);
            let metadata = std::fs::metadata(&path).map_err(ctx("read metadata"))?;
            let mode = metadata.permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "unexpected permissions for {file_name}");
        }
        Ok(())
    }

    #[test]
    fn test_write_uia_files_escapes_name_with_quotes_in_definition() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("create temp dir"))?;
        let target = dir.path().join("ada");
        let result = write_uia_files(
            &target,
            "harwness.agent.ada@1",
            "Ada \"the Bot\"",
            "identity\n",
            "personality\n",
            "user\n",
        );
        assert!(result.is_ok());

        let definition = std::fs::read_to_string(target.join("definition.toml"))
            .map_err(ctx("read definition.toml"))?;
        assert!(definition.contains("name = \"Ada \\\"the Bot\\\"\""));
        Ok(())
    }

    // --- require_terminal ---
    //
    // `require_terminal` checks `io::stdin().is_terminal() && io::stderr().is_terminal()`
    // directly against the real process file descriptors; there is no
    // injectable trait/abstraction in this module to substitute a fake
    // terminal check. Under `cargo test` stdin/stderr are not a TTY, so the
    // only reachable branch here is deterministic and already covered
    // indirectly: every dialog-entry test above that hits
    // `run_uia_setup_dialog`/`select_uia` via a non-terminal test process
    // would observe the `Err` branch. Adding a dedicated unit test would
    // either duplicate that guaranteed-non-terminal behavior or require
    // inventing a new abstraction purely for testability, which the task
    // instructions explicitly say not to do. Documented here instead of
    // forced.
}
