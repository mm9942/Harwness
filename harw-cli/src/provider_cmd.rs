//! `harw provider list|add|remove|enable|disable|scan`: Provider des
//! aktiven Profils verwalten.
//!
//! Dünner Adapter: die Verwaltung läuft über denselben Weg wie
//! `harw config provider …`, der Scan über denselben Weg wie
//! `harw model scan`.

use crate::cli::{
    GlobalArgs, ModelsAction, ProviderAction, SettingsAction, SettingsProviderAction,
};
use crate::output::Printer;

/// Führt `harw provider …` aus.
///
/// # Errors
/// Ein deutscher Fehlertext, wenn `--json` verlangt wird (nur
/// Textausgabe) oder die Provider-Verwaltung bzw. der Scan scheitert.
pub fn run(g: &GlobalArgs, a: ProviderAction) -> Result<(), String> {
    let printer = Printer::new(g.output());
    let action = match a {
        ProviderAction::Scan {
            provider,
            free_only,
            prune,
        } => {
            printer.require_text("harw provider scan")?;
            return crate::models::run(
                g.home.clone(),
                Some(ModelsAction::Scan {
                    provider,
                    add: false,
                    free_only,
                    prune,
                }),
            );
        }
        ProviderAction::List => SettingsProviderAction::List,
        ProviderAction::Add {
            name,
            api,
            base_url,
            auth,
            models,
        } => SettingsProviderAction::Add {
            name,
            api,
            base_url,
            auth,
            models,
        },
        ProviderAction::Remove { name } => SettingsProviderAction::Remove { name },
        ProviderAction::Enable { name } => SettingsProviderAction::Enable { name },
        ProviderAction::Disable { name } => SettingsProviderAction::Disable { name },
    };
    printer.require_text("harw provider")?;
    crate::settings::run(g.home.clone(), Some(SettingsAction::Provider { action }))
}
