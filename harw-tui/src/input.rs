//! Classification and tokenization for one user input line.

use crate::{CommandError, CommandName, CommandResult};

/// One prefix-classified input, before any runtime operation is performed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Invocation {
    Command { name: String, raw_args: Vec<String> },
    Shell(String),
    ShellRepeat,
    Note(String),
    Mention { target: String, body: String },
    Chat(String),
}

/// Classifies an input line according to the interaction contract.
pub fn classify_input(line: &str) -> CommandResult<Invocation> {
    let Some(prefix) = line.chars().next() else {
        return Ok(Invocation::Chat(String::new()));
    };

    if prefix == '\\' {
        let remainder = &line[prefix.len_utf8()..];
        if matches!(remainder.chars().next(), Some('/' | '!' | '#' | '@' | '$')) {
            return Ok(Invocation::Chat(remainder.to_owned()));
        }
    }

    match prefix {
        '/' => parse_command(&line[1..]),
        '!' => parse_shell(&line[1..]),
        '#' => Ok(Invocation::Note(line[1..].trim_start().to_owned())),
        '@' => parse_mention(&line[1..]),
        '$' => Err(CommandError::ReservedPrefix { prefix }),
        _ => Ok(Invocation::Chat(line.to_owned())),
    }
}

fn parse_command(input: &str) -> CommandResult<Invocation> {
    let tokens = tokenize(input)?;
    let Some(name) = tokens.first() else {
        return Err(CommandError::InvalidCommandName {
            input: String::new(),
        });
    };
    let name = name.strip_suffix(':').unwrap_or(name);
    CommandName::parse(name.to_owned())?;

    Ok(Invocation::Command {
        name: name.to_owned(),
        raw_args: tokens.into_iter().skip(1).collect(),
    })
}

fn parse_shell(input: &str) -> CommandResult<Invocation> {
    if let Some(remainder) = input.strip_prefix('!') {
        let extra = tokenize(remainder)?;
        if extra.is_empty() {
            return Ok(Invocation::ShellRepeat);
        }
        return Err(CommandError::TrailingTokens {
            command: "!!".to_owned(),
            extra,
        });
    }

    Ok(Invocation::Shell(input.trim_start().to_owned()))
}

fn parse_mention(input: &str) -> CommandResult<Invocation> {
    let tokens = tokenize(input)?;
    let Some((target, body)) = tokens.split_first() else {
        return Ok(Invocation::Chat("@".to_owned()));
    };

    Ok(Invocation::Mention {
        target: target.to_owned(),
        body: body.join(" "),
    })
}

/// Tokenizes the contract's quoted positional/flag syntax without shell expansion.
pub(crate) fn tokenize(input: &str) -> CommandResult<Vec<String>> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut token_started = false;
    let mut chars = input.chars();

    while let Some(character) = chars.next() {
        if let Some(open_quote) = quote {
            match character {
                value if value == open_quote => quote = None,
                '\\' => push_escape(&mut current, chars.next())?,
                value => current.push(value),
            }
            token_started = true;
            continue;
        }

        match character {
            '\'' | '"' => {
                quote = Some(character);
                token_started = true;
            }
            '\\' => {
                push_escape(&mut current, chars.next())?;
                token_started = true;
            }
            value if value.is_whitespace() => {
                if token_started {
                    tokens.push(std::mem::take(&mut current));
                    token_started = false;
                }
            }
            value => {
                current.push(value);
                token_started = true;
            }
        }
    }

    if let Some(quote) = quote {
        return Err(CommandError::UnterminatedQuote { quote });
    }
    if token_started {
        tokens.push(current);
    }
    Ok(tokens)
}

fn push_escape(current: &mut String, escaped: Option<char>) -> CommandResult<()> {
    match escaped {
        Some('"') => current.push('"'),
        Some('\\') => current.push('\\'),
        Some(' ') => current.push(' '),
        Some(value) => {
            return Err(CommandError::InvalidEscape {
                input: format!("\\{value}"),
            });
        }
        None => {
            return Err(CommandError::InvalidEscape {
                input: "\\".to_owned(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_supports_optional_separator_quotes_and_escapes() {
        assert_eq!(
            classify_input("/rename: s:01J8 'Release notes' --keep=30m escaped\\ value"),
            Ok(Invocation::Command {
                name: "rename".to_owned(),
                raw_args: vec![
                    "s:01J8".to_owned(),
                    "Release notes".to_owned(),
                    "--keep=30m".to_owned(),
                    "escaped value".to_owned(),
                ],
            })
        );
    }

    #[test]
    fn escaped_prefix_is_plain_chat() {
        assert_eq!(
            classify_input("\\/not-a-command"),
            Ok(Invocation::Chat("/not-a-command".to_owned()))
        );
    }

    #[test]
    fn shell_repeat_rejects_trailing_tokens() {
        assert!(matches!(
            classify_input("!! ls"),
            Err(CommandError::TrailingTokens { command, .. }) if command == "!!"
        ));
    }

    #[test]
    fn dollar_prefix_is_reserved() {
        assert!(matches!(
            classify_input("$SESSION"),
            Err(CommandError::ReservedPrefix { prefix: '$' })
        ));
    }
}
