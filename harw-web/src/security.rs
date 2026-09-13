//! Serverseitige Hälfte der Bestätigungsfläche (Knoten UI-06) — löst eine
//! [`harw_session_store::approval::ApprovalRecord`] auf, autorisiert dabei
//! aber **nichts**.
//!
//! # Verantwortungsbereich
//! Dieses Modul zeigt eine anstehende Genehmigungsanfrage an und nimmt eine
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
//! Autoritätspfad gibt"). `#[operation(...)]` kennt inzwischen ein
//! `web(...)`-Unterattribut (`harw-macros/src/operation.rs`), und **16**
//! Operationen tragen bereits eine `Surface::Web` — darunter
//! `approval.pending` (`web(path = "/api/approval-pending", readonly,
//! approval = "none")`) und `approval.resolve` (`web(path =
//! "/api/approval-resolve", approval = "none")`, siehe
//! `harw-ops/src/approval.rs`), die genau diese Fläche erreichbar machen.
//! Ihr `run()` bindet die Funktionen dieses Moduls ein, keine eigene,
//! parallele Logik.
//!
//! # Wer darf bestätigen — die ungeschlossene Stelle dieses Knotens
//! [`harw_session_store::approval::ApprovalStore::resolve`] verlangt einen
//! [`harw_types::ApprovalActor`], der **byte-für-byte** dem bei `issue()`
//! hinterlegten Actor entspricht. `harw-web` identifiziert einen Aufrufer
//! ausschließlich über [`crate::peer::PeerCredentials`] (`pid`/`uid`/`gid`,
//! `SO_PEERCRED`) — es gibt in `harw-web` (Stand dieses Knotens) **keine**
//! vorhandene Abbildung von einer `uid` auf einen `ApprovalActor`. Ein
//! `ApprovalActor` aus dem Anfragerumpf zu übernehmen ist ausdrücklich
//! verboten (der Absender wählt ihn sich selbst) — deshalb definiert dieses
//! Modul [`ApprovalActorResolver`], strukturell identisch zu
//! [`crate::authz::PeerAuthorizer`]: eine von außen (serverseitig
//! vertrauenswürdig, zur Komposition übergebene) Richtlinie, nie aus dem
//! Request abgeleitet. **Diese Richtlinie existiert bisher nirgends als
//! konkrete Instanz** — [`StaticUidApprovalActorMap`] ist die einfachste
//! Implementierung, aber welche `uid` welchem `ApprovalActor::Operator`
//! entspricht, muss derselben serverseitigen Konfiguration entstammen, die
//! auch `issue()` beim Ausstellen der Anfrage verwendet hat (sonst schlägt
//! [`harw_session_store::error::SessionStoreError::ApprovalActorMismatch`]
//! fehl, obwohl derselbe Mensch entscheidet). **Das ist der im
//! Abschlussbericht gemeldete Befund**, keine Verbesserung, die dieses
//! Modul selbst vornimmt.
//!
//! # Was fehlt, um die Genehmigungsanfrage anzuzeigen
//! [`harw_session_store::approval::ApprovalRecord`] trägt `request`,
//! `session`, `call_id`, `actor`, `issued_at` — **keinen** Befundtext und
//! keine `WardenAction`/`Reversibility`. Die eigentliche Anfrage (welche
//! Aktion, welche cgroup, welche Umkehrbarkeit) muss über `call_id` aus dem
//! Transkript/der Werkzeugaufruf-Historie nachgeschlagen werden — dieses
//! Modul erfindet dafür keine eigene Ablage, sondern nimmt den bereits
//! aufgelösten Anzeigeinhalt als Parameter entgegen (siehe
//! [`PendingApprovalView`]). **Auch das ist im Abschlussbericht gemeldet**:
//! welche Operation `call_id` in einen Anzeigeinhalt auflöst, ist ebenfalls
//! noch offen.
//!
//! # Keine Systemuhr
//! Keine Funktion in diesem Modul erzeugt einen Zeitstempel selbst —
//! `resolved_at` kommt immer vom Aufrufer (genau wie
//! `ApprovalStore::resolve` es bereits verlangt).
//!
//! # Fehlertypen
//! [`SecurityError`] — trägt sowohl den lokalen Fehler „Peer hat keinen
//! bekannten `ApprovalActor`" als auch, über `#[from]`, jeden
//! [`harw_session_store::error::SessionStoreError`] unverändert weiter.
//!
//! # Abhängigkeiten
//! `harw-web/Cargo.toml` führt `harw-session-store`, `harw-types` und `jiff`
//! bereits, und `harw-web/src/lib.rs` bindet dieses Modul bereits über
//! `pub mod security;` ein — dieses Modul kompiliert damit unverändert mit.

use harw_session_store::approval::{ApprovalRecord, ApprovalResolutionRecord, ApprovalStore};
use harw_session_store::error::SessionStoreError;
use harw_types::{ApprovalActor, ItemId, ReviewDecision, SessionId};
use jiff::Timestamp;

use crate::peer::PeerCredentials;

/// Löst die Berechtigungsstufe-analoge Identität auf: welcher
/// [`ApprovalActor`] entspricht einem über `SO_PEERCRED` identifizierten Peer.
///
/// # Description
/// Strukturell identisch zu [`crate::authz::PeerAuthorizer`] — eine bei der
/// Komposition des Servers aus vertrauenswürdiger, serverseitiger
/// Konfiguration gebaute Richtlinie, nie aus dem HTTP-Request abgeleitet.
/// Ein `ApprovalActor`, der aus dem Anfragerumpf stammt, ist wertlos: der
/// Absender wählt ihn sich selbst (siehe Moduldoku, Abschnitt „Wer darf
/// bestätigen").
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
            .map(|(_, id)| ApprovalActor::Operator { id: id.clone() })
    }
}

/// Fehler dieses Moduls.
///
/// # Varianten
/// - [`SecurityError::UnknownApprover`]: der Peer ist keinem
///   [`ApprovalActor`] zugeordnet — die Anfrage wird nicht aufgelöst.
/// - [`SecurityError::Store`]: ein Fehler aus
///   [`harw_session_store::approval::ApprovalStore`] (z. B. bereits
///   aufgelöst, Actor-Mismatch, nicht gefunden).
#[derive(Debug)]
pub enum SecurityError {
    /// Der anfragende Peer ist keinem `ApprovalActor` zugeordnet.
    UnknownApprover {
        /// Die effektive UID des unbekannten Peers.
        uid: u32,
    },
    /// Ein durchgereichter Fehler des Genehmigungsspeichers.
    Store(SessionStoreError),
}

impl std::fmt::Display for SecurityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownApprover { uid } => {
                write!(f, "Peer mit uid {uid} ist keinem Genehmiger zugeordnet")
            }
            Self::Store(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for SecurityError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::UnknownApprover { .. } => None,
            Self::Store(error) => Some(error),
        }
    }
}

impl From<SessionStoreError> for SecurityError {
    fn from(error: SessionStoreError) -> Self {
        Self::Store(error)
    }
}

/// Menschenlesbarer Anzeigeinhalt einer anstehenden Genehmigungsanfrage.
///
/// # Description
/// [`ApprovalRecord`] selbst trägt keinen Befundtext und keine
/// `WardenAction`/`Reversibility` (siehe Moduldoku, Abschnitt „Was fehlt,
/// um die Genehmigungsanfrage anzuzeigen"). Dieser Typ nimmt den bereits
/// aus `call_id` aufgelösten Anzeigeinhalt entgegen, statt ihn selbst zu
/// beschaffen — welche künftige Operation `call_id` in diese Felder
/// auflöst, ist im Abschlussbericht dieses Knotens gemeldet.
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
/// keine Zugriffsentscheidung — das ist Sache der künftigen
/// `Surface::Web`-Deklaration (Tier-Ablehnungsmatrix über
/// `crate::router::decide_route`), nicht dieses Moduls.
///
/// # Arguments
/// - `store` (`&ApprovalStore`): der Genehmigungsspeicher der Sitzung.
/// - `session` (`&SessionId`): die betroffene Sitzung.
/// - `request` (`&ItemId`): die Kennung der Genehmigungsanfrage.
///
/// # Returns
/// Der noch nicht aufgelöste [`ApprovalRecord`].
///
/// # Errors
/// Jeder [`SessionStoreError`], den [`ApprovalStore::pending`] liefert
/// (insbesondere `ApprovalNotFound`), unverändert weitergereicht über
/// [`SecurityError::Store`].
pub fn pending_approval(
    store: &ApprovalStore,
    session: &SessionId,
    request: &ItemId,
) -> Result<ApprovalRecord, SecurityError> {
    Ok(store.pending(session, request)?)
}

/// Löst eine anstehende Genehmigungsanfrage anhand einer menschlichen
/// Entscheidung auf.
///
/// # Description
/// Leitet den `ApprovalActor` **ausschließlich** aus `peer` über `resolver`
/// ab — nie aus einem vom Client mitgeschickten Wert (es gibt in dieser
/// Signatur keinen `actor`-Parameter, der aus einem Anfragerumpf stammen
/// könnte). Danach delegiert sie vollständig an
/// [`ApprovalStore::resolve`], das den Aktualitäts-, Actor- und
/// Einmaligkeitscheck durchführt. Diese Funktion führt selbst **keine**
/// Aktion aus — sie löst nur den `ApprovalRecord` auf; der Warden führt die
/// eigentliche Aktion aus, nachdem `harw-dod-escalate` einen an sie
/// gebundenen `AuthorizationProof` ausgestellt hat.
///
/// # Arguments
/// - `store` (`&ApprovalStore`): der Genehmigungsspeicher der Sitzung.
/// - `resolver` (`&dyn ApprovalActorResolver`): löst `peer` in einen
///   `ApprovalActor` auf.
/// - `peer` (`&PeerCredentials`): die über `SO_PEERCRED` gelesene Identität
///   des Bedieners, der die HTTP-Anfrage gestellt hat.
/// - `session` (`&SessionId`): die betroffene Sitzung.
/// - `request` (`&ItemId`): die Kennung der Genehmigungsanfrage.
/// - `decision` (`ReviewDecision`): die menschliche Entscheidung.
/// - `comment` (`Option<String>`): optionaler Freitextkommentar des Bedieners.
/// - `resolved_at` (`Timestamp`): vom Aufrufer bereitgestellter Zeitstempel
///   (dieses Modul liest keine Systemuhr).
///
/// # Returns
/// Der durabel geschriebene [`ApprovalResolutionRecord`].
///
/// # Errors
/// - [`SecurityError::UnknownApprover`]: `resolver` kennt `peer` nicht —
///   die Anfrage wird nicht aufgelöst.
/// - [`SecurityError::Store`]: u. a. `ApprovalActorMismatch` (der
///   aufgelöste Actor stimmt nicht mit dem bei `issue()` hinterlegten
///   überein), `ApprovalAlreadyResolved` (zweite Bestätigung derselben
///   Anfrage), `ApprovalNotFound`.
///
/// # Concurrency
/// Delegiert die Sperrung vollständig an [`ApprovalStore::resolve`]
/// (Datei-Lock je Sitzung); diese Funktion hält selbst keinen Zustand.
// Acht Argumente: Peer-Credentials, Store, Anfrage und Zeit gehören alle
// hierher; ein Sammelstruct verschöbe die Frage nur an den Aufrufer.
#[allow(clippy::too_many_arguments)]
pub fn resolve_approval(
    store: &ApprovalStore,
    resolver: &dyn ApprovalActorResolver,
    peer: &PeerCredentials,
    session: &SessionId,
    request: &ItemId,
    decision: ReviewDecision,
    comment: Option<String>,
    resolved_at: Timestamp,
) -> Result<ApprovalResolutionRecord, SecurityError> {
    let actor = resolver
        .actor_for(peer)
        .ok_or(SecurityError::UnknownApprover { uid: peer.uid })?;
    Ok(store.resolve(session, request, &actor, decision, comment, resolved_at)?)
}

#[cfg(test)]
mod tests {
    use super::{
        ApprovalActorResolver, PendingApprovalView, SecurityError, StaticUidApprovalActorMap,
        pending_approval, resolve_approval,
    };
    use crate::peer::PeerCredentials;
    use harw_session_store::approval::ApprovalStore;
    use harw_types::{ApprovalActor, ItemId, ReviewDecision, SessionId, ToolCallId};
    use jiff::Timestamp;

    fn peer(uid: u32) -> PeerCredentials {
        PeerCredentials::new(1, uid, uid)
    }

    fn resolver() -> StaticUidApprovalActorMap {
        StaticUidApprovalActorMap::new(vec![(1000, "alice".to_owned())])
    }

    fn issue_pending(store: &ApprovalStore) -> (SessionId, ItemId) {
        let session = SessionId::from_str("session-1");
        let request = ItemId::from_str("approval-1");
        let record = harw_session_store::approval::ApprovalRecord {
            request: request.clone(),
            session: session.clone(),
            call_id: ToolCallId::from_str("call-1"),
            actor: ApprovalActor::Operator {
                id: "alice".to_owned(),
            },
            issued_at: Timestamp::constant(0, 0),
        };
        store.issue(&record).unwrap();
        (session, request)
    }

    #[test]
    fn test_static_uid_approval_actor_map_resolves_listed_uid() {
        let resolver = resolver();
        assert_eq!(
            resolver.actor_for(&peer(1000)),
            Some(ApprovalActor::Operator {
                id: "alice".to_owned()
            })
        );
    }

    #[test]
    fn test_static_uid_approval_actor_map_unknown_uid_is_none() {
        let resolver = resolver();
        assert_eq!(resolver.actor_for(&peer(9999)), None);
    }

    #[test]
    fn test_resolve_approval_rejects_unknown_peer_without_touching_store() {
        let temp = tempfile::tempdir().unwrap();
        let store = ApprovalStore::new(temp.path());
        let (session, request) = issue_pending(&store);
        let resolver = resolver();

        let error = resolve_approval(
            &store,
            &resolver,
            &peer(9999),
            &session,
            &request,
            ReviewDecision::Approved,
            None,
            Timestamp::constant(1, 0),
        )
        .unwrap_err();
        assert!(matches!(error, SecurityError::UnknownApprover { uid: 9999 }));

        // Die Anfrage bleibt unangetastet — ein unbekannter Peer darf sie
        // weder auflösen noch in einen Fehlerzustand versetzen.
        assert!(pending_approval(&store, &session, &request).is_ok());
    }

    #[test]
    fn test_resolve_approval_derives_actor_from_peer_not_from_a_body_value() {
        let temp = tempfile::tempdir().unwrap();
        let store = ApprovalStore::new(temp.path());
        let (session, request) = issue_pending(&store);
        let resolver = resolver();

        // Es gibt in der Signatur von `resolve_approval` keinen Parameter,
        // über den ein Aufrufer einen `ApprovalActor` selbst vorschlagen
        // könnte — der einzige Weg zu einem Actor ist `resolver.actor_for`,
        // angewandt auf `peer`. Dieser Test belegt, dass genau der Peer
        // entscheidet, nicht irgendein anderer Wert.
        let resolution = resolve_approval(
            &store,
            &resolver,
            &peer(1000),
            &session,
            &request,
            ReviewDecision::ApprovedOnce,
            Some("bounded exception".to_owned()),
            Timestamp::constant(2, 0),
        )
        .unwrap();
        assert_eq!(
            resolution.actor,
            ApprovalActor::Operator {
                id: "alice".to_owned()
            }
        );
        assert_eq!(resolution.decision, ReviewDecision::ApprovedOnce);
    }

    #[test]
    fn test_resolve_approval_second_confirmation_is_rejected_not_overwritten() {
        let temp = tempfile::tempdir().unwrap();
        let store = ApprovalStore::new(temp.path());
        let (session, request) = issue_pending(&store);
        let resolver = resolver();

        resolve_approval(
            &store,
            &resolver,
            &peer(1000),
            &session,
            &request,
            ReviewDecision::Approved,
            None,
            Timestamp::constant(3, 0),
        )
        .unwrap();

        let replay = resolve_approval(
            &store,
            &resolver,
            &peer(1000),
            &session,
            &request,
            ReviewDecision::Rejected,
            None,
            Timestamp::constant(4, 0),
        )
        .unwrap_err();
        assert!(matches!(
            replay,
            SecurityError::Store(harw_session_store::error::SessionStoreError::ApprovalAlreadyResolved { .. })
        ));
    }

    #[test]
    fn test_pending_approval_not_found_is_reported_not_invented() {
        let temp = tempfile::tempdir().unwrap();
        let store = ApprovalStore::new(temp.path());
        let session = SessionId::from_str("session-2");
        let request = ItemId::from_str("does-not-exist");

        let error = pending_approval(&store, &session, &request).unwrap_err();
        assert!(matches!(
            error,
            SecurityError::Store(harw_session_store::error::SessionStoreError::ApprovalNotFound { .. })
        ));
    }

    #[test]
    fn test_pending_approval_view_carries_irreversibility_flag() {
        // `PendingApprovalView` wird noch von keiner Operation befüllt
        // (siehe Moduldoku) — dieser Test belegt nur die Konstruierbarkeit
        // und dass das Umkehrbarkeitsfeld unabhängig vom Rest gesetzt
        // werden kann, wie es die Bedienoberfläche verlangt.
        let temp = tempfile::tempdir().unwrap();
        let store = ApprovalStore::new(temp.path());
        let (session, request) = issue_pending(&store);
        let record = pending_approval(&store, &session, &request).unwrap();

        let view = PendingApprovalView {
            record,
            action_kind: "warden.kill_process_tree".to_owned(),
            irreversible: true,
            rationale: "<img src=x onerror=alert(1)>".to_owned(),
        };
        assert!(view.irreversible);
        assert_eq!(view.rationale, "<img src=x onerror=alert(1)>");
    }
}
