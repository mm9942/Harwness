// ACHTUNG: automatisch erzeugte Datei — nicht von Hand bearbeiten.
//
// Erzeugt von `cargo xtask webui types` aus den `Surface::Web`-Deklarationen
// im Rust-Quelltext (siehe xtask/src/webui.rs für die Extraktionsregeln).
// Bei Abweichung schlägt `cargo xtask webui types --check` fehl — das ist
// das CI-Gate für diese Datei.
//
// Neu erzeugen: `cargo xtask webui types`

export type PermissionTier = "Observer" | "Operator" | "Maintainer" | "Owner";
export type ApprovalPolicy =
  | "None"
  | "Always"
  | "RequireForScope"
  | "RequireForEffect"
  | "RequireForRiskClass";
export type HttpMethod = "GET" | "POST";

export interface WebRouteDescriptor {
  readonly operation: string;
  readonly path: string;
  readonly method: HttpMethod;
  readonly permission: PermissionTier;
  readonly approval: ApprovalPolicy;
}

export const WEB_ROUTES: readonly WebRouteDescriptor[] = [
  // Analysiert den Workspace bottom-up über read-only Analyst-Kindagenten.
  { operation: "analyze", path: "/api/analyze", method: "GET", permission: "Operator", approval: "None" },
  // Listet alle offenen Genehmigungsanfragen. Autorisiert nichts.
  { operation: "approval.pending", path: "/api/approval-pending", method: "GET", permission: "Observer", approval: "None" },
  // Löst eine offene Genehmigungsanfrage anhand einer menschlichen Entscheidung auf. Autorisiert selbst nichts.
  { operation: "approval.resolve", path: "/api/approval-resolve", method: "POST", permission: "Operator", approval: "None" },
  // Inspiziert einen dauerhaft gespeicherten Job.
  { operation: "attach", path: "/api/attach", method: "GET", permission: "Operator", approval: "None" },
  // Zeigt den aktuellen git-Diff des Workspaces.
  { operation: "diff", path: "/api/diff", method: "GET", permission: "Observer", approval: "None" },
  // Beantwortet eine gebundene Frage durch einen read-only Explorer-Kindagenten.
  { operation: "explore", path: "/api/explore", method: "GET", permission: "Operator", approval: "None" },
  // Ziel verwalten: setzen, schärfen, Kriterien und Invarianten ergänzen, gegen den Plan bewerten.
  { operation: "goal", path: "/api/goal", method: "POST", permission: "Operator", approval: "Always" },
  // Listet alle verfügbaren /-Befehle mit Zusammenfassung.
  { operation: "help", path: "/api/help", method: "GET", permission: "Observer", approval: "None" },
  // Shows the current immutable sandbox workspace and permissions.
  { operation: "permissions", path: "/api/permissions", method: "GET", permission: "Maintainer", approval: "None" },
  // Plan verwalten: anlegen, Knoten pflegen, Wellen und Bereitschaft lesen, abgleichen.
  { operation: "plan", path: "/api/plan", method: "POST", permission: "Operator", approval: "Always" },
  // Listet alle laufenden Jobs und Kindprozesse.
  { operation: "ps", path: "/api/ps", method: "GET", permission: "Observer", approval: "None" },
  // Recherchiert Dependency-Fakten (Versionen, MSRV, Features) über einen read-only Kindagenten.
  { operation: "research_deps", path: "/api/research-deps", method: "GET", permission: "Operator", approval: "None" },
  // Recherchiert eine gebundene Frage im Web über einen read-only Kindagenten.
  { operation: "research_web", path: "/api/research-web", method: "GET", permission: "Operator", approval: "None" },
  // Zeigt aktuellen Session- und Turn-Status.
  { operation: "status", path: "/api/status", method: "GET", permission: "Observer", approval: "None" },
  // Bricht einen laufenden Job kontrolliert ab.
  { operation: "stop", path: "/api/stop", method: "POST", permission: "Operator", approval: "Always" },
  // Zeigt dauerhafte Job-Zusammenfassung; Approval- und Diff-Daten lokal nicht verfügbar.
  { operation: "work", path: "/api/work", method: "GET", permission: "Observer", approval: "None" },
] as const;
