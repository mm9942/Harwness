//! Renderer-seitige Ergänzung zum kanonischen Host-Permit-Fragevertrag.
//!
//! # Verantwortungsbereich
//! Der eigentliche Frage-/Antwortvertrag ([`HostPermitPrompt`],
//! [`HostPermitVariant`], [`HostPermitPromptReceiver`]) und die
//! Freigabe-Ausstellungslogik (Ledger/Registry) leben seit der
//! Host-Permit-Weiterentwicklung nicht mehr hier, sondern vollständig in
//! [`harw_tool_shell::host_permit_prompt`] (Vertrag) und
//! `harw_tool_shell::exec::ShellExecutor::authorize_host_command`
//! (Ausstellung). Dieses Modul re-exportiert die drei Typen nur noch
//! (siehe unten) und besitzt ausschließlich, was tatsächlich zur
//! Oberfläche gehört:
//!
//! - die Ableitung einer **Anzeige**-Vorauswahl aus dem aktiven
//!   [`InteractionMode`] ([`preselected_variant_for_mode`]) — reine
//!   Hilfsfunktion für Aufrufer, die noch keine vom Produzenten
//!   vorausgewählte Frage in der Hand haben; die tatsächlich bei einer
//!   ankommenden [`HostPermitPrompt`] gezeigte Vorauswahl liefert bereits
//!   [`HostPermitPrompt::preselected_variant`] (vom Produzenten gesetzt, z. B.
//!   `harw_runtime::RuntimeAssembly` aus `spec.mode_override`);
//! - das Zusammenspiel mit dem Rendering: der Aufbau des
//!   Auswahldialogs (`ChoiceDialog` mit den zwei Varianten plus
//!   Ablehnungsoption) und das Absenden der Antwort über
//!   [`HostPermitPrompt::approve`]/[`HostPermitPrompt::deny`] liegen in
//!   `crate::app` (`build_host_permit_dialog`/`apply_host_permit_decision`),
//!   ebenso das Arming-Delay gegen versehentliche Tastendrücke
//!   (`drive_pauses_to_completion`, dieselbe Mechanik wie beim normalen
//!   Freigabe-Panel, siehe `approval_dialog_key_is_armed`).
//!
//! # Re-Exporte
//! [`HostPermitPrompt`], [`HostPermitVariant`], [`HostPermitPromptReceiver`]
//! — siehe [`harw_tool_shell::host_permit_prompt`] für den vollständigen
//! Vertrag und die Sicherheitsregel „Ablehnung ist der Default": ein
//! geschlossener Fragekanal, eine fallengelassene Antwort oder ein
//! Zeitablauf sind dort **keine** Zustimmung.
//!
//! # Verdrahtung (außerhalb dieses Moduls)
//! Die Empfängerseite des Fragekanals holt `harw-tui`
//! (`crate::runtime_root::build_root_runtime`) einmalig über
//! [`harw_runtime::RuntimeAssembly::take_host_permit_prompts`] ab — derselbe
//! Kanal, dessen Sendeseite
//! `harw_tool_shell::exec::ShellExecutor::authorize_host_command` über
//! [`harw_runtime::RuntimeAssembly::host_permit_prompt_sender`] bedient, wenn
//! ein `Host`-profilierter Worker ausführen will.
//!
//! # Examples
//! ```rust
//! use harw_core::InteractionMode;
//! use harw_tui::host_permit_dialog::{HostPermitVariant, preselected_variant_for_mode};
//!
//! assert_eq!(
//!     preselected_variant_for_mode(InteractionMode::Shell),
//!     HostPermitVariant::SessionLease
//! );
//! assert_eq!(
//!     preselected_variant_for_mode(InteractionMode::Work),
//!     HostPermitVariant::SingleExecution
//! );
//! ```

use harw_core::InteractionMode;

pub use harw_tool_shell::host_permit_prompt::{
    HostPermitPrompt, HostPermitPromptReceiver, HostPermitVariant,
};

/// Leitet eine **Anzeige**-Vorauswahl aus einem [`InteractionMode`] ab.
///
/// # Beschreibung
/// Im Shell-Modus (`--mode shell` / `/mode shell`) ist die
/// sitzungsweite Phase ([`HostPermitVariant::SessionLease`]) die naheliegende
/// Vorauswahl; außerhalb bleibt die engere Einzelfreigabe
/// ([`HostPermitVariant::SingleExecution`]) vorausgewählt. Bewusst ohne
/// Wildcard-Zweig (`_ =>`): kommt später ein weiterer [`InteractionMode`]
/// hinzu, muss diese Zuordnung bewusst erweitert werden, statt ihn
/// stillschweigend der Einzelfreigabe zuzuschlagen.
///
/// Ändert **nie**, was tatsächlich genehmigt wird — das entscheidet
/// ausschließlich der Mensch über [`HostPermitPrompt::approve`]. Die
/// tatsächlich bei einer ankommenden Frage angezeigte Vorauswahl ist
/// [`HostPermitPrompt::preselected_variant`], die bereits vom Produzenten
/// der Frage (z. B. `harw_runtime::RuntimeAssembly`, abgeleitet aus
/// `spec.mode_override`) gesetzt wurde; diese Funktion steht für Aufrufer
/// bereit, die dieselbe Ableitung ohne eine bereits vorliegende Frage
/// brauchen.
///
/// # Argumente
/// - `mode` ([`InteractionMode`]): der zu betrachtende Interaktionsmodus.
///
/// # Rückgabe
/// [`HostPermitVariant::SessionLease`] für [`InteractionMode::Shell`],
/// [`HostPermitVariant::SingleExecution`] für jeden anderen Modus.
#[must_use]
pub fn preselected_variant_for_mode(mode: InteractionMode) -> HostPermitVariant {
    match mode {
        InteractionMode::Shell => HostPermitVariant::SessionLease,
        InteractionMode::Chat
        | InteractionMode::Plan
        | InteractionMode::Explore
        | InteractionMode::Work => HostPermitVariant::SingleExecution,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_preselected_variant_for_mode_shell_prefers_session_lease() {
        assert_eq!(
            preselected_variant_for_mode(InteractionMode::Shell),
            HostPermitVariant::SessionLease
        );
    }

    #[test]
    fn test_preselected_variant_for_mode_work_prefers_single_execution() {
        assert_eq!(
            preselected_variant_for_mode(InteractionMode::Work),
            HostPermitVariant::SingleExecution
        );
    }

    #[test]
    fn test_preselected_variant_for_mode_chat_plan_explore_prefer_single_execution() {
        for mode in [InteractionMode::Chat, InteractionMode::Plan, InteractionMode::Explore] {
            assert_eq!(preselected_variant_for_mode(mode), HostPermitVariant::SingleExecution);
        }
    }

    /// Re-Export-Sanity: die drei Typen sind unter diesem Modulpfad
    /// erreichbar — genau die Erwartung, gegen die `crate::app` und
    /// `crate::runtime_root` heute importieren.
    #[test]
    fn test_reexported_types_are_reachable_under_this_module_path() {
        let _variant: HostPermitVariant = HostPermitVariant::default();
        fn _type_check(_receiver: HostPermitPromptReceiver) {}
        fn _prompt_type_check(_prompt: HostPermitPrompt) {}
    }
}
