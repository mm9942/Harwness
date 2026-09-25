//! Payload path normalization.
//!
//! A payload path names a payload inside the artifact, never a file on the
//! building machine. It is relative, uses `/` as its only separator and can
//! never climb out of the artifact root, so a runner that materializes
//! payloads under a directory cannot be steered elsewhere.

use crate::error::{ArtifactError, Limit, PathRejection};

/// Maximum length in bytes of a payload path (before and after normalization).
pub const MAX_PATH_LEN: usize = 512;

/// Normalizes a payload path.
///
/// Empty and `.` segments are dropped (`./a//b` becomes `a/b`). Everything
/// else that could be ambiguous is rejected instead of rewritten.
///
/// # Errors
/// [`ArtifactError::InvalidPath`] when the path is absolute (leading `/`,
/// a Windows drive such as `C:`), contains a `..` segment, a backslash or a
/// control character, or is empty after normalization;
/// [`ArtifactError::LimitExceeded`] when it is longer than [`MAX_PATH_LEN`].
///
/// # Examples
/// ```
/// # use harw_agent_artifact::normalize_path;
/// assert_eq!(normalize_path("./skills//review.md").ok().as_deref(), Some("skills/review.md"));
/// assert!(normalize_path("../etc/passwd").is_err());
/// assert!(normalize_path("/etc/passwd").is_err());
/// ```
pub fn normalize_path(path: &str) -> Result<String, ArtifactError> {
    let reject = |reason| ArtifactError::InvalidPath {
        path: path.to_owned(),
        reason,
    };
    if path.len() > MAX_PATH_LEN {
        return Err(ArtifactError::LimitExceeded {
            limit: Limit::PathLen,
            actual: path.len() as u64,
            max: MAX_PATH_LEN as u64,
        });
    }
    if path.chars().any(char::is_control) {
        return Err(reject(PathRejection::ControlCharacter));
    }
    if path.contains('\\') {
        return Err(reject(PathRejection::Backslash));
    }
    if path.starts_with('/') || has_drive_prefix(path) {
        return Err(reject(PathRejection::Absolute));
    }
    let mut segments = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => return Err(reject(PathRejection::ParentSegment)),
            other => segments.push(other),
        }
    }
    if segments.is_empty() {
        return Err(reject(PathRejection::Empty));
    }
    Ok(segments.join("/"))
}

/// Checks that a path read from an artifact is already normalized.
///
/// # Errors
/// Whatever [`normalize_path`] rejects, plus
/// [`PathRejection::NotNormalized`] when normalization would change it.
pub(crate) fn require_normalized(path: &str) -> Result<(), ArtifactError> {
    if normalize_path(path)? == path {
        Ok(())
    } else {
        Err(ArtifactError::InvalidPath {
            path: path.to_owned(),
            reason: PathRejection::NotNormalized,
        })
    }
}

/// `C:` / `c:foo`: a Windows drive-relative or drive-absolute path.
fn has_drive_prefix(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

#[cfg(test)]
mod tests {
    use super::{MAX_PATH_LEN, normalize_path, require_normalized};
    use crate::error::{ArtifactError, PathRejection};
    use crate::test_support::{TestResult, ctx};

    fn rejection(path: &str) -> Option<PathRejection> {
        match normalize_path(path) {
            Err(ArtifactError::InvalidPath { reason, .. }) => Some(reason),
            _ => None,
        }
    }

    #[test]
    fn test_normalize_drops_dot_and_empty_segments() -> TestResult {
        let normalized = normalize_path("./skills//a/./b.md").map_err(ctx("normalizes"))?;
        assert_eq!(normalized, "skills/a/b.md");
        Ok(())
    }

    #[test]
    fn test_normalize_rejects_parent_segments() {
        assert_eq!(rejection(".."), Some(PathRejection::ParentSegment));
        assert_eq!(rejection("../x"), Some(PathRejection::ParentSegment));
        assert_eq!(rejection("a/../b"), Some(PathRejection::ParentSegment));
        assert_eq!(rejection("a/b/.."), Some(PathRejection::ParentSegment));
    }

    #[test]
    fn test_normalize_rejects_absolute_paths() {
        assert_eq!(rejection("/etc/passwd"), Some(PathRejection::Absolute));
        assert_eq!(rejection("//server/share"), Some(PathRejection::Absolute));
        assert_eq!(rejection("C:/Windows"), Some(PathRejection::Absolute));
        assert_eq!(rejection("c:foo"), Some(PathRejection::Absolute));
    }

    #[test]
    fn test_normalize_rejects_backslash_control_and_empty() {
        assert_eq!(rejection("a\\b"), Some(PathRejection::Backslash));
        assert_eq!(rejection("a\0b"), Some(PathRejection::ControlCharacter));
        assert_eq!(rejection("a\nb"), Some(PathRejection::ControlCharacter));
        assert_eq!(rejection(""), Some(PathRejection::Empty));
        assert_eq!(rejection("./."), Some(PathRejection::Empty));
    }

    #[test]
    fn test_normalize_rejects_overlong_path() {
        let long = "a".repeat(MAX_PATH_LEN + 1);
        assert!(matches!(
            normalize_path(&long),
            Err(ArtifactError::LimitExceeded { .. })
        ));
    }

    #[test]
    fn test_normalize_keeps_dotted_names() -> TestResult {
        // `...` and `.hidden` are names, not navigation.
        let normalized = normalize_path(".hidden/.../x..y").map_err(ctx("normalizes"))?;
        assert_eq!(normalized, ".hidden/.../x..y");
        Ok(())
    }

    #[test]
    fn test_require_normalized_rejects_unnormalized_stored_path() {
        assert!(matches!(
            require_normalized("a//b"),
            Err(ArtifactError::InvalidPath {
                reason: PathRejection::NotNormalized,
                ..
            })
        ));
        assert!(require_normalized("a/b").is_ok());
    }
}
