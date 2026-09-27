//! Seatbelt (SBPL) profile generation and `sandbox-exec` wrapping.
//!
//! Darwin's kernel sandbox (Seatbelt) is reachable without `unsafe` through
//! `/usr/bin/sandbox-exec -p <profile> <program> <args…>`: `sandbox-exec`
//! applies the profile to itself and then `exec`s the program **in the same
//! process** (same PID, same process group), so a `DarwinProcess` spawned
//! from the wrapped command supervises the job directly.
//!
//! # Status of the mechanism (read before relying on it)
//! `sandbox-exec(1)` and SBPL are marked **deprecated** by Apple and are not
//! a documented, stable interface, but they work on current macOS and are
//! what Apple's own tools and many build systems use. The profile language
//! can change between releases. That is why enforcement through this module
//! is reported as [`harw_job_core::EnforcementState::Partial`], never
//! `Enforced` (see [`crate::DarwinSandbox`]).
//!
//! # Generated profile
//! - `(deny default)`: everything not allowed below is denied.
//! - Process basics: exec, fork, signals within the same sandbox, own
//!   process info, `sysctl-read`, `mach-lookup` (needed by libSystem; this
//!   is a coarse allowance and part of why the result is only partial).
//! - Read access: metadata everywhere; data under the system paths in
//!   [`SYSTEM_READ_PATHS`].
//! - Write access: `/dev/null`, `/dev/zero`, `/dev/tty`, `/dev/dtracehelper`.
//! - Workspace: read (all profiles) and write (all but
//!   [`SandboxProfileName::ReadOnlyAnalysis`]).
//! - Network: allowed for `WorkspaceBuild` and `ReadOnlyAnalysis`; denied for
//!   `NoNetwork` **and** `NetworkRestricted` (a profile name carries no
//!   allowlist, so the restricted case fails closed to "no network").
//!
//! The temporary directory (`$TMPDIR`, under `/private/var/folders`) is not
//! writable; point `TMPDIR` into the workspace if the job needs one.
//!
//! Seatbelt matches **resolved** paths (`/private/tmp`, not `/tmp`). The
//! generator does not touch the filesystem; callers pass a canonical path
//! (`DarwinSandbox::wrap` canonicalizes for them).

use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

use harw_job_core::SandboxProfileName;

use crate::error::SandboxError;

/// Absolute path of the `sandbox-exec` binary on macOS.
pub const SANDBOX_EXEC_PATH: &str = "/usr/bin/sandbox-exec";

/// System subpaths whose contents are readable inside the sandbox (dyld, the
/// shared cache, libSystem, `/bin/sh` and its `/private/var/select` link,
/// `/etc`, timezone data, devices).
pub const SYSTEM_READ_PATHS: &[&str] = &[
    "/usr",
    "/bin",
    "/sbin",
    "/System",
    "/Library",
    "/private/etc",
    "/private/var/db/timezone",
    "/private/var/db/dyld",
    "/private/var/select",
    "/dev",
];

/// Device files that may be written.
const DEVICE_WRITE_PATHS: &[&str] = &["/dev/null", "/dev/zero", "/dev/tty", "/dev/dtracehelper"];

/// Whether `profile` grants write access to the workspace.
const fn workspace_writable(profile: SandboxProfileName) -> bool {
    !matches!(profile, SandboxProfileName::ReadOnlyAnalysis)
}

/// Whether `profile` allows network access.
const fn network_allowed(profile: SandboxProfileName) -> bool {
    matches!(
        profile,
        SandboxProfileName::WorkspaceBuild | SandboxProfileName::ReadOnlyAnalysis
    )
}

/// Renders `path` as an SBPL string literal (`"…"` with `\` and `"`
/// escaped).
fn sbpl_string(path: &Path) -> Result<String, SandboxError> {
    let Some(text) = path.to_str() else {
        return Err(SandboxError::UnrepresentablePath {
            path: path.to_path_buf(),
            reason: "not valid UTF-8",
        });
    };
    if text.chars().any(char::is_control) {
        return Err(SandboxError::UnrepresentablePath {
            path: path.to_path_buf(),
            reason: "contains control characters",
        });
    }
    let mut literal = String::with_capacity(text.len() + 2);
    literal.push('"');
    for character in text.chars() {
        if matches!(character, '"' | '\\') {
            literal.push('\\');
        }
        literal.push(character);
    }
    literal.push('"');
    Ok(literal)
}

/// Generates the SBPL profile for `profile` with `workspace_root` as the
/// only non-system path (see module docs for the rules).
///
/// `workspace_root` must be absolute and should be canonical.
///
/// # Errors
/// [`SandboxError::RelativeWorkspaceRoot`] for a relative path,
/// [`SandboxError::UnrepresentablePath`] for a non-UTF-8 path or one with
/// control characters.
pub fn sbpl_profile_for(
    profile: &SandboxProfileName,
    workspace_root: &Path,
) -> Result<String, SandboxError> {
    let profile = *profile;
    if !workspace_root.is_absolute() {
        return Err(SandboxError::RelativeWorkspaceRoot {
            path: workspace_root.to_path_buf(),
        });
    }
    let workspace = sbpl_string(workspace_root)?;

    let mut out = String::new();
    // `write!` into a String cannot fail; the results are ignored on purpose.
    let _ = writeln!(out, "(version 1)");
    let _ = writeln!(out, "; harw-job-darwin generated profile: {profile:?}");
    let _ = writeln!(out, "(deny default)");
    let _ = writeln!(out, "(allow process-exec)");
    let _ = writeln!(out, "(allow process-fork)");
    let _ = writeln!(out, "(allow signal (target same-sandbox))");
    let _ = writeln!(out, "(allow process-info* (target self))");
    let _ = writeln!(out, "(allow sysctl-read)");
    let _ = writeln!(out, "(allow mach-lookup)");
    let _ = writeln!(out, "(allow file-read-metadata)");
    let _ = writeln!(out, "(allow file-read-data (literal \"/\"))");
    for path in SYSTEM_READ_PATHS {
        let _ = writeln!(out, "(allow file-read* (subpath \"{path}\"))");
    }
    for path in DEVICE_WRITE_PATHS {
        let _ = writeln!(out, "(allow file-write-data (literal \"{path}\"))");
    }
    let _ = writeln!(out, "(allow file-ioctl (literal \"/dev/dtracehelper\"))");
    let _ = writeln!(out, "(allow file-read* (subpath {workspace}))");
    if workspace_writable(profile) {
        let _ = writeln!(out, "(allow file-write* (subpath {workspace}))");
    }
    if network_allowed(profile) {
        let _ = writeln!(out, "(allow network*)");
        let _ = writeln!(out, "(allow system-socket)");
    } else {
        let _ = writeln!(out, "(deny network*)");
    }
    Ok(out)
}

/// Builds `/usr/bin/sandbox-exec -p <sbpl_profile> <program> <args…>` from
/// `command`, copying its program, arguments, explicit environment changes
/// and working directory.
///
/// Not copied (not observable on `Command`): `env_clear`, stdio, `uid`/`gid`
/// and `process_group`. Configure those on the returned command
/// (`DarwinProcess::spawn` sets the process group itself). Pass an absolute
/// program path: `sandbox-exec` resolves bare names through `PATH`.
///
/// # Errors
/// [`SandboxError::InvalidProgram`] if the program name starts with `-`
/// (it would be parsed as a `sandbox-exec` option).
pub fn wrap_with_sandbox_exec(
    command: &Command,
    sbpl_profile: &str,
) -> Result<Command, SandboxError> {
    let program = command.get_program();
    if program.as_encoded_bytes().first() == Some(&b'-') {
        return Err(SandboxError::InvalidProgram {
            program: program.into(),
            reason: "starts with '-' (would be read as a sandbox-exec option)",
        });
    }
    let mut wrapped = Command::new(SANDBOX_EXEC_PATH);
    wrapped
        .arg("-p")
        .arg(sbpl_profile)
        .arg(program)
        .args(command.get_args());
    copy_env_and_cwd(command, &mut wrapped);
    Ok(wrapped)
}

/// Copies the explicit environment changes and the working directory of
/// `from` onto `to`.
pub(crate) fn copy_env_and_cwd(from: &Command, to: &mut Command) {
    for (key, value) in from.get_envs() {
        match value {
            Some(value) => to.env(key, value),
            None => to.env_remove(key),
        };
    }
    if let Some(dir) = from.get_current_dir() {
        to.current_dir(dir);
    }
}

#[cfg(test)]
mod tests {
    use super::{SANDBOX_EXEC_PATH, sbpl_profile_for, sbpl_string, wrap_with_sandbox_exec};
    use crate::error::SandboxError;
    use crate::test_support::{TestResult, ctx};
    use harw_job_core::SandboxProfileName;
    use std::ffi::OsStr;
    use std::path::Path;
    use std::process::Command;

    const ALL: [SandboxProfileName; 4] = [
        SandboxProfileName::WorkspaceBuild,
        SandboxProfileName::ReadOnlyAnalysis,
        SandboxProfileName::NoNetwork,
        SandboxProfileName::NetworkRestricted,
    ];

    #[test]
    fn test_profile_denies_by_default_and_scopes_the_workspace() -> TestResult {
        for profile in ALL {
            let text =
                sbpl_profile_for(&profile, Path::new("/Users/dev/ws")).map_err(ctx("profile"))?;
            assert!(text.starts_with("(version 1)\n"), "{text}");
            assert!(text.contains("(deny default)"), "{text}");
            assert!(
                text.contains("(allow file-read* (subpath \"/Users/dev/ws\"))"),
                "{text}"
            );
            assert!(text.contains("(allow file-read* (subpath \"/usr\"))"));
            // No blanket write or read of the whole filesystem.
            assert!(!text.contains("(allow file-write*)"), "{text}");
            assert!(!text.contains("(allow file-read*)"), "{text}");
            assert!(!text.contains("(allow default)"), "{text}");
        }
        Ok(())
    }

    #[test]
    fn test_profile_write_and_network_rules_follow_the_profile_name() -> TestResult {
        let ws = Path::new("/ws");
        let write = "(allow file-write* (subpath \"/ws\"))";
        let cases = [
            (SandboxProfileName::WorkspaceBuild, true, true),
            (SandboxProfileName::ReadOnlyAnalysis, false, true),
            (SandboxProfileName::NoNetwork, true, false),
            (SandboxProfileName::NetworkRestricted, true, false),
        ];
        for (profile, writable, network) in cases {
            let text = sbpl_profile_for(&profile, ws).map_err(ctx("profile"))?;
            assert_eq!(text.contains(write), writable, "{profile:?}: {text}");
            assert_eq!(text.contains("(allow network*)"), network, "{profile:?}");
            assert_eq!(text.contains("(deny network*)"), !network, "{profile:?}");
        }
        Ok(())
    }

    #[test]
    fn test_profile_rejects_relative_and_unrepresentable_paths() -> TestResult {
        assert!(matches!(
            sbpl_profile_for(&SandboxProfileName::NoNetwork, Path::new("ws")),
            Err(SandboxError::RelativeWorkspaceRoot { .. })
        ));
        assert!(matches!(
            sbpl_profile_for(
                &SandboxProfileName::NoNetwork,
                Path::new("/ws\n(allow default)")
            ),
            Err(SandboxError::UnrepresentablePath { .. })
        ));
        // Quotes and backslashes are escaped, so they cannot close the literal.
        assert_eq!(
            sbpl_string(Path::new("/a\"b\\c")).map_err(ctx("escape"))?,
            "\"/a\\\"b\\\\c\""
        );
        let text = sbpl_profile_for(
            &SandboxProfileName::NoNetwork,
            Path::new("/x\") (allow default"),
        )
        .map_err(ctx("quoted path"))?;
        assert!(!text.contains("\n(allow default"), "{text}");
        Ok(())
    }

    #[test]
    fn test_wrap_builds_sandbox_exec_argv_and_copies_env_and_cwd() -> TestResult {
        let mut command = Command::new("/bin/echo");
        command
            .args(["a b", "c"])
            .env("HARW_X", "1")
            .env_remove("HARW_Y")
            .current_dir("/tmp");
        let wrapped = wrap_with_sandbox_exec(&command, "(version 1)").map_err(ctx("wrap"))?;
        assert_eq!(wrapped.get_program(), OsStr::new(SANDBOX_EXEC_PATH));
        let args: Vec<&OsStr> = wrapped.get_args().collect();
        assert_eq!(args, ["-p", "(version 1)", "/bin/echo", "a b", "c"]);
        let envs: Vec<(&OsStr, Option<&OsStr>)> = wrapped.get_envs().collect();
        assert!(envs.contains(&(OsStr::new("HARW_X"), Some(OsStr::new("1")))));
        assert!(envs.contains(&(OsStr::new("HARW_Y"), None)));
        assert_eq!(wrapped.get_current_dir(), Some(Path::new("/tmp")));

        assert!(matches!(
            wrap_with_sandbox_exec(&Command::new("-e"), "(version 1)"),
            Err(SandboxError::InvalidProgram { .. })
        ));
        Ok(())
    }
}
