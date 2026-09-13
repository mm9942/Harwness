// Fachliche Typen der Bestätigungsfläche (Knoten UI-06).
//
// Spiegelbild der bereits gelieferten Rust-Typen — jedes Feld hier hat eine
// Entsprechung im gelesenen Quelltext, keines ist erfunden:
// - `harw-session-store/src/approval.rs` → ApprovalRecord,
//   ApprovalResolutionRecord (Felder `request`/`session`/`call_id`/`actor`/
//   `issued_at`, bzw. zusätzlich `decision`/`comment`/`resolved_at`).
// - `harw-types/src/ids.rs`               → ApprovalActor
//   (`Operator { id }` | `ChannelPeer { channel, peer }`, `tag = "kind"`,
//   `rename_all = "snake_case"`).
// - `harw-types/src/roles.rs`             → ReviewDecision
//   (`Approved` | `Rejected` | `ApprovedOnce`, `rename_all = "snake_case"`).
// - `harw-dod-warden-proto/src/action.rs` → Reversibility, die vier
//   `WardenAction`-Varianten und ihre `kind_name()`-Werte
//   (`warden.freeze_cgroup`, `warden.release_cgroup`,
//   `warden.isolate_network`, `warden.kill_process_tree`).
//
// # Wo der Anzeigeinhalt herkommt — noch offen
// `ApprovalRecord` trägt keinen Befundtext und keine `WardenAction`/
// `Reversibility` — nur `call_id`. Welche Operation `call_id` in einen
// Anzeigeinhalt auflöst, ist (Stand dieses Knotens) nicht deklariert; siehe
// `harw-web/src/security.rs`-Kopf (`PendingApprovalView`) und den
// Abschlussbericht dieses Knotens. `PendingApprovalView` hier ist deshalb
// bewusst so geschnitten, dass eine künftige Operation sie direkt liefern
// kann, ohne dass diese Datei sich eine eigene Form ausdenkt.
//
// # Vertrauensklasse
// `rationale` stammt aus Sensordaten/Modellausgabe und ist
// angreiferkontrolliert — sie geht ausnahmslos durch `DataBlock`
// (siehe `_components/ApprovalRequestPanel.tsx`).

/** Wer eine Genehmigung ausstellen/auflösen darf (`harw_types::ApprovalActor`). */
export type ApprovalActor =
  | { readonly kind: "operator"; readonly id: string }
  | { readonly kind: "channel_peer"; readonly channel: string; readonly peer: string };

/** Menschliche Entscheidung über eine Anfrage (`harw_types::ReviewDecision`). */
export type ReviewDecision = "approved" | "rejected" | "approved_once";

/** Maschinenlesbarer Aktionsname (`harw_dod_warden_proto::WardenAction::kind_name`). */
export type WardenActionKind =
  | "warden.freeze_cgroup"
  | "warden.release_cgroup"
  | "warden.isolate_network"
  | "warden.kill_process_tree";

/** Eine durabel gespeicherte, noch offene Anfrage (`ApprovalRecord`). */
export interface ApprovalRecord {
  readonly request: string;
  readonly session: string;
  readonly callId: string;
  readonly actor: ApprovalActor;
  readonly issuedAt: string;
}

/**
 * Anzeigeinhalt einer anstehenden Genehmigungsanfrage
 * (`harw_web::security::PendingApprovalView`).
 *
 * `irreversible` muss der Bedienoberfläche **sichtbar** mitgeteilt werden —
 * „Prozessbaum beenden" und „cgroup einfrieren" sehen als Knopf gleich aus
 * und sind es nicht (siehe Auftrag, Abschnitt „Was sieht der Mensch?").
 */
export interface PendingApprovalView {
  readonly record: ApprovalRecord;
  readonly actionKind: WardenActionKind;
  readonly irreversible: boolean;
  /** Angreiferkontrollierter Befundtext — ausnahmslos über `DataBlock`. */
  readonly rationale: string;
}

/** Aufgelöste Anfrage (`ApprovalResolutionRecord`). */
export interface ApprovalResolutionRecord {
  readonly request: string;
  readonly session: string;
  readonly callId: string;
  readonly actor: ApprovalActor;
  readonly decision: ReviewDecision;
  readonly comment: string | null;
  readonly resolvedAt: string;
}

/**
 * Ergebnis eines Auflösungsversuchs — dieselbe Drei-Wege-Unterscheidung wie
 * `SecurityFetchResult` in `_lib/types.ts` (Knoten UI-05), hier für einen
 * einzelnen Schreibaufruf statt einer Liste.
 */
export type ApprovalSubmitResult =
  | { readonly status: "route-missing"; readonly operation: string }
  | { readonly status: "ok"; readonly resolution: ApprovalResolutionRecord }
  | { readonly status: "error"; readonly operation: string; readonly message: string };
