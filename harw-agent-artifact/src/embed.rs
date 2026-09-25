//! Embedding an artifact in an executable and finding it again.
//!
//! A built agent binary is `runner bytes ‖ artifact ‖ footer`. The footer is
//! the last [`FOOTER_LEN`] bytes of the file, so a runner finds its artifact
//! from the file end without knowing its own length at compile time:
//!
//! | Offset | Size | Field |
//! |---|---|---|
//! | 0 | 8 | `artifact_offset` (u64 LE): file offset of the artifact's first byte |
//! | 8 | 8 | `artifact_len` (u64 LE): artifact length in bytes |
//! | 16 | 32 | `executable_hash`: BLAKE3 of runner, artifact, offset and length |
//! | 48 | 8 | `footer_magic`: ASCII `HARWAEN2` |
//!
//! Extraction is fail-closed: no footer magic means
//! [`ArtifactError::NotEmbedded`]; an offset/length that does not end exactly
//! at the footer, or an executable hash mismatch, means
//! [`ArtifactError::Tampered`] with [`TamperScope::Footer`]. The old
//! `HARWAEND` footer is rejected: rebuild to cover the runner bytes too.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::artifact::{Artifact, HASH_LEN, MAX_ARTIFACT_LEN};
use crate::error::{ArtifactError, Limit, TamperScope};

/// Last eight bytes of a binary that carries an artifact.
pub const FOOTER_MAGIC: &[u8; 8] = b"HARWAEN2";
/// Size of the executable footer in bytes.
pub const FOOTER_LEN: usize = 8 + 8 + HASH_LEN + 8;

/// Returns `runner_bytes ‖ artifact ‖ footer`.
///
/// # Examples
/// ```
/// # use harw_agent_artifact::{ArtifactBuilder, EmbeddedArtifact, append_to_executable};
/// # fn main() -> Result<(), harw_agent_artifact::ArtifactError> {
/// let artifact = ArtifactBuilder::new(&serde_json::json!({"name": "demo"})).build()?;
/// let binary = append_to_executable(b"fake runner", &artifact);
/// let embedded = EmbeddedArtifact::from_executable_bytes(&binary)?;
/// assert_eq!(embedded.artifact().digest(), artifact.digest());
/// # Ok(())
/// # }
/// ```
#[must_use]
pub fn append_to_executable(runner_bytes: &[u8], artifact: &Artifact) -> Vec<u8> {
    let encoded = artifact.as_bytes();
    let mut out = Vec::with_capacity(runner_bytes.len() + encoded.len() + FOOTER_LEN);
    out.extend_from_slice(runner_bytes);
    out.extend_from_slice(encoded);
    out.extend_from_slice(&(runner_bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(&(encoded.len() as u64).to_le_bytes());
    let executable_hash = blake3::hash(&out);
    out.extend_from_slice(executable_hash.as_bytes());
    out.extend_from_slice(FOOTER_MAGIC);
    out
}

/// Writes a built binary to `path` and makes it executable (mode `0o755` on
/// unix; elsewhere the file is written as is).
///
/// # Errors
/// [`ArtifactError::Io`] when creating, writing, syncing or chmod-ing fails.
pub fn write_executable(path: &Path, bytes: &[u8]) -> Result<(), ArtifactError> {
    let io =
        |context: &'static str| move |source: std::io::Error| ArtifactError::Io { context, source };
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o755);
    }
    let mut file = options.open(path).map_err(io("create executable file"))?;
    file.write_all(bytes).map_err(io("write executable file"))?;
    file.sync_all().map_err(io("sync executable file"))?;
    // `mode` above only applies when the file is created (and is masked by
    // the umask); set it explicitly for an existing file too.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .map_err(io("set executable permissions"))?;
    }
    Ok(())
}

/// A verified artifact found at the end of an executable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedArtifact {
    artifact: Artifact,
    runner_len: u64,
}

/// The decoded footer.
struct Footer {
    offset: u64,
    len: usize,
    hash: [u8; HASH_LEN],
}

impl EmbeddedArtifact {
    /// Locates, verifies and parses the artifact embedded in `bytes` (a
    /// whole executable file).
    ///
    /// # Errors
    /// [`ArtifactError::NotEmbedded`] without a footer,
    /// [`ArtifactError::Tampered`] on any footer or hash mismatch, and
    /// every error of [`Artifact::from_bytes`].
    pub fn from_executable_bytes(bytes: &[u8]) -> Result<Self, ArtifactError> {
        let footer_start = bytes
            .len()
            .checked_sub(FOOTER_LEN)
            .ok_or(ArtifactError::NotEmbedded)?;
        let (_, tail) = bytes.split_at(footer_start);
        let footer = parse_footer(tail, bytes.len() as u64)?;
        // `parse_footer` checked `offset + len == footer_start`.
        let start = footer_start - footer.len;
        let artifact_bytes = bytes
            .get(start..footer_start)
            .ok_or(ArtifactError::Tampered(TamperScope::Footer))?;
        let artifact = verify_artifact(artifact_bytes)?;
        verify_hash(blake3::hash(&bytes[..footer_start + 16]), &footer)?;
        Ok(Self {
            artifact,
            runner_len: footer.offset,
        })
    }

    /// Reads the artifact embedded in the file at `path` and streams the
    /// executable through BLAKE3 without allocating the runner bytes.
    ///
    /// # Errors
    /// As [`EmbeddedArtifact::from_executable_bytes`], plus
    /// [`ArtifactError::Io`] when the file cannot be read.
    pub fn from_path(path: &Path) -> Result<Self, ArtifactError> {
        let io = |context: &'static str| {
            move |source: std::io::Error| ArtifactError::Io { context, source }
        };
        let mut file = File::open(path).map_err(io("open executable"))?;
        let file_len = file.metadata().map_err(io("stat executable"))?.len();
        if file_len < FOOTER_LEN as u64 {
            return Err(ArtifactError::NotEmbedded);
        }
        let mut tail = [0u8; FOOTER_LEN];
        file.seek(SeekFrom::End(-(FOOTER_LEN as i64)))
            .map_err(io("seek to executable footer"))?;
        file.read_exact(&mut tail)
            .map_err(io("read executable footer"))?;
        let footer = parse_footer(&tail, file_len)?;
        file.seek(SeekFrom::Start(footer.offset))
            .map_err(io("seek to embedded artifact"))?;
        let mut artifact_bytes = vec![0u8; footer.len];
        file.read_exact(&mut artifact_bytes)
            .map_err(io("read embedded artifact"))?;
        let artifact = verify_artifact(&artifact_bytes)?;
        file.seek(SeekFrom::Start(0))
            .map_err(io("seek to executable start"))?;
        let mut hasher = blake3::Hasher::new();
        let mut remaining = file_len - FOOTER_LEN as u64 + 16;
        let mut buffer = [0u8; 65536];
        while remaining > 0 {
            let count = remaining.min(buffer.len() as u64) as usize;
            file.read_exact(&mut buffer[..count])
                .map_err(io("hash executable"))?;
            hasher.update(&buffer[..count]);
            remaining -= count as u64;
        }
        verify_hash(hasher.finalize(), &footer)?;
        Ok(Self {
            artifact,
            runner_len: footer.offset,
        })
    }

    /// Reads the artifact embedded in the running executable
    /// (`std::env::current_exe()`).
    ///
    /// # Errors
    /// As [`EmbeddedArtifact::from_path`]; [`ArtifactError::Io`] when the
    /// executable path cannot be determined.
    pub fn from_current_exe() -> Result<Self, ArtifactError> {
        let path = std::env::current_exe().map_err(|source| ArtifactError::Io {
            context: "locate current executable",
            source,
        })?;
        Self::from_path(&path)
    }

    /// Verifies the complete native executable and its statically embedded
    /// artifact agree. A generated binary must be sealed before execution.
    ///
    /// # Errors
    /// As [`Self::from_current_exe`], or a tamper error if the artifacts differ.
    pub fn verify_current_exe_matches(expected: &[u8]) -> Result<(), ArtifactError> {
        let embedded = Self::from_current_exe()?;
        if embedded.artifact.as_bytes() != expected {
            return Err(ArtifactError::Tampered(TamperScope::Artifact));
        }
        Ok(())
    }

    /// The verified artifact.
    #[must_use]
    pub fn artifact(&self) -> &Artifact {
        &self.artifact
    }

    /// The verified artifact, by value.
    #[must_use]
    pub fn into_artifact(self) -> Artifact {
        self.artifact
    }

    /// Number of runner bytes in front of the artifact (= artifact offset).
    #[must_use]
    pub fn runner_len(&self) -> u64 {
        self.runner_len
    }
}

/// Decodes the last [`FOOTER_LEN`] bytes of a file of `file_len` bytes.
/// Checks the magic, that the artifact ends exactly where the footer
/// starts, and the size limit, in that order, before anything is read.
fn parse_footer(tail: &[u8], file_len: u64) -> Result<Footer, ArtifactError> {
    let field = |range: std::ops::Range<usize>| tail.get(range).ok_or(ArtifactError::NotEmbedded);
    if field(48..56)? == b"HARWAEND" {
        return Err(ArtifactError::LegacyExecutableFooter);
    }
    if field(48..56)? != FOOTER_MAGIC {
        return Err(ArtifactError::NotEmbedded);
    }
    let mut offset = [0u8; 8];
    offset.copy_from_slice(field(0..8)?);
    let mut len = [0u8; 8];
    len.copy_from_slice(field(8..16)?);
    let mut hash = [0u8; HASH_LEN];
    hash.copy_from_slice(field(16..48)?);
    let offset = u64::from_le_bytes(offset);
    let len = u64::from_le_bytes(len);

    let ends_at_footer = offset
        .checked_add(len)
        .and_then(|end| end.checked_add(FOOTER_LEN as u64))
        .is_some_and(|end| end == file_len);
    if !ends_at_footer {
        return Err(ArtifactError::Tampered(TamperScope::Footer));
    }
    let len = match usize::try_from(len) {
        Ok(len) if len <= MAX_ARTIFACT_LEN => len,
        _ => {
            return Err(ArtifactError::LimitExceeded {
                limit: Limit::ArtifactLen,
                actual: len,
                max: MAX_ARTIFACT_LEN as u64,
            });
        }
    };
    Ok(Footer { offset, len, hash })
}

/// Checks the digest covering the executable and footer location fields.
fn verify_artifact(bytes: &[u8]) -> Result<Artifact, ArtifactError> {
    let trailer = bytes
        .len()
        .checked_sub(HASH_LEN)
        .ok_or(ArtifactError::Tampered(TamperScope::Artifact))?;
    if blake3::hash(&bytes[..trailer]).as_bytes().as_slice() != &bytes[trailer..] {
        return Err(ArtifactError::Tampered(TamperScope::Artifact));
    }
    Artifact::from_bytes(bytes)
}

/// Checks the digest covering the executable and footer location fields.
fn verify_hash(hash: blake3::Hash, footer: &Footer) -> Result<(), ArtifactError> {
    if hash.as_bytes() != &footer.hash {
        return Err(ArtifactError::Tampered(TamperScope::Footer));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{EmbeddedArtifact, FOOTER_LEN, append_to_executable, write_executable};
    use crate::artifact::{Artifact, ArtifactBuilder, MAX_ARTIFACT_LEN};
    use crate::error::{ArtifactError, TamperScope};
    use crate::kind::PayloadKind;
    use crate::test_support::{TestResult, ctx};
    use serde_json::json;
    use std::io::{Seek, SeekFrom, Write};

    const RUNNER: &[u8] = b"\x7fELF\x02\x01\x01 fake runner blob with some code bytes \x00\x01\x02";

    fn sample() -> Result<Artifact, ArtifactError> {
        ArtifactBuilder::new(&json!({"schema": "harwness.agent-ir/v2", "name": "demo"}))
            .add_payload(
                PayloadKind::Skill,
                "skills/review/instructions.md",
                b"review".to_vec(),
            )
            .add_payload(
                PayloadKind::Instructions,
                "instructions/system.md",
                b"be brief".to_vec(),
            )
            .build()
    }

    fn flipped(bytes: &[u8], index: usize) -> Vec<u8> {
        let mut out = bytes.to_vec();
        if let Some(byte) = out.get_mut(index) {
            *byte ^= 0x01;
        }
        out
    }

    #[test]
    fn test_append_then_extract_roundtrips() -> TestResult {
        let artifact = sample().map_err(ctx("build sample"))?;
        let binary = append_to_executable(RUNNER, &artifact);
        assert_eq!(
            binary.len(),
            RUNNER.len() + artifact.as_bytes().len() + FOOTER_LEN
        );
        assert!(binary.starts_with(RUNNER));
        assert!(binary.ends_with(b"HARWAEN2"));
        let embedded = EmbeddedArtifact::from_executable_bytes(&binary).map_err(ctx("extract"))?;
        assert_eq!(embedded.runner_len(), RUNNER.len() as u64);
        assert_eq!(embedded.artifact(), &artifact);
        assert_eq!(embedded.into_artifact().digest(), artifact.digest());
        Ok(())
    }

    #[test]
    fn test_empty_runner_is_allowed() -> TestResult {
        let artifact = sample().map_err(ctx("build sample"))?;
        let binary = append_to_executable(b"", &artifact);
        let embedded = EmbeddedArtifact::from_executable_bytes(&binary).map_err(ctx("extract"))?;
        assert_eq!(embedded.runner_len(), 0);
        Ok(())
    }

    #[test]
    fn test_plain_file_is_not_embedded() -> TestResult {
        for plain in [&b""[..], &b"short"[..], RUNNER, &[0u8; 4096][..]] {
            assert!(matches!(
                EmbeddedArtifact::from_executable_bytes(plain),
                Err(ArtifactError::NotEmbedded)
            ));
        }
        // A bare artifact file has no footer either.
        let artifact = sample().map_err(ctx("build sample"))?;
        assert!(matches!(
            EmbeddedArtifact::from_executable_bytes(artifact.as_bytes()),
            Err(ArtifactError::NotEmbedded)
        ));
        Ok(())
    }

    #[test]
    fn test_flipped_footer_fields_are_tampered() -> TestResult {
        let artifact = sample().map_err(ctx("build sample"))?;
        let binary = append_to_executable(RUNNER, &artifact);
        let footer = binary.len() - FOOTER_LEN;
        // offset, length and executable hash.
        for index in [footer, footer + 8, footer + 16, footer + 47] {
            assert!(
                matches!(
                    EmbeddedArtifact::from_executable_bytes(&flipped(&binary, index)),
                    Err(ArtifactError::Tampered(TamperScope::Footer))
                ),
                "footer byte {} not detected",
                index - footer
            );
        }
        // The magic: without it there is no footer at all.
        assert!(matches!(
            EmbeddedArtifact::from_executable_bytes(&flipped(&binary, binary.len() - 1)),
            Err(ArtifactError::NotEmbedded)
        ));
        Ok(())
    }

    #[test]
    fn test_flipped_embedded_artifact_is_tampered() -> TestResult {
        let artifact = sample().map_err(ctx("build sample"))?;
        let binary = append_to_executable(RUNNER, &artifact);
        let start = RUNNER.len();
        let end = binary.len() - FOOTER_LEN;
        // Magic, header, payload data: the recomputed hash no longer matches.
        for index in [start, start + 20, end - 40] {
            assert!(
                matches!(
                    EmbeddedArtifact::from_executable_bytes(&flipped(&binary, index)),
                    Err(ArtifactError::Tampered(TamperScope::Artifact))
                ),
                "artifact byte {} not detected",
                index - start
            );
        }
        // The artifact's own trailer is verified as well.
        assert!(matches!(
            EmbeddedArtifact::from_executable_bytes(&flipped(&binary, end - 1)),
            Err(ArtifactError::Tampered(TamperScope::Artifact))
        ));
        Ok(())
    }

    #[test]
    fn test_runner_tampering_is_rejected_in_memory_and_on_disk() -> TestResult {
        let artifact = sample().map_err(ctx("build sample"))?;
        let binary = append_to_executable(RUNNER, &artifact);
        let tampered = flipped(&binary, 3);
        assert!(matches!(
            EmbeddedArtifact::from_executable_bytes(&tampered),
            Err(ArtifactError::Tampered(TamperScope::Footer))
        ));
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("tampered-agent");
        std::fs::write(&path, tampered).map_err(ctx("write tampered"))?;
        assert!(matches!(
            EmbeddedArtifact::from_path(&path),
            Err(ArtifactError::Tampered(TamperScope::Footer))
        ));
        Ok(())
    }

    #[test]
    fn test_legacy_footer_requires_a_rebuild() -> TestResult {
        let artifact = sample().map_err(ctx("build sample"))?;
        let mut binary = append_to_executable(RUNNER, &artifact);
        let magic = binary.len() - 8;
        binary[magic..].copy_from_slice(b"HARWAEND");
        assert!(matches!(
            EmbeddedArtifact::from_executable_bytes(&binary),
            Err(ArtifactError::LegacyExecutableFooter)
        ));
        Ok(())
    }

    #[test]
    fn test_write_executable_then_read_from_path() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("agent");
        let artifact = sample().map_err(ctx("build sample"))?;
        let binary = append_to_executable(RUNNER, &artifact);
        write_executable(&path, &binary).map_err(ctx("write executable"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path)
                .map_err(ctx("stat"))?
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o755);
        }
        let embedded = EmbeddedArtifact::from_path(&path).map_err(ctx("read back"))?;
        assert_eq!(embedded.artifact(), &artifact);
        assert_eq!(embedded.runner_len(), RUNNER.len() as u64);
        Ok(())
    }

    #[test]
    fn test_from_path_rejects_plain_and_short_files() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let short = dir.path().join("short");
        std::fs::write(&short, b"tiny").map_err(ctx("write short"))?;
        assert!(matches!(
            EmbeddedArtifact::from_path(&short),
            Err(ArtifactError::NotEmbedded)
        ));
        let plain = dir.path().join("plain");
        std::fs::write(&plain, RUNNER.repeat(10)).map_err(ctx("write plain"))?;
        assert!(matches!(
            EmbeddedArtifact::from_path(&plain),
            Err(ArtifactError::NotEmbedded)
        ));
        Ok(())
    }

    #[test]
    fn test_from_path_checks_the_limit_before_reading() -> TestResult {
        // A sparse file whose footer declares an artifact one byte over the
        // limit; the reader must refuse before allocating it.
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("huge");
        let mut file = std::fs::File::create(&path).map_err(ctx("create"))?;
        let len = MAX_ARTIFACT_LEN as u64 + 1;
        file.set_len(len).map_err(ctx("sparse length"))?;
        file.seek(SeekFrom::End(0)).map_err(ctx("seek end"))?;
        let mut footer = Vec::new();
        footer.extend_from_slice(&0u64.to_le_bytes());
        footer.extend_from_slice(&len.to_le_bytes());
        footer.extend_from_slice(&[0u8; 32]);
        footer.extend_from_slice(b"HARWAEN2");
        file.write_all(&footer).map_err(ctx("write footer"))?;
        drop(file);
        assert!(matches!(
            EmbeddedArtifact::from_path(&path),
            Err(ArtifactError::LimitExceeded { .. })
        ));
        Ok(())
    }

    #[test]
    fn test_current_test_binary_is_not_embedded() {
        assert!(matches!(
            EmbeddedArtifact::from_current_exe(),
            Err(ArtifactError::NotEmbedded)
        ));
    }
}
