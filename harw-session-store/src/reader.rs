//! Sequential JSONL reader/iterator over a session transcript.
//!
//! Spec: `docs/design/knowledge-surfaces.md` §1.4 — the read pattern is
//! sequential replay by session. This reader is fully implemented on `std`
//! (`BufReader::read_line`); it needs neither `fs4` nor `tempfile`. Blank lines
//! are skipped; every newline-terminated non-blank line is decoded into a
//! `TranscriptRecord`.

use std::fs::File;
#[cfg(not(unix))]
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader};
use std::path::Path;

use crate::error::{SessionStoreError, SessionStoreResult};
use crate::record::TranscriptRecord;

const MAX_RECORD_BYTES: usize = 1024 * 1024;

/// Streaming, non-buffering iterator over a transcript file's records.
pub struct TranscriptReader {
    /// Buffered transcript stream, retained so record terminators can be checked.
    reader: BufReader<File>,
}

impl TranscriptReader {
    /// Opens a transcript file for sequential replay.
    pub fn open(path: &Path) -> SessionStoreResult<Self> {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(symlink_error(path));
            }
            Ok(_) => {}
            Err(error) => return Err(SessionStoreError::Io(error)),
        }

        // F-006: das architekturabhängig falsche `O_NOFOLLOW` wurde durch
        // `harw_fsutil::open_nofollow` ersetzt (plattformkorrekt über
        // `rustix::fs::OFlags::NOFOLLOW`).
        #[cfg(unix)]
        let file =
            harw_fsutil::open_nofollow(path, harw_fsutil::OpenMode::read_only())
                .map_err(SessionStoreError::Io)?;
        #[cfg(not(unix))]
        let file = {
            let mut options = OpenOptions::new();
            options.read(true);
            options.open(path).map_err(SessionStoreError::Io)?
        };
        Ok(Self {
            reader: BufReader::new(file),
        })
    }

    fn read_line_bounded(&mut self) -> SessionStoreResult<Option<String>> {
        let mut bytes = Vec::new();

        loop {
            let buffer = self.reader.fill_buf()?;
            if buffer.is_empty() {
                return if bytes.is_empty() {
                    Ok(None)
                } else {
                    String::from_utf8(bytes).map(Some).map_err(|error| {
                        SessionStoreError::from(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            error,
                        ))
                    })
                };
            }

            let newline = buffer.iter().position(|byte| *byte == b'\n');
            let consumed = newline.map_or(buffer.len(), |index| index + 1);
            if bytes.len().saturating_add(consumed) > MAX_RECORD_BYTES {
                return Err(SessionStoreError::CorruptRecord {
                    detail: format!(
                        "transcript record exceeds maximum size of {MAX_RECORD_BYTES} bytes"
                    ),
                });
            }

            bytes.extend_from_slice(&buffer[..consumed]);
            self.reader.consume(consumed);
            if newline.is_some() {
                return String::from_utf8(bytes).map(Some).map_err(|error| {
                    SessionStoreError::from(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        error,
                    ))
                });
            }
        }
    }
}

fn symlink_error(path: &Path) -> SessionStoreError {
    SessionStoreError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!("transcript must not be a symbolic link: {}", path.display()),
    ))
}

impl Iterator for TranscriptReader {
    type Item = SessionStoreResult<TranscriptRecord>;

    /// Yields the next decoded record, skipping blank lines, or `None` at EOF.
    ///
    /// A non-blank record must end in a newline. This prevents an interrupted
    /// append whose bytes happen to form valid JSON from being replayed as a
    /// durable record.
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.read_line_bounded() {
                Err(error) => return Some(Err(error)),
                Ok(None) => return None,
                Ok(Some(line)) if line.trim().is_empty() => continue,
                Ok(Some(line)) if !line.ends_with('\n') => {
                    return Some(Err(SessionStoreError::CorruptRecord {
                        detail: "unterminated final JSONL record".to_owned(),
                    }));
                }
                Ok(Some(line)) => return Some(TranscriptRecord::from_jsonl_line(&line)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use harw_types::{SessionId, ThreadRef};

    use super::TranscriptReader;
    use crate::error::SessionStoreError;
    use crate::record::{RecordKind, TranscriptRecord};

    fn record() -> TranscriptRecord {
        TranscriptRecord::new(
            SessionId::from_str("session-a"),
            ThreadRef::from_str("root"),
            0,
            jiff::Timestamp::now(),
            RecordKind::Turn,
            serde_json::json!({ "message": "durable" }),
        )
    }

    #[test]
    fn rejects_unterminated_final_record_even_when_json_is_complete() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        let line = record().to_jsonl_line().unwrap();
        file.write_all(line.trim_end_matches('\n').as_bytes())
            .unwrap();

        let error = TranscriptReader::open(file.path())
            .unwrap()
            .next()
            .expect("the partial record must be observed")
            .unwrap_err();

        assert!(matches!(
            error,
            SessionStoreError::CorruptRecord { detail }
                if detail == "unterminated final JSONL record"
        ));
    }

    #[test]
    fn rejects_a_record_that_exceeds_the_maximum_size() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&vec![b' '; super::MAX_RECORD_BYTES])
            .unwrap();
        file.write_all(b"{}\n").unwrap();

        let error = TranscriptReader::open(file.path())
            .unwrap()
            .next()
            .expect("the oversized record must be observed")
            .unwrap_err();

        assert!(matches!(
            error,
            SessionStoreError::CorruptRecord { detail }
                if detail == format!(
                    "transcript record exceeds maximum size of {} bytes",
                    super::MAX_RECORD_BYTES
                )
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_without_reading_target() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("outside-transcript.jsonl");
        let link = temp.path().join("session.jsonl");
        std::fs::write(&target, b"not a transcript\n").unwrap();
        symlink(&target, &link).unwrap();

        let error = match TranscriptReader::open(&link) {
            Err(error) => error,
            Ok(_) => panic!("expected a symlink rejection"),
        };

        match error {
            SessionStoreError::Io(error) => {
                assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
            }
            error => panic!("expected a symlink rejection, got {error:?}"),
        }
        assert_eq!(
            std::fs::read_to_string(target).unwrap(),
            "not a transcript\n"
        );
    }
}
