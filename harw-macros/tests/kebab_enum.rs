//! Positive integration test for `#[derive(KebabEnum)]`: kebab default,
//! `case = "snake"`, `parse_option` and `no_from_str`.
//!
//! # Design-doc reference
//! AP W1-26e (KebabEnum-Derive-Makro), Erweiterung M6 (`case`, `parse_option`).

use harw_macros::KebabEnum;

mod common;
use common::{TestError, TestResult};

/// Fehlertyp unter dem Default-Pfad `crate::error::InvalidId`.
mod error {
    #[derive(Debug, PartialEq, Eq)]
    pub struct InvalidId {
        pub type_name: &'static str,
        pub value: String,
    }

    impl InvalidId {
        pub fn unknown_variant(type_name: &'static str, value: &str) -> Self {
            Self {
                type_name,
                value: value.to_owned(),
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, KebabEnum)]
enum Kebab {
    AddCriterion,
    HTTPServer,
    #[kebab_enum(rename = "super-admin")]
    Maintainer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, KebabEnum)]
#[kebab_enum(case = "snake", parse_option)]
enum Snake {
    Pending,
    CompleteDrain,
    HTTPServer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, KebabEnum)]
#[kebab_enum(case = "snake", parse_option, no_from_str)]
enum OptionOnly {
    Chat,
    DeepWork,
}

/// Eigenes `ALL` als Array (wie `InteractionMode`): `no_all` vermeidet die Kollision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, KebabEnum)]
#[kebab_enum(case = "snake", parse_option, no_from_str, no_all)]
enum OwnAll {
    One,
    Two,
}

impl OwnAll {
    const ALL: [Self; 2] = [Self::One, Self::Two];
}

const CONST_NAME: &str = Snake::CompleteDrain.as_str();

#[test]
fn kebab_is_the_default_wire_form() {
    assert_eq!(Kebab::AddCriterion.as_str(), "add-criterion");
    assert_eq!(Kebab::HTTPServer.to_string(), "http-server");
    assert_eq!(Kebab::Maintainer.as_str(), "super-admin");
    assert_eq!(Kebab::ALL.len(), 3);
}

#[test]
fn snake_case_changes_as_str_and_display() {
    assert_eq!(Snake::Pending.as_str(), "pending");
    assert_eq!(Snake::CompleteDrain.as_str(), "complete_drain");
    assert_eq!(Snake::HTTPServer.to_string(), "http_server");
    assert_eq!(CONST_NAME, "complete_drain");
}

#[test]
fn from_str_accepts_both_spellings_case_insensitively() -> TestResult {
    for input in [
        "complete_drain",
        "complete-drain",
        "COMPLETE_DRAIN",
        "Complete-Drain",
    ] {
        let parsed: Snake = input
            .parse()
            .map_err(|_| TestError::Unexpected(format!("`{input}` must parse")))?;
        assert_eq!(parsed, Snake::CompleteDrain);
    }
    let parsed: Kebab = "ADD_CRITERION"
        .parse()
        .map_err(|_| TestError::Missing("kebab enum parses snake input"))?;
    assert_eq!(parsed, Kebab::AddCriterion);
    Ok(())
}

#[test]
fn from_str_reports_type_name_and_raw_value() {
    let result = "nope".parse::<Snake>();
    assert_eq!(
        result,
        Err(error::InvalidId {
            type_name: "Snake",
            value: "nope".to_owned()
        })
    );
}

#[test]
fn parse_option_trims_and_returns_none_for_unknown() {
    assert_eq!(
        Snake::parse(" Complete-Drain \n"),
        Some(Snake::CompleteDrain)
    );
    assert_eq!(Snake::parse("complete drain"), None);
    assert_eq!(Snake::parse(""), None);
    assert_eq!(OptionOnly::parse("DEEP_WORK"), Some(OptionOnly::DeepWork));
    assert_eq!(OptionOnly::Chat.to_string(), "chat");
}

#[test]
fn round_trips_every_variant() {
    for variant in Snake::ALL {
        assert_eq!(Snake::parse(variant.as_str()), Some(*variant));
        assert_eq!(variant.as_str().parse::<Snake>().ok(), Some(*variant));
    }
}

#[test]
fn no_all_keeps_a_hand_written_array_constant() {
    let names: Vec<&str> = OwnAll::ALL.into_iter().map(|v| v.as_str()).collect();
    assert_eq!(names, ["one", "two"]);
}
