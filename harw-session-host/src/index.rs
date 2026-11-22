//! Profil-Index: TUI-Sessions des Profil-Stores fuer das Teilen (Plan §3.2, §3.4).
//!
//! Dieses Modul enthaelt nur den Trait und die Typen; die Implementierung liegt in
//! `harw-cli/src/profile_index.rs`. Alle Index-Methoden sind lesend.

use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use harw_protocol::session_wire::ShareMode;
use harw_protocol::session_wire::{
    HolderInfo, HolderKind as WireHolderKind, LastOrigin as WireLastOrigin, PageParams,
    ProjectSummary, ProjectsParams,
};
use harw_session_store::{HolderKind, LastOrigin, LeaseHolder};
use harw_types::{PermissionTier, SessionId, TenantId};

use crate::error::HostError;

/// Herkunft einer Session aus dem Thread ihres ERSTEN Datensatzes.
///
/// Praefixe: `cli-session:`, `web:`, `cli-run-session:`, `gateway-...`, `session-host:`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ThreadOrigin {
    Cli,
    Web,
    Run,
    Gateway,
    Other,
}

impl ThreadOrigin {
    /// `"cli-session:"` -> Cli, `"web:"` -> Web, `"cli-run-session:"` -> Run,
    /// `"gateway-"` oder `"session-host:"` -> Gateway, sonst Other.
    #[must_use]
    pub fn from_thread(thread: &str) -> Self {
        if thread.starts_with("cli-session:") {
            Self::Cli
        } else if thread.starts_with("web:") {
            Self::Web
        } else if thread.starts_with("cli-run-session:") {
            Self::Run
        } else if thread.starts_with("gateway-") || thread.starts_with("session-host:") {
            Self::Gateway
        } else {
            Self::Other
        }
    }

    /// Nur `Cli` ist fortsetzbar.
    #[must_use]
    pub const fn is_continuable(self) -> bool {
        matches!(self, Self::Cli)
    }
}

/// Eine Zeile des Profil-Index (Server-intern; Pfade verlassen den Server nie).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileRow {
    pub session_id: SessionId,
    /// `SessionMeta::display_title()` (nie leer).
    pub title: String,
    pub project_key: Option<String>,
    /// Anzeige-Name des Projekts (Basename bzw. Teil vor dem Hash).
    pub project_label: Option<String>,
    /// max(mtime `.jsonl`, `last_opened_at`, `last_activity_at`).
    pub updated_at: jiff::Timestamp,
    pub turns: u64,
    pub first_message: Option<String>,
    pub origin: ThreadOrigin,
    pub last_origin: Option<LastOrigin>,
    pub archived: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfilePage {
    pub rows: Vec<ProfileRow>,
    pub next_cursor: Option<String>,
}

/// `active_count` ist hier immer 0; der Host traegt laufende Slots ein.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectPage {
    pub projects: Vec<ProjectSummary>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedProject {
    pub project_key: String,
    /// Kanonisch, existiert, liegt in den bekannten Roots (und `share_roots`, falls gesetzt).
    pub project_root: PathBuf,
    pub label: String,
}

/// Nur-Lese-Index ueber `<profile>/sessions`. Implementiert in `harw-cli/src/profile_index.rs`.
/// Alle Methoden sind synchron, fehlertolerant (eine schlechte Datei ueberspringt nur sich selbst)
/// und schreiben nie.
pub trait ProfileIndex: Send + Sync {
    /// Transcript-Wurzel (= `profile_sessions_root`).
    fn sessions_root(&self) -> &Path;
    /// Projekte, sortiert nach `last_activity` absteigend, dann `project_key` aufsteigend.
    /// `query`: Teilstring ohne Gross/Klein in `label` und `path_hint`. Archivierte zaehlen nicht.
    ///
    /// # Errors
    /// `HostError` bei ungueltigem Cursor oder Speicherfehlern.
    fn projects(&self, params: &ProjectsParams) -> Result<ProjectPage, HostError>;
    /// Sessions, sortiert nach (`updated_at` desc, `session_id` desc).
    ///
    /// # Errors
    /// `HostError` bei ungueltigem Cursor oder Speicherfehlern.
    fn page(&self, params: &PageParams) -> Result<ProfilePage, HostError>;
    /// Eine sichtbare Zeile (leer/corrupt/verwaist/vor `share_since` -> `None`; archiviert -> `Some`).
    ///
    /// # Errors
    /// `HostError` bei Speicherfehlern.
    fn row(&self, id: &SessionId) -> Result<Option<ProfileRow>, HostError>;
    /// Die `limit` neuesten sichtbaren, nicht archivierten Zeilen (Legacy-Liste fuer Minor 2).
    ///
    /// # Errors
    /// `HostError` bei Speicherfehlern.
    fn newest(&self, limit: usize) -> Result<Vec<ProfileRow>, HostError>;
    /// Bekannter Projektschluessel -> geprueft Root. Unbekannt oder ungueltig -> `None`.
    ///
    /// # Errors
    /// `HostError` bei Speicherfehlern.
    fn resolve_project(&self, project_key: &str) -> Result<Option<ResolvedProject>, HostError>;
    /// Geprueft Arbeitsordner einer Session: `project_root` aus der Meta, sonst `cwd`; muss
    /// existieren, kanonisch gleich sein und in den bekannten `project_root`s liegen
    /// (eingeschraenkt durch `share_roots`). Filtert NICHT nach leer. `None` = nur lesen.
    ///
    /// # Errors
    /// `HostError` bei Speicherfehlern.
    fn workdir(&self, id: &SessionId) -> Result<Option<PathBuf>, HostError>;
}

/// Einstellungen des Teilens (aus `[session_listener]`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShareSettings {
    pub mode: ShareMode,
    /// Profil-Zeilen gehoeren zu diesem Tenant.
    pub tenant: TenantId,
    /// Legacy-Zeilen fuer Clients ohne Feature `projects` (Standard 100).
    pub legacy_rows: usize,
    /// Listener-Stufe (`[session_listener] tier`), fuer `ShareInfo.tier`.
    pub listener_tier: PermissionTier,
}

/// Was `SessionHost::open_with_profile` bekommt.
#[derive(Clone)]
pub struct ProfileShare {
    pub settings: ShareSettings,
    pub index: Arc<dyn ProfileIndex>,
}

const CURSOR_PREFIX: &str = "p1.";

/// Seiten-Cursor: undurchsichtig fuer Clients. Format `p1.<hex(utf8("<millis>|<key>"))>`.
#[must_use]
pub fn encode_cursor(at: jiff::Timestamp, key: &str) -> String {
    let payload = format!("{}|{key}", at.as_millisecond());
    let mut out = String::with_capacity(CURSOR_PREFIX.len() + payload.len() * 2);
    out.push_str(CURSOR_PREFIX);
    for byte in payload.as_bytes() {
        out.push(hex_digit(byte >> 4));
        out.push(hex_digit(byte & 0x0f));
    }
    out
}

/// Umkehrung; falsches Format -> `None` (Host antwortet `HostError::Protocol("invalid cursor")`).
#[must_use]
pub fn decode_cursor(cursor: &str) -> Option<(jiff::Timestamp, String)> {
    let hex = cursor.strip_prefix(CURSOR_PREFIX)?;
    if hex.is_empty() || hex.len() % 2 != 0 || !hex.is_ascii() {
        return None;
    }
    let raw = hex.as_bytes();
    let mut bytes = Vec::with_capacity(raw.len() / 2);
    for pair in raw.as_chunks::<2>().0 {
        bytes.push((hex_value(pair[0])? << 4) | hex_value(pair[1])?);
    }
    let text = String::from_utf8(bytes).ok()?;
    let (millis, key) = text.split_once('|')?;
    let millis: i64 = millis.parse().ok()?;
    let at = jiff::Timestamp::from_millisecond(millis).ok()?;
    Some((at, key.to_owned()))
}

const fn hex_digit(nibble: u8) -> char {
    match nibble {
        0..=9 => (b'0' + nibble) as char,
        _ => (b'a' + (nibble - 10)) as char,
    }
}

const fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Anzeigetext fuer BUSY.
#[must_use]
pub fn busy_reason(holder: Option<HolderKind>) -> &'static str {
    match holder {
        Some(HolderKind::Tui) => "Läuft gerade in der TUI",
        Some(HolderKind::Job) => "Job läuft in der TUI",
        Some(HolderKind::Gateway) => "Läuft gerade auf dem Gateway",
        Some(HolderKind::Unknown) | None => "Session ist gerade belegt",
    }
}

/// Holder fuer die Leitung: nur `kind` und `since`, nie `pid`.
#[must_use]
pub fn wire_holder(holder: &LeaseHolder) -> HolderInfo {
    HolderInfo {
        kind: wire_holder_kind(holder.kind),
        since: holder.since,
    }
}

#[must_use]
pub fn wire_last_origin(origin: LastOrigin) -> WireLastOrigin {
    match origin {
        LastOrigin::Tui => WireLastOrigin::Tui,
        LastOrigin::Gateway => WireLastOrigin::Gateway,
        LastOrigin::Unknown => WireLastOrigin::Unknown,
    }
}

#[must_use]
pub fn wire_holder_kind(kind: HolderKind) -> WireHolderKind {
    match kind {
        HolderKind::Tui => WireHolderKind::Tui,
        HolderKind::Gateway => WireHolderKind::Gateway,
        HolderKind::Job => WireHolderKind::Job,
        HolderKind::Unknown => WireHolderKind::Unknown,
    }
}

/// Text fuer DENIED im Lese-Modus.
pub const DENIED_READ_ONLY: &str = "Gateway teilt nur lesend";
/// Text fuer DENIED ohne Teilen.
pub const DENIED_NOT_SHARED: &str = "Dieses Gateway teilt keine TUI-Sessions";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_origin_prefixes() {
        assert_eq!(
            ThreadOrigin::from_thread("cli-session:abc"),
            ThreadOrigin::Cli
        );
        assert_eq!(ThreadOrigin::from_thread("web:abc"), ThreadOrigin::Web);
        assert_eq!(
            ThreadOrigin::from_thread("cli-run-session:abc"),
            ThreadOrigin::Run
        );
        assert_eq!(
            ThreadOrigin::from_thread("gateway-telegram:1"),
            ThreadOrigin::Gateway
        );
        assert_eq!(
            ThreadOrigin::from_thread("session-host:x"),
            ThreadOrigin::Gateway
        );
        assert_eq!(ThreadOrigin::from_thread("anything"), ThreadOrigin::Other);
        assert_eq!(ThreadOrigin::from_thread(""), ThreadOrigin::Other);
        assert!(ThreadOrigin::Cli.is_continuable());
        for origin in [
            ThreadOrigin::Web,
            ThreadOrigin::Run,
            ThreadOrigin::Gateway,
            ThreadOrigin::Other,
        ] {
            assert!(!origin.is_continuable());
        }
    }

    #[test]
    fn cursor_round_trip_and_rejects_garbage() {
        let at = jiff::Timestamp::from_millisecond(1_764_000_000_123).expect("timestamp");
        let key = "01ABCDEF|with-pipe-ü";
        let cursor = encode_cursor(at, key);
        assert!(cursor.starts_with("p1."));
        assert_eq!(decode_cursor(&cursor), Some((at, key.to_owned())));

        for garbage in [
            "",
            "p1.",
            "p1.zz",
            "p1.abc",
            "p2.31",
            "31327c6b",
            "p1.6e6f2d70697065",
            "p1.61|62",
            "p1.ff",
        ] {
            assert_eq!(decode_cursor(garbage), None, "{garbage}");
        }
    }

    #[test]
    fn wire_holder_never_carries_pid() {
        let since = jiff::Timestamp::from_millisecond(1_764_000_000_000).expect("timestamp");
        let holder = LeaseHolder {
            kind: HolderKind::Tui,
            pid: 424_242,
            since,
        };
        let info = wire_holder(&holder);
        assert_eq!(info.kind, WireHolderKind::Tui);
        assert_eq!(info.since, since);
        let json = serde_json::to_string(&info).expect("serialize");
        assert!(!json.contains("pid"), "{json}");
        assert!(!json.contains("424242"), "{json}");
    }

    #[test]
    fn busy_reason_texts() {
        assert_eq!(
            busy_reason(Some(HolderKind::Tui)),
            "Läuft gerade in der TUI"
        );
        assert_eq!(busy_reason(Some(HolderKind::Job)), "Job läuft in der TUI");
        assert_eq!(
            busy_reason(Some(HolderKind::Gateway)),
            "Läuft gerade auf dem Gateway"
        );
        assert_eq!(
            busy_reason(Some(HolderKind::Unknown)),
            "Session ist gerade belegt"
        );
        assert_eq!(busy_reason(None), "Session ist gerade belegt");
    }
}
