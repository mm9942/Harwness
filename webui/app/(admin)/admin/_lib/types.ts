// Fachliche Typen der Verwaltungsfläche (Knoten UI-07).
//
// # Herkunft
// Diese Typen sind TypeScript-Spiegelbilder von Rust-Typen aus
// `harw-model-catalog` (`descriptor.rs`, `runtime.rs`, `observed.rs`,
// `spec.rs`, `provenance.rs`), `harw-knowledge::model_behavior_proposal`
// und `harw-context::ceiling`/`harw-agent-dsl::{context_program,family,
// executable}`. Sie sind **ungeprüft gegen ein tatsächliches Wire-Format**,
// weil `WEB_ROUTES` für alle diese Domänen leer ist (siehe `webRoutes.ts`
// für die vollständige Begründung je Operation) — es gibt schlicht keine
// Antwort, gegen die man ein Schema verifizieren könnte. Jedes Feld hier
// zitiert seine Rust-Quelle im Kommentar, damit ein späterer Knoten, der die
// echte Route baut, exakt sieht, woraus dieser Entwurf abgeleitet wurde.
//
// # Vier Schichten, nie verrührt
// `ResolvedModelView` hält die vier Katalog-Layer als **vier getrennte
// Felder** (`provider`, `descriptor`, `runtime`, `observed`) — genau wie
// `harw_model_catalog::resolved::ResolvedModel` es tut. Es gibt hier keine
// Funktion, die aus ihnen eine einzelne Zahl bildet; das ist die tragende
// Auflage dieses Knotens (siehe Auftrag: „Eine Ansicht, die die vier
// Schichten zu einer Zahl verrührt, nimmt dem Bediener genau die
// Information, für die es sie gibt.").

/** Ladezustand einer Liste, der „Route fehlt", „lädt", „Fehler", „geladen mit Einträgen" und „geladen, aber explizit leer" unterscheidet. */
export type ListLoadState<T> =
  | { readonly kind: "route-missing" }
  | { readonly kind: "loading" }
  | { readonly kind: "error"; readonly message: string }
  | { readonly kind: "empty" }
  | { readonly kind: "loaded"; readonly items: readonly T[] };

// ─────────────────────────────────────────────────────────────────────────────
// Layer 1 — Provider (`harw_model_catalog::spec::ProviderSpec`)
// ─────────────────────────────────────────────────────────────────────────────

/** Spiegelbild von `ProviderSpec` (`harw-model-catalog/src/spec.rs`). Angreiferkontrolliert (Konfigurationsherkunft). */
export interface ProviderLayerView {
  readonly id: string;
  readonly name: string;
  readonly baseUrl: string;
  readonly api: string;
  readonly featured: boolean;
  readonly defaultModel: string | null;
}

// ─────────────────────────────────────────────────────────────────────────────
// Layer 2 — Descriptor (`harw_model_catalog::descriptor::ModelDescriptor`)
// ─────────────────────────────────────────────────────────────────────────────

/** Spiegelbild von `ModelDescriptor` (Layer 2 — vom Provider *behauptete* Fähigkeiten). Angreiferkontrolliert (Provider-Dokumentation). */
export interface DescriptorLayerView {
  readonly provider: string;
  readonly model: string;
  readonly contextWindow: number;
  readonly maxOutputTokens: number | null;
  readonly modalities: readonly string[];
  readonly toolCalling: string;
}

// ─────────────────────────────────────────────────────────────────────────────
// Layer 3 — Runtime (`harw_model_catalog::runtime::ModelRuntimeProfile`)
// ─────────────────────────────────────────────────────────────────────────────

/** Spiegelbild von `ModelRuntimeProfile` (Layer 3 — harness-seitige Policy, kein Provider-Anspruch). */
export interface RuntimeLayerView {
  readonly contextPolicy: string;
  readonly compactionPolicy: string;
  readonly delegationPolicy: string;
  readonly maxParallelTools: number;
  readonly maxChildFanout: number;
}

// ─────────────────────────────────────────────────────────────────────────────
// Layer 4 — Observed (`harw_model_catalog::observed::ObservedModelBehavior`)
// ─────────────────────────────────────────────────────────────────────────────

/**
 * Ein einzelner Score-Wert (0..=100) mit Evidenzlage — Spiegelbild von
 * `Score` + den Begleitfeldern `updated_at`/`evidence` aus
 * `ObservedModelBehavior`. `value === null` entspricht `Score::HALF` beim
 * Bootstrap: **kein Messwert**, nicht „Wert 50" — dieselbe Unterscheidung,
 * die `harw_model_catalog::provenance::MetricEstimate::unmeasured` trifft.
 */
export interface ObservedScoreView {
  readonly label: string;
  readonly value: number;
  readonly measured: boolean;
  readonly evidenceCount: number;
}

/** Spiegelbild von `ObservedModelBehavior` (Layer 4 — empirisch gemessen). */
export interface ObservedLayerView {
  readonly provider: string;
  readonly model: string;
  readonly updatedAtIso: string | null;
  readonly scores: readonly ObservedScoreView[];
}

// ─────────────────────────────────────────────────────────────────────────────
// Vier-Schichten-Aggregat
// ─────────────────────────────────────────────────────────────────────────────

/**
 * Vier getrennte Sichten auf ein Modell — Spiegelbild von
 * `harw_model_catalog::resolved::ResolvedModel`, plus dem separat
 * aufgelösten Layer-1-Provider (siehe `resolved.rs`-Moduldoku: „Layer 1 ist
 * implizit über `descriptor.provider` zugänglich").
 */
export interface ResolvedModelView {
  readonly provider: ProviderLayerView | null;
  readonly descriptor: DescriptorLayerView;
  readonly runtime: RuntimeLayerView;
  readonly observed: ObservedLayerView;
}

// ─────────────────────────────────────────────────────────────────────────────
// Layer-4-Rückkanal — `ModelBehaviorProposal` (AW6-06)
// ─────────────────────────────────────────────────────────────────────────────

/** Spiegelbild von `ProposedModelChange::DowngradeToolCalling`/`LowerContextWindowClaim`. */
export interface ProposedModelChangeView {
  readonly kind: "downgrade_tool_calling" | "lower_context_window_claim";
  readonly to: string;
}

/**
 * Spiegelbild von `harw_knowledge::model_behavior_proposal::ModelBehaviorProposal`.
 * Trägt bewusst **kein** Feld und **keine** Funktion, die einen Vorschlag
 * anwendet — im Rust-Quelltyp existiert `accept`/`reject` (setzt nur
 * `status`), aber keine `apply` (siehe Moduldoku dort: „schlägt vor,
 * committet nie").
 */
export interface ModelBehaviorProposalView {
  readonly id: string;
  readonly title: string;
  readonly targetProvider: string;
  readonly targetModel: string;
  readonly changes: readonly ProposedModelChangeView[];
  readonly producedBy: string;
  readonly status: "Pending" | "Accepted" | "Rejected" | string;
}

// ─────────────────────────────────────────────────────────────────────────────
// Definitionsregistry — `ContextCeiling`, `SnapshotId`, Familien/Mitgliedschaft
// ─────────────────────────────────────────────────────────────────────────────

/** Spiegelbild von `harw_context::fragment::TrustClass`. Rang: `Instruction` > `Evidence` > `Data` (`trust_rank`). */
export type TrustClassView = "Instruction" | "Evidence" | "Data";

/**
 * Eine Stufe einer `extends`-Ableitungskette (`harw_agent_dsl::context_program`).
 * `trustBefore`/`trustAfter` zeigen, ob diese Stufe die geerbte
 * `TrustClass` **geschnitten** hat (`cut === true`, wenn `trustAfter` einen
 * niedrigeren Rang trägt als `trustBefore`) — `detail`/`strength` sind laut
 * Auftrag additiv und werden deshalb nicht auf einen Schnitt geprüft.
 */
export interface CeilingStepView {
  readonly programId: string;
  readonly trustBefore: TrustClassView;
  readonly trustAfter: TrustClassView;
  readonly cut: boolean;
}

/** Eine vollständige Ableitungskette von `base` bis zum aufgelösten Kontextprogramm. */
export interface CeilingChainView {
  readonly leafProgramId: string;
  readonly steps: readonly CeilingStepView[];
}

/**
 * Eine Agentendefinition, wie sie ein Verwaltungsknopf zeigen würde —
 * Rolle, Familie, Kontextprogramm und ihr `SnapshotId`
 * (`harw_agent_dsl::executable::SnapshotId`), sofern erreichbar.
 *
 * `snapshotId === null` bildet exakt den bekannten Befund ab: `SnapshotId`
 * trägt weder `Serialize`/`Deserialize` noch einen öffentlichen
 * Konstruktor (das innere Feld ist privat) — siehe `webRoutes.ts` für den
 * vollständigen Befund.
 */
export interface DefinitionSummaryView {
  readonly definitionId: string;
  readonly role: string;
  readonly family: string | null;
  readonly contextProgramId: string;
  readonly snapshotId: string | null;
}
