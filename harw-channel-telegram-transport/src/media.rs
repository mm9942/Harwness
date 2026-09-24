//! Attachment intake for Telegram ingress.
//!
//! [`AttachmentIntake`] owns the checks that must happen before and after a
//! download; [`fetch_attachment`] chains them around the Telegram file API
//! (deklarierte Größe → `getFile` → gemeldete Dateigröße → begrenzter
//! Download → Inhaltsprüfung).
//!
//! MIME-Allowlists akzeptieren exakte Typen (`image/png`), Wildcards
//! (`image/*`, `*/*`) und gängige Aliasse (`image/jpg` → `image/jpeg`);
//! Parameter wie `; charset=utf-8` werden ignoriert. Inhalte, die keine
//! Magic-Bytes tragen, gelten als `text/plain`, sofern sie gültiges UTF-8 ohne
//! NUL-Bytes sind.

use std::collections::BTreeSet;

use harw_channel::AttachmentRef;

use crate::{
    client::TelegramClient,
    error::{TelegramTransportError, TransportResult},
};

/// MIME-Typ für Inhalte ohne Magic-Bytes, die als Text erkannt wurden.
const TEXT_PLAIN: &str = "text/plain";

/// Lädt einen Telegram-Anhang herunter und prüft ihn gegen `intake`.
///
/// Reihenfolge: (1) deklarierte Größe aus dem Update, (2) gemeldeter MIME-Typ
/// als frühe Vorprüfung (nur zum Sparen des Downloads, nie als Typquelle),
/// (3) `getFile`, (4) dort gemeldete `file_size`, (5) Download mit hartem
/// Byte-Limit, (6) [`AttachmentIntake::validate`] auf den echten Bytes.
///
/// # Errors
/// [`TelegramTransportError::AttachmentRejected`] bei Größen- oder
/// Typverstößen, sonst Transport-/API-Fehler des Clients.
pub async fn fetch_attachment(
    client: &TelegramClient,
    intake: &AttachmentIntake,
    attachment: &AttachmentRef,
) -> TransportResult<DownloadedAttachment> {
    intake.validate_declared_size(attachment.declared_size)?;
    if let Some(reported) = attachment.reported_mime.as_deref() {
        if !intake.may_accept_reported(reported) {
            return Err(rejected(format!(
                "reported MIME type is not allowed: {}",
                normalize_mime(reported)
            )));
        }
    }
    let file = client.get_file(&attachment.remote_id).await?;
    intake.validate_declared_size(file.file_size)?;
    let bytes = client.download_file(&file, intake.max_bytes()).await?;
    intake.validate(
        attachment.declared_size,
        attachment.reported_mime.as_deref(),
        attachment.filename.as_deref(),
        bytes,
    )
}

/// Policy applied to an inbound Telegram attachment.
#[derive(Clone, Debug)]
pub struct AttachmentIntake {
    max_bytes: usize,
    allowed_mime_types: BTreeSet<String>,
}

impl AttachmentIntake {
    /// Build an intake policy from a byte ceiling and an allowlist of MIME
    /// types. MIME values are normalized to trimmed lowercase strings.
    pub fn new<I, S>(max_bytes: usize, allowed_mime_types: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self {
            max_bytes,
            allowed_mime_types: allowed_mime_types
                .into_iter()
                .map(|mime| normalize_mime(mime.as_ref()))
                .collect(),
        }
    }

    /// Ob `mime` (normalisiert, Aliasse aufgelöst) von der Allowlist erfasst
    /// wird – exakt oder über eine Wildcard `type/*` bzw. `*/*`.
    #[must_use]
    pub fn allows(&self, mime: &str) -> bool {
        let mime = normalize_mime(mime);
        let Some((top_level, _)) = mime.split_once('/') else {
            return false;
        };
        self.allowed_mime_types.iter().any(|allowed| {
            if allowed == &mime || allowed == "*/*" {
                return true;
            }
            allowed
                .strip_suffix("/*")
                .is_some_and(|allowed_top| allowed_top == top_level)
        })
    }

    /// Vorprüfung des untrusted gemeldeten Typs vor dem Download. Textartige
    /// Meldungen werden durchgelassen, solange `text/plain` erlaubt ist, weil
    /// die Inhaltsprüfung sie ohnehin als `text/plain` einstuft.
    fn may_accept_reported(&self, reported: &str) -> bool {
        self.allows(reported)
            || (is_text_mime(&normalize_mime(reported)) && self.allows(TEXT_PLAIN))
    }

    /// The configured maximum attachment size in bytes.
    pub const fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    /// Check Telegram's declared size before issuing a file download.
    pub fn validate_declared_size(&self, declared_size: Option<u64>) -> TransportResult<()> {
        if let Some(declared_size) = declared_size {
            self.check_size(declared_size, "declared")?;
        }
        Ok(())
    }

    /// Validate Telegram metadata before downloading, then validate the
    /// downloaded content using magic-byte MIME sniffing.
    ///
    /// `reported_mime` is treated as an untrusted claim. It is checked against
    /// the sniffed value, but is never used to determine the attachment type.
    /// `file_name` is optional untrusted metadata and is reduced to a single,
    /// non-control path component before being retained.
    pub fn validate(
        &self,
        declared_size: Option<u64>,
        reported_mime: Option<&str>,
        file_name: Option<&str>,
        bytes: Vec<u8>,
    ) -> TransportResult<DownloadedAttachment> {
        self.validate_declared_size(declared_size)?;

        if bytes.is_empty() {
            return Err(rejected("attachment is empty"));
        }
        self.check_size(bytes.len() as u64, "actual")?;

        let sniffed_mime = match infer::get(&bytes) {
            Some(kind) => normalize_mime(kind.mime_type()),
            None if is_plain_text(&bytes) => TEXT_PLAIN.to_owned(),
            None => return Err(rejected("content type could not be identified")),
        };

        if let Some(reported_mime) = reported_mime {
            let reported = normalize_mime(reported_mime);
            // Text-Untertypen (text/markdown, text/x-python, …) sind nur
            // Etiketten für denselben Inhalt; maßgeblich bleibt der
            // erkannte Typ.
            let both_text = is_text_mime(&reported) && is_text_mime(&sniffed_mime);
            if reported != sniffed_mime && !both_text {
                return Err(rejected(format!(
                    "reported MIME type does not match content ({reported:?} vs {sniffed_mime:?})"
                )));
            }
        }

        if !self.allows(&sniffed_mime) {
            return Err(rejected(format!(
                "MIME type is not allowed: {sniffed_mime}"
            )));
        }

        Ok(DownloadedAttachment {
            bytes,
            sniffed_mime,
            file_name: file_name.and_then(sanitize_file_name),
        })
    }

    fn check_size(&self, size: u64, label: &str) -> TransportResult<()> {
        if size > self.max_bytes as u64 {
            return Err(rejected(format!(
                "{label} attachment size {size} exceeds maximum {} bytes",
                self.max_bytes
            )));
        }
        Ok(())
    }
}

/// Attachment content that passed the intake policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadedAttachment {
    bytes: Vec<u8>,
    sniffed_mime: String,
    file_name: Option<String>,
}

impl DownloadedAttachment {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    pub fn sniffed_mime(&self) -> &str {
        &self.sniffed_mime
    }

    pub fn file_name(&self) -> Option<&str> {
        self.file_name.as_deref()
    }
}

/// Kleinbuchstaben, ohne Parameter (`; charset=…`), Aliasse aufgelöst.
fn normalize_mime(mime: &str) -> String {
    let essence = mime
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    match essence.as_str() {
        "image/jpg" | "image/pjpeg" => "image/jpeg".to_owned(),
        "application/x-pdf" => "application/pdf".to_owned(),
        _ => essence,
    }
}

fn is_text_mime(mime: &str) -> bool {
    mime.starts_with("text/")
}

/// Gültiges UTF-8 ohne NUL-Bytes.
fn is_plain_text(bytes: &[u8]) -> bool {
    !bytes.contains(&0) && std::str::from_utf8(bytes).is_ok()
}

fn sanitize_file_name(file_name: &str) -> Option<String> {
    let name = file_name.rsplit(['/', '\\']).next()?.trim();
    if name.is_empty() || name.chars().any(char::is_control) {
        return None;
    }
    Some(name.to_owned())
}

fn rejected(reason: impl Into<String>) -> TelegramTransportError {
    TelegramTransportError::AttachmentRejected {
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    const PDF: &[u8] = b"%PDF-1.7\n";

    fn intake(max_bytes: usize, mime: &'static str) -> AttachmentIntake {
        AttachmentIntake::new(max_bytes, [mime])
    }

    #[test]
    fn accepts_signature_and_keeps_safe_metadata() -> TestResult {
        let attachment = intake(64, "image/png")
            .validate(
                Some(PNG.len() as u64),
                Some(" IMAGE/PNG "),
                Some("/tmp/telegram/image.png"),
                PNG.to_vec(),
            )
            .map_err(ctx("PNG signature should be accepted"))?;

        assert_eq!(attachment.bytes(), PNG);
        assert_eq!(attachment.sniffed_mime(), "image/png");
        assert_eq!(attachment.file_name(), Some("image.png"));
        Ok(())
    }

    #[test]
    fn rejects_declared_oversize_before_content_validation() -> TestResult {
        let Err(error) = intake(PNG.len(), "image/png").validate(
            Some((PNG.len() + 1) as u64),
            None,
            None,
            Vec::new(),
        ) else {
            return Err(TestError::Unexpected(
                "declared oversize must be rejected before download".to_owned(),
            ));
        };

        assert!(matches!(
            error,
            TelegramTransportError::AttachmentRejected { .. }
        ));
        Ok(())
    }

    #[test]
    fn rejects_actual_oversize_after_download() -> TestResult {
        let Err(error) = intake(PNG.len() - 1, "image/png").validate(
            Some((PNG.len() - 1) as u64),
            None,
            None,
            PNG.to_vec(),
        ) else {
            return Err(TestError::Unexpected(
                "actual oversize must be rejected".to_owned(),
            ));
        };

        assert!(matches!(
            error,
            TelegramTransportError::AttachmentRejected { .. }
        ));
        Ok(())
    }

    #[test]
    fn rejects_reported_mime_mismatch() -> TestResult {
        let Err(error) = intake(64, "image/png").validate(
            Some(PDF.len() as u64),
            Some("image/png"),
            None,
            PDF.to_vec(),
        ) else {
            return Err(TestError::Unexpected(
                "reported MIME must not override sniffed MIME".to_owned(),
            ));
        };

        assert!(matches!(
            error,
            TelegramTransportError::AttachmentRejected { .. }
        ));
        Ok(())
    }

    #[test]
    fn rejects_sniffed_mime_outside_allowlist() -> TestResult {
        let Err(error) = intake(64, "image/png").validate(
            Some(PDF.len() as u64),
            Some("application/pdf"),
            None,
            PDF.to_vec(),
        ) else {
            return Err(TestError::Unexpected(
                "sniffed MIME must be allowlisted".to_owned(),
            ));
        };

        assert!(matches!(
            error,
            TelegramTransportError::AttachmentRejected { .. }
        ));
        Ok(())
    }

    const JPEG: &[u8] = &[0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10, b'J', b'F', b'I', b'F'];

    fn attachment_ref(declared_size: Option<u64>, reported: Option<&str>) -> AttachmentRef {
        AttachmentRef {
            remote_id: "file-id".to_owned(),
            reported_mime: reported.map(str::to_owned),
            declared_size,
            filename: None,
        }
    }

    #[test]
    fn allows_supports_wildcards_aliases_and_parameters() {
        let intake = AttachmentIntake::new(64, ["image/*", "application/pdf", "TEXT/PLAIN"]);
        assert!(intake.allows("image/png"));
        assert!(intake.allows("image/jpg"));
        assert!(intake.allows(" Image/WebP "));
        assert!(intake.allows("application/x-pdf"));
        assert!(intake.allows("text/plain; charset=utf-8"));
        assert!(!intake.allows("text/html"));
        assert!(!intake.allows("imagex/png"));
        assert!(!intake.allows("image"));
        assert!(!intake.allows("video/mp4"));

        let everything = AttachmentIntake::new(64, ["*/*"]);
        assert!(everything.allows("video/mp4"));
        assert!(!everything.allows("garbage"));

        let exact = AttachmentIntake::new(64, ["image/jpg"]);
        assert!(exact.allows("image/jpeg"));
    }

    #[test]
    fn jpeg_alias_matches_sniffed_type() -> TestResult {
        let attachment = AttachmentIntake::new(64, ["image/*"])
            .validate(None, Some("image/jpg"), Some("foto.jpg"), JPEG.to_vec())
            .map_err(ctx("image/jpg muss als image/jpeg gelten"))?;
        assert_eq!(attachment.sniffed_mime(), "image/jpeg");
        Ok(())
    }

    #[test]
    fn utf8_without_signature_falls_back_to_text_plain() -> TestResult {
        let intake = intake(64, "text/plain");
        let attachment = intake
            .validate(
                None,
                Some("text/markdown"),
                Some("notiz.md"),
                "# Hallo Wält\n".as_bytes().to_vec(),
            )
            .map_err(ctx("UTF-8-Text muss als text/plain gelten"))?;
        assert_eq!(attachment.sniffed_mime(), "text/plain");

        let plain = intake
            .validate(
                None,
                Some("text/plain; charset=utf-8"),
                None,
                b"abc".to_vec(),
            )
            .map_err(ctx("text/plain mit Parameter muss passen"))?;
        assert_eq!(plain.sniffed_mime(), "text/plain");
        Ok(())
    }

    #[test]
    fn text_fallback_rejects_nul_bytes_invalid_utf8_and_foreign_claims() {
        let intake = intake(64, "text/plain");
        assert!(
            intake
                .validate(None, None, None, b"ab\0cd".to_vec())
                .is_err()
        );
        assert!(
            intake
                .validate(None, None, None, vec![0xff, 0xfe, 0x41])
                .is_err()
        );
        assert!(
            intake
                .validate(None, Some("image/png"), None, b"not a png".to_vec())
                .is_err()
        );
        assert!(
            AttachmentIntake::new(64, ["image/*"])
                .validate(None, None, None, b"plain".to_vec())
                .is_err()
        );
    }

    #[test]
    fn reported_precheck_lets_text_labels_through_only_with_text_plain() {
        let with_text = AttachmentIntake::new(64, ["image/*", "text/plain"]);
        assert!(with_text.may_accept_reported("text/markdown"));
        assert!(with_text.may_accept_reported("image/jpg"));
        assert!(!with_text.may_accept_reported("video/mp4"));
        let images_only = AttachmentIntake::new(64, ["image/*"]);
        assert!(!images_only.may_accept_reported("text/markdown"));
    }

    #[tokio::test]
    async fn fetch_rejects_declared_oversize_before_any_request() -> TestResult {
        let client = TelegramClient::new("test-token");
        let Err(error) = fetch_attachment(
            &client,
            &intake(4, "image/png"),
            &attachment_ref(Some(5), Some("image/png")),
        )
        .await
        else {
            return Err(TestError::Unexpected(
                "deklarierte Übergröße muss vor getFile scheitern".to_owned(),
            ));
        };
        assert!(matches!(
            error,
            TelegramTransportError::AttachmentRejected { .. }
        ));
        Ok(())
    }

    #[tokio::test]
    async fn fetch_rejects_disallowed_reported_type_before_any_request() -> TestResult {
        let client = TelegramClient::new("test-token");
        let Err(error) = fetch_attachment(
            &client,
            &intake(64, "image/png"),
            &attachment_ref(Some(3), Some("video/mp4")),
        )
        .await
        else {
            return Err(TestError::Unexpected(
                "nicht erlaubter gemeldeter Typ muss vor getFile scheitern".to_owned(),
            ));
        };
        assert!(matches!(
            error,
            TelegramTransportError::AttachmentRejected { .. }
        ));
        Ok(())
    }

    #[test]
    fn rejects_empty_attachment() -> TestResult {
        let Err(error) = intake(64, "image/png").validate(Some(0), None, None, Vec::new()) else {
            return Err(TestError::Unexpected(
                "empty attachments must be rejected".to_owned(),
            ));
        };

        assert!(matches!(
            error,
            TelegramTransportError::AttachmentRejected { .. }
        ));
        Ok(())
    }
}
