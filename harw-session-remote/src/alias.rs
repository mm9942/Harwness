//! Machine-local host alias store (W00 §8, S07).
//!
//! State path: `$HARW_STATE_DIR/hosts/<alias>.json`. Aliases are local
//! operator state; an endpoint is a locator only and the record pins the
//! expected authenticated host identity. Changing trust is an explicit,
//! trusted action; nothing auto-creates an alias.
//!
//! Alias grammar: 1..=64 bytes of `[a-z0-9_-]`, first byte alphanumeric. The
//! file name is exactly `<alias>.json`; with this closed alphabet the mapping
//! is injective (no case folding, no normalization, no separators, so no two
//! aliases share a file and none can traverse). Records never hold secrets:
//! endpoints that could embed credentials (`user@`, `?query`, `#fragment`,
//! whitespace/control bytes) are rejected.

use std::fs::DirBuilder;
use std::io::Read;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Component, Path, PathBuf};

use harw_fsutil::{
    AtomicWriteOptions, OpenMode, ensure_private_regular, open_nofollow, write_atomic,
};
use serde::{Deserialize, Serialize};

use crate::RemoteError;

/// One stored host alias.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostAlias {
    /// Operator-chosen alias name.
    pub alias: String,
    /// Locator: `unix:<path>` or `node:<host:port>`.
    pub endpoint: String,
    /// Node id the host must prove (node endpoints only).
    pub expected_node: Option<String>,
}

/// Longest accepted alias.
pub const MAX_ALIAS_LEN: usize = 64;
/// Largest record file read back (records are tiny; this bounds hostile files).
const MAX_RECORD_BYTES: u64 = 16 * 1024;
const MAX_ENDPOINT_LEN: usize = 4096;
const MAX_NODE_LEN: usize = 128;
const SUFFIX: &str = ".json";

fn state_err(what: &str, detail: impl std::fmt::Display) -> RemoteError {
    RemoteError::State(format!("{what}: {detail}"))
}

/// Check the alias grammar.
///
/// # Errors
/// [`RemoteError::State`] for empty, over-long, non `[a-z0-9_-]`, or
/// separator/dot/traversal-bearing aliases.
pub fn validate_alias(alias: &str) -> Result<(), RemoteError> {
    let bytes = alias.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_ALIAS_LEN {
        return Err(state_err("invalid alias", "length must be 1..=64"));
    }
    if !bytes
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_' || *b == b'-')
    {
        return Err(state_err("invalid alias", "only [a-z0-9_-] allowed"));
    }
    if !bytes.first().is_some_and(u8::is_ascii_alphanumeric) {
        return Err(state_err(
            "invalid alias",
            "must start with a letter or digit",
        ));
    }
    Ok(())
}

fn validate_node_id(node: &str) -> Result<(), RemoteError> {
    let ok = !node.is_empty()
        && node.len() <= MAX_NODE_LEN
        && node
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b':'));
    if ok {
        Ok(())
    } else {
        Err(state_err(
            "invalid expected_node",
            "1..=128 bytes of [A-Za-z0-9._:-]",
        ))
    }
}

fn validate_node_authority(authority: &str) -> Result<(), RemoteError> {
    let bad = || state_err("invalid endpoint", "node endpoint must be host:port");
    let (host, port) = authority.rsplit_once(':').ok_or_else(bad)?;
    let port: u16 = port.parse().map_err(|_| bad())?;
    if port == 0 {
        return Err(bad());
    }
    let host_ok = if let Some(inner) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        !inner.is_empty()
            && inner
                .bytes()
                .all(|b| b.is_ascii_hexdigit() || b == b':' || b == b'.')
    } else {
        !host.is_empty()
            && host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
    };
    if host_ok { Ok(()) } else { Err(bad()) }
}

/// Validate a full record: alias grammar, endpoint locator shape, node pin.
///
/// # Errors
/// [`RemoteError::State`] describing the first violated rule.
pub fn validate_record(record: &HostAlias) -> Result<(), RemoteError> {
    validate_alias(&record.alias)?;
    let endpoint = record.endpoint.as_str();
    if endpoint.is_empty() || endpoint.len() > MAX_ENDPOINT_LEN {
        return Err(state_err("invalid endpoint", "length out of range"));
    }
    if endpoint
        .bytes()
        .any(|b| b.is_ascii_control() || b == b' ' || b == b'@' || b == b'?' || b == b'#')
    {
        return Err(state_err(
            "invalid endpoint",
            "whitespace, control bytes, '@', '?' and '#' are refused (no credentials in locators)",
        ));
    }
    if let Some(path) = endpoint.strip_prefix("unix:") {
        let path = Path::new(path);
        if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err(state_err(
                "invalid endpoint",
                "unix endpoint must be an absolute path without '..'",
            ));
        }
        if record.expected_node.is_some() {
            return Err(state_err(
                "invalid record",
                "expected_node applies to node endpoints only",
            ));
        }
    } else if let Some(authority) = endpoint.strip_prefix("node:") {
        validate_node_authority(authority)?;
        match record.expected_node.as_deref() {
            Some(node) => validate_node_id(node)?,
            None => {
                return Err(state_err(
                    "invalid record",
                    "node endpoint requires a pinned expected_node",
                ));
            }
        }
    } else {
        return Err(state_err(
            "invalid endpoint",
            "expected `unix:<path>` or `node:<host:port>`",
        ));
    }
    Ok(())
}

/// Reads and writes alias records under a state directory.
#[derive(Clone, Debug)]
pub struct AliasStore {
    dir: PathBuf,
}

impl AliasStore {
    /// Store rooted at `state_dir` (records live in `<state_dir>/hosts`).
    #[must_use]
    pub fn new(state_dir: &Path) -> Self {
        Self {
            dir: state_dir.join("hosts"),
        }
    }

    fn path_for(&self, alias: &str) -> Result<PathBuf, RemoteError> {
        validate_alias(alias)?;
        Ok(self.dir.join(format!("{alias}{SUFFIX}")))
    }

    /// Read one record file, enforcing private perms, size bound, schema and
    /// that the stored alias matches the file name.
    fn read_record(&self, alias: &str, path: &Path) -> Result<Option<HostAlias>, RemoteError> {
        let file = match open_nofollow(path, OpenMode::read_only()) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(state_err("open alias record", e)),
        };
        ensure_private_regular(&file).map_err(|e| state_err("alias record permissions", e))?;
        let mut bytes = Vec::new();
        file.take(MAX_RECORD_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| state_err("read alias record", e))?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_RECORD_BYTES {
            return Err(state_err("alias record", "too large"));
        }
        let record: HostAlias =
            serde_json::from_slice(&bytes).map_err(|e| state_err("parse alias record", e))?;
        if record.alias != alias {
            return Err(state_err(
                "alias record",
                "alias does not match its file name",
            ));
        }
        validate_record(&record)?;
        Ok(Some(record))
    }

    /// Load one alias.
    ///
    /// # Errors
    /// [`RemoteError::State`] for an invalid alias, an unreadable, oversized,
    /// malformed, mismatched or group/other-accessible record.
    pub fn get(&self, alias: &str) -> Result<Option<HostAlias>, RemoteError> {
        let path = self.path_for(alias)?;
        self.read_record(alias, &path)
    }

    /// Create or replace one alias (explicit trusted action).
    ///
    /// The record is validated, then written atomically with mode `0600` into
    /// a `0700` state directory.
    ///
    /// # Errors
    /// [`RemoteError::State`] for an invalid record or an I/O failure.
    pub fn put(&self, record: &HostAlias) -> Result<(), RemoteError> {
        validate_record(record)?;
        let path = self.path_for(&record.alias)?;
        let mut builder = DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder
            .create(&self.dir)
            .map_err(|e| state_err("create hosts dir", e))?;
        let mut bytes =
            serde_json::to_vec_pretty(record).map_err(|e| state_err("encode alias record", e))?;
        bytes.push(b'\n');
        write_atomic(&path, &bytes, AtomicWriteOptions::private())
            .map_err(|e| state_err("write alias record", e))
    }

    /// All stored aliases, sorted by alias. Temp files (dot-prefixed) from an
    /// interrupted write are ignored; any other entry that is not a valid
    /// record is an error rather than silently skipped.
    ///
    /// # Errors
    /// [`RemoteError::State`] on I/O failure or an invalid record.
    pub fn list(&self) -> Result<Vec<HostAlias>, RemoteError> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(state_err("read hosts dir", e)),
        };
        let mut out = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| state_err("read hosts dir entry", e))?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                return Err(state_err("hosts dir", "non-UTF-8 entry name"));
            };
            if name.starts_with('.') {
                continue;
            }
            let Some(alias) = name.strip_suffix(SUFFIX) else {
                return Err(state_err("hosts dir", format!("unexpected entry `{name}`")));
            };
            validate_alias(alias)?;
            if let Some(record) = self.read_record(alias, &entry.path())? {
                out.push(record);
            }
        }
        out.sort_by(|a, b| a.alias.cmp(&b.alias));
        Ok(out)
    }

    /// Remove one alias; `Ok(false)` when it did not exist.
    ///
    /// # Errors
    /// [`RemoteError::State`] for an invalid alias or an I/O failure.
    pub fn remove(&self, alias: &str) -> Result<bool, RemoteError> {
        let path = self.path_for(alias)?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(state_err("remove alias record", e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grammar_rejects_traversal() {
        for bad in ["", ".", "..", "a/b", "../x", "A", "-a", "a.json", "a b"] {
            assert!(validate_alias(bad).is_err(), "{bad}");
        }
        assert!(validate_alias("prod-1_a").is_ok());
    }
}
