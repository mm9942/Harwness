//! Credential source detection for the model catalog.
//!
//! Spec source: `docs/design/CONTRACT-setup-install.md`, section
//! „harw-model-catalog / src/sources.rs".
//!
//! # Responsibility scope
//! This module owns the declarative description of *where* provider
//! credentials live on a local machine ([`CredentialSource`]) and the
//! read-only probing that turns those descriptions into concrete
//! [`DetectedCredential`] results. It never reads secret *values*, never
//! writes files, and never mutates process environment.
//!
//! # Key types exported
//! - [`SourceKind`] — kind of credential (API key vs OAuth token).
//! - [`ExtractRule`] — how the secret is extracted from a location.
//! - [`CredentialSource`] — a declarative credential location.
//! - [`DetectedCredential`] — the result of probing a source.
//!
//! # Concurrency model
//! All types are plain data (`Send + Sync`). The free functions
//! [`embedded_sources`] and [`detect_local_sources`] perform only read-only
//! filesystem/environment access and hold no shared state, so they are safe
//! to call concurrently from multiple threads.
//!
//! # Error handling
//! This module is intentionally *infallible*: probing that cannot complete
//! (missing file, unreadable file, absent JSON field, unset env var) is
//! reported as `exists = false` rather than an error. [`detect_local_sources`]
//! therefore returns an empty [`Vec`] when nothing is found.
//!
//! # Examples
//! ```rust,no_run
//! use harw_model_catalog::sources::{detect_local_sources, embedded_sources};
//!
//! let all = embedded_sources();
//! assert!(!all.is_empty());
//!
//! for detected in detect_local_sources("openai") {
//!     if detected.exists {
//!         println!("found credential ref: {}", detected.secret_ref);
//!     }
//! }
//! ```

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Kind of credential a [`CredentialSource`] yields.
///
/// # Description
/// Distinguishes a long-lived provider API key from a (typically shorter-lived)
/// OAuth access token. Serialized in kebab-case (`api-key`, `oauth-token`).
///
/// See `CONTRACT-setup-install.md`, section „src/sources.rs".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceKind {
    /// A long-lived provider API key.
    ApiKey,
    /// An OAuth access token.
    OAuthToken,
}

/// Rule describing how to extract a secret from a location.
///
/// # Description
/// - [`ExtractRule::JsonPointer`] — read a JSON file and dereference the given
///   [RFC 6901](https://www.rfc-editor.org/rfc/rfc6901) pointer (e.g.
///   `/OPENAI_API_KEY`).
/// - [`ExtractRule::EnvVar`] — read the named environment variable.
/// - [`ExtractRule::WholeFile`] — treat the entire file contents as the secret.
///
/// See `CONTRACT-setup-install.md`, section „src/sources.rs".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExtractRule {
    /// Dereference an RFC 6901 JSON pointer inside the source file.
    JsonPointer(String),
    /// Read the named environment variable.
    EnvVar(String),
    /// Use the whole file contents as the secret.
    WholeFile,
}

/// A declarative description of where a provider credential lives.
///
/// # Description
/// Describes a credential location without ever reading its secret value. The
/// [`CredentialSource::path`] may contain a leading `~` which is expanded
/// against `$HOME` during detection.
///
/// See `CONTRACT-setup-install.md`, section „src/sources.rs".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialSource {
    /// Stable identifier of the source (e.g. `"codex"`, `"claude-cli"`).
    pub id: String,
    /// Catalog provider id this source belongs to (e.g. `"openai"`).
    pub provider: String,
    /// Location of the credential; a leading `~` is allowed.
    pub path: String,
    /// How the secret is extracted from [`CredentialSource::path`].
    pub extract: ExtractRule,
    /// Kind of credential produced.
    pub kind: SourceKind,
}

/// Result of probing a [`CredentialSource`] on the local machine.
///
/// # Description
/// Produced by [`detect_local_sources`]. [`DetectedCredential::exists`] is
/// `true` only when the underlying file (and, for JSON pointer rules, the
/// pointed-to field) or environment variable was actually found.
///
/// See `CONTRACT-setup-install.md`, section „src/sources.rs".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedCredential {
    /// The source that was probed.
    pub source: CredentialSource,
    /// Whether the credential (file/field/env var) was found.
    pub exists: bool,
    /// Ready-to-use reference string.
    ///
    /// Formats:
    /// - `file-json:<abs>#<pointer>` for [`ExtractRule::JsonPointer`]
    /// - `file:<abs>` for [`ExtractRule::WholeFile`]
    /// - `env:<VAR>` for [`ExtractRule::EnvVar`]
    pub secret_ref: String,
}

/// Returns the embedded starter credential sources.
///
/// # Description
/// Provides the built-in set of known local credential locations for the
/// `codex` and `claude-cli` tooling, including an OAuth variant for `codex`.
///
/// # Returns
/// A [`Vec<CredentialSource>`] of embedded sources (never empty).
///
/// # Concurrency
/// Pure; allocates a fresh `Vec` per call. Safe to call from any thread.
///
/// # Examples
/// ```rust
/// use harw_model_catalog::sources::embedded_sources;
/// let sources = embedded_sources();
/// assert!(sources.iter().any(|s| s.id == "codex"));
/// ```
pub fn embedded_sources() -> Vec<CredentialSource> {
    vec![
        // Codex — API key stored in ~/.codex/auth.json.
        CredentialSource {
            id: "codex".to_owned(),
            provider: "openai".to_owned(),
            path: "~/.codex/auth.json".to_owned(),
            extract: ExtractRule::JsonPointer("/OPENAI_API_KEY".to_owned()),
            kind: SourceKind::ApiKey,
        },
        // Codex — OAuth access token variant in the same file.
        CredentialSource {
            id: "codex-oauth".to_owned(),
            provider: "openai".to_owned(),
            path: "~/.codex/auth.json".to_owned(),
            extract: ExtractRule::JsonPointer("/tokens/access_token".to_owned()),
            kind: SourceKind::OAuthToken,
        },
        // Claude CLI — OAuth token stored in ~/.claude/.credentials.json.
        CredentialSource {
            id: "claude-cli".to_owned(),
            provider: "anthropic".to_owned(),
            path: "~/.claude/.credentials.json".to_owned(),
            extract: ExtractRule::JsonPointer("/claudeAiOauth/accessToken".to_owned()),
            kind: SourceKind::OAuthToken,
        },
        // Claude setup-token — long-lived OAuth token exported by Claude Code as
        // `CLAUDE_CODE_OAUTH_TOKEN` (see `harw auth login/token anthropic`).
        CredentialSource {
            id: "claude-setup-token".to_owned(),
            provider: "anthropic".to_owned(),
            path: String::new(),
            extract: ExtractRule::EnvVar("CLAUDE_CODE_OAUTH_TOKEN".to_owned()),
            kind: SourceKind::OAuthToken,
        },
        // Gemini and Mistral publish API-key based developer interfaces. These
        // entries deliberately describe only those public environment paths;
        // no consumer-CLI credential files are inspected.
        CredentialSource {
            id: "gemini-env".to_owned(),
            provider: "gemini".to_owned(),
            path: String::new(),
            extract: ExtractRule::EnvVar("GEMINI_API_KEY".to_owned()),
            kind: SourceKind::ApiKey,
        },
        CredentialSource {
            id: "google-api-env".to_owned(),
            provider: "gemini".to_owned(),
            path: String::new(),
            extract: ExtractRule::EnvVar("GOOGLE_API_KEY".to_owned()),
            kind: SourceKind::ApiKey,
        },
        CredentialSource {
            id: "mistral-env".to_owned(),
            provider: "mistral".to_owned(),
            path: String::new(),
            extract: ExtractRule::EnvVar("MISTRAL_API_KEY".to_owned()),
            kind: SourceKind::ApiKey,
        },
    ]
}

/// Detects local credential sources for the given catalog provider.
///
/// # Description
/// Filters [`embedded_sources`] down to those matching `provider`, then probes
/// each one read-only: it expands a leading `~`, checks whether the file
/// exists, and — for [`ExtractRule::JsonPointer`] — whether the pointed-to
/// field is present (parsed via `serde_json`). For [`ExtractRule::EnvVar`] it
/// checks whether the variable is set. No secret values are stored.
///
/// # Arguments
/// - `provider` (`&str`): catalog provider id to filter sources by.
///
/// # Returns
/// A [`Vec<DetectedCredential>`] — one entry per matching source. The vector is
/// empty when no source matches the provider. Individual entries may have
/// `exists = false` when the source is configured but not present locally.
///
/// # Concurrency
/// Read-only; performs filesystem and environment reads but no mutation. Safe
/// to call concurrently.
///
/// # Examples
/// ```rust,no_run
/// use harw_model_catalog::sources::detect_local_sources;
/// let found = detect_local_sources("openai");
/// for c in &found {
///     println!("{} exists={}", c.secret_ref, c.exists);
/// }
/// ```
pub fn detect_local_sources(provider: &str) -> Vec<DetectedCredential> {
    embedded_sources()
        .into_iter()
        .filter(|source| source.provider == provider)
        .map(probe_source)
        .collect()
}

/// Probes a single source read-only, producing a [`DetectedCredential`].
///
/// Never fails: any I/O or parse problem is reported as `exists = false`.
fn probe_source(source: CredentialSource) -> DetectedCredential {
    match &source.extract {
        ExtractRule::JsonPointer(pointer) => {
            let abs = expand_tilde(&source.path);
            let abs_str = abs.to_string_lossy().into_owned();
            let exists = json_pointer_present(&abs, pointer);
            let secret_ref = format!("file-json:{abs_str}#{pointer}");
            DetectedCredential {
                source,
                exists,
                secret_ref,
            }
        }
        ExtractRule::WholeFile => {
            let abs = expand_tilde(&source.path);
            let abs_str = abs.to_string_lossy().into_owned();
            let exists = abs.is_file();
            let secret_ref = format!("file:{abs_str}");
            DetectedCredential {
                source,
                exists,
                secret_ref,
            }
        }
        ExtractRule::EnvVar(var) => {
            let exists = std::env::var_os(var).is_some();
            let secret_ref = format!("env:{var}");
            DetectedCredential {
                source,
                exists,
                secret_ref,
            }
        }
    }
}

/// Expands a leading `~` in `path` against `$HOME`.
///
/// If `$HOME` is unset or the path does not start with `~`, the path is used
/// verbatim.
fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return home.join(rest);
        }
    }
    if path == "~" {
        if let Some(home) = home_dir() {
            return home;
        }
    }
    PathBuf::from(path)
}

/// Returns the user's home directory from `$HOME`, if set and non-empty.
fn home_dir() -> Option<PathBuf> {
    match std::env::var_os("HOME") {
        Some(value) if !value.is_empty() => Some(PathBuf::from(value)),
        _ => None,
    }
}

/// Returns `true` if `path` is a readable JSON file whose `pointer` resolves to
/// a non-empty string.
///
/// A present-but-`null` field (e.g. `OPENAI_API_KEY` in a `~/.codex/auth.json`
/// written by an OAuth login) does **not** count as detected: the value cannot
/// be used as a credential, so offering it as a resolvable source would make
/// the onboarding wizard propose a reference that fails at resolution time.
/// Any read or parse failure yields `false` (read-only, infallible).
fn json_pointer_present(path: &std::path::Path, pointer: &str) -> bool {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&contents) else {
        return false;
    };
    value
        .pointer(pointer)
        .and_then(serde_json::Value::as_str)
        .is_some_and(|secret| !secret.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Minimal temp-dir helper that cleans up on drop; avoids external deps
    /// and any environment mutation.
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            let mut path = std::env::temp_dir();
            let unique = format!(
                "harw-sources-{tag}-{}-{:p}",
                std::process::id(),
                &tag as *const _
            );
            path.push(unique);
            std::fs::create_dir_all(&path).expect("test setup: create temp dir");
            Self { path }
        }

        fn write(&self, name: &str, contents: &str) -> PathBuf {
            let file = self.path.join(name);
            let mut handle = std::fs::File::create(&file).expect("test setup: create file");
            handle
                .write_all(contents.as_bytes())
                .expect("test setup: write file");
            file
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn test_embedded_sources_contains_codex_and_claude() {
        let sources = embedded_sources();
        assert!(sources.iter().any(|s| s.id == "codex"));
        assert!(sources.iter().any(|s| s.id == "codex-oauth"));
        assert!(sources.iter().any(|s| s.id == "claude-cli"));
        assert!(sources.iter().any(|s| s.id == "gemini-env"));
        assert!(sources.iter().any(|s| s.id == "mistral-env"));
    }

    #[test]
    fn test_embedded_sources_codex_apikey_rule() {
        let sources = embedded_sources();
        let codex = sources
            .iter()
            .find(|s| s.id == "codex")
            .expect("codex source present");
        assert_eq!(codex.kind, SourceKind::ApiKey);
        assert_eq!(
            codex.extract,
            ExtractRule::JsonPointer("/OPENAI_API_KEY".to_owned())
        );
    }

    #[test]
    fn test_detect_local_sources_unknown_provider_is_empty() {
        let found = detect_local_sources("no-such-provider");
        assert!(found.is_empty());
    }

    #[test]
    fn test_json_pointer_present_true_when_field_exists() {
        let dir = TempDir::new("present");
        let file = dir.write("auth.json", r#"{"OPENAI_API_KEY":"sk-xyz"}"#);
        assert!(json_pointer_present(&file, "/OPENAI_API_KEY"));
    }

    #[test]
    fn test_json_pointer_present_false_when_field_missing() {
        let dir = TempDir::new("missing");
        let file = dir.write("auth.json", r#"{"OTHER":"value"}"#);
        assert!(!json_pointer_present(&file, "/OPENAI_API_KEY"));
    }

    #[test]
    fn test_json_pointer_present_false_when_file_absent() {
        let dir = TempDir::new("absent");
        let file = dir.path.join("does-not-exist.json");
        assert!(!json_pointer_present(&file, "/OPENAI_API_KEY"));
    }

    #[test]
    fn test_json_pointer_present_false_on_invalid_json() {
        let dir = TempDir::new("invalid");
        let file = dir.write("auth.json", "not json {");
        assert!(!json_pointer_present(&file, "/OPENAI_API_KEY"));
    }

    #[test]
    fn test_probe_source_builds_file_json_secret_ref() {
        let dir = TempDir::new("ref");
        let file = dir.write("auth.json", r#"{"tokens":{"access_token":"a"}}"#);
        let source = CredentialSource {
            id: "codex-oauth".to_owned(),
            provider: "openai".to_owned(),
            path: file.to_string_lossy().into_owned(),
            extract: ExtractRule::JsonPointer("/tokens/access_token".to_owned()),
            kind: SourceKind::OAuthToken,
        };
        let detected = probe_source(source);
        assert!(detected.exists);
        let expected = format!("file-json:{}#/tokens/access_token", file.to_string_lossy());
        assert_eq!(detected.secret_ref, expected);
    }

    #[test]
    fn test_probe_source_whole_file_ref() {
        let dir = TempDir::new("whole");
        let file = dir.write("token.txt", "secret");
        let source = CredentialSource {
            id: "raw".to_owned(),
            provider: "custom".to_owned(),
            path: file.to_string_lossy().into_owned(),
            extract: ExtractRule::WholeFile,
            kind: SourceKind::ApiKey,
        };
        let detected = probe_source(source);
        assert!(detected.exists);
        assert_eq!(
            detected.secret_ref,
            format!("file:{}", file.to_string_lossy())
        );
    }

    #[test]
    fn test_probe_source_env_var_ref_format() {
        let source = CredentialSource {
            id: "env".to_owned(),
            provider: "custom".to_owned(),
            path: String::new(),
            extract: ExtractRule::EnvVar("HARW_TEST_UNSET_VAR_XYZ".to_owned()),
            kind: SourceKind::ApiKey,
        };
        let detected = probe_source(source);
        assert!(!detected.exists);
        assert_eq!(detected.secret_ref, "env:HARW_TEST_UNSET_VAR_XYZ");
    }

    #[test]
    fn test_expand_tilde_no_home_prefix_verbatim() {
        let p = expand_tilde("/etc/hosts");
        assert_eq!(p, PathBuf::from("/etc/hosts"));
    }

    #[test]
    fn test_json_pointer_present_false_when_field_is_null() {
        let dir = TempDir::new("null-field");
        let file = dir.write(
            "auth.json",
            r#"{"OPENAI_API_KEY": null, "tokens": {"access_token": "sk-abc"}}"#,
        );
        assert!(!json_pointer_present(&file, "/OPENAI_API_KEY"));
        assert!(json_pointer_present(&file, "/tokens/access_token"));
    }

    #[test]
    fn test_json_pointer_present_false_when_string_is_empty() {
        let dir = TempDir::new("empty-string");
        let file = dir.write("auth.json", r#"{"k":""}"#);
        assert!(!json_pointer_present(&file, "/k"));
    }

    #[test]
    fn test_json_pointer_present_false_when_value_is_number() {
        let dir = TempDir::new("number-value");
        let file = dir.write("auth.json", r#"{"k":42}"#);
        assert!(!json_pointer_present(&file, "/k"));
    }

    #[test]
    fn test_json_pointer_present_false_when_value_is_object() {
        let dir = TempDir::new("object-value");
        let file = dir.write("auth.json", r#"{"k":{"nested":"value"}}"#);
        assert!(!json_pointer_present(&file, "/k"));
    }

    #[test]
    fn test_json_pointer_present_false_when_value_is_array() {
        let dir = TempDir::new("array-value");
        let file = dir.write("auth.json", r#"{"k":["a","b"]}"#);
        assert!(!json_pointer_present(&file, "/k"));
    }

    #[test]
    fn test_json_pointer_present_true_when_string_non_empty() {
        let dir = TempDir::new("non-empty-string");
        let file = dir.write("auth.json", r#"{"k":"sk-abc"}"#);
        assert!(json_pointer_present(&file, "/k"));
    }

    #[test]
    fn test_probe_source_null_field_yields_exists_false() {
        let dir = TempDir::new("probe-null-field");
        let file = dir.write(
            "auth.json",
            r#"{"OPENAI_API_KEY": null, "tokens": {"access_token": "sk-abc"}}"#,
        );

        let api_key_source = CredentialSource {
            id: "codex".to_owned(),
            provider: "openai".to_owned(),
            path: file.to_string_lossy().into_owned(),
            extract: ExtractRule::JsonPointer("/OPENAI_API_KEY".to_owned()),
            kind: SourceKind::ApiKey,
        };
        let detected = probe_source(api_key_source);
        assert!(!detected.exists);

        let oauth_source = CredentialSource {
            id: "codex-oauth".to_owned(),
            provider: "openai".to_owned(),
            path: file.to_string_lossy().into_owned(),
            extract: ExtractRule::JsonPointer("/tokens/access_token".to_owned()),
            kind: SourceKind::OAuthToken,
        };
        let detected_oauth = probe_source(oauth_source);
        assert!(detected_oauth.exists);
    }
}
