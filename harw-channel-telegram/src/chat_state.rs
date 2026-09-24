//! Dauerhafter Chat-Zustand je Telegram-Gesprächsschlüssel.
//!
//! # Verantwortung
//! Hält pro [`SessionKey`] den gewählten Workspace-Alias (`/workspace`) und
//! die Sitzungsgeneration (`/new`). Aus Schlüssel und Generation leitet
//! [`telegram_session_id`] eine stabile [`SessionId`] ab, sodass aufeinander
//! folgende Nachrichten desselben Chats dieselbe Agent-Sitzung fortsetzen und
//! `/new` eine frische Sitzung beginnt.
//!
//! # Persistenz
//! Eine JSON-Datei je Schlüssel unter `<root>/<hex(canonical_key)>.json`
//! (bei sehr langen Schlüsseln `<root>/h-<sha256(canonical_key)>.json`, damit
//! Dateinamen-Grenzen nie überschritten werden). Schreiben erfolgt atomar über
//! [`NamedTempFile`] im selben Verzeichnis plus `persist` (unter Unix Modus
//! 0600). Ein einzelner prozessinterner [`Mutex`] serialisiert alle
//! Lese-Ändere-Schreibe-Zyklen.
//!
//! # Invariante
//! Ein Workspace-Alias wird nur gespeichert, wenn er über
//! [`WorkspaceRegistry::resolve`] für den Tenant des Schlüssels auflöst; der
//! Alias ist nie selbst ein Dateisystem-Selektor.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use harw_authority::WorkspaceRegistry;
use harw_channel::SessionKey;
use harw_types::{SessionId, WorkspaceId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use crate::error::{TelegramChannelError, TelegramChannelResult};

/// Maximale Länge des hex-kodierten Schlüssels als Dateiname; längere
/// Schlüssel werden auf ihren SHA-256-Digest abgebildet.
const MAX_HEX_FILENAME_LEN: usize = 200;

/// Zustand eines Telegram-Chats (bzw. Forum-Themas).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatState {
    /// Per `/workspace` gewählter, gegen die Registry geprüfter Alias.
    #[serde(default)]
    pub workspace_alias: Option<String>,
    /// Sitzungsgeneration; `/new` erhöht sie um eins.
    #[serde(default)]
    pub session_generation: u64,
}

/// On-Disk-Form: der Zustand plus der kanonische Schlüssel zur Kontrolle.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredChatState {
    key: String,
    #[serde(flatten)]
    state: ChatState,
}

/// Leitet die [`SessionId`] eines Chats aus Schlüssel und Generation ab:
/// `"tg-"` + die ersten 32 Hex-Zeichen von
/// `sha256(canonical_key || 0x00 || generation als u64 big-endian)`.
#[must_use]
pub fn telegram_session_id(key: &SessionKey, generation: u64) -> SessionId {
    let mut hasher = Sha256::new();
    hasher.update(key.canonical_key().as_bytes());
    hasher.update([0u8]);
    hasher.update(generation.to_be_bytes());
    let digest = hex(&hasher.finalize());
    let prefix = digest.get(..32).unwrap_or(&digest);
    SessionId::from_str(format!("tg-{prefix}"))
}

fn hex(bytes: &[u8]) -> String {
    use fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn state_file_name(key: &SessionKey) -> String {
    let canonical = key.canonical_key();
    let encoded = hex(canonical.as_bytes());
    if encoded.len() <= MAX_HEX_FILENAME_LEN {
        format!("{encoded}.json")
    } else {
        format!("h-{}.json", hex(&Sha256::digest(canonical.as_bytes())))
    }
}

/// Dauerhafter Speicher für [`ChatState`]s, siehe Moduldokumentation.
pub struct ChatStateStore {
    root: PathBuf,
    guard: Mutex<()>,
}

impl fmt::Debug for ChatStateStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChatStateStore")
            .field("root", &self.root)
            .finish()
    }
}

impl ChatStateStore {
    /// Erzeugt einen Speicher unter `root` (wird beim ersten Schreiben
    /// angelegt).
    #[must_use]
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            guard: Mutex::new(()),
        }
    }

    fn path(&self, key: &SessionKey) -> PathBuf {
        self.root.join(state_file_name(key))
    }

    /// Liefert den Zustand von `key`; unbekannte Schlüssel ergeben den
    /// Standardzustand (kein Alias, Generation 0).
    ///
    /// # Errors
    /// [`TelegramChannelError::Io`] / [`TelegramChannelError::Serde`] bei
    /// Lesefehlern oder beschädigter Datei.
    pub fn get(&self, key: &SessionKey) -> TelegramChannelResult<ChatState> {
        let _guard = self.guard.lock().unwrap_or_else(|p| p.into_inner());
        self.read(key)
    }

    /// Setzt (`Some`) oder löscht (`None`) den Workspace-Alias von `key`.
    ///
    /// # Errors
    /// - [`TelegramChannelError::WorkspaceUnresolved`]: der Alias löst für
    ///   `key.tenant` nicht über `workspaces` auf; nichts wird geändert.
    /// - [`TelegramChannelError::Io`] / [`TelegramChannelError::Serde`]:
    ///   Persistenzfehler.
    pub fn set_workspace(
        &self,
        key: &SessionKey,
        alias: Option<&str>,
        workspaces: &WorkspaceRegistry,
    ) -> TelegramChannelResult<ChatState> {
        if let Some(alias) = alias {
            workspaces
                .resolve(&key.tenant, &WorkspaceId::from_str(alias))
                .map_err(|source| TelegramChannelError::WorkspaceUnresolved {
                    alias: alias.to_owned(),
                    tenant: key.tenant.clone(),
                    source,
                })?;
        }
        let _guard = self.guard.lock().unwrap_or_else(|p| p.into_inner());
        let mut state = self.read(key)?;
        state.workspace_alias = alias.map(str::to_owned);
        self.write(key, &state)?;
        Ok(state)
    }

    /// Beginnt eine neue Sitzung für `key` (`/new`): erhöht die Generation.
    ///
    /// # Errors
    /// [`TelegramChannelError::Io`] / [`TelegramChannelError::Serde`].
    pub fn reset_session(&self, key: &SessionKey) -> TelegramChannelResult<ChatState> {
        let _guard = self.guard.lock().unwrap_or_else(|p| p.into_inner());
        let mut state = self.read(key)?;
        state.session_generation = state.session_generation.saturating_add(1);
        self.write(key, &state)?;
        Ok(state)
    }

    /// Die aktuelle [`SessionId`] von `key`, siehe [`telegram_session_id`].
    ///
    /// # Errors
    /// Wie [`Self::get`].
    pub fn session_id(&self, key: &SessionKey) -> TelegramChannelResult<SessionId> {
        let state = self.get(key)?;
        Ok(telegram_session_id(key, state.session_generation))
    }

    // Aufrufer hält `guard`.
    fn read(&self, key: &SessionKey) -> TelegramChannelResult<ChatState> {
        let bytes = match std::fs::read(self.path(key)) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ChatState::default());
            }
            Err(error) => return Err(TelegramChannelError::from(error)),
        };
        let stored: StoredChatState = serde_json::from_slice(&bytes)?;
        if stored.key != key.canonical_key() {
            return Err(TelegramChannelError::from(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Chat-Zustandsdatei gehört zu einem anderen Schlüssel",
            )));
        }
        Ok(stored.state)
    }

    // Aufrufer hält `guard`.
    fn write(&self, key: &SessionKey, state: &ChatState) -> TelegramChannelResult<()> {
        std::fs::create_dir_all(&self.root)?;
        let stored = StoredChatState {
            key: key.canonical_key(),
            state: state.clone(),
        };
        let mut temp = NamedTempFile::new_in(&self.root)?;
        serde_json::to_writer(temp.as_file_mut(), &stored)?;
        temp.as_file().sync_all()?;
        temp.persist(self.path(key))
            .map_err(|error| TelegramChannelError::from(error.error))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};
    use harw_authority::WorkspaceRegistration;
    use harw_types::{ChannelId, PeerId, TenantId, ThreadRef};

    fn key(peer: &str, thread: Option<&str>) -> SessionKey {
        SessionKey::new(
            TenantId::from_str("ops"),
            ChannelId::from_str("telegram:ops"),
            PeerId::from_str(peer),
            thread.map(ThreadRef::from_str),
        )
    }

    fn registry(root: &Path) -> TestResult<WorkspaceRegistry> {
        WorkspaceRegistry::build(
            root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("ops"),
                workspace: WorkspaceId::from_str("ops-room"),
                root: PathBuf::from("."),
            }],
        )
        .map_err(ctx("registry builds"))
    }

    #[test]
    fn unknown_key_yields_default_state() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("dir"))?;
        let store = ChatStateStore::new(&dir.path().join("chat"));
        assert_eq!(
            store.get(&key("1", None)).map_err(ctx("get"))?,
            ChatState::default()
        );
        Ok(())
    }

    #[test]
    fn set_workspace_validates_alias_and_persists() -> TestResult {
        let harness = tempfile::tempdir().map_err(ctx("harness"))?;
        let dir = tempfile::tempdir().map_err(ctx("dir"))?;
        let registry = registry(harness.path())?;
        let store = ChatStateStore::new(dir.path());
        let chat = key("1", Some("7"));

        let state = store
            .set_workspace(&chat, Some("ops-room"), &registry)
            .map_err(ctx("set alias"))?;
        assert_eq!(state.workspace_alias.as_deref(), Some("ops-room"));

        assert!(matches!(
            store.set_workspace(&chat, Some("unknown"), &registry),
            Err(TelegramChannelError::WorkspaceUnresolved { alias, .. }) if alias == "unknown"
        ));

        // Anderer Tenant: derselbe Alias löst nicht auf.
        let foreign = SessionKey::new(
            TenantId::from_str("other"),
            ChannelId::from_str("telegram:ops"),
            PeerId::from_str("1"),
            None,
        );
        assert!(matches!(
            store.set_workspace(&foreign, Some("ops-room"), &registry),
            Err(TelegramChannelError::WorkspaceUnresolved { .. })
        ));

        // Durabel über eine neue Instanz hinweg.
        let reopened = ChatStateStore::new(dir.path());
        assert_eq!(
            reopened
                .get(&chat)
                .map_err(ctx("reopen"))?
                .workspace_alias
                .as_deref(),
            Some("ops-room")
        );
        assert_eq!(
            reopened.get(&key("1", None)).map_err(ctx("root"))?,
            ChatState::default()
        );

        let cleared = reopened
            .set_workspace(&chat, None, &registry)
            .map_err(ctx("clear"))?;
        assert_eq!(cleared.workspace_alias, None);
        Ok(())
    }

    #[test]
    fn reset_session_bumps_generation_and_session_id() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("dir"))?;
        let store = ChatStateStore::new(dir.path());
        let chat = key("42", None);

        let first = store.session_id(&chat).map_err(ctx("first id"))?;
        assert_eq!(first, telegram_session_id(&chat, 0));
        assert!(first.as_str().starts_with("tg-"));
        assert_eq!(first.as_str().len(), 3 + 32);
        assert_eq!(store.session_id(&chat).map_err(ctx("stable"))?, first);

        let state = store.reset_session(&chat).map_err(ctx("reset"))?;
        assert_eq!(state.session_generation, 1);
        let second = store.session_id(&chat).map_err(ctx("second id"))?;
        assert_ne!(second, first);
        assert_eq!(second, telegram_session_id(&chat, 1));
        assert_ne!(
            telegram_session_id(&key("43", None), 0),
            telegram_session_id(&chat, 0)
        );
        Ok(())
    }

    #[test]
    fn very_long_keys_use_digest_file_name() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("dir"))?;
        let store = ChatStateStore::new(dir.path());
        let long_peer = "9".repeat(400);
        let chat = key(&long_peer, None);
        assert!(state_file_name(&chat).starts_with("h-"));
        let state = store.reset_session(&chat).map_err(ctx("reset"))?;
        assert_eq!(store.get(&chat).map_err(ctx("get"))?, state);
        Ok(())
    }
}
