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
//! end_host_mode = []
//! ```
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
//! Nicht umbelegbar sind bewusst: `Ctrl+C`/`Ctrl+D` (Notausstieg), `Esc`
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
/// Panel-Aktionen ([`KeyAction::is_panel_action`]) wertet
/// `PanelState::handle_key` aus, die übrigen der Chat-/Composer-Zweig in
/// `app.rs`.
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
}

impl KeyAction {
    /// Alle Aktionen in fester Reihenfolge (auch Vorrang bei Gleichstand).
    pub(crate) const ALL: [Self; 10] = [
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
        }
    }

    /// Umkehrung von [`KeyAction::name`]; `None` für unbekannte Namen.
    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|action| action.name() == name)
    }

    /// `true` für Aktionen, die `PanelState::handle_key` behandelt.
    pub(crate) const fn is_panel_action(self) -> bool {
        matches!(
            self,
            Self::ToggleExplorer
                | Self::ToggleAgents
                | Self::CycleFocus
                | Self::MaximizePanel
                | Self::FocusExplorer
        )
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
        };
        vec![KeyChord { code, modifiers }]
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
    let lowered = raw.trim().to_lowercase();
    if lowered.is_empty() {
        return Err("empty key chord".to_owned());
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
        return Err(format!("key chord '{raw}' has no key after '+'"));
    }

    let mut modifiers = KeyModifiers::NONE;
    if !modifier_part.is_empty() {
        for token in modifier_part.split('+') {
            let flag = match token.trim() {
                "ctrl" | "control" => KeyModifiers::CONTROL,
                "alt" | "option" | "meta" => KeyModifiers::ALT,
                "shift" => KeyModifiers::SHIFT,
                "super" | "cmd" | "win" => KeyModifiers::SUPER,
                "" => return Err(format!("key chord '{raw}' contains an empty modifier")),
                other => return Err(format!("unknown modifier '{other}' in key chord '{raw}'")),
            };
            if modifiers.contains(flag) {
                return Err(format!("duplicate modifier '{token}' in key chord '{raw}'"));
            }
            modifiers.insert(flag);
        }
    }

    let code = parse_key_name(key_part.trim())
        .ok_or_else(|| format!("unknown key '{key_part}' in key chord '{raw}'"))?;
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
        problems: Vec<String>,
    },
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
                problems.join("; ")
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
    pub(crate) fn chords(&self, action: KeyAction) -> &[KeyChord] {
        self.bindings
            .get(&action)
            .map(Vec::as_slice)
            .unwrap_or_default()
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
        for (name, value) in &table {
            let Some(action) = KeyAction::from_name(name) else {
                problems.push(format!("unknown action '{name}'"));
                continue;
            };
            let raw_chords: Vec<&toml::Value> = match value.as_array() {
                Some(items) => items.iter().collect(),
                None => vec![value],
            };
            let mut chords = Vec::with_capacity(raw_chords.len());
            for raw in raw_chords {
                match raw.as_str() {
                    Some(text) => match parse_chord(text) {
                        Ok(chord) => chords.push(chord),
                        Err(problem) => problems.push(format!("{name}: {problem}")),
                    },
                    None => problems.push(format!(
                        "{name}: expected a key chord string or an array of strings, found {}",
                        raw.type_str()
                    )),
                }
            }
            bindings.bindings.insert(action, chords);
        }
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

    /// Chords, die mehr als einer Aktion (oder einer Aktion doppelt)
    /// zugeordnet sind.
    fn conflicts(&self) -> Vec<String> {
        let mut seen: Vec<(KeyChord, KeyAction)> = Vec::new();
        let mut problems = Vec::new();
        for (action, chords) in &self.bindings {
            for chord in chords {
                match seen.iter().find(|(other, _)| other == chord) {
                    Some((_, owner)) if owner == action => {
                        problems.push(format!("{action}: key chord '{chord}' listed twice"));
                    }
                    Some((_, owner)) => problems.push(format!(
                        "key chord '{chord}' is bound to both '{owner}' and '{action}'"
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
            Err(KeyBindingsError::Invalid { problems, .. }) => problems,
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
    }

    #[test]
    fn action_names_round_trip() {
        for action in KeyAction::ALL {
            assert_eq!(KeyAction::from_name(action.name()), Some(action));
        }
        assert_eq!(KeyAction::from_name("nope"), None);
    }

    #[test]
    fn overrides_replace_only_named_actions() -> TestResult {
        let text = r#"
toggle_explorer = "F6"
toggle_agents = ["F7", "ctrl+g"]
end_host_mode = []
"#;
        let bindings = KeyBindings::from_toml_str(text, Path::new("kb.toml"))?;
        assert_eq!(
            bindings.action_for(&event(KeyCode::F(2), KeyModifiers::NONE)),
            None
        );
        assert_eq!(
            bindings.action_for(&event(KeyCode::F(6), KeyModifiers::NONE)),
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
launch_rockets = "F9"
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
            error.to_string().contains("unknown action 'bogus'"),
            "{error}"
        );
        Ok(())
    }
}
