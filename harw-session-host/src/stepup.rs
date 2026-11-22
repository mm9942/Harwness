//! Step-up for remote global writes (Welle 3 Scheibe 3, C9a; Vertrag K-W3b §5).
//!
//! A remote write that changes global state (roles, default model, providers,
//! `config.set`, `secret.put`) is never applied directly. The caller's
//! domain module builds a canonical text ("was aendert sich", with the full
//! URL and key name), parks its plan here with [`StepUpRegistry::request`]
//! and sends the returned [`StepUpChallenge`] to the phone (frame
//! `stepup.request`, wired in C9d). The phone shows the text, asks for a fresh
//! biometric confirmation, recomputes [`stepup_hash`] itself and answers with
//! `stepup.confirm {stepup_id, hash}`. Only [`StepUpRegistry::confirm`] hands
//! the parked payload back, exactly once.
//!
//! Rules enforced here (contract R5, R8):
//! - `control` AND `approve` are both required, at request and again at
//!   confirm; agent principals never request a step-up (a step-up is a human
//!   second factor).
//! - One-time nonce, 60 s lifetime, domain-separated SHA-256 over nonce and
//!   text. A wrong hash burns the step-up; an expired one is removed.
//! - A step-up belongs to tenant + device + connection. A foreign caller gets
//!   `NotFound` (same answer as unknown/consumed, no existence leak) and can
//!   neither confirm nor burn it.
//! - Limits: pending per caller, pending total, requests per caller per
//!   sliding hour.
//! - Audit for every outcome, never with text or nonce (only the text hash).
//!
//! Generic over the parked payload `P`; this crate does not depend on
//! `harw-ops` (Scheibe 3 parks `harw_ops::provider::ProviderPlan`).
//!
//! The nonce is not retained after the challenge is built: the registry only
//! keeps the expected hash. All time is passed in explicitly (`now`), so the
//! registry is deterministic in tests.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use harw_types::{DeviceId, TenantId};
use jiff::{SignedDuration, Timestamp};
use sha2::{Digest as _, Sha256};

use crate::error::HostError;
use crate::identity::{ClientIdentity, ConnectionId, Need};

/// Gueltigkeit eines Step-ups.
pub const STEPUP_TTL_SECS: i64 = 60;
/// Domaenen-Praefix der Hash-Eingabe.
pub const STEPUP_HASH_DOMAIN: &[u8] = b"harw-stepup-v1\0";
/// Hoechstens so viele offene Step-ups je Besitzer.
pub const MAX_PENDING_PER_OWNER: usize = 4;
/// Hoechstens so viele offene Step-ups insgesamt.
pub const MAX_PENDING_TOTAL: usize = 256;
/// Rate-Limit: angefragte Step-ups je Besitzer je gleitender Stunde.
pub const MAX_REQUESTS_PER_OWNER_PER_HOUR: usize = 30;
/// Hoechstlaenge des kanonischen Texts in Bytes.
pub const MAX_TEXT_BYTES: usize = 4096;
/// Laenge von Id und Nonce in Bytes (hex doppelt so lang).
pub const STEPUP_ID_BYTES: usize = 16;
pub const STEPUP_NONCE_BYTES: usize = 32;

/// Hoechstlaenge von `action` in Bytes (stabile Kennung wie `provider.add`).
const MAX_ACTION_BYTES: usize = 64;
/// Hoechstlaenge von `target` in Bytes (z. B. Provider-Name).
const MAX_TARGET_BYTES: usize = 256;
/// Versuche, eine freie Id zu ziehen, bevor `Entropy` gemeldet wird.
const ID_ATTEMPTS: usize = 4;

/// An wen ein Step-up gebunden ist. Nur aus `ClientIdentity` gebaut, nie vom Draht.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StepUpOwner {
    pub tenant: Option<TenantId>,
    pub device: Option<DeviceId>,
    pub connection: ConnectionId,
}

impl StepUpOwner {
    #[must_use]
    pub fn from_identity(identity: &ClientIdentity) -> Self {
        Self {
            tenant: identity.tenant.clone(),
            device: identity.device.clone(),
            connection: identity.connection,
        }
    }

    /// Key for the limits (pending per owner, requests per hour). An enrolled
    /// device is counted across all its connections, so reconnecting does not
    /// reset the limits; a caller without device is counted per connection.
    fn limit_key(&self) -> LimitKey {
        match &self.device {
            Some(device) => LimitKey::Device(self.tenant.clone(), device.clone()),
            None => LimitKey::Connection(self.tenant.clone(), self.connection),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum LimitKey {
    Device(Option<TenantId>, DeviceId),
    Connection(Option<TenantId>, ConnectionId),
}

/// Anfrage des Aufrufers.
pub struct StepUpRequest<P> {
    /// Stabile Aktion, z. B. `"provider.add"`, `"config.set"`, `"secret.put"`.
    pub action: String,
    /// Ziel, z. B. Provider-Name (keine Geheimnisse).
    pub target: String,
    /// Kanonischer Text (vom Fachmodul gebaut, z. B. 2.5).
    pub text: String,
    pub payload: P,
}

/// Was an das Handy geht (Frame `stepup.request`, C9d). Enthaelt nichts Geheimes.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct StepUpChallenge {
    /// 32 Hex-Zeichen, klein.
    pub stepup_id: String,
    pub action: String,
    pub target: String,
    pub text: String,
    /// 64 Hex-Zeichen, klein.
    pub nonce: String,
    /// `hex(sha256(STEPUP_HASH_DOMAIN || nonce_bytes || 0x00 || text_utf8))`, 64 Hex, klein.
    pub hash: String,
    pub expires_at: Timestamp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StepUpOutcome {
    Requested,
    Confirmed,
    HashMismatch,
    Expired,
    Cancelled,
    Refused,
}

/// Audit-Eintrag: nie Text-Werte mit Geheimnissen (Text enthaelt keine), nie Nonce.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct StepUpAuditEvent {
    pub at: Timestamp,
    pub stepup_id: String,
    pub action: String,
    pub target: String,
    /// `crate::identity::actor_label(&identity.actor)`.
    pub actor: String,
    pub outcome: StepUpOutcome,
    /// Hash des Texts (wie `StepUpChallenge::hash`).
    pub text_hash: String,
}

pub trait StepUpAudit: Send + Sync {
    fn record(&self, event: &StepUpAuditEvent);
}

/// Standard: `tracing::info!(target: "harw::audit::stepup", ...)` mit allen Feldern ausser Text.
pub struct TracingStepUpAudit;

impl StepUpAudit for TracingStepUpAudit {
    fn record(&self, event: &StepUpAuditEvent) {
        tracing::info!(
            target: "harw::audit::stepup",
            at = %event.at,
            stepup_id = %event.stepup_id,
            action = %event.action,
            stepup_target = %event.target,
            actor = %event.actor,
            outcome = ?event.outcome,
            text_hash = %event.text_hash,
            "step-up"
        );
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StepUpError {
    /// Fehlende Caps (`control` UND `approve` noetig) oder ungueltige Identitaet.
    Denied(&'static str),
    /// Unbekannt, schon verbraucht, oder fremder Besitzer (gleiche Antwort, kein Existenz-Leck).
    NotFound,
    Expired,
    HashMismatch,
    TooManyPending,
    RateLimited,
    TextTooLarge,
    /// Text leer oder mit Steuerzeichen ausser `\n`.
    InvalidText,
    /// Zufallsquelle nicht verfuegbar.
    Entropy,
}

impl fmt::Display for StepUpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Denied(reason) => write!(f, "Step-up verweigert: {reason}"),
            Self::NotFound => f.write_str("Step-up unbekannt oder schon verbraucht"),
            Self::Expired => f.write_str("Step-up abgelaufen"),
            Self::HashMismatch => f.write_str("Step-up-Bestaetigung passt nicht zum Text"),
            Self::TooManyPending => f.write_str("zu viele offene Step-ups"),
            Self::RateLimited => f.write_str("zu viele Step-ups in der letzten Stunde"),
            Self::TextTooLarge => f.write_str("Step-up-Text zu lang"),
            Self::InvalidText => f.write_str("Step-up-Text ungueltig"),
            Self::Entropy => f.write_str("Zufallsquelle nicht verfuegbar"),
        }
    }
}

impl std::error::Error for StepUpError {}

impl From<StepUpError> for HostError {
    fn from(error: StepUpError) -> Self {
        match error {
            StepUpError::Denied(_) => Self::Denied(error.to_string()),
            // Deliberately one message: the caller learns nothing about which
            // check failed.
            StepUpError::NotFound | StepUpError::Expired | StepUpError::HashMismatch => {
                Self::Denied("step-up ungueltig".into())
            }
            StepUpError::TooManyPending | StepUpError::RateLimited => {
                Self::Denied("zu viele Step-ups".into())
            }
            StepUpError::TextTooLarge | StepUpError::InvalidText => {
                Self::Protocol(error.to_string())
            }
            StepUpError::Entropy => Self::Storage(error.to_string()),
        }
    }
}

/// One parked step-up. The nonce is not kept, only the expected hash.
struct Entry<P> {
    owner: StepUpOwner,
    action: String,
    target: String,
    actor: String,
    text_hash: String,
    expires_at: Timestamp,
    payload: P,
}

impl<P> Entry<P> {
    fn event(&self, stepup_id: &str, at: Timestamp, outcome: StepUpOutcome) -> StepUpAuditEvent {
        StepUpAuditEvent {
            at,
            stepup_id: stepup_id.to_owned(),
            action: self.action.clone(),
            target: self.target.clone(),
            actor: self.actor.clone(),
            outcome,
            text_hash: self.text_hash.clone(),
        }
    }
}

struct State<P> {
    pending: HashMap<String, Entry<P>>,
    /// Accepted request times per limit key, oldest first, pruned to one hour.
    requests: HashMap<LimitKey, VecDeque<Timestamp>>,
}

impl<P> State<P> {
    /// Removes every expired entry; returns the audit events to emit.
    fn sweep(&mut self, now: Timestamp) -> Vec<StepUpAuditEvent> {
        let expired: Vec<String> = self
            .pending
            .iter()
            .filter(|(_, entry)| now >= entry.expires_at)
            .map(|(id, _)| id.clone())
            .collect();
        let mut events = Vec::with_capacity(expired.len());
        for id in expired {
            if let Some(entry) = self.pending.remove(&id) {
                events.push(entry.event(&id, now, StepUpOutcome::Expired));
            }
        }
        self.prune_requests(now);
        events
    }

    /// Drops request times older than one hour and empty keys.
    fn prune_requests(&mut self, now: Timestamp) {
        let Ok(window_start) = now.checked_sub(SignedDuration::from_hours(1)) else {
            return;
        };
        self.requests.retain(|_, times| {
            while times.front().is_some_and(|at| *at <= window_start) {
                times.pop_front();
            }
            !times.is_empty()
        });
    }
}

/// Registry of parked step-ups. Thread safe; never panics on a poisoned lock
/// (answers `Denied("interner Zustand")` instead).
pub struct StepUpRegistry<P> {
    state: Mutex<State<P>>,
    audit: Arc<dyn StepUpAudit>,
}

const POISONED: StepUpError = StepUpError::Denied("interner Zustand");

impl<P: Send> StepUpRegistry<P> {
    #[must_use]
    pub fn new(audit: Arc<dyn StepUpAudit>) -> Self {
        Self {
            state: Mutex::new(State {
                pending: HashMap::new(),
                requests: HashMap::new(),
            }),
            audit,
        }
    }

    /// Prueft `identity.validate()`, `Need::Control` und `Need::Approve` (beide Pflicht), Text, Limits;
    /// raeumt Abgelaufene (`sweep`) zuerst; erzeugt Id+Nonce per `getrandom::fill`; Audit `Requested`.
    pub fn request(
        &self,
        identity: &ClientIdentity,
        request: StepUpRequest<P>,
        now: Timestamp,
    ) -> Result<StepUpChallenge, StepUpError> {
        check_identity(identity)?;
        validate_action(&request.action)?;
        validate_target(&request.target)?;
        validate_text(&request.text)?;
        let expires_at = now
            .checked_add(SignedDuration::from_secs(STEPUP_TTL_SECS))
            .map_err(|_| StepUpError::Denied("Zeitpunkt ausserhalb des Bereichs"))?;

        let owner = StepUpOwner::from_identity(identity);
        let key = owner.limit_key();

        let mut events = Vec::new();
        let result = (|| -> Result<StepUpChallenge, StepUpError> {
            let mut state = self.state.lock().map_err(|_| POISONED)?;
            events.extend(state.sweep(now));

            let recent = state.requests.get(&key).map_or(0, VecDeque::len);
            if recent >= MAX_REQUESTS_PER_OWNER_PER_HOUR {
                return Err(StepUpError::RateLimited);
            }
            let own_pending = state
                .pending
                .values()
                .filter(|entry| entry.owner.limit_key() == key)
                .count();
            if own_pending >= MAX_PENDING_PER_OWNER || state.pending.len() >= MAX_PENDING_TOTAL {
                return Err(StepUpError::TooManyPending);
            }

            let stepup_id = fresh_id(|id| state.pending.contains_key(id))?;
            let mut nonce = [0u8; STEPUP_NONCE_BYTES];
            getrandom::fill(&mut nonce).map_err(|_| StepUpError::Entropy)?;
            let hash = stepup_hash(&nonce, &request.text);

            let entry = Entry {
                owner,
                action: request.action.clone(),
                target: request.target.clone(),
                actor: identity.actor_label(),
                text_hash: hash.clone(),
                expires_at,
                payload: request.payload,
            };
            events.push(entry.event(&stepup_id, now, StepUpOutcome::Requested));
            state.pending.insert(stepup_id.clone(), entry);
            state.requests.entry(key).or_default().push_back(now);

            Ok(StepUpChallenge {
                stepup_id,
                action: request.action,
                target: request.target,
                text: request.text,
                nonce: hex(&nonce),
                hash,
                expires_at,
            })
        })();
        self.emit(&events);
        result
    }

    /// Einmalig. Reihenfolge: Eintrag suchen; Besitzer != → NotFound (Eintrag BLEIBT);
    /// `now >= expires_at` → entfernen, Audit `Expired`, `Expired`;
    /// Hash ungleich (konstante Zeit) → entfernen, Audit `HashMismatch`, `HashMismatch`;
    /// Caps erneut pruefen (fehlend → entfernen, Audit `Refused`, `Denied`);
    /// sonst entfernen, Audit `Confirmed`, Nutzlast zurueck.
    pub fn confirm(
        &self,
        identity: &ClientIdentity,
        stepup_id: &str,
        hash_hex: &str,
        now: Timestamp,
    ) -> Result<P, StepUpError> {
        let owner = StepUpOwner::from_identity(identity);
        let (entry, outcome) = {
            let mut state = self.state.lock().map_err(|_| POISONED)?;
            match state.pending.get(stepup_id) {
                None => return Err(StepUpError::NotFound),
                Some(entry) if entry.owner != owner => return Err(StepUpError::NotFound),
                Some(_) => {}
            }
            let Some(entry) = state.pending.remove(stepup_id) else {
                return Err(StepUpError::NotFound);
            };
            let outcome = if now >= entry.expires_at {
                Err(StepUpError::Expired)
            } else if !constant_time_eq(entry.text_hash.as_bytes(), hash_hex.as_bytes()) {
                Err(StepUpError::HashMismatch)
            } else if let Err(error) = check_identity(identity) {
                Err(error)
            } else {
                Ok(())
            };
            (entry, outcome)
        };
        let audit_outcome = match &outcome {
            Ok(()) => StepUpOutcome::Confirmed,
            Err(StepUpError::Expired) => StepUpOutcome::Expired,
            Err(StepUpError::HashMismatch) => StepUpOutcome::HashMismatch,
            Err(_) => StepUpOutcome::Refused,
        };
        self.emit(&[entry.event(stepup_id, now, audit_outcome)]);
        outcome.map(|()| entry.payload)
    }

    /// Bricht ab (nur eigener Besitzer), Audit `Cancelled`.
    pub fn cancel(
        &self,
        identity: &ClientIdentity,
        stepup_id: &str,
        now: Timestamp,
    ) -> Result<(), StepUpError> {
        let owner = StepUpOwner::from_identity(identity);
        let entry = {
            let mut state = self.state.lock().map_err(|_| POISONED)?;
            match state.pending.get(stepup_id) {
                Some(entry) if entry.owner == owner => {}
                _ => return Err(StepUpError::NotFound),
            }
            state
                .pending
                .remove(stepup_id)
                .ok_or(StepUpError::NotFound)?
        };
        self.emit(&[entry.event(stepup_id, now, StepUpOutcome::Cancelled)]);
        Ok(())
    }

    /// Entfernt alle Eintraege einer Verbindung (beim Trennen), Audit `Cancelled` je Eintrag; liefert Anzahl.
    pub fn drop_connection(&self, connection: ConnectionId, now: Timestamp) -> usize {
        let events = {
            let Ok(mut state) = self.state.lock() else {
                return 0;
            };
            let ids: Vec<String> = state
                .pending
                .iter()
                .filter(|(_, entry)| entry.owner.connection == connection)
                .map(|(id, _)| id.clone())
                .collect();
            let mut events = Vec::with_capacity(ids.len());
            for id in ids {
                if let Some(entry) = state.pending.remove(&id) {
                    events.push(entry.event(&id, now, StepUpOutcome::Cancelled));
                }
            }
            events
        };
        self.emit(&events);
        events.len()
    }

    /// Entfernt Abgelaufene, Audit `Expired` je Eintrag; liefert Anzahl.
    pub fn sweep(&self, now: Timestamp) -> usize {
        let events = {
            let Ok(mut state) = self.state.lock() else {
                return 0;
            };
            state.sweep(now)
        };
        self.emit(&events);
        events.len()
    }

    #[must_use]
    pub fn pending_len(&self) -> usize {
        self.state.lock().map_or(0, |state| state.pending.len())
    }

    /// Audit outside the lock, so an audit sink can never dead-lock the registry.
    fn emit(&self, events: &[StepUpAuditEvent]) {
        for event in events {
            self.audit.record(event);
        }
    }
}

/// Reine Funktion (auch fuer Tests und fuer die App-Gegenpruefung): Hash wie oben.
#[must_use]
pub fn stepup_hash(nonce: &[u8; STEPUP_NONCE_BYTES], text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(STEPUP_HASH_DOMAIN);
    hasher.update(nonce);
    hasher.update([0u8]);
    hasher.update(text.as_bytes());
    let digest = hasher.finalize();
    hex(digest.as_slice())
}

/// Identity and capability check, used at request and again at confirm.
fn check_identity(identity: &ClientIdentity) -> Result<(), StepUpError> {
    if identity.validate().is_err() {
        return Err(StepUpError::Denied("ungueltige Identitaet"));
    }
    if identity.agent.is_some() {
        // A step-up is a human second factor; an agent principal never
        // confirms a global write on the owner's behalf.
        return Err(StepUpError::Denied(
            "Agenten duerfen keinen Step-up anfragen",
        ));
    }
    if !Need::Control.is_granted(identity.caps) {
        return Err(StepUpError::Denied("Recht `control` fehlt"));
    }
    if !Need::Approve.is_granted(identity.caps) {
        return Err(StepUpError::Denied("Recht `approve` fehlt"));
    }
    Ok(())
}

/// Characters that could make the confirmation text look different from what
/// is hashed: C0/C1 controls (except `\n`), bidi overrides/isolates,
/// zero-width characters, BOM, line/paragraph separators.
fn is_forbidden_char(c: char) -> bool {
    (c.is_control() && c != '\n')
        || matches!(
            c,
            '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{2069}'
                | '\u{FEFF}'
        )
}

fn validate_text(text: &str) -> Result<(), StepUpError> {
    if text.len() > MAX_TEXT_BYTES {
        return Err(StepUpError::TextTooLarge);
    }
    if text.trim().is_empty() || text.chars().any(is_forbidden_char) {
        return Err(StepUpError::InvalidText);
    }
    Ok(())
}

fn validate_action(action: &str) -> Result<(), StepUpError> {
    let well_formed = !action.is_empty()
        && action.len() <= MAX_ACTION_BYTES
        && action.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        });
    if well_formed {
        Ok(())
    } else {
        Err(StepUpError::InvalidText)
    }
}

fn validate_target(target: &str) -> Result<(), StepUpError> {
    if target.len() > MAX_TARGET_BYTES || target.chars().any(|c| c == '\n' || is_forbidden_char(c))
    {
        return Err(StepUpError::InvalidText);
    }
    Ok(())
}

/// Draws a random id not yet in use.
fn fresh_id(in_use: impl Fn(&str) -> bool) -> Result<String, StepUpError> {
    for _ in 0..ID_ATTEMPTS {
        let mut bytes = [0u8; STEPUP_ID_BYTES];
        getrandom::fill(&mut bytes).map_err(|_| StepUpError::Entropy)?;
        let id = hex(&bytes);
        if !in_use(&id) {
            return Ok(id);
        }
    }
    Err(StepUpError::Entropy)
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        // Writing to a String cannot fail.
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Compares without an early exit on the first differing byte. Lengths are
/// public (always 64 for a well-formed answer).
fn constant_time_eq(expected: &[u8], given: &[u8]) -> bool {
    if expected.len() != given.len() {
        return false;
    }
    let diff = expected
        .iter()
        .zip(given)
        .fold(0u8, |acc, (a, b)| acc | (a ^ b));
    std::hint::black_box(diff) == 0
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use harw_types::{DeviceId, PermissionTier, TenantId, TrustZone};
    use jiff::{SignedDuration, Timestamp};

    use super::*;
    use crate::identity::test_identity::local;

    /// Fixed vector for `hash_is_domain_separated_and_stable`, computed once
    /// with an independent SHA-256 implementation over
    /// `"harw-stepup-v1\0" || [7u8; 32] || 0x00 || TEXT`.
    const FIXED_TEXT: &str = "harw step-up v1\naktion: provider.add";
    const FIXED_HASH: &str = "e8342bc3b8b6a0b675f8e3de8740a92a7996816ff788252e253a009fe067ea89";

    #[derive(Default)]
    struct RecordingAudit(Mutex<Vec<StepUpAuditEvent>>);

    impl StepUpAudit for RecordingAudit {
        fn record(&self, event: &StepUpAuditEvent) {
            if let Ok(mut events) = self.0.lock() {
                events.push(event.clone());
            }
        }
    }

    impl RecordingAudit {
        fn outcomes(&self) -> Vec<StepUpOutcome> {
            self.0
                .lock()
                .map(|events| events.iter().map(|e| e.outcome).collect())
                .unwrap_or_default()
        }

        fn events(&self) -> Vec<StepUpAuditEvent> {
            self.0.lock().map(|e| e.clone()).unwrap_or_default()
        }
    }

    fn registry() -> (StepUpRegistry<&'static str>, Arc<RecordingAudit>) {
        let audit = Arc::new(RecordingAudit::default());
        (StepUpRegistry::new(audit.clone()), audit)
    }

    /// A remote phone identity with synthetic ids.
    fn phone(tier: PermissionTier, device: &str) -> ClientIdentity {
        let mut identity = local(4242, tier);
        identity.zone = TrustZone::Remote;
        identity.tenant = Some(TenantId::from_str("tenant-test"));
        identity.device = Some(DeviceId::from_str(device));
        identity
    }

    fn t0() -> Timestamp {
        Timestamp::from_second(1_800_000_000).unwrap_or(Timestamp::UNIX_EPOCH)
    }

    fn secs(s: i64) -> SignedDuration {
        SignedDuration::from_secs(s)
    }

    fn add(at: Timestamp, s: i64) -> Timestamp {
        at.checked_add(secs(s)).unwrap_or(at)
    }

    fn req(payload: &'static str) -> StepUpRequest<&'static str> {
        StepUpRequest {
            action: "provider.add".into(),
            target: "anthropic-key".into(),
            text: "harw step-up v1\naktion: provider.add\nurl: https://api.example.test/v1\nschluessel: anthropic-key".into(),
            payload,
        }
    }

    #[test]
    fn confirm_with_matching_hash_returns_payload_once() -> Result<(), StepUpError> {
        let (registry, audit) = registry();
        let owner = phone(PermissionTier::Owner, "device-test-1");
        let challenge = registry.request(&owner, req("plan-1"), t0())?;
        assert_eq!(challenge.stepup_id.len(), STEPUP_ID_BYTES * 2);
        assert_eq!(challenge.nonce.len(), STEPUP_NONCE_BYTES * 2);
        assert_eq!(challenge.hash.len(), 64);
        assert_eq!(challenge.expires_at, add(t0(), STEPUP_TTL_SECS));
        assert_eq!(registry.pending_len(), 1);

        let payload =
            registry.confirm(&owner, &challenge.stepup_id, &challenge.hash, add(t0(), 5))?;
        assert_eq!(payload, "plan-1");
        assert_eq!(registry.pending_len(), 0);

        // Replay of the same nonce/hash: consumed.
        assert_eq!(
            registry.confirm(&owner, &challenge.stepup_id, &challenge.hash, add(t0(), 6)),
            Err(StepUpError::NotFound)
        );
        assert_eq!(
            audit.outcomes(),
            vec![StepUpOutcome::Requested, StepUpOutcome::Confirmed]
        );
        Ok(())
    }

    #[test]
    fn hash_matches_recomputation_from_nonce() -> Result<(), StepUpError> {
        let (registry, _) = registry();
        let owner = phone(PermissionTier::Owner, "device-test-1");
        let challenge = registry.request(&owner, req("p"), t0())?;
        let mut nonce = [0u8; STEPUP_NONCE_BYTES];
        for (i, slot) in nonce.iter_mut().enumerate() {
            let pair = challenge.nonce.get(i * 2..i * 2 + 2).unwrap_or("zz");
            *slot = u8::from_str_radix(pair, 16).map_err(|_| StepUpError::InvalidText)?;
        }
        assert_eq!(stepup_hash(&nonce, &challenge.text), challenge.hash);
        Ok(())
    }

    #[test]
    fn expired_stepup_fails() -> Result<(), StepUpError> {
        let (registry, audit) = registry();
        let owner = phone(PermissionTier::Owner, "device-test-1");
        let challenge = registry.request(&owner, req("plan"), t0())?;
        assert_eq!(
            registry.confirm(
                &owner,
                &challenge.stepup_id,
                &challenge.hash,
                add(t0(), STEPUP_TTL_SECS)
            ),
            Err(StepUpError::Expired)
        );
        assert_eq!(
            registry.confirm(
                &owner,
                &challenge.stepup_id,
                &challenge.hash,
                add(t0(), STEPUP_TTL_SECS)
            ),
            Err(StepUpError::NotFound)
        );
        assert_eq!(
            audit.outcomes(),
            vec![StepUpOutcome::Requested, StepUpOutcome::Expired]
        );
        Ok(())
    }

    #[test]
    fn sweep_removes_expired_entries() -> Result<(), StepUpError> {
        let (registry, audit) = registry();
        let owner = phone(PermissionTier::Owner, "device-test-1");
        registry.request(&owner, req("a"), t0())?;
        registry.request(&owner, req("b"), add(t0(), 30))?;
        assert_eq!(registry.sweep(add(t0(), 59)), 0);
        assert_eq!(registry.sweep(add(t0(), 60)), 1);
        assert_eq!(registry.pending_len(), 1);
        assert_eq!(registry.sweep(add(t0(), 90)), 1);
        assert_eq!(registry.pending_len(), 0);
        assert_eq!(
            audit
                .outcomes()
                .iter()
                .filter(|o| **o == StepUpOutcome::Expired)
                .count(),
            2
        );
        Ok(())
    }

    #[test]
    fn wrong_hash_burns_the_stepup() -> Result<(), StepUpError> {
        let (registry, audit) = registry();
        let owner = phone(PermissionTier::Owner, "device-test-1");
        let challenge = registry.request(&owner, req("plan"), t0())?;
        let wrong = "0".repeat(64);
        assert_ne!(wrong, challenge.hash);
        assert_eq!(
            registry.confirm(&owner, &challenge.stepup_id, &wrong, add(t0(), 1)),
            Err(StepUpError::HashMismatch)
        );
        // Even the right hash no longer works.
        assert_eq!(
            registry.confirm(&owner, &challenge.stepup_id, &challenge.hash, add(t0(), 2)),
            Err(StepUpError::NotFound)
        );
        // Upper-case or truncated answers are wrong answers too.
        let second = registry.request(&owner, req("plan"), add(t0(), 3))?;
        assert_eq!(
            registry.confirm(
                &owner,
                &second.stepup_id,
                &second.hash.to_ascii_uppercase(),
                add(t0(), 4)
            ),
            Err(StepUpError::HashMismatch)
        );
        assert_eq!(
            audit.outcomes(),
            vec![
                StepUpOutcome::Requested,
                StepUpOutcome::HashMismatch,
                StepUpOutcome::Requested,
                StepUpOutcome::HashMismatch
            ]
        );
        Ok(())
    }

    #[test]
    fn foreign_device_cannot_confirm_or_burn() -> Result<(), StepUpError> {
        let (registry, _) = registry();
        let owner = phone(PermissionTier::Owner, "device-test-1");
        let challenge = registry.request(&owner, req("plan"), t0())?;

        let other_device = phone(PermissionTier::Owner, "device-test-2");
        let mut other_connection = owner.clone();
        other_connection.connection = ConnectionId::next();
        let mut other_tenant = owner.clone();
        other_tenant.tenant = Some(TenantId::from_str("tenant-test-2"));

        for foreign in [&other_device, &other_connection, &other_tenant] {
            assert_eq!(
                registry.confirm(foreign, &challenge.stepup_id, &challenge.hash, add(t0(), 1)),
                Err(StepUpError::NotFound)
            );
            assert_eq!(
                registry.confirm(foreign, &challenge.stepup_id, &"f".repeat(64), add(t0(), 1)),
                Err(StepUpError::NotFound)
            );
            assert_eq!(
                registry.cancel(foreign, &challenge.stepup_id, add(t0(), 1)),
                Err(StepUpError::NotFound)
            );
        }
        assert_eq!(registry.pending_len(), 1);
        assert_eq!(
            registry.confirm(&owner, &challenge.stepup_id, &challenge.hash, add(t0(), 2))?,
            "plan"
        );
        Ok(())
    }

    #[test]
    fn observer_and_operator_without_control_cannot_request() {
        let (registry, audit) = registry();
        let observer = phone(PermissionTier::Observer, "device-test-1");
        let operator = phone(PermissionTier::Operator, "device-test-1");
        assert!(matches!(
            registry.request(&observer, req("p"), t0()),
            Err(StepUpError::Denied(_))
        ));
        assert!(matches!(
            registry.request(&operator, req("p"), t0()),
            Err(StepUpError::Denied(_))
        ));

        // control without approve is not enough either.
        let mut control_only = phone(PermissionTier::Owner, "device-test-1");
        control_only.caps.approve = false;
        assert!(matches!(
            registry.request(&control_only, req("p"), t0()),
            Err(StepUpError::Denied(_))
        ));

        // A remote caller without tenant fails identity validation.
        let mut unscoped = phone(PermissionTier::Owner, "device-test-1");
        unscoped.tenant = None;
        assert!(matches!(
            registry.request(&unscoped, req("p"), t0()),
            Err(StepUpError::Denied(_))
        ));

        assert_eq!(registry.pending_len(), 0);
        assert!(audit.outcomes().is_empty());
    }

    #[test]
    fn caps_lost_before_confirm_are_refused() -> Result<(), StepUpError> {
        let (registry, audit) = registry();
        let mut owner = phone(PermissionTier::Owner, "device-test-1");
        let challenge = registry.request(&owner, req("plan"), t0())?;
        owner.caps.control = false;
        assert!(matches!(
            registry.confirm(&owner, &challenge.stepup_id, &challenge.hash, add(t0(), 1)),
            Err(StepUpError::Denied(_))
        ));
        assert_eq!(registry.pending_len(), 0);
        assert_eq!(
            audit.outcomes(),
            vec![StepUpOutcome::Requested, StepUpOutcome::Refused]
        );
        Ok(())
    }

    #[test]
    fn agent_principal_cannot_request() {
        let (registry, _) = registry();
        let mut agent = crate::identity::test_identity::uia(7, &[]);
        agent.caps.control = true;
        agent.caps.approve = true;
        assert!(matches!(
            registry.request(&agent, req("p"), t0()),
            Err(StepUpError::Denied(_))
        ));
    }

    #[test]
    fn pending_and_rate_limits() -> Result<(), StepUpError> {
        let (registry, _) = registry();
        let owner = phone(PermissionTier::Owner, "device-test-1");
        let mut accepted = 0usize;
        let mut ids = Vec::new();
        for i in 0..MAX_PENDING_PER_OWNER {
            let at = add(t0(), i64::try_from(i).unwrap_or(0));
            ids.push(registry.request(&owner, req("p"), at)?.stepup_id);
            accepted += 1;
        }
        // 5th open step-up.
        assert_eq!(
            registry
                .request(&owner, req("p"), add(t0(), 10))
                .map(|c| c.stepup_id),
            Err(StepUpError::TooManyPending)
        );
        // The limit is per device, not per connection.
        let mut second_connection = owner.clone();
        second_connection.connection = ConnectionId::next();
        assert_eq!(
            registry
                .request(&second_connection, req("p"), add(t0(), 10))
                .map(|c| c.stepup_id),
            Err(StepUpError::TooManyPending)
        );
        // Another device is not affected.
        let other = phone(PermissionTier::Owner, "device-test-2");
        let other_id = registry.request(&other, req("p"), add(t0(), 10))?.stepup_id;
        registry.cancel(&other, &other_id, add(t0(), 10))?;

        for id in ids.drain(..) {
            registry.cancel(&owner, &id, add(t0(), 11))?;
        }
        while accepted < MAX_REQUESTS_PER_OWNER_PER_HOUR {
            let at = add(t0(), 20 + i64::try_from(accepted).unwrap_or(0));
            let id = registry.request(&owner, req("p"), at)?.stepup_id;
            registry.cancel(&owner, &id, at)?;
            accepted += 1;
        }
        // 31st request within the hour.
        assert_eq!(
            registry
                .request(&owner, req("p"), add(t0(), 120))
                .map(|c| c.stepup_id),
            Err(StepUpError::RateLimited)
        );
        // Sliding window: an hour after the first request one slot is free again.
        let later = registry.request(&owner, req("p"), add(t0(), 3600))?;
        assert_eq!(later.stepup_id.len(), STEPUP_ID_BYTES * 2);
        assert_eq!(
            registry
                .request(&owner, req("p"), add(t0(), 3600))
                .map(|c| c.stepup_id),
            Err(StepUpError::RateLimited)
        );
        Ok(())
    }

    #[test]
    fn hash_is_domain_separated_and_stable() {
        let nonce = [7u8; STEPUP_NONCE_BYTES];
        assert_eq!(stepup_hash(&nonce, FIXED_TEXT), FIXED_HASH);
        assert_ne!(
            stepup_hash(&nonce, "harw step-up v1\naktion: provider.remove"),
            FIXED_HASH
        );
        assert_ne!(
            stepup_hash(&[8u8; STEPUP_NONCE_BYTES], FIXED_TEXT),
            FIXED_HASH
        );
        // Without the domain prefix the digest differs.
        let mut plain = Sha256::new();
        plain.update(nonce);
        plain.update([0u8]);
        plain.update(FIXED_TEXT.as_bytes());
        assert_ne!(hex(plain.finalize().as_slice()), FIXED_HASH);
    }

    #[test]
    fn audit_never_contains_text_or_nonce() -> Result<(), StepUpError> {
        let (registry, audit) = registry();
        let owner = phone(PermissionTier::Owner, "device-test-1");
        let first = registry.request(&owner, req("a"), t0())?;
        let second = registry.request(&owner, req("b"), t0())?;
        let third = registry.request(&owner, req("c"), t0())?;
        registry.confirm(&owner, &first.stepup_id, &first.hash, add(t0(), 1))?;
        let _ = registry.confirm(&owner, &second.stepup_id, &"0".repeat(64), add(t0(), 1));
        registry.cancel(&owner, &third.stepup_id, add(t0(), 1))?;
        registry.request(&owner, req("d"), add(t0(), 2))?;
        assert_eq!(registry.sweep(add(t0(), 120)), 1);

        let events = audit.events();
        assert_eq!(events.len(), 8);
        for event in &events {
            let json = serde_json::to_string(event).unwrap_or_default();
            assert!(!json.is_empty());
            assert!(!json.contains("api.example.test"), "audit leaks text");
            assert!(!json.contains("schluessel"), "audit leaks text");
            for nonce in [&first.nonce, &second.nonce, &third.nonce] {
                assert!(!json.contains(nonce.as_str()), "audit leaks nonce");
            }
        }
        assert_eq!(
            events.first().map(|e| e.text_hash.as_str()),
            Some(first.hash.as_str())
        );
        Ok(())
    }

    #[test]
    fn text_with_control_chars_is_rejected() {
        let (registry, _) = registry();
        let owner = phone(PermissionTier::Owner, "device-test-1");
        let with_text = |text: &str| StepUpRequest {
            action: "provider.add".into(),
            target: "x".into(),
            text: text.into(),
            payload: "p",
        };
        for bad in [
            "",
            "   \n",
            "a\rb",
            "a\tb",
            "a\u{1b}[2Jb",
            "a\u{7f}b",
            "url: https://api.example.test/\u{202e}1v",
            "a\u{200b}b",
            "a\u{2028}b",
        ] {
            assert_eq!(
                registry
                    .request(&owner, with_text(bad), t0())
                    .map(|c| c.stepup_id),
                Err(StepUpError::InvalidText),
                "accepted text {bad:?}"
            );
        }
        assert_eq!(
            registry
                .request(&owner, with_text(&"x".repeat(MAX_TEXT_BYTES + 1)), t0())
                .map(|c| c.stepup_id),
            Err(StepUpError::TextTooLarge)
        );
        let bad_action = StepUpRequest {
            action: "Provider Add".into(),
            ..with_text("ok")
        };
        assert_eq!(
            registry
                .request(&owner, bad_action, t0())
                .map(|c| c.stepup_id),
            Err(StepUpError::InvalidText)
        );
        let bad_target = StepUpRequest {
            target: "a\nb".into(),
            ..with_text("ok")
        };
        assert_eq!(
            registry
                .request(&owner, bad_target, t0())
                .map(|c| c.stepup_id),
            Err(StepUpError::InvalidText)
        );
        assert_eq!(registry.pending_len(), 0);
        assert!(
            registry
                .request(&owner, with_text(&"x".repeat(MAX_TEXT_BYTES)), t0())
                .is_ok()
        );
    }

    #[test]
    fn drop_connection_cancels_only_that_connection() -> Result<(), StepUpError> {
        let (registry, audit) = registry();
        let owner = phone(PermissionTier::Owner, "device-test-1");
        let mut other = owner.clone();
        other.connection = ConnectionId::next();
        registry.request(&owner, req("a"), t0())?;
        registry.request(&owner, req("b"), t0())?;
        let kept = registry.request(&other, req("c"), t0())?;
        assert_eq!(registry.drop_connection(owner.connection, add(t0(), 1)), 2);
        assert_eq!(registry.pending_len(), 1);
        assert_eq!(
            audit
                .outcomes()
                .iter()
                .filter(|o| **o == StepUpOutcome::Cancelled)
                .count(),
            2
        );
        assert_eq!(
            registry.confirm(&other, &kept.stepup_id, &kept.hash, add(t0(), 2))?,
            "c"
        );
        Ok(())
    }

    #[test]
    fn host_error_mapping_hides_which_check_failed() {
        let messages: Vec<String> = [
            StepUpError::NotFound,
            StepUpError::Expired,
            StepUpError::HashMismatch,
        ]
        .into_iter()
        .map(|e| HostError::from(e).to_string())
        .collect();
        assert!(messages.windows(2).all(|w| w[0] == w[1]));
        assert!(matches!(
            HostError::from(StepUpError::TextTooLarge),
            HostError::Protocol(_)
        ));
        assert!(matches!(
            HostError::from(StepUpError::Entropy),
            HostError::Storage(_)
        ));
    }
}
