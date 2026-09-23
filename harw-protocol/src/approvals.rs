//! Genehmigungs-Protokoll (Interaktionsvertrag §4.1): **eine**
//! `ApprovalRequest`/`ApprovalResponse`-Form für jede genehmigungspflichtige
//! Aktion des Harness, unabhängig davon, ob die Entscheidung über die TUI
//! (Tastendruck oder `/approve`/`/deny`-Befehl) oder einen künftigen
//! Channel-Adapter (z. B. ein Telegram-Inline-Keyboard) eintrifft.
//!
//! # Woher dieser Vertrag kommt
//! Vor dieser Fassung trug dieses Modul drei eigenständige, TurnId-adressierte
//! Nutzlasten (`ExecApproval`/`PatchApproval`/`DynamicToolApproval`) ohne
//! Akteur, `WorkId` oder Ablauffrist — eine Wire-Form, die heute nirgends im
//! Repository konstruiert wird (Konstruktionsstellen leben ausschließlich in
//! `harw-core`/`harw-tui` gegen eigene, protokollunabhängige Typen). Diese
//! Fassung bringt die Form auf den in `docs/design/interaction-contract.md`
//! §4.1 normierten Umfang — **ohne** die bisherigen Nutzlasten zu verlieren:
//! sie leben als [`ApprovalKind`] innerhalb der vereinheitlichten
//! [`ApprovalRequest`].
//!
//! # `work_id`: nicht für jede Anfrage dieselbe Bedeutung
//! Für eine WorkId-gebundene Governance-Aktion (Kanban-Claim, `/skills run`,
//! `/dream review`) ist `work_id` der `WorkId` des betroffenen
//! `harw-job-runtime::Job`. Für eine Turn-lokale Werkzeugfreigabe
//! ([`ApprovalKind::Exec`]/[`ApprovalKind::Patch`]/[`ApprovalKind::DynamicTool`])
//! gibt es (Stand dieser Fassung) noch keinen `Job`, der den Turn governt —
//! der Aufrufer muss hier einen eigenen, für diese eine Anfrage gültigen
//! `WorkId` vergeben. Das Feld bleibt trotzdem verpflichtend statt optional,
//! weil der Vertrag genau das verlangt und eine optionale `WorkId` denselben
//! Typ für zwei verschiedene Adressierungsschemata öffnen würde.
//!
//! # Warum `ReviewDecision` unverändert bleibt
//! `harw_types::ReviewDecision` (`Approved`/`Rejected`/`ApprovedOnce`) wird an
//! vielen Stellen außerhalb dieses Crates exhaustiv gematcht
//! (`harw-web::security`, `harw-session-store::approval`,
//! `harw-core::turn_loop`). Eine neue Variante hier hinzuzufügen, würde diese
//! Stellen — einige davon außerhalb des Bearbeitungsumfangs dieser Änderung —
//! zum Nichtkompilieren bringen. Die für Zeitablauf/Ablehnung nötige
//! Unterscheidbarkeit trägt stattdessen [`ApprovalResponse::reason`] (Freitext,
//! wie die bestehende `comment`-Konvention in
//! `harw-ops::approval::ApprovalResolveArgs`) — bei Zeitablauf gesetzt auf
//! [`harw_types::APPROVAL_TIMEOUT_REASON`] über [`ApprovalResponse::timed_out`].

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use harw_types::{
    APPROVAL_TIMEOUT_REASON, ApprovalActor, ApprovalId, DEFAULT_APPROVAL_TIMEOUT, ReviewDecision,
    RiskLevel, TurnId, WorkId,
};

/// Welche Art von Aktion eine [`ApprovalRequest`] betrifft.
///
/// # Beschreibung
/// Bewahrt die vor dieser Fassung eigenständigen Nutzlasten
/// (`ExecApproval`/`PatchApproval`/`DynamicToolApproval`) unverändert in ihrem
/// Feldbestand — `id`/`turn_id`/`risk_level` sind auf [`ApprovalRequest`]
/// gewandert (`id`, `turn_id` bleibt hier je Variante erhalten, `risk_level`
/// heißt dort `risk`), der Rest ist unverändert.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
pub enum ApprovalKind {
    /// Exec-Befehl wartet auf Nutzergenehmigung.
    Exec {
        turn_id: TurnId,
        command: Vec<String>,
        cwd: String,
        reasoning: Option<String>,
    },
    /// Patch-Anwendung wartet auf Genehmigung.
    Patch {
        turn_id: TurnId,
        /// Dateiname → unified diff.
        changes: HashMap<String, String>,
    },
    /// Generische Tool-Genehmigung für dynamische Tools.
    DynamicTool {
        turn_id: TurnId,
        tool_name: String,
        arguments: serde_json::Value,
    },
}

impl ApprovalKind {
    /// Gibt die `TurnId` zurück, der diese Anfrage entstammt.
    ///
    /// # Returns
    /// `&TurnId` — bei jeder Variante vorhanden, da alle drei innerhalb eines
    /// laufenden Turns entstehen.
    #[must_use]
    pub fn turn_id(&self) -> &TurnId {
        match self {
            Self::Exec { turn_id, .. }
            | Self::Patch { turn_id, .. }
            | Self::DynamicTool { turn_id, .. } => turn_id,
        }
    }
}

/// Eine genehmigungspflichtige Aktion, die auf eine Entscheidung wartet
/// (Interaktionsvertrag §4.1) — die eine Form, die jede genehmigungspflichtige
/// Aktion im Harness verwendet.
///
/// # Beispiel
/// ```rust
/// use harw_protocol::approvals::{ApprovalKind, ApprovalRequest};
/// use harw_types::{ApprovalId, ReviewDecision, RiskLevel, TurnId, WorkId};
///
/// let requested_at = jiff::Timestamp::UNIX_EPOCH;
/// let request = ApprovalRequest {
///     id: ApprovalId::new(),
///     work_id: WorkId::new(),
///     kind: ApprovalKind::Exec {
///         turn_id: TurnId::new(),
///         command: vec!["rm".to_owned(), "-rf".to_owned(), "build/".to_owned()],
///         cwd: "/workspace".to_owned(),
///         reasoning: None,
///     },
///     summary: "rm -rf build/ in /workspace".to_owned(),
///     risk: RiskLevel::Medium,
///     requested_at,
///     timeout_at: ApprovalRequest::default_timeout_at(requested_at),
///     decisions: vec![ReviewDecision::Approved, ReviewDecision::Rejected],
/// };
/// assert!(!request.is_expired(requested_at));
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalRequest {
    /// Eindeutige Kennung dieser einen Anfrage.
    pub id: ApprovalId,
    /// Der von dieser Anfrage betroffene Governance-Handle. Für eine
    /// Turn-lokale Werkzeugfreigabe vergibt der Aufrufer hier eine für diese
    /// Anfrage gültige `WorkId` (siehe Moduldoku).
    pub work_id: WorkId,
    /// Welche Art von Aktion — bewahrt die bisherigen Nutzlasten.
    pub kind: ApprovalKind,
    /// Menschenlesbare Zusammenfassung, unverändert auf jeder Oberfläche
    /// dargestellt (Interaktionsvertrag §4.1).
    pub summary: String,
    /// Risiko-Klassifikation dieser Aktion.
    pub risk: RiskLevel,
    /// Wanduhrzeit, zu der diese Anfrage gestellt wurde.
    pub requested_at: jiff::Timestamp,
    /// Ablaufzeitpunkt: nach Erreichen muss jede Oberfläche
    /// [`ApprovalResponse::timed_out`] liefern, statt weiter auf eine Antwort
    /// zu warten (§4.4).
    pub timeout_at: jiff::Timestamp,
    /// Welche Entscheidungen angeboten werden (die "Knöpfe"/Subcommands, die
    /// eine Oberfläche rendert).
    ///
    /// # Warum `Vec` statt `&'static [ReviewDecision]`
    /// `ApprovalRequest` ist ein Wire-Typ (`Serialize`/`Deserialize`); eine
    /// geliehene `&'static`-Slice kann beim Deserialisieren keinen Speicher
    /// referenzieren und rundet daher nicht über die Leitung. Ein Aufrufer
    /// mit einer statischen Auswahlliste (z. B. `&[ReviewDecision::Approved,
    /// ReviewDecision::Rejected]`) ruft dafür `.to_vec()` einmal bei der
    /// Konstruktion auf.
    pub decisions: Vec<ReviewDecision>,
}

impl ApprovalRequest {
    /// Berechnet den vorgegebenen Ablaufzeitpunkt für eine zu `requested_at`
    /// gestellte Anfrage.
    ///
    /// # Description
    /// `requested_at + `[`harw_types::DEFAULT_APPROVAL_TIMEOUT`] — dieselbe
    /// Politik, die [`harw_core::AgentSession::begin_approval`] für Turn-lokale
    /// Pausen verwendet. Ein Aufrufer, der eine andere Frist braucht, berechnet
    /// `timeout_at` selbst und lässt diese Methode aus.
    ///
    /// # Arguments
    /// - `requested_at` (`jiff::Timestamp`): Ausstellungszeitpunkt der Anfrage.
    ///
    /// # Returns
    /// `requested_at + DEFAULT_APPROVAL_TIMEOUT`, oder `requested_at`
    /// unverändert, falls die Addition den darstellbaren Zeitbereich verlässt
    /// (praktisch unerreichbar bei einer Fünf-Minuten-Frist, aber kein
    /// `unwrap`/`expect` außerhalb von Tests).
    #[must_use]
    pub fn default_timeout_at(requested_at: jiff::Timestamp) -> jiff::Timestamp {
        requested_at
            .checked_add(DEFAULT_APPROVAL_TIMEOUT)
            .unwrap_or(requested_at)
    }

    /// Prüft, ob diese Anfrage zu `now` bereits abgelaufen ist.
    ///
    /// # Arguments
    /// - `now` (`jiff::Timestamp`): die Wanduhrzeit der prüfenden Oberfläche.
    ///
    /// # Returns
    /// `true`, wenn `now` `timeout_at` erreicht oder überschritten hat — die
    /// Grenze ist inklusiv, wie beim TTL-Vergleich des durablen
    /// `harw-session-store::approval::ApprovalStore` und bei
    /// `harw_core::session::PendingApproval::is_timed_out`.
    #[must_use]
    pub fn is_expired(&self, now: jiff::Timestamp) -> bool {
        now >= self.timeout_at
    }
}

/// Antwort auf eine [`ApprovalRequest`] — dieselbe Form, ob die Entscheidung
/// per TUI-Tastendruck/-Befehl oder per Telegram-Inline-Keyboard eintraf
/// (Interaktionsvertrag §4.2).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalResponse {
    /// Korrelations-ID — identisch mit [`ApprovalRequest::id`].
    pub request_id: ApprovalId,
    /// Die getroffene Entscheidung.
    pub decision: ReviewDecision,
    /// Wer entschieden hat — nie aus modellgenerierten Argumenten, immer aus
    /// der vertrauenswürdigen Channel-/Session-Autoritätsebene
    /// ([`harw_types::ApprovalActor`]).
    pub actor: ApprovalActor,
    /// Wanduhrzeit der Entscheidung.
    pub decided_at: jiff::Timestamp,
    /// Menschenlesbare Begründung. Bewahrt die bisherige `comment`-Konvention
    /// dieses Vertrags (siehe `harw-ops::approval::ApprovalResolveArgs::comment`)
    /// unter einem Namen, der auch die Zeitablauf-Begründung sauber trägt —
    /// `ReviewDecision` selbst trägt keine Nutzlast (siehe Moduldoku).
    #[serde(default)]
    pub reason: Option<String>,
}

impl ApprovalResponse {
    /// Baut die eine Ablehnung, die jede Oberfläche für eine abgelaufene
    /// [`ApprovalRequest`] liefert (§4.4 — "timed-out-denied").
    ///
    /// # Arguments
    /// - `request_id` (`ApprovalId`): die abgelaufene Anfrage.
    /// - `actor` (`ApprovalActor`): der an die Anfrage gebundene Akteur — eine
    ///   Zeitablauf-Ablehnung hat keinen eigenen, "systemischen" Akteur;
    ///   [`harw_types::ApprovalActor`] kennt nur `Operator`/`ChannelPeer`
    ///   (menschliche Identitäten), also trägt die Antwort denselben Akteur,
    ///   der die Anfrage hätte beantworten sollen.
    /// - `decided_at` (`jiff::Timestamp`): Wanduhrzeit der Ablauf-Erkennung.
    ///
    /// # Returns
    /// [`ApprovalResponse`] mit `decision = Rejected` und
    /// `reason = Some(`[`harw_types::APPROVAL_TIMEOUT_REASON`]`)`.
    #[must_use]
    pub fn timed_out(
        request_id: ApprovalId,
        actor: ApprovalActor,
        decided_at: jiff::Timestamp,
    ) -> Self {
        Self {
            request_id,
            decision: ReviewDecision::Rejected,
            actor,
            decided_at,
            reason: Some(APPROVAL_TIMEOUT_REASON.to_owned()),
        }
    }

    /// `true`, wenn diese Antwort eine durch Zeitablauf erzwungene Ablehnung
    /// ist, statt einer ausdrücklichen menschlichen Entscheidung.
    ///
    /// # Description
    /// Erkennt genau die von [`Self::timed_out`] gebaute Antwort — geprüft am
    /// `reason`-Text, nicht an `decision` allein, da eine ausdrückliche
    /// Ablehnung ebenfalls `Rejected` trägt.
    #[must_use]
    pub fn is_timeout_denial(&self) -> bool {
        self.decision == ReviewDecision::Rejected
            && self.reason.as_deref() == Some(APPROVAL_TIMEOUT_REASON)
    }
}

#[cfg(test)]
mod tests {
    use super::{ApprovalKind, ApprovalRequest, ApprovalResponse};
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_types::{
        APPROVAL_TIMEOUT_REASON, ApprovalActor, ApprovalId, DEFAULT_APPROVAL_TIMEOUT,
        ReviewDecision, RiskLevel, TurnId, WorkId,
    };

    fn test_actor() -> ApprovalActor {
        ApprovalActor::Operator {
            id: "protocol-test-operator".to_owned(),
        }
    }

    fn test_request(requested_at: jiff::Timestamp) -> ApprovalRequest {
        ApprovalRequest {
            id: ApprovalId::new(),
            work_id: WorkId::new(),
            kind: ApprovalKind::Exec {
                turn_id: TurnId::new(),
                command: vec!["rm".to_owned(), "-rf".to_owned(), "build/".to_owned()],
                cwd: "/workspace".to_owned(),
                reasoning: None,
            },
            summary: "rm -rf build/ in /workspace".to_owned(),
            risk: RiskLevel::Medium,
            requested_at,
            timeout_at: ApprovalRequest::default_timeout_at(requested_at),
            decisions: vec![ReviewDecision::Approved, ReviewDecision::Rejected],
        }
    }

    #[test]
    fn approval_kind_turn_id_reads_every_variant() {
        let turn_id = TurnId::new();
        let exec = ApprovalKind::Exec {
            turn_id: turn_id.clone(),
            command: vec!["ls".to_owned()],
            cwd: "/".to_owned(),
            reasoning: None,
        };
        assert_eq!(exec.turn_id(), &turn_id);

        let patch = ApprovalKind::Patch {
            turn_id: turn_id.clone(),
            changes: std::collections::HashMap::new(),
        };
        assert_eq!(patch.turn_id(), &turn_id);

        let dynamic_tool = ApprovalKind::DynamicTool {
            turn_id: turn_id.clone(),
            tool_name: "custom.tool".to_owned(),
            arguments: serde_json::json!({}),
        };
        assert_eq!(dynamic_tool.turn_id(), &turn_id);
    }

    #[test]
    fn default_timeout_at_adds_the_shared_default_duration() -> TestResult {
        let requested_at = jiff::Timestamp::constant(1_700_000_000, 0);
        let timeout_at = ApprovalRequest::default_timeout_at(requested_at);
        assert_eq!(
            timeout_at,
            requested_at
                .checked_add(DEFAULT_APPROVAL_TIMEOUT)
                .map_err(ctx("default timeout stays in range"))?
        );
        Ok(())
    }

    #[test]
    fn is_expired_is_inclusive_at_the_deadline() -> TestResult {
        let requested_at = jiff::Timestamp::constant(1_700_000_000, 0);
        let request = test_request(requested_at);

        assert!(!request.is_expired(requested_at));
        assert!(request.is_expired(request.timeout_at));
        assert!(
            request.is_expired(
                request
                    .timeout_at
                    .checked_add(jiff::SignedDuration::from_secs(1))
                    .map_err(ctx("one second after the deadline stays in range"))?
            )
        );
        Ok(())
    }

    #[test]
    fn approval_response_timed_out_sets_the_canonical_rejection() {
        let decided_at = jiff::Timestamp::constant(1_700_000_100, 0);
        let response = ApprovalResponse::timed_out(ApprovalId::new(), test_actor(), decided_at);

        assert_eq!(response.decision, ReviewDecision::Rejected);
        assert_eq!(response.decided_at, decided_at);
        assert_eq!(response.reason.as_deref(), Some(APPROVAL_TIMEOUT_REASON));
        assert!(response.is_timeout_denial());
    }

    #[test]
    fn is_timeout_denial_is_false_for_an_explicit_human_rejection() {
        let response = ApprovalResponse {
            request_id: ApprovalId::new(),
            decision: ReviewDecision::Rejected,
            actor: test_actor(),
            decided_at: jiff::Timestamp::constant(1_700_000_200, 0),
            reason: Some("rejected by user".to_owned()),
        };
        assert!(!response.is_timeout_denial());
    }

    #[test]
    fn is_timeout_denial_is_false_for_an_approval() {
        let response = ApprovalResponse {
            request_id: ApprovalId::new(),
            decision: ReviewDecision::Approved,
            actor: test_actor(),
            decided_at: jiff::Timestamp::constant(1_700_000_300, 0),
            reason: None,
        };
        assert!(!response.is_timeout_denial());
    }

    #[test]
    fn approval_request_round_trips_through_json() -> TestResult {
        let requested_at = jiff::Timestamp::constant(1_700_000_000, 0);
        let request = test_request(requested_at);
        let json = serde_json::to_string(&request).map_err(ctx("request serializes"))?;
        let decoded: ApprovalRequest =
            serde_json::from_str(&json).map_err(ctx("request deserializes"))?;

        assert_eq!(decoded.id, request.id);
        assert_eq!(decoded.work_id, request.work_id);
        assert_eq!(decoded.summary, request.summary);
        assert_eq!(decoded.risk, request.risk);
        assert_eq!(decoded.requested_at, request.requested_at);
        assert_eq!(decoded.timeout_at, request.timeout_at);
        match (&decoded.kind, &request.kind) {
            (
                ApprovalKind::Exec { command: left, .. },
                ApprovalKind::Exec { command: right, .. },
            ) => assert_eq!(left, right),
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected two Exec kinds, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn approval_response_round_trips_through_json() -> TestResult {
        let response = ApprovalResponse {
            request_id: ApprovalId::new(),
            decision: ReviewDecision::ApprovedOnce,
            actor: test_actor(),
            decided_at: jiff::Timestamp::constant(1_700_000_400, 0),
            reason: Some("bounded exception".to_owned()),
        };
        let json = serde_json::to_string(&response).map_err(ctx("response serializes"))?;
        let decoded: ApprovalResponse =
            serde_json::from_str(&json).map_err(ctx("response deserializes"))?;

        assert_eq!(decoded.request_id, response.request_id);
        assert_eq!(decoded.decision, response.decision);
        assert_eq!(decoded.decided_at, response.decided_at);
        assert_eq!(decoded.reason, response.reason);
        Ok(())
    }
}
