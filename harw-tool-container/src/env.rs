//! Environment handling, in two separate directions.
//!
//! 1. **Engine environment**: what the `podman` process itself sees. It is
//!    built from an allowlist, never inherited. Variables that redirect an
//!    engine to another socket, host or credential file
//!    ([`REDIRECTING_VARIABLES`]) can therefore never reach it.
//! 2. **Container environment**: what the command inside the container sees.
//!    Only keys on the configured allowlist, values bounded and single-line.

use crate::error::ContainerPolicyError;

/// Variables that can point an engine at another daemon, remote host or
/// credential store. None of them is ever passed on.
pub const REDIRECTING_VARIABLES: [&str; 12] = [
    "DOCKER_HOST",
    "DOCKER_CONTEXT",
    "DOCKER_CONFIG",
    "DOCKER_API_VERSION",
    "CONTAINER_HOST",
    "CONTAINER_CONNECTION",
    "CONTAINER_SSHKEY",
    "CONTAINERS_CONF",
    "CONTAINERS_STORAGE_CONF",
    "REGISTRY_AUTH_FILE",
    "PODMAN_COMPOSE_PROVIDER",
    "XDG_CONFIG_HOME",
];

/// Container environment keys allowed unless the config narrows or widens it.
pub const DEFAULT_ENV_ALLOW: [&str; 5] = [
    "CARGO_TERM_COLOR",
    "RUST_BACKTRACE",
    "RUST_LOG",
    "CI",
    "TERM",
];

/// Fixed `PATH` of the engine process.
const ENGINE_PATH: &str = "/usr/bin:/bin";
/// Longest accepted environment value in bytes.
const MAX_VALUE_BYTES: usize = 4096;

/// Checks an environment key: `[A-Z_][A-Z0-9_]{0,63}`.
pub(crate) fn check_key(key: &str) -> Result<(), ContainerPolicyError> {
    let err = ContainerPolicyError::InvalidEnv;
    let mut chars = key.chars();
    let first = chars.next().ok_or(err("empty key"))?;
    if !(first.is_ascii_uppercase() || first == '_') {
        return Err(err("key must start with A-Z or '_'"));
    }
    if key.len() > 64 || !chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_') {
        return Err(err("key must match [A-Z_][A-Z0-9_]{0,63}"));
    }
    Ok(())
}

/// Checks an environment value: bounded, no NUL, no line break.
pub(crate) fn check_value(value: &str) -> Result<(), ContainerPolicyError> {
    let err = ContainerPolicyError::InvalidEnv;
    if value.len() > MAX_VALUE_BYTES {
        return Err(err("value too long"));
    }
    if value.contains(['\0', '\n', '\r']) {
        return Err(err("value contains NUL or a line break"));
    }
    Ok(())
}

/// Builds the engine process environment from an allowlist.
///
/// `lookup` reads the caller's environment (a closure so this stays pure).
/// Returned: `HOME` and `XDG_RUNTIME_DIR` when set to a plain absolute path,
/// a fixed `PATH`, and `SSH_AUTH_SOCK` only when `remote` is `true`. Nothing
/// else is returned, whatever `lookup` offers.
#[must_use]
pub fn engine_environment(
    lookup: &dyn Fn(&str) -> Option<String>,
    remote: bool,
) -> Vec<(String, String)> {
    let plain_abs =
        |v: &str| v.starts_with('/') && v.len() <= 4096 && !v.contains(['\0', '\n', '\r']);
    let mut env = vec![("PATH".to_owned(), ENGINE_PATH.to_owned())];
    let mut keys = vec!["HOME", "XDG_RUNTIME_DIR"];
    if remote {
        keys.push("SSH_AUTH_SOCK");
    }
    for key in keys {
        if let Some(value) = lookup(key).filter(|v| plain_abs(v)) {
            env.push((key.to_owned(), value));
        }
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ensure};

    fn full_env(key: &str) -> Option<String> {
        match key {
            "HOME" => Some("/home/u".to_owned()),
            "XDG_RUNTIME_DIR" => Some("/run/user/1000".to_owned()),
            "SSH_AUTH_SOCK" => Some("/tmp/agent.sock".to_owned()),
            "PATH" => Some("/evil".to_owned()),
            other => Some(format!("redirect-{other}")),
        }
    }

    #[test]
    fn engine_env_is_an_allowlist() -> TestResult {
        let env = engine_environment(&full_env, false);
        let keys: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
        ensure(keys == ["PATH", "HOME", "XDG_RUNTIME_DIR"], "local keys")?;
        ensure(env[0].1 == "/usr/bin:/bin", "path is fixed")?;
        for var in REDIRECTING_VARIABLES {
            ensure(!keys.contains(&var), var)?;
        }
        Ok(())
    }

    #[test]
    fn ssh_agent_only_in_remote_mode() -> TestResult {
        let local = engine_environment(&full_env, false);
        ensure(!local.iter().any(|(k, _)| k == "SSH_AUTH_SOCK"), "local")?;
        let remote = engine_environment(&full_env, true);
        ensure(remote.iter().any(|(k, _)| k == "SSH_AUTH_SOCK"), "remote")
    }

    #[test]
    fn engine_env_drops_implausible_values() -> TestResult {
        let bad = |key: &str| match key {
            "HOME" => Some("relative".to_owned()),
            "XDG_RUNTIME_DIR" => Some("/run/user/1\n000".to_owned()),
            _ => None,
        };
        let env = engine_environment(&bad, false);
        ensure(env.len() == 1 && env[0].0 == "PATH", "only PATH remains")
    }

    #[test]
    fn container_env_keys_and_values_are_bounded() -> TestResult {
        ensure(check_key("RUST_LOG").is_ok(), "key")?;
        for bad in ["", "rust", "1A", "A-B", "A B", &"A".repeat(65)] {
            ensure(check_key(bad).is_err(), bad)?;
        }
        ensure(check_value("info,harw=debug").is_ok(), "value")?;
        ensure(check_value("a\nb").is_err(), "newline")?;
        ensure(check_value("a\0b").is_err(), "nul")?;
        ensure(check_value(&"x".repeat(4097)).is_err(), "long")
    }
}
