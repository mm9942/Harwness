use harw_browser::error::{Error, Result};
use harw_browser::selector::{Selector, Target};
use thirtyfour::By;

/// Converts a target into the exact ordered sequence attempted by the adapter.
///
/// A single semantic selector may expand to more than one WebDriver selector
/// (currently role plus accessible name). Those candidates remain adjacent and
/// precede candidates derived from the target's explicit fallbacks.
pub(crate) fn target_candidates(target: &Target) -> Result<Vec<By>> {
    let mut candidates = Vec::new();
    for selector in target.candidates() {
        candidates.extend(selector_candidates(selector)?);
    }
    Ok(candidates)
}

fn selector_candidates(selector: &Selector) -> Result<Vec<By>> {
    match selector {
        Selector::TestId(value) => {
            require_nonempty("test ID", value)?;
            Ok(vec![By::Css(format!(
                "[data-testid=\"{}\"]",
                css_string(value)
            ))])
        }
        Selector::Css(value) => {
            require_nonempty("CSS selector", value)?;
            Ok(vec![By::Css(value.as_str())])
        }
        Selector::Id(value) => {
            require_nonempty("element ID", value)?;
            Ok(vec![By::Id(value.as_str())])
        }
        Selector::Name(value) => {
            require_nonempty("element name", value)?;
            Ok(vec![By::Name(value.as_str())])
        }
        Selector::TagClass { tag, class } => {
            validate_tag(tag)?;
            require_nonempty("class name", class)?;
            reject_ascii_whitespace("class name", class)?;
            Ok(vec![By::Css(format!(
                "{}[class~=\"{}\"]",
                tag,
                css_string(class)
            ))])
        }
        Selector::LinkText(value) => {
            require_nonempty("link text", value)?;
            Ok(vec![By::LinkText(value.as_str())])
        }
        Selector::XPath(value) => {
            require_nonempty("XPath selector", value)?;
            Ok(vec![By::XPath(value.as_str())])
        }
        Selector::TextAnchor(value) => {
            require_nonempty("text anchor", value)?;
            Ok(vec![By::XPath(format!(
                "//*[contains(normalize-space(.), {})]",
                xpath_literal(value)
            ))])
        }
        Selector::Role { role, name } => {
            require_nonempty("ARIA role", role)?;
            let role_css = css_string(role);
            match name {
                None => Ok(vec![By::Css(format!("[role=\"{role_css}\"]"))]),
                Some(name) => {
                    require_nonempty("accessible name", name)?;
                    let name_css = css_string(name);
                    let role_xpath = xpath_literal(role);
                    let name_xpath = xpath_literal(name);
                    Ok(vec![
                        // Native ARIA reflection is most reliably exposed through
                        // aria-label, so prefer the cheap CSS lookup first.
                        By::Css(format!("[role=\"{role_css}\"][aria-label=\"{name_css}\"]")),
                        // Fall back to normalized visible text for applications
                        // whose accessible name is text-derived.
                        By::XPath(format!(
                            "//*[@role={role_xpath} and normalize-space(.)={name_xpath}]"
                        )),
                    ])
                }
            }
        }
    }
}

fn require_nonempty(kind: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(Error::InvalidArgument {
            detail: format!("{kind} must not be empty"),
        });
    }
    Ok(())
}

fn reject_ascii_whitespace(kind: &str, value: &str) -> Result<()> {
    if value.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return Err(Error::InvalidArgument {
            detail: format!("{kind} must contain exactly one token"),
        });
    }
    Ok(())
}

fn validate_tag(tag: &str) -> Result<()> {
    require_nonempty("tag name", tag)?;
    let mut characters = tag.chars();
    let starts_validly = characters
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic());
    let remainder_is_valid = characters
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | ':'));
    if !starts_validly || !remainder_is_valid {
        return Err(Error::InvalidArgument {
            detail: format!("tag name '{tag}' is not a valid selector tag token"),
        });
    }
    Ok(())
}

// CSS quoted-string escaping. Attribute selectors use double-quoted strings,
// so quotes, backslashes, and line terminators must not escape their boundary.
fn css_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\a "),
            '\r' => escaped.push_str("\\d "),
            '\u{000c}' => escaped.push_str("\\c "),
            character => escaped.push(character),
        }
    }
    escaped
}

// XPath 1.0 has no string escaping syntax. Choose one quote delimiter where
// possible and otherwise build a concat expression with literal quote pieces.
fn xpath_literal(value: &str) -> String {
    if !value.contains('\'') {
        return format!("'{value}'");
    }
    if !value.contains('"') {
        return format!("\"{value}\"");
    }

    let pieces: Vec<String> = value
        .split('\'')
        .map(|piece| format!("'{piece}'"))
        .collect();
    format!("concat({})", pieces.join(", \"'\", "))
}
