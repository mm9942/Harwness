//! Interaktionsmodus einer Session — durchgesetzte Autoritätsgrenze, kein
//! Prompt-Hinweis.
//!
//! # Verantwortung
//! Dieses Modul besitzt die Abbildung `Modus → (Tool-Profil, Tool-Namen,
//! Permission-Obergrenze, Prompt-Abschnitt)`. Es besitzt **nicht** die
//! Anwendung dieser Abbildung: das Einsetzen in Activation und Sandbox leistet
//! [`crate::session::AgentSession::set_mode`].
//!
//! # Schlüsseltypen
//! - [`InteractionMode`] — `Chat` | `Plan` | `Explore` | `Work`
//!
//! # Autoritätsmodell
//! Die Permission-Obergrenze eines Modus ist **monoton**: sie wird stets mit
//! der Basis-Sandbox der Session geschnitten
//! ([`harw_sandbox::SandboxSpec::restrict`],
//! [`crate::session::AgentSession::set_mode`]), nie mit dem gerade aktuellen,
//! bereits verengten Wert — der Schnitt ist deshalb nie kumulativ. Ein Wechsel
//! von `Explore` zurück nach `Work` stellt entzogene Permissions daher bis zur
//! Basis **wieder her**; er kann nie mehr freigeben, als die Basis je hatte.
//! Das ist beabsichtigt und der Grund, warum der Modus als Grenze taugt.
//!
//! # Nebenläufigkeit
//! [`InteractionMode`] ist ein `Copy`-Enum ohne innere Veränderlichkeit:
//! `Send + Sync`, beliebig teilbar, keine Sperren. Alle Methoden sind reine
//! Berechnungen; [`InteractionMode::permission_ceiling`] alloziert eine neue
//! [`PermissionSet`] pro Aufruf.
//!
//! # Fehler
//! Dieses Modul erzeugt keine Fehler. Die einzige fehlbare Operation ist
//! [`InteractionMode::parse`]; sie meldet einen unbekannten Namen als `None`.
//!
//! # Beispiele
//! ```rust
//! use harw_core::mode::InteractionMode;
//! use harw_sandbox::Permission;
//!
//! let mode = InteractionMode::parse("Explore").expect("bekannter Modus");
//! assert_eq!(mode, InteractionMode::Explore);
//! assert!(mode.permission_ceiling().contains(Permission::ReadWorkspace));
//! assert!(!mode.permission_ceiling().contains(Permission::WriteWorkspace));
//! ```

use harw_sandbox::{Permission, PermissionSet};
use serde::{Deserialize, Serialize};

use crate::activation::ToolProfile;

/// Rein lesende Werkzeuge — die Basis von [`InteractionMode::Explore`].
///
/// Jeder Eintrag ist ein Werkzeugname, der den Workspace bzw. die bereits
/// entpackten Abhängigkeitsquellen ausschließlich liest. Kein Eintrag darf
/// schreiben, Prozesse starten oder ins Netz gehen.
const EXPLORE_TOOLS: &[&str] = &[
    "fs.read",
    "fs.list",
    "fs.search",
    "fs.glob",
    "fs.grep",
    "deps.graph",
    "deps.locked",
    "deps.source_read",
    "deps.source_search",
    "deps.source_list",
    "status",
    "ps",
    "diff",
];

/// Werkzeuge von [`InteractionMode::Plan`].
///
/// Der vordere Teil ist wortgleich [`EXPLORE_TOOLS`] (durch
/// `plan_tools_extend_explore_tools` abgesichert), der hintere Teil ergänzt
/// Planung, Ziele und Recherche. Auch hier mutiert kein Eintrag den Workspace:
/// `plan`/`goal` schreiben in den Plan-Graph, nicht auf die Platte.
const PLAN_TOOLS: &[&str] = &[
    // — identisch zu EXPLORE_TOOLS —
    "fs.read",
    "fs.list",
    "fs.search",
    "fs.glob",
    "fs.grep",
    "deps.graph",
    "deps.locked",
    "deps.source_read",
    "deps.source_search",
    "deps.source_list",
    "status",
    "ps",
    "diff",
    // — Planungs- und Rechercheerweiterung —
    "plan",
    "goal",
    "web.fetch",
    "web.docs_rs",
    "web.crates_io",
    "explore",
    "research_deps",
    "research_web",
];

/// Prompt-Abschnitt für [`InteractionMode::Chat`].
const CHAT_PROMPT: &str = "Modus: chat. Freies Gespräch mit dem vollen \
Werkzeugsatz. Der Modus schränkt weder Werkzeuge noch Sandbox-Permissions ein; \
jede Mutation unterliegt weiterhin den normalen Guardrails.";

/// Prompt-Abschnitt für [`InteractionMode::Plan`].
const PLAN_PROMPT: &str = "Modus: plan. Du planst und recherchierst. Verfügbar \
sind lesende Werkzeuge, Web-Recherche sowie die Plan- und Ziel-Werkzeuge. \
Schreiben, Shell und jede andere Mutation sind in diesem Modus nicht nur \
unerwünscht, sondern abgeschaltet — schlage Änderungen vor, führe sie nicht \
aus.";

/// Prompt-Abschnitt für [`InteractionMode::Explore`].
const EXPLORE_PROMPT: &str = "Modus: explore. Du liest ausschließlich: \
Workspace-Dateien und die bereits entpackten Abhängigkeitsquellen. Es gibt \
weder Schreib- noch Shell- noch Netzzugriff. Antworte mit Befunden und \
Fundstellen, nicht mit Änderungen.";

/// Prompt-Abschnitt für [`InteractionMode::Work`].
const WORK_PROMPT: &str = "Modus: work. Voller Werkzeugsatz inklusive Schreiben \
und Shell. Der Modus hebt keine Sandbox-Grenze auf: es gilt weiterhin genau \
die Autorität, die die Session beim Start bekommen hat.";

/// Betriebsmodus einer Session. Er bestimmt, welche Werkzeuge das Modell sieht
/// und welche Autorität die Sandbox höchstens tragen darf.
///
/// Der Modus ist **kein** Prompt-Hinweis, sondern eine durchgesetzte Grenze:
/// `Explore` reduziert sowohl die Tool-Aktivierung als auch das
/// Permission-Ceiling.
///
/// # Beschreibung
/// Jeder Modus liefert vier voneinander unabhängige Angaben:
/// [`Self::tool_profile`] (grobes Profil), [`Self::allowed_tools`] (feine
/// Namensliste), [`Self::permission_ceiling`] (Sandbox-Obergrenze) und
/// [`Self::prompt_section`] (Erklärung für das Modell). Die ersten drei sind
/// erzwungen, die vierte ist reine Kommunikation.
///
/// # Nebenläufigkeit
/// `Copy`, ohne innere Veränderlichkeit — beliebig zwischen Threads teilbar.
///
/// # Beispiele
/// ```rust
/// use harw_core::mode::InteractionMode;
///
/// assert_eq!(InteractionMode::default(), InteractionMode::Chat);
/// assert!(InteractionMode::Chat.allowed_tools().is_none());
/// assert!(InteractionMode::Explore.allowed_tools().is_some());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionMode {
    /// Gespräch: keine namensbasierte Einschränkung, kein Ceiling.
    #[default]
    Chat,
    /// Planung: Plan-/Goal-Werkzeuge und Recherche, keine Mutation.
    Plan,
    /// Exploration: ausschließlich lesende Werkzeuge.
    Explore,
    /// Ausführung: voller Werkzeugsatz inkl. Schreiben und Shell.
    Work,
}

impl InteractionMode {
    /// Liefert das Tool-Profil für die [`SessionActivation`][crate::activation::SessionActivation].
    ///
    /// # Beschreibung
    /// `Explore` und `Plan` starten von [`ToolProfile::Minimal`], also
    /// deny-by-default: erst die Namen aus [`Self::allowed_tools`] machen ein
    /// Werkzeug sichtbar. `Chat` und `Work` verwenden [`ToolProfile::Full`] und
    /// filtern nicht.
    ///
    /// # Returns
    /// Das [`ToolProfile`], mit dem eine frische `SessionActivation` gebaut
    /// wird.
    ///
    /// # Nebenläufigkeit
    /// Reine Berechnung, aus jedem Thread aufrufbar.
    ///
    /// # Beispiele
    /// ```rust
    /// use harw_core::activation::ToolProfile;
    /// use harw_core::mode::InteractionMode;
    ///
    /// assert_eq!(InteractionMode::Explore.tool_profile(), ToolProfile::Minimal);
    /// assert_eq!(InteractionMode::Work.tool_profile(), ToolProfile::Full);
    /// ```
    #[must_use]
    pub fn tool_profile(&self) -> ToolProfile {
        match self {
            Self::Plan | Self::Explore => ToolProfile::Minimal,
            Self::Chat | Self::Work => ToolProfile::Full,
        }
    }

    /// Liefert die Obergrenze der Sandbox-Permissions in diesem Modus.
    ///
    /// # Beschreibung
    /// Die Obergrenze wird **geschnitten**, nie addiert: der Aufrufer reicht
    /// sie an [`harw_sandbox::SandboxSpec::restrict`] weiter. `Chat` und `Work`
    /// liefern die vollständige Permission-Menge, wodurch der Schnitt zur
    /// Identität wird — sie sind damit ausdrücklich *kein* Ceiling und können
    /// nichts wiederherstellen, was ein früherer Modus entzogen hat.
    ///
    /// # Returns
    /// Eine frisch allozierte [`PermissionSet`] mit den Permissions, die dieser
    /// Modus höchstens zulässt.
    ///
    /// # Nebenläufigkeit
    /// Reine Berechnung; alloziert bei jedem Aufruf ein neues Set.
    ///
    /// # Beispiele
    /// ```rust
    /// use harw_core::mode::InteractionMode;
    /// use harw_sandbox::Permission;
    ///
    /// let ceiling = InteractionMode::Plan.permission_ceiling();
    /// assert!(ceiling.contains(Permission::NetworkAccess));
    /// assert!(!ceiling.contains(Permission::ExecuteProcess));
    /// ```
    #[must_use]
    pub fn permission_ceiling(&self) -> PermissionSet {
        match self {
            Self::Explore => PermissionSet::from_policy([
                Permission::ReadWorkspace,
                Permission::ReadCargoRegistry,
            ]),
            Self::Plan => PermissionSet::from_policy([
                Permission::ReadWorkspace,
                Permission::ReadCargoRegistry,
                Permission::NetworkAccess,
            ]),
            Self::Chat | Self::Work => all_permissions(),
        }
    }

    /// Liefert die Namen der Werkzeuge, die in diesem Modus aktiv sein dürfen.
    ///
    /// # Beschreibung
    /// `None` bedeutet „keine namensbasierte Einschränkung" und gilt für `Chat`
    /// und `Work`. `Some(list)` ist eine Positivliste, die zusätzlich zum
    /// (minimalen) Profil aus [`Self::tool_profile`] freigeschaltet wird.
    ///
    /// # Returns
    /// `Some(&'static [&'static str])` für `Plan`/`Explore`, `None` sonst.
    ///
    /// # Nebenläufigkeit
    /// Reine Berechnung ohne Allokation; die Listen sind `'static`.
    #[must_use]
    pub fn allowed_tools(&self) -> Option<&'static [&'static str]> {
        match self {
            Self::Explore => Some(EXPLORE_TOOLS),
            Self::Plan => Some(PLAN_TOOLS),
            Self::Chat | Self::Work => None,
        }
    }

    /// Liefert den Prompt-Abschnitt, der dem Modell den Modus erklärt.
    ///
    /// # Beschreibung
    /// Reine Kommunikation: der Abschnitt erklärt eine Grenze, er erzeugt sie
    /// nicht. Die Durchsetzung leisten [`Self::allowed_tools`] und
    /// [`Self::permission_ceiling`].
    ///
    /// # Returns
    /// Einen kurzen, `'static` Textabschnitt in deutscher Sprache.
    #[must_use]
    pub fn prompt_section(&self) -> &'static str {
        match self {
            Self::Chat => CHAT_PROMPT,
            Self::Plan => PLAN_PROMPT,
            Self::Explore => EXPLORE_PROMPT,
            Self::Work => WORK_PROMPT,
        }
    }

    /// Liest einen Modusnamen aus Konfiguration, CLI oder Slash-Kommando.
    ///
    /// # Beschreibung
    /// Akzeptiert Kebab- und Snake-Case sowie beliebige Groß-/Kleinschreibung
    /// und umgebende Leerzeichen. Ein unbekannter Name ergibt `None` — der
    /// Aufrufer entscheidet, ob das ein Fehler ist oder auf den Default fällt.
    /// Es gibt bewusst keine „ungefähre" Auflösung: ein Tippfehler darf nie
    /// stillschweigend in einem weiteren Modus landen.
    ///
    /// # Arguments
    /// - `value` (`&str`): der zu lesende Name, geliehen.
    ///
    /// # Returns
    /// `Some(mode)` bei exakter (normalisierter) Übereinstimmung, sonst `None`.
    ///
    /// # Beispiele
    /// ```rust
    /// use harw_core::mode::InteractionMode;
    ///
    /// assert_eq!(InteractionMode::parse(" WORK "), Some(InteractionMode::Work));
    /// assert_eq!(InteractionMode::parse("wörk"), None);
    /// ```
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let normalized = value.trim().to_ascii_lowercase().replace('-', "_");
        match normalized.as_str() {
            "chat" => Some(Self::Chat),
            "plan" => Some(Self::Plan),
            "explore" => Some(Self::Explore),
            "work" => Some(Self::Work),
            _ => None,
        }
    }

    /// Liefert den kanonischen Namen des Modus.
    ///
    /// # Beschreibung
    /// Gegenstück zu [`Self::parse`] und identisch mit der `serde`-Wire-Form
    /// (`snake_case`). Wird für [`harw_protocol::events::TurnEvent::ModeChanged`]
    /// verwendet.
    ///
    /// # Returns
    /// Einen der Werte `"chat"`, `"plan"`, `"explore"`, `"work"`.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Plan => "plan",
            Self::Explore => "explore",
            Self::Work => "work",
        }
    }
}

impl std::fmt::Display for InteractionMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Liefert die vollständige Permission-Menge, die `harw_sandbox` kennt.
///
/// Ein Schnitt gegen dieses Set ist die Identität — genau das macht `Chat` und
/// `Work` zu Modi ohne Ceiling.
fn all_permissions() -> PermissionSet {
    let all = [
        Permission::ReadWorkspace,
        Permission::WriteWorkspace,
        Permission::ExecuteProcess,
        Permission::NetworkAccess,
        Permission::ReadSecrets,
        Permission::ManagePlugins,
        Permission::ReadCargoRegistry,
    ];
    // Der Aufruf hält den Wächter unten am Leben; die Exhaustiveness-Prüfung
    // leistet der Compiler, nicht diese Zusicherung.
    debug_assert!(all.iter().copied().all(is_known_permission));
    PermissionSet::from_policy(all)
}

/// Wächter gegen eine stillschweigend veraltete Liste in [`all_permissions`].
///
/// Das `match` ist erschöpfend über [`Permission`]. Bekommt der Enum in
/// `harw_sandbox` eine neue Variante, bricht hier der Build — statt dass `Chat`
/// und `Work` die neue Permission unbemerkt wegschneiden.
fn is_known_permission(permission: Permission) -> bool {
    match permission {
        Permission::ReadWorkspace
        | Permission::WriteWorkspace
        | Permission::ExecuteProcess
        | Permission::NetworkAccess
        | Permission::ReadSecrets
        | Permission::ManagePlugins
        | Permission::ReadCargoRegistry => true,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_MODES: [InteractionMode; 4] = [
        InteractionMode::Chat,
        InteractionMode::Plan,
        InteractionMode::Explore,
        InteractionMode::Work,
    ];

    #[test]
    fn test_default_is_chat() {
        assert_eq!(InteractionMode::default(), InteractionMode::Chat);
    }

    #[test]
    fn test_parse_accepts_snake_kebab_and_mixed_case() {
        for (input, expected) in [
            ("chat", InteractionMode::Chat),
            ("CHAT", InteractionMode::Chat),
            ("Plan", InteractionMode::Plan),
            ("  plan  ", InteractionMode::Plan),
            ("EXPLORE", InteractionMode::Explore),
            ("explore", InteractionMode::Explore),
            ("Work", InteractionMode::Work),
            ("wOrK", InteractionMode::Work),
        ] {
            assert_eq!(
                InteractionMode::parse(input),
                Some(expected),
                "'{input}' muss als {expected} gelesen werden"
            );
        }
    }

    #[test]
    fn test_canonical_names_are_single_words_so_kebab_equals_snake() {
        // Solange kein Modusname einen Trenner enthält, fallen Kebab- und
        // Snake-Case zusammen. Der Test hält diese Voraussetzung fest: ein
        // künftiger zweiwortiger Modus muss hier scheitern und dann bewusst
        // beide Schreibweisen belegen.
        for mode in ALL_MODES {
            let name = mode.as_str();
            assert!(
                !name.contains('_') && !name.contains('-'),
                "'{name}' ist nicht einwortig"
            );
            assert_eq!(
                InteractionMode::parse(&name.replace('_', "-")),
                Some(mode),
                "Kebab-Schreibweise von '{name}' muss denselben Modus ergeben"
            );
        }
    }

    #[test]
    fn test_parse_rejects_unknown_value() {
        for input in ["", " ", "planning", "wörk", "read-only", "chat mode"] {
            assert_eq!(
                InteractionMode::parse(input),
                None,
                "'{input}' darf keinen Modus ergeben"
            );
        }
    }

    #[test]
    fn test_as_str_round_trips_through_parse() {
        for mode in ALL_MODES {
            assert_eq!(InteractionMode::parse(mode.as_str()), Some(mode));
        }
    }

    #[test]
    fn test_as_str_matches_serde_wire_form() {
        for mode in ALL_MODES {
            let json = serde_json::to_string(&mode).expect("Modus ist serialisierbar");
            assert_eq!(json, format!("\"{}\"", mode.as_str()));
            let parsed: InteractionMode =
                serde_json::from_str(&json).expect("Modus ist deserialisierbar");
            assert_eq!(parsed, mode);
        }
    }

    #[test]
    fn test_display_matches_as_str() {
        for mode in ALL_MODES {
            assert_eq!(mode.to_string(), mode.as_str());
        }
    }

    #[test]
    fn test_tool_profile_is_minimal_for_read_only_modes() {
        assert_eq!(
            InteractionMode::Explore.tool_profile(),
            ToolProfile::Minimal
        );
        assert_eq!(InteractionMode::Plan.tool_profile(), ToolProfile::Minimal);
        assert_eq!(InteractionMode::Chat.tool_profile(), ToolProfile::Full);
        assert_eq!(InteractionMode::Work.tool_profile(), ToolProfile::Full);
    }

    #[test]
    fn test_allowed_tools_is_none_only_for_chat_and_work() {
        assert!(InteractionMode::Chat.allowed_tools().is_none());
        assert!(InteractionMode::Work.allowed_tools().is_none());
        assert!(InteractionMode::Plan.allowed_tools().is_some());
        assert!(InteractionMode::Explore.allowed_tools().is_some());
    }

    #[test]
    fn test_explore_tools_exclude_every_mutating_tool() {
        let explore = InteractionMode::Explore
            .allowed_tools()
            .expect("Explore hat eine Positivliste");
        for forbidden in [
            "fs.write",
            "shell.exec",
            "web.fetch",
            "plan",
            "goal",
            "research_web",
        ] {
            assert!(
                !explore.contains(&forbidden),
                "'{forbidden}' darf in Explore nicht aktiv sein"
            );
        }
        assert!(explore.contains(&"fs.read"));
        assert!(explore.contains(&"deps.source_read"));
    }

    #[test]
    fn test_plan_tools_extend_explore_tools() {
        let explore = InteractionMode::Explore
            .allowed_tools()
            .expect("Explore hat eine Positivliste");
        let plan = InteractionMode::Plan
            .allowed_tools()
            .expect("Plan hat eine Positivliste");
        for name in explore {
            assert!(
                plan.contains(name),
                "Plan muss das lesende Werkzeug '{name}' erben"
            );
        }
        for added in [
            "plan",
            "goal",
            "web.fetch",
            "web.docs_rs",
            "web.crates_io",
            "explore",
            "research_deps",
            "research_web",
        ] {
            assert!(plan.contains(&added), "Plan muss '{added}' anbieten");
        }
        assert!(
            !plan.contains(&"fs.write") && !plan.contains(&"shell.exec"),
            "Plan bleibt mutationsfrei"
        );
        assert_eq!(plan.len(), explore.len() + 8);
    }

    #[test]
    fn test_explore_ceiling_is_read_only() {
        let ceiling = InteractionMode::Explore.permission_ceiling();
        assert!(ceiling.contains(Permission::ReadWorkspace));
        assert!(ceiling.contains(Permission::ReadCargoRegistry));
        for denied in [
            Permission::WriteWorkspace,
            Permission::ExecuteProcess,
            Permission::NetworkAccess,
            Permission::ReadSecrets,
            Permission::ManagePlugins,
        ] {
            assert!(
                !ceiling.contains(denied),
                "Explore darf {denied:?} nicht zulassen"
            );
        }
    }

    #[test]
    fn test_plan_ceiling_adds_only_network_access() {
        let explore = InteractionMode::Explore.permission_ceiling();
        let plan = InteractionMode::Plan.permission_ceiling();
        assert!(explore.is_subset_of(&plan));
        assert!(plan.contains(Permission::NetworkAccess));
        assert!(!plan.contains(Permission::WriteWorkspace));
        assert!(!plan.contains(Permission::ExecuteProcess));
        assert_eq!(plan.iter().count(), explore.iter().count() + 1);
    }

    #[test]
    fn test_chat_and_work_ceilings_are_the_full_permission_set() {
        let full = all_permissions();
        assert_eq!(InteractionMode::Chat.permission_ceiling(), full);
        assert_eq!(InteractionMode::Work.permission_ceiling(), full);
        assert_eq!(full.iter().count(), 7);
    }

    #[test]
    fn test_ceilings_are_ordered_from_narrow_to_wide() {
        // Explore ⊆ Plan ⊆ Work — jeder Wechsel „nach oben" bleibt trotzdem
        // ein Schnitt, siehe AgentSession::set_mode.
        let explore = InteractionMode::Explore.permission_ceiling();
        let plan = InteractionMode::Plan.permission_ceiling();
        let work = InteractionMode::Work.permission_ceiling();
        assert!(explore.is_subset_of(&plan));
        assert!(plan.is_subset_of(&work));
    }

    #[test]
    fn test_prompt_section_names_its_own_mode() {
        for mode in ALL_MODES {
            let section = mode.prompt_section();
            assert!(
                section.contains(mode.as_str()),
                "der Prompt-Abschnitt von {mode} muss den Modusnamen nennen"
            );
        }
    }

    #[test]
    fn test_is_known_permission_covers_every_variant() {
        for permission in [
            Permission::ReadWorkspace,
            Permission::WriteWorkspace,
            Permission::ExecuteProcess,
            Permission::NetworkAccess,
            Permission::ReadSecrets,
            Permission::ManagePlugins,
            Permission::ReadCargoRegistry,
        ] {
            assert!(is_known_permission(permission));
        }
    }
}
