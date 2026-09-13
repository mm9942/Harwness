// Operationsanbindung der Verwaltungsfläche (Knoten UI-07).
//
// # Auflage
// Wie jede andere Routen-Gruppe: ausschließlich `findRoute`/`callOperation`
// aus `@/lib/webClient` — kein roher Pfad-String. `WEB_ROUTES` ist zum
// Zeitpunkt dieses Knotens leer (`lib/generated/operations.ts` enthält
// `[] as const`).
//
// # Geprüft, nicht erfunden — der Modellkatalog
// `harw-model-catalog` (Layer 1–4: `ProviderSpec`, `ModelDescriptor`,
// `ModelRuntimeProfile`, `ObservedModelBehavior`) ist eine reine
// Bibliotheks-Crate ohne eigenes `#[operation]`. Die einzige `harw-ops`-
// Operation, die das Wort „Modell" im Namen trägt, ist `"model"`
// (`harw-ops/src/model.rs`): `command(path = "/model", visibility =
// "tui_only")`, **kein** `web(...)`-Unterattribut — dasselbe strukturelle
// Loch, das UI-03 für `"agent"`/`"plan"`/`"goal"` bereits gemeldet hat:
// `#[operation]` (`harw-macros/src/operation.rs`) kennt aktuell nur die
// Unterattribute `command(...)` und `model_tool(...)`, kein `web(...)`.
// Außerdem zeigt `/model show|list` nur das *aktive* Modell bzw. eine
// Namensliste — nicht die vier aufgelösten Layer eines Modells
// (`ResolvedModel` aus `harw_model_catalog::resolved`). Es gibt also
// **keine** Operation, die diese Ansicht überhaupt speisen könnte, selbst
// wenn `web(...)` existierte. Der hier verwendete Name ist deshalb eine
// **erwartete**, nicht existierende Operation.
//
// # Geprüft, nicht erfunden — der Layer-4-Rückkanal (`ModelBehaviorProposal`)
// `harw-knowledge::model_behavior_proposal` exportiert ausschließlich reine
// Funktionen/Typen (`propose_tool_calling_downgrades`, `ModelBehaviorProposal`)
// — keine `#[operation]`-Deklaration in der gesamten Datei. Das Vorbild
// `harw-ops::context_proposal` (dieselbe Mechanik für Kontextprogramme)
// wurde geprüft: auch dort listet `harw-ops/src/context_proposal.rs` seine
// Operation nur für `Surface::Command`, nicht `Surface::Web`. Der erwartete
// Name unten spiegelt diese Konvention, ist aber genauso unbestätigt.
//
// # Geprüft, nicht erfunden — die Definitionsregistry
// `harw-agent-dsl` (Familien, Kontextprogramme, `ContextCeiling`,
// `SnapshotId`) hat **keine** `#[operation]`-Datei im gesamten Baum — eine
// vollständige Suche nach `#[operation]` in `harw-agent-dsl/**` und
// `harw-registry-defaults/**` ergab keinen Treffer. Es gibt also nicht
// einmal einen Command-Zweig, den man auf `web(...)` prüfen könnte: die
// Registry ist bislang komplett ohne Operationsschicht gebaut. Der
// erwartete Name unten ist reine Erwartung, kein bestätigter Name.
//
// # Der SnapshotId-Befund
// Selbst wenn eine Web-Route für Definitionen entstünde: `SnapshotId`
// (`harw-agent-dsl/src/executable.rs`) ist `pub struct SnapshotId(String)`
// mit **privatem** Feld, ohne `#[derive(Serialize, Deserialize)]` — nur
// `Display`/`Debug` (Delegation an `Display`). `serde_json::to_string` auf
// einen Typ, der `ExecutableAgentIr` enthält, würde an dieser Stelle
// entweder gar nicht kompilieren (kein Serialize-Derive auf dem Feldtyp)
// oder — falls ein künftiger Knoten das Feld manuell serialisiert — nur
// über `.to_string()` einen rohen Hex-String liefern, nie ein typisiertes
// Objekt. Diese Datei behandelt `snapshotId` deshalb konsequent als
// `string | null`, nie als eigenen Typ mit Methoden.
import { callOperation, findRoute, type OperationResult } from "@/lib/webClient";

/** Erwartete, aber (Stand dieses Knotens) nicht deklarierte Operationsnamen der Verwaltungsfläche. */
export const ADMIN_OPERATIONS = {
  /** Erwartet: alle Modelle über ihre vier Layer aufgelöst. Keine Grundlage in `harw-ops` (siehe Dateikopf). */
  modelCatalogList: "model.catalog.resolve_all",
  /** Erwartet: `ModelBehaviorProposal`-Liste. Kein `#[operation]` in `harw-knowledge::model_behavior_proposal`. */
  modelBehaviorProposalList: "knowledge.model_behavior_proposal.list",
  /** Erwartet: aufgelöste Agentendefinitionen samt `ContextCeiling`-Kette und `SnapshotId`. Keine `#[operation]`-Datei in `harw-agent-dsl` überhaupt. */
  definitionRegistryList: "agent.definition.list",
} as const;

/** Sucht die Modellkatalog-Route in `WEB_ROUTES`. */
export function resolveModelCatalogRoute() {
  return findRoute(ADMIN_OPERATIONS.modelCatalogList);
}

/** Sucht die Vorschlags-Route (`ModelBehaviorProposal`) in `WEB_ROUTES`. */
export function resolveModelBehaviorProposalRoute() {
  return findRoute(ADMIN_OPERATIONS.modelBehaviorProposalList);
}

/** Sucht die Definitionsregistry-Route in `WEB_ROUTES`. */
export function resolveDefinitionRegistryRoute() {
  return findRoute(ADMIN_OPERATIONS.definitionRegistryList);
}

/**
 * Ruft die Modellkatalog-Route auf, falls deklariert.
 *
 * # Returns
 * `undefined`, wenn die Route (noch) nicht existiert. Sonst das
 * [`OperationResult`] von `callOperation`.
 */
export async function fetchModelCatalog(): Promise<OperationResult | undefined> {
  const route = resolveModelCatalogRoute();
  if (route === undefined) {
    return undefined;
  }
  return callOperation(route);
}

/**
 * Ruft die `ModelBehaviorProposal`-Route auf, falls deklariert.
 *
 * # Returns
 * `undefined`, wenn die Route (noch) nicht existiert. Sonst das
 * [`OperationResult`] von `callOperation`.
 */
export async function fetchModelBehaviorProposals(): Promise<OperationResult | undefined> {
  const route = resolveModelBehaviorProposalRoute();
  if (route === undefined) {
    return undefined;
  }
  return callOperation(route);
}

/**
 * Ruft die Definitionsregistry-Route auf, falls deklariert.
 *
 * # Returns
 * `undefined`, wenn die Route (noch) nicht existiert. Sonst das
 * [`OperationResult`] von `callOperation`.
 */
export async function fetchDefinitionRegistry(): Promise<OperationResult | undefined> {
  const route = resolveDefinitionRegistryRoute();
  if (route === undefined) {
    return undefined;
  }
  return callOperation(route);
}
