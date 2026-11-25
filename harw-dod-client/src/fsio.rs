//! Bounded, symlink-refusing file access shared by the readers.
//!
//! Every artefact this crate reads (findings export, health report) is
//! written by an unprivileged daemon from attacker-influenced observations,
//! so readers treat it as untrusted input: the path is opened exactly once
//! with `O_NOFOLLOW | O_NONBLOCK | O_NOCTTY | O_CLOEXEC` (a final symlink is
//! refused by the kernel, a FIFO swapped in cannot block the open), the
//! opened descriptor itself must be a regular file (`fstat`), and the
//! amount read is always bounded by the caller. There is no separate check
//! step, so nothing can be swapped between a check and the open.

use std::fs::File;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;

use rustix::fs::{Mode, OFlags};
use rustix::io::{Errno, retry_on_intr};

use crate::error::DodClientError;

/// A regular file opened by [`open_regular_nofollow`].
pub(crate) struct OpenedFile {
    /// The open file (blocking mode).
    pub(crate) file: File,
    /// Size at open time, in bytes.
    pub(crate) size: u64,
    /// `(st_dev, st_ino)` of the opened file.
    pub(crate) identity: (u64, u64),
}

/// Opens `path` for reading without following a final symlink.
///
/// Returns `Ok(None)` when the path does not exist.
pub(crate) fn open_regular_nofollow(path: &Path) -> Result<Option<OpenedFile>, DodClientError> {
    let flags =
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::NOCTTY | OFlags::CLOEXEC;
    let fd = match retry_on_intr(|| rustix::fs::open(path, flags, Mode::empty())) {
        Ok(fd) => fd,
        Err(Errno::NOENT) => return Ok(None),
        // `O_NOFOLLOW` on a final symlink.
        Err(Errno::LOOP) => {
            return Err(DodClientError::Symlink {
                path: path.to_path_buf(),
            });
        }
        // A socket (or a device without a driver) cannot be opened.
        Err(Errno::NXIO) => {
            return Err(DodClientError::NotRegularFile {
                path: path.to_path_buf(),
            });
        }
        Err(errno) => return Err(DodClientError::io(path, errno.into())),
    };
    let file = File::from(fd);
    // `fstat` on the opened descriptor: the check and the open name the
    // same inode by construction.
    let meta = file
        .metadata()
        .map_err(|error| DodClientError::io(path, error))?;
    if !meta.file_type().is_file() {
        return Err(DodClientError::NotRegularFile {
            path: path.to_path_buf(),
        });
    }
    // Regular files ignore `O_NONBLOCK`; clear it anyway so the descriptor
    // behaves like a plain `File::open`.
    let status =
        rustix::fs::fcntl_getfl(&file).map_err(|errno| DodClientError::io(path, errno.into()))?;
    if status.contains(OFlags::NONBLOCK) {
        rustix::fs::fcntl_setfl(&file, status.difference(OFlags::NONBLOCK))
            .map_err(|errno| DodClientError::io(path, errno.into()))?;
    }
    Ok(Some(OpenedFile {
        size: meta.len(),
        identity: (meta.dev(), meta.ino()),
        file,
    }))
}

/// `true` for characters that must not reach a terminal or a model prompt
/// verbatim.
///
/// Delegates to [`harw_untrusted_text::needs_escape`], the one shared
/// classification (PL-100 WP0.8) the sentinel export and the `dod`
/// operations use as well: controls (`Cc`, including the ESC/BEL of
/// ANSI/OSC sequences), format characters (`Cf`: bidi overrides/isolates,
/// zero-width characters, BOM, soft hyphen), line/paragraph separators,
/// private use, the tag block, variation selectors and an approximation of
/// unassigned code points. A superset of the former local list.
pub(crate) fn is_unsafe_char(ch: char) -> bool {
    harw_untrusted_text::needs_escape(ch)
}

/// Replaces unsafe characters ([`is_unsafe_char`]) by `replacement` and
/// truncates to at most `max_bytes` bytes on a character boundary.
///
/// The readers keep a one-character replacement instead of the visible
/// `\u{…}` escape of [`harw_untrusted_text::escape`]: every caller bounds
/// its field in bytes, and one replacement per unsafe character keeps those
/// byte caps exact.
pub(crate) fn sanitize(raw: &str, max_bytes: usize, replacement: char) -> String {
    let mut out = String::with_capacity(raw.len().min(max_bytes));
    for ch in raw.chars() {
        let ch = if is_unsafe_char(ch) { replacement } else { ch };
        if out.len().saturating_add(ch.len_utf8()) > max_bytes {
            break;
        }
        out.push(ch);
    }
    out
}

/// `true` when `value` is non-empty, at most `max_bytes` long and free of
/// unsafe characters ([`harw_untrusted_text::is_clean`]).
pub(crate) fn is_clean(value: &str, max_bytes: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max_bytes && harw_untrusted_text::is_clean(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustix::fs::FileType;
    use std::fs;
    use std::os::unix::fs::symlink;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn sanitize_replaces_controls_and_bidi_and_truncates_on_char_boundary() {
        assert_eq!(sanitize("a\nb\u{202E}c", 64, ' '), "a b c");
        assert_eq!(sanitize("äää", 5, ' '), "ää");
        assert!(is_clean("host-a", 16));
        assert!(!is_clean("host\u{7}", 16));
        assert!(!is_clean("   ", 16));
        assert!(!is_clean("abcdef", 3));
    }

    #[test]
    fn sanitize_and_is_clean_follow_the_shared_escaper_classification() -> TestResult {
        // Tag block, variation selector, private use, soft hyphen and an
        // ANSI escape byte: replaced like any other unsafe character.
        assert_eq!(
            sanitize("a\u{E0041}b\u{FE0F}c\u{E000}d\u{AD}e\u{1B}[0m", 64, '?'),
            "a?b?c?d?e?[0m"
        );
        assert!(!is_clean("host\u{E0041}", 16));
        assert!(!is_clean("id\u{200B}x", 16));
        assert!(!is_clean("id\u{FE0F}", 16));
        assert!(is_clean("Übersicht", 16));
        // Quotes and backslashes are visible and stay.
        assert_eq!(sanitize(r#"say "hi" \o/"#, 64, ' '), r#"say "hi" \o/"#);
        // The byte cap still holds after the replacement, on a char boundary.
        assert_eq!(sanitize("\u{E0041}äb", 3, '?'), "?ä");
        // Every character the shared escaper rewrites is unsafe here too.
        for ch in ['\u{202E}', '\u{2066}', '\u{2028}', '\u{E0100}', '\u{FFFE}'] {
            assert_eq!(is_unsafe_char(ch), harw_untrusted_text::needs_escape(ch));
            assert!(is_unsafe_char(ch), "{ch:?}");
        }
        Ok(())
    }

    #[test]
    fn open_refuses_symlinks_and_directories_and_reports_missing() -> TestResult {
        let dir = tempfile::tempdir()?;
        let target = dir.path().join("target");
        fs::write(&target, b"x")?;
        let link = dir.path().join("link");
        symlink(&target, &link)?;
        assert!(matches!(
            open_regular_nofollow(&link),
            Err(DodClientError::Symlink { .. })
        ));
        assert!(matches!(
            open_regular_nofollow(dir.path()),
            Err(DodClientError::NotRegularFile { .. })
        ));
        assert!(open_regular_nofollow(&dir.path().join("missing"))?.is_none());
        let opened = open_regular_nofollow(&target)?.ok_or("regular file must open")?;
        assert_eq!(opened.size, 1);
        Ok(())
    }

    #[test]
    fn open_refuses_a_fifo_without_blocking() -> TestResult {
        let dir = tempfile::tempdir()?;
        let fifo = dir.path().join("findings.jsonl");
        rustix::fs::mknodat(
            rustix::fs::CWD,
            &fifo,
            FileType::Fifo,
            Mode::from_raw_mode(0o600),
            0,
        )?;
        // No writer ever opens the FIFO: a blocking open would hang here.
        assert!(matches!(
            open_regular_nofollow(&fifo),
            Err(DodClientError::NotRegularFile { .. })
        ));
        Ok(())
    }
}
