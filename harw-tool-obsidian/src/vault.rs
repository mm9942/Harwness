//! Vault root resolution on top of the sandbox's authoritative workspace.
//!
//! # Responsibility
//! Every tool of this crate operates strictly inside one vault root. The
//! root is resolved per call: the `HARW_OBSIDIAN_VAULT` environment variable
//! when set, otherwise the `docs/planning` directory inside the canonical
//! workspace root (the repository's Obsidian-compatible notes area, see
//! `docs/planning/70-decisions/README.md`).
//!
//! # Path safety
//! The vault root itself is derived from the sandbox's
//! [`SandboxSpec::workspace`] canonical root, and every caller-supplied path
//! is resolved through that workspace binding
//! (`WorkspaceBinding::resolve_existing` / `resolve_for_create`), so
//! containment is enforced by the authority layer, not by this crate's own
//! string logic.

use std::path::{Path, PathBuf};

use harw_authority::{SandboxSpec, WorkspaceBinding};

/// Default vault root relative to the workspace root.
pub const DEFAULT_VAULT_SUBDIR: &str = "docs/planning";

/// Environment variable that overrides the default vault root.
pub const VAULT_ENV_VAR: &str = "HARW_OBSIDIAN_VAULT";

/// The resolved vault root plus the workspace binding it was derived from.
#[derive(Debug, Clone)]
pub struct VaultRoot {
    workspace: WorkspaceBinding,
    vault_relative: PathBuf,
}

impl VaultRoot {
    /// Resolve the vault root from the tool execution context's sandbox.
    ///
    /// # Errors
    /// Returns an error message when the resolved root does not exist or is
    /// not a directory, so callers surface it as a tool error instead of
    /// silently searching an empty tree.
    pub fn resolve(sandbox: &SandboxSpec) -> Result<Self, String> {
        let workspace = sandbox.workspace().clone();
        let vault_relative = if let Ok(from_env) = std::env::var(VAULT_ENV_VAR) {
            // Relative to the workspace; absolute values are rejected so the
            // sandbox boundary stays in charge even with the override set.
            let candidate = PathBuf::from(&from_env);
            if candidate.is_absolute() {
                return Err(format!(
                    "{VAULT_ENV_VAR} must be relative to the workspace root, got '{from_env}'"
                ));
            }
            candidate
        } else {
            PathBuf::from(DEFAULT_VAULT_SUBDIR)
        };
        let absolute = workspace
            .resolve_existing(&vault_relative)
            .map_err(|error| format!("vault root not accessible: {error}"))?;
        if !absolute.is_dir() {
            return Err(format!(
                "vault root {} is not a directory",
                absolute.display()
            ));
        }
        Ok(Self {
            workspace,
            vault_relative,
        })
    }

    /// The vault root relative to the workspace root (display form).
    #[must_use]
    pub fn vault_relative(&self) -> &Path {
        &self.vault_relative
    }

    /// Resolve a caller-supplied vault-relative path to an existing file.
    ///
    /// # Errors
    /// Returns an error message when the path escapes the vault or the file
    /// does not exist.
    pub fn existing_note(&self, relative: &str) -> Result<PathBuf, String> {
        let note_relative = self.vault_relative.join(relative);
        let absolute = self
            .workspace
            .resolve_existing(&note_relative)
            .map_err(|error| format!("note '{relative}' not accessible: {error}"))?;
        if !absolute.is_file() {
            return Err(format!("note '{relative}' does not exist in the vault"));
        }
        Ok(absolute)
    }

    /// Resolve the vault root itself (or a subfolder of it) as a directory.
    ///
    /// # Errors
    /// Returns an error message when the path escapes the vault or is not a
    /// directory.
    pub fn existing_dir(&self, relative: &str) -> Result<std::path::PathBuf, String> {
        let dir_relative = self.vault_relative.join(relative);
        let absolute = self
            .workspace
            .resolve_existing(&dir_relative)
            .map_err(|error| format!("folder '{relative}' not accessible: {error}"))?;
        if !absolute.is_dir() {
            return Err(format!("'{relative}' is not a folder in the vault"));
        }
        Ok(absolute)
    }

    /// Resolve a caller-supplied vault-relative path for writing; the file
    /// itself may not exist yet, its parent directory must.
    ///
    /// # Errors
    /// Returns an error message when the path escapes the vault or the
    /// parent directory is missing.
    pub fn writable_note(&self, relative: &str) -> Result<PathBuf, String> {
        let note_relative = self.vault_relative.join(relative);
        let absolute = self
            .workspace
            .resolve_for_create(&note_relative)
            .map_err(|error| format!("note '{relative}' not writable: {error}"))?;
        Ok(absolute)
    }

    /// Map an absolute file back to a vault-relative display path.
    #[must_use]
    pub fn display_path(&self, absolute: &Path) -> String {
        absolute
            .strip_prefix(self.workspace.canonical_root().join(&self.vault_relative))
            .map(|rel| rel.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| absolute.to_string_lossy().to_string())
    }
}

/// Split a note's raw text into `(frontmatter_yaml, body)`.
///
/// Frontmatter must be a `---` fenced block at the very start. Without one,
/// the whole text is body and the YAML part is `None`.
#[must_use]
pub fn split_frontmatter(text: &str) -> (Option<String>, &str) {
    let rest = match text.strip_prefix("---\n") {
        Some(rest) => rest,
        None => match text.strip_prefix("---\r\n") {
            Some(rest) => rest,
            None => return (None, text),
        },
    };
    let Some(end) = rest.find("\n---") else {
        return (None, text);
    };
    let yaml = rest[..end].trim().to_owned();
    let after = rest[end + 4..].strip_prefix('\n').unwrap_or(&rest[end + 4..]);
    (Some(yaml), after)
}

/// Extract the first scalar value for `key:` from a YAML frontmatter string.
/// Handles plain scalars and quoted values; lists are returned as the raw
/// bracketed text.
#[must_use]
pub fn frontmatter_scalar<'a>(yaml: &'a str, key: &str) -> Option<&'a str> {
    let prefix = format!("{key}:");
    for line in yaml.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix(&prefix) {
            let value = rest.trim();
            if value.is_empty() {
                return None;
            }
            return Some(value.trim_matches('"').trim_matches('\''));
        }
    }
    None
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn temp_vault() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let vault = dir.path().join(DEFAULT_VAULT_SUBDIR);
        std::fs::create_dir_all(&vault).expect("mkdir");
        (dir, vault)
    }

    #[test]
    fn split_frontmatter_parses_fenced_yaml() {
        let text = "---\ntitle: Hello\nmemory: palace/code-map\n---\n\n# Body\n";
        let (yaml, body) = split_frontmatter(text);
        assert_eq!(
            yaml.as_deref(),
            Some("title: Hello\nmemory: palace/code-map")
        );
        assert!(body.contains("# Body"));
    }

    #[test]
    fn split_frontmatter_without_fence_is_all_body() {
        let (yaml, body) = split_frontmatter("# Just a note\n");
        assert!(yaml.is_none());
        assert_eq!(body, "# Just a note\n");
    }

    #[test]
    fn frontmatter_scalar_reads_plain_and_quoted() {
        let yaml = "title: \"My Note\"\nstatus: draft\nlist: [a, b]\n";
        assert_eq!(frontmatter_scalar(yaml, "title"), Some("My Note"));
        assert_eq!(frontmatter_scalar(yaml, "status"), Some("draft"));
        assert_eq!(frontmatter_scalar(yaml, "list"), Some("[a, b]"));
        assert_eq!(frontmatter_scalar(yaml, "missing"), None);
    }

    #[test]
    fn vault_env_override_must_be_relative() {
        // An absolute override is rejected before any filesystem access, so
        // the sandbox boundary stays in charge even with the env set.
        let (dir, _vault) = temp_vault();
        let root = VaultRoot::resolve(&test_sandbox_at(dir.path(), "ws-vault-test"))
            .expect("default resolution works");
        assert_eq!(root.vault_relative(), Path::new(DEFAULT_VAULT_SUBDIR));
    }

    /// Build a minimal sandbox spec for tests: read+write workspace
    /// permissions over the given directory (pattern of `harw-tool-lens`).
    pub fn test_sandbox_at(root: &Path, harness_id: &str) -> SandboxSpec {
        use harw_authority::{Permission, PermissionSet, WorkspaceRegistration, WorkspaceRegistry};
        use harw_types::{TenantId, WorkspaceId};
        let registry = WorkspaceRegistry::build(
            root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("t"),
                workspace: WorkspaceId::from_str(harness_id),
                root: root.to_path_buf(),
            }],
        )
        .expect("test registry");
        let binding = registry
            .resolve(&TenantId::from_str("t"), &WorkspaceId::from_str(harness_id))
            .expect("test binding");
        SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy(vec![
                Permission::ReadWorkspace,
                Permission::WriteWorkspace,
            ]),
        )
    }
}
