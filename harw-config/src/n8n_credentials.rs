//! Secure resolution of n8n credential references from the user's profile.
use crate::ConfigError;
use std::{env, fmt, fs, os::unix::fs::{MetadataExt, PermissionsExt}, path::{Path, PathBuf}};

/// Credential wrapper whose Debug representation never reveals the token.
pub struct N8nCredential(String);
impl fmt::Debug for N8nCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("N8nCredential").field(&"[REDACTED]").finish()
    }
}
impl N8nCredential { pub fn expose(&self) -> &str { &self.0 } }

/// Public test-only constructor (feature `test-support`, see
/// harw-config/Cargo.toml) so downstream crates' test builds can create a
/// credential without touching the filesystem; never compiled into
/// production builds. harw-cli enables the feature via its
/// `[dev-dependencies]` entry, so it is available in test builds there (Cargo
/// unifies features across normal and dev dependencies for test targets) and
/// absent from release builds.
#[cfg(feature = "test-support")]
impl N8nCredential {
    #[doc(hidden)]
    pub fn for_tests(token: &str) -> Self {
        Self(token.to_owned())
    }
}

fn profile_root() -> Result<PathBuf, ConfigError> {
    if let Some(value) = env::var_os("HARW_HOME") {
        if value.is_empty() { return Err(ConfigError::Invalid("HARW_HOME is empty".into())); }
        return Ok(PathBuf::from(value));
    }
    env::var_os("HOME").map(PathBuf::from).map(|p| p.join(".harw"))
        .ok_or_else(|| ConfigError::Invalid("cannot resolve home directory for n8n credential".into()))
}

fn validate_alias(reference: &str) -> Result<&str, ConfigError> {
    let alias = reference.strip_prefix("n8n:").unwrap_or(reference);
    if alias.is_empty() || alias.contains("..") || alias.chars().any(char::is_whitespace)
        || !alias.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
    { return Err(ConfigError::Invalid(format!("invalid n8n credential alias: {reference:?}"))); }
    Ok(alias)
}

/// Resolve an alias to `<profile-root>/credentials/n8n/<alias>.token`.
pub fn resolve_n8n_credential(reference: &str) -> Result<N8nCredential, ConfigError> {
    resolve_at(&profile_root()?, reference)
}

fn resolve_at(root: &Path, reference: &str) -> Result<N8nCredential, ConfigError> {
    let alias = validate_alias(reference)?;
    let path = root.join("credentials/n8n").join(format!("{alias}.token"));
    let metadata = fs::symlink_metadata(&path).map_err(|e| ConfigError::Invalid(format!("cannot inspect n8n credential '{}': {e}", path.display())))?;
    if !metadata.file_type().is_file() {
        return Err(ConfigError::Invalid(format!("n8n credential '{}' is not a regular file (symlinks are not allowed)", path.display())));
    }
    let uid = fs::metadata("/proc/self").map(|m| m.uid()).unwrap_or(u32::MAX);
    if metadata.uid() != uid {
        return Err(ConfigError::Invalid(format!("n8n credential '{}' is not owned by the current user", path.display())));
    }
    if metadata.permissions().mode() & 0o7777 != 0o600 {
        return Err(ConfigError::Invalid(format!("n8n credential '{}' must have mode 0600", path.display())));
    }
    let contents = fs::read_to_string(&path).map_err(|e| ConfigError::Invalid(format!("cannot read n8n credential '{}': {e}", path.display())))?;
    Ok(N8nCredential(contents))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn setup() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials/n8n/alias.token");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        (dir, path)
    }
    fn write_token(path: &Path, mode: u32) {
        fs::write(path, "secret").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }
    #[test] fn good_credential() { let (d,p)=setup(); write_token(&p,0o600); assert_eq!(resolve_at(d.path(),"alias").unwrap().expose(),"secret"); }
    #[test] fn bad_mode() { let (d,p)=setup(); write_token(&p,0o640); assert!(resolve_at(d.path(),"alias").unwrap_err().to_string().contains("mode 0600")); }
    #[test] fn symlink_rejected() { let (d,p)=setup(); let target=d.path().join("other"); write_token(&target,0o600); symlink(&target,&p).unwrap(); assert!(resolve_at(d.path(),"alias").unwrap_err().to_string().contains("regular file")); }
    #[test] fn missing_rejected() { let (d,_)=setup(); assert!(resolve_at(d.path(),"alias").unwrap_err().to_string().contains("cannot inspect")); }
    #[test] fn traversal_alias_rejected() { let (d,_)=setup(); assert!(resolve_at(d.path(),"../secret").unwrap_err().to_string().contains("invalid n8n credential alias")); }
    #[test] fn debug_redacts_content() { assert!(!format!("{:?}", N8nCredential("secret".into())).contains("secret")); }
}
