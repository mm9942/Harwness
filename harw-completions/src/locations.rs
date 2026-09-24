//! Per-shell install locations resolved from an injected environment.
//!
//! Spec source: `harw-completions-CODING-DESIGN.md` §1 (src/locations.rs).
//!
//! # Responsibility
//! Maps `(HomeEnv, bin, Shell)` to the canonical completion file, the legacy
//! locations older installs may have used, the managed rc block to insert,
//! the rc files to clean, cache directories and a reload hint. Performs no
//! writes; the only filesystem access is the oh-my-zsh detection probe.
//!
//! # Concurrency
//! Plain data plus pure resolution; safe to call from any thread.
//!
//! # Errors
//! [`CompletionError::HomeMissing`] and [`CompletionError::UnsupportedShell`].
//!
//! # Examples
//! ```
//! use std::path::PathBuf;
//! use harw_completions::{HomeEnv, Shell, resolve_locations};
//!
//! let env = HomeEnv { home: Some(PathBuf::from("/home/u")), ..HomeEnv::default() };
//! let loc = resolve_locations(&env, "harw", Shell::Fish).expect("home is set");
//! assert_eq!(loc.canonical, PathBuf::from("/home/u/.config/fish/completions/harw.fish"));
//! ```

use std::path::{Path, PathBuf};

use clap_complete::Shell;
use tracing::debug;

use crate::error::{CompletionError, CompletionResult};

/// Injected environment. Empty strings from the process env are treated as unset.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HomeEnv {
    /// `$HOME`.
    pub home: Option<PathBuf>,
    /// `$XDG_DATA_HOME`.
    pub xdg_data_home: Option<PathBuf>,
    /// `$XDG_CONFIG_HOME`.
    pub xdg_config_home: Option<PathBuf>,
    /// `$ZSH` (oh-my-zsh directory).
    pub zsh: Option<PathBuf>,
    /// `$ZSH_CUSTOM`.
    pub zsh_custom: Option<PathBuf>,
    /// `$ZDOTDIR`.
    pub zdotdir: Option<PathBuf>,
    /// `$SHELL`.
    pub shell: Option<String>,
    /// Include /etc, /usr/... candidates in the sweep. `from_process()`: true. Default: false (tests).
    pub include_system_paths: bool,
}

// Reads a path variable from the process env; empty values count as unset.
fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

impl HomeEnv {
    /// Captures the relevant variables from the current process environment.
    pub fn from_process() -> Self {
        Self {
            home: env_path("HOME"),
            xdg_data_home: env_path("XDG_DATA_HOME"),
            xdg_config_home: env_path("XDG_CONFIG_HOME"),
            zsh: env_path("ZSH"),
            zsh_custom: env_path("ZSH_CUSTOM"),
            zdotdir: env_path("ZDOTDIR"),
            shell: std::env::var_os("SHELL")
                .filter(|value| !value.is_empty())
                .map(|value| value.to_string_lossy().into_owned()),
            include_system_paths: true,
        }
    }
}

/// Where a managed rc block is placed inside the rc file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockPlacement {
    /// Before the first `compinit` / `oh-my-zsh.sh` line (zsh `fpath`).
    BeforeCompinit,
    /// Appended at the end of the file.
    End,
}

/// A managed block to (re)insert into an rc file on install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RcBlock {
    /// The rc file receiving the block.
    pub rc_file: PathBuf,
    /// Body lines between the begin/end delimiters.
    pub body: Vec<String>,
    /// Placement inside the rc file.
    pub placement: BlockPlacement,
}

/// Resolved locations for one binary and shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Locations {
    /// Target shell.
    pub shell: Shell,
    /// Binary name.
    pub bin: String,
    /// The single place the script is installed to.
    pub canonical: PathBuf,
    /// Older locations swept on install (deduped, canonical excluded, order preserved).
    pub legacy: Vec<PathBuf>,
    /// Managed block to (re)insert on install.
    pub rc_block: Option<RcBlock>,
    /// rc files scanned for old blocks / legacy lines (deduped; includes `rc_block.rc_file`).
    pub rc_cleanup: Vec<PathBuf>,
    /// zsh only: directories scanned for `.zcompdump*` files.
    pub cache_dirs: Vec<PathBuf>,
    /// Command reloading completions, e.g. `"exec zsh"`.
    pub reload_hint: String,
}

// Removes duplicates while keeping the first occurrence.
fn dedup(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::with_capacity(paths.len());
    for path in paths {
        if !out.contains(&path) {
            out.push(path);
        }
    }
    out
}

// Dedups the legacy list and drops the canonical path from it.
fn legacy_without(canonical: &Path, legacy: Vec<PathBuf>) -> Vec<PathBuf> {
    dedup(legacy)
        .into_iter()
        .filter(|path| path != canonical)
        .collect()
}

/// Resolves all locations for `bin` under `shell` (design doc §1, resolution table).
///
/// # Errors
/// - [`CompletionError::HomeMissing`] when `env.home` is `None`.
/// - [`CompletionError::UnsupportedShell`] for shells without a location table.
pub fn resolve_locations(env: &HomeEnv, bin: &str, shell: Shell) -> CompletionResult<Locations> {
    let home = env.home.as_deref().ok_or(CompletionError::HomeMissing)?;
    let data = env
        .xdg_data_home
        .clone()
        .unwrap_or_else(|| home.join(".local/share"));
    let config = env
        .xdg_config_home
        .clone()
        .unwrap_or_else(|| home.join(".config"));

    let (canonical, legacy, rc_block, rc_cleanup, cache_dirs, reload_hint) = match shell {
        Shell::Zsh => {
            let zdot = env.zdotdir.clone().unwrap_or_else(|| home.to_path_buf());
            let omz = env.zsh.clone().unwrap_or_else(|| home.join(".oh-my-zsh"));
            let omz_detected = omz.join("oh-my-zsh.sh").is_file();
            let custom = env.zsh_custom.clone().unwrap_or_else(|| omz.join("custom"));
            let file = format!("_{bin}");
            let site_functions = data.join("zsh/site-functions");

            let (canonical, rc_block) = if omz_detected {
                (custom.join("completions").join(&file), None)
            } else {
                let block = RcBlock {
                    rc_file: zdot.join(".zshrc"),
                    body: vec![format!("fpath=(\"{}\" $fpath)", site_functions.display())],
                    placement: BlockPlacement::BeforeCompinit,
                };
                (site_functions.join(&file), Some(block))
            };
            debug!(omz = %omz.display(), omz_detected, "resolved oh-my-zsh state");

            let mut legacy = vec![
                home.join(".zfunc").join(&file),
                home.join(".zsh/completions").join(&file),
                home.join(".oh-my-zsh/completions").join(&file),
                omz.join("completions").join(&file),
                custom.join("completions").join(&file),
                omz.join("cache/completions").join(&file),
                site_functions.join(&file),
                home.join(".local/share/zsh/site-functions").join(&file),
            ];
            if env.include_system_paths {
                legacy.push(PathBuf::from("/usr/local/share/zsh/site-functions").join(&file));
                legacy.push(PathBuf::from("/usr/share/zsh/vendor-completions").join(&file));
            }
            (
                canonical,
                legacy,
                rc_block,
                vec![zdot.join(".zshrc"), home.join(".zshrc")],
                vec![zdot, home.to_path_buf()],
                "exec zsh",
            )
        }
        Shell::Bash => {
            let completions = data.join("bash-completion/completions");
            let home_completions = home.join(".local/share/bash-completion/completions");
            let mut legacy = vec![
                completions.join(format!("{bin}.bash")),
                home_completions.join(bin),
                home_completions.join(format!("{bin}.bash")),
                home.join(".bash_completion.d").join(bin),
            ];
            if env.include_system_paths {
                legacy.push(PathBuf::from("/etc/bash_completion.d").join(bin));
            }
            (
                completions.join(bin),
                legacy,
                None,
                vec![
                    home.join(".bashrc"),
                    home.join(".bash_profile"),
                    home.join(".bash_completion"),
                ],
                Vec::new(),
                "exec bash",
            )
        }
        Shell::Fish => (
            config.join("fish/completions").join(format!("{bin}.fish")),
            vec![
                home.join(".config/fish/completions")
                    .join(format!("{bin}.fish")),
            ],
            None,
            vec![config.join("fish/config.fish")],
            Vec::new(),
            "exec fish",
        ),
        Shell::Elvish => {
            let file = format!("{bin}-completions.elv");
            let rc_file = config.join("elvish/rc.elv");
            let block = RcBlock {
                rc_file: rc_file.clone(),
                body: vec![format!("use {bin}-completions")],
                placement: BlockPlacement::End,
            };
            (
                config.join("elvish/lib").join(&file),
                vec![home.join(".elvish/lib").join(&file)],
                Some(block),
                vec![rc_file, home.join(".elvish/rc.elv")],
                Vec::new(),
                "exec elvish",
            )
        }
        Shell::PowerShell => {
            let canonical = config
                .join("powershell")
                .join(format!("{bin}-completions.ps1"));
            let profile = config.join("powershell/Microsoft.PowerShell_profile.ps1");
            let block = RcBlock {
                rc_file: profile.clone(),
                body: vec![format!(". \"{}\"", canonical.display())],
                placement: BlockPlacement::End,
            };
            (
                canonical,
                Vec::new(),
                Some(block),
                vec![profile],
                Vec::new(),
                ". $PROFILE",
            )
        }
        other => {
            return Err(CompletionError::UnsupportedShell {
                shell: other.to_string(),
            });
        }
    };

    let mut rc_cleanup = rc_cleanup;
    if let Some(block) = &rc_block {
        rc_cleanup.push(block.rc_file.clone());
    }

    let locations = Locations {
        shell,
        bin: bin.to_owned(),
        legacy: legacy_without(&canonical, legacy),
        canonical,
        rc_block,
        rc_cleanup: dedup(rc_cleanup),
        cache_dirs: dedup(cache_dirs),
        reload_hint: reload_hint.to_owned(),
    };
    debug!(
        bin,
        shell = %shell,
        canonical = %locations.canonical.display(),
        legacy = locations.legacy.len(),
        "resolved completion locations"
    );
    Ok(locations)
}
