//! Konkrete Sicherheitsregeln dieser Crate (Knoten AW4-03).
//!
//! # Verantwortungsbereich
//! Jedes Untermodul implementiert genau eine [`crate::rule::Rule`] mit
//! goldenen Testfällen (feste Eingabe, festes erwartetes Ergebnis). Drei der
//! vier im Arbeitsauftrag skizzierten Regeln sind hier gebaut:
//!
//! - [`EgressFlowRule`] — ein beobachteter ausgehender Fluss außerhalb des
//!   erlaubten [`harw_sandbox::NetworkScope`].
//! - [`StructureDriftRule`] — meldet, was `harw-dod-workspace` als
//!   `EventKind::StructureDrift` liefert, mit passender Schwere.
//! - [`BaselineDeviationRule`] — demonstriert die Baseline-Regel
//!   (`Established` → `RuleTriggered`, `Provisional` → `Anomaly`) in einer
//!   vollständigen `Rule::evaluate`-Auswertung; siehe [`crate::baseline`]-
//!   Moduldoku für die Regel selbst.
//!
//! Zwei der vier ursprünglich skizzierten Regeln fehlen hier absichtlich:
//!
//! - **`ProcessExec` gegen `can_spawn`**: `harw_agent_dsl::roles::can_spawn`
//!   ist ohne Zyklus erreichbar (`harw-agent-dsl` hängt nur auf
//!   `harw-context`, `blake3`, `semver`, `serde`, `time`, `toml` — keine
//!   `dod`-Abhängigkeit). Erreichbar ist aber nicht dasselbe wie passend:
//!   `can_spawn(caller: AgentRoleId, target: AgentRoleId)` prüft die
//!   Spawn-Matrix zwischen **Orchestrierungsrollen** eines Agentenbaums
//!   (`UserInterface`/`RootOrchestrator`/`ChildOrchestrator`/`Worker`,
//!   durchgesetzt in `harw-core/src/child_controller.rs` beim Zulassen eines
//!   Kind-Agenten). `EventKind::ProcessExec { path, argv_digest }` und
//!   `Actor { uid, auid, cgroup }` — das einzige Vokabular, das ein
//!   Prozessstart auf dem Host mitbringt — enthalten keine `AgentRoleId` und
//!   keinen Bezug zu einem Agentenbaum. Eine Regel, die trotzdem prüfen
//!   wollte, müsste selbst erfinden, welche `AgentRoleId` zu einer `uid`
//!   oder `cgroup` gehört — exakt die „zweite Matrix“, die der Arbeitsauftrag
//!   ausdrücklich untersagt. Ohne eine gemeinsame Kennung zwischen
//!   Host-Prozessbeobachtung und Agent-Orchestrierungsrolle (die keine der
//!   gelesenen Crates heute trägt) ist diese Regel hier nicht ehrlich baubar.
//! - **Vertragsregeln über `validate_patch`/`contract_from_node`**
//!   (`harw-plan`): `harw-plan` selbst ist zyklusfrei anziehbar (hängt nur
//!   auf `harw-macros`/`harw-types`/serde/serde_json/time/tracing). Der
//!   eigentliche Hinderungsgrund ist ein fehlender Beobachtungskanal, nicht
//!   die Abhängigkeitskante: `validate_patch` prüft einen `UnifiedDiff` gegen
//!   einen `MutationContract` — ein Code-Patch, kein Host- oder
//!   Sicherheitsereignis. Kein Sensor dieses Programms emittiert einen Patch
//!   oder Diff in `HostSample`/`SecurityEvent` (Contract-Master: Sensoren
//!   liefern ausschließlich diese beiden Typen). `RuleContext` um ein
//!   Patch-Feld zu erweitern, für das es keinen Erzeuger gibt, wäre eine
//!   Regel ohne jemals erfüllbare Eingabe. Zusätzlich ist die Kantenrichtung
//!   fraglich: `harw-plan` beschreibt Planungs-/Patch-Governance auf einer
//!   höheren Ebene als eine Host-Sicherheitsregel-Engine — eine
//!   Sicherheitsregel-Crate, die sie importiert, ist eher die Konsumentin
//!   eines künftigen `harw-dod-rules`-Befundes als dessen Datenquelle.
//!
//! # Nebenläufigkeit
//! Jede Regel ist ein zustandsloser Unit-Struct: `Send + Sync`.
//!
//! # Fehler
//! Keine — siehe [`crate::rule`]-Moduldoku.

pub mod baseline_deviation;
pub mod egress_flow;
pub mod structure_drift;

pub use baseline_deviation::BaselineDeviationRule;
pub use egress_flow::EgressFlowRule;
pub use structure_drift::StructureDriftRule;
