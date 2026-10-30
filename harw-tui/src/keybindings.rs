//! Überschreibbare Tastenbelegung der TUI (`[tui].keybindings_file`).
//!
//! Die Datei ist eine flache TOML-Tabelle, die Aktionsnamen auf Tasten-Chords
//! abbildet. Ein Wert ist entweder ein einzelner Chord oder eine Liste von
//! Chords; eine leere Liste hebt die Belegung einer Aktion auf:
//!
//! ```toml
//! toggle_explorer = "F2"
//! toggle_agents = ["F3", "ctrl+g"]
//! cycle_focus = "F4"
//! maximize_panel = "F11"
//! focus_explorer = "ctrl+e"
//! toggle_tool_cells = "ctrl+o"
//! toggle_workbench = "F5"
//! open_kanban = "F6"
//! open_mode_picker = "F7"
//! open_models = "F8"
//! open_matrix = "F9"
//! show_help = "F1"
//! scroll_panel_up = "ctrl+up"
//! scroll_panel_down = "ctrl+down"
//! focus_jobs = "alt+j"
//! focus_finished = "alt+f"
//! toggle_live_detail = "alt+l"
//! end_host_mode = []
//! ```
//!
//! Mission Control (PL-97 C2): `focus_jobs` (Standard `Alt+J`) und
//! `focus_finished` (`Alt+F`) fokussieren das Jobs- bzw.
//! Finished/Failed-Fenster (ohne eigenes Fenster das Agenten-Panel),
//! `toggle_live_detail` (`Alt+L`) öffnet/schließt die Detailansicht des
//! Agenten, den das LIVE-Fenster zeigt. `Ctrl+J` bleibt der Zeilenumbruch.
//! Diese drei Voreinstellungen kamen nach bestehenden Keybindings-Dateien
//! hinzu ([`KeyAction::default_yields`]): belegt eine Datei einen ihrer
//! Chords für eine andere Aktion (etwa `scroll_panel_down = "alt+j"`),
//! gewinnt die Datei und die nicht genannte Mission-Aktion verliert nur
//! diesen Chord — die Datei bleibt gültig. Mit leerem Composer gilt `Alt+F`
//! als `focus_finished`; enthält der fokussierte Composer Text, bleibt
//! `Alt+F` dort „Wort vorwärts“.
//!
//! `scroll_panel_up`/`scroll_panel_down` (Standard `Ctrl+↑`/`Ctrl+↓`)
//! scrollen ohne Fokuswechsel: bei offenem Dialog (Freigabe, sudo,
//! Host-Permit, Plan, `ask_user`) dessen Text, sonst das sichtbare
//! Agenten-Panel (bzw. dessen Detailansicht). Ist beides nicht sichtbar,
//! geht die Taste normal an den Composer. `Alt+↑` bleibt dem Zurückholen
//! eingereihter Nachrichten, `Shift+↑↓`/`Bild↑↓` dem Verlauf vorbehalten.
//!
//! Fest (nicht umbelegbar) im fokussierten Agenten-Panel (`F4`): `↑`/`↓`,
//! `k`/`j` und `w`/`s` wählen, `Bild↑`/`Bild↓` blättern, `Pos1`/`Ende`
//! springen, `Enter` öffnet Details bzw. klappt die Sammelzeile der
//! fertigen Agenten auf, `f` klappt sie auf/zu, `c` quittiert Fehlschläge,
//! `p` heftet den gewählten Agenten an das LIVE-Fenster (erneut: lösen).
//! `w`/`s` gelten nur dort — im Composer bleiben es normale Buchstaben. Im
//! fokussierten Jobs-Fenster wählen `↑`/`↓`/`k`/`j` (Bild↑↓, Pos1/Ende),
//! `Enter` öffnet die Job-Details; im Finished/Failed-Fenster klappen
//! `Enter`/`f` die Kategorien auf/zu, `c` quittiert Fehlschläge.
//!
//! Nicht genannte Aktionen behalten ihre Voreinstellung
//! ([`KeyBindings::default`], identisch zur bisher fest verdrahteten
//! Belegung). Unbekannte Aktionsnamen, nicht lesbare Chords, falsche
//! Werttypen und doppelt vergebene Chords werden gesammelt und gemeinsam als
//! [`KeyBindingsError::Invalid`] gemeldet.
//!
//! Chord-Syntax (Groß-/Kleinschreibung egal): Modifikatoren `ctrl`/`control`,
//! `alt`/`option`/`meta`, `shift`, `super`/`cmd`/`win`, verbunden mit `+`,
//! gefolgt von genau einer Taste: `f1`…`f24`, `esc`, `enter`, `tab`,
//! `backtab`, `backspace`, `delete`, `insert`, `home`, `end`, `pageup`,
//! `pagedown`, `up`, `down`, `left`, `right`, `space`, `plus` oder ein
//! einzelnes Zeichen (`ctrl++` ist `ctrl` + `+`). `shift+tab` wird wie
//! `backtab` behandelt, weil Terminals diese Kombination so melden.
//!
//! Nicht umbelegbar sind bewusst: `Ctrl+C`/`Ctrl+D` (Notausstieg; 2× `Ctrl+C`
//! ist der feste Not-Aus, siehe `crate::hard_kill`), `Esc`
//! (kontextabhängiges Schließen/Zurück), `Enter` samt `Shift/Alt+Enter`
//! sowie Tasten innerhalb von Dialogen und Overlays.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Modifikatoren, die für den Vergleich zählen (`HYPER`/`META` werden
/// ignoriert, weil Terminals sie nur mit Erweiterungs-Flags melden).
const RELEVANT_MODIFIERS: KeyModifiers = KeyModifiers::CONTROL
    .union(KeyModifiers::ALT)
    .union(KeyModifiers::SHIFT)
    .union(KeyModifiers::SUPER);

/// „Echte" Kommando-Modifikatoren; `SHIFT` allein macht aus einem Zeichen
/// noch keinen Befehl.
const COMMAND_MODIFIERS: KeyModifiers = KeyModifiers::CONTROL
    .union(KeyModifiers::ALT)
    .union(KeyModifiers::SUPER);

/// Eine per Tastenbelegung auslösbare Aktion.
///
/// Panel-Aktionen (Explorer/Agenten/Workbench umschalten, Fokus, Vollbild)
/// wertet `PanelState::handle_key` aus, die übrigen (auch Kanban,
/// Modus-Auswahl, Modelle, Hilfe) der Chat-/Composer-Zweig in `app.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum KeyAction {
    /// Explorer-Panel ein-/ausblenden (Standard `F2`).
    ToggleExplorer,
    /// Agenten-Panel ein-/ausblenden (Standard `F3`).
    ToggleAgents,
    /// Fokus reihum über die sichtbaren Flächen (Standard `F4`).
    CycleFocus,
    /// Fokussiertes Panel im Vollbild (Standard `F11`).
    MaximizePanel,
    /// Explorer einblenden und fokussieren (Standard `Ctrl+E`).
    FocusExplorer,
    /// Werkzeugzellen auf-/zuklappen (Standard `Ctrl+O`).
    ToggleToolCells,
    /// Laufende Host-Arbeitsphase beenden (Standard `Ctrl+H`).
    EndHostMode,
    /// Aktuelle Composer-Zeile löschen (Standard `Ctrl+K`).
    DeleteLine,
    /// Neue Zeile im Composer (Standard `Ctrl+J`).
    InsertNewline,
    /// Berechtigungsmodus ask → auto → full → plan (Standard `Shift+Tab`).
    CyclePermissionMode,
    /// Workbench-Panel ein-/ausblenden (Standard `F5`).
    ToggleWorkbench,
    /// Kanban-Board öffnen (Standard `F6`).
    OpenKanban,
    /// Modus-/Freigabe-Auswahl öffnen (Standard `F7`).
    OpenModePicker,
    /// Modelle je Rolle anzeigen (Standard `F8`).
    OpenModels,
    /// Hilfe (Befehle, Tasten, Präfixe) öffnen (Standard `F1`).
    ShowHelp,
    /// Matrix-Game-Panel öffnen (Standard `F9`).
    OpenMatrix,
    /// Agenten-Panel bzw. Text eines offenen Dialogs nach oben scrollen,
    /// ohne den Fokus zu wechseln (Standard `Ctrl+↑`).
    ScrollPanelUp,
    /// Agenten-Panel bzw. Text eines offenen Dialogs nach unten scrollen,
    /// ohne den Fokus zu wechseln (Standard `Ctrl+↓`).
    ScrollPanelDown,
    /// Bildschirm vollständig neu zeichnen (Standard `Ctrl+L`), falls der
    /// Terminal-Emulator Zeichenbreiten anders zählt und Reste stehen bleiben.
    RedrawScreen,
    /// Focus the Active Jobs window (default `Alt+J`; PL-97 C2).
    FocusJobs,
    /// Focus the Finished / Failed window (default `Alt+F`; PL-97 C2).
    FocusFinished,
    /// Open or close the detail view of the agent the LIVE window shows
    /// (default `Alt+L`; PL-97 C2).
    ToggleLiveDetail,
}

impl KeyAction {
    /// Alle Aktionen in fester Reihenfolge (auch Vorrang bei Gleichstand).
    pub(crate) const ALL: [Self; 22] = [
        Self::ToggleExplorer,
        Self::ToggleAgents,
        Self::CycleFocus,
        Self::MaximizePanel,
        Self::FocusExplorer,
        Self::ToggleToolCells,
        Self::EndHostMode,
        Self::DeleteLine,
        Self::InsertNewline,
        Self::CyclePermissionMode,
        Self::ToggleWorkbench,
        Self::OpenKanban,
        Self::OpenModePicker,
        Self::OpenModels,
        Self::ShowHelp,
        Self::OpenMatrix,
        Self::ScrollPanelUp,
        Self::ScrollPanelDown,
        Self::RedrawScreen,
        Self::FocusJobs,
        Self::FocusFinished,
        Self::ToggleLiveDetail,
    ];

    /// Name der Aktion in der Keybindings-Datei.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::ToggleExplorer => "toggle_explorer",
            Self::ToggleAgents => "toggle_agents",
            Self::CycleFocus => "cycle_focus",
            Self::MaximizePanel => "maximize_panel",
            Self::FocusExplorer => "focus_explorer",
            Self::ToggleToolCells => "toggle_tool_cells",
            Self::EndHostMode => "end_host_mode",
            Self::DeleteLine => "delete_line",
            Self::InsertNewline => "insert_newline",
            Self::CyclePermissionMode => "cycle_permission_mode",
            Self::ToggleWorkbench => "toggle_workbench",
            Self::OpenKanban => "open_kanban",
            Self::OpenModePicker => "open_mode_picker",
            Self::OpenModels => "open_models",
            Self::ShowHelp => "show_help",
            Self::OpenMatrix => "open_matrix",
            Self::ScrollPanelUp => "scroll_panel_up",
            Self::ScrollPanelDown => "scroll_panel_down",
            Self::RedrawScreen => "redraw_screen",
            Self::FocusJobs => "focus_jobs",
            Self::FocusFinished => "focus_finished",
            Self::ToggleLiveDetail => "toggle_live_detail",
        }
    }

    /// Localized short description for help and key binding summaries.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::ToggleExplorer => {
                crate::i18n::tr("help.action.toggle_explorer", "Show/hide explorer")
            }
            Self::ToggleAgents => {
                crate::i18n::tr("help.action.toggle_agents", "Show/hide agent panel")
            }
            Self::CycleFocus => crate::i18n::tr("help.action.cycle_focus", "Cycle focus"),
            Self::MaximizePanel => {
                crate::i18n::tr("help.action.maximize_panel", "Maximize focused panel")
            }
            Self::FocusExplorer => crate::i18n::tr("help.action.focus_explorer", "Focus explorer"),
            Self::ToggleToolCells => crate::i18n::tr(
                "help.action.toggle_tool_cells",
                "Expand/collapse tool cells",
            ),
            Self::EndHostMode => {
                crate::i18n::tr("help.action.end_host_mode", "End host work phase")
            }
            Self::DeleteLine => crate::i18n::tr("help.action.delete_line", "Delete input line"),
            Self::InsertNewline => {
                crate::i18n::tr("help.action.insert_newline", "Insert input newline")
            }
            Self::CyclePermissionMode => crate::i18n::tr(
                "help.action.cycle_permission_mode",
                "Cycle approval: ask → auto → full → plan",
            ),
            Self::ToggleWorkbench => {
                crate::i18n::tr("help.action.toggle_workbench", "Show/hide workbench")
            }
            Self::OpenKanban => crate::i18n::tr("help.action.open_kanban", "Open Kanban board"),
            Self::OpenModePicker => {
                crate::i18n::tr("help.action.open_mode_picker", "Choose mode and approval")
            }
            Self::OpenModels => crate::i18n::tr("help.action.open_models", "Show models by role"),
            Self::ShowHelp => crate::i18n::tr("help.action.show_help", "Show help"),
            Self::OpenMatrix => {
                crate::i18n::tr("help.action.open_matrix", "Open Matrix game panel")
            }
            Self::ScrollPanelUp => crate::i18n::tr(
                "help.action.scroll_panel_up",
                "Scroll agent panel/dialog text up",
            ),
            Self::ScrollPanelDown => crate::i18n::tr(
                "help.action.scroll_panel_down",
                "Scroll agent panel/dialog text down",
            ),
            Self::RedrawScreen => {
                crate::i18n::tr("help.action.redraw_screen", "Redraw entire screen")
            }
            Self::FocusJobs => crate::i18n::tr("help.action.focus_jobs", "Focus Jobs window"),
            Self::FocusFinished => {
                crate::i18n::tr("help.action.focus_finished", "Focus Finished/Failed window")
            }
            Self::ToggleLiveDetail => crate::i18n::tr(
                "help.action.toggle_live_detail",
                "LIVE agent: open/close details",
            ),
        }
    }

    /// Umkehrung von [`KeyAction::name`]; `None` für unbekannte Namen.
    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|action| action.name() == name)
    }

    /// Voreingestellte Chords (entsprechen der bisherigen festen Belegung).
    fn default_chords(self) -> Vec<KeyChord> {
        let (code, modifiers) = match self {
            Self::ToggleExplorer => (KeyCode::F(2), KeyModifiers::NONE),
            Self::ToggleAgents => (KeyCode::F(3), KeyModifiers::NONE),
            Self::CycleFocus => (KeyCode::F(4), KeyModifiers::NONE),
            Self::MaximizePanel => (KeyCode::F(11), KeyModifiers::NONE),
            Self::FocusExplorer => (KeyCode::Char('e'), KeyModifiers::CONTROL),
            Self::ToggleToolCells => (KeyCode::Char('o'), KeyModifiers::CONTROL),
            Self::EndHostMode => (KeyCode::Char('h'), KeyModifiers::CONTROL),
            Self::DeleteLine => (KeyCode::Char('k'), KeyModifiers::CONTROL),
            Self::InsertNewline => (KeyCode::Char('j'), KeyModifiers::CONTROL),
            Self::CyclePermissionMode => (KeyCode::BackTab, KeyModifiers::NONE),
            Self::ToggleWorkbench => (KeyCode::F(5), KeyModifiers::NONE),
            Self::OpenKanban => (KeyCode::F(6), KeyModifiers::NONE),
            Self::OpenModePicker => (KeyCode::F(7), KeyModifiers::NONE),
            Self::OpenModels => (KeyCode::F(8), KeyModifiers::NONE),
            Self::ShowHelp => (KeyCode::F(1), KeyModifiers::NONE),
            Self::OpenMatrix => (KeyCode::F(9), KeyModifiers::NONE),
            Self::ScrollPanelUp => (KeyCode::Up, KeyModifiers::CONTROL),
            Self::ScrollPanelDown => (KeyCode::Down, KeyModifiers::CONTROL),
            Self::RedrawScreen => (KeyCode::Char('l'), KeyModifiers::CONTROL),
            Self::FocusJobs => (KeyCode::Char('j'), KeyModifiers::ALT),
            Self::FocusFinished => (KeyCode::Char('f'), KeyModifiers::ALT),
            Self::ToggleLiveDetail => (KeyCode::Char('l'), KeyModifiers::ALT),
        };
        vec![KeyChord { code, modifiers }]
    }
}

impl KeyAction {
    /// `true` for actions whose default chords were added after keybinding
    /// files were already in use (PL-97 C2): a chord a file binds to another
    /// action wins over such a default, so an older file that uses
    /// `alt+j`/`alt+f`/`alt+l` elsewhere stays valid. Chords set explicitly
    /// on both sides still conflict.
    #[must_use]
    pub(crate) fn default_yields(self) -> bool {
        matches!(
            self,
            Self::FocusJobs | Self::FocusFinished | Self::ToggleLiveDetail
        )
    }
}

impl fmt::Display for KeyAction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// Eine normalisierte Tastenkombination.
///
/// Normalform: Zeichen klein, nur [`RELEVANT_MODIFIERS`], `Shift+Tab` als
/// [`KeyCode::BackTab`] ohne `SHIFT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct KeyChord {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

impl KeyChord {
    /// Normalisiert ein Terminal-Ereignis in dieselbe Form wie geparste
    /// Chords: Großbuchstaben werden zu Kleinbuchstaben plus `SHIFT`.
    fn from_event(key: &KeyEvent) -> Self {
        Self::normalized(key.code, key.modifiers)
    }

    fn normalized(code: KeyCode, modifiers: KeyModifiers) -> Self {
        let mut modifiers = modifiers.intersection(RELEVANT_MODIFIERS);
        let code = match code {
            KeyCode::BackTab => {
                modifiers.remove(KeyModifiers::SHIFT);
                KeyCode::BackTab
            }
            KeyCode::Tab if modifiers.contains(KeyModifiers::SHIFT) => {
                modifiers.remove(KeyModifiers::SHIFT);
                KeyCode::BackTab
            }
            KeyCode::Char(character) if character.is_uppercase() => {
                let mut lower = character.to_lowercase();
                match (lower.next(), lower.next()) {
                    (Some(single), None) => {
                        modifiers.insert(KeyModifiers::SHIFT);
                        KeyCode::Char(single)
                    }
                    _ => KeyCode::Char(character),
                }
            }
            other => other,
        };
        Self { code, modifiers }
    }

    /// Passt dieser Chord auf das (normalisierte) Ereignis?
    ///
    /// Die Chord-Modifikatoren müssen im Ereignis enthalten sein (wie die
    /// bisherigen `ctrl && …`-Prüfungen: `Ctrl+Shift+E` löst auch `ctrl+e`
    /// aus). Ein Zeichen-Chord ohne Kommando-Modifikator passt jedoch nicht
    /// auf ein Ereignis mit `Ctrl`/`Alt`/`Super`.
    fn matches(&self, event: &Self) -> bool {
        if self.code != event.code || !event.modifiers.contains(self.modifiers) {
            return false;
        }
        let plain_char = matches!(self.code, KeyCode::Char(_))
            && self.modifiers.intersection(COMMAND_MODIFIERS).is_empty();
        !plain_char || event.modifiers.intersection(COMMAND_MODIFIERS).is_empty()
    }

    /// Spezifität für die Auswahl bei mehreren Treffern.
    fn specificity(&self) -> u32 {
        self.modifiers.bits().count_ones()
    }
}

impl FromStr for KeyChord {
    type Err = String;

    /// Parst einen Chord wie `ctrl+shift+x`, `F5`, `esc` oder `a`.
    ///
    /// # Fehler
    /// Eine lesbare Beschreibung, warum `raw` kein gültiger Chord ist.
    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        parse_chord(raw)
    }
}

impl fmt::Display for KeyChord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (flag, label) in [
            (KeyModifiers::CONTROL, "ctrl+"),
            (KeyModifiers::ALT, "alt+"),
            (KeyModifiers::SUPER, "super+"),
            (KeyModifiers::SHIFT, "shift+"),
        ] {
            if self.modifiers.contains(flag) {
                formatter.write_str(label)?;
            }
        }
        match self.code {
            KeyCode::F(number) => write!(formatter, "f{number}"),
            KeyCode::Char(' ') => formatter.write_str("space"),
            KeyCode::Char('+') => formatter.write_str("plus"),
            KeyCode::Char(character) => write!(formatter, "{character}"),
            KeyCode::Esc => formatter.write_str("esc"),
            KeyCode::Enter => formatter.write_str("enter"),
            KeyCode::Tab => formatter.write_str("tab"),
            KeyCode::BackTab => formatter.write_str("shift+tab"),
            KeyCode::Backspace => formatter.write_str("backspace"),
            KeyCode::Delete => formatter.write_str("delete"),
            KeyCode::Insert => formatter.write_str("insert"),
            KeyCode::Home => formatter.write_str("home"),
            KeyCode::End => formatter.write_str("end"),
            KeyCode::PageUp => formatter.write_str("pageup"),
            KeyCode::PageDown => formatter.write_str("pagedown"),
            KeyCode::Up => formatter.write_str("up"),
            KeyCode::Down => formatter.write_str("down"),
            KeyCode::Left => formatter.write_str("left"),
            KeyCode::Right => formatter.write_str("right"),
            other => write!(formatter, "{other:?}"),
        }
    }
}

/// Parst einen Tasten-Chord (siehe Moduldoku für die Syntax).
///
/// # Fehler
/// Eine lesbare Beschreibung bei leerem Chord, unbekanntem Modifikator,
/// unbekannter Taste, doppeltem Modifikator oder fehlender Taste.
pub(crate) fn parse_chord(raw: &str) -> Result<KeyChord, String> {
    parse_chord_diagnostic(raw).map_err(|error| error.to_string())
}

#[derive(Debug)]
pub(crate) struct BindingProblem {
    key: &'static str,
    english: &'static str,
    args: Vec<(&'static str, String)>,
    action: Option<String>,
}
impl BindingProblem {
    fn new(key: &'static str, english: &'static str, args: &[(&'static str, &str)]) -> Self {
        Self {
            key,
            english,
            args: args.iter().map(|(k, v)| (*k, (*v).to_owned())).collect(),
            action: None,
        }
    }
    fn present_for(&self, language: harw_config::UiLanguage) -> String {
        let args: Vec<_> = self.args.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let text = crate::i18n::tr_args_for(language, self.key, self.english, &args);
        self.action
            .as_ref()
            .map_or(text.clone(), |action| format!("{action}: {text}"))
    }
}
impl fmt::Display for BindingProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.present_for(harw_config::UiLanguage::English))
    }
}
fn problem(
    key: &'static str,
    english: &'static str,
    args: &[(&'static str, &str)],
) -> BindingProblem {
    BindingProblem::new(key, english, args)
}
fn parse_chord_diagnostic(raw: &str) -> Result<KeyChord, BindingProblem> {
    let lowered = raw.trim().to_lowercase();
    if lowered.is_empty() {
        return Err(problem(
            "command_feedback.key_empty",
            "Empty key chord",
            &[],
        ));
    }
    // `ctrl++` bzw. `+` meint die Plus-Taste selbst.
    let (modifier_part, key_part) = if lowered == "+" {
        ("", "+")
    } else if let Some(prefix) = lowered.strip_suffix("++") {
        (prefix, "+")
    } else {
        match lowered.rsplit_once('+') {
            Some((prefix, key)) => (prefix, key),
            None => ("", lowered.as_str()),
        }
    };
    if key_part.is_empty() {
        return Err(problem(
            "command_feedback.key_missing",
            "Key chord '{raw}' has no key after '+'",
            &[("raw", raw)],
        ));
    }

    let mut modifiers = KeyModifiers::NONE;
    if !modifier_part.is_empty() {
        for token in modifier_part.split('+') {
            let flag = match token.trim() {
                "ctrl" | "control" => KeyModifiers::CONTROL,
                "alt" | "option" | "meta" => KeyModifiers::ALT,
                "shift" => KeyModifiers::SHIFT,
                "super" | "cmd" | "win" => KeyModifiers::SUPER,
                "" => {
                    return Err(problem(
                        "command_feedback.key_empty_modifier",
                        "Key chord '{raw}' contains an empty modifier",
                        &[("raw", raw)],
                    ));
                }
                other => {
                    return Err(problem(
                        "command_feedback.key_modifier",
                        "Unknown modifier '{modifier}' in key chord '{raw}'",
                        &[("modifier", other), ("raw", raw)],
                    ));
                }
            };
            if modifiers.contains(flag) {
                return Err(problem(
                    "command_feedback.key_duplicate_modifier",
                    "Duplicate modifier '{modifier}' in key chord '{raw}'",
                    &[("modifier", token), ("raw", raw)],
                ));
            }
            modifiers.insert(flag);
        }
    }

    let code = parse_key_name(key_part.trim()).ok_or_else(|| {
        problem(
            "command_feedback.key_unknown",
            "Unknown key '{key}' in key chord '{raw}'",
            &[("key", key_part), ("raw", raw)],
        )
    })?;
    Ok(KeyChord::normalized(code, modifiers))
}

/// Tastenname (bereits klein geschrieben) → [`KeyCode`].
fn parse_key_name(name: &str) -> Option<KeyCode> {
    let code = match name {
        "esc" | "escape" => KeyCode::Esc,
        "enter" | "return" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "backtab" => KeyCode::BackTab,
        "backspace" | "bs" => KeyCode::Backspace,
        "delete" | "del" => KeyCode::Delete,
        "insert" | "ins" => KeyCode::Insert,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" | "pgup" => KeyCode::PageUp,
        "pagedown" | "pgdn" => KeyCode::PageDown,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "space" => KeyCode::Char(' '),
        "plus" => KeyCode::Char('+'),
        _ => {
            if let Some(number) = name.strip_prefix('f')
                && !number.is_empty()
                && number.bytes().all(|byte| byte.is_ascii_digit())
            {
                return match number.parse::<u8>() {
                    Ok(value @ 1..=24) => Some(KeyCode::F(value)),
                    _ => None,
                };
            }
            let mut characters = name.chars();
            match (characters.next(), characters.next()) {
                (Some(single), None) => KeyCode::Char(single),
                _ => return None,
            }
        }
    };
    Some(code)
}

/// Fehler beim Laden einer Keybindings-Datei.
#[derive(Debug)]
pub(crate) enum KeyBindingsError {
    /// Datei existiert, ist aber nicht lesbar.
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    /// Kein gültiges TOML.
    Parse { path: PathBuf, message: String },
    /// Gültiges TOML mit ungültigen Einträgen; `problems` listet jeden
    /// einzelnen (unbekannte Aktion, ungültiger Chord, Werttyp, Konflikt).
    Invalid {
        path: PathBuf,
        problems: Vec<BindingProblem>,
    },
}

impl KeyBindingsError {
    pub(crate) fn present_for(&self, language: harw_config::UiLanguage) -> String {
        let (key, english, path, detail) = match self {
            Self::Read { path, source } => (
                "command_feedback.key_read",
                "Cannot read keybindings file {path}: {detail}",
                path,
                source.to_string(),
            ),
            Self::Parse { path, message } => (
                "command_feedback.key_parse",
                "Invalid TOML in keybindings file {path}: {detail}",
                path,
                message.clone(),
            ),
            Self::Invalid { path, problems } => (
                "command_feedback.key_invalid",
                "Invalid keybindings in {path}: {detail}",
                path,
                problems
                    .iter()
                    .map(|p| p.present_for(language))
                    .collect::<Vec<_>>()
                    .join("; "),
            ),
        };
        crate::i18n::tr_args_for(
            language,
            key,
            english,
            &[("path", &path.display().to_string()), ("detail", &detail)],
        )
    }
}

impl fmt::Display for KeyBindingsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, source } => {
                write!(
                    formatter,
                    "cannot read keybindings file {}: {source}",
                    path.display()
                )
            }
            Self::Parse { path, message } => {
                write!(
                    formatter,
                    "invalid TOML in keybindings file {}: {message}",
                    path.display()
                )
            }
            Self::Invalid { path, problems } => write!(
                formatter,
                "invalid keybindings in {}: {}",
                path.display(),
                problems
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
        }
    }
}

impl std::error::Error for KeyBindingsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read { source, .. } => Some(source),
            Self::Parse { .. } | Self::Invalid { .. } => None,
        }
    }
}

/// Aktive Tastenbelegung: je Aktion null oder mehr Chords.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeyBindings {
    bindings: BTreeMap<KeyAction, Vec<KeyChord>>,
}

impl Default for KeyBindings {
    /// Die bisher fest verdrahtete Belegung aus `panes.rs` und `app.rs`.
    fn default() -> Self {
        Self {
            bindings: KeyAction::ALL
                .into_iter()
                .map(|action| (action, action.default_chords()))
                .collect(),
        }
    }
}

impl KeyBindings {
    /// Chords einer Aktion (leer, wenn aufgehoben) — etwa für Hilfetexte.
    #[cfg(test)]
    pub(crate) fn chords(&self, action: KeyAction) -> &[KeyChord] {
        self.bindings
            .get(&action)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// Alle Aktionen in [`KeyAction::ALL`]-Reihenfolge mit ihren Chords in
    /// Anzeigeform (z. B. `f5`, `ctrl+e`); aufgehobene Aktionen haben eine
    /// leere Liste. Grundlage für Hilfe und Tastenübersicht.
    pub(crate) fn describe(&self) -> Vec<(KeyAction, Vec<String>)> {
        KeyAction::ALL
            .into_iter()
            .map(|action| {
                let chords = self
                    .bindings
                    .get(&action)
                    .map(|chords| chords.iter().map(ToString::to_string).collect())
                    .unwrap_or_default();
                (action, chords)
            })
            .collect()
    }

    /// Welche Aktion löst `key` aus?
    ///
    /// Bei mehreren passenden Chords gewinnt der mit den meisten
    /// Modifikatoren (`ctrl+shift+x` vor `ctrl+x`), bei Gleichstand die in
    /// [`KeyAction::ALL`] zuerst genannte Aktion.
    pub(crate) fn action_for(&self, key: &KeyEvent) -> Option<KeyAction> {
        let event = KeyChord::from_event(key);
        let mut best: Option<(u32, KeyAction)> = None;
        for (action, chords) in &self.bindings {
            for chord in chords.iter().filter(|chord| chord.matches(&event)) {
                let score = chord.specificity();
                if best.is_none_or(|(best_score, _)| score > best_score) {
                    best = Some((score, *action));
                }
            }
        }
        best.map(|(_, action)| action)
    }

    /// Legt Überschreibungen aus TOML-Text über die Voreinstellung.
    ///
    /// # Fehler
    /// [`KeyBindingsError::Parse`] bei ungültigem TOML,
    /// [`KeyBindingsError::Invalid`] mit allen gefundenen Problemen.
    pub(crate) fn from_toml_str(text: &str, path: &Path) -> Result<Self, KeyBindingsError> {
        let table = text
            .parse::<toml::Table>()
            .map_err(|error| KeyBindingsError::Parse {
                path: path.to_path_buf(),
                message: error.to_string(),
            })?;

        let mut bindings = Self::default();
        let mut problems = Vec::new();
        let mut named: Vec<KeyAction> = Vec::new();
        for (name, value) in &table {
            let Some(action) = KeyAction::from_name(name) else {
                problems.push(problem(
                    "command_feedback.key_action",
                    "Unknown action '{name}'",
                    &[("name", name)],
                ));
                continue;
            };
            let raw_chords: Vec<&toml::Value> = match value.as_array() {
                Some(items) => items.iter().collect(),
                None => vec![value],
            };
            let mut chords = Vec::with_capacity(raw_chords.len());
            for raw in raw_chords {
                match raw.as_str() {
                    Some(text) => match parse_chord_diagnostic(text) {
                        Ok(chord) => chords.push(chord),
                        Err(mut problem) => {
                            problem.action = Some(name.clone());
                            problems.push(problem);
                        }
                    },
                    None => problems.push(problem(
                        "command_feedback.key_type",
                        "{name}: expected a key chord string or an array of strings, found {type}",
                        &[("name", name), ("type", raw.type_str())],
                    )),
                }
            }
            bindings.bindings.insert(action, chords);
            named.push(action);
        }
        bindings.yield_new_defaults(&named);
        problems.extend(bindings.conflicts());

        if problems.is_empty() {
            Ok(bindings)
        } else {
            Err(KeyBindingsError::Invalid {
                path: path.to_path_buf(),
                problems,
            })
        }
    }

    /// Nachgereichte Voreinstellungen ([`KeyAction::default_yields`]) einer in
    /// der Datei nicht genannten Aktion geben jeden Chord frei, den die Datei
    /// (`named`) einer anderen Aktion zuordnet.
    fn yield_new_defaults(&mut self, named: &[KeyAction]) {
        let explicit: Vec<KeyChord> = named
            .iter()
            .filter_map(|action| self.bindings.get(action))
            .flatten()
            .copied()
            .collect();
        for (action, chords) in &mut self.bindings {
            if action.default_yields() && !named.contains(action) {
                chords.retain(|chord| !explicit.contains(chord));
            }
        }
    }

    /// Chords, die mehr als einer Aktion (oder einer Aktion doppelt)
    /// zugeordnet sind.
    fn conflicts(&self) -> Vec<BindingProblem> {
        let mut seen: Vec<(KeyChord, KeyAction)> = Vec::new();
        let mut problems = Vec::new();
        for (action, chords) in &self.bindings {
            for chord in chords {
                match seen.iter().find(|(other, _)| other == chord) {
                    Some((_, owner)) if owner == action => {
                        problems.push(problem(
                            "command_feedback.key_twice",
                            "{action}: key chord '{chord}' listed twice",
                            &[
                                ("action", &action.to_string()),
                                ("chord", &chord.to_string()),
                            ],
                        ));
                    }
                    Some((_, owner)) => problems.push(problem(
                        "command_feedback.key_conflict",
                        "Key chord '{chord}' is bound to both '{owner}' and '{action}'",
                        &[
                            ("chord", &chord.to_string()),
                            ("owner", &owner.to_string()),
                            ("action", &action.to_string()),
                        ],
                    )),
                    None => seen.push((*chord, *action)),
                }
            }
        }
        problems
    }
}

/// Lädt die Keybindings-Datei unter `path` und legt sie über die
/// Voreinstellung.
///
/// Eine fehlende Datei ist kein Fehler: `[tui].keybindings_file` hat den
/// Standardwert `keybindings.toml`, den die meisten Profile nicht anlegen.
/// Dann gilt [`KeyBindings::default`].
///
/// # Fehler
/// - [`KeyBindingsError::Read`]: Datei vorhanden, aber nicht lesbar.
/// - [`KeyBindingsError::Parse`]: kein gültiges TOML.
/// - [`KeyBindingsError::Invalid`]: unbekannte Aktion, ungültiger Chord,
///   falscher Werttyp oder Doppelbelegung (alle Probleme gesammelt).
pub(crate) fn load(path: &Path) -> Result<KeyBindings, KeyBindingsError> {
    match std::fs::read_to_string(path) {
        Ok(text) => KeyBindings::from_toml_str(&text, path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(KeyBindings::default()),
        Err(source) => Err(KeyBindingsError::Read {
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn event(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn chord(raw: &str) -> Result<KeyChord, Box<dyn std::error::Error>> {
        Ok(parse_chord(raw)?)
    }

    fn invalid_problems(result: Result<KeyBindings, KeyBindingsError>) -> Vec<String> {
        match result {
            Err(KeyBindingsError::Invalid { problems, .. }) => {
                problems.iter().map(ToString::to_string).collect()
            }
            other => vec![format!("unexpected result: {other:?}")],
        }
    }

    #[test]
    fn parses_modifiers_function_keys_names_and_chars() -> TestResult {
        assert_eq!(
            chord("ctrl+shift+x")?,
            KeyChord {
                code: KeyCode::Char('x'),
                modifiers: KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            }
        );
        assert_eq!(chord("F5")?.code, KeyCode::F(5));
        assert_eq!(chord("f24")?.code, KeyCode::F(24));
        assert_eq!(chord("Esc")?.code, KeyCode::Esc);
        assert_eq!(chord("enter")?.code, KeyCode::Enter);
        assert_eq!(chord("tab")?.code, KeyCode::Tab);
        assert_eq!(
            chord("q")?,
            KeyChord {
                code: KeyCode::Char('q'),
                modifiers: KeyModifiers::NONE
            }
        );
        assert_eq!(chord("alt+space")?.code, KeyCode::Char(' '));
        assert_eq!(
            chord("ctrl++")?,
            KeyChord {
                code: KeyCode::Char('+'),
                modifiers: KeyModifiers::CONTROL
            }
        );
        assert_eq!(chord("CTRL+E")?, chord("ctrl+e")?);
        Ok(())
    }

    #[test]
    fn shift_tab_normalizes_to_backtab() -> TestResult {
        assert_eq!(chord("shift+tab")?, chord("backtab")?);
        assert_eq!(chord("shift+tab")?.modifiers, KeyModifiers::NONE);
        Ok(())
    }

    #[test]
    fn rejects_malformed_chords() {
        for raw in [
            "",
            "ctrl+",
            "hyper+x",
            "ctrl+ctrl+x",
            "f0",
            "f25",
            "fx1",
            "abc",
            "ctrl++x",
        ] {
            assert!(parse_chord(raw).is_err(), "'{raw}' should be rejected");
        }
    }

    #[test]
    fn display_round_trips() -> TestResult {
        for raw in [
            "ctrl+shift+x",
            "f11",
            "shift+tab",
            "alt+space",
            "ctrl+plus",
            "esc",
        ] {
            let parsed = chord(raw)?;
            assert_eq!(chord(&parsed.to_string())?, parsed, "{raw}");
        }
        Ok(())
    }

    #[test]
    fn defaults_match_hardcoded_keys() {
        let bindings = KeyBindings::default();
        let cases = [
            (
                event(KeyCode::F(2), KeyModifiers::NONE),
                KeyAction::ToggleExplorer,
            ),
            (
                event(KeyCode::F(3), KeyModifiers::NONE),
                KeyAction::ToggleAgents,
            ),
            (
                event(KeyCode::F(4), KeyModifiers::NONE),
                KeyAction::CycleFocus,
            ),
            (
                event(KeyCode::F(11), KeyModifiers::NONE),
                KeyAction::MaximizePanel,
            ),
            (
                event(KeyCode::Char('e'), KeyModifiers::CONTROL),
                KeyAction::FocusExplorer,
            ),
            (
                event(KeyCode::Char('E'), KeyModifiers::CONTROL),
                KeyAction::FocusExplorer,
            ),
            (
                event(KeyCode::Char('o'), KeyModifiers::CONTROL),
                KeyAction::ToggleToolCells,
            ),
            (
                event(KeyCode::Char('h'), KeyModifiers::CONTROL),
                KeyAction::EndHostMode,
            ),
            (
                event(KeyCode::Char('k'), KeyModifiers::CONTROL),
                KeyAction::DeleteLine,
            ),
            (
                event(KeyCode::Char('j'), KeyModifiers::CONTROL),
                KeyAction::InsertNewline,
            ),
            (
                event(KeyCode::BackTab, KeyModifiers::SHIFT),
                KeyAction::CyclePermissionMode,
            ),
            (
                event(KeyCode::Tab, KeyModifiers::SHIFT),
                KeyAction::CyclePermissionMode,
            ),
            (
                event(KeyCode::F(5), KeyModifiers::NONE),
                KeyAction::ToggleWorkbench,
            ),
            (
                event(KeyCode::F(6), KeyModifiers::NONE),
                KeyAction::OpenKanban,
            ),
            (
                event(KeyCode::F(7), KeyModifiers::NONE),
                KeyAction::OpenModePicker,
            ),
            (
                event(KeyCode::F(8), KeyModifiers::NONE),
                KeyAction::OpenModels,
            ),
            (
                event(KeyCode::F(1), KeyModifiers::NONE),
                KeyAction::ShowHelp,
            ),
            (
                event(KeyCode::F(9), KeyModifiers::NONE),
                KeyAction::OpenMatrix,
            ),
            (
                event(KeyCode::Up, KeyModifiers::CONTROL),
                KeyAction::ScrollPanelUp,
            ),
            (
                event(KeyCode::Down, KeyModifiers::CONTROL),
                KeyAction::ScrollPanelDown,
            ),
            (
                event(KeyCode::Char('j'), KeyModifiers::ALT),
                KeyAction::FocusJobs,
            ),
            (
                event(KeyCode::Char('f'), KeyModifiers::ALT),
                KeyAction::FocusFinished,
            ),
            (
                event(KeyCode::Char('l'), KeyModifiers::ALT),
                KeyAction::ToggleLiveDetail,
            ),
        ];
        for (key, expected) in cases {
            assert_eq!(bindings.action_for(&key), Some(expected), "{key:?}");
        }
        assert_eq!(
            bindings.action_for(&event(KeyCode::Char('e'), KeyModifiers::NONE)),
            None
        );
        assert_eq!(
            bindings.action_for(&event(KeyCode::Tab, KeyModifiers::NONE)),
            None
        );
        assert_eq!(
            bindings.action_for(&event(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            None
        );
        // Plain ↑/↓ und w/s gehören weiter Auswahl bzw. Composer.
        for code in [
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Char('w'),
            KeyCode::Char('s'),
        ] {
            assert_eq!(
                bindings.action_for(&event(code, KeyModifiers::NONE)),
                None,
                "{code:?}"
            );
        }
    }

    #[test]
    fn scroll_panel_actions_are_rebindable() -> TestResult {
        // `alt+j` ist ab PL-97 C2 die Voreinstellung von `focus_jobs`; eine
        // ältere Datei, die es anders belegt, bleibt gültig (die Datei
        // gewinnt, siehe `old_files_using_the_new_default_chords_still_load`).
        let bindings = KeyBindings::from_toml_str(
            "scroll_panel_up = \"alt+k\"\nscroll_panel_down = [\"alt+j\", \"ctrl+down\"]\n",
            Path::new("kb.toml"),
        )?;
        assert_eq!(
            bindings.action_for(&event(KeyCode::Char('k'), KeyModifiers::ALT)),
            Some(KeyAction::ScrollPanelUp)
        );
        assert_eq!(
            bindings.action_for(&event(KeyCode::Up, KeyModifiers::CONTROL)),
            None,
            "die Voreinstellung ist ersetzt"
        );
        assert_eq!(
            bindings.action_for(&event(KeyCode::Char('j'), KeyModifiers::ALT)),
            Some(KeyAction::ScrollPanelDown)
        );
        assert_eq!(
            bindings.action_for(&event(KeyCode::Down, KeyModifiers::CONTROL)),
            Some(KeyAction::ScrollPanelDown)
        );
        let described = bindings.describe();
        assert!(described.iter().any(|(action, chords)| {
            *action == KeyAction::ScrollPanelUp && chords == &vec!["alt+k".to_owned()]
        }));
        Ok(())
    }

    /// PL-97 C2: `Ctrl+J` bleibt der Zeilenumbruch, `Alt+J` fokussiert die
    /// Jobs; die drei neuen Aktionen sind umbelegbar und kollidieren nicht
    /// mit der Voreinstellung.
    #[test]
    fn mission_actions_are_rebindable_and_ctrl_j_stays_newline() -> TestResult {
        let defaults = KeyBindings::default();
        assert_eq!(
            defaults.action_for(&event(KeyCode::Char('j'), KeyModifiers::CONTROL)),
            Some(KeyAction::InsertNewline)
        );
        assert_eq!(
            defaults.action_for(&event(KeyCode::Char('j'), KeyModifiers::ALT)),
            Some(KeyAction::FocusJobs)
        );
        assert_eq!(
            defaults.action_for(&event(KeyCode::Char('j'), KeyModifiers::NONE)),
            None,
            "plain j stays a letter / list key"
        );
        let bindings = KeyBindings::from_toml_str(
            "focus_jobs = \"F13\"\nfocus_finished = [\"F14\", \"alt+x\"]\ntoggle_live_detail = []\n",
            Path::new("kb.toml"),
        )?;
        assert_eq!(
            bindings.action_for(&event(KeyCode::F(13), KeyModifiers::NONE)),
            Some(KeyAction::FocusJobs)
        );
        assert_eq!(
            bindings.action_for(&event(KeyCode::Char('x'), KeyModifiers::ALT)),
            Some(KeyAction::FocusFinished)
        );
        assert_eq!(
            bindings.action_for(&event(KeyCode::Char('j'), KeyModifiers::ALT)),
            None
        );
        assert!(bindings.chords(KeyAction::ToggleLiveDetail).is_empty());
        for (name, action) in [
            ("focus_jobs", KeyAction::FocusJobs),
            ("focus_finished", KeyAction::FocusFinished),
            ("toggle_live_detail", KeyAction::ToggleLiveDetail),
        ] {
            assert_eq!(KeyAction::from_name(name), Some(action));
        }
        // Zwei ausdrücklich gesetzte Chords kollidieren weiterhin.
        let problems = invalid_problems(KeyBindings::from_toml_str(
            "cycle_focus = \"alt+l\"\ntoggle_live_detail = \"alt+l\"\n",
            Path::new("kb.toml"),
        ));
        assert_eq!(problems.len(), 1, "alt+l is set twice: {problems:?}");
        Ok(())
    }

    /// PL-97 C2 regression: a keybindings file that was valid before the
    /// Mission Control defaults existed still loads. The file wins over the
    /// new default of an action it does not mention (that action only loses
    /// the chord); older defaults keep conflicting as before.
    #[test]
    fn old_files_using_the_new_default_chords_still_load() -> TestResult {
        let bindings = KeyBindings::from_toml_str(
            "scroll_panel_down = [\"alt+j\", \"ctrl+down\"]\ncycle_focus = \"alt+l\"\nshow_help = \"alt+f\"\n",
            Path::new("kb.toml"),
        )?;
        assert_eq!(
            bindings.action_for(&event(KeyCode::Char('j'), KeyModifiers::ALT)),
            Some(KeyAction::ScrollPanelDown)
        );
        assert_eq!(
            bindings.action_for(&event(KeyCode::Char('l'), KeyModifiers::ALT)),
            Some(KeyAction::CycleFocus)
        );
        assert_eq!(
            bindings.action_for(&event(KeyCode::Char('f'), KeyModifiers::ALT)),
            Some(KeyAction::ShowHelp)
        );
        assert!(bindings.chords(KeyAction::FocusJobs).is_empty());
        assert!(bindings.chords(KeyAction::FocusFinished).is_empty());
        assert!(bindings.chords(KeyAction::ToggleLiveDetail).is_empty());
        // Unberührte neue Aktionen behalten ihre Voreinstellung.
        let only_one =
            KeyBindings::from_toml_str("scroll_panel_down = \"alt+j\"\n", Path::new("kb.toml"))?;
        assert!(only_one.chords(KeyAction::FocusJobs).is_empty());
        assert_eq!(
            only_one.action_for(&event(KeyCode::Char('f'), KeyModifiers::ALT)),
            Some(KeyAction::FocusFinished)
        );
        // Ältere Voreinstellungen geben nicht nach (unverändert seit vor C2).
        let problems = invalid_problems(KeyBindings::from_toml_str(
            r#"show_help = "F2""#,
            Path::new("kb.toml"),
        ));
        assert_eq!(problems.len(), 1, "{problems:?}");
        Ok(())
    }

    #[test]
    fn action_names_round_trip() {
        for action in KeyAction::ALL {
            assert_eq!(KeyAction::from_name(action.name()), Some(action));
        }
        assert_eq!(KeyAction::from_name("nope"), None);
        for (name, action) in [
            ("toggle_workbench", KeyAction::ToggleWorkbench),
            ("open_kanban", KeyAction::OpenKanban),
            ("open_mode_picker", KeyAction::OpenModePicker),
            ("open_models", KeyAction::OpenModels),
            ("show_help", KeyAction::ShowHelp),
            ("open_matrix", KeyAction::OpenMatrix),
        ] {
            assert_eq!(KeyAction::from_name(name), Some(action));
        }
    }

    #[test]
    fn every_action_has_a_localized_label_and_a_default() {
        for action in KeyAction::ALL {
            assert!(!action.label().is_empty(), "{action}");
            assert_eq!(action.default_chords().len(), 1, "{action}");
        }
        assert!(
            KeyBindings::default().conflicts().is_empty(),
            "default bindings must not conflict"
        );
    }

    #[test]
    fn describe_lists_all_actions_in_order() -> TestResult {
        let bindings = KeyBindings::from_toml_str(
            "show_help = []\nopen_models = [\"F8\", \"ctrl+m\"]\n",
            Path::new("kb.toml"),
        )?;
        let described = bindings.describe();
        assert_eq!(described.len(), KeyAction::ALL.len());
        for ((action, _), expected) in described.iter().zip(KeyAction::ALL) {
            assert_eq!(*action, expected);
        }
        let lookup = |wanted: KeyAction| {
            described
                .iter()
                .find(|(action, _)| *action == wanted)
                .map(|(_, chords)| chords.clone())
                .unwrap_or_default()
        };
        assert_eq!(lookup(KeyAction::ToggleWorkbench), vec!["f5".to_owned()]);
        assert_eq!(
            lookup(KeyAction::OpenModels),
            vec!["f8".to_owned(), "ctrl+m".to_owned()]
        );
        assert!(lookup(KeyAction::ShowHelp).is_empty());
        Ok(())
    }

    #[test]
    fn keybindings_file_accepts_new_action_names() -> TestResult {
        let text = r#"
toggle_workbench = "ctrl+w"
open_kanban = "F20"
open_mode_picker = "alt+m"
open_models = "F10"
show_help = "F12"
"#;
        let bindings = KeyBindings::from_toml_str(text, Path::new("kb.toml"))?;
        for (key, action) in [
            (
                event(KeyCode::Char('w'), KeyModifiers::CONTROL),
                KeyAction::ToggleWorkbench,
            ),
            (
                event(KeyCode::F(20), KeyModifiers::NONE),
                KeyAction::OpenKanban,
            ),
            (
                event(KeyCode::Char('m'), KeyModifiers::ALT),
                KeyAction::OpenModePicker,
            ),
            (
                event(KeyCode::F(10), KeyModifiers::NONE),
                KeyAction::OpenModels,
            ),
            (
                event(KeyCode::F(12), KeyModifiers::NONE),
                KeyAction::ShowHelp,
            ),
        ] {
            assert_eq!(bindings.action_for(&key), Some(action), "{key:?}");
        }
        assert_eq!(
            bindings.action_for(&event(KeyCode::F(5), KeyModifiers::NONE)),
            None
        );
        let problems = invalid_problems(KeyBindings::from_toml_str(
            r#"show_help = "F2""#,
            Path::new("kb.toml"),
        ));
        assert_eq!(problems.len(), 1, "{problems:?}");
        Ok(())
    }

    #[test]
    fn overrides_replace_only_named_actions() -> TestResult {
        let text = r#"
toggle_explorer = "F20"
toggle_agents = ["F10", "ctrl+g"]
end_host_mode = []
"#;
        let bindings = KeyBindings::from_toml_str(text, Path::new("kb.toml"))?;
        assert_eq!(
            bindings.action_for(&event(KeyCode::F(2), KeyModifiers::NONE)),
            None
        );
        assert_eq!(
            bindings.action_for(&event(KeyCode::F(20), KeyModifiers::NONE)),
            Some(KeyAction::ToggleExplorer)
        );
        assert_eq!(
            bindings.action_for(&event(KeyCode::Char('g'), KeyModifiers::CONTROL)),
            Some(KeyAction::ToggleAgents)
        );
        assert_eq!(bindings.chords(KeyAction::ToggleAgents).len(), 2);
        assert!(bindings.chords(KeyAction::EndHostMode).is_empty());
        assert_eq!(
            bindings.action_for(&event(KeyCode::Char('h'), KeyModifiers::CONTROL)),
            None
        );
        assert_eq!(
            bindings.action_for(&event(KeyCode::F(4), KeyModifiers::NONE)),
            Some(KeyAction::CycleFocus),
            "untouched actions keep their defaults"
        );
        Ok(())
    }

    #[test]
    fn most_specific_chord_wins() -> TestResult {
        let text = r#"
toggle_tool_cells = "ctrl+x"
delete_line = "ctrl+shift+x"
"#;
        let bindings = KeyBindings::from_toml_str(text, Path::new("kb.toml"))?;
        assert_eq!(
            bindings.action_for(&event(KeyCode::Char('X'), KeyModifiers::CONTROL)),
            Some(KeyAction::DeleteLine)
        );
        assert_eq!(
            bindings.action_for(&event(KeyCode::Char('x'), KeyModifiers::CONTROL)),
            Some(KeyAction::ToggleToolCells)
        );
        Ok(())
    }

    #[test]
    fn plain_char_chord_ignores_ctrl_events() -> TestResult {
        let bindings = KeyBindings::from_toml_str(r#"cycle_focus = "q""#, Path::new("kb.toml"))?;
        assert_eq!(
            bindings.action_for(&event(KeyCode::Char('q'), KeyModifiers::NONE)),
            Some(KeyAction::CycleFocus)
        );
        assert_eq!(
            bindings.action_for(&event(KeyCode::Char('q'), KeyModifiers::CONTROL)),
            None
        );
        Ok(())
    }

    #[test]
    fn collects_all_problems() {
        let text = r#"
toggle_explorer = "hyper+q"
launch_rockets = "F12"
cycle_focus = 5
"#;
        let problems = invalid_problems(KeyBindings::from_toml_str(text, Path::new("kb.toml")));
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert!(problems.iter().any(|p| p.contains("launch_rockets")));
        assert!(problems.iter().any(|p| p.contains("hyper")));
        assert!(
            problems
                .iter()
                .any(|p| p.contains("cycle_focus") && p.contains("integer"))
        );
    }

    #[test]
    fn rejects_conflicting_chords() {
        let problems = invalid_problems(KeyBindings::from_toml_str(
            r#"toggle_agents = "F2""#,
            Path::new("kb.toml"),
        ));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("toggle_explorer") && problems[0].contains("toggle_agents"));
    }

    #[test]
    fn rejects_invalid_toml() {
        let result = KeyBindings::from_toml_str("toggle_explorer = ", Path::new("kb.toml"));
        assert!(matches!(result, Err(KeyBindingsError::Parse { .. })));
    }

    #[test]
    fn load_reads_file_and_falls_back_when_missing() -> TestResult {
        let dir = tempfile::tempdir()?;
        let missing = dir.path().join("keybindings.toml");
        assert_eq!(load(&missing)?, KeyBindings::default());

        std::fs::write(&missing, "maximize_panel = \"ctrl+m\"\n")?;
        let bindings = load(&missing)?;
        assert_eq!(
            bindings.action_for(&event(KeyCode::Char('m'), KeyModifiers::CONTROL)),
            Some(KeyAction::MaximizePanel)
        );

        std::fs::write(&missing, "bogus = \"F1\"\n")?;
        let error = load(&missing).err().ok_or("expected an error")?;
        assert!(
            error.to_string().contains("Unknown action 'bogus'"),
            "{error}"
        );
        Ok(())
    }
    #[test]
    fn diagnostics_render_in_display_locale_without_parsing_cached_text() -> TestResult {
        let error = KeyBindings::from_toml_str(
            "toggle_explorer = \"hyper+q\"",
            Path::new("/user/keys.toml"),
        )
        .err()
        .ok_or("expected diagnostic")?;
        let german = error.present_for(harw_config::UiLanguage::German);
        let japanese = error.present_for(harw_config::UiLanguage::Japanese);
        assert!(
            german.contains("Unbekannter Modifikator 'hyper'"),
            "{german}"
        );
        assert!(japanese.contains("不明な修飾キー 'hyper'"), "{japanese}");
        for text in [german, japanese] {
            assert!(text.contains("toggle_explorer"));
            assert!(text.contains("/user/keys.toml"));
            assert!(text.contains("hyper+q"));
        }
        assert!(error.to_string().contains("Unknown modifier 'hyper'"));
        Ok(())
    }
}
