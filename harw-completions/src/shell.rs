//! Script generation, ownership markers and shell detection.
//!
//! Spec source: `harw-completions-CODING-DESIGN.md` §1 (src/shell.rs).
//!
//! # Responsibility
//! - Generates completion scripts via `clap_complete` and stamps them with a
//!   harw-managed marker line ([`generate_script`], [`marker_line`]).
//! - Recognises files this crate may replace or remove: marker-stamped files
//!   ([`is_managed_script`]) and unmarked clap-generated scripts for the same
//!   binary ([`has_clap_signature`]); combined in [`is_ours`].
//! - Provides the rc-file managed-block delimiters ([`block_begin`],
//!   [`block_end`]).
//! - Detects the user's shell from `$SHELL` ([`detect_shell`]).
//!
//! # Concurrency
//! Pure functions without shared state; safe to call from any thread.
//!
//! # Errors
//! [`detect_shell`] returns [`CompletionError::ShellUndetected`].
//!
//! # Examples
//! ```
//! use harw_completions::{is_managed_script, marker_line};
//!
//! let content = format!("{}\n", marker_line("harw", "1.2.3"));
//! assert!(is_managed_script(&content, "harw"));
//! ```

use clap_complete::Shell;
use tracing::debug;

use crate::error::{CompletionError, CompletionResult};

/// Prefix of the marker line stamped into every generated script.
pub const MANAGED_MARKER_PREFIX: &str = "# >>> harw-managed completion: ";

/// Returns the marker line `"# >>> harw-managed completion: {bin} v{version} <<<"`.
pub fn marker_line(bin: &str, version: &str) -> String {
    format!("{MANAGED_MARKER_PREFIX}{bin} v{version} <<<")
}

/// Returns the begin delimiter of the managed rc block: `"# >>> harw completions ({bin}) >>>"`.
pub fn block_begin(bin: &str) -> String {
    format!("# >>> harw completions ({bin}) >>>")
}

/// Returns the end delimiter of the managed rc block: `"# <<< harw completions ({bin}) <<<"`.
pub fn block_end(bin: &str) -> String {
    format!("# <<< harw completions ({bin}) <<<")
}

/// Generates the completion script for `bin` and inserts the marker line.
///
/// For zsh the marker goes after a leading `#compdef` line (which must stay
/// first), otherwise it becomes the very first line. The output always ends
/// with `'\n'`.
pub fn generate_script(cmd: &mut clap::Command, bin: &str, shell: Shell) -> Vec<u8> {
    let version = cmd.get_version().unwrap_or("unknown").to_owned();
    let mut generated: Vec<u8> = Vec::new();
    clap_complete::generate(shell, cmd, bin, &mut generated);

    let mut marker = marker_line(bin, &version).into_bytes();
    marker.push(b'\n');

    let mut script: Vec<u8> = Vec::with_capacity(generated.len() + marker.len() + 1);
    if shell == Shell::Zsh && generated.starts_with(b"#compdef") {
        match generated.iter().position(|&b| b == b'\n') {
            Some(newline) => {
                script.extend_from_slice(&generated[..=newline]);
                script.extend_from_slice(&marker);
                script.extend_from_slice(&generated[newline + 1..]);
            }
            None => {
                script.extend_from_slice(&generated);
                script.push(b'\n');
                script.extend_from_slice(&marker);
            }
        }
    } else {
        script.extend_from_slice(&marker);
        script.extend_from_slice(&generated);
    }
    if script.last() != Some(&b'\n') {
        script.push(b'\n');
    }
    debug!(bin, shell = %shell, bytes = script.len(), "generated completion script");
    script
}

/// Returns true if any line starts with `"{MANAGED_MARKER_PREFIX}{bin} v"`.
pub fn is_managed_script(content: &str, bin: &str) -> bool {
    let prefix = format!("{MANAGED_MARKER_PREFIX}{bin} v");
    content.lines().any(|line| line.starts_with(&prefix))
}

// Function-name spellings clap_complete may derive from `bin`: verbatim (zsh),
// '-' -> '_' and '-' -> "__" (bash).
fn name_variants(bin: &str) -> [String; 3] {
    [bin.to_owned(), bin.replace('-', "_"), bin.replace('-', "__")]
}

/// Returns true if `content` carries a clap_complete-generated signature for
/// `bin` (zsh, bash, fish, elvish or powershell form).
pub fn has_clap_signature(content: &str, bin: &str) -> bool {
    let variants = name_variants(bin);

    let compdef = format!("#compdef {bin}");
    let compdef_more = format!("#compdef {bin} ");
    let zsh_header = content
        .lines()
        .any(|line| line == compdef || line.starts_with(&compdef_more));
    let zsh_fn = variants
        .iter()
        .any(|name| content.contains(&format!("_{name}() {{")));
    if zsh_header && zsh_fn {
        return true;
    }

    if variants
        .iter()
        .any(|name| content.contains(&format!("complete -F _{name} ")))
    {
        return true;
    }

    let fish = format!("complete -c {bin} ");
    if content.lines().any(|line| line.starts_with(&fish)) {
        return true;
    }

    if content.contains(&format!("edit:completion:arg-completer[{bin}]")) {
        return true;
    }

    content.contains(&format!(
        "Register-ArgumentCompleter -Native -CommandName '{bin}'"
    ))
}

/// Returns true if the file is harw-managed or clap-generated for `bin`.
pub fn is_ours(content: &str, bin: &str) -> bool {
    is_managed_script(content, bin) || has_clap_signature(content, bin)
}

/// Detects the shell from a `$SHELL` value (basename, `.exe` stripped).
///
/// # Errors
/// [`CompletionError::ShellUndetected`] when the value is unset, empty or
/// names an unsupported shell.
pub fn detect_shell(shell_env: Option<&str>) -> CompletionResult<Shell> {
    let value = shell_env.unwrap_or("").trim();
    let base = value.rsplit(['/', '\\']).next().unwrap_or(value);
    let base = base.strip_suffix(".exe").unwrap_or(base);
    let shell = match base {
        "bash" => Some(Shell::Bash),
        "zsh" => Some(Shell::Zsh),
        "fish" => Some(Shell::Fish),
        "elvish" => Some(Shell::Elvish),
        "pwsh" | "powershell" => Some(Shell::PowerShell),
        _ => None,
    };
    match shell {
        Some(shell) => {
            debug!(value, shell = %shell, "detected shell from $SHELL");
            Ok(shell)
        }
        None => Err(CompletionError::ShellUndetected {
            value: value.to_owned(),
        }),
    }
}
