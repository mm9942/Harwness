//! Entfernte Sitzungen: dieselbe Konversation in einem anderen Prozess.
//!
//! # Beschreibung
//! Ein `harw gateway --session-socket` hostet Sitzungen und spricht
//! `harw.session.v1` (WebSocket über einen Unix-Socket). [`RemoteHarwness`]
//! verbindet sich damit; [`RemoteSession`] ist eine angehängte Sitzung. Die
//! Fläche gleicht der lokalen [`crate::Session`]: [`RemoteSession::send`]
//! fährt einen Turn zu Ende und liefert einen [`TurnReport`], die Ereignisse
//! sind dieselben [`SdkEvent`], Freigaben beantwortet derselbe
//! [`ApprovalHandler`]. Kein interner Protokolltyp erscheint in einer Signatur.
//!
//! # Was `send` kapselt
//! - **Absenden:** ein Prompt trägt einen Idempotenzschlüssel, der einen
//!   Neustart des Clients übersteht (Epoche aus Startzeit und Zähler); steht
//!   der Verlauf inzwischen weiter (`Stale`), wird mit dem neuen Stand erneut
//!   abgesendet.
//! - **Freigabe:** hält der Turn an, fragt `send` den [`ApprovalHandler`] und
//!   meldet die Entscheidung. Der Host entscheidet, wer freigeben darf; nur
//!   wenn er das Recht `approve` erteilt hat ([`RemoteSession::may_approve`]),
//!   wird der Handler gefragt. Sonst wartet der Turn auf ein anderes Gerät.
//!   Gewinnt dort jemand anderes, ist das kein Fehler.
//! - **Strom:** verliert der Strom seine Position (`Lagged`, `Resync`), hängt
//!   sich die Sitzung ab dem Stand des Hosts neu an; Wiedergabe-Frames älterer
//!   Positionen zählen nicht als Ereignisse dieses Turns.
//!
//! # Grenzen
//! - `send` setzt voraus, dass kein anderer Client gleichzeitig Turns derselben
//!   Sitzung fährt. Steht der eigene Prompt hinter laufenden Turns, wartet
//!   `send` auf deren Ende mit.
//! - Eine verlorene Verbindung beendet `send` mit einem Fehler; eine
//!   automatische Wiederverbindung gibt es hier noch nicht (sie kommt aus
//!   `harw-session-remote`, sobald sie dort vorhanden ist).
//!
//! # Nebenläufigkeit
//! [`RemoteHarwness`] ist billig klonbar. `send` nimmt `&mut self`: eine
//! [`RemoteSession`] fährt nie zwei Turns zugleich.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use harw_protocol::session_wire::{
    ApprovalRespondParams, AttachParams, CreateParams, Cursor, DEFAULT_TAIL_ITEMS, FrameEnvelope,
    HostedState, InterruptParams, RespondResult, SessionFrame, StreamProfile, SubmitParams,
    SubmitResult,
};
use harw_protocol::{ApprovalKind, FrameSource, PortError, SessionPort};
use harw_session_remote::{ConnectOptions, RemotePort, connect_unix};
use harw_types::{ApprovalId, ReviewDecision};

use crate::approval::{ApprovalHandler, ApprovalRequest, Decision, default_handler};
use crate::error::{Result, SdkError};
use crate::event::{EventSource, FinishStatus, SdkEvent, Usage, map_turn_event};
use crate::ids::SessionId;
use crate::session::{TurnReport, TurnStatus};

/// Wie oft ein Prompt bei einem veralteten Stand erneut abgesendet wird.
const MAX_SUBMIT_ATTEMPTS: usize = 3;

/// Instanzen in diesem Prozess; mit der Startzeit ergibt das die Epoche der
/// Idempotenzschlüssel.
static INSTANCES: AtomicU64 = AtomicU64::new(0);

fn fresh_epoch() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    format!("{nanos:x}.{}", INSTANCES.fetch_add(1, Ordering::Relaxed))
}

/// Eine Sitzung des Hosts, wie `sessions` sie auflistet.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RemoteSessionInfo {
    /// Kennung der Sitzung.
    pub id: SessionId,
    /// Titel, falls gesetzt.
    pub title: Option<String>,
    /// Zustand in Kurzform: `idle`, `running`, `waiting_for_approval`,
    /// `waiting_for_child`, `queued`, `interrupted`, `failed`, `closed`
    /// (oder `unknown` für künftige Zustände).
    pub state: String,
    /// Wie viele Clients gerade angehängt sind.
    pub attached: u32,
}

fn state_word(state: &HostedState) -> &'static str {
    match state {
        HostedState::Idle => "idle",
        HostedState::Running => "running",
        HostedState::WaitingForApproval => "waiting_for_approval",
        HostedState::WaitingForChild => "waiting_for_child",
        HostedState::Queued { .. } => "queued",
        HostedState::Interrupted => "interrupted",
        HostedState::Failed => "failed",
        HostedState::Closed => "closed",
        #[allow(unreachable_patterns)]
        _ => "unknown",
    }
}

/// Übersetzt einen Portfehler an der Grenze in den SDK-Fehler.
fn session_error(error: PortError) -> SdkError {
    SdkError::Session {
        detail: error.to_string(),
    }
}

/// Baut eine [`RemoteHarwness`]; siehe [`RemoteHarwness::builder`].
pub struct RemoteBuilder {
    socket: Option<PathBuf>,
    label: String,
    approvals: Arc<dyn ApprovalHandler>,
    compact: bool,
    turn_timeout: Option<Duration>,
}

impl std::fmt::Debug for RemoteBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteBuilder")
            .field("socket", &self.socket)
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

impl RemoteBuilder {
    /// Der Unix-Socket des Hosts (Pflicht), z. B.
    /// `$XDG_RUNTIME_DIR/harw/session.sock`.
    #[must_use]
    pub fn socket(mut self, path: impl Into<PathBuf>) -> Self {
        self.socket = Some(path.into());
        self
    }

    /// Anzeigename dieses Clients (Anwesenheit und Protokoll des Hosts).
    #[must_use]
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    /// Beantwortet Freigaben; Vorgabe [`crate::AutoDeny`].
    #[must_use]
    pub fn approval_handler(mut self, handler: impl ApprovalHandler) -> Self {
        self.approvals = Arc::new(handler);
        self
    }

    /// Fordert das schmale Profil an (kein Reasoning und keine Kind-Deltas,
    /// zusammengefasster Text); gedacht für Telefone und schmale Leitungen.
    #[must_use]
    pub fn compact(mut self, compact: bool) -> Self {
        self.compact = compact;
        self
    }

    /// Obergrenze für einen ganzen [`RemoteSession::send`]; Vorgabe keine.
    /// Schützt davor, dass ein Turn ewig auf ein anderes Gerät wartet.
    #[must_use]
    pub fn turn_timeout(mut self, timeout: Duration) -> Self {
        self.turn_timeout = Some(timeout);
        self
    }

    /// Verbindet sich, hebt die Verbindung auf `harw.session.v1` und führt
    /// `session.hello` aus.
    ///
    /// # Fehler
    /// [`SdkError::InvalidInput`] ohne Socket; [`SdkError::Session`], wenn der
    /// Host nicht erreichbar ist oder die Verbindung ablehnt.
    pub async fn connect(self) -> Result<RemoteHarwness> {
        let socket = self
            .socket
            .ok_or_else(|| SdkError::invalid("socket", "must be set"))?;
        let connection = connect_unix(socket, ConnectOptions::new(self.label.clone()))
            .await
            .map_err(|error| SdkError::Session {
                detail: error.to_string(),
            })?;
        Ok(RemoteHarwness {
            port: Arc::new(RemotePort::new(connection)),
            approvals: self.approvals,
            label: self.label,
            compact: self.compact,
            turn_timeout: self.turn_timeout,
        })
    }
}

/// Eine Verbindung zu einem Host, der Sitzungen bereitstellt.
#[derive(Clone)]
pub struct RemoteHarwness {
    port: Arc<dyn SessionPort>,
    approvals: Arc<dyn ApprovalHandler>,
    label: String,
    compact: bool,
    turn_timeout: Option<Duration>,
}

impl std::fmt::Debug for RemoteHarwness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteHarwness")
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

impl RemoteHarwness {
    /// Beginnt eine Verbindung.
    #[must_use]
    pub fn builder() -> RemoteBuilder {
        RemoteBuilder {
            socket: None,
            label: "harwness-sdk".to_owned(),
            approvals: default_handler(),
            compact: false,
            turn_timeout: None,
        }
    }

    /// Die Sitzungen, die der Host kennt und die dieser Client sehen darf.
    ///
    /// # Fehler
    /// [`SdkError::Session`] bei einem Verbindungs- oder Zugriffsfehler.
    pub async fn sessions(&self) -> Result<Vec<RemoteSessionInfo>> {
        let list = self.port.list().await.map_err(session_error)?;
        Ok(list
            .into_iter()
            .map(|summary| RemoteSessionInfo {
                id: SessionId::from_core(&summary.session_id),
                title: summary.title,
                state: state_word(&summary.state).to_owned(),
                attached: summary.attached,
            })
            .collect())
    }

    /// Legt eine neue Sitzung an und hängt sich an.
    ///
    /// # Fehler
    /// [`SdkError::Session`], wenn der Host ablehnt (etwa mangels Recht).
    pub async fn create_session(&self, title: Option<&str>) -> Result<RemoteSession> {
        let created = self
            .port
            .create(CreateParams {
                workspace: None,
                title: title.map(str::to_owned),
            })
            .await
            .map_err(session_error)?;
        self.open(created.session_id, None).await
    }

    /// Hängt sich an eine vorhandene Sitzung an.
    ///
    /// # Fehler
    /// [`SdkError::InvalidInput`] bei einer ungültigen Kennung;
    /// [`SdkError::Session`], wenn der Host ablehnt oder sie nicht kennt.
    pub async fn attach(&self, id: &SessionId) -> Result<RemoteSession> {
        self.open(id.to_core()?, None).await
    }

    async fn open(
        &self,
        session: harw_types::SessionId,
        from: Option<Cursor>,
    ) -> Result<RemoteSession> {
        let (ack, source) = self
            .port
            .attach(AttachParams {
                session_id: session.clone(),
                from,
                profile: if self.compact {
                    StreamProfile::Compact
                } else {
                    StreamProfile::Full
                },
                tail_items: DEFAULT_TAIL_ITEMS,
            })
            .await
            .map_err(session_error)?;
        Ok(RemoteSession {
            id: SessionId::from_core(&session),
            wire_id: session,
            port: Arc::clone(&self.port),
            approvals: Arc::clone(&self.approvals),
            compact: self.compact,
            turn_timeout: self.turn_timeout,
            source: Some(source),
            head: ack.head,
            last_seen: None,
            resume: None,
            may_approve: ack.granted.approve,
            answered: HashSet::new(),
            key_prefix: format!("{}-{}", self.label, fresh_epoch()),
            sent: 0,
        })
    }
}

/// Was während eines Turns eingesammelt wird.
#[derive(Default)]
struct Collected {
    final_text: Option<String>,
    last_text: Option<String>,
    tool_calls: u32,
    approvals: u32,
    usage: Usage,
    finished: usize,
    status: Option<TurnStatus>,
}

/// Eine angehängte Sitzung eines Hosts.
pub struct RemoteSession {
    id: SessionId,
    wire_id: harw_types::SessionId,
    port: Arc<dyn SessionPort>,
    approvals: Arc<dyn ApprovalHandler>,
    compact: bool,
    turn_timeout: Option<Duration>,
    source: Option<Box<dyn FrameSource>>,
    /// Der Stand des Hosts beim (Neu-)Anhängen: die Position des nächsten
    /// Eintrags, den dieser Client noch nicht gesehen hat. Ein Ereignis davor
    /// ist Wiedergabe; eines **auf** diesem Stand ist neu (auf einer frischen
    /// Sitzung trägt schon der erste Live-Frame genau diese Position).
    head: Cursor,
    /// Position des zuletzt verarbeiteten Ereignisses; neue Ereignisse liegen
    /// strikt dahinter.
    last_seen: Option<Cursor>,
    /// Wohin neu angehängt wird, solange keine Quelle da ist.
    resume: Option<Cursor>,
    may_approve: bool,
    answered: HashSet<String>,
    key_prefix: String,
    sent: u64,
}

impl std::fmt::Debug for RemoteSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteSession")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl RemoteSession {
    /// Die Kennung der Sitzung.
    #[must_use]
    pub fn id(&self) -> &SessionId {
        &self.id
    }

    /// `true`, wenn der Host diesem Client das Recht `approve` erteilt hat.
    /// Nur dann fragt [`Self::send`] den [`ApprovalHandler`]; sonst muss ein
    /// anderes Gerät entscheiden.
    #[must_use]
    pub fn may_approve(&self) -> bool {
        self.may_approve
    }

    /// Sendet einen Prompt und wartet, bis der Turn endet.
    ///
    /// # Fehler
    /// [`SdkError::Turn`], wenn der Host ablehnt, die Warteschlange voll ist
    /// oder die Zeitgrenze überschritten wird; [`SdkError::Session`] bei
    /// einem Verbindungsfehler; [`SdkError::Approval`], wenn eine
    /// Entscheidung nicht zugestellt werden kann.
    pub async fn send(&mut self, text: impl Into<String>) -> Result<TurnReport> {
        self.send_with(text, |_| {}).await
    }

    /// Wie [`Self::send`], und meldet jedes Ereignis des Turns (Wurzel und
    /// Kind-Agenten) an `on_event`.
    ///
    /// # Fehler
    /// Wie [`Self::send`].
    pub async fn send_with<F>(
        &mut self,
        text: impl Into<String>,
        mut on_event: F,
    ) -> Result<TurnReport>
    where
        F: FnMut(&SdkEvent),
    {
        let text = text.into();
        let timeout = self.turn_timeout;
        let work = self.run_turn(text, &mut on_event);
        match timeout {
            Some(limit) => tokio::time::timeout(limit, work)
                .await
                .map_err(|_| SdkError::Turn {
                    detail: format!("the turn did not finish within {limit:?}"),
                })?,
            None => work.await,
        }
    }

    /// Bricht den laufenden Turn ab.
    ///
    /// # Fehler
    /// [`SdkError::Turn`], wenn der Host ablehnt (etwa mangels Recht).
    pub async fn cancel(&self) -> Result<()> {
        self.port
            .interrupt(InterruptParams {
                session_id: self.wire_id.clone(),
                turn_id: None,
            })
            .await
            .map_err(|error| SdkError::Turn {
                detail: error.to_string(),
            })
    }

    async fn run_turn(
        &mut self,
        text: String,
        on_event: &mut dyn FnMut(&SdkEvent),
    ) -> Result<TurnReport> {
        self.drain().await;
        let ahead = self.submit(text).await?;
        let wanted = ahead + 1;
        let mut got = Collected::default();
        while got.finished < wanted {
            let envelope = self.next_frame().await?;
            self.on_frame(envelope, &mut got, on_event).await?;
        }
        Ok(TurnReport {
            session_id: self.id.clone(),
            status: got.status.unwrap_or(TurnStatus::Completed),
            text: got.final_text.or(got.last_text),
            usage: got.usage,
            tool_calls: got.tool_calls,
            approvals: got.approvals,
        })
    }

    /// Verbraucht bereits wartende Frames (Wiedergabe beim Anhängen, Ereignisse
    /// anderer Clients) und merkt sich ihre Position.
    async fn drain(&mut self) {
        let head = self.head;
        let mut seen = self.last_seen;
        if let Some(source) = self.source.as_mut() {
            while let Ok(Ok(Some(envelope))) =
                tokio::time::timeout(Duration::ZERO, source.next()).await
            {
                let event = matches!(
                    envelope.frame,
                    SessionFrame::Turn(_) | SessionFrame::Child { .. }
                );
                let new = envelope.cursor >= head
                    && seen.as_ref().is_none_or(|last| envelope.cursor > *last);
                if event && new {
                    seen = Some(envelope.cursor);
                }
            }
        }
        self.last_seen = seen;
    }

    /// Sendet den Prompt; wiederholt bei veraltetem Stand. Liefert, wie viele
    /// Turns vor dem eigenen laufen oder warten.
    async fn submit(&mut self, text: String) -> Result<usize> {
        self.sent += 1;
        let key = format!("{}-{}", self.key_prefix, self.sent);
        for _ in 0..MAX_SUBMIT_ATTEMPTS {
            let result = self
                .port
                .submit(SubmitParams {
                    session_id: self.wire_id.clone(),
                    text: text.clone(),
                    expect_head: self.head,
                    client_msg_id: key.clone(),
                    force: false,
                })
                .await
                .map_err(|error| SdkError::Turn {
                    detail: error.to_string(),
                })?;
            match result {
                SubmitResult::Accepted { position } => {
                    return Ok(usize::try_from(position).unwrap_or(usize::MAX));
                }
                SubmitResult::Stale { head } => self.head = head,
                SubmitResult::QueueFull => {
                    return Err(SdkError::Turn {
                        detail: "the host's queue for this session is full".to_owned(),
                    });
                }
                SubmitResult::Denied { reason } => {
                    return Err(SdkError::Turn {
                        detail: format!("not allowed: {reason}"),
                    });
                }
                #[allow(unreachable_patterns)]
                _ => {
                    return Err(SdkError::Turn {
                        detail: "the host answered with an unknown submit result".to_owned(),
                    });
                }
            }
        }
        Err(SdkError::Turn {
            detail: "the session kept moving; the prompt could not be sent".to_owned(),
        })
    }

    /// `true`, wenn ein Ereignis an `cursor` neu ist und keine Wiedergabe.
    fn is_new(&self, cursor: &Cursor) -> bool {
        *cursor >= self.head && self.last_seen.as_ref().is_none_or(|seen| cursor > seen)
    }

    async fn next_frame(&mut self) -> Result<FrameEnvelope> {
        if self.source.is_none() {
            let from = self.resume.take();
            self.reattach(from).await?;
        }
        let source = self.source.as_mut().ok_or_else(|| SdkError::Session {
            detail: "the session is not attached".to_owned(),
        })?;
        match source.next().await {
            Ok(Some(envelope)) => Ok(envelope),
            Ok(None) => {
                self.source = None;
                Err(SdkError::Session {
                    detail: "the host closed the session stream".to_owned(),
                })
            }
            Err(error) => {
                self.source = None;
                Err(session_error(error))
            }
        }
    }

    /// Hängt sich neu an; ein Fehlschlag lässt keine Quelle zurück und merkt
    /// sich das Ziel, damit der nächste Versuch es noch kennt.
    async fn reattach(&mut self, from: Option<Cursor>) -> Result<()> {
        self.source = None;
        self.resume = from;
        let (ack, source) = self
            .port
            .attach(AttachParams {
                session_id: self.wire_id.clone(),
                from,
                profile: if self.compact {
                    StreamProfile::Compact
                } else {
                    StreamProfile::Full
                },
                tail_items: DEFAULT_TAIL_ITEMS,
            })
            .await
            .map_err(session_error)?;
        self.may_approve = ack.granted.approve;
        // Nach einem Verlust zählt alles ab dem Ziel als neu (es wurde nicht
        // gesehen); sonst gilt der Stand, den der Host jetzt meldet.
        self.head = from.unwrap_or(ack.head);
        self.last_seen = None;
        self.source = Some(source);
        self.resume = None;
        Ok(())
    }

    async fn on_frame(
        &mut self,
        envelope: FrameEnvelope,
        got: &mut Collected,
        on_event: &mut dyn FnMut(&SdkEvent),
    ) -> Result<()> {
        let fresh = self.is_new(&envelope.cursor);
        match envelope.frame {
            SessionFrame::Turn(event) if fresh => {
                self.last_seen = Some(envelope.cursor);
                let source = EventSource {
                    session_id: self.id.clone(),
                    parent: None,
                    role: "assistant".to_owned(),
                };
                if let Some(mapped) = map_turn_event(source, event) {
                    collect(&mapped, got);
                    on_event(&mapped);
                }
            }
            SessionFrame::Child {
                agent,
                parent,
                role,
                event,
            } if fresh => {
                self.last_seen = Some(envelope.cursor);
                let source = EventSource {
                    session_id: SessionId::from_core(&agent),
                    parent: Some(
                        parent
                            .as_ref()
                            .map_or_else(|| self.id.clone(), SessionId::from_core),
                    ),
                    role,
                };
                if let Some(mapped) = map_turn_event(source, event) {
                    on_event(&mapped);
                }
            }
            SessionFrame::ApprovalRequested(request) => self.approve(&request, got).await?,
            SessionFrame::Resync { head, .. } => self.reattach(Some(head)).await?,
            SessionFrame::Lagged { resume_from } => self.reattach(Some(resume_from)).await?,
            _ => {}
        }
        Ok(())
    }

    /// Fragt den Handler und meldet die Entscheidung.
    async fn approve(
        &mut self,
        request: &harw_protocol::ApprovalRequest,
        got: &mut Collected,
    ) -> Result<()> {
        if !self.may_approve || !self.answered.insert(request.id.as_str().to_owned()) {
            return Ok(());
        }
        got.approvals += 1;
        let mapped = map_request(&self.id, request);
        let (decision, reason) = match self.approvals.decide(&mapped).await {
            Decision::Approve => (ReviewDecision::Approved, None),
            Decision::Deny { reason } => (ReviewDecision::Rejected, Some(reason)),
            #[allow(unreachable_patterns)]
            _ => (ReviewDecision::Rejected, Some("denied".to_owned())),
        };
        let answer = self
            .port
            .respond(ApprovalRespondParams {
                request_id: ApprovalId::from_str(request.id.as_str()),
                decision,
                reason,
            })
            .await
            .map_err(|error| SdkError::Approval {
                detail: error.to_string(),
            })?;
        match answer {
            // Ein anderes Gerät war schneller oder die Frist lief ab: der
            // Turn geht trotzdem weiter, das ist kein Fehler dieses Clients.
            RespondResult::Resolved
            | RespondResult::AlreadyResolved { .. }
            | RespondResult::Expired => Ok(()),
            RespondResult::Denied { reason } => Err(SdkError::Approval {
                detail: format!("not allowed to decide: {reason}"),
            }),
            #[allow(unreachable_patterns)]
            _ => Err(SdkError::Approval {
                detail: "the host answered with an unknown result".to_owned(),
            }),
        }
    }
}

/// Zählt ein Ereignis der Wurzelsitzung in den Bericht.
fn collect(event: &SdkEvent, got: &mut Collected) {
    match event {
        SdkEvent::Message {
            source,
            text,
            final_answer,
        } if source.is_root() => {
            got.last_text = Some(text.clone());
            if *final_answer {
                got.final_text = Some(text.clone());
            }
        }
        SdkEvent::ToolCall { source, .. } if source.is_root() => got.tool_calls += 1,
        SdkEvent::Finished {
            source,
            status,
            usage,
        } if source.is_root() => {
            got.finished += 1;
            got.usage = usage.unwrap_or_default();
            got.status = Some(match status {
                FinishStatus::Completed => TurnStatus::Completed,
                FinishStatus::Aborted => TurnStatus::Cancelled {
                    reason: "user".to_owned(),
                },
                #[allow(unreachable_patterns)]
                _ => TurnStatus::Completed,
            });
        }
        SdkEvent::Error {
            source, message, ..
        } if source.is_root() => {
            got.finished += 1;
            got.status = Some(TurnStatus::Failed {
                reason: message.clone(),
            });
        }
        _ => {}
    }
}

/// Übersetzt eine Freigabeanfrage des Hosts in die SDK-Form.
///
/// Der Host kennt keine Aufruf-Kennung der Runtime; `call_id` trägt die
/// Kennung des Governance-Handles der Anfrage.
fn map_request(session: &SessionId, request: &harw_protocol::ApprovalRequest) -> ApprovalRequest {
    let (tool, arguments) = match &request.kind {
        ApprovalKind::DynamicTool {
            tool_name,
            arguments,
            ..
        } => (tool_name.clone(), arguments.clone()),
        ApprovalKind::Exec {
            command,
            cwd,
            reasoning,
            ..
        } => (
            "shell.exec".to_owned(),
            serde_json::json!({ "command": command, "cwd": cwd, "reasoning": reasoning }),
        ),
        ApprovalKind::Patch { changes, .. } => {
            let mut files: Vec<&String> = changes.keys().collect();
            files.sort();
            ("fs.patch".to_owned(), serde_json::json!({ "files": files }))
        }
    };
    ApprovalRequest {
        session_id: session.clone(),
        request_id: request.id.as_str().to_owned(),
        call_id: request.work_id.as_str().to_owned(),
        tool,
        arguments,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_protocol::ApprovalRequest as Wire;
    use harw_types::{RiskLevel, TurnId, WorkId};

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    fn wire(kind: ApprovalKind) -> Wire {
        let now = jiff::Timestamp::UNIX_EPOCH;
        Wire {
            id: ApprovalId::from_str("a1"),
            work_id: WorkId::from_str("w1"),
            kind,
            summary: "x".to_owned(),
            risk: RiskLevel::Low,
            requested_at: now,
            timeout_at: Wire::default_timeout_at(now),
            decisions: vec![ReviewDecision::Approved, ReviewDecision::Rejected],
        }
    }

    #[test]
    fn a_dynamic_tool_request_maps_name_and_arguments() -> TestResult {
        let session = SessionId::new("s1")?;
        let mapped = map_request(
            &session,
            &wire(ApprovalKind::DynamicTool {
                turn_id: TurnId::from_str("t"),
                tool_name: "fs.write".to_owned(),
                arguments: serde_json::json!({"path": "a"}),
            }),
        );
        assert_eq!(mapped.tool, "fs.write");
        assert_eq!(mapped.arguments, serde_json::json!({"path": "a"}));
        assert_eq!(
            (mapped.request_id.as_str(), mapped.call_id.as_str()),
            ("a1", "w1")
        );
        Ok(())
    }

    #[test]
    fn exec_and_patch_requests_get_stable_tool_names_and_no_diff_bodies() -> TestResult {
        let session = SessionId::new("s1")?;
        let exec = map_request(
            &session,
            &wire(ApprovalKind::Exec {
                turn_id: TurnId::from_str("t"),
                command: vec!["ls".to_owned(), "-l".to_owned()],
                cwd: "/w".to_owned(),
                reasoning: None,
            }),
        );
        assert_eq!(exec.tool, "shell.exec");
        assert_eq!(exec.arguments["command"], serde_json::json!(["ls", "-l"]));

        let mut changes = std::collections::HashMap::new();
        changes.insert("b.rs".to_owned(), "SECRET DIFF".to_owned());
        changes.insert("a.rs".to_owned(), "SECRET DIFF".to_owned());
        let patch = map_request(
            &session,
            &wire(ApprovalKind::Patch {
                turn_id: TurnId::from_str("t"),
                changes,
            }),
        );
        assert_eq!(patch.tool, "fs.patch");
        assert_eq!(
            patch.arguments,
            serde_json::json!({"files": ["a.rs", "b.rs"]})
        );
        assert!(!patch.arguments.to_string().contains("SECRET"));
        Ok(())
    }

    #[test]
    fn positions_order_by_generation_then_durable_then_live() {
        let at = |generation, durable, live| Cursor {
            generation,
            durable,
            live,
        };
        assert!(at(2, 0, 0) > at(1, 99, 99));
        assert!(at(1, 5, 0) > at(1, 4, 9));
        assert!(at(1, 5, 2) > at(1, 5, 1));
        assert!(at(1, 5, 1) == at(1, 5, 1));
    }

    #[test]
    fn the_report_counts_only_the_root() {
        let root = EventSource {
            session_id: SessionId::from_core(&harw_types::SessionId::from_str("r")),
            parent: None,
            role: "assistant".to_owned(),
        };
        let mut child = root.clone();
        child.parent = Some(root.session_id.clone());
        let mut got = Collected::default();
        collect(
            &SdkEvent::ToolCall {
                source: root.clone(),
                call_id: "c".into(),
                tool: "t".into(),
                arguments: serde_json::Value::Null,
            },
            &mut got,
        );
        collect(
            &SdkEvent::ToolCall {
                source: child.clone(),
                call_id: "c2".into(),
                tool: "t".into(),
                arguments: serde_json::Value::Null,
            },
            &mut got,
        );
        collect(
            &SdkEvent::Finished {
                source: child,
                status: FinishStatus::Completed,
                usage: None,
            },
            &mut got,
        );
        assert_eq!(got.tool_calls, 1);
        assert_eq!(got.finished, 0, "a child's end is not the turn's end");
        collect(
            &SdkEvent::Finished {
                source: root,
                status: FinishStatus::Completed,
                usage: None,
            },
            &mut got,
        );
        assert_eq!(got.finished, 1);
    }

    // --- Wiedergabe gegen einen scriptbaren Port ------------------------------

    use std::collections::VecDeque;
    use std::sync::Mutex;

    use harw_protocol::PortFuture;
    use harw_protocol::TurnEvent;
    use harw_protocol::items::{AssistantMessageItem, ContentPart};
    use harw_protocol::session_wire::{
        AttachAck, HelloAck, HelloParams, HistoryParams, SessionSummary, SetEffortParams,
        SetModeParams, SetModelParams,
    };

    type Queue = Arc<Mutex<VecDeque<FrameEnvelope>>>;

    /// Liefert Frames aus einer Warteschlange, sonst nie etwas.
    struct QueueSource(Queue);

    impl FrameSource for QueueSource {
        fn next(&mut self) -> PortFuture<'_, Option<FrameEnvelope>> {
            let next = self.0.lock().ok().and_then(|mut q| q.pop_front());
            Box::pin(async move {
                match next {
                    Some(frame) => Ok(Some(frame)),
                    None => std::future::pending().await,
                }
            })
        }
    }

    /// Ein Port, dessen `submit` die vorbereiteten Frames in den Strom legt:
    /// zuerst späte Wiedergabe, dann die Frames des eigenen Turns.
    struct ScriptPort {
        queue: Queue,
        after_submit: Mutex<Vec<FrameEnvelope>>,
    }

    fn unsupported<T: Send + 'static>() -> PortFuture<'static, T> {
        Box::pin(async { Err(PortError::Protocol("not scripted".to_owned())) })
    }

    impl SessionPort for ScriptPort {
        fn hello(&self, _: HelloParams) -> PortFuture<'_, HelloAck> {
            unsupported()
        }
        fn list(&self) -> PortFuture<'_, Vec<SessionSummary>> {
            unsupported()
        }
        fn create(&self, _: CreateParams) -> PortFuture<'_, SessionSummary> {
            unsupported()
        }
        fn attach(&self, _: AttachParams) -> PortFuture<'_, (AttachAck, Box<dyn FrameSource>)> {
            unsupported()
        }
        fn detach(&self, _: harw_types::SessionId) -> PortFuture<'_, ()> {
            unsupported()
        }
        fn history(&self, _: HistoryParams) -> PortFuture<'_, Vec<FrameEnvelope>> {
            unsupported()
        }
        fn submit(&self, _: SubmitParams) -> PortFuture<'_, SubmitResult> {
            if let (Ok(mut queue), Ok(mut script)) = (self.queue.lock(), self.after_submit.lock()) {
                queue.extend(script.drain(..));
            }
            Box::pin(async { Ok(SubmitResult::Accepted { position: 0 }) })
        }
        fn interrupt(&self, _: InterruptParams) -> PortFuture<'_, ()> {
            unsupported()
        }
        fn resume(&self, _: harw_types::SessionId) -> PortFuture<'_, ()> {
            unsupported()
        }
        fn close(&self, _: harw_types::SessionId) -> PortFuture<'_, ()> {
            unsupported()
        }
        fn respond(&self, _: ApprovalRespondParams) -> PortFuture<'_, RespondResult> {
            unsupported()
        }
        fn set_model(&self, _: SetModelParams) -> PortFuture<'_, ()> {
            unsupported()
        }
        fn set_mode(&self, _: SetModeParams) -> PortFuture<'_, ()> {
            unsupported()
        }
        fn set_effort(&self, _: SetEffortParams) -> PortFuture<'_, ()> {
            unsupported()
        }
    }

    fn at(generation: u32, durable: u64, live: u32, event: TurnEvent) -> FrameEnvelope {
        FrameEnvelope {
            session_id: harw_types::SessionId::from_str("s1"),
            cursor: Cursor {
                generation,
                durable,
                live,
            },
            frame: SessionFrame::Turn(event),
        }
    }

    fn answer(text: &str) -> TurnEvent {
        TurnEvent::ItemAdded {
            turn_id: TurnId::from_str("t"),
            item: TurnItem::AssistantMessage(AssistantMessageItem {
                id: harw_types::ItemId::from_str(format!("m-{text}")),
                content: vec![ContentPart::Text {
                    text: text.to_owned(),
                }],
                phase: Some(harw_types::MessagePhase::FinalAnswer),
            }),
        }
    }

    fn done() -> TurnEvent {
        TurnEvent::TurnCompleted {
            turn_id: TurnId::from_str("t"),
            usage: None,
        }
    }

    use harw_protocol::items::TurnItem;

    fn scripted(
        before: Vec<FrameEnvelope>,
        after: Vec<FrameEnvelope>,
        head: Cursor,
    ) -> RemoteSession {
        let queue: Queue = Arc::new(Mutex::new(before.into()));
        let port = Arc::new(ScriptPort {
            queue: Arc::clone(&queue),
            after_submit: Mutex::new(after),
        });
        RemoteSession {
            id: SessionId::from_core(&harw_types::SessionId::from_str("s1")),
            wire_id: harw_types::SessionId::from_str("s1"),
            port,
            approvals: default_handler(),
            compact: false,
            turn_timeout: None,
            source: Some(Box::new(QueueSource(queue))),
            head,
            last_seen: None,
            resume: None,
            may_approve: true,
            answered: HashSet::new(),
            key_prefix: "test".to_owned(),
            sent: 0,
        }
    }

    #[tokio::test]
    async fn replay_before_and_after_the_submit_is_not_the_new_turn() -> TestResult {
        let head = Cursor {
            generation: 0,
            durable: 5,
            live: 0,
        };
        // Wiedergabe des alten Turns liegt vor dem Stand des Anhängens.
        let before = vec![at(0, 3, 0, answer("old")), at(0, 4, 0, done())];
        // Nach dem Absenden kommt erst noch späte Wiedergabe, dann der neue
        // Turn; sein erster Frame liegt genau auf dem Stand des Anhängens.
        let after = vec![
            at(0, 4, 0, done()),
            at(
                0,
                5,
                0,
                TurnEvent::TurnStarted {
                    turn_id: TurnId::from_str("t"),
                    thread_id: harw_types::ThreadId::from_str("th"),
                },
            ),
            at(0, 5, 1, answer("new")),
            at(0, 5, 2, done()),
        ];
        let mut session = scripted(before, after, head);
        let mut events = 0_usize;
        let report = tokio::time::timeout(
            Duration::from_secs(5),
            session.send_with("go", |_| events += 1),
        )
        .await??;
        assert_eq!(
            report.text.as_deref(),
            Some("new"),
            "not the replayed answer"
        );
        assert_eq!(report.status, TurnStatus::Completed);
        assert_eq!(events, 3, "started, message, finished; no replayed event");
        Ok(())
    }

    #[tokio::test]
    async fn a_prompt_queued_behind_a_running_turn_waits_for_its_own_end() -> TestResult {
        // Ein zweites Ende wird nur mitgezählt, wenn der Host `position > 0` meldete;
        // hier meldet der Port 0: das erste Ende beendet den Turn.
        let head = Cursor::default();
        let after = vec![
            at(0, 0, 0, answer("only")),
            at(0, 0, 1, done()),
            at(0, 0, 2, answer("never read")),
        ];
        let mut session = scripted(Vec::new(), after, head);
        let report = tokio::time::timeout(Duration::from_secs(5), session.send("go")).await??;
        assert_eq!(report.text.as_deref(), Some("only"));
        Ok(())
    }
}
