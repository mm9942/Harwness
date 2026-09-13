//! `approval.pending` / `approval.resolve` — die Bestätigungsfläche (Knoten
//! UI-06-Folgeknoten) über `harw-web` erreichbar machen.
//!
//! # Verantwortungsbereich
//! `harw_web::security` (siehe dortige Moduldoku) definiert bereits die
//! gesamte Logik, die einen [`harw_session_store::approval::ApprovalRecord`]
//! anzeigt oder auflöst — [`harw_web::security::pending_approval`] und
//! [`harw_web::security::resolve_approval`] — aber diese Logik war bislang
//! über **keine** Fläche erreichbar: `#[operation(...)]` kannte kein
//! `web(...)`-Unterattribut (das hat der vorherige Knoten geschlossen, siehe
//! `harw-macros/src/operation.rs`). Diese Datei schließt genau diese Lücke,
//! ohne die Logik selbst zu duplizieren: `run()` beider Operationen ruft
//! ausschließlich [`harw_web::security::pending_approval`] bzw.
//! [`harw_web::security::resolve_approval`] auf.
//!
//! # Warum `approval.resolve` selbst keine Bestätigung verlangt
//! `approval.resolve` **ändert** einen Zustand (ein `ApprovalRecord` wird
//! einmalig verbraucht) und trägt trotzdem `web(..., approval = "none")`.
//! Das ist kein Versehen: eine Operation, die eine menschliche Bestätigung
//! *entgegennimmt*, kann nicht selbst eine Bestätigung verlangen — sie
//! müsste dann von sich selbst genehmigt werden, bevor sie eine Genehmigung
//! entgegennehmen darf, eine Endlosschleife. `approval.resolve` autorisiert
//! dabei **nichts**: sie reicht eine bereits getroffene menschliche
//! Entscheidung unverändert an
//! [`harw_session_store::approval::ApprovalStore::resolve`] weiter, und
//! **dieser Speicher** entscheidet — über Aktualitäts-, Actor- und
//! Einmaligkeitsprüfung —, ob die Entscheidung zulässig ist. Diese Datei
//! ruft an keiner Stelle `harw-dod-escalate` oder `harw-dod-warden` auf (sie
//! hängt nicht einmal von diesen Crates ab, siehe `harw-ops/Cargo.toml`) —
//! Autorisierung entsteht in dieser Codebasis an genau einer Stelle
//! (Invariante S1), und die ist nicht hier.
//!
//! # Woher der `ApprovalActor` kommt
//! Beide Operationen leiten den [`harw_types::ApprovalActor`] **ausschließlich**
//! aus den über `SO_PEERCRED` gelesenen Peer-Credentials her — nie aus einem
//! Feld des JSON-Anfragerumpfs. `run()` liest dafür genau zwei Services aus
//! dem [`OpContext`]:
//! - `harw_web::peer::PeerCredentials` — die Identität des Aufrufers.
//! - `Arc<dyn harw_web::security::ApprovalActorResolver>` — die
//!   serverseitig vertraute Richtlinie, die diese Identität auf einen
//!   `ApprovalActor` abbildet.
//!
//! Es gibt in [`ApprovalResolveArgs`] **kein** Feld, aus dem ein Aufrufer
//! selbst einen Actor vorschlagen könnte — der Typ macht das strukturell
//! unmöglich, nicht nur per Konvention.
//!
//! **Offener Befund dieses Knotens:** [`OpContext`] (Session, Turn, Sandbox,
//! `ServiceMap`) hat kein eigenes Feld für Peer-Credentials — sie müssen als
//! `PeerCredentials`-Service in der `ServiceMap` registriert sein, damit
//! `run()` sie lesen kann. Ob die kompositionsseitige
//! `harw_web::server::WebContextFactory` (die pro Aufruf `(&PeerCredentials,
//! PermissionTier) -> OpContext` baut) diesen Service tatsächlich einfügt,
//! liegt außerhalb des Schreibbereichs dieses Knotens (`harw-cli`/die
//! Composition Root, nicht `harw-ops`). Fehlt der Service, meldet
//! `approval_resolve` das fail-closed als [`OpError::NotAvailable`] — es
//! erfindet keinen Ersatz-Actor und nimmt keinen Actor aus `args` entgegen.
//! Siehe Abschlussbericht dieses Knotens für die genaue Fundstelle.
//!
//! # Warum `approval.pending` alle offenen Anfragen listet, nicht eine
//! `Surface::Web { path, readonly, approval }` trägt weder Pfadparameter
//! noch Rumpf für `GET` — eine Route ist ein fest verdrahteter Pfad (siehe
//! `harw_web`-Moduldoku). Eine Einzelabfrage nach `session`/`request` bräuchte
//! eine Erweiterung von `Surface::Web`, die außerhalb dieses Knotens liegt.
//! `approval.pending` listet deshalb bewusst **alle** für den Store
//! sichtbaren offenen Anfragen: es sind wenige, und der Bediener soll sie
//! ohnehin alle sehen, um zu entscheiden.
//!
//! [`harw_session_store::approval::ApprovalStore`] hat dafür **keine**
//! eigene Auflistungsmethode — nur `issue`, `resolve` und `pending` (Einzel-
//! abfrage nach `session`+`request`). Diese Datei erfindet keinen zweiten,
//! ungeprüften Lesepfad: [`list_pending_records`] öffnet ausschließlich den
//! über [`harw_session_store::approval::ApprovalStore::root`] bereits
//! öffentlichen Wurzelpfad, um Kandidaten-Dateinamen (`<session>/<request>
//! .pending.json`, gefiltert um jede Anfrage mit einer bereits vorhandenen
//! `<request>.resolved.json`-Datei — `ApprovalStore::resolve` löscht die
//! Pending-Datei nicht, sie legt nur zusätzlich die Resolved-Datei an) zu
//! **entdecken** — die eigentliche, gegen Symlink-Angriffe
//! gehärtete Leseoperation bleibt ausnahmslos
//! [`harw_web::security::pending_approval`] (das intern
//! `ApprovalStore::pending` aufruft) vorbehalten. Ein Kandidat, der zwischen
//! Entdeckung und Abfrage bereits aufgelöst wurde oder sich als Symlink
//! entpuppt, wird stillschweigend übersprungen (kein Fehler) — das ist keine
//! Race Condition in dieser Datei, weil die einzige Autorität für den Inhalt
//! immer `ApprovalStore::pending` bleibt. **Offener Befund:** eine
//! `ApprovalStore::list_pending(...)`-Methode, die diese Entdeckung intern
//! und mit derselben Härtung wie `pending`/`resolve` durchführt, gehört
//! eigentlich nach `harw-session-store` — das liegt außerhalb des
//! Schreibbereichs dieses Knotens.
//!
//! # Was hier fehlt (siehe `harw_web::security`-Moduldoku)
//! `approval.pending` gibt ausschließlich die Felder von
//! [`harw_session_store::approval::ApprovalRecord`] aus (`request`,
//! `session`, `call_id`, `actor`, `issued_at`) — keinen Befundtext, keine
//! `WardenAction`/`Reversibility`. `harw_web::security::PendingApprovalView`
//! bräuchte dafür einen bereits aus `call_id` aufgelösten Anzeigeinhalt, den
//! noch keine Operation liefert (siehe dortige Moduldoku). Diese Lücke wird
//! hier nicht geschlossen, nur nicht verschwiegen.
//!
//! # Nebenläufigkeit
//! Beide Op-Structs sind zustandslose Unit-Structs (`#[operation]`-generiert).
//! [`ApprovalStore::resolve`] serialisiert konkurrierende Auflösungen
//! derselben Sitzung über einen Datei-Lock; diese Datei hält selbst keinen
//! Zustand.
//!
//! # Fehlertypen
//! Beide Operationen übersetzen [`harw_web::security::SecurityError`] nach
//! [`OpError`] (siehe [`map_security_error`]); zusätzlich melden sie fehlende
//! Services als [`OpError::NotAvailable`].

use std::sync::Arc;

use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};
use harw_session_store::approval::{ApprovalRecord, ApprovalStore};
use harw_session_store::error::SessionStoreError;
use harw_types::{ItemId, ReviewDecision, SessionId};
use harw_web::peer::PeerCredentials;
use harw_web::security::{ApprovalActorResolver, SecurityError, pending_approval, resolve_approval};
use jiff::Timestamp;

/// Übersetzt [`SecurityError`] nach [`OpError`].
///
/// # Description
/// [`SecurityError::UnknownApprover`] ist eine Autorisierungsablehnung (der
/// Peer darf keine Anfrage auflösen) und wird als [`OpError::NotAvailable`]
/// gemeldet — dieselbe Behandlung wie eine fehlende Berechtigung an anderer
/// Stelle in dieser Crate. Innerhalb von [`SecurityError::Store`] werden
/// „der Aufrufer hat etwas Falsches benannt oder wiederholt"-Fälle
/// ([`SessionStoreError::ApprovalNotFound`],
/// [`SessionStoreError::ApprovalAlreadyResolved`]) als
/// [`OpError::InvalidArguments`] gemeldet — eine zweite Bestätigung derselben
/// Anfrage wird damit abgewiesen, nicht als interner Fehler getarnt. Ein
/// Actor-Fehlanpassung ([`SessionStoreError::ApprovalActorMismatch`]) ist
/// eine Autorisierungsablehnung wie `UnknownApprover`. Alles andere ist ein
/// Laufzeitfehler des Speichers.
///
/// # Arguments
/// - `error` (`SecurityError`): der zu übersetzende Fehler.
///
/// # Returns
/// Der äquivalente [`OpError`].
fn map_security_error(error: SecurityError) -> OpError {
    match error {
        SecurityError::UnknownApprover { uid } => OpError::NotAvailable(format!(
            "Peer mit uid {uid} ist keinem Genehmiger zugeordnet"
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
        SecurityError::Store(SessionStoreError::ApprovalActorMismatch { request }) => {
            OpError::NotAvailable(format!(
                "abweichender Genehmiger für Anfrage '{request}'"
            ))
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
        .cloned()
        .ok_or_else(|| OpError::NotAvailable("kein Genehmigungsspeicher im Kontext".to_owned()))
}

/// Entdeckt Kandidaten-Anfragen (`session`, `request`) unterhalb von
/// [`ApprovalStore::root`], ohne selbst eine Autoritätsentscheidung zu
/// treffen.
///
/// # Description
/// Liest genau zwei Verzeichnisebenen: `root()/<session>/` und darin jede
/// Datei mit der Endung `.pending.json`. Der Dateiname ohne diese Endung ist
/// die `request`-Id, der Ordnername die `session`-Id — dieselbe Kodierung,
/// die `ApprovalStore::pending_path` intern verwendet
/// (`harw-session-store/src/approval.rs`). Diese Funktion **liest keinen
/// Dateiinhalt** und trifft keine Sichtbarkeits- oder Gültigkeitsentscheidung
/// — sie liefert nur Namen, die anschließend ausnahmslos über
/// [`pending_approval`] (und damit über den Symlink-gehärteten Lesepfad von
/// `ApprovalStore::pending`) bestätigt werden müssen.
///
/// # Arguments
/// - `store` (`&ApprovalStore`): der Genehmigungsspeicher, dessen `root()`
///   gelesen wird.
///
/// # Returns
/// Ein `Vec<(SessionId, ItemId)>` mit allen gefundenen Kandidaten, in keiner
/// garantierten Reihenfolge.
///
/// # Errors
/// [`std::io::Error`], wenn `root()` oder eines der Sitzungsverzeichnisse
/// nicht gelesen werden kann. Ein fehlendes `root()`-Verzeichnis (noch nie
/// eine Anfrage ausgestellt) liefert eine leere Liste, keinen Fehler.
fn discover_pending_candidates(store: &ApprovalStore) -> std::io::Result<Vec<(SessionId, ItemId)>> {
    let mut candidates = Vec::new();
    let root_entries = match std::fs::read_dir(store.root()) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(candidates),
        Err(error) => return Err(error),
    };
    for session_entry in root_entries {
        let session_entry = session_entry?;
        if !session_entry.file_type()?.is_dir() {
            continue;
        }
        let Some(session_name) = session_entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };

        // Beide Suffixe derselben Sitzung werden zuerst gesammelt, damit
        // `<request>.resolved.json` bereits aufgelöste Anfragen ausfiltern
        // kann, ohne eine zweite Verzeichnisleseoperation zu brauchen —
        // `ApprovalStore::resolve` löscht die `.pending.json`-Datei nicht,
        // sie legt nur zusätzlich die `.resolved.json`-Datei an (siehe
        // `harw-session-store/src/approval.rs`).
        let mut file_names = std::collections::BTreeSet::new();
        for file_entry in std::fs::read_dir(session_entry.path())? {
            let file_entry = file_entry?;
            if let Some(name) = file_entry.file_name().to_str() {
                file_names.insert(name.to_owned());
            }
        }
        for file_name in &file_names {
            let Some(request_name) = file_name.strip_suffix(".pending.json") else {
                continue;
            };
            let resolved_name = format!("{request_name}.resolved.json");
            if file_names.contains(&resolved_name) {
                // Bereits aufgelöst — die Anfrage ist keine offene Anfrage
                // mehr, auch wenn ihre `.pending.json`-Datei zu
                // Auditzwecken liegen bleibt. Die Autorität für "aufgelöst"
                // bleibt trotzdem `ApprovalStore::resolve`; dieser Filter
                // ist nur eine Anzeige-Heuristik.
                continue;
            }
            candidates.push((
                SessionId::from_str(session_name.clone()),
                ItemId::from_str(request_name.to_owned()),
            ));
        }
    }
    Ok(candidates)
}

/// Bestätigt jeden Kandidaten über [`pending_approval`] und verwirft, was
/// zwischen Entdeckung und Bestätigung ungültig wurde.
///
/// # Description
/// Ruft für jeden von [`discover_pending_candidates`] gefundenen Kandidaten
/// [`pending_approval`] auf. [`SecurityError::Store`] mit
/// [`SessionStoreError::ApprovalNotFound`] wird stillschweigend übersprungen
/// (die Anfrage wurde inzwischen aufgelöst oder war ein Symlink, siehe
/// Moduldoku) — jeder andere Fehler wird propagiert.
///
/// # Arguments
/// - `store` (`&ApprovalStore`): der Genehmigungsspeicher.
///
/// # Returns
/// Alle noch offenen, bestätigten [`ApprovalRecord`]s.
///
/// # Errors
/// [`OpError::Execution`] bei einem I/O-Fehler beim Entdecken der Kandidaten
/// oder einem nicht-transienten Speicherfehler.
fn list_pending_records(store: &ApprovalStore) -> Result<Vec<ApprovalRecord>, OpError> {
    let candidates = discover_pending_candidates(store)
        .map_err(|error| OpError::Execution(format!("Genehmigungsverzeichnis unlesbar: {error}")))?;
    let mut records = Vec::with_capacity(candidates.len());
    for (session, request) in candidates {
        match pending_approval(store, &session, &request) {
            Ok(record) => records.push(record),
            Err(SecurityError::Store(SessionStoreError::ApprovalNotFound { .. })) => continue,
            Err(other) => return Err(map_security_error(other)),
        }
    }
    Ok(records)
}

/// Argument-Container für `approval.pending` — leer, weil die Fläche alle
/// offenen Anfragen listet (siehe Moduldoku).
#[derive(Default, serde::Deserialize, harw_macros::FromRawArgs)]
pub struct ApprovalPendingArgs {}

/// Listet alle über den Genehmigungsspeicher sichtbaren offenen Anfragen.
///
/// # Description
/// Reine Anzeige, keine Auflösung — ruft ausschließlich [`pending_approval`]
/// auf (über [`list_pending_records`], das die Kandidaten dafür entdeckt,
/// siehe Moduldoku „Warum `approval.pending` alle offenen Anfragen listet").
/// Trifft keine eigene Sichtbarkeits- oder Autorisierungsentscheidung.
///
/// # Arguments
/// - `ctx` (`&OpContext`): muss `Arc<ApprovalStore>` als Service anbieten.
/// - `_args` (`ApprovalPendingArgs`): leer.
///
/// # Returns
/// `Ok(OpOutput { text })` mit einer Zeile je offener Anfrage, oder einem
/// Hinweistext, wenn keine offen ist.
///
/// # Errors
/// - [`OpError::NotAvailable`]: kein Genehmigungsspeicher im Kontext.
/// - [`OpError::Execution`]: das Genehmigungsverzeichnis konnte nicht
///   gelesen werden.
///
/// # Examples
/// ```rust,no_run
/// // Aufruf erfolgt über Operation::run(); siehe Modultests für den
/// // direkten Aufruf gegen einen temporären ApprovalStore.
/// ```
#[operation(
    name = "approval.pending",
    summary = "Listet alle offenen Genehmigungsanfragen. Autorisiert nichts.",
    domain = "execution",
    permission = "observer",
    web(path = "/api/approval-pending", readonly, approval = "none")
)]
async fn approval_pending(
    ctx: &OpContext,
    _args: ApprovalPendingArgs,
) -> Result<OpOutput, OpError> {
    let store = approval_store(ctx)?;
    let records = list_pending_records(&store)?;
    if records.is_empty() {
        return Ok(OpOutput {
            text: "Keine offenen Genehmigungsanfragen.".to_owned(),
        });
    }
    let mut buf = format!("{} offene Genehmigungsanfrage(n):\n", records.len());
    for record in &records {
        buf.push_str(&format!(
            "· {} (Sitzung {}, Aufruf {}, angefragt von {:?} um {})\n",
            record.request, record.session, record.call_id, record.actor, record.issued_at
        ));
    }
    Ok(OpOutput { text: buf })
}

/// Argument-Container für `approval.resolve`.
///
/// # Beschreibung
/// Trägt **keinen** `actor`-Parameter — der `ApprovalActor` kommt
/// ausschließlich aus den Peer-Credentials des Aufrufers (siehe Moduldoku).
/// `resolved_at` kommt vom Aufrufer, weil diese Operation keine Systemuhr
/// liest.
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
    /// Vom Aufrufer bereitgestellter Zeitstempel der Entscheidung.
    pub resolved_at: Timestamp,
}

impl Default for ApprovalResolveArgs {
    /// Sentinel-Default, das das `#[operation]`-Makro für den (bei dieser
    /// Fläche nie eintretenden) Fall `args.is_null()` benötigt — `harw-web`
    /// liefert für eine POST-Route mit Rumpf keinen `null`-Wert. Die
    /// Sentinel-Werte sind bewusst ungültig (leere IDs, `Rejected`,
    /// Unix-Epoche): ein versehentlich durchgereichtes Default würde sofort
    /// als `OpError::InvalidArguments`/`ApprovalNotFound` auffallen, nie
    /// als stille Genehmigung.
    fn default() -> Self {
        Self {
            session: SessionId::from_str(String::new()),
            request: ItemId::from_str(String::new()),
            decision: ReviewDecision::Rejected,
            comment: None,
            resolved_at: Timestamp::UNIX_EPOCH,
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
/// Leitet den `ApprovalActor` ausschließlich aus den über `SO_PEERCRED`
/// gelesenen Peer-Credentials her (Service `PeerCredentials` im
/// [`OpContext`]) und ruft dafür ausschließlich [`resolve_approval`] auf.
/// Trägt `web(..., approval = "none")`, obwohl sie mutiert — siehe
/// Moduldoku „Warum `approval.resolve` selbst keine Bestätigung verlangt":
/// sie autorisiert selbst nichts, sondern reicht eine bereits getroffene
/// menschliche Entscheidung an [`harw_session_store::approval::ApprovalStore::resolve`]
/// weiter, das über Aktualität, Actor-Identität und Einmaligkeit entscheidet.
///
/// # Arguments
/// - `ctx` (`&OpContext`): muss `Arc<ApprovalStore>`,
///   `Arc<dyn ApprovalActorResolver>` und `PeerCredentials` als Services
///   anbieten.
/// - `args` (`ApprovalResolveArgs`): Sitzung, Anfrage, Entscheidung,
///   optionaler Kommentar, Zeitstempel — kein Actor-Feld.
///
/// # Returns
/// `Ok(OpOutput { text })` mit dem durabel geschriebenen Auflösungsergebnis.
///
/// # Errors
/// - [`OpError::NotAvailable`]: kein Genehmigungsspeicher, kein
///   Actor-Resolver oder keine Peer-Credentials im Kontext registriert
///   (letzteres ist der im Abschlussbericht gemeldete Befund: der
///   `OpContext` muss `PeerCredentials` als Service tragen, sonst kann
///   diese Operation den Aufrufer nicht identifizieren); ebenso, wenn der
///   Peer keinem `ApprovalActor` zugeordnet ist oder der aufgelöste Actor
///   nicht zum bei `issue()` hinterlegten passt.
/// - [`OpError::InvalidArguments`]: die Anfrage existiert nicht (mehr) oder
///   wurde bereits aufgelöst — eine zweite Bestätigung wird abgewiesen,
///   nicht überschrieben.
///
/// # Concurrency
/// Delegiert die Sperrung vollständig an
/// [`harw_session_store::approval::ApprovalStore::resolve`] (Datei-Lock je
/// Sitzung).
///
/// # Examples
/// ```rust,no_run
/// // Aufruf erfolgt über Operation::run(); siehe Modultests für den
/// // direkten Aufruf mit einem Test-Resolver und Test-Peer.
/// ```
#[operation(
    name = "approval.resolve",
    summary = "Löst eine offene Genehmigungsanfrage anhand einer menschlichen Entscheidung auf. Autorisiert selbst nichts.",
    domain = "execution",
    permission = "operator",
    web(path = "/api/approval-resolve", approval = "none")
)]
async fn approval_resolve(
    ctx: &OpContext,
    args: ApprovalResolveArgs,
) -> Result<OpOutput, OpError> {
    let store = approval_store(ctx)?;
    let resolver = ctx
        .service::<Arc<dyn ApprovalActorResolver>>()
        .cloned()
        .ok_or_else(|| {
            OpError::NotAvailable("kein Genehmiger-Resolver im Kontext registriert".to_owned())
        })?;
    let peer = ctx.service::<PeerCredentials>().copied().ok_or_else(|| {
        OpError::NotAvailable(
            "keine Peer-Credentials im Kontext registriert — der OpContext muss \
             `PeerCredentials` als Service tragen, sonst kann diese Operation den \
             Aufrufer nicht identifizieren"
                .to_owned(),
        )
    })?;

    let resolution = resolve_approval(
        &store,
        resolver.as_ref(),
        &peer,
        &args.session,
        &args.request,
        args.decision,
        args.comment,
        args.resolved_at,
    )
    .map_err(map_security_error)?;

    Ok(OpOutput {
        text: format!(
            "Anfrage {} (Sitzung {}) aufgelöst als {:?} durch {:?} um {}.",
            resolution.request,
            resolution.session,
            resolution.decision,
            resolution.actor,
            resolution.resolved_at
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        ApprovalPendingArgs, ApprovalResolveArgs, approval_pending, approval_resolve,
        discover_pending_candidates,
    };
    use crate::testutil::toks;
    use harw_operations::{FromRawArgs, OpContext, OpError, context::ServiceMap};
    use harw_sandbox::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_session_store::approval::{ApprovalRecord, ApprovalStore};
    use harw_types::{ApprovalActor, ItemId, ReviewDecision, SessionId, TenantId, ToolCallId, TurnId, WorkspaceId};
    use harw_web::peer::PeerCredentials;
    use harw_web::security::{ApprovalActorResolver, StaticUidApprovalActorMap};
    use jiff::Timestamp;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    fn unique_root(prefix: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("{prefix}-{}-{id}", std::process::id()))
    }

    /// Baut einen Test-`OpContext`. `store`, `resolver` und `peer` werden nur
    /// eingefügt, wenn übergeben — genau das belegt den fehlenden
    /// Verdrahtungspfad in `approval_resolve_without_peer_credentials_...`.
    fn test_context(
        store: Option<Arc<ApprovalStore>>,
        resolver: Option<Arc<dyn ApprovalActorResolver>>,
        peer: Option<PeerCredentials>,
    ) -> (OpContext, PathBuf) {
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
        if let Some(store) = store {
            services.insert(store);
        }
        if let Some(resolver) = resolver {
            services.insert(resolver);
        }
        if let Some(peer) = peer {
            services.insert(peer);
        }
        (
            OpContext::new(SessionId::new(), TurnId::new(), sandbox, services),
            root,
        )
    }

    fn issue_pending(store: &ApprovalStore, session: &str, request: &str, actor_id: &str) {
        store
            .issue(&ApprovalRecord {
                request: ItemId::from_str(request),
                session: SessionId::from_str(session),
                call_id: ToolCallId::from_str("call-1"),
                actor: ApprovalActor::Operator {
                    id: actor_id.to_owned(),
                },
                issued_at: Timestamp::constant(0, 0),
            })
            .expect("issue pending approval");
    }

    fn resolver_for(entries: Vec<(u32, String)>) -> Arc<dyn ApprovalActorResolver> {
        Arc::new(StaticUidApprovalActorMap::new(entries))
    }

    #[test]
    fn approval_pending_args_from_raw_args_ignores_tokens() {
        let args = ApprovalPendingArgs::from_raw_args(&toks(&["ignored"])).expect("parses");
        let _ = args;
    }

    #[test]
    fn approval_resolve_args_from_raw_args_is_rejected() {
        let error = ApprovalResolveArgs::from_raw_args(&toks(&[])).unwrap_err();
        assert!(matches!(error, OpError::InvalidArguments(_)));
    }

    #[test]
    fn approval_resolve_args_deserializes_without_an_actor_field() {
        let json = serde_json::json!({
            "session": "session-1",
            "request": "approval-1",
            "decision": "approved",
            "resolved_at": "1970-01-01T00:00:00Z",
        });
        let args: ApprovalResolveArgs =
            serde_json::from_value(json).expect("deserializes without actor");
        assert_eq!(args.session.as_str(), "session-1");
        assert_eq!(args.decision, ReviewDecision::Approved);
    }

    #[tokio::test]
    async fn approval_pending_without_store_returns_not_available() {
        let (ctx, root) = test_context(None, None, None);
        let result = approval_pending(&ctx, ApprovalPendingArgs::default()).await;
        std::fs::remove_dir_all(root).ok();
        assert!(matches!(result, Err(OpError::NotAvailable(_))));
    }

    #[tokio::test]
    async fn approval_pending_with_empty_store_reports_none_open() {
        let store_root = unique_root("harw-ops-approval-empty-store");
        let store = Arc::new(ApprovalStore::new(&store_root));
        let (ctx, ws_root) = test_context(Some(store), None, None);
        let output = approval_pending(&ctx, ApprovalPendingArgs::default())
            .await
            .expect("list succeeds on empty store");
        std::fs::remove_dir_all(&store_root).ok();
        std::fs::remove_dir_all(ws_root).ok();
        assert_eq!(output.text, "Keine offenen Genehmigungsanfragen.");
    }

    #[tokio::test]
    async fn approval_pending_lists_an_issued_request() {
        let store_root = unique_root("harw-ops-approval-list-store");
        let store = Arc::new(ApprovalStore::new(&store_root));
        issue_pending(&store, "session-1", "approval-1", "alice");
        let (ctx, ws_root) = test_context(Some(Arc::clone(&store)), None, None);

        let output = approval_pending(&ctx, ApprovalPendingArgs::default())
            .await
            .expect("list succeeds");

        let candidates =
            discover_pending_candidates(&store).expect("discovery reads the store root");
        std::fs::remove_dir_all(&store_root).ok();
        std::fs::remove_dir_all(ws_root).ok();

        assert_eq!(candidates.len(), 1);
        assert!(output.text.contains("approval-1"));
        assert!(output.text.contains("session-1"));
        assert!(output.text.contains("1 offene"));
    }

    #[tokio::test]
    async fn approval_resolve_without_approval_store_returns_not_available() {
        let (ctx, root) = test_context(None, None, None);
        let args = ApprovalResolveArgs {
            session: SessionId::from_str("session-1"),
            request: ItemId::from_str("approval-1"),
            decision: ReviewDecision::Approved,
            comment: None,
            resolved_at: Timestamp::constant(1, 0),
        };
        let result = approval_resolve(&ctx, args).await;
        std::fs::remove_dir_all(root).ok();
        assert!(matches!(result, Err(OpError::NotAvailable(_))));
    }

    /// Der gemeldete Befund dieses Knotens: ohne einen `PeerCredentials`-
    /// Service im `OpContext` kann `approval_resolve` den Aufrufer nicht
    /// identifizieren — sie erfindet keinen Ersatz-Actor, sondern meldet
    /// `NotAvailable` mit einer Nachricht, die genau das benennt.
    #[tokio::test]
    async fn approval_resolve_without_peer_credentials_reports_the_missing_wiring() {
        let store_root = unique_root("harw-ops-approval-no-peer-store");
        let store = Arc::new(ApprovalStore::new(&store_root));
        issue_pending(&store, "session-1", "approval-1", "alice");
        let resolver = resolver_for(vec![(1000, "alice".to_owned())]);
        let (ctx, ws_root) = test_context(Some(store), Some(resolver), None);

        let args = ApprovalResolveArgs {
            session: SessionId::from_str("session-1"),
            request: ItemId::from_str("approval-1"),
            decision: ReviewDecision::Approved,
            comment: None,
            resolved_at: Timestamp::constant(1, 0),
        };
        let result = approval_resolve(&ctx, args).await;
        std::fs::remove_dir_all(&store_root).ok();
        std::fs::remove_dir_all(ws_root).ok();

        match result {
            Err(OpError::NotAvailable(message)) => {
                assert!(message.contains("Peer-Credentials"));
            }
            other => panic!("expected NotAvailable mentioning Peer-Credentials, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn approval_resolve_rejects_a_peer_unknown_to_the_resolver() {
        let store_root = unique_root("harw-ops-approval-unknown-peer-store");
        let store = Arc::new(ApprovalStore::new(&store_root));
        issue_pending(&store, "session-1", "approval-1", "alice");
        let resolver = resolver_for(vec![(1000, "alice".to_owned())]);
        let (ctx, ws_root) = test_context(
            Some(store),
            Some(resolver),
            Some(PeerCredentials::new(1, 9999, 9999)),
        );

        let args = ApprovalResolveArgs {
            session: SessionId::from_str("session-1"),
            request: ItemId::from_str("approval-1"),
            decision: ReviewDecision::Approved,
            comment: None,
            resolved_at: Timestamp::constant(1, 0),
        };
        let result = approval_resolve(&ctx, args).await;
        std::fs::remove_dir_all(&store_root).ok();
        std::fs::remove_dir_all(ws_root).ok();

        assert!(matches!(result, Err(OpError::NotAvailable(_))));
    }

    /// Belegt zusätzlich zur Typsignatur (kein `actor`-Feld in
    /// `ApprovalResolveArgs`), dass der tatsächlich verwendete Actor exakt
    /// der aus den Peer-Credentials abgeleitete ist — keine autorisierte
    /// Aktion entsteht dabei: nur der bereits durabel gespeicherte Zustand
    /// wechselt von "pending" zu "resolved".
    #[tokio::test]
    async fn approval_resolve_derives_the_actor_from_peer_credentials_and_resolves_once() {
        let store_root = unique_root("harw-ops-approval-resolve-store");
        let store = Arc::new(ApprovalStore::new(&store_root));
        issue_pending(&store, "session-1", "approval-1", "alice");
        let resolver = resolver_for(vec![(1000, "alice".to_owned())]);
        let (ctx, ws_root) = test_context(
            Some(Arc::clone(&store)),
            Some(resolver),
            Some(PeerCredentials::new(1, 1000, 1000)),
        );

        let args = ApprovalResolveArgs {
            session: SessionId::from_str("session-1"),
            request: ItemId::from_str("approval-1"),
            decision: ReviewDecision::ApprovedOnce,
            comment: Some("bounded exception".to_owned()),
            resolved_at: Timestamp::constant(2, 0),
        };
        let output = approval_resolve(&ctx, args)
            .await
            .expect("resolution succeeds for the known peer");
        assert!(output.text.contains("approval-1"));
        assert!(output.text.contains("ApprovedOnce"));
        assert!(output.text.contains("alice"));

        // Nach der Auflösung ist die Anfrage nicht mehr "pending" — der
        // Endpunkt ändert nur den bereits vorhandenen Zustand des Speichers.
        let remaining = discover_pending_candidates(&store).expect("discovery still works");
        std::fs::remove_dir_all(&store_root).ok();
        std::fs::remove_dir_all(ws_root).ok();
        assert!(remaining.is_empty());
    }

    /// Eine zweite Bestätigung derselben Anfrage wird abgewiesen, nicht
    /// überschrieben.
    #[tokio::test]
    async fn approval_resolve_rejects_a_second_confirmation_of_the_same_request() {
        let store_root = unique_root("harw-ops-approval-twice-store");
        let store = Arc::new(ApprovalStore::new(&store_root));
        issue_pending(&store, "session-1", "approval-1", "alice");
        let resolver = resolver_for(vec![(1000, "alice".to_owned())]);
        let peer = PeerCredentials::new(1, 1000, 1000);

        let (ctx_first, ws_root_first) =
            test_context(Some(Arc::clone(&store)), Some(Arc::clone(&resolver)), Some(peer));
        approval_resolve(
            &ctx_first,
            ApprovalResolveArgs {
                session: SessionId::from_str("session-1"),
                request: ItemId::from_str("approval-1"),
                decision: ReviewDecision::Approved,
                comment: None,
                resolved_at: Timestamp::constant(3, 0),
            },
        )
        .await
        .expect("first resolution succeeds");

        let (ctx_second, ws_root_second) = test_context(Some(Arc::clone(&store)), Some(resolver), Some(peer));
        let replay = approval_resolve(
            &ctx_second,
            ApprovalResolveArgs {
                session: SessionId::from_str("session-1"),
                request: ItemId::from_str("approval-1"),
                decision: ReviewDecision::Rejected,
                comment: None,
                resolved_at: Timestamp::constant(4, 0),
            },
        )
        .await
        .unwrap_err();

        std::fs::remove_dir_all(&store_root).ok();
        std::fs::remove_dir_all(ws_root_first).ok();
        std::fs::remove_dir_all(ws_root_second).ok();

        assert!(matches!(replay, OpError::InvalidArguments(message) if message.contains("bereits aufgelöst")));
    }
}
