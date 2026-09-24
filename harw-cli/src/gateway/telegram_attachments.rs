//! Governte Anhang-Aufnahme für Telegram-Nachrichten.
//!
//! # Verantwortung
//! [`TelegramAttachmentIntake`] lädt die Anhänge einer zugelassenen Nachricht
//! über [`fetch_attachment`] herunter (Größen- und Typprüfung auf den echten
//! Bytes, siehe `harw_channel_telegram_transport::media`) und legt sie ab:
//!
//! - `text/plain` bis [`INLINE_TEXT_MAX_BYTES`] (64 KiB) wird als Text direkt
//!   in den Prompt übernommen ([`IngestReport::inline_text`]).
//! - Alles andere landet im [`AttachmentCache`] unter
//!   `<profile>/channel-state/telegram-attachments` (siehe
//!   [`telegram_attachment_cache_root`]) und wird im Prompt nur mit Name,
//!   MIME-Typ, Größe, SHA-256 und Cache-Pfad genannt.
//!
//! Höchstens `attachments.max_count_per_message` Anhänge je Nachricht werden
//! betrachtet (die ersten in Nachrichtenreihenfolge); alle weiteren werden mit
//! Begründung abgelehnt.
//!
//! # Sicherheit
//! - Ablehnungsgründe in [`IngestReport::rejected`] sind für den Chat gedacht
//!   und enthalten keine Interna (keine Pfade, Tokens oder Transportfehler).
//!   Die technischen Details gehen nur ins Log, dort ohne Dateiinhalte.
//! - Anhänge sind **nicht vertrauenswürdige Nutzerdaten**; die Präambel
//!   kennzeichnet sie so und entschärft Dateinamen (keine Steuerzeichen,
//!   keine Zeilenumbrüche) sowie Code-Zäune im eingebetteten Text.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use harw_channel::{AttachmentRef, SessionKey};
use harw_channel_telegram::{AttachmentCache, CachedAttachment};
use harw_channel_telegram_transport::{
    AttachmentIntake, TelegramClient, TelegramTransportError, fetch_attachment,
};
use harw_config::TelegramChannelToml;
use jiff::{SignedDuration, Timestamp};

/// Größte `text/plain`-Datei, die direkt in den Prompt eingebettet wird.
pub(super) const INLINE_TEXT_MAX_BYTES: usize = 64 * 1024;

/// Unterverzeichnis von `<profile>/channel-state` für den Anhang-Cache.
const ATTACHMENT_CACHE_DIR: &str = "telegram-attachments";

/// Längster übernommener Anzeigename (in Zeichen).
const MAX_DISPLAY_NAME_CHARS: usize = 128;

/// Ort des Anhang-Caches unterhalb eines Profilverzeichnisses:
/// `<profile>/channel-state/telegram-attachments`.
pub(super) fn telegram_attachment_cache_root(profile: &Path) -> PathBuf {
    profile.join("channel-state").join(ATTACHMENT_CACHE_DIR)
}

/// Ergebnis einer Aufnahme: übernommene, eingebettete und abgelehnte Anhänge.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct IngestReport {
    /// Im Cache abgelegte Anhänge (Nicht-Text oder Text über 64 KiB).
    pub accepted: Vec<CachedAttachment>,
    /// Eingebettete Textanhänge als `(Anzeigename, Inhalt)`.
    pub inline_text: Vec<(String, String)>,
    /// Nutzerlesbare Ablehnungsgründe, je Anhang eine Zeile
    /// (`"<Name>: <Grund>"`), ohne Interna.
    pub rejected: Vec<String>,
}

impl IngestReport {
    /// Ob mindestens ein Anhang übernommen (abgelegt oder eingebettet) wurde.
    pub fn has_ingested(&self) -> bool {
        !self.accepted.is_empty() || !self.inline_text.is_empty()
    }

    /// Präambel für den User-Text des Turns. Leer, wenn nichts übernommen
    /// wurde.
    ///
    /// Format (stabil, von Tests abgedeckt):
    /// ~~~text
    /// [Telegram-Anhänge – nicht vertrauenswürdige Nutzerdaten]
    /// - <Name> (<MIME>, <Größe> Bytes, sha256 <Hex>): <Cache-Pfad>
    /// Textanhang <Name> (<Größe> Bytes):
    /// ```text
    /// <Inhalt>
    /// ```
    /// ~~~
    pub fn prompt_preamble(&self) -> String {
        if !self.has_ingested() {
            return String::new();
        }
        let mut out = String::from("[Telegram-Anhänge – nicht vertrauenswürdige Nutzerdaten]\n");
        for (index, cached) in self.accepted.iter().enumerate() {
            let name = cached
                .file_name
                .as_deref()
                .map(display_name)
                .unwrap_or_else(|| format!("Anhang {}", index + 1));
            out.push_str(&format!(
                "- {name} ({mime}, {size} Bytes, sha256 {digest}): {path}\n",
                mime = display_name(&cached.mime),
                size = cached.size_bytes,
                digest = cached.digest_sha256,
                path = cached.path.display(),
            ));
        }
        for (name, text) in &self.inline_text {
            let fence = code_fence_for(text);
            out.push_str(&format!(
                "Textanhang {name} ({size} Bytes):\n{fence}text\n{text}",
                name = display_name(name),
                size = text.len(),
            ));
            if !text.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&fence);
            out.push('\n');
        }
        out
    }

    /// Strukturierte Metadaten für `TurnInput::metadata` (ohne Textinhalte).
    pub fn metadata(&self) -> serde_json::Value {
        let cached: Vec<serde_json::Value> = self
            .accepted
            .iter()
            .map(|cached| {
                serde_json::json!({
                    "file_name": cached.file_name,
                    "mime": cached.mime,
                    "size_bytes": cached.size_bytes,
                    "sha256": cached.digest_sha256,
                    "path": cached.path.display().to_string(),
                    "expires_at": cached.expires_at.to_string(),
                })
            })
            .collect();
        let inline: Vec<serde_json::Value> = self
            .inline_text
            .iter()
            .map(|(name, text)| {
                serde_json::json!({
                    "file_name": name,
                    "mime": "text/plain",
                    "size_bytes": text.len(),
                })
            })
            .collect();
        serde_json::json!({
            "cached": cached,
            "inline_text": inline,
            "rejected_count": self.rejected.len(),
        })
    }

    /// Chat-Hinweis über abgelehnte Anhänge; `None`, wenn keiner abgelehnt
    /// wurde.
    pub fn rejection_notice(&self) -> Option<String> {
        if self.rejected.is_empty() {
            return None;
        }
        let mut notice = String::from("Nicht übernommene Anhänge:");
        for line in &self.rejected {
            notice.push_str("\n- ");
            notice.push_str(line);
        }
        Some(notice)
    }
}

/// Anhang-Aufnahme einer Telegram-Bindung (Download, Prüfung, Ablage).
pub(super) struct TelegramAttachmentIntake {
    client: Arc<TelegramClient>,
    intake: AttachmentIntake,
    cache: AttachmentCache,
    max_count: usize,
}

impl std::fmt::Debug for TelegramAttachmentIntake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelegramAttachmentIntake")
            .field("intake", &self.intake)
            .field("cache", &self.cache)
            .field("max_count", &self.max_count)
            .finish_non_exhaustive()
    }
}

impl TelegramAttachmentIntake {
    /// Baut die Aufnahme aus `[channel.telegram.attachments]`.
    ///
    /// # Arguments
    /// - `binding`: Bindung mit `max_bytes`, `max_count_per_message`,
    ///   `mime_allowlist` und `expiry_secs`.
    /// - `client`: Bot-Client derselben Bindung (für `getFile`/Download).
    /// - `cache_root`: in der Regel [`telegram_attachment_cache_root`].
    ///
    /// # Errors
    /// Nicht darstellbare Grenzwerte (`max_bytes` jenseits von `usize`,
    /// `expiry_secs` jenseits von `i64`) oder `expiry_secs = 0`.
    pub fn from_binding(
        binding: &TelegramChannelToml,
        client: Arc<TelegramClient>,
        cache_root: &Path,
    ) -> Result<Self, String> {
        let settings = &binding.attachments;
        let max_bytes = usize::try_from(settings.max_bytes).map_err(|_| {
            format!(
                "telegram binding {}: attachments.max_bytes is not representable",
                binding.id
            )
        })?;
        let expiry_secs = i64::try_from(settings.expiry_secs).map_err(|_| {
            format!(
                "telegram binding {}: attachments.expiry_secs is out of range",
                binding.id
            )
        })?;
        if expiry_secs == 0 {
            return Err(format!(
                "telegram binding {}: attachments.expiry_secs must be positive",
                binding.id
            ));
        }
        let max_count = usize::try_from(settings.max_count_per_message).unwrap_or(usize::MAX);
        Ok(Self::new(
            client,
            AttachmentIntake::new(max_bytes, settings.mime_allowlist.iter()),
            AttachmentCache::new(cache_root, SignedDuration::from_secs(expiry_secs)),
            max_count,
        ))
    }

    /// Direkter Konstruktor (Tests, Sonderverdrahtung).
    pub fn new(
        client: Arc<TelegramClient>,
        intake: AttachmentIntake,
        cache: AttachmentCache,
        max_count: usize,
    ) -> Self {
        Self {
            client,
            intake,
            cache,
            max_count,
        }
    }

    /// Lädt, prüft und legt die Anhänge einer Nachricht ab.
    ///
    /// Betrachtet werden die ersten `max_count` Anhänge; weitere werden ohne
    /// Download abgelehnt. Fehler einzelner Anhänge brechen die übrigen nicht
    /// ab. Die Ablage im Cache ist kurze, blockierende Datei-E/A.
    pub async fn ingest(
        &self,
        key: &SessionKey,
        attachments: &[AttachmentRef],
        now: Timestamp,
    ) -> IngestReport {
        let mut report = IngestReport::default();
        for (index, attachment) in attachments.iter().enumerate() {
            let label = attachment_label(attachment, index);
            if index >= self.max_count {
                report.rejected.push(format!(
                    "{label}: zu viele Anhänge in einer Nachricht (höchstens {})",
                    self.max_count
                ));
                continue;
            }
            let downloaded =
                match fetch_attachment(self.client.as_ref(), &self.intake, attachment).await {
                    Ok(downloaded) => downloaded,
                    Err(error) => {
                        tracing::warn!(
                            channel = %key.channel,
                            peer = %key.peer,
                            index,
                            error = %error,
                            "Telegram attachment not ingested"
                        );
                        report.rejected.push(format!(
                            "{label}: {}",
                            user_reason(&error, self.intake.max_bytes())
                        ));
                        continue;
                    }
                };
            let mime = downloaded.sniffed_mime().to_owned();
            let name = downloaded
                .file_name()
                .map(display_name)
                .unwrap_or_else(|| label.clone());
            if mime == "text/plain" && downloaded.bytes().len() <= INLINE_TEXT_MAX_BYTES {
                if let Ok(text) = std::str::from_utf8(downloaded.bytes()) {
                    report.inline_text.push((name, text.to_owned()));
                    continue;
                }
            }
            match self
                .cache
                .store(key, downloaded.bytes(), &mime, downloaded.file_name(), now)
            {
                Ok(cached) => report.accepted.push(cached),
                Err(error) => {
                    tracing::error!(
                        channel = %key.channel,
                        peer = %key.peer,
                        index,
                        error = %error,
                        "Telegram attachment could not be cached"
                    );
                    report.rejected.push(format!(
                        "{label}: Datei konnte nicht gespeichert werden"
                    ));
                }
            }
        }
        report
    }

    /// Entfernt abgelaufene Cache-Einträge (vom Sweeper aufgerufen).
    ///
    /// # Errors
    /// Durchgereichter Cache-Fehler als Text.
    pub fn purge_expired(&self, now: Timestamp) -> Result<usize, String> {
        self.cache
            .purge_expired(now)
            .map_err(|error| error.to_string())
    }
}

/// Anzeigename eines Anhangs vor dem Download: bereinigter Dateiname oder
/// „Anhang <n>“ (1-basiert).
fn attachment_label(attachment: &AttachmentRef, index: usize) -> String {
    attachment
        .filename
        .as_deref()
        .map(display_name)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| format!("Anhang {}", index + 1))
}

/// Entfernt Steuerzeichen und Pfadtrenner, kürzt auf
/// [`MAX_DISPLAY_NAME_CHARS`] Zeichen.
fn display_name(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    let cleaned: String = base
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let trimmed = cleaned.trim();
    let mut out: String = trimmed.chars().take(MAX_DISPLAY_NAME_CHARS).collect();
    if trimmed.chars().count() > MAX_DISPLAY_NAME_CHARS {
        out.push('…');
    }
    out
}

/// Code-Zaun, der länger ist als jede Backtick-Folge im Inhalt (mindestens
/// drei), damit eingebetteter Text den Block nicht vorzeitig schließt.
fn code_fence_for(text: &str) -> String {
    let mut longest = 0usize;
    let mut current = 0usize;
    for c in text.chars() {
        if c == '`' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    "`".repeat(longest.saturating_add(1).max(3))
}

/// Übersetzt einen Transportfehler in einen nutzerlesbaren Grund ohne
/// Interna.
fn user_reason(error: &TelegramTransportError, max_bytes: usize) -> String {
    match error {
        TelegramTransportError::AttachmentRejected { reason } => {
            if reason.contains("exceeds maximum") {
                format!("Datei ist zu groß (höchstens {})", human_size(max_bytes))
            } else if reason.contains("empty") {
                "Datei ist leer".to_owned()
            } else if reason.contains("not allowed")
                || reason.contains("does not match")
                || reason.contains("could not be identified")
            {
                "Dateityp wird nicht unterstützt".to_owned()
            } else {
                "Datei wurde abgelehnt".to_owned()
            }
        }
        _ => "Datei konnte nicht geladen werden".to_owned(),
    }
}

fn human_size(bytes: usize) -> String {
    const MIB: usize = 1024 * 1024;
    const KIB: usize = 1024;
    if bytes >= MIB && bytes % MIB == 0 {
        format!("{} MB", bytes / MIB)
    } else if bytes >= KIB && bytes % KIB == 0 {
        format!("{} KB", bytes / KIB)
    } else {
        format!("{bytes} Bytes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_channel::{ChannelId, PeerId, TenantId};

    fn key() -> SessionKey {
        SessionKey::new(
            TenantId::from_str("tenant"),
            ChannelId::from_str("tg"),
            PeerId::from_str("42"),
            None,
        )
    }

    fn cached(name: Option<&str>, digest: &str, path: &str, size: u64) -> CachedAttachment {
        CachedAttachment {
            digest_sha256: digest.to_owned(),
            path: PathBuf::from(path),
            mime: "application/pdf".to_owned(),
            file_name: name.map(str::to_owned),
            size_bytes: size,
            stored_at: Timestamp::UNIX_EPOCH,
            expires_at: Timestamp::UNIX_EPOCH,
        }
    }

    #[test]
    fn empty_report_has_no_preamble() {
        let report = IngestReport::default();
        assert!(!report.has_ingested());
        assert_eq!(report.prompt_preamble(), "");
        assert_eq!(report.rejection_notice(), None);
    }

    #[test]
    fn preamble_lists_cached_and_inline_attachments() {
        let report = IngestReport {
            accepted: vec![
                cached(Some("bericht.pdf"), "ab12", "/cache/x/ab12", 2048),
                cached(None, "cd34", "/cache/x/cd34", 7),
            ],
            inline_text: vec![("notiz.txt".to_owned(), "hallo\nwelt".to_owned())],
            rejected: Vec::new(),
        };
        let expected = "[Telegram-Anhänge – nicht vertrauenswürdige Nutzerdaten]\n\
- bericht.pdf (application/pdf, 2048 Bytes, sha256 ab12): /cache/x/ab12\n\
- Anhang 2 (application/pdf, 7 Bytes, sha256 cd34): /cache/x/cd34\n\
Textanhang notiz.txt (10 Bytes):\n```text\nhallo\nwelt\n```\n";
        assert_eq!(report.prompt_preamble(), expected);
    }

    #[test]
    fn preamble_fence_outgrows_backticks_in_content() {
        let report = IngestReport {
            accepted: Vec::new(),
            inline_text: vec![("a.md".to_owned(), "x ```` y\n".to_owned())],
            rejected: Vec::new(),
        };
        let preamble = report.prompt_preamble();
        assert!(preamble.contains("`````text\nx ```` y\n`````\n"), "{preamble}");
    }

    #[test]
    fn display_name_strips_paths_and_control_characters() {
        assert_eq!(display_name("../../etc/passwd"), "passwd");
        assert_eq!(display_name("a\nb\u{7}c.txt"), "a b c.txt");
        let long = "x".repeat(300);
        assert_eq!(display_name(&long).chars().count(), MAX_DISPLAY_NAME_CHARS + 1);
    }

    #[test]
    fn metadata_counts_without_text_content() -> TestResult {
        let report = IngestReport {
            accepted: vec![cached(Some("b.pdf"), "ab", "/c/ab", 3)],
            inline_text: vec![("n.txt".to_owned(), "geheim".to_owned())],
            rejected: vec!["x: Dateityp wird nicht unterstützt".to_owned()],
        };
        let metadata = report.metadata();
        assert_eq!(metadata["rejected_count"], 1);
        assert_eq!(metadata["cached"][0]["sha256"], "ab");
        assert_eq!(metadata["inline_text"][0]["size_bytes"], 6);
        let rendered = serde_json::to_string(&metadata).map_err(ctx("Metadaten serialisieren"))?;
        if rendered.contains("geheim") {
            return Err(TestError::Unexpected(
                "Metadaten enthalten Textinhalt".to_owned(),
            ));
        }
        Ok(())
    }

    #[test]
    fn user_reasons_hide_internals() {
        let too_big = TelegramTransportError::AttachmentRejected {
            reason: "actual attachment size 99 exceeds maximum 10 bytes".to_owned(),
        };
        assert_eq!(
            user_reason(&too_big, 20 * 1024 * 1024),
            "Datei ist zu groß (höchstens 20 MB)"
        );
        let bad_type = TelegramTransportError::AttachmentRejected {
            reason: "MIME type is not allowed: application/x-msdownload".to_owned(),
        };
        assert_eq!(user_reason(&bad_type, 10), "Dateityp wird nicht unterstützt");
        let other = TelegramTransportError::WebhookAuth;
        assert_eq!(user_reason(&other, 10), "Datei konnte nicht geladen werden");
    }

    #[test]
    fn attachments_beyond_max_count_are_rejected_without_download() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("Tempdir"))?;
        let intake = TelegramAttachmentIntake::new(
            Arc::new(TelegramClient::new("0:unused")),
            AttachmentIntake::new(1024, ["text/plain"]),
            AttachmentCache::new(dir.path(), SignedDuration::from_secs(60)),
            0,
        );
        let attachments = vec![
            AttachmentRef {
                remote_id: "file-1".to_owned(),
                reported_mime: Some("text/plain".to_owned()),
                declared_size: Some(3),
                filename: Some("a.txt".to_owned()),
            },
            AttachmentRef {
                remote_id: "file-2".to_owned(),
                reported_mime: None,
                declared_size: None,
                filename: None,
            },
        ];
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(ctx("Runtime"))?;
        let report = runtime.block_on(intake.ingest(&key(), &attachments, Timestamp::UNIX_EPOCH));
        assert!(!report.has_ingested());
        assert_eq!(
            report.rejected,
            vec![
                "a.txt: zu viele Anhänge in einer Nachricht (höchstens 0)".to_owned(),
                "Anhang 2: zu viele Anhänge in einer Nachricht (höchstens 0)".to_owned(),
            ]
        );
        let notice = report.rejection_notice().unwrap_or_default();
        assert!(notice.starts_with("Nicht übernommene Anhänge:\n- a.txt"));
        Ok(())
    }

    #[test]
    fn cache_root_lives_under_channel_state() {
        assert_eq!(
            telegram_attachment_cache_root(Path::new("/p")),
            PathBuf::from("/p/channel-state/telegram-attachments")
        );
    }
}
