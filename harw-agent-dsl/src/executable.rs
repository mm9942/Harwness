//! Executable-IR lowering step for resolved agent definitions.
//!
//! This is the fourth stage of the DSL compiler cascade documented in
//! `docs/design/agent-ir-v1.md`:
//!
//! ```text
//! TOML → RawAgentDefinition (AST)
//!      → ResolvedAgentDefinition (semantic IR)
//!      → ExecutableAgentIr        (this module — runtime-shaped IR)
//!      → Runtime effect (jobs, tool surface, model requests)
//! ```
//!
//! The lowering deliberately does NOT execute optimization passes yet — see the
//! anchor doc `docs/design/agent-ir-v1.md §3` for the deferred pass list.
//! Its purpose is to name the boundary between semantic-analysis output and the
//! runtime-consumer input.
//!
//! # Key Types
//! - [`ExecutableAgentIr`] — the runtime-shaped IR produced by this module
//! - [`lower`] — the lowering function: `ResolvedAgentDefinition → ExecutableAgentIr`
//! - [`SpawnContract`] / [`BudgetSpec`] — Spawn-Parameter und typisierte
//!   Ressourcenobergrenzen für Kindsitzungen
//! - [`ContextProgram`] / [`SectionDetail`] — Kontextauswahl je Agent inkl.
//!   [`harw_context::DetailMode`] je Sektion (Folgeknoten zu AW2-01, seit
//!   diesem Knoten `SNAPSHOT_HASH_DOMAIN` `v3`)
//! - [`LifecycleMachine`] — erlaubte Lebenszyklus-Übergänge, inkl. Pause-Sperre
//! - [`ReturnPipeline`] — Validatoren und Rückgabevertrag des Ergebnisses
//! - [`SnapshotId`] — inhaltsadressierter BLAKE3-Digest der IR
//! - [`ReferencedSnapshotId`] — von außen eingelesener, unbestätigter Verweis
//!   auf eine `SnapshotId` (Knoten AW5-09 / UI-07: serde-fähig, aber kein
//!   Konstruktor aus freiem Text für [`SnapshotId`] selbst)
//!
//! # Concurrency
//! All types are `Send + Sync`. [`lower`] is a pure function with no I/O.

#![allow(clippy::module_name_repetitions)]

use crate::authority::AuthorityCeiling;
use crate::error::DslError;
use crate::ids::DefinitionId;
use crate::resolved::{ResolutionTrace, ResolvedAgentDefinition};
use crate::roles::AgentRoleId;

/// Content-addressable identifier for a frozen [`ExecutableAgentIr`] snapshot.
///
/// # Description
/// Wraps a lowercase hex-encoded BLAKE3 digest computed over a stable byte
/// serialization of the IR's content fields (§ Task A). The digest is
/// deterministic: equal IR content always produces the same `SnapshotId` across
/// processes, rebuilds, and platforms.
///
/// The `trace` field (which contains wall-clock timestamps) is intentionally
/// excluded from the digest so that re-lowering the same semantic definition
/// at a different time still yields the same snapshot ID.
///
/// # Hash strategy
/// Hand-written stable byte stream fed to BLAKE3 (single-crate, no MSRV concerns,
/// no Serialize derive cascade onto sub-types). Each field is prefixed with a
/// fixed 4-byte little-endian length tag so that adjacent fields cannot alias.
///
/// # Warum es keinen öffentlichen Konstruktor aus freiem Text gibt
/// `SnapshotId` ist kein Bezeichner, den ein Aufrufer sich ausdenken darf —
/// sie ist eine **berechnete** Funktion des IR-Inhalts (siehe
/// [`compute_snapshot_id`]) und trägt implizit die aktuelle
/// `SNAPSHOT_HASH_DOMAIN`-Fassung. Ein öffentlicher `SnapshotId::new(String)`
/// würde diese Garantie zerstören: jeder eingelesene Wert, der aussieht wie
/// ein 64-stelliger Hex-Digest, wäre dann ununterscheidbar von einem
/// tatsächlich mit [`compute_snapshot_id`] gebildeten. Deshalb bleibt das
/// Feld privat, und dieser Typ implementiert **kein** `Deserialize` — wer
/// eine Snapshot-Identität von außen (Vorschlag, Verwaltungsfläche,
/// gespeicherter Datensatz) einliest, tut das über [`ReferencedSnapshotId`],
/// die ausdrücklich **unbestätigt** heißt, bis sie per
/// [`ReferencedSnapshotId::confirm`] gegen eine frisch berechnete
/// `SnapshotId` geprüft wurde. Von den drei im Rückblick erwogenen Wegen
/// (Deserialize ohne Konstruktor; ein formprüfender Konstruktor; die
/// Trennung berechnet/referenziert) ist das hier gewählte die dritte — die
/// ehrlichste, weil sie „sieht aus wie ein Digest" nie mit „ist der Digest
/// dieses Inhalts" verwechselt, aber auch die teuerste, weil sie einen
/// zweiten Typ und einen expliziten Bestätigungsschritt verlangt. Ein reiner
/// Formprüfer (Länge, Alphabet) hätte diese Verwechslung nicht verhindert —
/// er hätte nur Tippfehler abgewiesen, nicht fremde, aber wohlgeformte
/// Digests aus einer anderen Domänenfassung. Und `Deserialize` ganz ohne
/// Konstruktor (Weg eins) macht denselben Fehler nur stillschweigend: die
/// Typebene sähe aus, als sei der Wert berechnet.
///
/// # Serde-Form und Domänenfassung
/// `SnapshotId` implementiert `Serialize`, aber bewusst nicht `Deserialize`.
/// Die serialisierte Form ist kein nackter String, sondern ein Objekt
/// `{ "domain": "…", "digest": "…" }` — identisch zur Form von
/// [`ReferencedSnapshotId`] (§ dort). Das `domain`-Feld trägt immer die
/// `SNAPSHOT_HASH_DOMAIN`-Fassung, unter der dieser Digest berechnet wurde,
/// sodass ein `v2`-Digest beim erneuten Einlesen nie mit einem `v3`-Digest
/// verwechselt wird — die beiden sehen als Hex-String identisch lang aus,
/// aber die Domäne steht daneben, nicht nur implizit im Hash-Inhalt.
///
/// # Konsument in `harw-knowledge` / Verwaltungsfläche
/// Wer eine Snapshot-Identität einliest (Vorschlag, gespeicherter Datensatz),
/// deserialisiert sie als [`ReferencedSnapshotId`], liest deren `domain()`,
/// und bestätigt sie — falls die passende `ExecutableAgentIr` zur Hand ist —
/// über [`ReferencedSnapshotId::confirm`]. Erst danach ist der Wert eine
/// echte `SnapshotId`, keine Behauptung.
///
/// # Concurrency
/// `Clone + Send + Sync`; immutable after construction.
///
/// # Examples
/// ```rust,no_run
/// use harw_agent_dsl::executable::SnapshotId;
///
/// // Snapshot IDs are created by `lower`; consumers can render an obtained ID.
/// fn render(id: &SnapshotId) -> String {
///     id.to_string()
/// }
/// ```
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct SnapshotId(String);

impl std::fmt::Display for SnapshotId {
    /// Formats the snapshot ID as its lowercase hex digest string.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::fmt::Debug for SnapshotId {
    /// Delegates to Display for a concise representation.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SnapshotId({self})")
    }
}

impl serde::Serialize for SnapshotId {
    /// Serialisiert als `{ "domain": SNAPSHOT_HASH_DOMAIN, "digest": <hex> }`,
    /// damit die Hash-Domänenfassung im gespeicherten/übertragenen Wert
    /// erkennbar bleibt statt implizit im Hash-Inhalt zu verschwinden.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("SnapshotId", 2)?;
        state.serialize_field("domain", SNAPSHOT_HASH_DOMAIN)?;
        state.serialize_field("digest", &self.0)?;
        state.end()
    }
}

/// Von außen eingelesener, **unbestätigter** Verweis auf eine [`SnapshotId`].
///
/// # Beschreibung
/// Ein Vorschlag oder eine Verwaltungsfläche kann eine Programmfassung nur
/// referenzieren, wenn irgendein Typ eine gespeicherte oder übertragene
/// Zeichenkette als Snapshot-Identität wieder einlesen kann. `SnapshotId`
/// selbst tut das nicht (§ dortige Doku) — genau dafür existiert dieser Typ.
/// Er trägt seine Domänenfassung als eigenes Feld (nicht nur implizit im
/// Hash-Inhalt), sodass ein `v2`-Verweis von einem `v3`-Verweis unterscheidbar
/// bleibt, auch wenn beide Digests zufällig dieselbe Hex-Länge haben.
///
/// Ein `ReferencedSnapshotId` ist **kein** Beweis, dass der referenzierte
/// Inhalt existiert oder dass der Digest tatsächlich zu irgendeiner IR
/// gehört — er ist nur eine wohlgeformte Behauptung. [`Self::parse`] prüft
/// ausschließlich die **Form** (Länge, Hex-Alphabet, nicht-leere Domäne); er
/// kann weder erkennen, ob der Digest je berechnet wurde, noch, ob er zum
/// referenzierten Inhalt passt. Erst [`Self::confirm`] gegen eine frisch
/// berechnete [`SnapshotId`] liefert diese Bestätigung.
///
/// # Concurrency
/// `Clone + Send + Sync`; immutable after construction.
///
/// # Examples
/// ```rust,no_run
/// use harw_agent_dsl::executable::{ReferencedSnapshotId, SnapshotId};
///
/// fn confirm_or_explain(
///     referenced: &ReferencedSnapshotId,
///     computed: &SnapshotId,
/// ) -> String {
///     if referenced.confirm(computed).is_some() {
///         "bestätigt".to_owned()
///     } else if referenced.is_current_domain() {
///         "gleiche Domäne, aber Inhalt weicht ab".to_owned()
///     } else {
///         format!("stammt aus fremder Domänenfassung: {}", referenced.domain())
///     }
/// }
/// ```
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReferencedSnapshotId {
    domain: String,
    digest: String,
}

impl ReferencedSnapshotId {
    /// Prüft die Form eines eingelesenen Snapshot-Verweises und baut ihn.
    ///
    /// # Beschreibung
    /// Weist Tippfehler und offensichtlich fremde Werte zurück, ohne zu
    /// behaupten, den Inhalt verifiziert zu haben: geprüft werden nur Länge
    /// (64 Zeichen, wie ein BLAKE3-Hex-Digest), Alphabet (Kleinbuchstaben-Hex)
    /// und eine nicht-leere Domänenangabe. Ob der Digest je berechnet wurde
    /// oder zu einer bestimmten IR passt, kann diese Prüfung **nicht**
    /// feststellen — dafür ist [`Self::confirm`] da.
    ///
    /// # Arguments
    /// - `domain` (`impl Into<String>`): die vom Aufrufer behauptete
    ///   Hash-Domänenfassung (z. B. `"harwness.executable-ir.snapshot/v2"`).
    /// - `digest` (`impl Into<String>`): der behauptete Hex-Digest.
    ///
    /// # Returns
    /// `Ok(ReferencedSnapshotId)` bei wohlgeformter Eingabe.
    ///
    /// # Errors
    /// - [`DslError::Parse`]: wenn die Domäne leer ist, der Digest nicht
    ///   genau 64 Zeichen hat, oder der Digest nicht ausschließlich aus
    ///   Kleinbuchstaben-Hexziffern besteht.
    ///
    /// # Concurrency
    /// Rein; von jedem Thread aus sicher.
    pub fn parse(domain: impl Into<String>, digest: impl Into<String>) -> Result<Self, DslError> {
        let domain = domain.into();
        let digest = digest.into();
        if domain.is_empty() {
            return Err(DslError::Parse(
                "ReferencedSnapshotId: domain darf nicht leer sein".to_owned(),
            ));
        }
        if digest.len() != 64 {
            return Err(DslError::Parse(format!(
                "ReferencedSnapshotId: digest muss 64 Zeichen lang sein, war {}",
                digest.len()
            )));
        }
        if !digest
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        {
            return Err(DslError::Parse(
                "ReferencedSnapshotId: digest muss aus Kleinbuchstaben-Hexziffern bestehen"
                    .to_owned(),
            ));
        }
        Ok(Self { domain, digest })
    }

    /// Liefert die behauptete Hash-Domänenfassung dieses Verweises.
    pub fn domain(&self) -> &str {
        &self.domain
    }

    /// Liefert den behaupteten Hex-Digest dieses Verweises.
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// Prüft, ob dieser Verweis unter der aktuellen `SNAPSHOT_HASH_DOMAIN`
    /// eingelesen wurde.
    ///
    /// # Returns
    /// `true`, wenn [`Self::domain`] exakt der aktuellen Hash-Domänenfassung
    /// entspricht, unter der dieses Crate `SnapshotId`s berechnet. `false`
    /// bei jeder anderen (z. B. veralteten) Domänenfassung — auch dann, wenn
    /// der Digest zufällig wohlgeformt ist.
    ///
    /// # Concurrency
    /// Rein; von jedem Thread aus sicher.
    pub fn is_current_domain(&self) -> bool {
        self.domain == SNAPSHOT_HASH_DOMAIN
    }

    /// Bestätigt diesen Verweis gegen eine frisch berechnete [`SnapshotId`].
    ///
    /// # Beschreibung
    /// Vergleicht Domäne und Digest byteweise gegen `computed`. Das ist die
    /// einzige Stelle, an der ein `ReferencedSnapshotId` zu einer
    /// vertrauenswürdigen `SnapshotId` wird — ohne diesen Aufruf bleibt der
    /// Verweis eine unbestätigte Behauptung, auch wenn [`Self::parse`]
    /// erfolgreich war.
    ///
    /// # Arguments
    /// - `computed` (`&SnapshotId`): die für den tatsächlichen IR-Inhalt neu
    ///   berechnete Identität, gegen die geprüft wird.
    ///
    /// # Returns
    /// `Some(SnapshotId)` (ein Klon von `computed`), wenn Domäne und Digest
    /// übereinstimmen; `None` sonst — entweder weil der Inhalt abweicht oder
    /// weil der Verweis aus einer anderen Domänenfassung stammt.
    ///
    /// # Concurrency
    /// Rein; von jedem Thread aus sicher.
    pub fn confirm(&self, computed: &SnapshotId) -> Option<SnapshotId> {
        if self.is_current_domain() && self.digest == computed.0 {
            Some(computed.clone())
        } else {
            None
        }
    }
}

/// The runtime-shaped IR built from a [`ResolvedAgentDefinition`].
///
/// # Description
/// Consumers (registry factories, job admitters, spawn planners) MUST NOT read
/// [`ResolvedAgentDefinition`] directly — they read this IR. The lowering
/// enforces that every runtime-relevant field is either present, defaulted,
/// or explicitly typed as `Option`.
///
/// This type corresponds to the `ExecutableAgentIr` boundary described in
/// `docs/design/agent-ir-v1.md §2` (the fourth compiler stage).
///
/// # Concurrency
/// `Send + Sync`; immutable after construction.
#[derive(Debug, Clone)]
pub struct ExecutableAgentIr {
    /// Unique namespace ID of this executable IR instance.
    id: DefinitionId,
    /// Authoritative role of this agent (sealed enum, §3).
    role: AgentRoleId,
    /// Specialization identifier (§10); always non-empty after lowering.
    specialization: String,
    /// Authority ceiling after resolution (§7, §12).
    authority: AuthorityCeiling,

    /// Standard-Reasoning-Effort dieses Agenten nach der DSL-Vererbungsregel
    /// (`extends`/Mixins/Schichten: spezifischere Definition überschreibt).
    /// Undurchsichtiger `String` — analog zu `BudgetSpec::effort_cap` — weil
    /// diese Crate nicht von `harw_types::ReasoningEffort` abhängen darf.
    /// `None` bedeutet: keine Ebene der DSL-Auflösung hat eine Aussage
    /// getroffen; die Rangfolge gegenüber Provider-/Modell-/Rollen-Ebene ist
    /// Aufgabe des Konsumenten (`harw-runtime::guard_wiring::resolve_default_reasoning_effort`).
    reasoning_effort: Option<String>,

    /// SpawnContract — the immutable snapshot passed at session construction.
    spawn_contract: SpawnContract,
    /// JobTemplate — the shape of the work unit this agent runs.
    job_template: JobTemplate,
    /// ContextProgram — how instructions and context are assembled for each turn.
    context_program: ContextProgram,
    /// ResolvedToolSurface — the tool inventory this agent may see.
    tool_surface: ResolvedToolSurface,
    /// LifecycleMachine — start/stop/pause states and their allowed transitions.
    lifecycle_machine: LifecycleMachine,
    /// ReturnPipeline — how return values are validated and reported upward.
    return_pipeline: ReturnPipeline,

    /// Provenance from the resolved definition; carried through unchanged.
    /// NOTE: `trace` is excluded from the snapshot hash (contains timestamps).
    trace: ResolutionTrace,

    /// Content-addressable BLAKE3 digest of this IR's stable fields.
    ///
    /// Populated by [`lower`] at the end of lowering. Excludes `trace` and
    /// `snapshot_id` itself from the hashed bytes.
    snapshot_id: SnapshotId,
}

/// Budgetobergrenzen eines Spawn-Vertrags.
///
/// # Beschreibung
/// Typisierte Ressourcenobergrenzen, die die Runtime beim Aufbau einer
/// Kindsitzung durchsetzt. **Jedes Feld ist `Option`**, weil „nicht gesetzt"
/// semantisch etwas anderes ist als „auf 0 begrenzt":
///
/// - `Some(0)` bedeutet: die Ressource ist hart auf null begrenzt (der Agent
///   darf sie nicht verbrauchen).
/// - `None` bedeutet: die Definition macht keine Aussage; die Runtime setzt
///   ihren eigenen konservativen Default ein. `None` heißt ausdrücklich **nicht**
///   „unbegrenzt" — die Entscheidung liegt beim Runtime-Konsumenten, nicht bei
///   dieser IR.
///
/// `effort_cap` wird bewusst als undurchsichtiger `String` geführt und nicht als
/// `harw-types`-Enum: die DSL-Crate darf keine Kopplung an die Runtime-Typen
/// aufbauen. Die Übersetzung in den Effort-Typ ist Aufgabe des Konsumenten und
/// muss dort fail-closed erfolgen (unbekanntes Label → Ablehnung, kein Default).
///
/// # Nebenläufigkeit
/// `Send + Sync`; unveränderlich nach Erstellung.
#[derive(Debug, Clone, Default)]
pub struct BudgetSpec {
    /// Obergrenze der Modell-Token über die gesamte Kindsitzung.
    max_tokens: Option<u64>,
    /// Obergrenze der Werkzeugaufrufe über die gesamte Kindsitzung.
    max_tool_calls: Option<u32>,
    /// Obergrenze der Wanduhrzeit in Sekunden.
    max_wall_secs: Option<u64>,
    /// Obergrenze der Denk-/Effort-Stufe als undurchsichtiges Label.
    effort_cap: Option<String>,
}

/// Spawn contract snapshot — the immutable parameters passed at session construction.
///
/// # Beschreibung
/// Trägt die Parameter, die die Runtime beim Aufbau der Ausführungsumgebung
/// einer (Kind-)Sitzung benötigt: den Workspace-Hinweis, die typisierten
/// Budgetobergrenzen ([`BudgetSpec`]) und die maximale Spawn-Tiefe.
///
/// `max_depth` begrenzt, wie viele Ebenen an Kindagenten unterhalb dieses
/// Agenten noch entstehen dürfen. `None` bedeutet auch hier „keine Aussage der
/// Definition" — die Runtime setzt ihre eigene Obergrenze durch.
///
/// # Nebenläufigkeit
/// `Send + Sync`.
#[derive(Debug, Clone, Default)]
pub struct SpawnContract {
    /// Optional caller-provided workspace root hint (opaque string label).
    workspace_hint: Option<String>,
    /// Typisierte Ressourcenbudgets; `None` = Sektion `[spawn.budget]` fehlt.
    budget: Option<BudgetSpec>,
    /// Maximale Spawn-Tiefe unterhalb dieses Agenten.
    max_depth: Option<u32>,
    /// Exact registered role names of child orchestrators this definition may
    /// create. An empty list denies child-orchestrator delegation by default.
    child_orchestrators: Vec<String>,
}

/// Job template — the shape of the work unit this agent runs.
///
/// # Description
/// Captures the symbolic goal type that drives work-unit creation. The runtime
/// cross-references this label against its job-type registry to select the
/// correct work-unit constructor.
///
/// # Concurrency
/// `Send + Sync`.
#[derive(Debug, Clone, Default)]
pub struct JobTemplate {
    /// Symbolic goal type this agent addresses (opaque string label).
    goal_kind: Option<String>,
}

/// Context program — how instructions and context are assembled for each turn.
///
/// # Beschreibung
/// Verweist auf die Context-Policy, die beim Aufbau des System-Prompts jeder
/// Agentenrunde gilt. `None` bedeutet: die Runtime verwendet ihre
/// Standard-Kontextmontage.
///
/// `must_include` und `exclude` sind Selektor-Listen für die Kontextmontage.
/// Beide werden in **Deklarationsreihenfolge** geführt und von dieser IR nicht
/// normalisiert (nicht sortiert, nicht dedupliziert) — die Reihenfolge ist
/// deshalb Teil des Inhalts und geht genau so in den Snapshot-Hash ein.
/// Bei Konflikt gilt für die Runtime `exclude` vor `must_include` (fail-closed:
/// ein ausgeschlossener Pfad wird nicht durch einen must-include-Treffer
/// wieder hereingeholt).
///
/// # Nebenläufigkeit
/// `Send + Sync`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContextProgram {
    /// Label of the context-policy definition to apply, if any.
    context_policy: Option<String>,
    /// Selektoren, die zwingend im Kontext liegen müssen (Deklarationsreihenfolge).
    must_include: Vec<String>,
    /// Selektoren, die nie in den Kontext gelangen dürfen (Deklarationsreihenfolge).
    exclude: Vec<String>,
    /// Rendermodus je Sektion, in Deklarationsreihenfolge (Knoten-Nachtrag zu
    /// AW2-01, § Doku von [`ContextProgram::from_resolved_program`]).
    section_detail: Vec<SectionDetail>,
}

/// Rendermodus einer einzelnen Sektion eines [`ContextProgram`]s.
///
/// # Beschreibung
/// Trägt genau die zwei Angaben nach, die
/// [`ContextProgram::from_resolved_program`] bislang verwarf: den Sektionsnamen
/// und dessen [`harw_context::DetailMode`] — unabhängig von `strength` (§
/// dortige Doku). Die Liste auf [`ContextProgram::section_detail`] ist
/// reihenfolgetreu, genau wie `must_include`/`exclude`.
///
/// # Nebenläufigkeit
/// `Send + Sync`; klonierbar; unveränderlich nach Erstellung.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionDetail {
    /// Sektionsname, z. B. `"history.tail"`.
    name: String,
    /// Rendermodus dieser Sektion.
    detail: harw_context::DetailMode,
}

impl SectionDetail {
    /// Liefert den Sektionsnamen.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Liefert den Rendermodus dieser Sektion.
    pub fn detail(&self) -> harw_context::DetailMode {
        self.detail
    }
}

/// Aggregated tool surface — what tools this agent may see.
///
/// # Description
/// Holds the admitted and forbidden tool label sets derived from the resolved
/// definition. Runtime consumers cross-reference `admitted` against the
/// ExtensionRegistry to produce the final tool list for each turn.
///
/// # Concurrency
/// `Send + Sync`.
#[derive(Debug, Clone, Default)]
pub struct ResolvedToolSurface {
    /// Labels of tools admitted by the resolved definition.
    /// Runtime-consumers cross-reference these against the ExtensionRegistry.
    admitted: Vec<String>,
    /// Labels of tools explicitly forbidden.
    forbidden: Vec<String>,
}

/// Lifecycle machine — start/stop/pause states and their allowed transitions.
///
/// # Beschreibung
/// Kodiert, welche Lebenszyklus-Übergänge für diese Agenteninstanz erlaubt sind.
/// Die Runtime setzt diese Flags beim Verarbeiten von Lifecycle-Steuernachrichten
/// durch.
///
/// ## `allow_pause = false` ist eine Sperre, kein Hinweis
/// Steht `allow_pause` auf `false`, darf ein Kind **niemals** in einen
/// Wartezustand wie `AwaitingApproval` oder `AwaitingChild` übergehen. Erreicht
/// ein solcher Agent trotzdem einen Punkt, an dem er pausieren müsste (er
/// verlangt eine Freigabe oder wartet auf ein eigenes Kind), ist das ein
/// Vertragsbruch der Definition: die Runtime bricht die Sitzung **fail-closed**
/// ab, statt zu warten. Für read-only Sub-Agenten (Explorer, Researcher) ist
/// genau das der gewünschte Zustand — sie haben keinen Kanal, über den eine
/// Freigabe je eintreffen könnte, und würden sonst unbegrenzt hängen.
///
/// `max_attempts` begrenzt, wie oft ein Lauf insgesamt gestartet werden darf
/// (Erstversuch plus Wiederholungen). `None` heißt „keine Aussage der
/// Definition" — die Runtime setzt ihre eigene Obergrenze durch, nicht
/// „unbegrenzt".
///
/// # Nebenläufigkeit
/// `Send + Sync`.
#[derive(Debug, Clone, Default)]
pub struct LifecycleMachine {
    /// Whether this agent is allowed to pause and resume across turns.
    allow_pause: bool,
    /// Whether this agent is allowed to be replayed from history.
    allow_rerun: bool,
    /// Obergrenze der Gesamtversuche (Erstversuch inklusive).
    max_attempts: Option<u32>,
}

/// Return pipeline — how return values are validated and reported upward.
///
/// # Beschreibung
/// Führt die Validator-Labels auf, die beim Melden eines Ergebnisses laufen.
/// Validatoren laufen in Deklarationsreihenfolge; der erste Fehlschlag bricht ab.
///
/// `contract` benennt zusätzlich das **Rückgabeschema**, an das sich das
/// Ergebnis dieses Agenten halten muss, z. B.
/// `"harwness.return.research-finding@1"`. Es ist ein undurchsichtiges Label:
/// diese IR prüft weder Existenz noch Form des Schemas, sie transportiert nur
/// den Vertrag. Der Konsument muss ein unbekanntes Label fail-closed ablehnen
/// und darf nicht auf „kein Vertrag" zurückfallen. `None` bedeutet, dass die
/// Definition kein Schema festlegt — dann gelten nur die `validators`.
///
/// # Nebenläufigkeit
/// `Send + Sync`.
#[derive(Debug, Clone, Default)]
pub struct ReturnPipeline {
    /// Labels of return-contract validators to run (in declaration order).
    validators: Vec<String>,
    /// Label des Rückgabeschemas, an das sich das Ergebnis halten muss.
    contract: Option<String>,
}

/// Domänen-Tag und Version des Snapshot-Hash-Formats.
///
/// # Beschreibung
/// Wird als erstes Feld in den Hash-Strom geschrieben und trennt die
/// Adressräume verschiedener Hash-Formatversionen voneinander. Sobald die
/// Menge oder Reihenfolge der gehashten Felder sich ändert, **muss** dieser
/// Tag hochgezählt werden: sonst könnten zwei IRs, die unter verschiedenen
/// Formatversionen gehasht wurden, denselben Digest tragen, obwohl sie nicht
/// dasselbe bedeuten.
///
/// `v2` fügt gegenüber `v1` die typisierten Spawn-Budgets, `max_depth`,
/// `lifecycle.max_attempts`, `return.contract` sowie
/// `context.must_include`/`exclude` hinzu und macht `Option`-Felder durch ein
/// Präsenz-Byte eindeutig (in `v1` waren `None` und `Some("")` ununterscheidbar).
///
/// `v3` (dieser Knoten) fügt `context_program.section_detail` hinzu: den
/// [`harw_context::DetailMode`] je Sektion, den
/// [`ContextProgram::from_resolved_program`] bis hierhin verwarf (§ dortige
/// Doku, „Limitation"). Das ist ein neues Feld auf einer bereits gehashten
/// Teilstruktur (`context_program`), nicht nur ein angehängtes — nach der
/// Normativität oben (§ „Die Reihenfolge ist Teil des Vertrags") verschiebt
/// das den Digest **jeder** bestehenden Definition, ob sie das neue Feld
/// nutzt oder nicht (ein leeres `section_detail` hasht trotzdem eine
/// zusätzliche 4-Byte-Null-Länge). Deshalb steigt der Domain-Tag: ohne die
/// Anhebung könnten zwei IRs — eine unter `v2` gehasht, eine inhaltlich
/// identische unter `v3` — denselben Digest tragen, obwohl ihre Byte-Ströme
/// unterschiedlich lang sind (ein `v2`-Digest kollidiert damit nie zufällig
/// mit einem `v3`-Digest; beide Räume sind durch den Domain-String getrennt).
/// Golden-Snapshots aus `v2` sind ab diesem Knoten bewusst ungültig — kein
/// stillschweigend verschobener Hash, siehe Abschlussbericht des Knotens.
const SNAPSHOT_HASH_DOMAIN: &str = "harwness.executable-ir.snapshot/v5";

/// Berechnet einen stabilen BLAKE3-Digest über die Inhaltsfelder einer [`ExecutableAgentIr`].
///
/// # Beschreibung
/// Speist einen deterministischen Byte-Strom in einen BLAKE3-Hasher. Kodierung:
///
/// - **Strings**: 4-Byte-Längenpräfix (little endian) gefolgt von den UTF-8-Bytes.
/// - **`Option<…>`**: ein Präsenz-Byte (`0x00` = `None`, `0x01` = `Some`); nur bei
///   `Some` folgt die Nutzlast. Dadurch kann `None` nie mit `Some("")` oder
///   `Some(0)` kollidieren.
/// - **`bool`**: ein Byte (`0` oder `1`).
/// - **Ganzzahlen**: little endian in fester Breite (`u32` = 4 Byte, `u64` = 8 Byte).
/// - **Listen**: 4-Byte-Anzahl (little endian), danach die Einträge mit je
///   eigenem Längenpräfix.
///
/// ## Die Reihenfolge ist Teil des Vertrags
/// Die unten festgelegte Feldreihenfolge ist **normativ**. Sie folgt der
/// Deklarationsreihenfolge der Struktur (`id` → `role` → `specialization` →
/// `authority` → `spawn_contract` → `job_template` → `context_program` →
/// `tool_surface` → `lifecycle_machine` → `return_pipeline`), und innerhalb
/// jeder Teilstruktur wiederum deren Felddeklarationsreihenfolge. Neue Felder
/// werden bei ihrer Teilstruktur eingefügt, nicht hinten angehängt — und jede
/// solche Änderung erfordert eine Erhöhung von [`SNAPSHOT_HASH_DOMAIN`], weil
/// sie sämtliche Digests verschiebt. Wer die Reihenfolge ändert, ändert die
/// Adressierung aller Snapshots.
///
/// ## Was bewusst nicht gehasht wird
/// `trace` (enthält Wanduhr-Zeitstempel) und `snapshot_id` selbst bleiben außen
/// vor: der Digest muss über Wiederholungen des Lowerings stabil bleiben und
/// darf sich nicht selbst referenzieren.
///
/// ## Sortiert vs. reihenfolgetreu
/// `authority.capabilities`, `tool_surface.admitted` und `tool_surface.forbidden`
/// werden vor dem Hashen sortiert (Mengensemantik). `return_pipeline.validators`
/// sowie `context_program.must_include`/`exclude`/`section_detail` (neu in
/// `v3`) werden reihenfolgetreu gehasht, weil dort die Deklarationsreihenfolge
/// Bedeutung trägt.
///
/// # Arguments
/// - `ir` (`&ExecutableAgentIr`): die IR, deren Inhaltsfelder gehasht werden.
///
/// # Returns
/// Ein Kleinbuchstaben-Hex-String des BLAKE3-Digests (64 Zeichen).
///
/// # Concurrency
/// Rein; von jedem Thread aus sicher.
fn compute_snapshot_id(ir: &ExecutableAgentIr) -> SnapshotId {
    // Längenpräfigierte Bytes: das 4-Byte-LE-Längentag verhindert, dass
    // benachbarte Felder ineinanderlaufen ("ab" + "c" vs. "a" + "bc").
    fn hash_bytes(h: &mut blake3::Hasher, data: &[u8]) {
        let len = data.len() as u32;
        h.update(&len.to_le_bytes());
        h.update(data);
    }
    fn hash_str(h: &mut blake3::Hasher, s: &str) {
        hash_bytes(h, s.as_bytes());
    }
    fn hash_bool(h: &mut blake3::Hasher, b: bool) {
        h.update(&[u8::from(b)]);
    }
    // Präsenz-Byte für Option-Felder; nur bei Some folgt die Nutzlast.
    fn hash_opt_str(h: &mut blake3::Hasher, value: Option<&str>) {
        match value {
            Some(s) => {
                h.update(&[1u8]);
                hash_str(h, s);
            }
            None => {
                h.update(&[0u8]);
            }
        }
    }
    fn hash_opt_u64(h: &mut blake3::Hasher, value: Option<u64>) {
        match value {
            Some(n) => {
                h.update(&[1u8]);
                h.update(&n.to_le_bytes());
            }
            None => {
                h.update(&[0u8]);
            }
        }
    }
    fn hash_opt_u32(h: &mut blake3::Hasher, value: Option<u32>) {
        match value {
            Some(n) => {
                h.update(&[1u8]);
                h.update(&n.to_le_bytes());
            }
            None => {
                h.update(&[0u8]);
            }
        }
    }
    fn hash_str_vec(h: &mut blake3::Hasher, v: &[String]) {
        let count = v.len() as u32;
        h.update(&count.to_le_bytes());
        for s in v {
            hash_str(h, s);
        }
    }
    // Stabile, von `Debug` unabhängige Kurzform je `DetailMode`-Variante —
    // anders als bei `role` (§ Kopffelder) bewusst nicht über `{:?}`, weil
    // dieses Feld neu ist und keinen bestehenden Format-Präzedenzfall trägt.
    fn detail_mode_tag(mode: harw_context::DetailMode) -> &'static str {
        match mode {
            harw_context::DetailMode::Full => "full",
            harw_context::DetailMode::Summary => "summary",
            harw_context::DetailMode::References => "references",
        }
    }
    // Reihenfolgetreu, wie `context_program.must_include`/`exclude`: die
    // Deklarationsreihenfolge der Sektionen ist Inhalt.
    fn hash_section_detail_vec(h: &mut blake3::Hasher, v: &[SectionDetail]) {
        let count = v.len() as u32;
        h.update(&count.to_le_bytes());
        for entry in v {
            hash_str(h, &entry.name);
            hash_str(h, detail_mode_tag(entry.detail));
        }
    }

    let mut hasher = blake3::Hasher::new();

    // Domänen-/Formatversions-Tag zuerst — trennt Hash-Formatversionen.
    hash_str(&mut hasher, SNAPSHOT_HASH_DOMAIN);

    // --- Kopffelder ---
    // id
    hash_str(&mut hasher, &ir.id.to_string());
    // role — use Debug for a stable, process-independent string
    hash_str(&mut hasher, &format!("{:?}", ir.role));
    // specialization
    hash_str(&mut hasher, &ir.specialization);
    // reasoning_effort — neu in v5: undurchsichtiges Label, Präsenz-Byte + Wert
    hash_opt_str(&mut hasher, ir.reasoning_effort.as_deref());
    // authority.capabilities (sorted for determinism)
    let mut sorted_caps = ir.authority.capabilities.clone();
    sorted_caps.sort();
    hash_str_vec(&mut hasher, &sorted_caps);

    // --- spawn_contract (Felddeklarationsreihenfolge) ---
    hash_opt_str(&mut hasher, ir.spawn_contract.workspace_hint.as_deref());
    // budget: Präsenz-Byte, danach die vier Budgetfelder in Deklarationsreihenfolge.
    // Eine leere `[spawn.budget]`-Sektion ist damit unterscheidbar von einer
    // fehlenden Sektion — beides sind verschiedene Aussagen der Definition.
    match ir.spawn_contract.budget.as_ref() {
        Some(budget) => {
            hasher.update(&[1u8]);
            hash_opt_u64(&mut hasher, budget.max_tokens);
            hash_opt_u32(&mut hasher, budget.max_tool_calls);
            hash_opt_u64(&mut hasher, budget.max_wall_secs);
            hash_opt_str(&mut hasher, budget.effort_cap.as_deref());
        }
        None => {
            hasher.update(&[0u8]);
        }
    }
    hash_opt_u32(&mut hasher, ir.spawn_contract.max_depth);
    hash_str_vec(&mut hasher, &ir.spawn_contract.child_orchestrators);

    // --- job_template ---
    hash_opt_str(&mut hasher, ir.job_template.goal_kind.as_deref());

    // --- context_program ---
    hash_opt_str(&mut hasher, ir.context_program.context_policy.as_deref());
    // Reihenfolgetreu: die IR normalisiert diese Listen nicht, also ist ihre
    // Reihenfolge Inhalt und muss den Digest beeinflussen.
    hash_str_vec(&mut hasher, &ir.context_program.must_include);
    hash_str_vec(&mut hasher, &ir.context_program.exclude);
    // Neu in v3: DetailMode je Sektion, reihenfolgetreu (§ SNAPSHOT_HASH_DOMAIN).
    hash_section_detail_vec(&mut hasher, &ir.context_program.section_detail);

    // --- tool_surface ---
    // tool_surface.admitted (sorted for determinism)
    let mut admitted = ir.tool_surface.admitted.clone();
    admitted.sort();
    hash_str_vec(&mut hasher, &admitted);
    // tool_surface.forbidden (sorted for determinism)
    let mut forbidden = ir.tool_surface.forbidden.clone();
    forbidden.sort();
    hash_str_vec(&mut hasher, &forbidden);

    // --- lifecycle_machine ---
    hash_bool(&mut hasher, ir.lifecycle_machine.allow_pause);
    hash_bool(&mut hasher, ir.lifecycle_machine.allow_rerun);
    hash_opt_u32(&mut hasher, ir.lifecycle_machine.max_attempts);

    // --- return_pipeline ---
    // validators (order-preserving — declaration order is semantic)
    hash_str_vec(&mut hasher, &ir.return_pipeline.validators);
    hash_opt_str(&mut hasher, ir.return_pipeline.contract.as_deref());

    SnapshotId(hasher.finalize().to_hex().to_string())
}

impl ExecutableAgentIr {
    /// Returns the definition ID carried by this IR.
    pub fn id(&self) -> &DefinitionId {
        &self.id
    }

    /// Returns the authoritative role carried by this IR.
    pub fn role(&self) -> AgentRoleId {
        self.role
    }

    /// Returns the non-empty specialization label.
    pub fn specialization(&self) -> &str {
        &self.specialization
    }

    /// Returns the resolved authority ceiling.
    pub fn authority(&self) -> &AuthorityCeiling {
        &self.authority
    }

    /// Returns the agent-level default reasoning effort, if any DSL layer
    /// (target, mixin, or `extends` ancestor) set one.
    ///
    /// # Description
    /// Undurchsichtiges Label (nicht `harw_types::ReasoningEffort`, siehe
    /// Feld-Doku). Der Konsument entscheidet die Rangfolge gegenüber
    /// Provider-/Modell-/Rollen-Default und muss ein unbekanntes Label
    /// fail-closed behandeln (nicht raten).
    ///
    /// # Returns
    /// `Some(&str)` mit dem aufgelösten Label, `None` wenn keine Ebene eine
    /// Aussage getroffen hat.
    pub fn reasoning_effort(&self) -> Option<&str> {
        self.reasoning_effort.as_deref()
    }

    /// Returns the immutable spawn contract snapshot.
    pub fn spawn_contract(&self) -> &SpawnContract {
        &self.spawn_contract
    }

    /// Returns the immutable job template.
    pub fn job_template(&self) -> &JobTemplate {
        &self.job_template
    }

    /// Returns the immutable context program.
    pub fn context_program(&self) -> &ContextProgram {
        &self.context_program
    }

    /// Returns the immutable resolved tool surface.
    pub fn tool_surface(&self) -> &ResolvedToolSurface {
        &self.tool_surface
    }

    /// Returns the immutable lifecycle machine.
    pub fn lifecycle_machine(&self) -> &LifecycleMachine {
        &self.lifecycle_machine
    }

    /// Returns the immutable return pipeline.
    pub fn return_pipeline(&self) -> &ReturnPipeline {
        &self.return_pipeline
    }

    /// Returns the lowering provenance.
    pub fn trace(&self) -> &ResolutionTrace {
        &self.trace
    }

    /// Returns the content-addressable [`SnapshotId`] for this IR instance.
    ///
    /// # Description
    /// The `snapshot_id` field is computed by [`lower`] at construction time and
    /// cached on the struct. This accessor returns a clone. It is provided for
    /// convenience so callers do not need to access the public field directly.
    ///
    /// # Returns
    /// The [`SnapshotId`] that pins this IR to its content digest.
    ///
    /// # Concurrency
    /// Safe from any thread; no I/O.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_agent_dsl::executable::lower;
    /// // (see `lower` doc for a full example)
    /// ```
    pub fn snapshot_id(&self) -> SnapshotId {
        self.snapshot_id.clone()
    }
}

impl SpawnContract {
    /// Returns the optional workspace root hint.
    pub fn workspace_hint(&self) -> Option<&str> {
        self.workspace_hint.as_deref()
    }

    /// Liefert die typisierten Ressourcenbudgets dieses Spawn-Vertrags.
    ///
    /// # Returns
    /// `Some(&BudgetSpec)`, wenn die Definition eine `[spawn.budget]`-Sektion
    /// führt — auch dann, wenn darin kein einziges Feld gesetzt ist. `None`,
    /// wenn die Sektion fehlt. Beide Fälle sind verschiedene Aussagen und
    /// tragen verschiedene [`SnapshotId`]s.
    ///
    /// # Concurrency
    /// Reiner Lesezugriff; von jedem Thread aus sicher.
    pub fn budget(&self) -> Option<&BudgetSpec> {
        self.budget.as_ref()
    }

    /// Liefert die maximale Spawn-Tiefe unterhalb dieses Agenten.
    ///
    /// # Returns
    /// `Some(n)`, wenn die Definition eine Tiefe festlegt (`Some(0)` = dieser
    /// Agent darf keine Kinder erzeugen); `None`, wenn sie keine Aussage macht —
    /// dann gilt die Obergrenze der Runtime, nicht „unbegrenzt".
    ///
    /// # Concurrency
    /// Reiner Lesezugriff; von jedem Thread aus sicher.
    pub fn max_depth(&self) -> Option<u32> {
        self.max_depth
    }

    /// Returns the exact child-orchestrator role names this agent may spawn.
    /// An absent or empty TOML list is an explicit default-deny policy.
    #[must_use]
    pub fn child_orchestrators(&self) -> &[String] {
        &self.child_orchestrators
    }

    /// Tests whether this definition explicitly grants spawning `role_name` as
    /// a child orchestrator.
    #[must_use]
    pub fn permits_child_orchestrator(&self, role_name: &str) -> bool {
        self.child_orchestrators
            .iter()
            .any(|allowed| allowed == role_name)
    }
}

impl BudgetSpec {
    /// Liefert die Token-Obergrenze der Kindsitzung.
    ///
    /// # Returns
    /// `Some(n)` als harte Obergrenze; `None`, wenn die Definition keine Aussage
    /// macht (Runtime-Default, **nicht** unbegrenzt).
    pub fn max_tokens(&self) -> Option<u64> {
        self.max_tokens
    }

    /// Liefert die Obergrenze der Werkzeugaufrufe.
    ///
    /// # Returns
    /// `Some(n)` als harte Obergrenze; `None`, wenn die Definition keine Aussage
    /// macht (Runtime-Default, **nicht** unbegrenzt).
    pub fn max_tool_calls(&self) -> Option<u32> {
        self.max_tool_calls
    }

    /// Liefert die Obergrenze der Wanduhrzeit in Sekunden.
    ///
    /// # Returns
    /// `Some(n)` als harte Obergrenze; `None`, wenn die Definition keine Aussage
    /// macht (Runtime-Default, **nicht** unbegrenzt).
    pub fn max_wall_secs(&self) -> Option<u64> {
        self.max_wall_secs
    }

    /// Liefert die Obergrenze der Effort-Stufe als undurchsichtiges Label.
    ///
    /// # Returns
    /// `Some(label)` — der Konsument übersetzt das Label selbst und muss ein
    /// unbekanntes Label fail-closed ablehnen. `None`, wenn keine Obergrenze
    /// festgelegt ist.
    pub fn effort_cap(&self) -> Option<&str> {
        self.effort_cap.as_deref()
    }
}

impl JobTemplate {
    /// Returns the optional symbolic goal kind.
    pub fn goal_kind(&self) -> Option<&str> {
        self.goal_kind.as_deref()
    }
}

impl ContextProgram {
    /// Returns the optional context-policy label.
    pub fn context_policy(&self) -> Option<&str> {
        self.context_policy.as_deref()
    }

    /// Liefert die Selektoren, die zwingend im Kontext liegen müssen.
    ///
    /// # Returns
    /// Die Labels in Deklarationsreihenfolge; leer, wenn die Definition nichts
    /// erzwingt.
    pub fn must_include(&self) -> &[String] {
        &self.must_include
    }

    /// Liefert die Selektoren, die nie in den Kontext gelangen dürfen.
    ///
    /// # Returns
    /// Die Labels in Deklarationsreihenfolge; leer, wenn nichts ausgeschlossen
    /// ist. Bei Konflikt mit [`ContextProgram::must_include`] hat der Ausschluss
    /// Vorrang.
    pub fn exclude(&self) -> &[String] {
        &self.exclude
    }

    /// Liefert den Rendermodus je Sektion, in Deklarationsreihenfolge.
    ///
    /// # Returns
    /// Eine Liste, die für jede Sektion eines über
    /// [`ContextProgram::from_resolved_program`] gebauten Programms Name und
    /// [`harw_context::DetailMode`] trägt — unabhängig von `strength`, also
    /// auch für `Normal`-starke Sektionen, die weder in [`Self::must_include`]
    /// noch in [`Self::exclude`] auftauchen. Ein über [`lower`]s
    /// `[context]`/`[context_program]`-Konfigurationspfad gebautes Programm
    /// liefert immer eine leere Liste: dieser Pfad kennt keine
    /// Pro-Sektion-Detailangabe.
    pub fn section_detail(&self) -> &[SectionDetail] {
        &self.section_detail
    }
}

impl ContextProgram {
    /// Baut ein [`ContextProgram`] aus einer aufgelösten, gegen eine Decke
    /// geprüften Kontextprogramm-Deklaration (Knoten AW2-01,
    /// [`crate::context_program`]).
    ///
    /// # Description
    /// Das ist die fehlende Verbindung zwischen einem *deklarierten*
    /// `harwness.context.<name>@<v>`-Programm
    /// ([`crate::context_program::ResolvedContextProgramDefinition`]) und der
    /// Executable-Repräsentation, die [`ExecutableAgentIr`] konsumiert. `program`
    /// wird **vor** jeder Feldbefüllung gegen `ceiling` geprüft
    /// ([`crate::context_program::ContextCeilingAdmission::admits_program`]) —
    /// ein Programm, das mehr verlangt, als `ceiling` zulässt, erreicht dieses
    /// Struct nie, es wird abgewiesen, nicht stillschweigend beschnitten (Knoten
    /// AW2-01 "Der Resolver ruft ceiling.admits").
    ///
    /// `context_policy` wird auf die kanonische ID von `program` gesetzt (damit
    /// das Ergebnis dokumentiert, welches deklarierte Programm es erzeugt hat).
    /// `must_include` sammelt die Namen aller Sektionen mit
    /// [`crate::context_program::SectionStrength::MustInclude`], in
    /// Deklarationsreihenfolge; `exclude` wird unverändert übernommen.
    ///
    /// Diese Konstruktion ändert weder die Feldmenge von [`ContextProgram`] noch
    /// [`compute_snapshot_id`] — beide bleiben exakt wie vor Knoten AW2-01. Ein
    /// per `from_resolved_program` gebautes Programm geht über dieselben drei
    /// bereits gehashten Felder in die [`SnapshotId`] ein wie jedes andere
    /// [`ContextProgram`].
    ///
    /// # Limitation (aktualisiert, Folgeknoten zu AW2-01)
    /// Der [`harw_context::DetailMode`] je Sektion wird jetzt getragen — siehe
    /// [`ContextProgram::section_detail`], befüllt für **alle** Sektionen
    /// unabhängig von `strength`. Das hat `SNAPSHOT_HASH_DOMAIN` von `v2` auf
    /// `v3` angehoben (§ dortige Doku): eine neue Teilstruktur-Feld-Angabe
    /// verschiebt jeden bestehenden Digest, unabhängig davon, ob er das neue
    /// Feld nutzt.
    ///
    /// Weiterhin **nicht** getragen: die [`harw_context::TrustClass`] einer
    /// Sektion, und — für `must_include`/`exclude` — eine `Normal`-starke
    /// Sektion, die nicht ausgeschlossen ist, taucht dort weiterhin nicht auf
    /// (sie tut es jetzt aber in `section_detail`, weil dieses Feld nicht nach
    /// `strength` filtert). Beide Sektionen fließen in die Deckenprüfung ein
    /// (ein zu weitreichendes Programm wird also weiterhin abgewiesen), aber
    /// `TrustClass` selbst hat noch kein Feld auf [`ContextProgram`]. Sie zu
    /// tragen, verlangt ein weiteres neues Feld plus eine weitere Anhebung von
    /// `SNAPSHOT_HASH_DOMAIN` (siehe Abschlussbericht dieses Folgeknotens).
    ///
    /// # Arguments
    /// - `program` (`&ResolvedContextProgramDefinition`): die vollständig
    ///   aufgelöste Deklaration, die umgewandelt wird.
    /// - `ceiling` (`&harw_context::ContextCeiling`): die Kontextdecke des
    ///   Agenten, gegen die `program` geprüft wird.
    ///
    /// # Returns
    /// `Ok(ContextProgram)`, wenn `program` vollständig innerhalb von `ceiling` liegt.
    ///
    /// # Errors
    /// - [`crate::context_program::ProgramCeilingViolation`]: `program` verlangt
    ///   eine Sektion oder Vertrauensklasse, die `ceiling` nicht zulässt.
    ///
    /// # Concurrency
    /// Reine Funktion; von jedem Thread aus sicher.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_agent_dsl::context_program::ResolvedContextProgramDefinition;
    /// use harw_agent_dsl::executable::ContextProgram;
    /// use harw_context::ContextCeiling;
    ///
    /// fn build(program: &ResolvedContextProgramDefinition, ceiling: &ContextCeiling) {
    ///     match ContextProgram::from_resolved_program(program, ceiling) {
    ///         Ok(cp) => println!("must_include: {:?}", cp.must_include()),
    ///         Err(e) => eprintln!("rejected: {e}"),
    ///     }
    /// }
    /// ```
    pub fn from_resolved_program(
        program: &crate::context_program::ResolvedContextProgramDefinition,
        ceiling: &harw_context::ContextCeiling,
    ) -> Result<Self, crate::context_program::ProgramCeilingViolation> {
        use crate::context_program::ContextCeilingAdmission;
        ceiling.admits_program(program)?;

        let must_include = program
            .sections
            .iter()
            .filter(|section| {
                section.strength == crate::context_program::SectionStrength::MustInclude
            })
            .map(|section| section.name.clone())
            .collect();

        // Anders als `must_include`: nicht nach `strength` gefiltert. Der
        // Rendermodus gilt für jede zugelassene Sektion, nicht nur für die
        // zwingend aufzunehmenden.
        let section_detail = program
            .sections
            .iter()
            .map(|section| SectionDetail {
                name: section.name.clone(),
                detail: section.detail,
            })
            .collect();

        Ok(ContextProgram {
            context_policy: Some(program.id.to_string()),
            must_include,
            exclude: program.exclude.clone(),
            section_detail,
        })
    }
}

impl ResolvedToolSurface {
    /// Returns the admitted tool labels.
    pub fn admitted(&self) -> &[String] {
        &self.admitted
    }

    /// Returns the forbidden tool labels.
    pub fn forbidden(&self) -> &[String] {
        &self.forbidden
    }
}

impl LifecycleMachine {
    /// Returns whether pause/resume is allowed.
    pub fn allow_pause(&self) -> bool {
        self.allow_pause
    }

    /// Returns whether replay is allowed.
    pub fn allow_rerun(&self) -> bool {
        self.allow_rerun
    }

    /// Liefert die Obergrenze der Gesamtversuche (Erstversuch inklusive).
    ///
    /// # Returns
    /// `Some(n)` als harte Obergrenze; `None`, wenn die Definition keine Aussage
    /// macht — dann gilt die Obergrenze der Runtime, nicht „unbegrenzt".
    pub fn max_attempts(&self) -> Option<u32> {
        self.max_attempts
    }
}

impl ReturnPipeline {
    /// Returns return-contract validators in declaration order.
    pub fn validators(&self) -> &[String] {
        &self.validators
    }

    /// Liefert das Label des Rückgabeschemas.
    ///
    /// # Returns
    /// `Some(label)`, z. B. `"harwness.return.research-finding@1"`; `None`, wenn
    /// die Definition kein Schema festlegt. Ein unbekanntes Label muss der
    /// Konsument fail-closed ablehnen und darf nicht auf „kein Vertrag"
    /// zurückfallen.
    pub fn contract(&self) -> Option<&str> {
        self.contract.as_deref()
    }
}

/// Lowers a [`ResolvedAgentDefinition`] into an [`ExecutableAgentIr`].
///
/// # Description
/// This is the fourth compiler stage in the DSL cascade
/// (`docs/design/agent-ir-v1.md §2`). The lowering is deliberately minimal:
/// it maps the resolved definition's authoritative fields directly into the
/// IR's runtime-shaped slots. No optimisation passes are applied.
///
/// The resolved definition keeps extension fields in its typed TOML config
/// table.  This lowering reads the fields whose executable representation is
/// already typed here and ignores unrelated/ill-typed config values.  It must
/// not silently discard a supported value merely because it arrived through
/// the free-form resolved config table.
///
/// # Erkannte Konfigurationssektionen
/// Jede Sektion ist unter ihrem Kurznamen und unter dem Namen der Zielstruktur
/// ansprechbar (`[spawn]` oder `[spawn_contract]` usw.):
///
/// - `[spawn]`: `workspace_hint` (String), `max_depth` (u32), `child_orchestrators` (exact role-name list)
/// - `[spawn.budget]`: `max_tokens` (u64), `max_tool_calls` (u32),
///   `max_wall_secs` (u64), `effort_cap` (String)
/// - `[job]`: `goal_kind` (String)
/// - `[context]`: `policy` bzw. `context_policy` (String),
///   `must_include` (Liste), `exclude` (Liste)
/// - `[tools]`: `admitted` (Liste), `forbidden` (Liste)
/// - `[lifecycle]`: `allow_pause` (bool), `allow_rerun` (bool),
///   `max_attempts` (u32)
/// - `[return]`: `validators` (Liste), `contract` (String)
///
/// Fehlende Sektionen und fehlende Felder ergeben `None` bzw. leere Listen.
/// Werte mit falschem TOML-Typ (und negative bzw. überlaufende Ganzzahlen)
/// gelten bewusst als nicht vorhanden: die aufgelöste Config ist eine
/// freiformige Kompatibilitätsschicht, die typisierte IR darf daraus keine
/// Werte erfinden. Die Booleans fallen auf `false` zurück — für `allow_pause`
/// ist das die fail-closed-Richtung.
///
/// # Arguments
/// - `resolved` (`&ResolvedAgentDefinition`): the fully resolved definition to lower.
///
/// # Returns
/// `Ok(ExecutableAgentIr)` on success.
///
/// # Errors
/// - [`DslError::Parse`]: if `resolved.specialization` is empty. A non-empty
///   specialization is a hard precondition for the executable IR (§10).
///
/// # Concurrency
/// Pure function — safe from any thread. Does not touch I/O.
///
/// # Examples
/// ```rust,no_run
/// use harw_agent_dsl::parse::parse_toml;
/// use harw_agent_dsl::resolve::resolve_definition;
/// use harw_agent_dsl::executable::lower;
/// use harw_agent_dsl::ids::DefinitionId;
/// use harw_agent_dsl::layers::DefinitionLayer;
///
/// let src = r#"
/// schema = "harwness.agent/v1"
/// id = "harwness.agent.my-worker@1"
/// version = "1.0.0"
/// role = "worker"
/// specialization = "my-worker"
/// "#;
/// let raw = parse_toml(src).unwrap();
/// let id = DefinitionId::parse("harwness.agent.my-worker@1").unwrap();
/// let layers = vec![(DefinitionLayer::BuiltIn, raw)];
/// let resolved = resolve_definition(&id, &layers, time::OffsetDateTime::now_utc()).unwrap();
/// let ir = lower(&resolved).unwrap();
/// assert_eq!(ir.specialization(), "my-worker");
/// ```
pub fn lower(resolved: &ResolvedAgentDefinition) -> Result<ExecutableAgentIr, DslError> {
    if resolved.specialization.is_empty() {
        return Err(DslError::Parse(format!(
            "cannot lower definition '{}': specialization field is empty (§10 requires non-empty specialization)",
            resolved.id,
        )));
    }

    // Build the IR without snapshot_id first; compute digest from content fields only,
    // then attach it. The placeholder SnapshotId("") is never exposed externally.
    // Die `[spawn.budget]`-Sektion wird als Ganzes gesucht: fehlt sie, bleibt
    // `budget` `None` ("die Definition macht keine Aussage"). Existiert sie,
    // entsteht ein BudgetSpec — auch wenn darin kein Feld auswertbar ist.
    let budget = config_table(&resolved.config, &["spawn", "spawn_contract"], "budget").map(
        |budget_table| BudgetSpec {
            max_tokens: table_u64(budget_table, "max_tokens"),
            max_tool_calls: table_u32(budget_table, "max_tool_calls"),
            max_wall_secs: table_u64(budget_table, "max_wall_secs"),
            effort_cap: table_string(budget_table, "effort_cap"),
        },
    );
    let spawn_contract = SpawnContract {
        workspace_hint: config_string(
            &resolved.config,
            &["spawn", "spawn_contract"],
            "workspace_hint",
        ),
        budget,
        max_depth: config_u32(&resolved.config, &["spawn", "spawn_contract"], "max_depth"),
        child_orchestrators: config_strings(
            &resolved.config,
            &["spawn", "spawn_contract"],
            "child_orchestrators",
        ),
    };
    let job_template = JobTemplate {
        goal_kind: config_string(&resolved.config, &["job", "job_template"], "goal_kind"),
    };
    let context_program = ContextProgram {
        context_policy: config_string(&resolved.config, &["context", "context_program"], "policy")
            .or_else(|| {
                config_string(
                    &resolved.config,
                    &["context", "context_program"],
                    "context_policy",
                )
            }),
        must_include: config_strings(
            &resolved.config,
            &["context", "context_program"],
            "must_include",
        ),
        exclude: config_strings(&resolved.config, &["context", "context_program"], "exclude"),
        // The free-form `[context]`/`[context_program]` config path has no
        // per-section detail grammar; only `from_resolved_program` populates
        // this field.
        section_detail: Vec::new(),
    };
    let tool_surface = ResolvedToolSurface {
        admitted: config_strings(&resolved.config, &["tools", "tool_surface"], "admitted"),
        forbidden: config_strings(&resolved.config, &["tools", "tool_surface"], "forbidden"),
    };
    let lifecycle_machine = LifecycleMachine {
        allow_pause: config_bool(
            &resolved.config,
            &["lifecycle", "lifecycle_machine"],
            "allow_pause",
        )
        .unwrap_or(false),
        allow_rerun: config_bool(
            &resolved.config,
            &["lifecycle", "lifecycle_machine"],
            "allow_rerun",
        )
        .unwrap_or(false),
        max_attempts: config_u32(
            &resolved.config,
            &["lifecycle", "lifecycle_machine"],
            "max_attempts",
        ),
    };
    let return_pipeline = ReturnPipeline {
        validators: config_strings(
            &resolved.config,
            &["return", "return_pipeline"],
            "validators",
        ),
        contract: config_string(&resolved.config, &["return", "return_pipeline"], "contract"),
    };

    let mut ir = ExecutableAgentIr {
        id: resolved.id.clone(),
        role: resolved.role,
        specialization: resolved.specialization.clone(),
        authority: resolved.authority.clone(),
        reasoning_effort: resolved.reasoning_effort.clone(),
        spawn_contract,
        job_template,
        context_program,
        tool_surface,
        lifecycle_machine,
        return_pipeline,
        trace: resolved.trace.clone(),
        // Populated below after content fields are finalized.
        snapshot_id: SnapshotId(String::new()),
    };
    // Compute and attach the snapshot ID over the finalized content fields.
    // snapshot_id itself and trace are excluded from the digest.
    ir.snapshot_id = compute_snapshot_id(&ir);
    Ok(ir)
}

/// Reads a string field from one of the supported executable-config tables.
/// Invalid TOML types are deliberately treated as unavailable: the resolved
/// type is a free-form compatibility boundary, while the executable IR is
/// strongly typed and must never manufacture values from malformed input.
fn config_string(config: &toml::Table, table_names: &[&str], field: &str) -> Option<String> {
    table_names
        .iter()
        .filter_map(|name| config.get(*name).and_then(toml::Value::as_table))
        .find_map(|table| {
            table
                .get(field)
                .and_then(toml::Value::as_str)
                .map(str::to_owned)
        })
}

/// Reads a string array from one of the supported executable-config tables.
fn config_strings(config: &toml::Table, table_names: &[&str], field: &str) -> Vec<String> {
    table_names
        .iter()
        .filter_map(|name| config.get(*name).and_then(toml::Value::as_table))
        .find_map(|table| {
            table
                .get(field)
                .and_then(toml::Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(toml::Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
        })
        .unwrap_or_default()
}

/// Reads a boolean field from one of the supported executable-config tables.
fn config_bool(config: &toml::Table, table_names: &[&str], field: &str) -> Option<bool> {
    table_names
        .iter()
        .filter_map(|name| config.get(*name).and_then(toml::Value::as_table))
        .find_map(|table| table.get(field).and_then(toml::Value::as_bool))
}

/// Liest eine vorzeichenlose 32-Bit-Ganzzahl aus einer der unterstützten
/// Executable-Config-Tabellen.
///
/// Negative Werte und Werte außerhalb des `u32`-Bereichs gelten wie ein
/// Typfehler als „nicht vorhanden" (`None`) — die IR erfindet aus fehlerhafter
/// Eingabe keinen Wert.
fn config_u32(config: &toml::Table, table_names: &[&str], field: &str) -> Option<u32> {
    table_names
        .iter()
        .filter_map(|name| config.get(*name).and_then(toml::Value::as_table))
        .find_map(|table| table_u32(table, field))
}

/// Liest eine verschachtelte Tabelle (z. B. `[spawn.budget]`) aus einer der
/// unterstützten Executable-Config-Tabellen.
///
/// Fehlt die äußere Sektion oder ist das Feld keine Tabelle, ist das Ergebnis
/// `None` — niemals eine Panik.
fn config_table<'a>(
    config: &'a toml::Table,
    table_names: &[&str],
    field: &str,
) -> Option<&'a toml::Table> {
    table_names
        .iter()
        .filter_map(|name| config.get(*name).and_then(toml::Value::as_table))
        .find_map(|table| table.get(field).and_then(toml::Value::as_table))
}

/// Liest ein String-Feld direkt aus einer bereits aufgelösten TOML-Tabelle.
fn table_string(table: &toml::Table, field: &str) -> Option<String> {
    table
        .get(field)
        .and_then(toml::Value::as_str)
        .map(str::to_owned)
}

/// Liest eine vorzeichenlose 64-Bit-Ganzzahl direkt aus einer TOML-Tabelle.
///
/// TOML kennt nur vorzeichenbehaftete 64-Bit-Ganzzahlen; negative Werte sind für
/// ein Budget bedeutungslos und werden deshalb wie ein Typfehler als `None`
/// behandelt.
fn table_u64(table: &toml::Table, field: &str) -> Option<u64> {
    table
        .get(field)
        .and_then(toml::Value::as_integer)
        .and_then(|raw| u64::try_from(raw).ok())
}

/// Liest eine vorzeichenlose 32-Bit-Ganzzahl direkt aus einer TOML-Tabelle.
///
/// Negative Werte und Werte oberhalb von [`u32::MAX`] gelten als `None`.
fn table_u32(table: &toml::Table, field: &str) -> Option<u32> {
    table
        .get(field)
        .and_then(toml::Value::as_integer)
        .and_then(|raw| u32::try_from(raw).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority::AuthorityCeiling;
    use crate::ids::{DefinitionId, Version};
    use crate::resolved::{ResolutionTrace, ResolvedAgentDefinition};
    use crate::roles::AgentRoleId;
    use crate::test_support::{TestError, TestResult, ctx};

    fn make_id(s: &str) -> TestResult<DefinitionId> {
        Ok(DefinitionId::parse(s)?)
    }

    fn make_version(s: &str) -> TestResult<Version> {
        Ok(Version(
            semver::Version::parse(s).map_err(ctx("semver::Version::parse should succeed"))?,
        ))
    }

    fn base_resolved(specialization: &str) -> TestResult<ResolvedAgentDefinition> {
        Ok(ResolvedAgentDefinition {
            id: make_id("harwness.agent.test-worker@1")?,
            version: make_version("1.0.0")?,
            role: AgentRoleId::Worker,
            specialization: specialization.to_owned(),
            name: None,
            description: None,
            reasoning_effort: None,
            authority: AuthorityCeiling {
                capabilities: vec!["filesystem.read".to_owned()],
            },
            trace: ResolutionTrace { steps: vec![] },
            config: toml::Table::new(),
        })
    }

    /// Wie [`base_resolved`], aber mit gesetztem `reasoning_effort` — für
    /// Tests des Durchreichens von `ResolvedAgentDefinition::reasoning_effort`
    /// bis in [`ExecutableAgentIr::reasoning_effort`].
    fn base_resolved_with_reasoning_effort(
        specialization: &str,
        reasoning_effort: Option<&str>,
    ) -> TestResult<ResolvedAgentDefinition> {
        Ok(ResolvedAgentDefinition {
            reasoning_effort: reasoning_effort.map(str::to_owned),
            ..base_resolved(specialization)?
        })
    }

    #[test]
    fn test_lower_produces_ir_with_matching_id_and_role() -> TestResult {
        let resolved = base_resolved("test-worker")?;
        let ir = lower(&resolved).map_err(ctx("lower should succeed"))?;
        assert_eq!(ir.id, make_id("harwness.agent.test-worker@1")?);
        assert_eq!(ir.role, AgentRoleId::Worker);
        Ok(())
    }

    #[test]
    fn test_lower_carries_specialization_through() -> TestResult {
        let resolved = base_resolved("focused-pure-coding")?;
        let ir = lower(&resolved).map_err(ctx("lower should succeed"))?;
        assert_eq!(ir.specialization, "focused-pure-coding");
        Ok(())
    }

    #[test]
    fn test_lower_maps_context_policy_when_present() {
        // Context policy is carried through the resolved free-form config table.
        let cp = ContextProgram {
            context_policy: Some("strict-isolation".to_owned()),
            ..ContextProgram::default()
        };
        assert_eq!(cp.context_policy.as_deref(), Some("strict-isolation"));
    }

    #[test]
    fn test_lower_maps_none_context_policy_when_absent() -> TestResult {
        let resolved = base_resolved("my-worker")?;
        let ir = lower(&resolved).map_err(ctx("lower should succeed"))?;
        assert!(ir.context_program.context_policy.is_none());
        Ok(())
    }

    #[test]
    fn test_lower_maps_context_program_table_policy() -> TestResult {
        let mut resolved = base_resolved("configured-worker")?;
        resolved.config = toml::from_str(
            r#"
            [context_program]
            policy = "strict-isolation"
            "#,
        )
        .map_err(ctx("fixture config should parse"))?;

        let ir = lower(&resolved).map_err(ctx("lower should succeed"))?;

        assert_eq!(
            ir.context_program().context_policy(),
            Some("strict-isolation")
        );
        Ok(())
    }

    #[test]
    fn test_lower_preserves_supported_executable_config() -> TestResult {
        let mut resolved = base_resolved("configured-worker")?;
        resolved.config = toml::from_str(
            r#"
            spawn = { workspace_hint = "workspace-a" }
            job = { goal_kind = "bounded-change" }
            context = { policy = "strict-isolation" }
            tools = { admitted = ["fs.read", "shell.exec"], forbidden = ["network.fetch"] }
            lifecycle = { allow_pause = true, allow_rerun = true }
            return = { validators = ["schema.v1", "redact.secrets"] }
            "#,
        )
        .map_err(ctx("fixture config should parse"))?;

        let ir = lower(&resolved).map_err(ctx("lower should succeed"))?;

        assert_eq!(
            ir.spawn_contract.workspace_hint.as_deref(),
            Some("workspace-a")
        );
        assert_eq!(ir.job_template.goal_kind.as_deref(), Some("bounded-change"));
        assert_eq!(
            ir.context_program.context_policy.as_deref(),
            Some("strict-isolation")
        );
        assert_eq!(ir.tool_surface.admitted, ["fs.read", "shell.exec"]);
        assert_eq!(ir.tool_surface.forbidden, ["network.fetch"]);
        assert!(ir.lifecycle_machine.allow_pause);
        assert!(ir.lifecycle_machine.allow_rerun);
        assert_eq!(
            ir.return_pipeline.validators,
            ["schema.v1", "redact.secrets"]
        );
        Ok(())
    }

    #[test]
    fn lower_carries_exact_child_orchestrator_grants() -> TestResult {
        let mut resolved = base_resolved("manager")?;
        resolved.config = toml::toml! {
            [spawn]
            child_orchestrators = ["specialist", "reviewer"]
        };

        let ir = lower(&resolved).map_err(ctx("spawn grant lowers"))?;
        assert!(ir.spawn_contract().permits_child_orchestrator("specialist"));
        assert!(ir.spawn_contract().permits_child_orchestrator("reviewer"));
        assert!(!ir.spawn_contract().permits_child_orchestrator("other"));
        Ok(())
    }

    #[test]
    fn test_lower_populates_tool_surface_from_admitted_forbidden() -> TestResult {
        // In this wave, tool surface fields are absent from ResolvedAgentDefinition.
        // The IR defaults to empty vecs; confirm no panic and correct defaults.
        let resolved = base_resolved("my-worker")?;
        let ir = lower(&resolved).map_err(ctx("lower should succeed"))?;
        assert!(ir.tool_surface.admitted.is_empty());
        assert!(ir.tool_surface.forbidden.is_empty());
        Ok(())
    }

    #[test]
    fn test_lower_returns_parse_error_when_specialization_empty() -> TestResult {
        let resolved = base_resolved("")?;
        let result = lower(&resolved);
        assert!(
            result.is_err(),
            "lower should fail when specialization is empty"
        );
        let Err(err) = result else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(
            matches!(err, DslError::Parse(_)),
            "error should be DslError::Parse, got: {err}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("specialization"),
            "error message should mention 'specialization', got: {msg}"
        );
        Ok(())
    }

    #[test]
    fn test_lower_carries_authority_through_unchanged() -> TestResult {
        let mut resolved = base_resolved("x")?;
        resolved.authority = AuthorityCeiling {
            capabilities: vec![
                "process.spawn.sandboxed".to_owned(),
                "filesystem.read".to_owned(),
            ],
        };
        let ir = lower(&resolved).map_err(ctx("lower should succeed"))?;
        assert_eq!(ir.authority, resolved.authority);
        Ok(())
    }

    #[test]
    fn test_lower_carries_trace_through_unchanged() -> TestResult {
        use crate::resolved::ResolutionStep;
        let mut resolved = base_resolved("my-worker")?;
        resolved.trace = ResolutionTrace {
            steps: vec![ResolutionStep {
                source: "harwness.agent.worker-base@1".to_owned(),
                kind: "base".to_owned(),
                applied_at: time::OffsetDateTime::now_utc(),
            }],
        };
        let ir = lower(&resolved).map_err(ctx("lower should succeed"))?;
        assert_eq!(ir.trace.steps.len(), 1);
        assert_eq!(ir.trace.steps[0].source, "harwness.agent.worker-base@1");
        Ok(())
    }

    #[test]
    fn test_lower_role_orchestrator_is_preserved() -> TestResult {
        let mut resolved = base_resolved("orchestrator-spec")?;
        resolved.role = AgentRoleId::RootOrchestrator;
        resolved.id = make_id("harwness.agent.root-orch@2")?;
        let ir = lower(&resolved).map_err(ctx("lower should succeed"))?;
        assert_eq!(ir.role, AgentRoleId::RootOrchestrator);
        Ok(())
    }

    #[test]
    fn test_lifecycle_machine_defaults_to_no_pause_no_rerun() -> TestResult {
        let resolved = base_resolved("basic")?;
        let ir = lower(&resolved).map_err(ctx("lower should succeed"))?;
        assert!(!ir.lifecycle_machine.allow_pause);
        assert!(!ir.lifecycle_machine.allow_rerun);
        Ok(())
    }

    #[test]
    fn test_return_pipeline_defaults_to_empty_validators() -> TestResult {
        let resolved = base_resolved("basic")?;
        let ir = lower(&resolved).map_err(ctx("lower should succeed"))?;
        assert!(ir.return_pipeline.validators.is_empty());
        Ok(())
    }

    #[test]
    fn snapshot_id_is_deterministic_across_lower_calls() -> TestResult {
        // Lower the same ResolvedAgentDefinition twice; both SnapshotIds must be equal.
        let resolved = base_resolved("determinism-check")?;
        let ir1 = lower(&resolved).map_err(ctx("first lower should succeed"))?;
        let ir2 = lower(&resolved).map_err(ctx("second lower should succeed"))?;
        assert_eq!(
            ir1.snapshot_id(),
            ir2.snapshot_id(),
            "SnapshotId must be deterministic: same IR content → same digest"
        );
        // Also verify the SnapshotId is a 64-char hex string (BLAKE3 output)
        assert_eq!(
            ir1.snapshot_id.0.len(),
            64,
            "BLAKE3 hex digest should be 64 chars"
        );
        assert!(
            ir1.snapshot_id.0.chars().all(|c| c.is_ascii_hexdigit()),
            "SnapshotId should be lowercase hex"
        );
        Ok(())
    }

    #[test]
    fn snapshot_id_changes_when_role_changes() -> TestResult {
        // Two defs differing only in role must produce different SnapshotIds.
        let mut resolved_worker = base_resolved("role-test")?;
        resolved_worker.role = AgentRoleId::Worker;
        resolved_worker.id = make_id("harwness.agent.role-test-worker@1")?;

        let mut resolved_orch = base_resolved("role-test")?;
        resolved_orch.role = AgentRoleId::RootOrchestrator;
        resolved_orch.id = make_id("harwness.agent.role-test-orch@1")?;

        let ir_worker = lower(&resolved_worker).map_err(ctx("worker lower should succeed"))?;
        let ir_orch = lower(&resolved_orch).map_err(ctx("orch lower should succeed"))?;

        assert_ne!(
            ir_worker.snapshot_id(),
            ir_orch.snapshot_id(),
            "Different roles must yield different SnapshotIds"
        );
        Ok(())
    }

    #[test]
    fn snapshot_id_accessor_matches_field() -> TestResult {
        let resolved = base_resolved("accessor-test")?;
        let ir = lower(&resolved).map_err(ctx("lower should succeed"))?;
        assert_eq!(ir.snapshot_id(), ir.snapshot_id.clone());
        Ok(())
    }

    #[test]
    fn snapshot_id_serde_round_trip_confirms_same_identity() -> TestResult {
        // Der wichtigste Test: Serialize -> JSON -> ReferencedSnapshotId ->
        // confirm() muss dieselbe Identität zurückliefern wie das Original.
        let resolved = base_resolved("serde-roundtrip")?;
        let ir = lower(&resolved).map_err(ctx("lower should succeed"))?;
        let original = ir.snapshot_id();

        let json = serde_json::to_string(&original).map_err(ctx("SnapshotId must serialize"))?;
        #[derive(serde::Deserialize)]
        struct Wire {
            domain: String,
            digest: String,
        }
        let wire: Wire = serde_json::from_str(&json).map_err(ctx("wire form must deserialize"))?;
        assert_eq!(
            wire.domain, SNAPSHOT_HASH_DOMAIN,
            "serialized form must carry the current hash-domain version"
        );

        let referenced = ReferencedSnapshotId::parse(wire.domain, wire.digest)
            .map_err(ctx("well-formed wire values must parse"))?;
        let confirmed = referenced.confirm(&original).ok_or(TestError::Unexpected(
            "a serde round trip must not change the identity".into(),
        ))?;
        assert_eq!(
            confirmed, original,
            "confirmed identity must equal the original SnapshotId"
        );
        Ok(())
    }

    #[test]
    fn referenced_snapshot_id_from_other_domain_is_recognizable_and_unconfirmed() -> TestResult {
        let resolved = base_resolved("foreign-domain")?;
        let ir = lower(&resolved).map_err(ctx("lower should succeed"))?;
        let computed = ir.snapshot_id();

        // Gleicher Digest-Text, aber eine ältere Domänenfassung behauptet.
        let foreign =
            ReferencedSnapshotId::parse("harwness.executable-ir.snapshot/v2", computed.to_string())
                .map_err(ctx("well-formed digest must parse regardless of domain"))?;

        assert!(
            !foreign.is_current_domain(),
            "a v2-tagged reference must not report itself as current-domain"
        );
        assert!(
            foreign.confirm(&computed).is_none(),
            "a reference from a foreign hash-domain must never confirm, even with a matching digest"
        );
        Ok(())
    }

    #[test]
    fn snapshot_id_stable_across_repeated_lowering_via_referenced_confirm() -> TestResult {
        // Stabilität über wiederholtes Absenken bleibt auch über den
        // Referenz/Bestätigungs-Weg erhalten, nicht nur bei direktem Vergleich.
        let resolved = base_resolved("repeated-lowering")?;
        let first = lower(&resolved)
            .map_err(ctx("first lower should succeed"))?
            .snapshot_id();
        let second = lower(&resolved)
            .map_err(ctx("second lower should succeed"))?
            .snapshot_id();
        let third = lower(&resolved)
            .map_err(ctx("third lower should succeed"))?
            .snapshot_id();

        let referenced_from_first =
            ReferencedSnapshotId::parse(SNAPSHOT_HASH_DOMAIN, first.to_string())
                .map_err(ctx("digest from a real SnapshotId is always well-formed"))?;

        assert_eq!(
            referenced_from_first.confirm(&second),
            referenced_from_first.confirm(&third),
            "a reference to a stable digest must confirm identically across repeated lowerings"
        );
        assert!(referenced_from_first.confirm(&second).is_some());
        Ok(())
    }

    #[test]
    fn referenced_snapshot_id_rejects_malformed_values() {
        // Falsche Länge.
        assert!(matches!(
            ReferencedSnapshotId::parse(SNAPSHOT_HASH_DOMAIN, "too-short"),
            Err(DslError::Parse(_))
        ));
        // Richtige Länge, aber nicht-hex Zeichen.
        let not_hex = "z".repeat(64);
        assert!(matches!(
            ReferencedSnapshotId::parse(SNAPSHOT_HASH_DOMAIN, not_hex),
            Err(DslError::Parse(_))
        ));
        // Richtige Länge und Alphabet, aber Großbuchstaben-Hex.
        let upper_hex = "A".repeat(64);
        assert!(matches!(
            ReferencedSnapshotId::parse(SNAPSHOT_HASH_DOMAIN, upper_hex),
            Err(DslError::Parse(_))
        ));
        // Leere Domäne.
        let valid_digest = "a".repeat(64);
        assert!(matches!(
            ReferencedSnapshotId::parse("", valid_digest),
            Err(DslError::Parse(_))
        ));
    }

    #[test]
    fn snapshot_id_display_is_hex_string() -> TestResult {
        let resolved = base_resolved("display-test")?;
        let ir = lower(&resolved).map_err(ctx("lower should succeed"))?;
        let displayed = ir.snapshot_id.to_string();
        assert_eq!(displayed, ir.snapshot_id.0);
        Ok(())
    }

    #[test]
    fn integrity_bearing_fields_are_read_only_through_public_api() -> TestResult {
        let ir = lower(&base_resolved("read-only-test")?).map_err(ctx("lower should succeed"))?;
        let original_snapshot = ir.snapshot_id();

        assert_eq!(ir.id(), &make_id("harwness.agent.test-worker@1")?);
        assert_eq!(ir.role(), AgentRoleId::Worker);
        assert_eq!(ir.specialization(), "read-only-test");
        assert_eq!(ir.authority().capabilities, ["filesystem.read"]);
        assert_eq!(ir.spawn_contract().workspace_hint(), None);
        assert_eq!(ir.job_template().goal_kind(), None);
        assert_eq!(ir.context_program().context_policy(), None);
        assert!(ir.tool_surface().admitted().is_empty());
        assert!(!ir.lifecycle_machine().allow_pause());
        assert!(ir.return_pipeline().validators().is_empty());

        // The content fields are private, so callers cannot mutate them through
        // these borrowed views and leave `original_snapshot` stale.
        assert_eq!(ir.snapshot_id(), original_snapshot);
        Ok(())
    }

    /// Vollständige TOML-Fixture mit allen in Wave W1-27 ergänzten Feldern.
    const READ_ONLY_CHILD_TOML: &str = r#"
        [spawn]
        workspace_hint = "workspace-a"
        max_depth = 0

        [spawn.budget]
        max_tokens = 120000
        max_tool_calls = 64
        max_wall_secs = 900
        effort_cap = "medium"

        [job]
        goal_kind = "read-only-research"

        [context]
        policy = "strict-isolation"
        must_include = ["mission.md", "read-list.md"]
        exclude = ["secrets/**", ".env"]

        [tools]
        admitted = ["fs.read", "search.grep"]
        forbidden = ["fs.write", "shell.exec"]

        [lifecycle]
        allow_pause = false
        allow_rerun = true
        max_attempts = 2

        [return]
        validators = ["schema.v1"]
        contract = "harwness.return.research-finding@1"
    "#;

    /// Parst eine TOML-Fixture in die freiformige Config-Tabelle.
    fn parse_config(src: &str) -> TestResult<toml::Table> {
        toml::from_str(src).map_err(ctx("fixture config should parse"))
    }

    #[test]
    fn test_lower_maps_full_read_only_child_contract() -> TestResult {
        let mut resolved = base_resolved("explorer")?;
        resolved.config = parse_config(READ_ONLY_CHILD_TOML)?;

        let ir = lower(&resolved).map_err(ctx("lower should succeed"))?;

        // SpawnContract inkl. Budget und Tiefe.
        assert_eq!(ir.spawn_contract().workspace_hint(), Some("workspace-a"));
        assert_eq!(
            ir.spawn_contract().max_depth(),
            Some(0),
            "max_depth = 0 must survive as a hard 'no children' cap, not as 'unset'"
        );
        let budget = ir
            .spawn_contract()
            .budget()
            .ok_or(TestError::Missing("budget section should lower to Some"))?;
        assert_eq!(budget.max_tokens(), Some(120_000));
        assert_eq!(budget.max_tool_calls(), Some(64));
        assert_eq!(budget.max_wall_secs(), Some(900));
        assert_eq!(budget.effort_cap(), Some("medium"));

        // ContextProgram inkl. Selektorlisten (reihenfolgetreu).
        assert_eq!(
            ir.context_program().context_policy(),
            Some("strict-isolation")
        );
        assert_eq!(
            ir.context_program().must_include(),
            ["mission.md", "read-list.md"].as_slice()
        );
        assert_eq!(
            ir.context_program().exclude(),
            ["secrets/**", ".env"].as_slice()
        );

        // LifecycleMachine: Pause gesperrt, Versuche begrenzt.
        assert!(!ir.lifecycle_machine().allow_pause());
        assert!(ir.lifecycle_machine().allow_rerun());
        assert_eq!(ir.lifecycle_machine().max_attempts(), Some(2));

        // ReturnPipeline inkl. Rückgabevertrag.
        assert_eq!(ir.return_pipeline().validators(), ["schema.v1"].as_slice());
        assert_eq!(
            ir.return_pipeline().contract(),
            Some("harwness.return.research-finding@1")
        );

        // Tool-Surface bleibt unverändert erhalten.
        assert_eq!(
            ir.tool_surface().admitted(),
            ["fs.read", "search.grep"].as_slice()
        );
        assert_eq!(
            ir.tool_surface().forbidden(),
            ["fs.write", "shell.exec"].as_slice()
        );
        Ok(())
    }

    #[test]
    fn test_lower_leaves_new_fields_unset_for_empty_config() -> TestResult {
        let ir = lower(&base_resolved("bare")?).map_err(ctx("lower should succeed"))?;

        assert!(ir.spawn_contract().budget().is_none());
        assert_eq!(ir.spawn_contract().max_depth(), None);
        assert!(ir.context_program().must_include().is_empty());
        assert!(ir.context_program().exclude().is_empty());
        assert_eq!(ir.lifecycle_machine().max_attempts(), None);
        assert_eq!(ir.return_pipeline().contract(), None);
        Ok(())
    }

    #[test]
    fn test_lower_rejects_malformed_budget_values_without_panicking() -> TestResult {
        // Negative und überlaufende Ganzzahlen sowie falsche Typen sind keine
        // gültigen Budgets: sie werden wie „nicht gesetzt" behandelt, statt
        // einen Wert zu erfinden oder zu panieren.
        let mut resolved = base_resolved("malformed-budget")?;
        resolved.config = parse_config(
            r#"
            [spawn]
            max_depth = -1

            [spawn.budget]
            max_tokens = -5
            max_tool_calls = 9999999999
            max_wall_secs = "sehr lange"
            effort_cap = 3
            "#,
        )?;

        let ir = lower(&resolved).map_err(ctx("lower should succeed"))?;

        assert_eq!(ir.spawn_contract().max_depth(), None);
        let budget = ir.spawn_contract().budget().ok_or(TestError::Missing(
            "declared budget section stays Some even when unusable",
        ))?;
        assert_eq!(
            budget.max_tokens(),
            None,
            "negative token cap is not a budget"
        );
        assert_eq!(
            budget.max_tool_calls(),
            None,
            "value above u32::MAX is not a tool-call cap"
        );
        assert_eq!(budget.max_wall_secs(), None);
        assert_eq!(budget.effort_cap(), None);
        Ok(())
    }

    #[test]
    fn snapshot_id_changes_when_budget_max_tokens_changes() -> TestResult {
        // Zwei IRs, die sich ausschließlich in spawn.budget.max_tokens
        // unterscheiden, dürfen niemals dieselbe Adresse tragen.
        let mut low = base_resolved("budget-sensitivity")?;
        low.config = parse_config(READ_ONLY_CHILD_TOML)?;

        let mut high = base_resolved("budget-sensitivity")?;
        high.config = parse_config(
            &READ_ONLY_CHILD_TOML.replace("max_tokens = 120000", "max_tokens = 120001"),
        )?;

        let ir_low = lower(&low).map_err(ctx("lower should succeed"))?;
        let ir_high = lower(&high).map_err(ctx("lower should succeed"))?;

        assert_eq!(
            ir_low
                .spawn_contract()
                .budget()
                .and_then(BudgetSpec::max_tokens),
            Some(120_000)
        );
        assert_eq!(
            ir_high
                .spawn_contract()
                .budget()
                .and_then(BudgetSpec::max_tokens),
            Some(120_001)
        );
        assert_ne!(
            ir_low.snapshot_id(),
            ir_high.snapshot_id(),
            "a different token budget is different content and must hash differently"
        );
        Ok(())
    }

    #[test]
    fn snapshot_id_distinguishes_absent_budget_from_empty_budget() -> TestResult {
        // Eine deklarierte, aber leere [spawn.budget]-Sektion ist eine andere
        // Aussage als eine fehlende Sektion — das Präsenz-Byte muss das zeigen.
        let mut absent = base_resolved("budget-presence")?;
        absent.config = parse_config(
            r#"
            [spawn]
            workspace_hint = "w"
            "#,
        )?;

        let mut empty = base_resolved("budget-presence")?;
        empty.config = parse_config(
            r#"
            [spawn]
            workspace_hint = "w"

            [spawn.budget]
            "#,
        )?;

        let ir_absent = lower(&absent).map_err(ctx("lower should succeed"))?;
        let ir_empty = lower(&empty).map_err(ctx("lower should succeed"))?;

        assert!(ir_absent.spawn_contract().budget().is_none());
        assert!(ir_empty.spawn_contract().budget().is_some());
        assert_ne!(
            ir_absent.snapshot_id(),
            ir_empty.snapshot_id(),
            "declared-but-empty budget must not alias with an absent budget"
        );
        Ok(())
    }

    #[test]
    fn snapshot_id_changes_when_return_contract_changes() -> TestResult {
        let mut with_contract = base_resolved("contract-sensitivity")?;
        with_contract.config = parse_config(
            r#"
            [return]
            contract = "harwness.return.research-finding@1"
            "#,
        )?;

        let without_contract = base_resolved("contract-sensitivity")?;

        let ir_with = lower(&with_contract).map_err(ctx("lower should succeed"))?;
        let ir_without = lower(&without_contract).map_err(ctx("lower should succeed"))?;

        assert_ne!(
            ir_with.snapshot_id(),
            ir_without.snapshot_id(),
            "the return contract is part of the addressed content"
        );
        Ok(())
    }

    #[test]
    fn snapshot_id_changes_when_lifecycle_max_attempts_changes() -> TestResult {
        let mut two = base_resolved("attempt-sensitivity")?;
        two.config = parse_config(
            r#"
            [lifecycle]
            max_attempts = 2
            "#,
        )?;

        let mut three = base_resolved("attempt-sensitivity")?;
        three.config = parse_config(
            r#"
            [lifecycle]
            max_attempts = 3
            "#,
        )?;

        let ir_two = lower(&two).map_err(ctx("lower should succeed"))?;
        let ir_three = lower(&three).map_err(ctx("lower should succeed"))?;

        assert_ne!(ir_two.snapshot_id(), ir_three.snapshot_id());
        Ok(())
    }

    #[test]
    fn snapshot_id_changes_when_context_selectors_are_reordered() -> TestResult {
        // must_include wird nicht normalisiert; die Reihenfolge ist Inhalt und
        // muss daher den Digest beeinflussen.
        let mut forward = base_resolved("selector-order")?;
        forward.config = parse_config(
            r#"
            [context]
            must_include = ["a.md", "b.md"]
            "#,
        )?;

        let mut reversed = base_resolved("selector-order")?;
        reversed.config = parse_config(
            r#"
            [context]
            must_include = ["b.md", "a.md"]
            "#,
        )?;

        let ir_forward = lower(&forward).map_err(ctx("lower should succeed"))?;
        let ir_reversed = lower(&reversed).map_err(ctx("lower should succeed"))?;

        assert_ne!(
            ir_forward.snapshot_id(),
            ir_reversed.snapshot_id(),
            "unnormalized list order must not be erased by the digest"
        );
        Ok(())
    }

    #[test]
    fn snapshot_id_is_deterministic_for_extended_config() -> TestResult {
        // Unveränderte Config über zwei lower()-Aufrufe → identische Adresse.
        let mut resolved = base_resolved("determinism-extended")?;
        resolved.config = parse_config(READ_ONLY_CHILD_TOML)?;

        let first = lower(&resolved).map_err(ctx("first lower should succeed"))?;
        let second = lower(&resolved).map_err(ctx("second lower should succeed"))?;

        assert_eq!(
            first.snapshot_id(),
            second.snapshot_id(),
            "identical extended config must produce an identical SnapshotId"
        );
        assert_eq!(first.snapshot_id.0.len(), 64);
        Ok(())
    }

    #[test]
    fn changed_content_gets_a_new_snapshot_id() -> TestResult {
        let unchanged =
            lower(&base_resolved("mutation-test")?).map_err(ctx("lower should succeed"))?;
        let changed =
            lower(&base_resolved("changed-mutation-test")?).map_err(ctx("lower should succeed"))?;

        assert_ne!(
            unchanged.snapshot_id(),
            changed.snapshot_id(),
            "content changes must never retain an old snapshot ID"
        );
        Ok(())
    }

    // -------------------------------------------------------------------
    // ContextProgram::from_resolved_program (Knoten AW2-01 follow-up:
    // the missing link from a declared `harwness.context.<name>@<v>`
    // program to the executable IR).
    // -------------------------------------------------------------------

    /// Builds a permissive `ContextCeiling` fixture via `serde_json`, avoiding
    /// a direct dependency on `harw-lens-types` (needed only to name
    /// `harw_lens_types::BudgetSpec` for a struct literal) — the budget is
    /// never consulted by `admits_program`, so its exact value is immaterial.
    fn permissive_ceiling(sections: &[&str]) -> TestResult<harw_context::ContextCeiling> {
        let json = serde_json::json!({
            "sections": sections,
            "max_trust": "instruction",
            "budget": { "total": { "total": 1_000_000 }, "per_section": {} },
        });
        serde_json::from_value(json).map_err(ctx("ceiling fixture should deserialize"))
    }

    fn context_section(
        name: &str,
        strength: crate::context_program::SectionStrength,
    ) -> crate::context_program::RawContextSectionSpec {
        crate::context_program::RawContextSectionSpec {
            name: name.to_owned(),
            strength,
            detail: harw_context::DetailMode::Summary,
            trust: harw_context::TrustClass::Data,
        }
    }

    fn resolved_program(
        id_str: &str,
        sections: Vec<crate::context_program::RawContextSectionSpec>,
        exclude: Vec<String>,
    ) -> TestResult<crate::context_program::ResolvedContextProgramDefinition> {
        Ok(crate::context_program::ResolvedContextProgramDefinition {
            id: make_id(id_str)?,
            version: make_version("1.0.0")?,
            sections,
            exclude,
            trace: ResolutionTrace { steps: vec![] },
        })
    }

    #[test]
    fn from_resolved_program_maps_must_include_sections_only() -> TestResult {
        let ceiling = permissive_ceiling(&["goal.invariants", "history.tail"])?;
        let program = resolved_program(
            "harwness.context.mapping-test@1",
            vec![
                context_section(
                    "goal.invariants",
                    crate::context_program::SectionStrength::MustInclude,
                ),
                context_section(
                    "history.tail",
                    crate::context_program::SectionStrength::Normal,
                ),
            ],
            vec!["memory.*".to_owned()],
        )?;

        let cp = ContextProgram::from_resolved_program(&program, &ceiling)
            .map_err(ctx("program should be admitted"))?;

        assert_eq!(
            cp.must_include().to_vec(),
            vec!["goal.invariants".to_owned()],
            "only the must-include-strength section should populate must_include"
        );
        assert_eq!(cp.exclude().to_vec(), vec!["memory.*".to_owned()]);
        assert_eq!(cp.context_policy(), Some("harwness.context.mapping-test@1"));
        Ok(())
    }

    #[test]
    fn from_resolved_program_rejects_section_outside_ceiling() -> TestResult {
        let ceiling = permissive_ceiling(&["history.tail"])?;
        let program = resolved_program(
            "harwness.context.overreaching@1",
            vec![context_section(
                "plan.other-clans",
                crate::context_program::SectionStrength::Normal,
            )],
            vec![],
        )?;

        let result = ContextProgram::from_resolved_program(&program, &ceiling);
        assert!(
            result.is_err(),
            "a program requiring a section outside the ceiling must be rejected, not silently pruned"
        );
        Ok(())
    }

    #[test]
    fn from_resolved_program_same_program_same_snapshot_changed_program_differs() -> TestResult {
        // Proves the guarantee this follow-up must not break (see
        // `crate::context_program`, "Das Programm bleibt Teil der SnapshotId"):
        // the same resolved context program yields the same SnapshotId; a
        // changed one yields a different SnapshotId. `compute_snapshot_id` and
        // `ContextProgram`'s field set are both untouched by this addition —
        // this test recomputes the digest directly (bypassing the cached
        // `ir.snapshot_id`, which `lower()` only ever sets once) to observe the
        // effect of swapping in a freshly-built `ContextProgram`.
        let ceiling = permissive_ceiling(&["goal.invariants", "history.tail"])?;

        let program_a = resolved_program(
            "harwness.context.snap-a@1",
            vec![context_section(
                "goal.invariants",
                crate::context_program::SectionStrength::MustInclude,
            )],
            vec![],
        )?;
        let program_b = resolved_program(
            "harwness.context.snap-b@1",
            vec![context_section(
                "history.tail",
                crate::context_program::SectionStrength::MustInclude,
            )],
            vec![],
        )?;

        let cp_a1 = ContextProgram::from_resolved_program(&program_a, &ceiling)
            .map_err(ctx("program_a should be admitted"))?;
        let cp_a2 = ContextProgram::from_resolved_program(&program_a, &ceiling)
            .map_err(ctx("program_a should be admitted"))?;
        let cp_b = ContextProgram::from_resolved_program(&program_b, &ceiling)
            .map_err(ctx("program_b should be admitted"))?;

        let mut ir =
            lower(&base_resolved("snapshot-target")?).map_err(ctx("lower should succeed"))?;

        ir.context_program = cp_a1;
        let snap_a1 = compute_snapshot_id(&ir);

        ir.context_program = cp_a2;
        let snap_a2 = compute_snapshot_id(&ir);

        ir.context_program = cp_b;
        let snap_b = compute_snapshot_id(&ir);

        assert_eq!(
            snap_a1, snap_a2,
            "the same resolved context program must yield the same SnapshotId"
        );
        assert_ne!(
            snap_a1, snap_b,
            "a changed resolved context program must yield a different SnapshotId"
        );
        Ok(())
    }

    // -------------------------------------------------------------------
    // section_detail (Folgeknoten zu AW2-01, "Punkt 4"): der DetailMode je
    // Sektion überlebt das Absenken in die IR.
    // -------------------------------------------------------------------

    fn context_section_with_detail(
        name: &str,
        strength: crate::context_program::SectionStrength,
        detail: harw_context::DetailMode,
    ) -> crate::context_program::RawContextSectionSpec {
        crate::context_program::RawContextSectionSpec {
            name: name.to_owned(),
            strength,
            detail,
            trust: harw_context::TrustClass::Data,
        }
    }

    #[test]
    fn from_resolved_program_carries_detail_mode_for_every_section_regardless_of_strength()
    -> TestResult {
        let ceiling = permissive_ceiling(&["goal.invariants", "history.tail", "plan.current"])?;
        let program = resolved_program(
            "harwness.context.detail-test@1",
            vec![
                context_section_with_detail(
                    "goal.invariants",
                    crate::context_program::SectionStrength::MustInclude,
                    harw_context::DetailMode::Full,
                ),
                context_section_with_detail(
                    "history.tail",
                    crate::context_program::SectionStrength::Normal,
                    harw_context::DetailMode::Summary,
                ),
                context_section_with_detail(
                    "plan.current",
                    crate::context_program::SectionStrength::MustInclude,
                    harw_context::DetailMode::References,
                ),
            ],
            vec![],
        )?;

        let cp = ContextProgram::from_resolved_program(&program, &ceiling)
            .map_err(ctx("program should be admitted"))?;

        let details: Vec<(&str, harw_context::DetailMode)> = cp
            .section_detail()
            .iter()
            .map(|entry| (entry.name(), entry.detail()))
            .collect();

        assert_eq!(
            details,
            vec![
                ("goal.invariants", harw_context::DetailMode::Full),
                ("history.tail", harw_context::DetailMode::Summary),
                ("plan.current", harw_context::DetailMode::References),
            ],
            "section_detail must carry every section (including Normal-strength ones) \
             in declaration order, unlike must_include which filters by strength"
        );
        Ok(())
    }

    #[test]
    fn lower_from_config_path_never_populates_section_detail() -> TestResult {
        // The `[context]`/`[context_program]` free-form config path (used by
        // `lower`) has no per-section detail grammar; only
        // `from_resolved_program` (declared `harwness.context.<name>@<v>`
        // programs) can populate `section_detail`.
        let mut resolved = base_resolved("config-path-worker")?;
        resolved.config = parse_config(
            r#"
            [context]
            must_include = ["mission.md"]
            "#,
        )?;
        let ir = lower(&resolved).map_err(ctx("lower should succeed"))?;
        assert!(ir.context_program().section_detail().is_empty());
        Ok(())
    }

    #[test]
    fn snapshot_id_changes_when_only_a_section_detail_mode_changes() -> TestResult {
        // Two programs identical except for one section's DetailMode must
        // never alias to the same SnapshotId.
        let ceiling = permissive_ceiling(&["history.tail"])?;

        let program_summary = resolved_program(
            "harwness.context.detail-sensitivity@1",
            vec![context_section_with_detail(
                "history.tail",
                crate::context_program::SectionStrength::Normal,
                harw_context::DetailMode::Summary,
            )],
            vec![],
        )?;
        let program_full = resolved_program(
            "harwness.context.detail-sensitivity@1",
            vec![context_section_with_detail(
                "history.tail",
                crate::context_program::SectionStrength::Normal,
                harw_context::DetailMode::Full,
            )],
            vec![],
        )?;

        let cp_summary = ContextProgram::from_resolved_program(&program_summary, &ceiling)
            .map_err(ctx("program should be admitted"))?;
        let cp_full = ContextProgram::from_resolved_program(&program_full, &ceiling)
            .map_err(ctx("program should be admitted"))?;

        let mut ir = lower(&base_resolved("detail-sensitivity-target")?)
            .map_err(ctx("lower should succeed"))?;

        ir.context_program = cp_summary;
        let snap_summary = compute_snapshot_id(&ir);

        ir.context_program = cp_full;
        let snap_full = compute_snapshot_id(&ir);

        assert_ne!(
            snap_summary, snap_full,
            "a changed DetailMode on an otherwise identical section must change the SnapshotId"
        );
        Ok(())
    }

    #[test]
    fn snapshot_id_is_stable_across_repeated_lowering_with_section_detail_present() -> TestResult {
        // Mirrors `snapshot_id_is_deterministic_across_lower_calls` but with a
        // populated `section_detail`, proving repeated lowering stays stable
        // for the new field too (not just a fixed literal digest, per the
        // node's brief: no golden hash-value test exists in this module).
        let ceiling = permissive_ceiling(&["history.tail"])?;
        let program = resolved_program(
            "harwness.context.stability-check@1",
            vec![context_section_with_detail(
                "history.tail",
                crate::context_program::SectionStrength::Normal,
                harw_context::DetailMode::References,
            )],
            vec![],
        )?;
        let cp = ContextProgram::from_resolved_program(&program, &ceiling)
            .map_err(ctx("program should be admitted"))?;

        let mut ir1 =
            lower(&base_resolved("stability-check")?).map_err(ctx("lower should succeed"))?;
        ir1.context_program = cp.clone();
        let snap1 = compute_snapshot_id(&ir1);

        let mut ir2 =
            lower(&base_resolved("stability-check")?).map_err(ctx("lower should succeed"))?;
        ir2.context_program = cp;
        let snap2 = compute_snapshot_id(&ir2);

        assert_eq!(
            snap1, snap2,
            "identical section_detail content must produce an identical SnapshotId across repeated lowering"
        );
        Ok(())
    }

    // -----------------------------------------------------------------------
    // reasoning_effort: Durchreichen von ResolvedAgentDefinition in
    // ExecutableAgentIr (Vererbungsregel selbst ist Sache von `resolve.rs`).
    // -----------------------------------------------------------------------

    #[test]
    fn test_lower_passes_through_absent_reasoning_effort() -> TestResult {
        let resolved = base_resolved_with_reasoning_effort("worker", None)?;
        let ir = lower(&resolved).map_err(ctx("lower should succeed"))?;
        assert_eq!(ir.reasoning_effort(), None);
        Ok(())
    }

    #[test]
    fn test_lower_passes_through_set_reasoning_effort() -> TestResult {
        let resolved = base_resolved_with_reasoning_effort("worker", Some("high"))?;
        let ir = lower(&resolved).map_err(ctx("lower should succeed"))?;
        assert_eq!(ir.reasoning_effort(), Some("high"));
        Ok(())
    }

    #[test]
    fn test_snapshot_id_differs_when_reasoning_effort_differs() -> TestResult {
        let ir1 = lower(&base_resolved_with_reasoning_effort("worker", Some("low"))?)
            .map_err(ctx("lower should succeed"))?;
        let ir2 = lower(&base_resolved_with_reasoning_effort(
            "worker",
            Some("high"),
        )?)
        .map_err(ctx("lower should succeed"))?;
        assert_ne!(
            ir1.snapshot_id(),
            ir2.snapshot_id(),
            "reasoning_effort participates in the content hash (v5)"
        );
        Ok(())
    }
}
