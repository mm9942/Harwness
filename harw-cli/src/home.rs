//! Auflösung des Root-Space für die CLI: `--home`-Override vor
//! `HARW_HOME`/`$HOME/.harw`.

use std::path::PathBuf;

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
