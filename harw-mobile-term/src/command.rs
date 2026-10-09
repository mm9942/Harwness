//! What a typed line means. A pure parser: no I/O.
//!
//! Plain text is a prompt. A line starting with `/` is a command; `//` sends
//! a prompt that itself starts with a slash. Approval commands take the
//! 1-based number shown in the list (`/y 2`); without a number they act on
//! the only open approval.

/// One parsed input line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Send this text as a prompt.
    Send(String),
    /// Approve the open approval with this 1-based number (`None`: the only one).
    Approve(Option<usize>),
    /// Reject it, with an optional reason.
    Deny(Option<usize>, Option<String>),
    /// Interrupt the running turn.
    Interrupt,
    /// List the open approvals.
    Pending,
    /// Show the help text.
    Help,
    /// Leave.
    Quit,
    /// A command that did not parse, with the message to show.
    Invalid(String),
}

/// The help text shown by `/help` and at start.
pub const HELP: &str = "text            send a prompt (//text sends a leading slash)\n\
/y [n]          approve open approval n\n\
/n [n] [why]    reject it, with an optional reason\n\
/p              list open approvals\n\
/stop           interrupt the running turn\n\
/help           this text\n\
/q              leave";

fn number(word: &str) -> Option<usize> {
    word.parse::<usize>().ok().filter(|n| *n >= 1)
}

/// Parses one line; `None` for a blank line.
#[must_use]
pub fn parse(line: &str) -> Option<Command> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    if let Some(escaped) = line.strip_prefix("//") {
        return Some(Command::Send(format!("/{escaped}")));
    }
    let Some(rest) = line.strip_prefix('/') else {
        return Some(Command::Send(line.to_owned()));
    };
    let (name, args) = rest
        .split_once(char::is_whitespace)
        .map_or((rest, ""), |(n, a)| (n, a.trim()));
    Some(match name {
        "y" | "yes" | "approve" => match args {
            "" => Command::Approve(None),
            word => match number(word) {
                Some(n) => Command::Approve(Some(n)),
                None => Command::Invalid("approve takes a number like /y 2".to_owned()),
            },
        },
        "n" | "no" | "deny" | "reject" => {
            let (first, tail) = args
                .split_once(char::is_whitespace)
                .map_or((args, ""), |(f, t)| (f, t.trim()));
            let (index, reason) = match number(first) {
                Some(n) => (Some(n), tail),
                None => (None, args),
            };
            let reason = (!reason.is_empty()).then(|| reason.to_owned());
            Command::Deny(index, reason)
        }
        "p" | "pending" => Command::Pending,
        "stop" | "interrupt" | "i" => Command::Interrupt,
        "help" | "h" | "?" => Command::Help,
        "q" | "quit" | "exit" => Command::Quit,
        other => Command::Invalid(format!("unknown command /{other}; /help lists them")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ensure};

    #[test]
    fn plain_text_is_a_prompt_and_blank_is_nothing() -> TestResult {
        ensure(parse("  ").is_none(), "blank")?;
        ensure(
            parse("list the files") == Some(Command::Send("list the files".to_owned())),
            "prompt",
        )?;
        ensure(
            parse("//etc is a dir") == Some(Command::Send("/etc is a dir".to_owned())),
            "escaped slash",
        )
    }

    #[test]
    fn approval_commands_take_an_optional_number() -> TestResult {
        ensure(parse("/y") == Some(Command::Approve(None)), "only one")?;
        ensure(parse("/y 2") == Some(Command::Approve(Some(2))), "numbered")?;
        ensure(
            matches!(parse("/y two"), Some(Command::Invalid(_))),
            "not a number",
        )?;
        ensure(
            matches!(parse("/y 0"), Some(Command::Invalid(_))),
            "numbers start at 1",
        )
    }

    #[test]
    fn deny_takes_a_number_and_a_reason_in_either_order_of_presence() -> TestResult {
        ensure(parse("/n") == Some(Command::Deny(None, None)), "bare")?;
        ensure(
            parse("/n 3") == Some(Command::Deny(Some(3), None)),
            "number",
        )?;
        ensure(
            parse("/n 3 too risky") == Some(Command::Deny(Some(3), Some("too risky".to_owned()))),
            "number and reason",
        )?;
        ensure(
            parse("/n not now") == Some(Command::Deny(None, Some("not now".to_owned()))),
            "reason only",
        )
    }

    #[test]
    fn the_other_commands_and_unknown_ones() -> TestResult {
        ensure(parse("/p") == Some(Command::Pending), "pending")?;
        ensure(parse("/stop") == Some(Command::Interrupt), "stop")?;
        ensure(parse("/?") == Some(Command::Help), "help")?;
        ensure(parse("/q") == Some(Command::Quit), "quit")?;
        ensure(
            matches!(parse("/frobnicate"), Some(Command::Invalid(_))),
            "unknown",
        )
    }
}
