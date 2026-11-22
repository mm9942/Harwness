//! Write-only-Anbindung des Secret-Stores fuer `secret.put` (Welle 3, Scheibe 3, Paket C9c).
//!
//! # Verantwortungsbereich
//! Ein Handy (Remote-Client) darf einen Provider-Schluessel nur SCHREIBEN, nie lesen. Der Ablauf:
//!
//! 1. `secret.put {name, value, for_provider}` → [`StagedSecrets::stage`] merkt den Wert im Speicher
//!    vor und liefert einen undurchsichtigen [`StagedSecretHandle`] plus [`StagedSecretInfo`]
//!    (Name, Provider, grobe [`KeyShape`], Ablauf). Der Wert verlaesst diese Datei nie wieder
//!    Richtung Aufrufer.
//! 2. Die folgende `/provider add*` prueft per [`StagedSecrets::describe`] Bindung und Form.
//! 3. Erst nach bestaetigtem Step-up entnimmt der Gateway den Wert einmalig per
//!    [`StagedSecrets::take`] und versiegelt ihn ueber [`WriteOnlySecretSink::put`] im Store.
//!    Scheitert das Schreiben der Provider-Datei, entfernt [`WriteOnlySecretSink::discard`] die Id.
//!
//! Regeln (Vertrag K-W3b, R2/R6): nur schreiben; Bindung an genau EINEN neuen Provider
//! (`for_provider`); Verfall nach [`STAGED_SECRET_TTL_SECS`]; Besitzer = Verbindung; kein Wert in
//! `Debug`, `Display`, Fehlertext oder Info. Es gibt bewusst keine Lese-Methode.
//!
//! # Nebenlaeufigkeit
//! [`StagedSecrets`] ist `Send + Sync` (eine `Mutex`); eine Instanz je Gateway (`Arc`).
//! Eine vergiftete Sperre liefert [`WriteOnlyError::Store`], nie panic; Aufraeum-Methoden
//! (`discard_owner`, `sweep`) raeumen auch bei vergifteter Sperre.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Mutex, MutexGuard, PoisonError};

use jiff::{SignedDuration, Timestamp};
use secrecy::{ExposeSecret, SecretBox, SecretString};

use crate::error::SecretsError;
use crate::id::SecretId;
use crate::store::SecretStore;

/// Vorgemerkte Schluessel verfallen nach dieser Zeit (Step-up 60 s plus Eingabe).
pub const STAGED_SECRET_TTL_SECS: i64 = 180;
/// Hoechstens so viele gleichzeitig vorgemerkte Schluessel je Besitzer.
pub const MAX_STAGED_PER_OWNER: usize = 4;
/// Hoechstens so viele gleichzeitig vorgemerkte Schluessel insgesamt.
pub const MAX_STAGED_TOTAL: usize = 64;
/// Mindestlaenge des Werts nach `trim()`.
pub const MIN_SECRET_LEN: usize = 8;
/// Hoechstlaenge des Werts nach `trim()`.
pub const MAX_SECRET_LEN: usize = 8192;
/// Hoechstlaenge des Schluesselnamens.
pub const MAX_KEY_NAME_LEN: usize = 64;
/// Hoechstlaenge des Provider-Namens.
pub const MAX_PROVIDER_NAME_LEN: usize = 48;
/// `purpose` im Store fuer remote angelegte Provider-Schluessel.
pub const REMOTE_PROVIDER_PURPOSE: &str = "provider authentication (remote)";
/// Zufallsbytes je Griff (32 Hex-Zeichen).
pub const HANDLE_BYTES: usize = 16;

/// Undurchsichtiger Griff (32 Hex, klein). Kein Bezug zum Wert.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StagedSecretHandle(String);

impl StagedSecretHandle {
    /// Der Griff als Hex-Text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn generate() -> Result<Self, WriteOnlyError> {
        let mut bytes = [0_u8; HANDLE_BYTES];
        getrandom::fill(&mut bytes).map_err(|_| WriteOnlyError::Entropy)?;
        let mut hex = String::with_capacity(HANDLE_BYTES * 2);
        for byte in bytes {
            hex.push(hex_digit(byte >> 4));
            hex.push(hex_digit(byte & 0x0f));
        }
        Ok(Self(hex))
    }
}

impl fmt::Display for StagedSecretHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn hex_digit(nibble: u8) -> char {
    char::from(b"0123456789abcdef"[usize::from(nibble & 0x0f)])
}

/// Besitzer: aus `ClientIdentity` im Gateway gebaut (`tenant.as_str()`, `device.as_str()`, `connection.0`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SecretOwner {
    pub tenant: Option<String>,
    pub device: Option<String>,
    pub connection: u64,
}

/// Grobe Form, nur Praefix-Klasse. Nie Bytes des Werts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyShape {
    /// `sk-ant-oat…` (Anthropic-Setup-/OAuth-Token).
    AnthropicOAuthToken,
    /// `sk-ant-…` (Anthropic-API-Schluessel).
    AnthropicApiKey,
    /// `sk-…` (OpenAI und kompatible).
    OpenAiStyleKey,
    /// Alles andere.
    Other,
}

/// Eingabe von `secret.put` (C9d baut sie; `value` sofort als SecretString, Rohstring verwerfen).
pub struct SecretPutRequest {
    /// Name im Store, `[A-Za-z0-9._-]`, 1..=MAX_KEY_NAME_LEN.
    pub key_name: String,
    /// Neuer Provider, an den der Schluessel gebunden ist, `[A-Za-z0-9_-]`, 1..=MAX_PROVIDER_NAME_LEN.
    pub for_provider: String,
    pub value: SecretString,
}

impl fmt::Debug for SecretPutRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretPutRequest")
            .field("key_name", &self.key_name)
            .field("for_provider", &self.for_provider)
            .field("value", &"[redacted]")
            .finish()
    }
}

/// Oeffentliche Beschreibung eines vorgemerkten Schluessels (geht ans Handy). Ohne Wert.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StagedSecretInfo {
    pub key_name: String,
    pub for_provider: String,
    pub shape: KeyShape,
    pub expires_at: Timestamp,
}

/// Ein entnommener Schluessel. Kein `Clone`, kein `Serialize`; `Debug` zeigt nur Namen.
pub struct StagedSecret {
    key_name: String,
    for_provider: String,
    value: SecretString,
}

impl StagedSecret {
    /// Name, unter dem der Wert im Store abgelegt wird.
    #[must_use]
    pub fn key_name(&self) -> &str {
        &self.key_name
    }

    /// Provider, an den der Wert gebunden ist.
    #[must_use]
    pub fn for_provider(&self) -> &str {
        &self.for_provider
    }
    // Bewusst KEIN oeffentlicher Zugriff auf `value`: nur `WriteOnlySecretSink` in dieser Datei.
}

impl fmt::Debug for StagedSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StagedSecret")
            .field("key_name", &self.key_name)
            .field("for_provider", &self.for_provider)
            .field("value", &"[redacted]")
            .finish()
    }
}

/// Fehler der Write-only-Anbindung. Kein Wert in irgendeiner Variante.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WriteOnlyError {
    InvalidKeyName,
    InvalidProvider,
    EmptyValue,
    ValueTooShort,
    ValueTooLong,
    /// Nicht nur druckbares ASCII ohne Leerraum (0x21..=0x7E).
    ValueNotPrintable,
    TooManyStaged,
    /// Unbekannt, verbraucht oder fremder Besitzer (eine Antwort).
    NotFound,
    Expired,
    /// `for_provider` passt nicht.
    BindingMismatch,
    Entropy,
    /// Store-Fehler; Text aus `SecretsError` ohne Wert (wie `harw-cli` `store_error_message`).
    Store(String),
}

impl fmt::Display for WriteOnlyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidKeyName => write!(
                f,
                "ungueltiger Schluesselname: erlaubt sind A-Z, a-z, 0-9, '.', '_', '-' (1 bis {MAX_KEY_NAME_LEN} Zeichen)"
            ),
            Self::InvalidProvider => write!(
                f,
                "ungueltiger Provider-Name: erlaubt sind A-Z, a-z, 0-9, '_', '-' (1 bis {MAX_PROVIDER_NAME_LEN} Zeichen)"
            ),
            Self::EmptyValue => f.write_str("Schluessel ist leer"),
            Self::ValueTooShort => {
                write!(f, "Schluessel ist zu kurz (mindestens {MIN_SECRET_LEN} Zeichen)")
            }
            Self::ValueTooLong => {
                write!(f, "Schluessel ist zu lang (hoechstens {MAX_SECRET_LEN} Zeichen)")
            }
            Self::ValueNotPrintable => f.write_str(
                "Schluessel enthaelt unzulaessige Zeichen (nur druckbares ASCII ohne Leerraum)",
            ),
            Self::TooManyStaged => f.write_str(
                "zu viele vorgemerkte Schluessel; bitte bestaetigen oder kurz warten",
            ),
            Self::NotFound => f.write_str("vorgemerkter Schluessel nicht gefunden"),
            Self::Expired => f.write_str("vorgemerkter Schluessel ist abgelaufen; bitte neu eingeben"),
            Self::BindingMismatch => {
                f.write_str("vorgemerkter Schluessel gehoert zu einem anderen Provider")
            }
            Self::Entropy => f.write_str("Zufallsquelle nicht verfuegbar"),
            Self::Store(reason) => write!(f, "Schluessel konnte nicht abgelegt werden: {reason}"),
        }
    }
}

impl std::error::Error for WriteOnlyError {}

/// Uebersetzt einen Store-Fehler ohne Wert (Spiegel von `harw-cli` `store_error_message`).
fn store_error(error: &SecretsError) -> WriteOnlyError {
    match error {
        SecretsError::DekWrapperUnavailable { reason, .. } => {
            WriteOnlyError::Store(format!("Schluessel-Dienst nicht erreichbar: {reason}"))
        }
        SecretsError::NotFound { .. } => WriteOnlyError::Store("Id nicht gefunden".to_owned()),
        _ => WriteOnlyError::Store("versiegelter Speicher meldet einen Fehler".to_owned()),
    }
}

/// Schreib-Senke. Es gibt bewusst KEINE Lese-Methode.
pub trait WriteOnlySecretSink {
    /// Versiegelt `secret` unter `secret.key_name()` mit `purpose`; liefert die neue Id.
    fn put(&mut self, secret: StagedSecret, purpose: &str) -> Result<SecretId, WriteOnlyError>;
    /// Rollback: entfernt eine eben angelegte Id.
    fn discard(&mut self, id: &SecretId) -> Result<(), WriteOnlyError>;
}

impl WriteOnlySecretSink for SecretStore {
    fn put(&mut self, secret: StagedSecret, purpose: &str) -> Result<SecretId, WriteOnlyError> {
        let StagedSecret {
            key_name, value, ..
        } = secret;
        // Kopie direkt in einen zeroizenden Container; `value` wird beim Drop genullt.
        let bytes: SecretBox<[u8]> =
            SecretBox::new(value.expose_secret().as_bytes().to_vec().into_boxed_slice());
        drop(value);
        self.create(&key_name, purpose, &bytes)
            .map_err(|error| store_error(&error))
    }

    fn discard(&mut self, id: &SecretId) -> Result<(), WriteOnlyError> {
        self.delete(id).map_err(|error| store_error(&error))
    }
}

/// Ein vorgemerkter Eintrag. Kein `Debug`/`Clone`.
struct Entry {
    owner: SecretOwner,
    key_name: String,
    for_provider: String,
    value: SecretString,
    expires_at: Timestamp,
    shape: KeyShape,
}

impl Entry {
    fn info(&self) -> StagedSecretInfo {
        StagedSecretInfo {
            key_name: self.key_name.clone(),
            for_provider: self.for_provider.clone(),
            shape: self.shape,
            expires_at: self.expires_at,
        }
    }

    fn is_expired(&self, now: Timestamp) -> bool {
        now >= self.expires_at
    }
}

/// In-Memory-Vormerkung (Gateway-weit eine Instanz, `Arc<StagedSecrets>`).
pub struct StagedSecrets {
    entries: Mutex<HashMap<String, Entry>>,
}

impl fmt::Debug for StagedSecrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StagedSecrets")
            .field("len", &self.len())
            .finish_non_exhaustive()
    }
}

impl Default for StagedSecrets {
    fn default() -> Self {
        Self::new()
    }
}

fn poisoned() -> WriteOnlyError {
    WriteOnlyError::Store("interner Zustand".to_owned())
}

fn valid_name(name: &str, max: usize, allow_dot: bool) -> bool {
    !name.is_empty()
        && name.len() <= max
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-' || (allow_dot && byte == b'.')
        })
}

/// Prueft den Wert nach `trim()` und liefert ihn als neuen `SecretString`.
fn validated_value(value: &SecretString) -> Result<SecretString, WriteOnlyError> {
    let trimmed = value.expose_secret().trim();
    if trimmed.is_empty() {
        return Err(WriteOnlyError::EmptyValue);
    }
    if trimmed.len() > MAX_SECRET_LEN {
        return Err(WriteOnlyError::ValueTooLong);
    }
    if !trimmed.bytes().all(|byte| (0x21..=0x7e).contains(&byte)) {
        return Err(WriteOnlyError::ValueNotPrintable);
    }
    if trimmed.len() < MIN_SECRET_LEN {
        return Err(WriteOnlyError::ValueTooShort);
    }
    Ok(SecretString::new(trimmed.to_owned()))
}

impl StagedSecrets {
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    fn lock(&self) -> Result<MutexGuard<'_, HashMap<String, Entry>>, WriteOnlyError> {
        self.entries.lock().map_err(|_| poisoned())
    }

    /// Fuer Aufraeumen: auch eine vergiftete Sperre wird betreten (Verwerfen ist immer sicher).
    fn lock_for_cleanup(&self) -> MutexGuard<'_, HashMap<String, Entry>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Validiert (Name, Provider, Wert nach `trim()`), raeumt Abgelaufene, prueft Limits, erzeugt Griff.
    pub fn stage(
        &self,
        owner: SecretOwner,
        request: SecretPutRequest,
        now: Timestamp,
    ) -> Result<(StagedSecretHandle, StagedSecretInfo), WriteOnlyError> {
        let SecretPutRequest {
            key_name,
            for_provider,
            value,
        } = request;
        if !valid_name(&key_name, MAX_KEY_NAME_LEN, true) {
            return Err(WriteOnlyError::InvalidKeyName);
        }
        if !valid_name(&for_provider, MAX_PROVIDER_NAME_LEN, false) {
            return Err(WriteOnlyError::InvalidProvider);
        }
        let value = validated_value(&value)?;
        let shape = classify_key(&value);
        let expires_at = now
            .checked_add(SignedDuration::from_secs(STAGED_SECRET_TTL_SECS))
            .map_err(|_| WriteOnlyError::Store("Zeitberechnung".to_owned()))?;

        let mut entries = self.lock()?;
        entries.retain(|_, entry| !entry.is_expired(now));
        if entries.len() >= MAX_STAGED_TOTAL {
            return Err(WriteOnlyError::TooManyStaged);
        }
        let owned = entries.values().filter(|entry| entry.owner == owner).count();
        if owned >= MAX_STAGED_PER_OWNER {
            return Err(WriteOnlyError::TooManyStaged);
        }
        // Kollision bei 128 Zufallsbit praktisch ausgeschlossen; trotzdem nie ueberschreiben.
        let handle = StagedSecretHandle::generate()?;
        if entries.contains_key(handle.as_str()) {
            return Err(WriteOnlyError::Entropy);
        }
        let entry = Entry {
            owner,
            key_name,
            for_provider,
            value,
            expires_at,
            shape,
        };
        let info = entry.info();
        entries.insert(handle.as_str().to_owned(), entry);
        Ok((handle, info))
    }

    /// Ohne Verbrauch. Fremder Besitzer → NotFound; `for_provider` ungleich → BindingMismatch.
    /// Ein abgelaufener Eintrag wird entfernt (→ Expired).
    pub fn describe(
        &self,
        owner: &SecretOwner,
        handle: &str,
        for_provider: &str,
        now: Timestamp,
    ) -> Result<StagedSecretInfo, WriteOnlyError> {
        let mut entries = self.lock()?;
        check_entry(&mut entries, owner, handle, for_provider, now)?;
        entries
            .get(handle)
            .map(Entry::info)
            .ok_or(WriteOnlyError::NotFound)
    }

    /// Einmalige Entnahme (entfernt den Eintrag). Abgelaufen → entfernen + Expired.
    pub fn take(
        &self,
        owner: &SecretOwner,
        handle: &str,
        for_provider: &str,
        now: Timestamp,
    ) -> Result<StagedSecret, WriteOnlyError> {
        let mut entries = self.lock()?;
        check_entry(&mut entries, owner, handle, for_provider, now)?;
        let entry = entries.remove(handle).ok_or(WriteOnlyError::NotFound)?;
        Ok(StagedSecret {
            key_name: entry.key_name,
            for_provider: entry.for_provider,
            value: entry.value,
        })
    }

    /// Beim Trennen der Verbindung: alle Eintraege dieses Besitzers verwerfen (Wert wird gezeroized beim Drop).
    pub fn discard_owner(&self, owner: &SecretOwner) -> usize {
        let mut entries = self.lock_for_cleanup();
        let before = entries.len();
        entries.retain(|_, entry| entry.owner != *owner);
        before - entries.len()
    }

    /// Entfernt alle abgelaufenen Eintraege; liefert deren Anzahl.
    pub fn sweep(&self, now: Timestamp) -> usize {
        let mut entries = self.lock_for_cleanup();
        let before = entries.len();
        entries.retain(|_, entry| !entry.is_expired(now));
        before - entries.len()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.lock_for_cleanup().len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Gemeinsame Pruefung fuer `describe`/`take`: Existenz, Besitzer, Ablauf, Bindung (in dieser Reihenfolge).
fn check_entry(
    entries: &mut HashMap<String, Entry>,
    owner: &SecretOwner,
    handle: &str,
    for_provider: &str,
    now: Timestamp,
) -> Result<(), WriteOnlyError> {
    let (foreign, expired, mismatch) = match entries.get(handle) {
        None => return Err(WriteOnlyError::NotFound),
        Some(entry) => (
            entry.owner != *owner,
            entry.is_expired(now),
            entry.for_provider != for_provider,
        ),
    };
    if foreign {
        // Fremder Besitzer: gleiche Antwort wie "unbekannt", Eintrag bleibt fuer den Besitzer.
        return Err(WriteOnlyError::NotFound);
    }
    if expired {
        let _ = entries.remove(handle);
        return Err(WriteOnlyError::Expired);
    }
    if mismatch {
        return Err(WriteOnlyError::BindingMismatch);
    }
    Ok(())
}

/// Praefix-Klasse: `sk-ant-oat` → AnthropicOAuthToken; `sk-ant-` → AnthropicApiKey; `sk-` → OpenAiStyleKey; sonst Other.
#[must_use]
pub fn classify_key(value: &SecretString) -> KeyShape {
    let value = value.expose_secret().trim_start();
    if value.starts_with("sk-ant-oat") {
        KeyShape::AnthropicOAuthToken
    } else if value.starts_with("sk-ant-") {
        KeyShape::AnthropicApiKey
    } else if value.starts_with("sk-") {
        KeyShape::OpenAiStyleKey
    } else {
        KeyShape::Other
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::kek::{KekProvenance, derive_public_key};
    use crate::policy::CryptoPolicy;
    use crate::store::KekMaterial;
    use crate::id::KeyVersion;
    use crate::test_support::{TestError, TestResult};

    const FAKE: &str = "sk-test-FAKE-0000000000000000";

    impl From<WriteOnlyError> for TestError {
        fn from(source: WriteOnlyError) -> Self {
            TestError::Unexpected(format!("write-only: {source:?}"))
        }
    }

    fn owner(connection: u64) -> SecretOwner {
        SecretOwner {
            tenant: Some("tenant-test".to_owned()),
            device: Some("device-test-1".to_owned()),
            connection,
        }
    }

    fn request(key_name: &str, for_provider: &str, value: &str) -> SecretPutRequest {
        SecretPutRequest {
            key_name: key_name.to_owned(),
            for_provider: for_provider.to_owned(),
            value: SecretString::new(value.to_owned()),
        }
    }

    fn t0() -> TestResult<Timestamp> {
        Timestamp::from_second(1_800_000_000)
            .map_err(|error| TestError::Unexpected(error.to_string()))
    }

    fn later(now: Timestamp, secs: i64) -> TestResult<Timestamp> {
        now.checked_add(SignedDuration::from_secs(secs))
            .map_err(|error| TestError::Unexpected(error.to_string()))
    }

    fn expect_err<T>(result: Result<T, WriteOnlyError>, want: &WriteOnlyError) -> TestResult {
        match result {
            Err(error) if error == *want => Ok(()),
            Err(error) => Err(TestError::Unexpected(format!("Fehler {error:?}, erwartet {want:?}"))),
            Ok(_) => Err(TestError::Unexpected(format!("Ok, erwartet {want:?}"))),
        }
    }

    #[test]
    fn stage_never_returns_value() -> TestResult {
        let staged = StagedSecrets::new();
        let now = t0()?;
        let (handle, info) = staged.stage(owner(1), request("openai-key", "openai-key", FAKE), now)?;
        assert_eq!(handle.as_str().len(), HANDLE_BYTES * 2);
        assert!(handle
            .as_str()
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));
        assert_eq!(info.shape, KeyShape::OpenAiStyleKey);
        assert_eq!(info.expires_at, later(now, STAGED_SECRET_TTL_SECS)?);

        let texts = [
            format!("{info:?}"),
            format!("{handle}"),
            format!("{handle:?}"),
            format!("{staged:?}"),
            format!("{:?}", request("k", "p", FAKE)),
        ];
        for text in &texts {
            assert!(!text.contains(FAKE));
            assert!(!text.contains("FAKE"));
        }
        let taken = staged.take(&owner(1), handle.as_str(), "openai-key", now)?;
        let debug = format!("{taken:?}");
        assert!(!debug.contains("FAKE"));
        assert!(debug.contains("[redacted]"));
        assert_eq!(taken.key_name(), "openai-key");
        assert_eq!(taken.for_provider(), "openai-key");
        Ok(())
    }

    #[test]
    fn take_is_single_use() -> TestResult {
        let staged = StagedSecrets::new();
        let now = t0()?;
        let (handle, _) = staged.stage(owner(1), request("k1", "p1", FAKE), now)?;
        let _first = staged.take(&owner(1), handle.as_str(), "p1", now)?;
        expect_err(
            staged.take(&owner(1), handle.as_str(), "p1", now),
            &WriteOnlyError::NotFound,
        )?;
        expect_err(
            staged.describe(&owner(1), handle.as_str(), "p1", now),
            &WriteOnlyError::NotFound,
        )?;
        assert!(staged.is_empty());
        Ok(())
    }

    #[test]
    fn take_after_ttl_is_expired() -> TestResult {
        let staged = StagedSecrets::new();
        let now = t0()?;
        let (handle, _) = staged.stage(owner(1), request("k1", "p1", FAKE), now)?;
        // Kurz vor Ablauf: noch gueltig (describe verbraucht nicht).
        let info = staged.describe(&owner(1), handle.as_str(), "p1", later(now, STAGED_SECRET_TTL_SECS - 1)?)?;
        assert_eq!(info.key_name, "k1");
        expect_err(
            staged.take(&owner(1), handle.as_str(), "p1", later(now, STAGED_SECRET_TTL_SECS)?),
            &WriteOnlyError::Expired,
        )?;
        // Abgelaufen wurde entfernt.
        assert!(staged.is_empty());
        expect_err(
            staged.take(&owner(1), handle.as_str(), "p1", now),
            &WriteOnlyError::NotFound,
        )?;

        let (_h2, _) = staged.stage(owner(1), request("k2", "p2", FAKE), now)?;
        assert_eq!(staged.sweep(later(now, STAGED_SECRET_TTL_SECS + 1)?), 1);
        assert!(staged.is_empty());
        Ok(())
    }

    #[test]
    fn foreign_owner_gets_not_found() -> TestResult {
        let staged = StagedSecrets::new();
        let now = t0()?;
        let (handle, _) = staged.stage(owner(1), request("k1", "p1", FAKE), now)?;
        let other_connection = owner(2);
        let other_device = SecretOwner {
            device: Some("device-test-2".to_owned()),
            ..owner(1)
        };
        for foreign in [&other_connection, &other_device] {
            expect_err(
                staged.describe(foreign, handle.as_str(), "p1", now),
                &WriteOnlyError::NotFound,
            )?;
            expect_err(
                staged.take(foreign, handle.as_str(), "p1", now),
                &WriteOnlyError::NotFound,
            )?;
        }
        expect_err(
            staged.take(&owner(1), "00000000000000000000000000000000", "p1", now),
            &WriteOnlyError::NotFound,
        )?;
        // Der Besitzer bekommt ihn weiterhin.
        assert_eq!(staged.len(), 1);
        let _ = staged.take(&owner(1), handle.as_str(), "p1", now)?;

        // Disconnect verwirft alle Eintraege des Besitzers.
        let _ = staged.stage(owner(3), request("a", "p1", FAKE), now)?;
        let _ = staged.stage(owner(3), request("b", "p2", FAKE), now)?;
        let _ = staged.stage(owner(4), request("c", "p3", FAKE), now)?;
        assert_eq!(staged.discard_owner(&owner(3)), 2);
        assert_eq!(staged.len(), 1);
        Ok(())
    }

    #[test]
    fn binding_mismatch_is_rejected() -> TestResult {
        let staged = StagedSecrets::new();
        let now = t0()?;
        let (handle, _) = staged.stage(owner(1), request("k1", "new-provider", FAKE), now)?;
        expect_err(
            staged.describe(&owner(1), handle.as_str(), "other", now),
            &WriteOnlyError::BindingMismatch,
        )?;
        expect_err(
            staged.take(&owner(1), handle.as_str(), "other", now),
            &WriteOnlyError::BindingMismatch,
        )?;
        let taken = staged.take(&owner(1), handle.as_str(), "new-provider", now)?;
        assert_eq!(taken.for_provider(), "new-provider");
        Ok(())
    }

    #[test]
    fn limits_per_owner_and_total() -> TestResult {
        let staged = StagedSecrets::new();
        let now = t0()?;
        for index in 0..MAX_STAGED_PER_OWNER {
            let _ = staged.stage(owner(1), request(&format!("k{index}"), "p", FAKE), now)?;
        }
        expect_err(
            staged.stage(owner(1), request("extra", "p", FAKE), now),
            &WriteOnlyError::TooManyStaged,
        )?;
        // Nach Ablauf ist wieder Platz.
        let _ = staged.stage(
            owner(1),
            request("extra", "p", FAKE),
            later(now, STAGED_SECRET_TTL_SECS)?,
        )?;
        assert_eq!(staged.len(), 1);

        let total = StagedSecrets::new();
        let mut connection = 100_u64;
        while total.len() < MAX_STAGED_TOTAL {
            let _ = total.stage(owner(connection), request("k", "p", FAKE), now)?;
            connection += 1;
        }
        expect_err(
            total.stage(owner(connection), request("k", "p", FAKE), now),
            &WriteOnlyError::TooManyStaged,
        )?;
        Ok(())
    }

    #[test]
    fn value_validation() -> TestResult {
        let staged = StagedSecrets::new();
        let now = t0()?;
        let cases: [(&str, WriteOnlyError); 5] = [
            ("", WriteOnlyError::EmptyValue),
            ("   \n", WriteOnlyError::EmptyValue),
            // 7 Zeichen: eins unter MIN_SECRET_LEN.
            ("sk-shor", WriteOnlyError::ValueTooShort),
            ("sk-test-FAKE 0000000000", WriteOnlyError::ValueNotPrintable),
            ("sk-test-FAKE-\u{00e4}000000000", WriteOnlyError::ValueNotPrintable),
        ];
        for (value, want) in &cases {
            expect_err(staged.stage(owner(1), request("k", "p", value), now), want)?;
        }
        // Genau MIN_SECRET_LEN ist gueltig.
        let (edge, _) = staged.stage(owner(1), request("k", "p", "sk-short"), now)?;
        let _ = staged.take(&owner(1), edge.as_str(), "p", now)?;
        let too_long = "a".repeat(MAX_SECRET_LEN + 1);
        expect_err(
            staged.stage(owner(1), request("k", "p", &too_long), now),
            &WriteOnlyError::ValueTooLong,
        )?;
        expect_err(
            staged.stage(owner(1), request("k", "p", "sk-test-FAKE-\t000000"), now),
            &WriteOnlyError::ValueNotPrintable,
        )?;

        // Namen.
        let long_key = "k".repeat(MAX_KEY_NAME_LEN + 1);
        for bad in ["", "a b", "a/b", long_key.as_str()] {
            expect_err(
                staged.stage(owner(1), request(bad, "p", FAKE), now),
                &WriteOnlyError::InvalidKeyName,
            )?;
        }
        let long_provider = "p".repeat(MAX_PROVIDER_NAME_LEN + 1);
        for bad in ["", "a.b", "a b", long_provider.as_str()] {
            expect_err(
                staged.stage(owner(1), request("k", bad, FAKE), now),
                &WriteOnlyError::InvalidProvider,
            )?;
        }
        assert!(staged.is_empty());

        // Zeilenumbruch am Ende wird getrimmt: der entnommene Wert ist ohne Umbruch.
        let (handle, _) = staged.stage(owner(1), request("k.v1", "p_1", &format!("{FAKE}\r\n")), now)?;
        let taken = staged.take(&owner(1), handle.as_str(), "p_1", now)?;
        assert_eq!(taken.value.expose_secret().as_str(), FAKE);
        Ok(())
    }

    #[test]
    fn classify_shapes() {
        let shape = |value: &str| classify_key(&SecretString::new(value.to_owned()));
        assert_eq!(shape("sk-ant-oat01-FAKE-0000000000"), KeyShape::AnthropicOAuthToken);
        assert_eq!(shape("sk-ant-api03-FAKE-0000000000"), KeyShape::AnthropicApiKey);
        assert_eq!(shape(FAKE), KeyShape::OpenAiStyleKey);
        assert_eq!(shape("FAKE-token-0000000000"), KeyShape::Other);
    }

    #[test]
    fn errors_name_no_value() {
        let all = [
            WriteOnlyError::InvalidKeyName,
            WriteOnlyError::InvalidProvider,
            WriteOnlyError::EmptyValue,
            WriteOnlyError::ValueTooShort,
            WriteOnlyError::ValueTooLong,
            WriteOnlyError::ValueNotPrintable,
            WriteOnlyError::TooManyStaged,
            WriteOnlyError::NotFound,
            WriteOnlyError::Expired,
            WriteOnlyError::BindingMismatch,
            WriteOnlyError::Entropy,
            WriteOnlyError::Store("interner Zustand".to_owned()),
        ];
        for error in &all {
            let text = error.to_string();
            assert!(!text.is_empty());
            assert!(!text.contains("sk-"));
        }
    }

    // ---- Senke gegen einen echten Store (OFFEN-15 aufgeloest: Material ueber die oeffentliche
    // `derive_public_key` + `KekMaterial::new`, wie `store.rs` `test_material`). ----

    fn test_root() -> PathBuf {
        std::env::temp_dir().join(format!(
            "harw-secrets-write-only-{}-{}",
            std::process::id(),
            SecretId::new()
        ))
    }

    fn test_store(root: PathBuf) -> TestResult<SecretStore> {
        let policy = CryptoPolicy::strongest();
        let seed = [0xA5_u8; 32];
        let public = derive_public_key(&policy, &SecretBox::new(seed.to_vec().into_boxed_slice()))?;
        let material = KekMaterial::new(public, SecretBox::new(seed.to_vec().into_boxed_slice()))?;
        Ok(SecretStore::with_key_material(
            root,
            policy,
            KekProvenance::EnvSeed {
                var: "HARW_TEST_SEED".to_owned(),
            },
            KeyVersion::initial(),
            material,
        ))
    }

    fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) -> TestResult {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if path.is_dir() {
                collect_files(&path, out)?;
            } else {
                out.push(path);
            }
        }
        Ok(())
    }

    fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
        !needle.is_empty() && haystack.windows(needle.len()).any(|window| window == needle)
    }

    fn sink_round_trip(root: &Path) -> TestResult {
        let mut store = test_store(root.to_path_buf())?;
        let staged = StagedSecrets::new();
        let now = t0()?;
        let (handle, _) = staged.stage(owner(1), request("openai-key", "openai-key", FAKE), now)?;
        let secret = staged.take(&owner(1), handle.as_str(), "openai-key", now)?;

        let sink: &mut dyn WriteOnlySecretSink = &mut store;
        let id = sink.put(secret, REMOTE_PROVIDER_PURPOSE)?;
        assert!(store.contains(&id));
        assert_eq!(store.metadata(&id)?.name, "openai-key");
        assert_eq!(store.metadata(&id)?.purpose, REMOTE_PROVIDER_PURPOSE);

        let mut files = Vec::new();
        collect_files(root, &mut files)?;
        assert!(!files.is_empty());
        for file in &files {
            let bytes = std::fs::read(file)?;
            assert!(!contains_bytes(&bytes, FAKE.as_bytes()));
            assert!(!contains_bytes(&bytes, b"FAKE-0000"));
        }

        let sink: &mut dyn WriteOnlySecretSink = &mut store;
        sink.discard(&id)?;
        assert!(!store.contains(&id));
        let sink: &mut dyn WriteOnlySecretSink = &mut store;
        expect_err(
            sink.discard(&id),
            &WriteOnlyError::Store("Id nicht gefunden".to_owned()),
        )?;
        Ok(())
    }

    #[test]
    fn sink_put_writes_no_plaintext() -> TestResult {
        let root = test_root();
        let result = sink_round_trip(&root);
        let _ = std::fs::remove_dir_all(&root);
        result
    }
}
