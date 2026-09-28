//! Lokale Peer-Identitätsauflösung — `SO_PEERCRED` → Tier + Mandant +
//! (optional) vom SecurityHub bestätigter Sicherheitskontext (Masterplan v2
//! §13, §14, §15, §25; Welle H12).
//!
//! # Verantwortungsbereich
//! Bis H12 kannte `harw-web` nur `SO_PEERCRED → PeerAuthorizer →
//! PermissionTier`. Dieses Modul hebt das auf
//!
//! ```text
//! SO_PEERCRED ─► LocalPeerIdentityResolver ─► ResolvedPeer { tier, tenant, principal_id, summary, source }
//!                         │ (Modus security_hub)
//!                         └─► SecurityHub GET /v1/contexts/{id}
//! ```
//!
//! und reicht das Ergebnis unverändert an die bestehende Routenentscheidung
//! ([`crate::router::decide_resolved_route`]) und an den Operationskontext
//! ([`ResolvedPeer::scope_op_context`] → `OpContext::tenant`) weiter. Es gibt
//! keinen zweiten Autoritätspfad: die Tier-Ablehnungsmatrix bleibt
//! [`crate::authz::tier_permits`].
//!
//! # Zwei Modi (`[web.identity] mode`)
//! - **`tier_map`** (Vorgabe): [`TierMapResolver`] — exakt das bisherige
//!   Verhalten von [`crate::authz::PeerAuthorizer`], ergänzt um einen
//!   optionalen, rein konfigurierten Mandanten je UID (`uid_tenants`). Ein
//!   vorgelegter Kontext-Header wird ignoriert.
//! - **`security_hub`**: [`SecurityHubResolver`] — ein lokaler Client legt
//!   im Header [`SECURITY_CONTEXT_HEADER`] die **Id** eines Kontexts vor, den
//!   er sich selbst beim SecurityHub geholt hat; `harw-web` prüft sie über
//!   [`ContextVerifier`] (produktiv: [`SecurityHubClient::verify_context`]).
//!
//! # Das Vertrauensmodell im Modus `security_hub`
//! Der SecurityHub leitet die Ansprüche eines Kontexts aus **seinen eigenen**
//! `SO_PEERCRED` ab. Würde `harw-web` einen Kontext „für den Nutzer"
//! anfordern, sähe der Hub `harw-web` als Peer, nicht den Endnutzer — und
//! `harw-web` müsste die Nutzer-UID im Rumpf mitschicken, also genau den
//! Identitätsanspruch, den kein Rumpf tragen darf (§14). Darum:
//!
//! 1. Der lokale Client (Browser-Bridge, CLI, TUI) ruft selbst
//!    `POST /v1/contexts` auf `security.sock`. Der Hub sieht die **echte**
//!    UID des Nutzers und stellt einen kurzlebigen Kontext aus.
//! 2. Der Client legt `harw-web` nur die **Id** vor
//!    (`x-harw-security-context: <id>`) — eine Referenz, keinen Wert (§25).
//! 3. `harw-web` (in der Hub-Richtlinie als *Verifier* eingetragen) fragt
//!    `GET /v1/contexts/{id}`. `200` liefert die Zusammenfassung, die der Hub
//!    über **seinen** authentisierten Socket verbürgt; `410` (abgelaufen oder
//!    widerrufen) und `404` werden abgelehnt.
//! 4. **Bindung:** die `principal_id` des Kontexts muss dem in
//!    `uid_principals` für die `SO_PEERCRED`-UID **dieser** Verbindung
//!    konfigurierten Principal entsprechen. Eine gestohlene Kontext-Id nützt
//!    einem anderen lokalen Nutzer damit nichts.
//! 5. Der Tier bleibt durch die lokale Tier-Tabelle gedeckelt; der Kontext
//!    kann ihn nur **verengen** (`min(tabelle, kontext)`), nie anheben. Ein
//!    Peer ohne Tabelleneintrag bleibt unbekannt — auch mit gültigem Kontext.
//! 6. Mandant: der des Kontexts; ist zusätzlich `uid_tenants` für die UID
//!    gesetzt, müssen beide übereinstimmen (sonst Ablehnung).
//!
//! Fehlt der Header oder ist der Hub nicht erreichbar, entscheidet
//! `require_context`: `true` → Ablehnung, `false` → Rückfall auf
//! [`TierMapResolver`] — **außer** die UID hat einen `uid_principals`-
//! Eintrag, aber `uid_tenants` pinnt für sie keinen Mandanten. Dann würde
//! der Rückfall den vom Hub zugewiesenen Mandanten stillschweigend fallen
//! lassen und den Aufrufer unscoped (§15, sieht dann **alle** Mandanten)
//! weiterlaufen lassen; das wird wie `require_context = true` abgelehnt.
//! Ein **vorgelegter, aber ungültiger** Kontext (falsches Format,
//! unbekannt, abgelaufen/widerrufen, fremder Principal, fremder Mandant)
//! wird **immer** abgelehnt — ein Rückfall würde einen kaputten Nachweis
//! stillschweigend in einen Einzelnutzer-Zugang umdeuten.
//!
//! # Was nie gelesen wird
//! `harw-web` liest weder `x-harw-principal` noch `x-harw-tenant` noch
//! `x-harw-tier` noch irgendein Rumpffeld als Identität. Der Mandant eines
//! Aufrufs ist damit **nicht** durch Editieren des Rumpfs änderbar (H12-
//! Exit-Kriterium): [`LocalPeerIdentityResolver::resolve`] bekommt den Rumpf
//! gar nicht zu sehen.
//!
//! # Mandanten-Scoping (§15)
//! [`ResolvedPeer::scope_op_context`] setzt `OpContext::with_tenant` und
//! hängt die (nicht-autoritative) Zusammenfassung an. Operationen, die
//! mandantengebundene Daten auflisten, filtern mit
//! `harw_operations::context::tenant_admits` (Aufrufer ohne Mandant sieht
//! alles wie bisher; mit Mandant nur Elemente genau dieses Mandanten).
//!
//! # Nebenläufigkeit
//! Alle Resolver sind `Send + Sync` und werden hinter `Arc` geteilt;
//! [`LocalPeerIdentityResolver::resolve`] liefert eine `Send`-Future. Eine
//! Hub-Anfrage läuft je Web-Anfrage mit Kontext-Header, begrenzt durch
//! [`HUB_VERIFY_TIMEOUT`].

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use harw_infra_client::{
    ClientOptions, ContextVerification, DEFAULT_SECURITY_SOCKET, InfraClientError,
    SecurityHubClient,
};
use harw_operations::context::OpContext;
use harw_operations::operation::PermissionTier;
use harw_types::{AuthStrength, Clock, SecurityContextSummary, SystemClock, TenantId, TrustZone};
use hyper::HeaderMap;
use serde::Deserialize;

use crate::authz::PeerAuthorizer;
use crate::peer::PeerCredentials;

/// Der Header, in dem ein lokaler Client die **Id** eines beim SecurityHub
/// selbst bezogenen Kontexts vorlegt. Nur eine Referenz; der Inhalt wird
/// ausschließlich beim Hub nachgeschlagen.
pub const SECURITY_CONTEXT_HEADER: &str = "x-harw-security-context";

/// Frist für eine Kontextprüfung beim SecurityHub.
pub const HUB_VERIFY_TIMEOUT: Duration = Duration::from_secs(2);

/// Längste akzeptierte Kontext-Id (identisch zur Grenze des Hubs).
const MAX_CONTEXT_ID_LEN: usize = harw_infra_client::MAX_CONTEXT_ID_LEN;

/// Woher eine [`ResolvedPeer`]-Identität stammt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentitySource {
    /// Aus `SO_PEERCRED` + statischer Konfiguration (Tier-Tabelle,
    /// `uid_tenants`) — das Verhalten vor H12.
    TierMap,
    /// Zusätzlich durch einen vom SecurityHub bestätigten Kontext gebunden.
    SecurityHub,
}

/// Die aufgelöste Identität eines Web-Aufrufers.
///
/// # Description
/// Nur die Resolver dieses Moduls bauen Werte dieses Typs (private Felder);
/// ein HTTP-Request kann keinen erzeugen. `tenant` ist der verbindliche
/// Mandanten-Scope; `summary` ist die nicht-autoritative Anzeige-Kopie des
/// Hub-Kontexts (siehe `harw_types::security`-Moduldoku).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPeer {
    tier: PermissionTier,
    tenant: Option<TenantId>,
    principal_id: String,
    summary: Option<SecurityContextSummary>,
    source: IdentitySource,
}

impl ResolvedPeer {
    /// Die wirksame Berechtigungsstufe (Eingang der Tier-Ablehnungsmatrix).
    #[must_use]
    pub fn tier(&self) -> PermissionTier {
        self.tier
    }

    /// Der Mandanten-Scope, falls einer aufgelöst wurde.
    #[must_use]
    pub fn tenant(&self) -> Option<&TenantId> {
        self.tenant.as_ref()
    }

    /// Principal-Id: im Modus `tier_map` `uid:<uid>`, sonst die des
    /// Hub-Kontexts.
    #[must_use]
    pub fn principal_id(&self) -> &str {
        &self.principal_id
    }

    /// Die Hub-Zusammenfassung (nur Modus `security_hub` mit Kontext).
    #[must_use]
    pub fn summary(&self) -> Option<&SecurityContextSummary> {
        self.summary.as_ref()
    }

    /// Die Herkunft dieser Identität.
    #[must_use]
    pub fn source(&self) -> IdentitySource {
        self.source
    }

    /// Fädelt Mandant und Zusammenfassung in einen frisch gebauten
    /// [`OpContext`] ein.
    ///
    /// # Description
    /// Aufgerufen genau dort, wo `harw-web` den Kontext einer freigegebenen
    /// Ausführung baut (direkt nach der `WebContextFactory`). Ohne Mandant
    /// und ohne Zusammenfassung bleibt der Kontext unverändert — das ist der
    /// Modus `tier_map` ohne `uid_tenants`, also das Verhalten vor H12.
    ///
    /// # Arguments
    /// - `ctx` (`OpContext`): der von der Kontext-Fabrik gebaute Kontext.
    ///
    /// # Returns
    /// Denselben Kontext mit `tenant()`/`security_context_summary()` gesetzt.
    #[must_use]
    pub fn scope_op_context(&self, ctx: OpContext) -> OpContext {
        let ctx = match &self.tenant {
            Some(tenant) => ctx.with_tenant(tenant.clone()),
            None => ctx,
        };
        match &self.summary {
            Some(summary) => ctx.with_security_context_summary(summary.clone()),
            None => ctx,
        }
    }
}

/// Warum eine Identität nicht aufgelöst werden konnte. Jeder Fall führt zu
/// `403` (Grund: [`Self::code`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityError {
    /// Die UID ist in der Tier-Tabelle unbekannt.
    UnknownPeer,
    /// Kein Kontext-Header vorgelegt, und entweder `require_context = true`
    /// oder die UID hat einen `uid_principals`-Eintrag ohne `uid_tenants`-Pin
    /// (Rückfall gesperrt, siehe Moduldoku).
    ContextRequired,
    /// Header mehrfach, leer, nicht ASCII oder keine gültige Kontext-Id.
    InvalidContextReference,
    /// Für die UID ist kein `uid_principals`-Eintrag konfiguriert — ein
    /// Kontext kann ihr nicht zugeordnet werden.
    NoPrincipalBinding,
    /// Der Hub kennt den Kontext nicht (oder verbirgt ihn, `404`), oder die
    /// zurückgelieferte Id passt nicht zur vorgelegten.
    ContextUnknown,
    /// Der Kontext ist abgelaufen oder widerrufen (`410`).
    ContextGone,
    /// Der Kontext gehört einem anderen Principal als dem der Peer-UID.
    PrincipalMismatch,
    /// Mandant des Kontexts widerspricht dem für die UID konfigurierten.
    TenantMismatch,
    /// Authentisierungsstärke unter `peer_credential` oder Vertrauenszone
    /// jenseits von `host`.
    ContextNotPermitted,
    /// Der Hub war nicht erreichbar (Socket fehlt, Zeitüberschreitung, `503`),
    /// und entweder `require_context = true` oder die UID hat einen
    /// `uid_principals`-Eintrag ohne `uid_tenants`-Pin (Rückfall gesperrt,
    /// siehe Moduldoku).
    HubUnavailable,
    /// Der Hub antwortete, aber nicht vertragsgemäß oder verweigerte die
    /// Prüfung (`harw-web` ist kein Verifier) — immer Ablehnung.
    HubFailure,
}

impl IdentityError {
    /// Stabiler, maschinenlesbarer Grund für den `reason` der `403`-Antwort.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnknownPeer => "unknown_peer",
            Self::ContextRequired => "security_context_required",
            Self::InvalidContextReference => "invalid_security_context",
            Self::NoPrincipalBinding => "no_principal_binding",
            Self::ContextUnknown => "security_context_unknown",
            Self::ContextGone => "security_context_gone",
            Self::PrincipalMismatch => "security_context_principal_mismatch",
            Self::TenantMismatch => "security_context_tenant_mismatch",
            Self::ContextNotPermitted => "security_context_not_permitted",
            Self::HubUnavailable => "security_hub_unavailable",
            Self::HubFailure => "security_hub_failure",
        }
    }
}

impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for IdentityError {}

/// Liest den vorgelegten Kontextverweis aus den Request-Headern.
///
/// # Returns
/// - `Ok(None)`: kein [`SECURITY_CONTEXT_HEADER`].
/// - `Ok(Some(id))`: genau ein Header mit nicht-leerem ASCII-Wert.
///
/// # Errors
/// [`IdentityError::InvalidContextReference`] bei mehreren Headern, leerem
/// oder nicht als sichtbares ASCII lesbarem Wert.
pub fn presented_context(headers: &HeaderMap) -> Result<Option<&str>, IdentityError> {
    let mut values = headers.get_all(SECURITY_CONTEXT_HEADER).iter();
    let Some(first) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(IdentityError::InvalidContextReference);
    }
    let value = first
        .to_str()
        .map_err(|_| IdentityError::InvalidContextReference)?
        .trim();
    if value.is_empty() {
        return Err(IdentityError::InvalidContextReference);
    }
    Ok(Some(value))
}

/// Future eines [`LocalPeerIdentityResolver`].
pub type IdentityFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ResolvedPeer, IdentityError>> + Send + 'a>>;

/// Löst die Identität eines lokalen Peers auf.
///
/// # Description
/// Einmal bei der Komposition aus serverseitig vertrauter Konfiguration
/// gebaut ([`WebIdentityConfig::build_resolver`]). `presented_context` ist
/// ausschließlich der Wert von [`SECURITY_CONTEXT_HEADER`] (siehe
/// [`presented_context`]) — eine Referenz, die der Resolver beim Hub
/// nachschlägt, nie ein Identitätsanspruch.
///
/// # Concurrency
/// `Send + Sync`, hinter `Arc` über alle Verbindungs-Tasks geteilt.
pub trait LocalPeerIdentityResolver: Send + Sync {
    /// Löst `peer` (plus optionalen Kontextverweis) auf.
    ///
    /// # Errors
    /// Siehe [`IdentityError`]; jeder Fehler bedeutet „nicht autorisiert".
    fn resolve<'a>(
        &'a self,
        peer: &'a PeerCredentials,
        presented_context: Option<&'a str>,
    ) -> IdentityFuture<'a>;
}

/// UID → Mandant, aus `uid_tenants`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UidTenantMap {
    entries: HashMap<u32, TenantId>,
}

impl UidTenantMap {
    /// Baut die Tabelle aus Paaren.
    #[must_use]
    pub fn new(entries: impl IntoIterator<Item = (u32, TenantId)>) -> Self {
        Self {
            entries: entries.into_iter().collect(),
        }
    }

    /// Der Mandant für `uid`, falls konfiguriert.
    #[must_use]
    pub fn get(&self, uid: u32) -> Option<&TenantId> {
        self.entries.get(&uid)
    }
}

/// Der Resolver des Modus `tier_map` — bisheriges Verhalten plus
/// konfigurierter Mandant.
///
/// # Description
/// Der Tier kommt unverändert aus dem übergebenen [`PeerAuthorizer`]
/// (typisch `StaticUidTierMap`), der Mandant aus [`UidTenantMap`]. Ein
/// vorgelegter Kontext-Header hat in diesem Modus keine Bedeutung und wird
/// ignoriert.
pub struct TierMapResolver {
    authorizer: Arc<dyn PeerAuthorizer>,
    tenants: UidTenantMap,
}

impl TierMapResolver {
    /// Resolver ohne Mandanten — exakt das Verhalten vor H12.
    #[must_use]
    pub fn new(authorizer: Arc<dyn PeerAuthorizer>) -> Self {
        Self::with_tenants(authorizer, UidTenantMap::default())
    }

    /// Resolver mit UID→Mandant-Tabelle.
    #[must_use]
    pub fn with_tenants(authorizer: Arc<dyn PeerAuthorizer>, tenants: UidTenantMap) -> Self {
        Self {
            authorizer,
            tenants,
        }
    }

    /// Synchrone Auflösung (ohne Hub).
    ///
    /// # Errors
    /// [`IdentityError::UnknownPeer`], wenn der Authorizer `None` liefert.
    pub fn resolve_peer(&self, peer: &PeerCredentials) -> Result<ResolvedPeer, IdentityError> {
        let tier = self
            .authorizer
            .tier_for(peer)
            .ok_or(IdentityError::UnknownPeer)?;
        Ok(ResolvedPeer {
            tier,
            tenant: self.tenants.get(peer.uid).cloned(),
            principal_id: format!("uid:{}", peer.uid),
            summary: None,
            source: IdentitySource::TierMap,
        })
    }

    fn tier_for(&self, peer: &PeerCredentials) -> Option<PermissionTier> {
        self.authorizer.tier_for(peer)
    }
}

impl LocalPeerIdentityResolver for TierMapResolver {
    fn resolve<'a>(
        &'a self,
        peer: &'a PeerCredentials,
        _presented_context: Option<&'a str>,
    ) -> IdentityFuture<'a> {
        let result = self.resolve_peer(peer);
        Box::pin(async move { result })
    }
}

/// Antwort des Hubs auf eine Kontextprüfung.
#[derive(Debug, Clone, PartialEq, Eq)]
// Ein kurzlebiger Rückgabewert je Anfrage; Boxen brächte nur eine
// zusätzliche Allokation.
#[allow(clippy::large_enum_variant)]
pub enum HubVerdict {
    /// Aktiv; die vom Hub verbürgte Zusammenfassung.
    Active(SecurityContextSummary),
    /// Abgelaufen oder widerrufen (`410`).
    Gone,
    /// Unbekannt oder verborgen (`404`).
    Unknown,
}

/// Warum der Hub keine Antwort geliefert hat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifierError {
    /// Nicht erreichbar / Zeitüberschreitung / `503` — rückfallfähig.
    Unavailable,
    /// Vertragsverletzung oder verweigerte Prüfung — nie rückfallfähig.
    Failed,
}

/// Future eines [`ContextVerifier`].
pub type VerifyFuture<'a> =
    Pin<Box<dyn Future<Output = Result<HubVerdict, VerifierError>> + Send + 'a>>;

/// Prüft eine Kontext-Id beim SecurityHub. Produktiv implementiert von
/// [`SecurityHubClient`]; Tests injizieren eine Attrappe.
pub trait ContextVerifier: Send + Sync {
    /// `GET /v1/contexts/{id}`.
    fn verify<'a>(&'a self, context_id: &'a str) -> VerifyFuture<'a>;
}

impl ContextVerifier for SecurityHubClient {
    fn verify<'a>(&'a self, context_id: &'a str) -> VerifyFuture<'a> {
        Box::pin(async move {
            match self
                .verify_context::<SecurityContextSummary>(context_id)
                .await
            {
                Ok(ContextVerification::Active(summary)) => Ok(HubVerdict::Active(summary)),
                Ok(ContextVerification::Gone) => Ok(HubVerdict::Gone),
                Ok(ContextVerification::Unknown) => Ok(HubVerdict::Unknown),
                Err(InfraClientError::Unavailable | InfraClientError::Timeout) => {
                    Err(VerifierError::Unavailable)
                }
                Err(error) => {
                    tracing::warn!(%error, "security hub context verification failed");
                    Err(VerifierError::Failed)
                }
            }
        })
    }
}

/// `true` für eine syntaktisch zulässige Kontext-Id (dieselbe Regel wie im
/// Hub: 1..=128 Bytes aus `[A-Za-z0-9_-]`).
fn is_valid_context_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_CONTEXT_ID_LEN
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// Der Resolver des Modus `security_hub`; siehe Moduldoku für das Modell.
pub struct SecurityHubResolver {
    fallback: TierMapResolver,
    verifier: Arc<dyn ContextVerifier>,
    principals: HashMap<u32, String>,
    require_context: bool,
    clock: Arc<dyn Clock>,
}

impl SecurityHubResolver {
    /// Baut den Resolver.
    ///
    /// # Arguments
    /// - `fallback` (`TierMapResolver`): Tier-Deckel und Rückfall.
    /// - `verifier` (`Arc<dyn ContextVerifier>`): Hub-Zugang.
    /// - `principals`: UID → erwartete `principal_id` des Kontexts.
    /// - `require_context` (`bool`): ohne Kontext/erreichbaren Hub ablehnen.
    #[must_use]
    pub fn new(
        fallback: TierMapResolver,
        verifier: Arc<dyn ContextVerifier>,
        principals: impl IntoIterator<Item = (u32, String)>,
        require_context: bool,
    ) -> Self {
        Self {
            fallback,
            verifier,
            principals: principals.into_iter().collect(),
            require_context,
            clock: Arc::new(SystemClock),
        }
    }

    /// Ersetzt die Uhr (Tests).
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Rückfall bzw. Ablehnung, wenn kein Kontext genutzt werden kann.
    ///
    /// # Description
    /// `require_context = true` lehnt immer ab. Bei `false` fällt die Regel
    /// grundsätzlich auf [`TierMapResolver`] zurück — außer die UID ist über
    /// `uid_principals` an einen Hub-Principal gebunden, aber `uid_tenants`
    /// pinnt für sie keinen Mandanten: der Kontext (den es hier gerade nicht
    /// gibt) wäre in diesem Fall die einzige Mandantenquelle, und der
    /// Rückfall würde den Aufrufer sonst mandantenlos — also mit Sicht auf
    /// **alle** Mandanten (§15) — weiterlaufen lassen. Dann gilt dieselbe
    /// Ablehnung wie bei `require_context = true`.
    fn without_context(
        &self,
        peer: &PeerCredentials,
        denial: IdentityError,
    ) -> Result<ResolvedPeer, IdentityError> {
        if self.require_context {
            return Err(denial);
        }
        let has_principal_binding = self.principals.contains_key(&peer.uid);
        let has_tenant_pin = self.fallback.tenants.get(peer.uid).is_some();
        if has_principal_binding && !has_tenant_pin {
            return Err(denial);
        }
        self.fallback.resolve_peer(peer)
    }

    async fn resolve_inner(
        &self,
        peer: &PeerCredentials,
        presented: Option<&str>,
    ) -> Result<ResolvedPeer, IdentityError> {
        // Die lokale Tier-Tabelle ist immer der Deckel: ohne Eintrag kein Zugang.
        let table_tier = self
            .fallback
            .tier_for(peer)
            .ok_or(IdentityError::UnknownPeer)?;

        let Some(context_id) = presented else {
            return self.without_context(peer, IdentityError::ContextRequired);
        };
        // Ein vorgelegter, aber kaputter Verweis wird nie in einen Rückfall umgedeutet.
        if !is_valid_context_id(context_id) {
            return Err(IdentityError::InvalidContextReference);
        }
        let expected_principal = self
            .principals
            .get(&peer.uid)
            .ok_or(IdentityError::NoPrincipalBinding)?;

        let summary = match self.verifier.verify(context_id).await {
            Ok(HubVerdict::Active(summary)) => summary,
            Ok(HubVerdict::Gone) => return Err(IdentityError::ContextGone),
            Ok(HubVerdict::Unknown) => return Err(IdentityError::ContextUnknown),
            Err(VerifierError::Unavailable) => {
                tracing::warn!(
                    uid = peer.uid,
                    "security hub unavailable during context verification"
                );
                return self.without_context(peer, IdentityError::HubUnavailable);
            }
            Err(VerifierError::Failed) => return Err(IdentityError::HubFailure),
        };

        if summary.id.as_str() != context_id {
            return Err(IdentityError::ContextUnknown);
        }
        if summary.expires_at <= self.clock.now() {
            return Err(IdentityError::ContextGone);
        }
        if summary.principal_id != *expected_principal {
            tracing::warn!(
                uid = peer.uid,
                presented_principal = %summary.principal_id,
                "security context principal does not match peer uid"
            );
            return Err(IdentityError::PrincipalMismatch);
        }
        if !summary.auth_strength.at_least(AuthStrength::PeerCredential)
            || !summary.trust_zone.is_within(TrustZone::Host)
        {
            return Err(IdentityError::ContextNotPermitted);
        }
        let configured_tenant = self.fallback.tenants.get(peer.uid);
        let tenant = match (summary.tenant.as_ref(), configured_tenant) {
            (Some(from_hub), Some(configured)) if from_hub != configured => {
                return Err(IdentityError::TenantMismatch);
            }
            (Some(from_hub), _) => Some(from_hub.clone()),
            (None, configured) => configured.cloned(),
        };

        Ok(ResolvedPeer {
            // Der Kontext kann nur verengen, nie anheben.
            tier: std::cmp::min(table_tier, summary.permission_tier),
            tenant,
            principal_id: summary.principal_id.clone(),
            summary: Some(summary),
            source: IdentitySource::SecurityHub,
        })
    }
}

impl LocalPeerIdentityResolver for SecurityHubResolver {
    fn resolve<'a>(
        &'a self,
        peer: &'a PeerCredentials,
        presented_context: Option<&'a str>,
    ) -> IdentityFuture<'a> {
        Box::pin(self.resolve_inner(peer, presented_context))
    }
}

// ── Konfiguration: [web.identity] ─────────────────────────────────────────────

/// `[web.identity] mode`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityMode {
    /// Vorgabe: UID → Tier (+ optional Mandant), wie vor H12.
    #[default]
    TierMap,
    /// Zusätzlich Kontextbindung über den SecurityHub.
    SecurityHub,
}

/// Die Tabelle `[web.identity]`.
///
/// ```toml
/// [web.identity]
/// mode = "security_hub"                        # oder "tier_map" (Vorgabe)
/// security_socket = "/run/harw/infra/security.sock"
/// require_context = true
/// uid_tenants = { "1000" = "tenant-a", "1001" = "tenant-b" }
/// uid_principals = { "1000" = "alice", "1001" = "bob" }
/// ```
///
/// Unbekannte Schlüssel werden abgelehnt (`deny_unknown_fields`). Ohne die
/// Tabelle gilt `mode = "tier_map"` ohne Mandanten — das Verhalten vor H12.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebIdentityConfig {
    /// Auflösungsmodus.
    #[serde(default)]
    pub mode: IdentityMode,
    /// Socket des SecurityHub (nur `security_hub`; Vorgabe
    /// [`DEFAULT_SECURITY_SOCKET`]).
    #[serde(default)]
    pub security_socket: Option<PathBuf>,
    /// Nur `security_hub`: ohne Kontext bzw. erreichbaren Hub ablehnen statt
    /// auf die Tier-Tabelle zurückzufallen. Bei `false` bleibt der Rückfall
    /// für eine UID mit `uid_principals`-Eintrag aber trotzdem gesperrt,
    /// solange `uid_tenants` für sie keinen Mandanten pinnt — sonst würde
    /// der Rückfall den vom Hub zugewiesenen Mandanten stillschweigend
    /// fallen lassen (siehe Moduldoku).
    #[serde(default)]
    pub require_context: bool,
    /// UID (als Zeichenkette) → Mandant. Schlüssel werden über `parse_uid`
    /// normalisiert; zwei Schlüssel, die auf dieselbe UID abbilden (z. B.
    /// `"1000"` und `"01000"`), werden abgelehnt statt stillschweigend die
    /// Reihenfolge der TOML-Tabelle entscheiden zu lassen.
    #[serde(default)]
    pub uid_tenants: BTreeMap<String, String>,
    /// Nur `security_hub`: UID (als Zeichenkette) → erwartete Principal-Id
    /// des vorgelegten Kontexts (muss zur Hub-Richtlinie passen). Dieselbe
    /// Normalisierungs-/Duplikatsprüfung wie bei `uid_tenants`.
    #[serde(default)]
    pub uid_principals: BTreeMap<String, String>,
}

/// Fehler beim Validieren von [`WebIdentityConfig`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityConfigError {
    /// Ein Schlüssel in `uid_tenants`/`uid_principals` ist keine `u32`.
    InvalidUid {
        /// Der betroffene Schlüssel.
        key: String,
    },
    /// Ein Mandant ist leer oder ungültig.
    InvalidTenant {
        /// Die betroffene UID.
        uid: u32,
    },
    /// Eine Principal-Id ist leer.
    BlankPrincipal {
        /// Die betroffene UID.
        uid: u32,
    },
    /// Ein nur im Modus `security_hub` gültiger Schlüssel steht im Modus
    /// `tier_map` — er hätte dort keine Wirkung und würde Schutz vortäuschen.
    SecurityHubOnly {
        /// Der betroffene Schlüssel.
        field: &'static str,
    },
    /// Modus `security_hub` ohne einen einzigen `uid_principals`-Eintrag.
    MissingPrincipals,
    /// Zwei (nach `parse_uid` normalisierte) Schlüssel in `uid_tenants` bzw.
    /// `uid_principals` bezeichnen dieselbe UID (z. B. `"1000"` und
    /// `"01000"`) — welcher Eintrag gewinnt, hinge sonst von der
    /// TOML-Schlüsselreihenfolge ab.
    DuplicateUid {
        /// `"uid_tenants"` oder `"uid_principals"`.
        field: &'static str,
        /// Die betroffene UID.
        uid: u32,
    },
}

impl fmt::Display for IdentityConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUid { key } => write!(f, "[web.identity]: `{key}` is not a numeric uid"),
            Self::InvalidTenant { uid } => {
                write!(
                    f,
                    "[web.identity] uid_tenants: invalid tenant for uid {uid}"
                )
            }
            Self::BlankPrincipal { uid } => {
                write!(
                    f,
                    "[web.identity] uid_principals: blank principal for uid {uid}"
                )
            }
            Self::SecurityHubOnly { field } => write!(
                f,
                "[web.identity] `{field}` only applies to mode = \"security_hub\""
            ),
            Self::MissingPrincipals => f.write_str(
                "[web.identity] mode = \"security_hub\" needs at least one uid_principals entry",
            ),
            Self::DuplicateUid { field, uid } => write!(
                f,
                "[web.identity] `{field}`: uid {uid} configured more than once (aliased key?)"
            ),
        }
    }
}

impl std::error::Error for IdentityConfigError {}

fn parse_uid(key: &str) -> Result<u32, IdentityConfigError> {
    key.trim()
        .parse::<u32>()
        .map_err(|_| IdentityConfigError::InvalidUid {
            key: key.to_owned(),
        })
}

impl WebIdentityConfig {
    /// Validiert `uid_tenants`.
    ///
    /// `parse_uid` normalisiert den Schlüssel (führende Nullen, `+`,
    /// umgebende Leerzeichen); mehrere Schlüssel, die auf dieselbe UID
    /// abbilden, würden ohne diese Prüfung von der TOML-Schlüsselreihenfolge
    /// abhängig kollidieren.
    ///
    /// # Errors
    /// [`IdentityConfigError::InvalidUid`] / [`IdentityConfigError::InvalidTenant`]
    /// / [`IdentityConfigError::DuplicateUid`].
    pub fn tenant_map(&self) -> Result<UidTenantMap, IdentityConfigError> {
        let mut seen = HashSet::with_capacity(self.uid_tenants.len());
        let mut entries = Vec::with_capacity(self.uid_tenants.len());
        for (key, tenant) in &self.uid_tenants {
            let uid = parse_uid(key)?;
            if !seen.insert(uid) {
                return Err(IdentityConfigError::DuplicateUid {
                    field: "uid_tenants",
                    uid,
                });
            }
            let tenant = TenantId::try_from_str(tenant.trim())
                .map_err(|_| IdentityConfigError::InvalidTenant { uid })?;
            entries.push((uid, tenant));
        }
        Ok(UidTenantMap::new(entries))
    }

    /// Validiert `uid_principals`.
    ///
    /// Dieselbe Normalisierungs-/Duplikatsprüfung wie [`Self::tenant_map`].
    ///
    /// # Errors
    /// [`IdentityConfigError::InvalidUid`] / [`IdentityConfigError::BlankPrincipal`]
    /// / [`IdentityConfigError::DuplicateUid`].
    pub fn principal_map(&self) -> Result<Vec<(u32, String)>, IdentityConfigError> {
        let mut seen = HashSet::with_capacity(self.uid_principals.len());
        let mut entries = Vec::with_capacity(self.uid_principals.len());
        for (key, principal) in &self.uid_principals {
            let uid = parse_uid(key)?;
            if !seen.insert(uid) {
                return Err(IdentityConfigError::DuplicateUid {
                    field: "uid_principals",
                    uid,
                });
            }
            let principal = principal.trim();
            if principal.is_empty() {
                return Err(IdentityConfigError::BlankPrincipal { uid });
            }
            entries.push((uid, principal.to_owned()));
        }
        Ok(entries)
    }

    /// Baut den Resolver für den konfigurierten Modus.
    ///
    /// # Arguments
    /// - `authorizer` (`Arc<dyn PeerAuthorizer>`): die bisherige UID→Tier-
    ///   Richtlinie (Tier-Deckel in beiden Modi).
    ///
    /// # Errors
    /// Siehe [`IdentityConfigError`].
    pub fn build_resolver(
        &self,
        authorizer: Arc<dyn PeerAuthorizer>,
    ) -> Result<Arc<dyn LocalPeerIdentityResolver>, IdentityConfigError> {
        self.build_resolver_with(authorizer, |socket| {
            Arc::new(SecurityHubClient::new(
                socket,
                ClientOptions {
                    timeout: HUB_VERIFY_TIMEOUT,
                    ..ClientOptions::default()
                },
            ))
        })
    }

    fn build_resolver_with(
        &self,
        authorizer: Arc<dyn PeerAuthorizer>,
        make_verifier: impl FnOnce(PathBuf) -> Arc<dyn ContextVerifier>,
    ) -> Result<Arc<dyn LocalPeerIdentityResolver>, IdentityConfigError> {
        let tenants = self.tenant_map()?;
        let principals = self.principal_map()?;
        let fallback = TierMapResolver::with_tenants(authorizer, tenants);
        match self.mode {
            IdentityMode::TierMap => {
                if self.require_context {
                    return Err(IdentityConfigError::SecurityHubOnly {
                        field: "require_context",
                    });
                }
                if self.security_socket.is_some() {
                    return Err(IdentityConfigError::SecurityHubOnly {
                        field: "security_socket",
                    });
                }
                if !principals.is_empty() {
                    return Err(IdentityConfigError::SecurityHubOnly {
                        field: "uid_principals",
                    });
                }
                Ok(Arc::new(fallback))
            }
            IdentityMode::SecurityHub => {
                if principals.is_empty() {
                    return Err(IdentityConfigError::MissingPrincipals);
                }
                let socket = self
                    .security_socket
                    .clone()
                    .unwrap_or_else(|| PathBuf::from(DEFAULT_SECURITY_SOCKET));
                Ok(Arc::new(SecurityHubResolver::new(
                    fallback,
                    make_verifier(socket),
                    principals,
                    self.require_context,
                )))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use harw_operations::operation::PermissionTier;
    use harw_types::{
        AuthStrength, Clock, PrincipalKind, SecurityContextId, SecurityContextSummary, TenantId,
        TrustZone,
    };
    use hyper::HeaderMap;
    use hyper::header::HeaderValue;
    use jiff::Timestamp;

    use super::{
        ContextVerifier, HubVerdict, IdentityConfigError, IdentityError, IdentityMode,
        IdentitySource, LocalPeerIdentityResolver, ResolvedPeer, SECURITY_CONTEXT_HEADER,
        SecurityHubResolver, TierMapResolver, UidTenantMap, VerifierError, VerifyFuture,
        WebIdentityConfig, presented_context,
    };
    use crate::authz::{PeerAuthorizer, StaticUidTierMap};
    use crate::peer::PeerCredentials;
    use crate::test_support::{TestError, TestResult, ctx};

    const ALICE: u32 = 1000;
    const BOB: u32 = 1001;
    const NOW: i64 = 1_700_000_000;

    fn peer(uid: u32) -> PeerCredentials {
        PeerCredentials::new(7, uid, uid)
    }

    fn at(second: i64) -> TestResult<Timestamp> {
        Timestamp::from_second(second).map_err(ctx("timestamp"))
    }

    struct FixedClock(Timestamp);
    impl Clock for FixedClock {
        fn now(&self) -> Timestamp {
            self.0
        }
    }

    /// Attrappe des Hubs: liefert eine feste Antwort und merkt sich die Ids.
    struct FakeHub {
        answer: Result<HubVerdict, VerifierError>,
        seen: Mutex<Vec<String>>,
    }

    impl FakeHub {
        fn new(answer: Result<HubVerdict, VerifierError>) -> Arc<Self> {
            Arc::new(Self {
                answer,
                seen: Mutex::new(Vec::new()),
            })
        }

        fn calls(&self) -> usize {
            self.seen
                .lock()
                .map(|seen| seen.len())
                .unwrap_or(usize::MAX)
        }
    }

    impl ContextVerifier for FakeHub {
        fn verify<'a>(&'a self, context_id: &'a str) -> VerifyFuture<'a> {
            if let Ok(mut seen) = self.seen.lock() {
                seen.push(context_id.to_owned());
            }
            let answer = self.answer.clone();
            Box::pin(async move { answer })
        }
    }

    fn summary(
        id: &str,
        principal: &str,
        tenant: Option<&str>,
        tier: PermissionTier,
    ) -> TestResult<SecurityContextSummary> {
        Ok(SecurityContextSummary {
            id: SecurityContextId::try_from_str(id).map_err(ctx("context id"))?,
            principal_id: principal.to_owned(),
            principal_kind: PrincipalKind::Human,
            permission_tier: tier,
            tenant: match tenant {
                Some(tenant) => Some(TenantId::try_from_str(tenant).map_err(ctx("tenant"))?),
                None => None,
            },
            workspace: None,
            node: None,
            device: None,
            service_identity: None,
            trust_zone: TrustZone::Local,
            auth_strength: AuthStrength::PeerCredential,
            issued_at: at(NOW - 60)?,
            expires_at: at(NOW + 300)?,
            issuer: "harw-security-hub".to_owned(),
        })
    }

    fn tier_table() -> Arc<dyn PeerAuthorizer> {
        Arc::new(StaticUidTierMap::new(vec![
            (ALICE, PermissionTier::Owner),
            (BOB, PermissionTier::Operator),
        ]))
    }

    fn tenants() -> TestResult<UidTenantMap> {
        Ok(UidTenantMap::new(vec![
            (
                ALICE,
                TenantId::try_from_str("tenant-a").map_err(ctx("tenant"))?,
            ),
            (
                BOB,
                TenantId::try_from_str("tenant-b").map_err(ctx("tenant"))?,
            ),
        ]))
    }

    fn hub_resolver(hub: Arc<FakeHub>, require_context: bool) -> TestResult<SecurityHubResolver> {
        Ok(SecurityHubResolver::new(
            TierMapResolver::with_tenants(tier_table(), tenants()?),
            hub,
            vec![(ALICE, "alice".to_owned()), (BOB, "bob".to_owned())],
            require_context,
        )
        .with_clock(Arc::new(FixedClock(at(NOW)?))))
    }

    fn expect_err(
        result: Result<ResolvedPeer, IdentityError>,
        expected: IdentityError,
    ) -> TestResult {
        match result {
            Err(error) if error == expected => Ok(()),
            other => Err(TestError::Unexpected(format!(
                "expected Err({expected:?}), got {other:?}"
            ))),
        }
    }

    fn parse(toml_text: &str) -> TestResult<WebIdentityConfig> {
        toml::from_str(toml_text).map_err(ctx("parse [web.identity]"))
    }

    // ── tier_map: bisheriges Verhalten ────────────────────────────────────────

    #[tokio::test]
    async fn test_tier_map_resolver_matches_peer_authorizer_exactly() -> TestResult {
        let authorizer: Arc<dyn PeerAuthorizer> = Arc::new(StaticUidTierMap::with_default(
            vec![(ALICE, PermissionTier::Maintainer)],
            PermissionTier::Observer,
        ));
        let resolver = TierMapResolver::new(Arc::clone(&authorizer));
        for uid in [ALICE, BOB, 0, 4242] {
            let resolved = resolver
                .resolve(&peer(uid), None)
                .await
                .map_err(ctx("resolve"))?;
            assert_eq!(Some(resolved.tier()), authorizer.tier_for(&peer(uid)));
            assert_eq!(resolved.source(), IdentitySource::TierMap);
            assert_eq!(resolved.tenant(), None);
            assert_eq!(resolved.summary(), None);
            assert_eq!(resolved.principal_id(), format!("uid:{uid}"));
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_tier_map_resolver_unknown_peer_stays_unknown() -> TestResult {
        let resolver = TierMapResolver::new(tier_table());
        expect_err(
            resolver.resolve(&peer(4242), None).await,
            IdentityError::UnknownPeer,
        )
    }

    #[tokio::test]
    async fn test_tier_map_resolver_ignores_presented_context_header() -> TestResult {
        let resolver = TierMapResolver::new(tier_table());
        let resolved = resolver
            .resolve(&peer(BOB), Some("ctx-forged"))
            .await
            .map_err(ctx("resolve"))?;
        assert_eq!(resolved.tier(), PermissionTier::Operator);
        assert_eq!(resolved.source(), IdentitySource::TierMap);
        assert_eq!(resolved.summary(), None);
        Ok(())
    }

    #[tokio::test]
    async fn test_default_config_is_tier_map_with_unchanged_behaviour() -> TestResult {
        let config = parse("")?;
        assert_eq!(config, WebIdentityConfig::default());
        assert_eq!(config.mode, IdentityMode::TierMap);
        let resolver = config
            .build_resolver(tier_table())
            .map_err(ctx("build resolver"))?;
        let alice = resolver
            .resolve(&peer(ALICE), None)
            .await
            .map_err(ctx("resolve"))?;
        assert_eq!(alice.tier(), PermissionTier::Owner);
        assert_eq!(alice.tenant(), None);
        expect_err(
            resolver.resolve(&peer(4242), None).await,
            IdentityError::UnknownPeer,
        )
    }

    #[tokio::test]
    async fn test_two_uids_map_to_different_tenants() -> TestResult {
        let config = parse(
            r#"
            mode = "tier_map"
            uid_tenants = { "1000" = "tenant-a", "1001" = "tenant-b" }
            "#,
        )?;
        let resolver = config
            .build_resolver(tier_table())
            .map_err(ctx("build resolver"))?;
        let alice = resolver
            .resolve(&peer(ALICE), None)
            .await
            .map_err(ctx("resolve"))?;
        let bob = resolver
            .resolve(&peer(BOB), None)
            .await
            .map_err(ctx("resolve"))?;
        assert_eq!(alice.tenant().map(TenantId::as_str), Some("tenant-a"));
        assert_eq!(bob.tenant().map(TenantId::as_str), Some("tenant-b"));
        assert_ne!(alice.tenant(), bob.tenant());
        // Tier kommt unverändert aus der Tier-Tabelle.
        assert_eq!(alice.tier(), PermissionTier::Owner);
        assert_eq!(bob.tier(), PermissionTier::Operator);
        Ok(())
    }

    // ── security_hub ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_security_hub_valid_context_binds_principal_and_tenant() -> TestResult {
        let hub = FakeHub::new(Ok(HubVerdict::Active(summary(
            "ctx-1",
            "alice",
            Some("tenant-a"),
            PermissionTier::Owner,
        )?)));
        let resolver = hub_resolver(Arc::clone(&hub), true)?;
        let resolved = resolver
            .resolve(&peer(ALICE), Some("ctx-1"))
            .await
            .map_err(ctx("resolve"))?;
        assert_eq!(resolved.source(), IdentitySource::SecurityHub);
        assert_eq!(resolved.principal_id(), "alice");
        assert_eq!(resolved.tenant().map(TenantId::as_str), Some("tenant-a"));
        assert_eq!(resolved.tier(), PermissionTier::Owner);
        assert_eq!(
            resolved.summary().map(|summary| summary.id.as_str()),
            Some("ctx-1")
        );
        assert_eq!(hub.calls(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn test_security_hub_missing_header_falls_back_without_require_context() -> TestResult {
        let hub = FakeHub::new(Err(VerifierError::Failed));
        let resolver = hub_resolver(Arc::clone(&hub), false)?;
        let resolved = resolver
            .resolve(&peer(BOB), None)
            .await
            .map_err(ctx("resolve"))?;
        assert_eq!(resolved.source(), IdentitySource::TierMap);
        assert_eq!(resolved.tier(), PermissionTier::Operator);
        assert_eq!(resolved.tenant().map(TenantId::as_str), Some("tenant-b"));
        assert_eq!(hub.calls(), 0, "no header, no hub call");
        Ok(())
    }

    #[tokio::test]
    async fn test_security_hub_missing_header_denied_with_require_context() -> TestResult {
        let hub = FakeHub::new(Err(VerifierError::Failed));
        let resolver = hub_resolver(Arc::clone(&hub), true)?;
        expect_err(
            resolver.resolve(&peer(BOB), None).await,
            IdentityError::ContextRequired,
        )?;
        assert_eq!(hub.calls(), 0);
        Ok(())
    }

    /// Ein Peer, dessen UID an einen Hub-Principal gebunden ist, aber ohne
    /// gepinnten Mandanten: ohne Header darf `require_context = false` NICHT
    /// stillschweigend mandantenlos (= alle Mandanten) durchlassen.
    #[tokio::test]
    async fn test_security_hub_missing_header_without_tenant_pin_stays_scoped() -> TestResult {
        let unpinned = SecurityHubResolver::new(
            TierMapResolver::new(tier_table()),
            FakeHub::new(Err(VerifierError::Failed)),
            vec![(ALICE, "alice".to_owned())],
            false,
        );
        expect_err(
            unpinned.resolve(&peer(ALICE), None).await,
            IdentityError::ContextRequired,
        )?;

        // Mit gepinntem Mandanten bleibt der Rückfall erlaubt und scoped.
        let pinned = SecurityHubResolver::new(
            TierMapResolver::with_tenants(
                tier_table(),
                UidTenantMap::new(vec![(
                    ALICE,
                    TenantId::try_from_str("tenant-a").map_err(ctx("tenant"))?,
                )]),
            ),
            FakeHub::new(Err(VerifierError::Failed)),
            vec![(ALICE, "alice".to_owned())],
            false,
        );
        let resolved = pinned
            .resolve(&peer(ALICE), None)
            .await
            .map_err(ctx("resolve"))?;
        assert_eq!(resolved.source(), IdentitySource::TierMap);
        assert_eq!(resolved.tenant().map(TenantId::as_str), Some("tenant-a"));

        // Eine UID ohne `uid_principals`-Eintrag ist von der neuen Sperre
        // unberührt — sie kann ohnehin nie einen Kontext vorlegen.
        let unbound = SecurityHubResolver::new(
            TierMapResolver::new(tier_table()),
            FakeHub::new(Err(VerifierError::Failed)),
            vec![(ALICE, "alice".to_owned())],
            false,
        );
        let bob = unbound
            .resolve(&peer(BOB), None)
            .await
            .map_err(ctx("resolve"))?;
        assert_eq!(bob.tenant(), None);
        Ok(())
    }

    #[tokio::test]
    async fn test_security_hub_context_of_other_principal_is_denied() -> TestResult {
        // Bob legt Alices (gültigen) Kontext vor.
        let hub = FakeHub::new(Ok(HubVerdict::Active(summary(
            "ctx-alice",
            "alice",
            Some("tenant-a"),
            PermissionTier::Owner,
        )?)));
        for require_context in [true, false] {
            let resolver = hub_resolver(Arc::clone(&hub), require_context)?;
            expect_err(
                resolver.resolve(&peer(BOB), Some("ctx-alice")).await,
                IdentityError::PrincipalMismatch,
            )?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_security_hub_expired_or_revoked_context_is_denied() -> TestResult {
        let hub = FakeHub::new(Ok(HubVerdict::Gone));
        for require_context in [true, false] {
            let resolver = hub_resolver(Arc::clone(&hub), require_context)?;
            expect_err(
                resolver.resolve(&peer(ALICE), Some("ctx-old")).await,
                IdentityError::ContextGone,
            )?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_security_hub_unknown_context_is_denied() -> TestResult {
        let hub = FakeHub::new(Ok(HubVerdict::Unknown));
        let resolver = hub_resolver(hub, false)?;
        expect_err(
            resolver.resolve(&peer(ALICE), Some("ctx-nope")).await,
            IdentityError::ContextUnknown,
        )
    }

    #[tokio::test]
    async fn test_security_hub_locally_expired_summary_is_denied() -> TestResult {
        let mut stale = summary("ctx-1", "alice", None, PermissionTier::Owner)?;
        stale.expires_at = at(NOW)?;
        let resolver = hub_resolver(FakeHub::new(Ok(HubVerdict::Active(stale))), false)?;
        expect_err(
            resolver.resolve(&peer(ALICE), Some("ctx-1")).await,
            IdentityError::ContextGone,
        )
    }

    #[tokio::test]
    async fn test_security_hub_answer_for_other_id_is_denied() -> TestResult {
        let other = summary("ctx-other", "alice", None, PermissionTier::Owner)?;
        let resolver = hub_resolver(FakeHub::new(Ok(HubVerdict::Active(other))), false)?;
        expect_err(
            resolver.resolve(&peer(ALICE), Some("ctx-1")).await,
            IdentityError::ContextUnknown,
        )
    }

    #[tokio::test]
    async fn test_security_hub_unavailable_denies_with_require_context() -> TestResult {
        let resolver = hub_resolver(FakeHub::new(Err(VerifierError::Unavailable)), true)?;
        expect_err(
            resolver.resolve(&peer(ALICE), Some("ctx-1")).await,
            IdentityError::HubUnavailable,
        )
    }

    #[tokio::test]
    async fn test_security_hub_unavailable_falls_back_to_tier_map() -> TestResult {
        let resolver = hub_resolver(FakeHub::new(Err(VerifierError::Unavailable)), false)?;
        let resolved = resolver
            .resolve(&peer(ALICE), Some("ctx-1"))
            .await
            .map_err(ctx("resolve"))?;
        assert_eq!(resolved.source(), IdentitySource::TierMap);
        assert_eq!(resolved.tier(), PermissionTier::Owner);
        assert_eq!(resolved.tenant().map(TenantId::as_str), Some("tenant-a"));
        assert_eq!(resolved.summary(), None);
        Ok(())
    }

    /// Zweiter Auslöser von `HubUnavailable`: auch bei `require_context =
    /// false` bleibt der Rückfall für eine an einen Principal gebundene UID
    /// ohne `uid_tenants`-Pin gesperrt.
    #[tokio::test]
    async fn test_security_hub_unavailable_without_tenant_pin_is_denied() -> TestResult {
        let hub = FakeHub::new(Err(VerifierError::Unavailable));
        let unpinned = SecurityHubResolver::new(
            TierMapResolver::new(tier_table()),
            Arc::clone(&hub) as Arc<dyn ContextVerifier>,
            vec![(ALICE, "alice".to_owned())],
            false,
        );
        expect_err(
            unpinned.resolve(&peer(ALICE), Some("ctx-1")).await,
            IdentityError::HubUnavailable,
        )?;
        assert_eq!(hub.calls(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn test_security_hub_failure_is_never_a_fallback() -> TestResult {
        let resolver = hub_resolver(FakeHub::new(Err(VerifierError::Failed)), false)?;
        expect_err(
            resolver.resolve(&peer(ALICE), Some("ctx-1")).await,
            IdentityError::HubFailure,
        )
    }

    #[tokio::test]
    async fn test_security_hub_malformed_reference_is_denied_without_hub_call() -> TestResult {
        let hub = FakeHub::new(Err(VerifierError::Unavailable));
        let resolver = hub_resolver(Arc::clone(&hub), false)?;
        let too_long = "x".repeat(129);
        for bad in ["../v1/health", "a/b", "ctx 1", too_long.as_str()] {
            expect_err(
                resolver.resolve(&peer(ALICE), Some(bad)).await,
                IdentityError::InvalidContextReference,
            )?;
        }
        assert_eq!(hub.calls(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn test_security_hub_unknown_peer_denied_even_with_valid_context() -> TestResult {
        let hub = FakeHub::new(Ok(HubVerdict::Active(summary(
            "ctx-1",
            "alice",
            None,
            PermissionTier::Owner,
        )?)));
        let resolver = hub_resolver(Arc::clone(&hub), false)?;
        expect_err(
            resolver.resolve(&peer(4242), Some("ctx-1")).await,
            IdentityError::UnknownPeer,
        )?;
        assert_eq!(hub.calls(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn test_security_hub_context_only_narrows_tier() -> TestResult {
        // Kontext sagt Owner, Tabelle sagt Operator → Operator (keine Anhebung).
        let high = summary("ctx-1", "bob", None, PermissionTier::Owner)?;
        let resolver = hub_resolver(FakeHub::new(Ok(HubVerdict::Active(high))), true)?;
        let bob = resolver
            .resolve(&peer(BOB), Some("ctx-1"))
            .await
            .map_err(ctx("resolve"))?;
        assert_eq!(bob.tier(), PermissionTier::Operator);

        // Kontext sagt Observer, Tabelle sagt Owner → Observer (Verengung).
        let low = summary("ctx-2", "alice", None, PermissionTier::Observer)?;
        let resolver = hub_resolver(FakeHub::new(Ok(HubVerdict::Active(low))), true)?;
        let alice = resolver
            .resolve(&peer(ALICE), Some("ctx-2"))
            .await
            .map_err(ctx("resolve"))?;
        assert_eq!(alice.tier(), PermissionTier::Observer);
        // Kein Mandant im Kontext → der konfigurierte.
        assert_eq!(alice.tenant().map(TenantId::as_str), Some("tenant-a"));
        Ok(())
    }

    #[tokio::test]
    async fn test_security_hub_tenant_conflict_with_configuration_is_denied() -> TestResult {
        let foreign = summary("ctx-1", "alice", Some("tenant-b"), PermissionTier::Owner)?;
        let resolver = hub_resolver(FakeHub::new(Ok(HubVerdict::Active(foreign))), true)?;
        expect_err(
            resolver.resolve(&peer(ALICE), Some("ctx-1")).await,
            IdentityError::TenantMismatch,
        )
    }

    #[tokio::test]
    async fn test_security_hub_remote_zone_context_is_not_permitted() -> TestResult {
        let mut remote = summary("ctx-1", "alice", None, PermissionTier::Owner)?;
        remote.trust_zone = TrustZone::Remote;
        let resolver = hub_resolver(FakeHub::new(Ok(HubVerdict::Active(remote))), true)?;
        expect_err(
            resolver.resolve(&peer(ALICE), Some("ctx-1")).await,
            IdentityError::ContextNotPermitted,
        )
    }

    #[tokio::test]
    async fn test_security_hub_peer_without_principal_binding_is_denied() -> TestResult {
        let resolver = SecurityHubResolver::new(
            TierMapResolver::new(tier_table()),
            FakeHub::new(Ok(HubVerdict::Unknown)),
            vec![(ALICE, "alice".to_owned())],
            false,
        );
        expect_err(
            resolver.resolve(&peer(BOB), Some("ctx-1")).await,
            IdentityError::NoPrincipalBinding,
        )
    }

    /// Echter `SecurityHubClient` gegen einen nicht existierenden Socket:
    /// belegt die Abbildung `InfraClientError::Unavailable` → Rückfall bzw.
    /// Ablehnung über den produktiven Konfigurationspfad.
    #[tokio::test]
    async fn test_config_built_security_hub_resolver_handles_unreachable_hub() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let socket = dir.path().join("absent-security.sock");
        for (require_context, expect_fallback) in [(false, true), (true, false)] {
            let config = WebIdentityConfig {
                mode: IdentityMode::SecurityHub,
                security_socket: Some(socket.clone()),
                require_context,
                uid_tenants: [("1000".to_owned(), "tenant-a".to_owned())].into(),
                uid_principals: [("1000".to_owned(), "alice".to_owned())].into(),
            };
            let resolver = config
                .build_resolver(tier_table())
                .map_err(ctx("build resolver"))?;
            let result = resolver.resolve(&peer(ALICE), Some("ctx-1")).await;
            if expect_fallback {
                let resolved = result.map_err(ctx("resolve"))?;
                assert_eq!(resolved.source(), IdentitySource::TierMap);
                assert_eq!(resolved.tenant().map(TenantId::as_str), Some("tenant-a"));
            } else {
                expect_err(result, IdentityError::HubUnavailable)?;
            }
        }
        Ok(())
    }

    // ── Header ────────────────────────────────────────────────────────────────

    #[test]
    fn test_presented_context_header_parsing() -> TestResult {
        let mut headers = HeaderMap::new();
        assert_eq!(presented_context(&headers), Ok(None));

        headers.insert(SECURITY_CONTEXT_HEADER, HeaderValue::from_static(" ctx-1 "));
        assert_eq!(presented_context(&headers), Ok(Some("ctx-1")));

        headers.append(SECURITY_CONTEXT_HEADER, HeaderValue::from_static("ctx-2"));
        assert_eq!(
            presented_context(&headers),
            Err(IdentityError::InvalidContextReference)
        );

        let mut empty = HeaderMap::new();
        empty.insert(SECURITY_CONTEXT_HEADER, HeaderValue::from_static("   "));
        assert_eq!(
            presented_context(&empty),
            Err(IdentityError::InvalidContextReference)
        );

        let mut opaque = HeaderMap::new();
        let value = HeaderValue::from_bytes(b"ctx-\xff").map_err(ctx("header value"))?;
        opaque.insert(SECURITY_CONTEXT_HEADER, value);
        assert_eq!(
            presented_context(&opaque),
            Err(IdentityError::InvalidContextReference)
        );
        Ok(())
    }

    #[test]
    fn test_identity_error_codes_are_distinct() {
        let all = [
            IdentityError::UnknownPeer,
            IdentityError::ContextRequired,
            IdentityError::InvalidContextReference,
            IdentityError::NoPrincipalBinding,
            IdentityError::ContextUnknown,
            IdentityError::ContextGone,
            IdentityError::PrincipalMismatch,
            IdentityError::TenantMismatch,
            IdentityError::ContextNotPermitted,
            IdentityError::HubUnavailable,
            IdentityError::HubFailure,
        ];
        let codes: std::collections::BTreeSet<&str> = all.iter().map(|e| e.code()).collect();
        assert_eq!(codes.len(), all.len());
        // Derselbe Grund wie `ForbiddenReason::UnknownPeer` im Router.
        assert_eq!(IdentityError::UnknownPeer.code(), "unknown_peer");
    }

    // ── Konfiguration ─────────────────────────────────────────────────────────

    #[test]
    fn test_config_parses_full_security_hub_table() -> TestResult {
        let config = parse(
            r#"
            mode = "security_hub"
            security_socket = "/run/harw/infra/security.sock"
            require_context = true
            uid_tenants = { "1000" = "tenant-a" }
            uid_principals = { "1000" = "alice" }
            "#,
        )?;
        assert_eq!(config.mode, IdentityMode::SecurityHub);
        assert!(config.require_context);
        assert_eq!(
            config.principal_map(),
            Ok(vec![(ALICE, "alice".to_owned())])
        );
        let tenants = config.tenant_map().map_err(ctx("tenants"))?;
        assert_eq!(tenants.get(ALICE).map(TenantId::as_str), Some("tenant-a"));
        Ok(())
    }

    #[test]
    fn test_config_rejects_unknown_fields() {
        let unknown: Result<WebIdentityConfig, _> =
            toml::from_str("uid_tiers = { \"1000\" = \"owner\" }");
        assert!(unknown.is_err());
        let bad_mode: Result<WebIdentityConfig, _> = toml::from_str("mode = \"bearer\"");
        assert!(bad_mode.is_err());
    }

    #[test]
    fn test_config_rejects_invalid_entries_and_misplaced_hub_options() -> TestResult {
        let bad_uid = parse(r#"uid_tenants = { "alice" = "tenant-a" }"#)?;
        assert_eq!(
            bad_uid.build_resolver(tier_table()).err(),
            Some(IdentityConfigError::InvalidUid {
                key: "alice".to_owned()
            })
        );
        let blank_tenant = parse(r#"uid_tenants = { "1000" = " " }"#)?;
        assert_eq!(
            blank_tenant.build_resolver(tier_table()).err(),
            Some(IdentityConfigError::InvalidTenant { uid: ALICE })
        );
        let require_in_tier_map = parse("require_context = true")?;
        assert_eq!(
            require_in_tier_map.build_resolver(tier_table()).err(),
            Some(IdentityConfigError::SecurityHubOnly {
                field: "require_context"
            })
        );
        let hub_without_principals = parse(r#"mode = "security_hub""#)?;
        assert_eq!(
            hub_without_principals.build_resolver(tier_table()).err(),
            Some(IdentityConfigError::MissingPrincipals)
        );
        let blank_principal = parse(
            r#"
            mode = "security_hub"
            uid_principals = { "1000" = "" }
            "#,
        )?;
        assert_eq!(
            blank_principal.build_resolver(tier_table()).err(),
            Some(IdentityConfigError::BlankPrincipal { uid: ALICE })
        );
        Ok(())
    }

    /// Zwei nach `parse_uid` gleichwertige Schlüssel (führende Null, `+`,
    /// umgebendes Leerzeichen) dürfen nicht stillschweigend nach
    /// TOML-Schlüsselreihenfolge kollidieren.
    #[test]
    fn test_config_rejects_aliased_duplicate_uid_keys() -> TestResult {
        let dup_tenants = parse(r#"uid_tenants = { "1000" = "tenant-a", "01000" = "tenant-b" }"#)?;
        assert_eq!(
            dup_tenants.build_resolver(tier_table()).err(),
            Some(IdentityConfigError::DuplicateUid {
                field: "uid_tenants",
                uid: ALICE
            })
        );

        let dup_principals = parse(
            r#"
            mode = "security_hub"
            uid_principals = { "1000" = "alice", "+1000" = "alice2" }
            "#,
        )?;
        assert_eq!(
            dup_principals.build_resolver(tier_table()).err(),
            Some(IdentityConfigError::DuplicateUid {
                field: "uid_principals",
                uid: ALICE
            })
        );

        let dup_whitespace =
            parse(r#"uid_tenants = { "1000" = "tenant-a", " 1000" = "tenant-b" }"#)?;
        assert_eq!(
            dup_whitespace.build_resolver(tier_table()).err(),
            Some(IdentityConfigError::DuplicateUid {
                field: "uid_tenants",
                uid: ALICE
            })
        );
        Ok(())
    }
}
