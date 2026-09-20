//! Auflösung und Herrichtung des Root-Space für die CLI: `--home`-Override vor
//! `HARW_HOME`/`$HOME/.harw`.
//!
//! [`resolve_home`] löst nur auf, [`ensure_home`] richtet zusätzlich ein. Die
//! CLI ruft durchgehend [`ensure_home`] statt [`harw_home::ensure_home`], weil
//! erst hier beide Hälften der Startausstattung zusammenkommen: das Scaffolding
//! samt eingebetteter Agentenhierarchie aus `harw-home` und der materialisierte
//! Provider-Katalog aus `harw-model-catalog`.

use std::path::{Path, PathBuf};

/// Löst den Root-Space auf, ohne ihn anzulegen.
///
/// # Arguments
/// - `override_dir` (`Option<PathBuf>`): expliziter `--home`-Wert; hat Vorrang
///   vor jeder Env-/Default-Auflösung.
///
/// # Errors
/// Ein `String`, wenn weder ein Override noch eine Home-Auflösung greift.
pub fn resolve_home(override_dir: Option<PathBuf>) -> Result<PathBuf, String> {
    match override_dir {
        Some(dir) => Ok(dir),
        None => harw_home::home_dir().map_err(|error| error.to_string()),
    }
}

/// Legt den Root-Space an und materialisiert den Provider-Katalog im aktiven
/// Profil (idempotent).
///
/// # Description
/// Führt [`harw_home::ensure_home`] aus — Verzeichnisse, Default-Dateien und
/// die eingebettete Agenten-/Skill-Hierarchie — und schreibt danach über
/// [`harw_model_catalog::seed_profile_providers`] für jeden Katalog-Provider
/// eine deaktivierte `providers/<id>.toml` ohne Schlüssel. Beide Schritte
/// überschreiben nie eine vorhandene Datei, laufen also auch auf einem
/// gewachsenen Root-Space gefahrlos und ergänzen dort nur, was fehlt.
///
/// # Arguments
/// - `home` (`&Path`): Root-Space, üblicherweise aus [`resolve_home`].
///
/// # Returns
/// Der Scaffold-Bericht; `written_files` enthält zusätzlich die neu
/// geschriebenen Provider-Dateien.
///
/// # Errors
/// Ein `String` mit der Ursache, wenn das Scaffolding oder das Provider-Seeding
/// fehlschlägt.
///
/// # Concurrency
/// Kein Locking gegen konkurrente Schreiber; ein `harw`-Prozess pro Root-Space
/// ist die erwartete Nutzung.
pub fn ensure_home(home: &Path) -> Result<harw_home::Scaffolded, String> {
    let mut report = harw_home::ensure_home(home).map_err(|error| error.to_string())?;
    let providers = harw_model_catalog::seed_profile_providers(&report.profile_dir)
        .map_err(|error| error.to_string())?;
    report.written_files.extend(providers);
    Ok(report)
}
