//! Wurzel-Budget und Kind-Grenzen eines Laufs.
//!
//! # Verantwortungsbereich
//! Zwei Grenzen, die bisher an unterschiedlichen Stellen (oder gar nicht)
//! entstanden:
//!
//! 1. [`RootBudget`] — was der **Wurzel**-Agent eines Laufs höchstens
//!    verbrauchen darf (Runden, Tokens, Wanduhrzeit). Vertrag
//!    `docs/remediation/CONTRACTS.md` §runtime-spec.
//! 2. [`child_limits`] — wie viele **Kinder** ein Agent gleichzeitig halten
//!    darf.
//!
//! # Warum `ResolvedConfig` heute keine Budgetfelder beisteuert
//! `harw_config::ResolvedConfig` (`harw-config/src/discovery.rs:24`) trägt
//! `harness`, `agents`, `executable_agents`, `providers`, `models`, `skills`,
//! `plugins`, `mcps`, `channels`, `auth` und `env_layer`; `HarnessConfig`
//! (`harw-config/src/harness_config.rs:11`) trägt `logging`, `tui`,
//! `session`, `policy`, `mcp_listener`, `onboarding`, `tools`, `mode`,
//! `research` — **kein** Feld nennt Rundenzahl, Token- oder Zeitbudget einer
//! Sitzung. [`RootBudget::from_config`] nimmt die Konfiguration deshalb
//! entgegen, ohne heute eine Grenze daraus zu lesen: die Signatur ist die
//! Stelle, an der ein künftiges `[budget]`-Kapitel andockt, ohne dass ein
//! Aufrufer sich ändert. Die Werte kommen bis dahin aus der Tabelle in
//! [`RootBudget::from_config`].
//!
//! # Fehler
//! Keine: beide Funktionen sind total.

use std::time::Duration;

use harw_config::ResolvedConfig;
use harw_core::ChildLimits;

use crate::spec::{EntryKind, RootBudget};

/// Maximale Modellrunden einer lokal-vertrauten Wurzelsitzung.
const LOCAL_MAX_ROUNDS: u32 = 64;
/// Maximale Tokens einer lokal-vertrauten Wurzelsitzung.
const LOCAL_MAX_TOKENS: u64 = 2_000_000;
/// Maximale Wanduhrzeit einer lokal-vertrauten Wurzelsitzung (1 h).
const LOCAL_MAX_WALL_SECS: u64 = 60 * 60;

/// Maximale Modellrunden eines durablen Jobs.
///
/// # Beschreibung
/// Enger als lokal: ein Job läuft ohne anwesende Person, die ihn abbrechen
/// könnte. Die Zahl ist die Hälfte von [`LOCAL_MAX_ROUNDS`] und bleibt damit
/// unter der harten Werkzeugobergrenze eines MCP-Jobs
/// (`harw_mcp_server::supervisor::MCP_JOB_MAX_TOOL_CALLS` = 64), die pro Runde
/// mindestens einmal belastet wird.
const JOB_MAX_ROUNDS: u32 = 32;
/// Maximale Tokens eines durablen Jobs.
///
/// # Beschreibung
/// Wörtlich die bestehende harte Server-Obergrenze
/// `harw_mcp_server::supervisor::MCP_JOB_MAX_TOKENS` (200_000), auf die
/// `harw-cli/src/job_worker.rs::effective_prompt_budget` jedes gespeicherte
/// Job-Budget bereits deckelt. `harw-runtime` hängt nicht von
/// `harw-mcp-server` ab (Schichtung L12 → L?); die Zahl steht deshalb hier
/// mit Herkunftsangabe statt als importierte Konstante.
const JOB_MAX_TOKENS: u64 = 200_000;
/// Maximale Wanduhrzeit eines durablen Jobs (10 min).
///
/// # Beschreibung
/// Wörtlich `harw_mcp_server::supervisor::MCP_JOB_MAX_WALL_SECONDS` (600),
/// Herkunft wie [`JOB_MAX_TOKENS`].
const JOB_MAX_WALL_SECS: u64 = 600;

/// Maximale Modellrunden eines Gateway-Turns.
const GATEWAY_MAX_ROUNDS: u32 = 16;
/// Maximale Tokens eines Gateway-Turns.
///
/// # Beschreibung
/// Wörtlich `DREAM_MAX_TOKENS` aus `harw-cli/src/gateway.rs:279`, die
/// bestehende Obergrenze des einzigen heute montierten Gateways.
const GATEWAY_MAX_TOKENS: u64 = 16_384;
/// Maximale Wanduhrzeit eines Gateway-Turns (5 min).
const GATEWAY_MAX_WALL_SECS: u64 = 300;

impl RootBudget {
    /// Das Budget einer lokal-vertrauten Wurzelsitzung.
    ///
    /// # Beschreibung
    /// „Unbounded" im Sinne des lokalen Betriebs: die Grenzen sind so
    /// gewählt, dass ein Mensch am Terminal nie an sie stößt — endlich sind
    /// sie trotzdem, denn eine fehlende Grenze ist keine Grenze, sondern ein
    /// Ausfall der Begrenzung (dieselbe Lesart wie
    /// `harw_core::child_controller`s `closed_ceiling`). Eine Sitzung, die
    /// 64 Modellrunden, zwei Millionen Tokens oder eine Stunde Wanduhrzeit
    /// überschreitet, ist kein Normalbetrieb mehr.
    ///
    /// # Rückgabe
    /// `RootBudget { max_model_rounds: 64, max_total_tokens: 2_000_000,
    /// max_wall: 1 h }`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_runtime::RootBudget;
    ///
    /// assert_eq!(RootBudget::unbounded_local().max_model_rounds, 64);
    /// ```
    #[must_use]
    pub const fn unbounded_local() -> Self {
        Self {
            max_model_rounds: LOCAL_MAX_ROUNDS,
            max_total_tokens: LOCAL_MAX_TOKENS,
            max_wall: Duration::from_secs(LOCAL_MAX_WALL_SECS),
        }
    }

    /// Das Wurzelbudget eines Einstiegs.
    ///
    /// # Beschreibung
    /// Drei Gruppen, nach der Frage „wer kann den Lauf abbrechen?":
    ///
    /// | Einstiege | Runden | Tokens | Wanduhr |
    /// |---|---|---|---|
    /// | `Tui`, `OneShot`, `LocalEcho`, `Analyze`, `Doctor`, `Web` | 64 | 2_000_000 | 1 h |
    /// | `McpServe`, `JobPrompt`, `JobPlanNode` | 32 | 200_000 | 10 min |
    /// | `GatewayTelegram`, `GatewayDream` | 16 | 16_384 | 5 min |
    ///
    /// `Web` steht in der lokalen Gruppe: der Web-Einstieg lauscht auf einem
    /// Unix-Socket und kennt seinen Aufrufer über `PeerCredentials` — es ist
    /// dieselbe Maschine und dieselbe Person wie in der TUI. Seine Verengung
    /// leistet das Tier ([`crate::sandbox::permissions_for_tier`]), nicht das
    /// Budget.
    ///
    /// Die Job- und Gateway-Zahlen sind keine Erfindung dieser Datei, sondern
    /// die bereits geltenden Obergrenzen des Workspace (siehe
    /// die Konstanten `JOB_MAX_TOKENS`, `JOB_MAX_WALL_SECS` und
    /// `GATEWAY_MAX_TOKENS` in dieser Datei).
    ///
    /// # Argumente
    /// - `config` (`&ResolvedConfig`): die aufgelöste Konfiguration. Trägt
    ///   heute kein Budgetfeld (siehe Moduldoku) und wird deshalb nur
    ///   protokolliert; die Signatur hält die Andockstelle offen.
    /// - `entry` ([`EntryKind`]): der Einstieg.
    ///
    /// # Rückgabe
    /// Das Budget der Gruppe, in der `entry` liegt.
    #[must_use]
    pub fn from_config(config: &ResolvedConfig, entry: EntryKind) -> Self {
        let budget = match entry {
            EntryKind::Tui
            | EntryKind::OneShot
            | EntryKind::LocalEcho
            | EntryKind::Analyze
            | EntryKind::Doctor
            | EntryKind::Web => Self::unbounded_local(),
            EntryKind::McpServe | EntryKind::JobPrompt | EntryKind::JobPlanNode => Self {
                max_model_rounds: JOB_MAX_ROUNDS,
                max_total_tokens: JOB_MAX_TOKENS,
                max_wall: Duration::from_secs(JOB_MAX_WALL_SECS),
            },
            EntryKind::GatewayTelegram | EntryKind::GatewayDream => Self {
                max_model_rounds: GATEWAY_MAX_ROUNDS,
                max_total_tokens: GATEWAY_MAX_TOKENS,
                max_wall: Duration::from_secs(GATEWAY_MAX_WALL_SECS),
            },
        };
        tracing::debug!(
            entry = ?entry,
            default_model = config.harness.default_model.as_deref().unwrap_or("<none>"),
            max_model_rounds = budget.max_model_rounds,
            max_total_tokens = budget.max_total_tokens,
            max_wall_secs = budget.max_wall.as_secs(),
            "runtime.root_budget.derived"
        );
        budget
    }
}

/// Die Kind-Grenzen eines Agenten, der `model_id` fährt.
///
/// # Beschreibung
/// Befund aus `harw-tui/src/app.rs:1821-1834` (`tui_child_limits`): die
/// Ableitung lief über [`ChildLimits::with_max_children`], und diese Funktion
/// hebt einen Fan-out von `0` **auf `1` an** („ein Deckel von 0 wäre keine
/// Grenze, sondern ein Ausfall der Delegation",
/// `harw-core/src/child_controller.rs:186-188`). Ein Modell mit
/// [`harw_model_catalog::runtime::DelegationPolicy::Forbidden`] hat per
/// Katalog-Invariante `max_child_fanout == 0`
/// (`ModelRuntimeProfile::validate` erzwingt das) — genau diese Modelle
/// durften danach doch ein Kind starten. Delegation *verboten* hieß in der
/// Montage Delegation *erlaubt, aber eines*.
///
/// Hier wird deshalb nicht über [`ChildLimits::with_max_children`] abgeleitet,
/// sondern direkt: `min(Profil-Fan-out, konservative Grenze)`. `0` bleibt `0`
/// — ein Modell, das nicht delegieren darf, bekommt keinen Kind-Slot. Die
/// Ableitung bleibt damit monoton reduzierend gegenüber
/// [`ChildLimits::conservative`] und berührt `max_depth` und `lease_seconds`
/// nicht.
///
/// # Argumente
/// - `config` (`&ResolvedConfig`): die aufgelöste Konfiguration. Entscheidet
///   nichts, sagt aber, ob `model_id` überhaupt deklariert ist — ein im
///   Katalog unbekanntes Modell fällt auf `DEFAULT_PROFILE` zurück
///   (`harw-model-catalog/src/runtime.rs`, „Unbekannte Inputs dürfen
///   Authority niemals erweitern"), und diese Diagnose macht den Rückfall
///   sichtbar statt stumm.
/// - `model_id` (`&str`): das Modell des Agenten.
///
/// # Rückgabe
/// [`ChildLimits`] mit `max_active_children_per_parent` ≤
/// [`ChildLimits::conservative`] und `== 0` für Modelle ohne Delegation.
#[must_use]
pub fn child_limits(config: &ResolvedConfig, model_id: &str) -> ChildLimits {
    let profile = harw_model_catalog::profile_for(model_id);
    let conservative = ChildLimits::conservative();
    let limits = ChildLimits {
        max_active_children_per_parent: usize::from(profile.max_child_fanout)
            .min(conservative.max_active_children_per_parent),
        ..conservative
    };
    tracing::debug!(
        model = model_id,
        declared_in_config = config.models.contains_key(model_id),
        delegation_policy = ?profile.delegation_policy,
        max_child_fanout = profile.max_child_fanout,
        max_active_children_per_parent = limits.max_active_children_per_parent,
        "runtime.child_limits.derived_from_model_profile"
    );
    limits
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_model_catalog::runtime::DelegationPolicy;

    const ALL_ENTRIES: [EntryKind; 11] = [
        EntryKind::Tui,
        EntryKind::OneShot,
        EntryKind::LocalEcho,
        EntryKind::Analyze,
        EntryKind::Doctor,
        EntryKind::Web,
        EntryKind::McpServe,
        EntryKind::JobPrompt,
        EntryKind::JobPlanNode,
        EntryKind::GatewayTelegram,
        EntryKind::GatewayDream,
    ];

    /// Ein Modell, dessen Katalogprofil Delegation verbietet
    /// (`harw-model-catalog/src/runtime.rs`: `max_child_fanout: 0`).
    const FORBIDDEN_MODEL: &str = "deepseek-reasoner";

    #[test]
    fn unbounded_local_is_finite() {
        let budget = RootBudget::unbounded_local();
        assert_eq!(budget.max_model_rounds, LOCAL_MAX_ROUNDS);
        assert_eq!(budget.max_total_tokens, LOCAL_MAX_TOKENS);
        assert_eq!(budget.max_wall, Duration::from_secs(LOCAL_MAX_WALL_SECS));
    }

    #[test]
    fn local_entries_get_the_local_budget() {
        let config = ResolvedConfig::default();
        for entry in [
            EntryKind::Tui,
            EntryKind::OneShot,
            EntryKind::LocalEcho,
            EntryKind::Analyze,
            EntryKind::Doctor,
            EntryKind::Web,
        ] {
            assert_eq!(
                RootBudget::from_config(&config, entry),
                RootBudget::unbounded_local(),
                "{entry:?}"
            );
        }
    }

    #[test]
    fn jobs_and_gateways_are_narrower_than_local() {
        let config = ResolvedConfig::default();
        let local = RootBudget::unbounded_local();
        for entry in [
            EntryKind::McpServe,
            EntryKind::JobPrompt,
            EntryKind::JobPlanNode,
            EntryKind::GatewayTelegram,
            EntryKind::GatewayDream,
        ] {
            let budget = RootBudget::from_config(&config, entry);
            assert!(budget.max_model_rounds < local.max_model_rounds, "{entry:?}");
            assert!(budget.max_total_tokens < local.max_total_tokens, "{entry:?}");
            assert!(budget.max_wall < local.max_wall, "{entry:?}");
        }
    }

    #[test]
    fn every_entry_has_a_finite_budget() {
        let config = ResolvedConfig::default();
        for entry in ALL_ENTRIES {
            let budget = RootBudget::from_config(&config, entry);
            assert!(budget.max_model_rounds > 0, "{entry:?}");
            assert!(budget.max_total_tokens > 0, "{entry:?}");
            assert!(budget.max_wall > Duration::ZERO, "{entry:?}");
        }
    }

    #[test]
    fn forbidden_delegation_yields_zero_children() {
        let config = ResolvedConfig::default();
        let profile = harw_model_catalog::profile_for(FORBIDDEN_MODEL);
        assert_eq!(profile.delegation_policy, DelegationPolicy::Forbidden);
        assert_eq!(profile.max_child_fanout, 0);

        let limits = child_limits(&config, FORBIDDEN_MODEL);
        assert_eq!(limits.max_active_children_per_parent, 0);
        // Die Anhebung auf 1, die `with_max_children` vornimmt, ist der
        // Befund — dieser Vergleich hält ihn fest.
        assert_eq!(
            ChildLimits::with_max_children(0).max_active_children_per_parent,
            1
        );
    }

    #[test]
    fn child_limits_never_exceed_the_conservative_grant() {
        let config = ResolvedConfig::default();
        let conservative = ChildLimits::conservative();
        for model in [
            FORBIDDEN_MODEL,
            "claude-opus-4-8",
            "claude-sonnet-5",
            "llama-3.3-70b-versatile",
            "a-model-nobody-declared",
        ] {
            let limits = child_limits(&config, model);
            assert!(
                limits.max_active_children_per_parent
                    <= conservative.max_active_children_per_parent,
                "{model}"
            );
            assert_eq!(limits.max_depth, conservative.max_depth, "{model}");
            assert_eq!(limits.lease_seconds, conservative.lease_seconds, "{model}");
        }
    }

    #[test]
    fn an_unknown_model_falls_back_to_the_catalog_default() {
        let config = ResolvedConfig::default();
        let fallback = harw_model_catalog::profile_for("a-model-nobody-declared");
        let limits = child_limits(&config, "a-model-nobody-declared");
        assert_eq!(
            limits.max_active_children_per_parent,
            usize::from(fallback.max_child_fanout)
        );
    }
}
