//! `RunnerArgs`: parses `harw-agent-runner`'s command line.
//!
//! # Description
//! Hand-rolled, not `clap`: `clap` is a direct dependency of `harw-cli`
//! only, not a workspace dependency, and this crate's surface (a dozen
//! flags, no subcommands) does not earn pulling it in. [`RunnerArgs::parse`]
//! takes an iterator of `String` (not `std::env::args_os`) so tests build
//! arg vectors directly; [`crate::run_from_current_exe`] passes
//! `std::env::args().skip(1)`.

use harw_runtime::embedded::RightsFlags;

use harw_agent_dsl::ir_v2::Interface;

use crate::error::RunnerError;

/// The parsed command line of one runner invocation.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RunnerArgs {
    /// `--interface <name>`: overrides the manifest's default interface.
    pub interface: Option<Interface>,
    /// The positional prompt words, joined by a single space; `None`
    /// without any positional argument (an interactive interface reads its
    /// own input instead).
    pub prompt: Option<String>,
    /// `--json`: machine-readable output on stdout instead of the human
    /// banner/report.
    pub json: bool,
    /// `--manifest`: print the embedded manifest's permissions and exit.
    pub manifest: bool,
    /// `--verify`: verify the embedded artifact's integrity and exit.
    pub verify: bool,
    /// `--version`: print the runner version and exit.
    pub version: bool,
    /// `--capabilities`: print the capabilities JSON and exit.
    pub capabilities: bool,
    /// `--child <id>`: run as a delegated child of `id` instead of a
    /// top-level interface (`crate::child::run_child`).
    pub child: Option<String>,
    /// `--child-protocol <label>`: the protocol the parent speaks with this
    /// child (required together with `--child`).
    pub child_protocol: Option<String>,
    /// Rights flags narrowing the embedded manifest
    /// (`--deny-tool`/`--no-network`/`--read-only`/`--full-access`/
    /// `--max-tokens`).
    pub flags: RightsFlags,
    /// `--listen <addr>`: bind address for the `http`/`mcp` interfaces.
    pub listen: Option<String>,
    /// `--offline-echo`: answer every model call locally with the offline
    /// echo (reply text from `HARW_OFFLINE_ECHO` when set). Without this
    /// flag only a `debug_assertions` build honors `HARW_OFFLINE_ECHO`
    /// (`crate::context::offline_echo_enabled`).
    pub offline_echo: bool,
}

impl RunnerArgs {
    /// Parses `args` (program name already stripped).
    ///
    /// # Errors
    /// [`RunnerError::Usage`] for an unknown flag, a flag missing its value,
    /// an unparsable `--interface`/`--max-tokens`, or `--child` without
    /// `--child-protocol` (or the reverse).
    pub fn parse<I, S>(args: I) -> Result<Self, RunnerError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        fn value(
            iter: &mut impl Iterator<Item = String>,
            flag: &str,
        ) -> Result<String, RunnerError> {
            iter.next()
                .ok_or_else(|| RunnerError::Usage(format!("{flag} needs a value")))
        }

        let mut out = Self::default();
        let mut prompt_words: Vec<String> = Vec::new();
        let mut iter = args.into_iter().map(|arg| arg.as_ref().to_owned());
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--interface" => {
                    let text = value(&mut iter, "--interface")?;
                    out.interface = Some(Interface::parse(&text).ok_or_else(|| {
                        RunnerError::Usage(format!(
                            "--interface `{text}`, expected one of: {}",
                            Interface::ALL
                                .iter()
                                .map(|interface| interface.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ))
                    })?);
                }
                "--json" => out.json = true,
                "--manifest" => out.manifest = true,
                "--verify" => out.verify = true,
                "--version" => out.version = true,
                "--capabilities" => out.capabilities = true,
                "--child" => out.child = Some(value(&mut iter, "--child")?),
                "--child-protocol" => {
                    out.child_protocol = Some(value(&mut iter, "--child-protocol")?);
                }
                "--listen" => out.listen = Some(value(&mut iter, "--listen")?),
                "--offline-echo" => out.offline_echo = true,
                "--deny-tool" => out.flags.deny_tools.push(value(&mut iter, "--deny-tool")?),
                "--no-network" => out.flags.no_network = true,
                "--read-only" => out.flags.read_only = true,
                "--full-access" => out.flags.full_access = true,
                "--max-tokens" => {
                    let text = value(&mut iter, "--max-tokens")?;
                    out.flags.max_tokens = Some(text.parse::<u64>().map_err(|error| {
                        RunnerError::Usage(format!("--max-tokens `{text}`: {error}"))
                    })?);
                }
                "--" => prompt_words.extend(iter.by_ref()),
                flag if flag.starts_with("--") => {
                    return Err(RunnerError::Usage(format!("unknown flag `{flag}`")));
                }
                word => prompt_words.push(word.to_owned()),
            }
        }
        if out.child.is_some() != out.child_protocol.is_some() {
            return Err(RunnerError::Usage(
                "--child and --child-protocol must be given together".to_owned(),
            ));
        }
        if !prompt_words.is_empty() {
            out.prompt = Some(prompt_words.join(" "));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn test_defaults_are_empty() -> TestResult {
        let parsed = RunnerArgs::parse(args(&[]))?;
        assert_eq!(parsed, RunnerArgs::default());
        Ok(())
    }

    #[test]
    fn test_flags_and_prompt() -> TestResult {
        let parsed = RunnerArgs::parse(args(&[
            "--interface",
            "mcp",
            "--json",
            "--deny-tool",
            "shell.exec",
            "--deny-tool",
            "fs.write",
            "--no-network",
            "--max-tokens",
            "1000",
            "review",
            "this",
            "diff",
        ]))?;
        assert_eq!(parsed.interface, Some(Interface::Mcp));
        assert!(parsed.json);
        assert_eq!(
            parsed.flags.deny_tools,
            vec!["shell.exec".to_owned(), "fs.write".to_owned()]
        );
        assert!(parsed.flags.no_network);
        assert_eq!(parsed.flags.max_tokens, Some(1000));
        assert_eq!(parsed.prompt.as_deref(), Some("review this diff"));
        Ok(())
    }

    #[test]
    fn test_child_flags_require_each_other() {
        assert!(RunnerArgs::parse(args(&["--child", "explorer-1"])).is_err());
        assert!(RunnerArgs::parse(args(&["--child-protocol", "harwness.agent-child/v1"])).is_err());
        let ok = RunnerArgs::parse(args(&[
            "--child",
            "explorer-1",
            "--child-protocol",
            "harwness.agent-child/v1",
        ]));
        assert!(ok.is_ok());
    }

    #[test]
    fn test_unknown_flag_is_a_usage_error() {
        assert!(RunnerArgs::parse(args(&["--nope"])).is_err());
    }

    #[test]
    fn test_bad_interface_and_max_tokens_are_usage_errors() {
        assert!(RunnerArgs::parse(args(&["--interface", "gopher"])).is_err());
        assert!(RunnerArgs::parse(args(&["--max-tokens", "not-a-number"])).is_err());
    }

    #[test]
    fn test_offline_echo_flag() -> TestResult {
        assert!(!RunnerArgs::parse(args(&[]))?.offline_echo);
        let parsed = RunnerArgs::parse(args(&["--offline-echo", "hello"]))?;
        assert!(parsed.offline_echo);
        assert_eq!(parsed.prompt.as_deref(), Some("hello"));
        Ok(())
    }

    #[test]
    fn test_capabilities_manifest_verify_version_flags() -> TestResult {
        let parsed = RunnerArgs::parse(args(&["--capabilities"]))?;
        assert!(parsed.capabilities);
        let parsed = RunnerArgs::parse(args(&["--manifest", "--json"]))?;
        assert!(parsed.manifest && parsed.json);
        let parsed = RunnerArgs::parse(args(&["--verify"]))?;
        assert!(parsed.verify);
        let parsed = RunnerArgs::parse(args(&["--version"]))?;
        assert!(parsed.version);
        Ok(())
    }
}
