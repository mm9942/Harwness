//! Serverseitige Hälfte der Bestätigungsfläche (Knoten UI-06, A-APPR) — löst
//! eine [`harw_session_store::approval::ApprovalRecord`] auf, autorisiert
//! dabei aber **nichts**.
//!
//! # Verantwortungsbereich
//! Dieses Modul zeigt anstehende Genehmigungsanfragen an und nimmt eine
//! menschliche Entscheidung entgegen. Es führt keine irreversible Aktion
//! aus und ist kein zweiter Autoritätspfad zu `authorize` in
//! `harw-dod-escalate` (`pub(crate)`, von außen nachweislich unerreichbar).
//! Ein `ApprovalRecord` wird aufgelöst — mehr nicht. Die Aktion selbst führt
//! der Warden aus, nachdem die Eskalationsleiter einen
//! `AuthorizationProof` ausgestellt hat, der über `bound_action` an genau
//! diese eine Aktion gebunden ist. Dieses Modul kürzt diese Kette nicht ab:
//! es ruft `harw-dod-escalate` und `harw-dod-warden` an keiner Stelle auf.
//!
//! # Wie dieses Modul erreichbar wird
//! Wie jede `harw-web`-Fläche wird auch diese ausschließlich über eine
//! `Surface::Web`-Deklaration in einer [`harw_operations::operation::OperationMeta`]
//! erreichbar (siehe `crate`-Moduldoku, Abschnitt „Warum es keinen zweiten
//! Autoritätspfad gibt"): `approval.pending` (`web(path =
//! "/api/approval-pending", method = "get", approval = "none")`) und
//! `approval.resolve` (`web(path = "/api/approval-resolve", method = "post",
//! approval = "none")`, siehe `harw-ops/src/approval.rs`). Ihr `run()` bindet
//! die Funktionen dieses Moduls ein, keine eigene, parallele Logik.
//!
//! # Wer darf bestätigen (A-APPR, F-172, F-121, G-011)
//! Der [`ApprovalActor`] eines Aufrufers entsteht **ausschließlich** aus
//! seinem vertrauenswürdigen [`Principal`] über [`Principal::actor_id`] —
//! nie aus einem Feld des Anfragerumpfs. [`ApprovalCaller::actor`] prüft in
//! dieser Reihenfolge:
//!
//! 1. `principal.actor_id()` ist `Some` — sonst
//!    [`SecurityError::NoApproverActor`]. Modelle, Kind-Agenten, Operationen
//!    und nicht zugelassene Kanäle haben keinen Actor und dürfen **nie**
//!    auflösen, auch wenn sie die Operation irgendwie erreichen.
//! 2. Kommt der Principal über [`IngressSurface::Web`], muss zusätzlich ein
//!    über `SO_PEERCRED` authentifizierter Peer samt serverseitigem
//!    [`ApprovalActorResolver`] vorliegen — sonst
//!    [`SecurityError::UnauthenticatedWebPeer`]. Der Resolver muss den Peer
//!    kennen ([`SecurityError::UnknownApprover`]) und auf **denselben** Actor
//!    abbilden wie der Principal ([`SecurityError::ApproverIdentityMismatch`]).
//!    Damit bekommt ein Web-Peer, dem der `PeerAuthorizer` (Tier) zwar Zutritt
//!    gewährt, der aber nicht in der Genehmiger-Tabelle steht, nie den
//!    `owner`-Actor, den `Principal::actor_id` für jeden `Human × Web`-Principal
//!    liefert.
//!
//! Danach entscheidet [`ApprovalStore::resolve`] über Einmaligkeit,
//! Actor-Bindung (`ApprovalActorMismatch`) und TTL (`ApprovalExpired`).
//!
//! **Grenzen (Ledger `docs/remediation/ledger/W4a/A-APPR.md`):**
//! - *Transport:* Der Unix-Socket authentisiert heute nur über die UID. Ein
//!   Same-UID-Prozess (auch ein modellgestarteter) ist vom Bediener nicht zu
//!   unterscheiden. Die Token-Pflicht für `approval.resolve` auch am
//!   Unix-Socket ist WB-SRV (W5).
//! - *Selbstgenehmigung:* `ApprovalRecord::actor` ist der gebundene
//!   **Beantworter**, nicht der Anfragende; der Anfragende wird nirgends
//!   persistiert. Eine Prüfung „Anfragender ≠ Beantworter" ist ohne neues
//!   Feld nicht ehrlich möglich und wird hier deshalb **nicht** vorgetäuscht
//!   (C-APPR-Folgearbeit).
//!
//! # Serveruhr statt Client-Zeit (F-122)
//! Kein Parameter dieses Moduls nimmt einen Zeitstempel vom Aufrufer an.
//! `resolved_at` und die TTL-Prüfung laufen über eine übergebene
//! [`Clock`] — produktiv [`harw_types::SystemClock`], in Tests eine feste Uhr.
//!
//! # Was fehlt, um die Genehmigungsanfrage anzuzeigen
//! [`harw_session_store::approval::ApprovalRecord`] trägt `request`,
//! `session`, `call_id`, `actor`, `issued_at` — **keinen** Befundtext und
//! keine `WardenAction`/`Reversibility`. Die eigentliche Anfrage (welche
//! Aktion, welche cgroup, welche Umkehrbarkeit) muss über `call_id` aus dem
//! Transkript/der Werkzeugaufruf-Historie nachgeschlagen werden — dieses
//! Modul erfindet dafür keine eigene Ablage, sondern nimmt den bereits
//! aufgelösten Anzeigeinhalt als Parameter entgegen (siehe
//! [`PendingApprovalView`]). Welche Operation `call_id` in einen
//! Anzeigeinhalt auflöst, ist noch offen.
//!
//! # Nebenläufigkeit
//! Alle Funktionen sind zustandslos; [`ApprovalActorResolver`] ist
//! `Send + Sync` und wird hinter `Arc` geteilt. Sperren hält ausschließlich
//! [`ApprovalStore::resolve`] (Datei-Lock je Sitzung).
//!
//! # Fehlertypen
//! [`SecurityError`] — lokale Ablehnungen (kein Actor, kein authentifizierter
//! Web-Peer, unbekannter Genehmiger, abweichende Identität) sowie, über
//! `From`, jeden [`harw_session_store::error::SessionStoreError`] unverändert.
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_session_store::approval::ApprovalStore;
//! use harw_types::{
//!     IngressSurface, ItemId, PermissionTier, Principal, PrincipalKind, ReviewDecision,
//!     SessionId, SystemClock,
//! };
//! use harw_web::peer::PeerCredentials;
//! use harw_web::security::{ApprovalCaller, StaticUidApprovalActorMap, resolve_approval};
//!
//! let store = ApprovalStore::new(std::path::Path::new("/tmp/harw-home"));
//! let principal = Principal::trusted_ingress(
//!     PrincipalKind::Human,
//!     "uid:1000",
//!     IngressSurface::Web,
//!     PermissionTier::Owner,
//! );
//! let resolver = StaticUidApprovalActorMap::new(vec![(1000, "owner".to_owned())]);
//! let peer = PeerCredentials::new(42, 1000, 1000);
//! let caller = ApprovalCaller::new(&principal).with_web_peer(&peer, &resolver);
//! let resolution = resolve_approval(
//!     &store,
//!     &caller,
//!     &SessionId::from_str("session-1"),
//!     &ItemId::from_str("approval-1"),
//!     ReviewDecision::Approved,
//!     None,
//!     &SystemClock,
//! )?;
//! assert_eq!(resolution.decision, ReviewDecision::Approved);
//! # Ok::<(), harw_web::security::SecurityError>(())
//! ```

use harw_session_store::approval::{ApprovalRecord, ApprovalResolutionRecord, ApprovalStore};
use harw_session_store::error::SessionStoreError;
use harw_types::{
    ApprovalActor, Clock, IngressSurface, ItemId, Principal, PrincipalKind, ReviewDecision,
    SessionId,
};

use crate::peer::PeerCredentials;

/// Löst die Genehmiger-Identität eines über `SO_PEERCRED` identifizierten
/// Peers auf: welcher [`ApprovalActor`] entspricht ihm.
///
/// # Description
/// Strukturell identisch zu [`crate::authz::PeerAuthorizer`] — eine bei der
/// Komposition des Servers aus vertrauenswürdiger, serverseitiger
/// Konfiguration gebaute Richtlinie, nie aus dem HTTP-Request abgeleitet.
/// Für Web-Principals ist sie die zweite, UID-gebundene Hälfte der
/// Identitätsprüfung in [`ApprovalCaller::actor`].
///
/// # Concurrency
/// Muss `Send + Sync` sein: Implementierungen werden hinter `Arc` über alle
/// Verbindungs-Tasks geteilt, wie [`crate::authz::PeerAuthorizer`].
pub trait ApprovalActorResolver: Send + Sync {
    /// Liefert den `ApprovalActor` für `peer`, falls bekannt.
    ///
    /// # Arguments
    /// - `peer` (`&PeerCredentials`): die über `SO_PEERCRED` gelesene Identität.
    ///
    /// # Returns
    /// - `Some(actor)`: der Peer ist einem `ApprovalActor` zugeordnet.
    /// - `None`: der Peer ist unbekannt; der Aufrufer darf keine
    ///   Genehmigungsanfrage auflösen (kein impliziter Rückfall auf einen
    ///   generischen Actor).
    fn actor_for(&self, peer: &PeerCredentials) -> Option<ApprovalActor>;
}

/// Statische, UID-basierte Zuordnung von Peer zu [`ApprovalActor::Operator`] —
/// die einfachste Implementierung von [`ApprovalActorResolver`].
///
/// # Description
/// Bildet effektive Nutzer-IDs auf eine `ApprovalActor::Operator`-Kennung
/// ab. Eine nicht gelistete UID bleibt unautorisiert
/// ([`ApprovalActorResolver::actor_for`] liefert `None`) — es gibt bewusst
/// keinen Rückfallwert, anders als bei [`crate::authz::StaticUidTierMap`]:
/// ein unbekannter Genehmiger ist niemals ein akzeptabler Standardwert.
///
/// # Examples
/// ```rust
/// use harw_types::ApprovalActor;
/// use harw_web::peer::PeerCredentials;
/// use harw_web::security::{ApprovalActorResolver, StaticUidApprovalActorMap};
///
/// let resolver = StaticUidApprovalActorMap::new(vec![(1000, "alice".to_owned())]);
/// assert_eq!(
///     resolver.actor_for(&PeerCredentials::new(1, 1000, 1000)),
///     Some(ApprovalActor::Operator { id: "alice".to_owned() })
/// );
/// assert_eq!(resolver.actor_for(&PeerCredentials::new(2, 9999, 9999)), None);
/// ```
pub struct StaticUidApprovalActorMap {
    entries: Vec<(u32, String)>,
}

impl StaticUidApprovalActorMap {
    /// Baut eine Zuordnung aus UID-zu-Operator-Kennung-Paaren.
    ///
    /// # Arguments
    /// - `entries` (`Vec<(u32, String)>`): UID-zu-Operator-Kennung-Paare.
    ///
    /// # Returns
    /// Eine neue `StaticUidApprovalActorMap`.
    #[must_use]
    pub fn new(entries: Vec<(u32, String)>) -> Self {
        Self { entries }
    }
}

impl ApprovalActorResolver for StaticUidApprovalActorMap {
    fn actor_for(&self, peer: &PeerCredentials) -> Option<ApprovalActor> {
        self.entries
            .iter()
            .find(|(uid, _)| *uid == peer.uid)
            .map(|(_, id)| ApprovalActor::Operator { id: id.to_owned() })
    }
}

/// Fehler dieses Moduls.
///
/// # Varianten
/// - [`SecurityError::NoApproverActor`]: der Principal hat keinen Actor.
/// - [`SecurityError::UnauthenticatedWebPeer`]: Web-Principal ohne
///   authentifizierten Peer bzw. Resolver.
/// - [`SecurityError::UnknownApprover`]: der Peer ist keinem
///   [`ApprovalActor`] zugeordnet.
/// - [`SecurityError::ApproverIdentityMismatch`]: Peer und Principal bilden
///   auf verschiedene Actors ab.
/// - [`SecurityError::Store`]: ein Fehler aus
///   [`harw_session_store::approval::ApprovalStore`] (z. B. bereits
///   aufgelöst, Actor-Mismatch, abgelaufen, nicht gefunden).
#[derive(Debug)]
pub enum SecurityError {
    /// Der Principal des Aufrufers bildet auf keinen `ApprovalActor` ab
    /// (Modell, Kind-Agent, Operation, nicht zugelassener Kanal).
    NoApproverActor {
        /// Art des abgelehnten Principals.
        kind: PrincipalKind,
        /// Eingangsfläche des abgelehnten Principals.
        surface: IngressSurface,
    },
    /// Ein Web-Principal ohne über `SO_PEERCRED` authentifizierten Peer oder
    /// ohne serverseitigen Genehmiger-Resolver.
    UnauthenticatedWebPeer,
    /// Der anfragende Peer ist keinem `ApprovalActor` zugeordnet.
    UnknownApprover {
        /// Die effektive UID des unbekannten Peers.
        uid: u32,
    },
    /// Der Peer bildet auf einen anderen `ApprovalActor` ab als der Principal.
    ApproverIdentityMismatch {
        /// Die effektive UID des Peers.
        uid: u32,
    },
    /// Ein durchgereichter Fehler des Genehmigungsspeichers.
    Store(SessionStoreError),
}

impl std::fmt::Display for SecurityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoApproverActor { kind, surface } => write!(
                f,
                "Aufrufer ({kind:?} über {surface:?}) darf keine Genehmigungsanfrage auflösen"
            ),
            Self::UnauthenticatedWebPeer => write!(
                f,
                "Web-Aufrufer ohne authentifizierte Peer-Credentials oder Genehmiger-Tabelle"
            ),
            Self::UnknownApprover { uid } => {
                write!(f, "Peer mit uid {uid} ist keinem Genehmiger zugeordnet")
            }
            Self::ApproverIdentityMismatch { uid } => write!(
                f,
                "Peer mit uid {uid} entspricht nicht dem Genehmiger des Aufrufers"
            ),
            Self::Store(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for SecurityError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Store(error) => Some(error),
            Self::NoApproverActor { .. }
            | Self::UnauthenticatedWebPeer
            | Self::UnknownApprover { .. }
            | Self::ApproverIdentityMismatch { .. } => None,
        }
    }
}

impl From<SessionStoreError> for SecurityError {
    fn from(error: SessionStoreError) -> Self {
        Self::Store(error)
    }
}

/// Vertrauenswürdige Identität eines Aufrufers von [`resolve_approval`].
///
/// # Description
/// Bündelt den [`Principal`] (Pflicht) und — für Web-Aufrufer — den über
/// `SO_PEERCRED` authentifizierten Peer samt serverseitigem
/// [`ApprovalActorResolver`]. Alle Felder sind privat; ein Actor lässt sich
/// nur über [`Self::actor`] gewinnen, nie direkt setzen.
///
/// # Concurrency
/// `Copy`, hält nur geteilte Referenzen; `Send + Sync`, weil alle
/// referenzierten Typen es sind.
///
/// # Examples
/// ```rust
/// use harw_types::{ApprovalActor, IngressSurface, PermissionTier, Principal, PrincipalKind};
/// use harw_web::security::ApprovalCaller;
///
/// let tui = Principal::trusted_ingress(
///     PrincipalKind::Human,
///     "mia",
///     IngressSurface::Tui,
///     PermissionTier::Owner,
/// );
/// assert_eq!(
///     ApprovalCaller::new(&tui).actor().ok(),
///     Some(ApprovalActor::Operator { id: "local-tui".to_owned() })
/// );
/// let child = tui.child_of("explorer");
/// assert!(ApprovalCaller::new(&child).actor().is_err());
/// ```
#[derive(Clone, Copy)]
pub struct ApprovalCaller<'a> {
    principal: &'a Principal,
    web_peer: Option<(&'a PeerCredentials, &'a dyn ApprovalActorResolver)>,
}

impl std::fmt::Debug for ApprovalCaller<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApprovalCaller")
            .field("principal", self.principal)
            .field("web_peer", &self.web_peer.map(|(peer, _)| peer))
            .finish()
    }
}

impl<'a> ApprovalCaller<'a> {
    /// Baut einen Aufrufer aus seinem vertrauenswürdigen Principal.
    ///
    /// # Arguments
    /// - `principal` (`&Principal`): an der Eingangsgrenze gebaute Identität.
    ///
    /// # Returns
    /// Einen `ApprovalCaller` ohne Web-Peer.
    #[must_use]
    pub fn new(principal: &'a Principal) -> Self {
        Self {
            principal,
            web_peer: None,
        }
    }

    /// Ergänzt den über `SO_PEERCRED` authentifizierten Peer und die
    /// serverseitige Genehmiger-Tabelle.
    ///
    /// # Arguments
    /// - `peer` (`&PeerCredentials`): Kernel-verbürgte Identität der Verbindung.
    /// - `resolver` (`&dyn ApprovalActorResolver`): UID → Actor-Tabelle.
    ///
    /// # Returns
    /// Denselben Aufrufer mit Web-Peer.
    #[must_use]
    pub fn with_web_peer(
        self,
        peer: &'a PeerCredentials,
        resolver: &'a dyn ApprovalActorResolver,
    ) -> Self {
        Self {
            principal: self.principal,
            web_peer: Some((peer, resolver)),
        }
    }

    /// Returns the principal of this caller.
    ///
    /// # Returns
    /// Den zugrunde liegenden [`Principal`].
    #[must_use]
    pub fn principal(&self) -> &'a Principal {
        self.principal
    }

    /// Bestimmt den `ApprovalActor` dieses Aufrufers (siehe Moduldoku,
    /// Abschnitt „Wer darf bestätigen").
    ///
    /// # Returns
    /// Den Actor aus [`Principal::actor_id`], für Web-Principals zusätzlich
    /// durch den Peer bestätigt.
    ///
    /// # Errors
    /// - [`SecurityError::NoApproverActor`]: `actor_id()` ist `None`.
    /// - [`SecurityError::UnauthenticatedWebPeer`]: Web-Principal ohne Peer/Resolver.
    /// - [`SecurityError::UnknownApprover`]: Resolver kennt den Peer nicht.
    /// - [`SecurityError::ApproverIdentityMismatch`]: Peer-Actor ≠ Principal-Actor.
    ///
    /// # Concurrency
    /// Reine Funktion.
    pub fn actor(&self) -> Result<ApprovalActor, SecurityError> {
        let principal = self.principal;
        let actor = principal
            .actor_id()
            .ok_or_else(|| SecurityError::NoApproverActor {
                kind: principal.kind(),
                surface: principal.surface(),
            })?;
        if principal.surface() != IngressSurface::Web {
            return Ok(actor);
        }
        let (peer, resolver) = self
            .web_peer
            .ok_or(SecurityError::UnauthenticatedWebPeer)?;
        let peer_actor = resolver
            .actor_for(peer)
            .ok_or(SecurityError::UnknownApprover { uid: peer.uid })?;
        if peer_actor != actor {
            return Err(SecurityError::ApproverIdentityMismatch { uid: peer.uid });
        }
        Ok(actor)
    }
}

/// Menschenlesbarer Anzeigeinhalt einer anstehenden Genehmigungsanfrage.
///
/// # Description
/// [`ApprovalRecord`] selbst trägt keinen Befundtext und keine
/// `WardenAction`/`Reversibility` (siehe Moduldoku, Abschnitt „Was fehlt,
/// um die Genehmigungsanfrage anzuzeigen"). Dieser Typ nimmt den bereits
/// aus `call_id` aufgelösten Anzeigeinhalt entgegen, statt ihn selbst zu
/// beschaffen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingApprovalView {
    /// Die durabel gespeicherte Anfrage selbst.
    pub record: ApprovalRecord,
    /// Maschinenlesbarer Name der angefragten, potenziell irreversiblen
    /// Aktion (z. B. `"warden.kill_process_tree"`,
    /// `harw_dod_warden_proto::WardenAction::kind_name`).
    pub action_kind: String,
    /// `true`, wenn die Aktion nicht umkehrbar ist
    /// (`harw_dod_warden_proto::Reversibility::Irreversible`). Muss der
    /// Bedienoberfläche sichtbar mitgeteilt werden — „Prozessbaum beenden"
    /// und „cgroup einfrieren" sehen als Knopf gleich aus und sind es
    /// nicht.
    pub irreversible: bool,
    /// Angreiferkontrollierter Befundtext, der die Anfrage begründet.
    /// Ausnahmslos über `DataBlock` anzuzeigen, niemals als Markup oder
    /// Markdown zu interpretieren.
    pub rationale: String,
}

/// Liest eine anstehende Genehmigungsanfrage — reine Anzeige, keine
/// Auflösung.
///
/// # Description
/// Dünner Aufruf von [`ApprovalStore::pending`]. Diese Funktion trifft
/// keine Zugriffsentscheidung — das ist Sache der `Surface::Web`-Deklaration
/// (Tier-Ablehnungsmatrix über `crate::router::decide_route`).
///
/// # Arguments
/// - `store` (`&ApprovalStore`): der Genehmigungsspeicher.
/// - `session` (`&SessionId`): die betroffene Sitzung.
/// - `request` (`&ItemId`): die Kennung der Genehmigungsanfrage.
///
/// # Returns
/// Der gespeicherte [`ApprovalRecord`].
///
/// # Errors
/// Jeder [`SessionStoreError`], den [`ApprovalStore::pending`] liefert
/// (insbesondere `ApprovalNotFound`), über [`SecurityError::Store`].
pub fn pending_approval(
    store: &ApprovalStore,
    session: &SessionId,
    request: &ItemId,
) -> Result<ApprovalRecord, SecurityError> {
    Ok(store.pending(session, request)?)
}

/// Listet offene, nicht abgelaufene Genehmigungsanfragen über alle Sitzungen.
///
/// # Description
/// Dünner Aufruf von [`ApprovalStore::pending_all`] — der einzige
/// Auflistungspfad; kein eigener Verzeichnis-Scan. Defekte Einträge
/// überspringt der Speicher selbst (mit `tracing::warn!`). Die TTL-Prüfung
/// läuft gegen `clock`, nie gegen eine Client-Zeit.
///
/// # Arguments
/// - `store` (`&ApprovalStore`): der Genehmigungsspeicher.
/// - `limit` (`usize`): Höchstzahl der Einträge (`0` → leer).
/// - `clock` (`&dyn Clock`): Serveruhr.
///
/// # Returns
/// Offene Anfragen, aufsteigend nach `issued_at`, höchstens `limit`.
///
/// # Errors
/// [`SecurityError::Store`] mit `Io`, wenn das Wurzelverzeichnis nicht
/// lesbar ist.
///
/// # Concurrency
/// Lock-freier Schnappschuss; die Autorität bleibt [`resolve_approval`].
pub fn list_pending_approvals(
    store: &ApprovalStore,
    limit: usize,
    clock: &dyn Clock,
) -> Result<Vec<ApprovalRecord>, SecurityError> {
    Ok(store.pending_all(limit, clock)?)
}

/// Löst eine anstehende Genehmigungsanfrage anhand einer menschlichen
/// Entscheidung auf.
///
/// # Description
/// Bestimmt den `ApprovalActor` **ausschließlich** über
/// [`ApprovalCaller::actor`] (Principal, für Web zusätzlich Peer) — es gibt
/// keinen Parameter, über den ein Anfragerumpf einen Actor oder einen
/// Zeitstempel einschleusen könnte. Danach delegiert sie an
/// [`ApprovalStore::resolve`] mit der Serveruhr `clock`; der Speicher prüft
/// Einmaligkeit, Actor-Bindung und TTL. Diese Funktion führt selbst **keine**
/// Aktion aus.
///
/// # Arguments
/// - `store` (`&ApprovalStore`): der Genehmigungsspeicher.
/// - `caller` (`&ApprovalCaller`): vertrauenswürdige Aufruferidentität.
/// - `session` (`&SessionId`): die betroffene Sitzung.
/// - `request` (`&ItemId`): die Kennung der Genehmigungsanfrage.
/// - `decision` (`ReviewDecision`): die menschliche Entscheidung.
/// - `comment` (`Option<String>`): optionaler Freitextkommentar.
/// - `clock` (`&dyn Clock`): Serveruhr für `resolved_at` und TTL.
///
/// # Returns
/// Der durabel geschriebene [`ApprovalResolutionRecord`];
/// `resolved_at == clock.now()`.
///
/// # Errors
/// - Jede Ablehnung aus [`ApprovalCaller::actor`] — der Speicher wird dann
///   nicht berührt.
/// - [`SecurityError::Store`]: u. a. `ApprovalActorMismatch`,
///   `ApprovalAlreadyResolved`, `ApprovalNotFound`, `ApprovalExpired`.
///
/// # Concurrency
/// Delegiert die Sperrung vollständig an [`ApprovalStore::resolve`]
/// (Datei-Lock je Sitzung).
pub fn resolve_approval(
    store: &ApprovalStore,
    caller: &ApprovalCaller<'_>,
    session: &SessionId,
    request: &ItemId,
    decision: ReviewDecision,
    comment: Option<String>,
    clock: &dyn Clock,
) -> Result<ApprovalResolutionRecord, SecurityError> {
    let actor = match caller.actor() {
        Ok(actor) => actor,
        Err(error) => {
            tracing::warn!(
                principal_kind = ?caller.principal().kind(),
                surface = ?caller.principal().surface(),
                session = %session,
                request = %request,
                error = %error,
                "approval.resolve rejected: caller is not an approver"
            );
            return Err(error);
        }
    };
    match store.resolve(session, request, decision, comment, &actor, clock) {
        Ok(resolution) => {
            tracing::info!(
                session = %session,
                request = %request,
                decision = ?resolution.decision,
                actor = ?resolution.actor,
                resolved_at = %resolution.resolved_at,
                "approval.resolve accepted"
            );
            Ok(resolution)
        }
        Err(error) => {
            tracing::warn!(
                session = %session,
                request = %request,
                error = %error,
                "approval.resolve rejected by store"
            );
            Err(SecurityError::Store(error))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ApprovalActorResolver, ApprovalCaller, PendingApprovalView, SecurityError,
        StaticUidApprovalActorMap, list_pending_approvals, pending_approval, resolve_approval,
    };
    use crate::peer::PeerCredentials;
    use harw_session_store::approval::{ApprovalRecord, ApprovalStore};
    use harw_session_store::error::SessionStoreError;
    use harw_types::{
        ApprovalActor, Clock, IngressSurface, ItemId, PermissionTier, Principal, PrincipalKind,
        ReviewDecision, SessionId, ToolCallId,
    };
    use jiff::{SignedDuration, Timestamp};

    // Feste Serveruhr für deterministische Zeit- und TTL-Tests.
    struct FixedClock(Timestamp);

    impl Clock for FixedClock {
        fn now(&self) -> Timestamp {
            self.0
        }
    }

    const ISSUED_SECS: i64 = 1_700_000_000;

    fn issued_at() -> Timestamp {
        Timestamp::constant(ISSUED_SECS, 0)
    }

    fn clock_after(offset: SignedDuration) -> FixedClock {
        // Testhilfe: Überlauf ist bei 1.7e9 s + Minuten ausgeschlossen.
        FixedClock(issued_at().checked_add(offset).expect("timestamp in range"))
    }

    fn peer(uid: u32) -> PeerCredentials {
        PeerCredentials::new(1, uid, uid)
    }

    fn resolver() -> StaticUidApprovalActorMap {
        StaticUidApprovalActorMap::new(vec![(1000, "owner".to_owned())])
    }

    fn web_principal() -> Principal {
        Principal::trusted_ingress(
            PrincipalKind::Human,
            "uid:1000",
            IngressSurface::Web,
            PermissionTier::Owner,
        )
    }

    fn owner() -> ApprovalActor {
        ApprovalActor::Operator {
            id: "owner".to_owned(),
        }
    }

    fn issue_pending(
        store: &ApprovalStore,
        request: &str,
        actor: ApprovalActor,
    ) -> (SessionId, ItemId) {
        let session = SessionId::from_str("session-1");
        let request = ItemId::from_str(request);
        let record = ApprovalRecord {
            request: request.clone(),
            session: session.clone(),
            call_id: ToolCallId::from_str("call-1"),
            actor,
            issued_at: issued_at(),
        };
        store.issue(&record).expect("issue pending approval");
        (session, request)
    }

    #[test]
    fn test_static_uid_approval_actor_map_resolves_listed_uid() {
        assert_eq!(resolver().actor_for(&peer(1000)), Some(owner()));
    }

    #[test]
    fn test_static_uid_approval_actor_map_unknown_uid_is_none() {
        assert_eq!(resolver().actor_for(&peer(9999)), None);
    }

    #[test]
    fn test_approval_caller_actor_rejects_model_principal() {
        let model = Principal::trusted_ingress(
            PrincipalKind::Model,
            "root",
            IngressSurface::Web,
            PermissionTier::Owner,
        );
        let web_peer = peer(1000);
        let table = resolver();
        let error = ApprovalCaller::new(&model)
            .with_web_peer(&web_peer, &table)
            .actor()
            .expect_err("model principals never approve");
        assert!(matches!(
            error,
            SecurityError::NoApproverActor {
                kind: PrincipalKind::Model,
                surface: IngressSurface::Web
            }
        ));
    }

    #[test]
    fn test_approval_caller_actor_rejects_child_principal() {
        let child = web_principal().child_of("explorer");
        let error = ApprovalCaller::new(&child)
            .actor()
            .expect_err("child principals never approve");
        assert!(matches!(
            error,
            SecurityError::NoApproverActor {
                surface: IngressSurface::Child,
                ..
            }
        ));
    }

    #[test]
    fn test_approval_caller_actor_web_without_peer_is_unauthenticated() {
        let principal = web_principal();
        let error = ApprovalCaller::new(&principal)
            .actor()
            .expect_err("web principal requires an authenticated peer");
        assert!(matches!(error, SecurityError::UnauthenticatedWebPeer));
    }

    #[test]
    fn test_approval_caller_actor_web_unknown_peer_is_rejected() {
        let principal = web_principal();
        let web_peer = peer(9999);
        let table = resolver();
        let error = ApprovalCaller::new(&principal)
            .with_web_peer(&web_peer, &table)
            .actor()
            .expect_err("peer outside the approver table");
        assert!(matches!(error, SecurityError::UnknownApprover { uid: 9999 }));
    }

    #[test]
    fn test_approval_caller_actor_web_peer_mapping_to_other_actor_is_mismatch() {
        let principal = web_principal();
        let web_peer = peer(1000);
        let table = StaticUidApprovalActorMap::new(vec![(1000, "alice".to_owned())]);
        let error = ApprovalCaller::new(&principal)
            .with_web_peer(&web_peer, &table)
            .actor()
            .expect_err("peer and principal disagree");
        assert!(matches!(
            error,
            SecurityError::ApproverIdentityMismatch { uid: 1000 }
        ));
    }

    #[test]
    fn test_approval_caller_actor_web_confirmed_peer_yields_principal_actor() {
        let principal = web_principal();
        let web_peer = peer(1000);
        let table = resolver();
        let actor = ApprovalCaller::new(&principal)
            .with_web_peer(&web_peer, &table)
            .actor()
            .expect("confirmed web approver");
        assert_eq!(actor, owner());
    }

    #[test]
    fn test_resolve_approval_rejects_caller_without_actor_without_touching_store() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = ApprovalStore::new(temp.path());
        let (session, request) = issue_pending(&store, "approval-1", owner());
        let child = web_principal().child_of("explorer");

        let error = resolve_approval(
            &store,
            &ApprovalCaller::new(&child),
            &session,
            &request,
            ReviewDecision::Approved,
            None,
            &clock_after(SignedDuration::from_mins(1)),
        )
        .expect_err("no actor → rejected");
        assert!(matches!(error, SecurityError::NoApproverActor { .. }));
        assert_eq!(store.resolution(&session, &request).expect("readable"), None);
    }

    #[test]
    fn test_resolve_approval_uses_server_clock_for_resolved_at() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = ApprovalStore::new(temp.path());
        let (session, request) = issue_pending(&store, "approval-1", owner());
        let principal = web_principal();
        let web_peer = peer(1000);
        let table = resolver();
        let clock = clock_after(SignedDuration::from_mins(5));

        let resolution = resolve_approval(
            &store,
            &ApprovalCaller::new(&principal).with_web_peer(&web_peer, &table),
            &session,
            &request,
            ReviewDecision::ApprovedOnce,
            Some("bounded exception".to_owned()),
            &clock,
        )
        .expect("resolution succeeds");
        assert_eq!(resolution.resolved_at, clock.0);
        assert_eq!(resolution.actor, owner());
        assert_eq!(resolution.decision, ReviewDecision::ApprovedOnce);
        let durable = store
            .resolution(&session, &request)
            .expect("readable")
            .expect("durable resolution");
        assert_eq!(durable.resolved_at, clock.0);
    }

    #[test]
    fn test_resolve_approval_expired_request_is_rejected() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = ApprovalStore::new(temp.path());
        let (session, request) = issue_pending(&store, "approval-1", owner());
        let principal = web_principal();
        let web_peer = peer(1000);
        let table = resolver();

        let error = resolve_approval(
            &store,
            &ApprovalCaller::new(&principal).with_web_peer(&web_peer, &table),
            &session,
            &request,
            ReviewDecision::Approved,
            None,
            &clock_after(SignedDuration::from_mins(31)),
        )
        .expect_err("expired");
        assert!(matches!(
            error,
            SecurityError::Store(SessionStoreError::ApprovalExpired { .. })
        ));
        assert_eq!(store.resolution(&session, &request).expect("readable"), None);
    }

    #[test]
    fn test_resolve_approval_second_confirmation_is_rejected_not_overwritten() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = ApprovalStore::new(temp.path());
        let (session, request) = issue_pending(&store, "approval-1", owner());
        let principal = web_principal();
        let web_peer = peer(1000);
        let table = resolver();
        let caller = ApprovalCaller::new(&principal).with_web_peer(&web_peer, &table);
        let clock = clock_after(SignedDuration::from_mins(1));

        resolve_approval(
            &store,
            &caller,
            &session,
            &request,
            ReviewDecision::Approved,
            None,
            &clock,
        )
        .expect("first resolution");
        let replay = resolve_approval(
            &store,
            &caller,
            &session,
            &request,
            ReviewDecision::Rejected,
            None,
            &clock,
        )
        .expect_err("replay");
        assert!(matches!(
            replay,
            SecurityError::Store(SessionStoreError::ApprovalAlreadyResolved { .. })
        ));
    }

    #[test]
    fn test_list_pending_approvals_excludes_resolved_and_uses_clock() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = ApprovalStore::new(temp.path());
        let (session, first) = issue_pending(&store, "approval-1", owner());
        issue_pending(&store, "approval-2", owner());
        let principal = web_principal();
        let web_peer = peer(1000);
        let table = resolver();
        let clock = clock_after(SignedDuration::from_mins(1));

        resolve_approval(
            &store,
            &ApprovalCaller::new(&principal).with_web_peer(&web_peer, &table),
            &session,
            &first,
            ReviewDecision::Rejected,
            None,
            &clock,
        )
        .expect("resolve first");

        let open = list_pending_approvals(&store, 10, &clock).expect("list");
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].request.as_str(), "approval-2");

        let later = clock_after(SignedDuration::from_mins(30));
        assert!(list_pending_approvals(&store, 10, &later).expect("list").is_empty());
    }

    #[test]
    fn test_pending_approval_not_found_is_reported_not_invented() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = ApprovalStore::new(temp.path());
        let error = pending_approval(
            &store,
            &SessionId::from_str("session-2"),
            &ItemId::from_str("does-not-exist"),
        )
        .expect_err("missing");
        assert!(matches!(
            error,
            SecurityError::Store(SessionStoreError::ApprovalNotFound { .. })
        ));
    }

    #[test]
    fn test_pending_approval_view_carries_irreversibility_flag() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = ApprovalStore::new(temp.path());
        let (session, request) = issue_pending(&store, "approval-1", owner());
        let record = pending_approval(&store, &session, &request).expect("pending");

        let view = PendingApprovalView {
            record,
            action_kind: "warden.kill_process_tree".to_owned(),
            irreversible: true,
            rationale: "<img src=x onerror=alert(1)>".to_owned(),
        };
        assert!(view.irreversible);
        assert_eq!(view.rationale, "<img src=x onerror=alert(1)>");
    }

    #[test]
    fn test_security_error_display_names_rejection() {
        let error = SecurityError::NoApproverActor {
            kind: PrincipalKind::Model,
            surface: IngressSurface::Child,
        };
        assert!(error.to_string().contains("darf keine Genehmigungsanfrage auflösen"));
        assert!(std::error::Error::source(&error).is_none());
        let mismatch = SecurityError::ApproverIdentityMismatch { uid: 7 };
        assert!(mismatch.to_string().contains("uid 7"));
    }
}
