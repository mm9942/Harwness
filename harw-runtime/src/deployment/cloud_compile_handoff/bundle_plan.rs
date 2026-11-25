//! Strict, uncompressed file-only compile inputs. Parsing never writes to disk.

use std::collections::{BTreeMap, btree_map::Entry};
use std::ops::Range;

use harw_types::ContentDigest;
use serde::Deserialize;

const MAGIC: &[u8] = b"HARW-COMPILE-BUNDLE\0";
const VERSION: u8 = 1;
const MAX_MANIFEST_BYTES: usize = 512 * 1024;
const MAX_FILES: usize = 4096;
const MAX_PATH_BYTES: usize = 240;
const MAX_COMPONENT_BYTES: usize = 96;
const MAX_DEPTH: usize = 16;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub(super) enum BundleRole {
    Source,
    Toolchain,
}

/// Closed file modes. Ownership, links, times, special modes and extended
/// attributes are not part of this plan and cannot be supplied in a bundle.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum CompileFileMode {
    /// A regular non-executable file.
    Data,
    /// A regular toolchain executable, permitted only in top-level bin/.
    Executable,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u8,
    role: BundleRole,
    files: Vec<ManifestFile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestFile {
    path: String,
    mode: CompileFileMode,
    bytes: usize,
    digest: ContentDigest,
}

/// A verified regular file slice within one pinned bundle. The range is
/// relative to the start of the entire bundle, not a destination path.
#[derive(Debug)]
pub struct CompilePlannedFile {
    path: String,
    mode: CompileFileMode,
    range: Range<usize>,
}

impl CompilePlannedFile {
    /// Normalized relative destination inside an isolated materialization root.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Closed file mode derived from the inspected manifest.
    #[must_use]
    pub const fn mode(&self) -> CompileFileMode {
        self.mode
    }

    /// Absolute byte positions within the verified bundle.
    #[must_use]
    pub fn bundle_range(&self) -> Range<usize> {
        self.range.clone()
    }
}

/// Fully checked metadata; materialization and executable verification are
/// intentionally separate and not implemented here.
#[derive(Debug)]
pub struct CompileBundlePlan {
    files: Vec<CompilePlannedFile>,
}

impl CompileBundlePlan {
    /// Files in strictly ascending, exact, ASCII path order.
    #[must_use]
    pub fn files(&self) -> &[CompilePlannedFile] {
        &self.files
    }

    pub(super) fn validate(bytes: &[u8], expected_role: BundleRole) -> Option<Self> {
        let rest = bytes.strip_prefix(MAGIC)?;
        let len_bytes: [u8; 4] = rest.get(..4)?.try_into().ok()?;
        let len = usize::try_from(u32::from_be_bytes(len_bytes)).ok()?;
        if len == 0 || len > MAX_MANIFEST_BYTES {
            return None;
        }
        let payload_start = MAGIC.len().checked_add(4)?.checked_add(len)?;
        let json_start = MAGIC.len().checked_add(4)?;
        let manifest: Manifest = serde_json::from_slice(bytes.get(json_start..payload_start)?).ok()?;
        if manifest.version != VERSION
            || manifest.role != expected_role
            || manifest.files.is_empty()
            || manifest.files.len() > MAX_FILES
        {
            return None;
        }

        let mut names = BTreeMap::new();
        let mut files = Vec::<CompilePlannedFile>::with_capacity(manifest.files.len());
        let mut cursor = payload_start;
        let mut required_a = false;
        let mut required_b = false;
        for file in manifest.files {
            if files.last().is_some_and(|previous| previous.path.as_str() >= file.path.as_str())
                || !valid_path(&file.path)
                || !accepted_mode(expected_role, &file)
                || !record_path(&file.path, &mut names)
            {
                return None;
            }
            let end = cursor.checked_add(file.bytes)?;
            let contents = bytes.get(cursor..end)?;
            if ContentDigest::of(contents) != file.digest {
                return None;
            }
            match expected_role {
                BundleRole::Source => {
                    required_a |= file.path == "Cargo.toml" && !contents.is_empty();
                    required_b |= file.path == "Cargo.lock" && !contents.is_empty();
                }
                BundleRole::Toolchain => {
                    if file.mode == CompileFileMode::Executable && !contents.is_empty() {
                        required_a |= file.path == "bin/cargo";
                        required_b |= file.path == "bin/rustc";
                    }
                }
            }
            files.push(CompilePlannedFile {
                path: file.path,
                mode: file.mode,
                range: cursor..end,
            });
            cursor = end;
        }
        (required_a && required_b && cursor == bytes.len()).then_some(Self { files })
    }
}

fn accepted_mode(role: BundleRole, file: &ManifestFile) -> bool {
    match role {
        BundleRole::Source => file.mode == CompileFileMode::Data,
        BundleRole::Toolchain => {
            file.mode == CompileFileMode::Data || file.path.starts_with("bin/")
        }
    }
}

fn valid_path(path: &str) -> bool {
    if path.is_empty() || path.len() > MAX_PATH_BYTES {
        return false;
    }
    let mut parts = path.split('/');
    let mut depth = 0;
    parts.all(|part| {
        depth += 1;
        if depth > MAX_DEPTH
            || part.is_empty()
            || part.len() > MAX_COMPONENT_BYTES
            || part == "."
            || part == ".."
            || part.ends_with('.')
            || !part.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+')
            })
        {
            return false;
        }
        let lower = part.to_ascii_lowercase();
        let stem = lower.split_once('.').map_or(lower.as_str(), |(stem, _)| stem);
        !matches!(
            lower.as_str(),
            ".git" | ".cargo" | "rust-toolchain" | "rust-toolchain.toml"
        ) && !matches!(stem, "con" | "prn" | "aux" | "nul")
            && !stem.strip_prefix("com").or_else(|| stem.strip_prefix("lpt")).is_some_and(
                |suffix| matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"),
            )
    })
}

fn record_path(path: &str, names: &mut BTreeMap<String, (String, bool)>) -> bool {
    // Compare both complete file names and implied directory names. This
    // rejects a/b after a, A/x plus a/y, and duplicate case-folded files.
    path.match_indices('/')
        .map(|(index, _)| (&path[..index], true))
        .chain(std::iter::once((path, false)))
        .all(|(prefix, directory)| match names.entry(prefix.to_ascii_lowercase()) {
            Entry::Vacant(entry) => {
                entry.insert((prefix.to_owned(), directory));
                true
            }
            Entry::Occupied(entry) => {
                let (stored_name, stored_directory) = entry.get();
                directory && *stored_directory && stored_name.as_str() == prefix
            }
        })
}
