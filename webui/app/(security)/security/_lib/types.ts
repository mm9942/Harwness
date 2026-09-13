// Fachliche Typen der Sicherheitszentrale (Knoten UI-05).
//
// Spiegelbild der bereits im DoD-Teilbaum gelieferten Rust-Typen — jedes
// Feld hier hat eine Entsprechung im gelesenen Quelltext, keines ist
// erfunden:
// - `harw-dod-signals/src/event.rs`   → Actor, AuthOutcome, EventKind,
//   SecurityEvent, DriftSeverity
// - `harw-dod-signals/src/sample.rs`  → HostSample
// - `harw-dod-signals/src/evidence.rs`→ Hardness, Severity, SecurityEvidence
// - `harw-dod-signals/src/verdict.rs` → VerdictClassification,
//   SuggestedResponse, SecurityVerdict
// - `harw-dod-rules/src/finding.rs`   → FindingKind, Verdict, Finding<S>
//   (Raw | RuleChecked | Triaged)
// - `harw-session-store/src/freeze.rs`→ Freeze, FreezeResolutionOutcome
// - `harw-dod-warden/src/audit.rs`    → AuditEvent (Warden-Audit)
//
// # Vertrauensklasse (`TrustClass`) — noch nicht anschließbar
// `harw-web` liefert (Stand dieses Knotens) keine `TrustClass` auf
// `WebEvent`/`OpOutput` — siehe `lib/sse.ts` (`WebEventKind` kennt nur
// `operation_completed`/`heartbeat`) und `components/ui/DataBlock.tsx`-Kopf
// (`OpOutput` ist nur `{ text: String }`). Jedes Feld unten, das aus einem
// Sensor stammt, ist deshalb angreiferkontrolliert und geht ausnahmslos
// durch `DataBlock`/`DataBlockList` — auch `path`, `destination`, `detail`,
// `summary`, `rationale`. Sobald `harw-web` ein Klassenfeld liefert, ist die
// anzuschließende Stelle `DataBlock`s künftiger `trust`-Prop (siehe dortiger
// Kopf) — dieser Knoten erfindet keine eigene Zwei-Block-Darstellung daneben.

/** Wer ein Ereignis ausgelöst hat (`harw_dod_signals::Actor`). */
export interface ActorInfo {
  readonly uid: number;
  readonly auid: number | null;
  readonly cgroup: string | null;
}

/** Ausgang eines Anmeldeversuchs (`harw_dod_signals::AuthOutcome`). */
export type AuthOutcome = "success" | "failure";

/** Schwere einer Strukturabweichung (`harw_dod_signals::DriftSeverity`). */
export type DriftSeverity = "unknown" | "low" | "medium" | "high";

/**
 * Art eines Sicherheitsereignisses — geschlossen, Spiegelbild von
 * `harw_dod_signals::EventKind` (`tag = "kind"`, kebab-case).
 *
 * `argvDigest` trägt bewusst nur einen Digest, nie die rohe Kommandozeile
 * (siehe Rust-Moduldoku) — trotzdem angreiferkontrolliert genug (ein Digest
 * ist eine vom Sensor gelieferte Zeichenkette), um durch `DataBlock` zu
 * gehen.
 */
export type EventKind =
  | { readonly kind: "process-exec"; readonly path: string; readonly argvDigest: string }
  | { readonly kind: "file-write"; readonly path: string }
  | { readonly kind: "egress-flow"; readonly destination: string; readonly port: number }
  | { readonly kind: "listener-opened"; readonly port: number }
  | { readonly kind: "auth-event"; readonly outcome: AuthOutcome }
  | {
      readonly kind: "structure-drift";
      readonly severity: DriftSeverity;
      readonly detail: string;
    }
  | { readonly kind: "sensor-degraded"; readonly sensor: string };

/** Ein sicherheitsrelevantes Ereignis (`harw_dod_signals::SecurityEvent`). */
export interface SecurityEvent {
  readonly sensor: string;
  readonly observedAt: string;
  readonly actor: ActorInfo | null;
  readonly kind: EventKind;
}

/** Ein Hostmesswert — Zahlen, kein Befund (`harw_dod_signals::HostSample`). */
export interface HostSample {
  readonly sensor: string;
  readonly observedAt: string;
  readonly metric: string;
  readonly value: number;
}

/** Wie hart ein Nachweis ist (`harw_dod_signals::Hardness`). Aufsteigend. */
export type Hardness = "observed" | "correlated" | "inferred";

/** Wie schwer ein Befund/Urteil wiegt (`harw_dod_signals::Severity`). Aufsteigend. */
export type Severity = "info" | "low" | "medium" | "high" | "critical";

/**
 * Eingefrorene Beobachtungen als Beleg (`harw_dod_signals::SecurityEvidence`).
 *
 * `digest` macht den Beleg zitierfähig (siehe Rust-Moduldoku, Abschnitt
 * „Warum ein Digest") — genau dieser Wert ist die Voraussetzung dafür, dass
 * ein Feld je als `TrustClass::Evidence` statt `TrustClass::Data` gelten
 * könnte.
 */
export interface SecurityEvidence {
  readonly digest: string;
  readonly samples: readonly HostSample[];
  readonly events: readonly SecurityEvent[];
}

/** Womit ein Befund seine Existenz rechtfertigt (`harw_dod_rules::FindingKind`). */
export type FindingKind = "rule-triggered" | "anomaly";

/** Ergebnis einer Triage-Entscheidung (`harw_dod_rules::Verdict`). */
export type FindingVerdict = "confirmed" | "false-positive" | "needs-review";

/** Gemeinsame Felder aller drei `Finding<S>`-Zustände. */
interface FindingCommon {
  readonly ruleId: string;
  readonly kind: FindingKind;
  readonly severity: Severity;
  readonly hardness: Hardness;
  /** Menschenlesbare Zusammenfassung — angreiferkontrolliert, über `DataBlock`. */
  readonly summary: string;
  readonly observedAt: string;
}

/**
 * Ein Befund im Typestate `Finding<S>` — Spiegelbild von
 * `harw_dod_rules::finding::Finding<Raw|RuleChecked|Triaged>`. `state`
 * trägt hier zur Laufzeit nach, was in Rust ein Typparameter ist: ein
 * `Finding<Raw>` hat keine `FindingId` und kein Triage-Ergebnis, ein
 * `Finding<RuleChecked>` eine `FindingId` ohne Ergebnis, ein
 * `Finding<Triaged>` beides.
 */
export type Finding =
  | (FindingCommon & { readonly state: "raw" })
  | (FindingCommon & { readonly state: "rule-checked"; readonly findingId: string })
  | (FindingCommon & {
      readonly state: "triaged";
      readonly findingId: string;
      readonly verdict: FindingVerdict;
    });

/** Einstufung eines geprüften Befunds (`harw_dod_signals::VerdictClassification`). */
export type VerdictClassification = "benign" | "suspicious" | "confirmed";

/** Grobe, nicht ausführbare Vorschlagskategorie (`harw_dod_signals::SuggestedResponse`). */
export type SuggestedResponse = "monitor" | "escalate" | "contain";

/**
 * Das Urteil eines Sicherheitsagenten über einen geprüften Befund
 * (`harw_dod_signals::SecurityVerdict`). **Nur eine Aussage, keine
 * Anweisung** — es gibt kein UI-Element, das ein Urteil in eine Aktion
 * übersetzt (das ist AW5-03/`authorize`, `pub(crate)`, siehe Auftrag).
 */
export interface SecurityVerdict {
  readonly findingId: string;
  readonly boundEvidenceDigest: string;
  readonly classification: VerdictClassification;
  readonly severity: Severity;
  /** Freitext-Begründung — angreiferkontrolliert (vom Agenten formuliert), über `DataBlock`. */
  readonly rationale: string;
  readonly suggestedResponse: SuggestedResponse | null;
  readonly issuedBy: string;
  readonly issuedAt: string;
}

/** Warum ein Freeze in den Endzustand überging (`FreezeResolutionOutcome`). */
export type FreezeResolutionOutcome = "lifted" | "expired";

/**
 * Ein Freeze-Datensatz (`harw_session_store::freeze::Freeze`). Rein
 * anzeigend — diese Fläche friert nichts ein und hebt nichts auf (siehe
 * Auftrag, Abschnitt 2: der Durchsetzer ist `harw-warden`).
 */
export interface FreezeRecord {
  readonly cgroup: string;
  readonly finding: string;
  readonly frozenAt: string;
  readonly expiresAt: string | null;
  readonly resolvedAt: string | null;
  readonly resolutionOutcome: FreezeResolutionOutcome | null;
}

/** Ablehnungskategorie einer Warden-Anfrage (`harw_dod_warden_proto::Denial`). */
export type WardenDenial = "proof-mismatch" | "not-admissible-at-stage";

/**
 * Ein Eintrag des Warden-Audit-Protokolls (`harw_dod_warden::audit::AuditEvent`).
 *
 * **Bekannte Einschränkung:** Dieses Protokoll lebt (Stand des gelesenen
 * Quelltexts) ausschließlich im Prozessspeicher des Warden
 * (`RecordingAuditSink`, `Mutex<Vec<AuditEvent>>`) — es gibt keinen
 * dateibasierten Speicher und keine Operation, die es exportiert. Dieser
 * Typ existiert, damit eine künftige Operation sich anschließen lässt, ohne
 * dass diese Ansicht sich einen Pfad ausdenkt (siehe Abschlussbericht).
 */
export type WardenAuditEvent =
  | { readonly event: "attempting"; readonly actionSummary: string }
  | { readonly event: "executed"; readonly actionSummary: string; readonly auditName: string }
  | { readonly event: "execution-failed"; readonly actionSummary: string }
  | { readonly event: "denied"; readonly actionSummary: string; readonly reason: WardenDenial }
  | { readonly event: "verification-failed"; readonly actionSummary: string };

/** Eine einzelne, im UI gesammelte Lücken-Meldung aus dem Ereignisstrom. */
export interface SecurityGapNotice {
  readonly id: number;
  readonly missingCount: number;
  readonly detectedAtIso: string;
}

/**
 * Ergebnis eines Abrufversuchs für eine der Sicherheits-Datenquellen.
 *
 * Unterscheidet ausdrücklich drei Fälle, damit „keine Daten verfügbar" nie
 * mit „keine Befunde" verwechselt wird (siehe Auftrag, Abschnitt „Tests"):
 * - `route-missing`: `WEB_ROUTES` kennt die erwartete Operation (noch)
 *   nicht — es wurde nie eine Anfrage gestellt.
 * - `ok` mit `items: []`: die Operation wurde erfolgreich aufgerufen und
 *   hat eine leere Liste geliefert — es gibt tatsächlich keine Befunde.
 * - `error`: die Operation existiert, der Aufruf ist aber fehlgeschlagen.
 */
export type SecurityFetchResult<T> =
  | { readonly status: "route-missing"; readonly operation: string }
  | { readonly status: "ok"; readonly items: readonly T[] }
  | { readonly status: "error"; readonly operation: string; readonly message: string };
