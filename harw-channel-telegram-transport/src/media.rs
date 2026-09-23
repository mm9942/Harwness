//! Attachment intake for Telegram ingress.
//!
//! This module deliberately stops at the downloaded-bytes boundary.  The
//! ingress implementation is responsible for obtaining the bytes from the
//! Telegram file API; this type owns the checks that must happen before and
//! after that operation.

use std::collections::BTreeSet;

use crate::error::{TelegramTransportError, TransportResult};

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

        let sniffed_mime = infer::get(&bytes)
            .map(|kind| kind.mime_type().to_owned())
            .ok_or_else(|| rejected("content type could not be identified"))?;

        if let Some(reported_mime) = reported_mime {
            if normalize_mime(reported_mime) != sniffed_mime {
                return Err(rejected(format!(
                    "reported MIME type does not match content ({reported_mime:?} vs {sniffed_mime:?})"
                )));
            }
        }

        if !self.allowed_mime_types.contains(&sniffed_mime) {
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

fn normalize_mime(mime: &str) -> String {
    mime.trim().to_ascii_lowercase()
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
