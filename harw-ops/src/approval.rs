//! `approval.pending` / `approval.resolve` — die Bestätigungsfläche (Knoten
//! UI-06-Folgeknoten, A-APPR) über `harw-web` erreichbar machen.
//!
//! # Verantwortungsbereich
//! `harw_web::security` (siehe dortige Moduldoku) definiert die gesamte
//! Logik, die offene [`harw_session_store::approval::ApprovalRecord`]s
//! auflistet oder eine davon auflöst —
//! [`harw_web::security::list_pending_approvals`] und
//! [`harw_web::security::resolve_approval`]. Diese Datei macht sie über
//! `Surface::Web` erreichbar, ohne die Logik zu duplizieren, und formt das
//! Ergebnis als [`OpOutput`] mit Text **und** strukturierter `data`-Nutzlast.
//!
//! # Warum `approval.resolve` selbst keine Bestätigung verlangt
//! `approval.resolve` **ändert** einen Zustand (ein `ApprovalRecord` wird
//! einmalig verbraucht) und trägt trotzdem `web(..., approval = "none")`.
//! Eine Operation, die eine menschliche Bestätigung *entgegennimmt*, kann
//! nicht selbst eine Bestätigung verlangen. Sie autorisiert dabei **nichts**:
//! [`harw_session_store::approval::ApprovalStore::resolve`] entscheidet über
//! Einmaligkeit, Actor-Bindung und TTL. Diese Datei ruft an keiner Stelle
//! `harw-dod-escalate` oder `harw-dod-warden` auf (Invariante S1).
//!
//! # Woher der `ApprovalActor` kommt (F-172, F-121)
//! Ausschließlich aus dem vertrauenswürdigen [`Principal`] des Aufrufers
//! (`Principal::actor_id`), den die Eingangsgrenze als Service in die
//! `ServiceMap` legt — für Web `harw-cli/src/web.rs::web_op_context`
//! (`services.insert(web_principal(peer.uid, tier))`). `OpContext` hat kein
//! eigenes `principal()`-Feld; der Service ist der einzige Zugriffsweg.
//! Für Web-Principals verlangt [`harw_web::security::ApprovalCaller::actor`]
//! zusätzlich die `SO_PEERCRED`-Peer-Credentials und die serverseitige
//! Genehmiger-Tabelle (`Arc<dyn ApprovalActorResolver>`), die denselben Actor
//! bestätigen müssen. Fehlt der Principal oder hat er keinen Actor, wird
//! fail-closed abgelehnt. [`ApprovalResolveArgs`] hat **kein** Actor- und
//! **kein** Zeitfeld.
//!
//! # Serveruhr (F-122)
//! `resolved_at` und die TTL stammen aus der Serveruhr: ein registrierter
//! `Arc<dyn Clock>`-Service (Tests: feste Uhr), sonst
//! [`harw_types::SystemClock`]. Ein vom Client mitgeschicktes `resolved_at`
//! wird nicht gelesen.
//!
//! # Warum `approval.pending` alle offenen Anfragen listet
//! `Surface::Web` trägt für `GET` weder Pfadparameter noch Rumpf.
//! `approval.pending` listet deshalb alle offenen, nicht abgelaufenen Anfragen
//! über [`harw_session_store::approval::ApprovalStore::pending_all`] (höchstens
//! [`PENDING_LIMIT`]) — kein eigener Verzeichnis-Scan in dieser Datei.
//!
//! # Selbstgenehmigung — offen
//! `ApprovalRecord::actor` ist der gebundene Beantworter, nicht der
//! Anfragende; ohne persistierten Anfragenden ist „Anfragender ≠ Beantworter"
//! nicht prüfbar (C-APPR-Folgearbeit, Ledger `W4a/A-APPR.md`). Wirksam ist
//! heute nur: Modell-/Kind-/Operations-Principals haben keinen Actor und
//! werden abgelehnt.
//!
//! # Nebenläufigkeit
//! Beide Op-Structs sind zustandslose Unit-Structs (`#[operation]`-generiert).
//! `ApprovalStore::resolve` serialisiert konkurrierende Auflösungen derselben
//! Sitzung über einen Datei-Lock; diese Datei hält selbst keinen Zustand.
//!
//! # Fehlertypen
//! Beide Operationen übersetzen [`SecurityError`] nach [`OpError`] (siehe
//! [`map_security_error`]); fehlende Services melden sie als
//! [`OpError::NotAvailable`].

use std::sync::Arc;

use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};
use harw_session_store::approval::{ApprovalRecord, ApprovalResolutionRecord, ApprovalStore};
use harw_session_store::error::SessionStoreError;
use harw_types::{Clock, ItemId, Principal, ReviewDecision, SessionId, SystemClock};
use harw_web::peer::PeerCredentials;
use harw_web::security::{
    ApprovalActorResolver, ApprovalCaller, SecurityError, list_pending_approvals,
    resolve_approval,
};

/// Höchstzahl der von `approval.pending` gelieferten Anfragen.
pub const PENDING_LIMIT: usize = 200;

/// Übersetzt [`SecurityError`] nach [`OpError`].
///
/// # Description
/// Jede Identitätsablehnung (kein Actor, kein authentifizierter Web-Peer,
/// unbekannter Genehmiger, abweichende Identität, Actor-Mismatch im
/// Speicher) ist [`OpError::NotAvailable`]. „Falsch benannt, wiederholt oder
/// abgelaufen" (`ApprovalNotFound`, `ApprovalAlreadyResolved`,
/// `ApprovalExpired`) ist [`OpError::InvalidArguments`]. Alles andere ist
/// ein Laufzeitfehler des Speichers.
///
/// # Arguments
/// - `error` (`SecurityError`): der zu übersetzende Fehler.
///
/// # Returns
/// Der äquivalente [`OpError`].
fn map_security_error(error: SecurityError) -> OpError {
    match error {
        SecurityError::NoApproverActor { kind, surface } => OpError::NotAvailable(format!(
            "Aufrufer ({kind:?} über {surface:?}) darf keine Genehmigungsanfrage auflösen"
        )),
        SecurityError::UnauthenticatedWebPeer => OpError::NotAvailable(
            "keine authentifizierten Peer-Credentials oder keine Genehmiger-Tabelle im \
             Kontext — Web-Aufrufer kann nicht als Genehmiger bestätigt werden"
                .to_owned(),
        ),
        SecurityError::UnknownApprover { uid } => OpError::NotAvailable(format!(
            "Peer mit uid {uid} ist keinem Genehmiger zugeordnet"
        )),
        SecurityError::ApproverIdentityMismatch { uid } => OpError::NotAvailable(format!(
            "Peer mit uid {uid} entspricht nicht dem Genehmiger des Aufrufers"
        )),
        SecurityError::Store(SessionStoreError::ApprovalNotFound { session, request }) => {
            OpError::InvalidArguments(format!(
                "keine offene Genehmigungsanfrage '{request}' in Sitzung '{session}'"
            ))
        }
        SecurityError::Store(SessionStoreError::ApprovalAlreadyResolved { request }) => {
            OpError::InvalidArguments(format!(
                "Genehmigungsanfrage '{request}' wurde bereits aufgelöst"
            ))
        }
        SecurityError::Store(SessionStoreError::ApprovalExpired {
            request,
            expires_at,
            ..
        }) => OpError::InvalidArguments(format!(
            "Genehmigungsanfrage '{request}' ist seit {expires_at} abgelaufen"
        )),
        SecurityError::Store(SessionStoreError::ApprovalActorMismatch { request }) => {
            OpError::NotAvailable(format!("abweichender Genehmiger für Anfrage '{request}'"))
        }
        SecurityError::Store(other) => {
            OpError::Execution(format!("Genehmigungsspeicher-Fehler: {other}"))
        }
    }
}

/// Löst den registrierten [`ApprovalStore`] aus dem [`OpContext`] auf.
///
/// # Errors
/// [`OpError::NotAvailable`], wenn kein `Arc<ApprovalStore>` registriert ist.
fn approval_store(ctx: &OpContext) -> Result<Arc<ApprovalStore>, OpError> {
    ctx.service::<Arc<ApprovalStore>>()
        .map(Arc::clone)
        .ok_or_else(|| OpError::NotAvailable("kein Genehmigungsspeicher im Kontext".to_owned()))
}

/// Liefert die Serveruhr: registrierter `Arc<dyn Clock>`-Service oder
/// [`SystemClock`].
///
/// # Description
/// Beide Quellen sind serverseitig; ein Rückfall auf die Systemuhr ist daher
/// keine Abschwächung. Der Service existiert, damit Tests eine feste Uhr
/// injizieren können.
///
/// # Returns
/// Eine geteilte Uhr.
fn server_clock(ctx: &OpContext) -> Arc<dyn Clock> {
    match ctx.service::<Arc<dyn Clock>>() {
        Some(clock) => Arc::clone(clock),
        None => Arc::new(SystemClock),
    }
}

/// Baut die strukturierte Nutzlast einer Auflösung.
///
/// # Description
/// Felder `id` (Anfrage), `session`, `decision`, `resolved_at` (Serveruhr),
/// `actor` — jeweils in ihrer serde-Wire-Form (`snake_case`, RFC 3339).
///
/// # Errors
/// [`OpError::Execution`], wenn ein Feld nicht serialisierbar ist.
fn resolution_data(resolution: &ApprovalResolutionRecord) -> Result<serde_json::Value, OpError> {
    let encode = |error: serde_json::Error| {
        OpError::Execution(format!("Auflösung nicht serialisierbar: {error}"))
    };
    Ok(serde_json::json!({
        "id": resolution.request.as_str(),
        "session": resolution.session.as_str(),
        "decision": serde_json::to_value(resolution.decision).map_err(encode)?,
        "resolved_at": serde_json::to_value(resolution.resolved_at).map_err(encode)?,
        "actor": serde_json::to_value(&resolution.actor).map_err(encode)?,
    }))
}

/// Argument-Container für `approval.pending` — leer, weil die Fläche alle
/// offenen Anfragen listet (siehe Moduldoku).
#[derive(Default, serde::Deserialize, harw_macros::FromRawArgs)]
pub struct ApprovalPendingArgs {}

/// Listet alle offenen, nicht abgelaufenen Genehmigungsanfragen.
///
/// # Description
/// Reine Anzeige, keine Auflösung — ruft ausschließlich
/// [`list_pending_approvals`] (`ApprovalStore::pending_all`) mit
/// [`PENDING_LIMIT`] und der Serveruhr auf.
///
/// # Arguments
/// - `ctx` (`&OpContext`): muss `Arc<ApprovalStore>` als Service anbieten;
///   optional `Arc<dyn Clock>`.
/// - `_args` (`ApprovalPendingArgs`): leer.
///
/// # Returns
/// `OpOutput` mit einer Textzeile je Anfrage und
/// `data = {"pending": [ApprovalRecord…], "limit": PENDING_LIMIT}`.
///
/// # Errors
/// - [`OpError::NotAvailable`]: kein Genehmigungsspeicher im Kontext.
/// - [`OpError::Execution`]: das Genehmigungsverzeichnis ist nicht lesbar.
///
/// # Examples
/// ```rust,no_run
/// // Aufruf erfolgt über Operation::run(); siehe Modultests.
/// ```
#[operation(
    name = "approval.pending",
    summary = "Listet alle offenen Genehmigungsanfragen. Autorisiert nichts.",
    domain = "execution",
    permission = "observer",
    web(path = "/api/approval-pending", method = "get", approval = "none")
)]
async fn approval_pending(
    ctx: &OpContext,
    _args: ApprovalPendingArgs,
) -> Result<OpOutput, OpError> {
    let store = approval_store(ctx)?;
    let clock = server_clock(ctx);
    let records: Vec<ApprovalRecord> =
        list_pending_approvals(&store, PENDING_LIMIT, clock.as_ref())
            .map_err(map_security_error)?;
    let data = serde_json::json!({
        "pending": serde_json::to_value(&records).map_err(|error| {
            OpError::Execution(format!("Anfragen nicht serialisierbar: {error}"))
        })?,
        "limit": PENDING_LIMIT,
    });
    let text = if records.is_empty() {
        "Keine offenen Genehmigungsanfragen.".to_owned()
    } else {
        let mut buf = format!("{} offene Genehmigungsanfrage(n):\n", records.len());
        for record in &records {
            buf.push_str(&format!(
                "· {} (Sitzung {}, Aufruf {}, Genehmiger {:?}, ausgestellt {})\n",
                record.request, record.session, record.call_id, record.actor, record.issued_at
            ));
        }
        buf
    };
    Ok(OpOutput {
        text,
        data: Some(data),
    })
}

/// Argument-Container für `approval.resolve`.
///
/// # Beschreibung
/// Trägt **keinen** `actor`- und **keinen** Zeitparameter — der Actor kommt
/// aus dem Principal des Aufrufers, die Zeit aus der Serveruhr (siehe
/// Moduldoku). Unbekannte Felder (etwa ein altes `resolved_at`) werden von
/// serde ignoriert und nie gelesen.
#[derive(Debug, serde::Deserialize)]
pub struct ApprovalResolveArgs {
    /// Die betroffene Sitzung.
    pub session: SessionId,
    /// Die Kennung der aufzulösenden Genehmigungsanfrage.
    pub request: ItemId,
    /// Die menschliche Entscheidung.
    pub decision: ReviewDecision,
    /// Optionaler Freitextkommentar des Bedieners.
    #[serde(default)]
    pub comment: Option<String>,
}

impl Default for ApprovalResolveArgs {
    /// Sentinel-Default, das das `#[operation]`-Makro für den Fall
    /// `args.is_null()` benötigt. Leere IDs und `Rejected` fallen sofort als
    /// `ApprovalNotFound`/`UnsafeApprovalPath` auf, nie als stille Genehmigung.
    fn default() -> Self {
        Self {
            session: SessionId::from_str(String::new()),
            request: ItemId::from_str(String::new()),
            decision: ReviewDecision::Rejected,
            comment: None,
        }
    }
}

impl harw_operations::FromRawArgs for ApprovalResolveArgs {
    /// Command-Tokens sind für diese Web-only-Fläche nicht vorgesehen —
    /// jeder rohe Aufruf ist ungültig.
    fn from_raw_args(_tokens: &[String]) -> Result<Self, harw_operations::OpError> {
        Err(harw_operations::OpError::InvalidArguments(
            "approval.resolve erwartet JSON-Argumente, keine Befehlstokens".to_owned(),
        ))
    }
}

/// Löst eine offene Genehmigungsanfrage anhand einer menschlichen
/// Entscheidung auf.
///
/// # Description
/// Liest den [`Principal`] aus dem [`OpContext`] (Service), ergänzt für
/// Web-Aufrufer `PeerCredentials` und `Arc<dyn ApprovalActorResolver>`, und
/// ruft [`resolve_approval`] mit der Serveruhr auf. Der Actor entsteht nur in
/// [`ApprovalCaller::actor`]; ohne Actor wird abgelehnt.
///
/// # Arguments
/// - `ctx` (`&OpContext`): muss `Arc<ApprovalStore>` und `Principal`
///   anbieten; für Web-Principals zusätzlich `PeerCredentials` und
///   `Arc<dyn ApprovalActorResolver>`; optional `Arc<dyn Clock>`.
/// - `args` (`ApprovalResolveArgs`): Sitzung, Anfrage, Entscheidung,
///   optionaler Kommentar.
///
/// # Returns
/// `OpOutput` mit Text und
/// `data = {"id", "session", "decision", "resolved_at", "actor"}`.
///
/// # Errors
/// - [`OpError::NotAvailable`]: kein Speicher/Principal im Kontext; Principal
///   ohne Actor; Web-Aufrufer ohne authentifizierten Peer, unbekannt oder mit
///   abweichender Identität; Actor passt nicht zur Anfrage.
/// - [`OpError::InvalidArguments`]: Anfrage unbekannt, bereits aufgelöst oder
///   abgelaufen.
/// - [`OpError::Execution`]: sonstiger Speicherfehler.
///
/// # Concurrency
/// Delegiert die Sperrung vollständig an `ApprovalStore::resolve`.
///
/// # Examples
/// ```rust,no_run
/// // Aufruf erfolgt über Operation::run(); siehe Modultests.
/// ```
#[operation(
    name = "approval.resolve",
    summary = "Löst eine offene Genehmigungsanfrage anhand einer menschlichen Entscheidung auf. Autorisiert selbst nichts.",
    domain = "execution",
    permission = "operator",
    web(path = "/api/approval-resolve", method = "post", approval = "none")
)]
async fn approval_resolve(
    ctx: &OpContext,
    args: ApprovalResolveArgs,
) -> Result<OpOutput, OpError> {
    let store = approval_store(ctx)?;
    let principal = ctx.service::<Principal>().ok_or_else(|| {
        OpError::NotAvailable(
            "kein Principal im Kontext — Aufrufer nicht identifizierbar, Freigabe abgelehnt"
                .to_owned(),
        )
    })?;
    let resolver = ctx.service::<Arc<dyn ApprovalActorResolver>>();
    let peer = ctx.service::<PeerCredentials>();
    let caller = match (peer, resolver) {
        (Some(peer), Some(resolver)) => {
            ApprovalCaller::new(principal).with_web_peer(peer, resolver.as_ref())
        }
        _ => ApprovalCaller::new(principal),
    };
    let clock = server_clock(ctx);

    let resolution = resolve_approval(
        &store,
        &caller,
        &args.session,
        &args.request,
        args.decision,
        args.comment,
        clock.as_ref(),
    )
    .map_err(map_security_error)?;

    let data = resolution_data(&resolution)?;
    Ok(OpOutput {
        text: format!(
            "Anfrage {} (Sitzung {}) aufgelöst als {:?} durch {:?} um {}.",
            resolution.request,
            resolution.session,
            resolution.decision,
            resolution.actor,
            resolution.resolved_at
        ),
        data: Some(data),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        ApprovalPendingArgs, ApprovalResolveArgs, PENDING_LIMIT, approval_pending,
        approval_resolve,
    };
    use crate::testutil::toks;
    use harw_operations::{FromRawArgs, OpContext, OpError, context::ServiceMap};
    use harw_sandbox::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_session_store::approval::{ApprovalRecord, ApprovalStore};
    use harw_types::{
        ApprovalActor, Clock, IngressSurface, ItemId, PermissionTier, Principal, PrincipalKind,
        ReviewDecision, SessionId, TenantId, ToolCallId, TurnId, WorkspaceId,
    };
    use harw_web::peer::PeerCredentials;
    use harw_web::security::{ApprovalActorResolver, StaticUidApprovalActorMap};
    use jiff::{SignedDuration, Timestamp};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    const ISSUED_SECS: i64 = 1_700_000_000;

    // Feste Serveruhr, als `Arc<dyn Clock>`-Service injiziert.
    struct FixedClock(Timestamp);

    impl Clock for FixedClock {
        fn now(&self) -> Timestamp {
            self.0
        }
    }

    fn issued_at() -> Timestamp {
        Timestamp::constant(ISSUED_SECS, 0)
    }

    fn at_offset(offset: SignedDuration) -> Timestamp {
        // Testhilfe: 1.7e9 s plus Minuten liegt sicher im Wertebereich.
        issued_at().checked_add(offset).expect("timestamp in range")
    }

    fn unique_root(prefix: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("{prefix}-{}-{id}", std::process::id()))
    }

    /// Dienste eines Test-`OpContext`; nur gesetzte Felder werden eingefügt.
    #[derive(Default)]
    struct Services {
        store: Option<Arc<ApprovalStore>>,
        principal: Option<Principal>,
        resolver: Option<Arc<dyn ApprovalActorResolver>>,
        peer: Option<PeerCredentials>,
        clock: Option<Timestamp>,
    }

    fn test_context(services_in: Services) -> (OpContext, PathBuf) {
        let root = unique_root("harw-ops-approval-test");
        std::fs::create_dir_all(root.join("ws")).expect("create test workspace");
        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("ws"),
                root: PathBuf::from("ws"),
            }],
        )
        .expect("build workspace registry");
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("ws"),
            )
            .expect("resolve workspace binding");
        let sandbox = SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy([Permission::ReadWorkspace]),
        );
        let mut services = ServiceMap::new();
        if let Some(store) = services_in.store {
            services.insert(store);
        }
        if let Some(principal) = services_in.principal {
            services.insert(principal);
        }
        if let Some(resolver) = services_in.resolver {
            services.insert(resolver);
        }
        if let Some(peer) = services_in.peer {
            services.insert(peer);
        }
        if let Some(now) = services_in.clock {
            let clock: Arc<dyn Clock> = Arc::new(FixedClock(now));
            services.insert(clock);
        }
        (
            OpContext::new(SessionId::new(), TurnId::new(), sandbox, services),
            root,
        )
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

    fn issue_pending(store: &ApprovalStore, session: &str, request: &str, issued: Timestamp) {
        store
            .issue(&ApprovalRecord {
                request: ItemId::from_str(request),
                session: SessionId::from_str(session),
                call_id: ToolCallId::from_str("call-1"),
                actor: owner(),
                issued_at: issued,
            })
            .expect("issue pending approval");
    }

    fn resolver_owner() -> Arc<dyn ApprovalActorResolver> {
        Arc::new(StaticUidApprovalActorMap::new(vec![(1000, "owner".to_owned())]))
    }

    fn approve_args(request: &str) -> ApprovalResolveArgs {
        ApprovalResolveArgs {
            session: SessionId::from_str("session-1"),
            request: ItemId::from_str(request),
            decision: ReviewDecision::Approved,
            comment: None,
        }
    }

    /// Voll verdrahteter Web-Kontext (Principal, Peer uid 1000, Resolver, Uhr).
    fn web_services(store: &Arc<ApprovalStore>, now: Timestamp) -> Services {
        Services {
            store: Some(Arc::clone(store)),
            principal: Some(web_principal()),
            resolver: Some(resolver_owner()),
            peer: Some(PeerCredentials::new(1, 1000, 1000)),
            clock: Some(now),
        }
    }

    fn cleanup(store_root: &Path, ws_root: PathBuf) {
        std::fs::remove_dir_all(store_root).ok();
        std::fs::remove_dir_all(ws_root).ok();
    }

    #[test]
    fn test_approval_pending_args_from_raw_args_ignores_tokens() {
        assert!(ApprovalPendingArgs::from_raw_args(&toks(&["ignored"])).is_ok());
    }

    #[test]
    fn test_approval_resolve_args_from_raw_args_is_rejected() {
        let error = ApprovalResolveArgs::from_raw_args(&toks(&[])).unwrap_err();
        assert!(matches!(error, OpError::InvalidArguments(_)));
    }

    /// Ein vom Client mitgeschicktes `resolved_at`/`actor` hat kein Zielfeld.
    #[test]
    fn test_approval_resolve_args_ignore_client_time_and_actor() {
        let json = serde_json::json!({
            "session": "session-1",
            "request": "approval-1",
            "decision": "approved",
            "resolved_at": "1970-01-01T00:00:00Z",
            "actor": {"kind": "operator", "id": "attacker"},
        });
        let args: ApprovalResolveArgs = serde_json::from_value(json).expect("deserializes");
        assert_eq!(args.session.as_str(), "session-1");
        assert_eq!(args.decision, ReviewDecision::Approved);
        assert!(!format!("{args:?}").contains("1970"));
        assert!(!format!("{args:?}").contains("attacker"));
    }

    #[tokio::test]
    async fn test_approval_pending_without_store_returns_not_available() {
        let (ctx, root) = test_context(Services::default());
        let result = approval_pending(&ctx, ApprovalPendingArgs::default()).await;
        std::fs::remove_dir_all(root).ok();
        assert!(matches!(result, Err(OpError::NotAvailable(_))));
    }

    #[tokio::test]
    async fn test_approval_pending_with_empty_store_reports_none_open() {
        let store_root = unique_root("harw-ops-approval-empty-store");
        let store = Arc::new(ApprovalStore::new(&store_root));
        let (ctx, ws_root) = test_context(Services {
            store: Some(store),
            ..Services::default()
        });
        let output = approval_pending(&ctx, ApprovalPendingArgs::default())
            .await
            .expect("list succeeds on empty store");
        cleanup(&store_root, ws_root);
        assert_eq!(output.text, "Keine offenen Genehmigungsanfragen.");
        let data = output.data.expect("structured payload");
        assert_eq!(data["pending"], serde_json::json!([]));
        assert_eq!(data["limit"], serde_json::json!(PENDING_LIMIT));
    }

    /// `pending_all`-Ausgabe: sortiert nach `issued_at`, ohne abgelaufene und
    /// aufgelöste Anfragen, als `data.pending`.
    #[tokio::test]
    async fn test_approval_pending_lists_open_requests_from_pending_all() {
        let store_root = unique_root("harw-ops-approval-list-store");
        let store = Arc::new(ApprovalStore::new(&store_root));
        let now = at_offset(SignedDuration::from_mins(40));
        issue_pending(&store, "session-1", "stale", issued_at());
        issue_pending(&store, "session-1", "later", at_offset(SignedDuration::from_mins(20)));
        issue_pending(&store, "session-2", "earlier", at_offset(SignedDuration::from_mins(15)));
        issue_pending(&store, "session-1", "done", at_offset(SignedDuration::from_mins(25)));

        let (resolve_ctx, ws_resolve) = test_context(web_services(&store, now));
        approval_resolve(&resolve_ctx, approve_args("done"))
            .await
            .expect("resolve 'done'");

        let (ctx, ws_root) = test_context(Services {
            store: Some(Arc::clone(&store)),
            clock: Some(now),
            ..Services::default()
        });
        let output = approval_pending(&ctx, ApprovalPendingArgs::default())
            .await
            .expect("list succeeds");
        std::fs::remove_dir_all(ws_resolve).ok();
        cleanup(&store_root, ws_root);

        assert!(output.text.contains("2 offene"));
        assert!(!output.text.contains("stale"));
        let data = output.data.expect("structured payload");
        let pending = data["pending"].as_array().expect("pending array");
        let requests: Vec<&str> = pending
            .iter()
            .filter_map(|record| record["request"].as_str())
            .collect();
        assert_eq!(requests, vec!["earlier", "later"]);
        assert_eq!(pending[0]["session"], serde_json::json!("session-2"));
        assert_eq!(
            pending[0]["actor"],
            serde_json::json!({"kind": "operator", "id": "owner"})
        );
    }

    #[tokio::test]
    async fn test_approval_resolve_without_approval_store_returns_not_available() {
        let (ctx, root) = test_context(Services::default());
        let result = approval_resolve(&ctx, approve_args("approval-1")).await;
        std::fs::remove_dir_all(root).ok();
        assert!(matches!(result, Err(OpError::NotAvailable(_))));
    }

    /// Ohne Principal-Service gibt es keinen Actor — abgelehnt, Speicher
    /// unberührt, auch wenn Peer und Resolver vorhanden sind.
    #[tokio::test]
    async fn test_approval_resolve_without_principal_is_rejected() {
        let store_root = unique_root("harw-ops-approval-no-principal");
        let store = Arc::new(ApprovalStore::new(&store_root));
        issue_pending(&store, "session-1", "approval-1", issued_at());
        let mut services = web_services(&store, at_offset(SignedDuration::from_mins(1)));
        services.principal = None;
        let (ctx, ws_root) = test_context(services);

        let result = approval_resolve(&ctx, approve_args("approval-1")).await;
        let resolution = store
            .resolution(&SessionId::from_str("session-1"), &ItemId::from_str("approval-1"))
            .expect("readable");
        cleanup(&store_root, ws_root);

        match result {
            Err(OpError::NotAvailable(message)) => assert!(message.contains("kein Principal")),
            other => panic!("expected NotAvailable(kein Principal), got: {other:?}"),
        }
        assert_eq!(resolution, None);
    }

    /// Ein Principal ohne `actor_id()` (Kind-Agent) wird abgelehnt.
    #[tokio::test]
    async fn test_approval_resolve_principal_without_actor_is_rejected() {
        let store_root = unique_root("harw-ops-approval-child-principal");
        let store = Arc::new(ApprovalStore::new(&store_root));
        issue_pending(&store, "session-1", "approval-1", issued_at());
        let mut services = web_services(&store, at_offset(SignedDuration::from_mins(1)));
        services.principal = Some(web_principal().child_of("explorer"));
        let (ctx, ws_root) = test_context(services);

        let result = approval_resolve(&ctx, approve_args("approval-1")).await;
        let resolution = store
            .resolution(&SessionId::from_str("session-1"), &ItemId::from_str("approval-1"))
            .expect("readable");
        cleanup(&store_root, ws_root);

        match result {
            Err(OpError::NotAvailable(message)) => {
                assert!(message.contains("darf keine Genehmigungsanfrage auflösen"));
            }
            other => panic!("expected NotAvailable(no actor), got: {other:?}"),
        }
        assert_eq!(resolution, None);
    }

    /// Web-Principal ohne `PeerCredentials` — kein authentifizierter Peer.
    #[tokio::test]
    async fn test_approval_resolve_web_principal_without_peer_credentials_is_rejected() {
        let store_root = unique_root("harw-ops-approval-no-peer-store");
        let store = Arc::new(ApprovalStore::new(&store_root));
        issue_pending(&store, "session-1", "approval-1", issued_at());
        let mut services = web_services(&store, at_offset(SignedDuration::from_mins(1)));
        services.peer = None;
        let (ctx, ws_root) = test_context(services);

        let result = approval_resolve(&ctx, approve_args("approval-1")).await;
        cleanup(&store_root, ws_root);

        match result {
            Err(OpError::NotAvailable(message)) => assert!(message.contains("Peer-Credentials")),
            other => panic!("expected NotAvailable mentioning Peer-Credentials, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_approval_resolve_rejects_a_peer_unknown_to_the_resolver() {
        let store_root = unique_root("harw-ops-approval-unknown-peer-store");
        let store = Arc::new(ApprovalStore::new(&store_root));
        issue_pending(&store, "session-1", "approval-1", issued_at());
        let mut services = web_services(&store, at_offset(SignedDuration::from_mins(1)));
        services.peer = Some(PeerCredentials::new(1, 9999, 9999));
        let (ctx, ws_root) = test_context(services);

        let result = approval_resolve(&ctx, approve_args("approval-1")).await;
        cleanup(&store_root, ws_root);

        match result {
            Err(OpError::NotAvailable(message)) => {
                assert!(message.contains("keinem Genehmiger zugeordnet"));
            }
            other => panic!("expected NotAvailable(unknown approver), got: {other:?}"),
        }
    }

    /// `resolved_at` stammt aus der injizierten Serveruhr; `data` trägt
    /// `id`, `session`, `decision`, `resolved_at`, `actor`.
    #[tokio::test]
    async fn test_approval_resolve_uses_server_clock_and_returns_json_data() {
        let store_root = unique_root("harw-ops-approval-resolve-store");
        let store = Arc::new(ApprovalStore::new(&store_root));
        issue_pending(&store, "session-1", "approval-1", issued_at());
        let now = at_offset(SignedDuration::from_mins(7));
        let (ctx, ws_root) = test_context(web_services(&store, now));

        let args = ApprovalResolveArgs {
            decision: ReviewDecision::ApprovedOnce,
            comment: Some("bounded exception".to_owned()),
            ..approve_args("approval-1")
        };
        let output = approval_resolve(&ctx, args)
            .await
            .expect("resolution succeeds for the confirmed web approver");
        let durable = store
            .resolution(&SessionId::from_str("session-1"), &ItemId::from_str("approval-1"))
            .expect("readable")
            .expect("durable resolution");
        cleanup(&store_root, ws_root);

        assert!(output.text.contains("approval-1"));
        assert!(output.text.contains("ApprovedOnce"));
        assert!(output.text.contains("owner"));
        assert_eq!(durable.resolved_at, now);

        let data = output.data.expect("structured payload");
        let expected = serde_json::json!({
            "id": "approval-1",
            "session": "session-1",
            "decision": "approved_once",
            "resolved_at": serde_json::to_value(now).expect("timestamp serializes"),
            "actor": {"kind": "operator", "id": "owner"},
        });
        assert_eq!(data, expected);
    }

    /// Abgelaufene Anfrage (Serveruhr ≥ `issued_at` + 30 min) → Fehler.
    #[tokio::test]
    async fn test_approval_resolve_expired_request_is_rejected() {
        let store_root = unique_root("harw-ops-approval-expired-store");
        let store = Arc::new(ApprovalStore::new(&store_root));
        issue_pending(&store, "session-1", "approval-1", issued_at());
        let (ctx, ws_root) =
            test_context(web_services(&store, at_offset(SignedDuration::from_mins(30))));

        let result = approval_resolve(&ctx, approve_args("approval-1")).await;
        let resolution = store
            .resolution(&SessionId::from_str("session-1"), &ItemId::from_str("approval-1"))
            .expect("readable");
        cleanup(&store_root, ws_root);

        match result {
            Err(OpError::InvalidArguments(message)) => assert!(message.contains("abgelaufen")),
            other => panic!("expected InvalidArguments(abgelaufen), got: {other:?}"),
        }
        assert_eq!(resolution, None);
    }

    /// Eine zweite Bestätigung derselben Anfrage wird abgewiesen, nicht
    /// überschrieben.
    #[tokio::test]
    async fn test_approval_resolve_rejects_a_second_confirmation_of_the_same_request() {
        let store_root = unique_root("harw-ops-approval-twice-store");
        let store = Arc::new(ApprovalStore::new(&store_root));
        issue_pending(&store, "session-1", "approval-1", issued_at());
        let now = at_offset(SignedDuration::from_mins(1));

        let (ctx_first, ws_root_first) = test_context(web_services(&store, now));
        approval_resolve(&ctx_first, approve_args("approval-1"))
            .await
            .expect("first resolution succeeds");

        let (ctx_second, ws_root_second) = test_context(web_services(&store, now));
        let replay = approval_resolve(
            &ctx_second,
            ApprovalResolveArgs {
                decision: ReviewDecision::Rejected,
                ..approve_args("approval-1")
            },
        )
        .await
        .unwrap_err();

        std::fs::remove_dir_all(ws_root_first).ok();
        cleanup(&store_root, ws_root_second);

        match replay {
            OpError::InvalidArguments(message) => assert!(message.contains("bereits aufgelöst")),
            other => panic!("expected InvalidArguments(bereits aufgelöst), got: {other:?}"),
        }
    }
}
