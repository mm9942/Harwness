//! Command-line arguments of `harw-mobile`. Pure: the environment is passed in.

use std::path::PathBuf;

/// What the command line asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    /// The session socket.
    pub socket: PathBuf,
    /// A session to attach to; `None` picks the first hosted one.
    pub session: Option<String>,
    /// Create a new session instead.
    pub new: bool,
    /// Title for a new session.
    pub title: Option<String>,
}

/// The usage line.
pub const USAGE: &str = "usage: harw-mobile [--socket PATH] [--session ID | --new [--title TEXT]]";

/// The socket `harw gateway --session-socket` and `harw attach` use.
#[must_use]
pub fn default_socket(xdg_runtime_dir: Option<&str>) -> PathBuf {
    match xdg_runtime_dir.filter(|dir| !dir.is_empty()) {
        Some(dir) => PathBuf::from(dir).join("harw").join("session.sock"),
        None => PathBuf::from("session.sock"),
    }
}

/// Parses the arguments (without the program name).
///
/// # Errors
/// A message to show: the usage line, an unknown option, a missing value or
/// contradicting options.
pub fn parse_args(
    mut args: impl Iterator<Item = String>,
    xdg_runtime_dir: Option<&str>,
) -> Result<Args, String> {
    let mut parsed = Args {
        socket: default_socket(xdg_runtime_dir),
        session: None,
        new: false,
        title: None,
    };
    while let Some(flag) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match flag.as_str() {
            "--socket" => parsed.socket = PathBuf::from(value("--socket")?),
            "--session" => parsed.session = Some(value("--session")?),
            "--title" => parsed.title = Some(value("--title")?),
            "--new" => parsed.new = true,
            "-h" | "--help" => return Err(USAGE.to_owned()),
            other => return Err(format!("unknown option {other}\n{USAGE}")),
        }
    }
    if parsed.new && parsed.session.is_some() {
        return Err("--new and --session contradict each other".to_owned());
    }
    if parsed.title.is_some() && !parsed.new {
        return Err("--title only applies with --new".to_owned());
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ensure};

    fn parse(words: &[&str], xdg: Option<&str>) -> Result<Args, String> {
        parse_args(words.iter().map(|w| (*w).to_owned()), xdg)
    }

    #[test]
    fn the_default_socket_matches_what_the_gateway_binds() -> TestResult {
        let args = parse(&[], Some("/run/user/1000"))
            .map_err(crate::test_support::TestError::Unexpected)?;
        ensure(
            args.socket.as_path() == std::path::Path::new("/run/user/1000/harw/session.sock"),
            "xdg path",
        )?;
        ensure(
            default_socket(Some("")).as_path() == std::path::Path::new("session.sock"),
            "empty xdg",
        )?;
        ensure(
            default_socket(None).as_path() == std::path::Path::new("session.sock"),
            "no xdg",
        )
    }

    #[test]
    fn flags_are_read() -> TestResult {
        let args = parse(&["--socket", "/tmp/s", "--session", "abc"], None)
            .map_err(crate::test_support::TestError::Unexpected)?;
        ensure(
            args.socket.as_path() == std::path::Path::new("/tmp/s"),
            "socket",
        )?;
        ensure(args.session.as_deref() == Some("abc"), "session")?;
        let args = parse(&["--new", "--title", "phone"], None)
            .map_err(crate::test_support::TestError::Unexpected)?;
        ensure(
            args.new && args.title.as_deref() == Some("phone"),
            "new with title",
        )
    }

    #[test]
    fn mistakes_are_explained() -> TestResult {
        ensure(parse(&["--socket"], None).is_err(), "missing value")?;
        ensure(parse(&["--bogus"], None).is_err(), "unknown option")?;
        ensure(
            parse(&["--new", "--session", "x"], None).is_err(),
            "contradiction",
        )?;
        ensure(
            parse(&["--title", "t"], None).is_err(),
            "title without --new",
        )?;
        ensure(
            parse(&["--help"], None).is_err(),
            "help is shown as a message",
        )
    }
}
