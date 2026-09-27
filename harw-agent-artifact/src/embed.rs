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
//!
//! [`write_executable`] writes to a destination the caller chose, and
//! [`TempExecutable`] writes a throwaway binary into a fresh private
//! directory and removes it on drop.

use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

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
/// `path` is opened as given, so an existing file is truncated and reused
/// and a symlink is followed. It is meant for a destination the caller
/// chose (`-o`, the harw bin dir); a throwaway binary in a shared directory
/// goes through [`TempExecutable`].
///
/// # Errors
/// [`ArtifactError::Io`] when creating, writing, chmod-ing or syncing fails.
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
    // `mode` above only applies when the file is created (and is masked by
    // the umask); set it explicitly for an existing file too. The open handle
    // is chmod-ed, not the path, so a swapped path is never touched.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o755))
            .map_err(io("set executable permissions"))?;
    }
    file.sync_all().map_err(io("sync executable file"))?;
    Ok(())
}

/// Process-wide counter that keeps private directory names unique.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Creates a fresh directory `<parent>/<prefix>-<pid>-<nanos>-<n>` (mode
/// `0o700` on unix) and returns its path. mkdir fails on any existing entry,
/// including a symlink, so the directory is always new and ours; a name
/// clash is retried up to 16 times.
fn create_private_dir(parent: &Path, prefix: &str) -> Result<PathBuf, ArtifactError> {
    let mut attempt: u32 = 0;
    loop {
        let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let candidate = parent.join(format!("{prefix}-{}-{nanos}-{n}", std::process::id()));
        let mut builder = std::fs::DirBuilder::new();
        // Not recursive: only `candidate` itself is created, never a parent.
        builder.recursive(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            builder.mode(0o700);
        }
        match builder.create(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == ErrorKind::AlreadyExists && attempt < 16 => attempt += 1,
            Err(source) => {
                return Err(ArtifactError::Io {
                    context: "create private directory",
                    source,
                });
            }
        }
    }
}

/// Writes `bytes` to a new file at `path` (mode `0o755` on unix).
/// `create_new` is `O_CREAT | O_EXCL`, so it never follows a symlink and
/// never reuses a file. Nothing is synced: the file is a throwaway.
fn write_new_executable(path: &Path, bytes: &[u8]) -> Result<(), ArtifactError> {
    let io =
        |context: &'static str| move |source: std::io::Error| ArtifactError::Io { context, source };
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o755);
    }
    let mut file = options.open(path).map_err(io("create executable file"))?;
    file.write_all(bytes).map_err(io("write executable file"))?;
    // `mode` above is masked by the umask; undo that on the open handle.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o755))
            .map_err(io("set executable permissions"))?;
    }
    // Close the write handle before returning, so a later exec of `path`
    // does not fail with ETXTBSY.
    drop(file);
    Ok(())
}

/// A built binary written to a fresh private directory for one exec; the
/// directory and the file are removed when this value is dropped.
#[derive(Debug)]
pub struct TempExecutable {
    dir: PathBuf,
    path: PathBuf,
}

impl TempExecutable {
    /// Writes `bytes` to `<parent>/<prefix>-<pid>-<unique>/<prefix>`, where
    /// `prefix` is a plain file name. The directory is mode `0o700` on unix
    /// and created exclusively; the file is mode `0o755` and created with
    /// `create_new`. A shared `parent` such as the system temp dir leaves
    /// other local users nothing to pre-plant or swap.
    ///
    /// # Errors
    /// [`ArtifactError::Io`] when no fresh directory can be created
    /// (`AlreadyExists` after 16 name clashes, or any other mkdir error) or
    /// when writing the file fails, and with `InvalidInput` when `prefix` is
    /// not a plain file name. Nothing is left behind on error.
    pub fn create(parent: &Path, prefix: &str, bytes: &[u8]) -> Result<Self, ArtifactError> {
        // `prefix` names both the directory and the file; a separator or
        // `..` would place them outside `parent`.
        if !is_plain_file_name(prefix) {
            return Err(ArtifactError::Io {
                context: "check temp executable prefix",
                source: ErrorKind::InvalidInput.into(),
            });
        }
        let dir = create_private_dir(parent, prefix)?;
        // From here on an error drops `temp`, which removes the directory.
        let temp = Self {
            path: dir.join(prefix),
            dir,
        };
        write_new_executable(&temp.path, bytes)?;
        Ok(temp)
    }

    /// Path of the executable file inside the private directory.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempExecutable {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// True when `name` is exactly one normal path component (not empty, no
/// separator, not `.` or `..`).
fn is_plain_file_name(name: &str) -> bool {
    let mut components = Path::new(name).components();
    match (components.next(), components.next()) {
        (Some(std::path::Component::Normal(first)), None) => first == name,
        _ => false,
    }
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
    use super::{
        EmbeddedArtifact, FOOTER_LEN, TempExecutable, append_to_executable, write_executable,
        write_new_executable,
    };
    use crate::artifact::{Artifact, ArtifactBuilder, MAX_ARTIFACT_LEN};
    use crate::error::{ArtifactError, TamperScope};
    use crate::kind::PayloadKind;
    use crate::test_support::{TestResult, ctx};
    use serde_json::json;
    use std::io::{Seek, SeekFrom, Write};
    use std::path::Path;

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

    #[cfg(unix)]
    #[test]
    fn test_write_executable_resets_the_mode_of_an_existing_file() -> TestResult {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("agent");
        std::fs::write(&path, b"older and longer contents").map_err(ctx("pre-write"))?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(ctx("chmod 0600"))?;
        write_executable(&path, b"new").map_err(ctx("write executable"))?;
        assert_eq!(std::fs::read(&path).map_err(ctx("read back"))?, b"new");
        let mode = std::fs::metadata(&path)
            .map_err(ctx("stat"))?
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
        Ok(())
    }

    #[test]
    fn test_temp_executable_is_private_and_removed_on_drop() -> TestResult {
        let parent = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let artifact = sample().map_err(ctx("build sample"))?;
        let binary = append_to_executable(RUNNER, &artifact);
        let temp = TempExecutable::create(parent.path(), "harw-agent-run", &binary)
            .map_err(ctx("create temp executable"))?;
        let path = temp.path().to_path_buf();
        let dir = path
            .parent()
            .map(Path::to_path_buf)
            .ok_or("no private directory")
            .map_err(ctx("temp executable parent"))?;
        assert!(path.is_file());
        assert_eq!(path.parent().and_then(Path::parent), Some(parent.path()));
        assert_eq!(std::fs::read(&path).map_err(ctx("read back"))?, binary);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir_mode = std::fs::metadata(&dir)
                .map_err(ctx("stat dir"))?
                .permissions()
                .mode();
            assert_eq!(dir_mode & 0o077, 0);
            let file_mode = std::fs::metadata(&path)
                .map_err(ctx("stat file"))?
                .permissions()
                .mode();
            assert_eq!(file_mode & 0o777, 0o755);
        }
        let embedded = EmbeddedArtifact::from_path(&path).map_err(ctx("read embedded"))?;
        assert_eq!(embedded.artifact(), &artifact);
        drop(temp);
        assert!(!path.exists());
        assert!(!dir.exists());
        Ok(())
    }

    #[test]
    fn test_temp_executable_names_are_unique() -> TestResult {
        let parent = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let first = TempExecutable::create(parent.path(), "harw-agent-run", b"one")
            .map_err(ctx("create first"))?;
        let second = TempExecutable::create(parent.path(), "harw-agent-run", b"two")
            .map_err(ctx("create second"))?;
        let first_dir = first.path().parent().map(Path::to_path_buf);
        let second_dir = second.path().parent().map(Path::to_path_buf);
        assert!(first_dir.is_some());
        assert!(second_dir.is_some());
        assert_ne!(first_dir, second_dir);
        assert!(first.path().is_file());
        assert!(second.path().is_file());
        drop(first);
        drop(second);
        for dir in [first_dir, second_dir].into_iter().flatten() {
            assert!(!dir.exists());
        }
        let left = std::fs::read_dir(parent.path())
            .map_err(ctx("list parent"))?
            .count();
        assert_eq!(left, 0);
        Ok(())
    }

    #[test]
    fn test_temp_executable_rejects_a_prefix_that_is_not_a_file_name() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let parent = root.path().join("parent");
        std::fs::create_dir(&parent).map_err(ctx("create parent"))?;
        for prefix in ["", ".", "..", "../escape", "nested/name", "/absolute"] {
            assert!(
                matches!(
                    TempExecutable::create(&parent, prefix, b"x"),
                    Err(ArtifactError::Io { .. })
                ),
                "prefix {prefix:?} accepted"
            );
        }
        // Nothing was created, neither in `parent` nor next to it.
        let in_parent = std::fs::read_dir(&parent)
            .map_err(ctx("list parent"))?
            .count();
        assert_eq!(in_parent, 0);
        let in_root = std::fs::read_dir(root.path())
            .map_err(ctx("list root"))?
            .count();
        assert_eq!(in_root, 1);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn test_write_new_executable_refuses_a_planted_symlink() -> TestResult {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let victim = dir.path().join("victim");
        std::fs::write(&victim, b"keep").map_err(ctx("write victim"))?;
        std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o600))
            .map_err(ctx("chmod victim"))?;
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&victim, &link).map_err(ctx("plant symlink"))?;
        assert!(matches!(
            write_new_executable(&link, b"evil"),
            Err(ArtifactError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::AlreadyExists
        ));
        assert_eq!(std::fs::read(&victim).map_err(ctx("read victim"))?, b"keep");
        let mode = std::fs::metadata(&victim)
            .map_err(ctx("stat victim"))?
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        // A dangling link must not create its target either.
        let missing = dir.path().join("missing");
        let dangling = dir.path().join("dangling");
        std::os::unix::fs::symlink(&missing, &dangling).map_err(ctx("plant dangling symlink"))?;
        assert!(write_new_executable(&dangling, b"evil").is_err());
        assert!(!missing.exists());
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
