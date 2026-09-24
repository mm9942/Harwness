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

/// Systemzeile beim Sitzungswechsel (`/new`, `/resume`), falls vorher eine
/// Host-Phase aktiv war (Runde 5, Teil N).
///
/// # Beschreibung
/// Ein Sitzungswechsel baut eine neue Runtime-Montage mit frischer
/// Host-Permit-Registry — eine laufende Host-Arbeitsphase (auch eine
/// prozessweite aus `/sandbox-lease` oder `request_host`) endet damit
/// bewusst. Diese Funktion liefert dann die Zeile, die die TUI anzeigt.
///
/// # Rückgabe
/// `Some("Host-Modus beendet (neue Sitzung).")`, wenn `previous_session` in
/// `registry` noch eine Phase hatte, sonst `None`.
#[must_use]
pub fn session_switch_host_notice(
    registry: &harw_sandbox::HostPermitSessionRegistry,
    previous_session: &str,
) -> Option<&'static str> {
    registry
        .is_session_approved(previous_session)
        .then_some("Host-Modus beendet (neue Sitzung).")
}

/// Titel und einzeiliger Hinweis für eine Host-Mode-Anfrage aus dem
/// Agentenbaum (Runde 5, Teil N).
///
/// # Beschreibung
/// Trägt die Frage einen Anfragenden
/// ([`HostPermitPrompt::requester`], gesetzt von `shell.exec` mit
/// `request_host`), zeigt der Dialog **wer** fragt — Baum-Pfad im Titel,
/// Rolle und Kind-ID im Hinweis —, dazu Grund, exakten Befehl und cwd, und
/// dass eine Host-Arbeitsphase für die ganze harw-Sitzung inkl. aller
/// Kind-Agenten gilt. Ohne Anfragenden liefert die Funktion `None`; der
/// Aufrufer bleibt dann bei seinem bisherigen Text.
///
/// # Rückgabe
/// `Some((titel, hinweis))` für eine Host-Mode-Anfrage, sonst `None`.
#[must_use]
pub fn escalation_dialog_text(prompt: &HostPermitPrompt) -> Option<(String, String)> {
    let requester = prompt.requester()?;
    let title = format!("Host-Mode angefragt: {}", requester.path);
    let hint = format!(
        "Rolle {} · Kind {} · Grund: {} · Befehl: {} · cwd: {} · Host-Arbeitsphase gilt für die \
         ganze harw-Sitzung inkl. aller Kind-Agenten; sudo bleibt verboten.",
        requester.role,
        requester.session,
        prompt.reason().unwrap_or("(kein Grund angegeben)"),
        prompt.command(),
        prompt.workspace().display(),
    );
    Some((title, hint))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_switch_notice_only_with_an_active_phase() {
        let registry = harw_sandbox::HostPermitSessionRegistry::default();
        assert_eq!(session_switch_host_notice(&registry, "old"), None);
        registry.mark_global_approval(std::time::Duration::from_secs(60));
        assert_eq!(
            session_switch_host_notice(&registry, "old"),
            Some("Host-Modus beendet (neue Sitzung).")
        );
    }

    #[test]
    fn test_escalation_dialog_text_names_requester_reason_command_and_cwd()
    -> crate::test_support::TestResult {
        let (plain, _answer) = HostPermitPrompt::new(
            "child-7".to_owned(),
            "host-escalation@1:executor".to_owned(),
            "cargo fetch".to_owned(),
            std::path::PathBuf::from("/workspace/shared/proj"),
            HostPermitVariant::SingleExecution,
        );
        assert!(
            escalation_dialog_text(&plain).is_none(),
            "ohne Anfragenden bleibt der bisherige Dialogtext"
        );
        let prompt = plain
            .with_requester(harw_tool_shell::HostRequester {
                role: "executor".to_owned(),
                session: "child-7".to_owned(),
                path: "uia › root-orchestrator › executor".to_owned(),
            })
            .with_reason("braucht Netz für crates.io");
        let (title, hint) = escalation_dialog_text(&prompt).ok_or(
            crate::test_support::TestError::Missing("Dialogtext einer Host-Mode-Anfrage"),
        )?;
        assert!(
            title.contains("uia › root-orchestrator › executor"),
            "{title}"
        );
        for needle in [
            "Rolle executor",
            "Kind child-7",
            "braucht Netz für crates.io",
            "cargo fetch",
            "/workspace/shared/proj",
        ] {
            assert!(hint.contains(needle), "{needle} fehlt in: {hint}");
        }
        Ok(())
    }

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
        for mode in [
            InteractionMode::Chat,
            InteractionMode::Plan,
            InteractionMode::Explore,
        ] {
            assert_eq!(
                preselected_variant_for_mode(mode),
                HostPermitVariant::SingleExecution
            );
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
