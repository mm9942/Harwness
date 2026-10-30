//! Apply committed profile preferences to live display consumers only.
use super::*;

impl ChatApp {
    /// Current session-specific interface language, independent of thread state.
    pub(crate) fn interface_language(&self) -> harw_config::UiLanguage {
        self.language
    }

    pub(crate) fn apply_display_preferences(&mut self, tui: &harw_config::TuiSection) {
        self.apply_selected_display_preferences(tui, None);
    }

    fn apply_selected_display_preferences(
        &mut self,
        tui: &harw_config::TuiSection,
        key: Option<&str>,
    ) {
        let applies = |candidate: &str| key.is_none_or(|key| key == candidate);
        let language_changed = applies("tui.language") && self.language != tui.language;
        if applies("tui.language") {
            self.language = tui.language;
        }
        if applies("tui.theme") {
            self.theme = style::configured_theme(&tui.theme);
        }
        if applies("tui.reasoning") {
            self.reasoning_mode = tui.reasoning;
        }
        if applies("tui.status_expiry") {
            self.status_expiry = tui.status_expiry;
        }
        if applies("tui.child_stream") {
            self.child_stream.set_mode(tui.child_stream.into());
        }
        if language_changed {
            crate::i18n::with_language(self.language, || {
                let registry = CommandRegistry::from_command_adapters(&self.adapters)
                    .with_local_specs(crate::command_catalog::local_command_specs());
                self.command_registry = CommandRegistry::new(
                    registry
                        .specs()
                        .iter()
                        .filter(|spec| {
                            !self
                                .hidden_commands
                                .iter()
                                .any(|hidden| hidden.eq_ignore_ascii_case(spec.name.as_str()))
                        })
                        .cloned()
                        .collect(),
                );
                if let Some(popup) = self.command_popup.as_mut() {
                    popup.refresh_catalog(&self.command_registry);
                }
                if let Some(Overlay::View(view)) = self.overlay.as_mut() {
                    view.refresh_presentation(&self.command_registry, &self.key_bindings);
                }
                match self.overlay.as_mut() {
                    Some(Overlay::ExportChoice(dialog)) => {
                        dialog.refresh_owned_presentation(export_choice_dialog())
                    }
                    Some(Overlay::EffortChoice { target, dialog }) => {
                        dialog.refresh_owned_presentation(effort_choice_dialog(*target))
                    }
                    Some(Overlay::StopJob(dialog)) => dialog.refresh_presentation(),
                    _ => {}
                }
                jobs_glue::refresh_quit_presentation(self);
                if let (Some(prompt), Some(dialog)) = (
                    &self.pending_host_permit,
                    &mut self.pending_host_permit_dialog,
                ) {
                    dialog.refresh_owned_presentation(build_host_permit_dialog(prompt));
                }
            });
        }
    }

    /// Complete a settings mutation in both idle and busy paths. Ordinary show
    /// refreshes update rows without erasing the panel's mutation feedback.
    pub(super) fn apply_settings_completion(
        &mut self,
        raw: &str,
        language: harw_config::UiLanguage,
        result: Result<&OpOutput, &str>,
    ) {
        if changed_setting_key(raw).is_none() {
            return;
        }
        if result.is_ok() {
            self.refresh_changed_display_preference(raw);
        }
        if let Some(Overlay::View(view)) = self.overlay.as_mut()
            && view.refresh_command().as_deref() == Some("/settings show")
        {
            view.apply_command_result(language, result);
        }
    }

    /// Refresh only the successful command's key, preserving other session overrides.
    pub(super) fn refresh_changed_display_preference(&mut self, raw: &str) {
        let Some(key) = changed_setting_key(raw) else {
            return;
        };
        if let Some(runtime) = &self.runtime {
            let config = runtime.current_config();
            self.apply_selected_display_preferences(&config.harness.tui, Some(&key));
        }
    }

    pub(crate) fn refresh_display_preferences(&mut self) {
        if let Some(runtime) = &self.runtime {
            let config = runtime.current_config();
            self.apply_display_preferences(&config.harness.tui);
        }
    }
}

fn changed_setting_key(raw: &str) -> Option<String> {
    let crate::Invocation::Command { name, raw_args } = crate::classify_input(raw).ok()? else {
        return None;
    };
    if name == "settings" && matches!(raw_args.first().map(String::as_str), Some("set" | "reset")) {
        raw_args.get(1).cloned()
    } else {
        None
    }
}
