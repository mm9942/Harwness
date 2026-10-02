use harw_macros::HarwError;
use std::error::Error;

mod common;
use common::{TestResult, ctx};

type SessionId = u64;

#[derive(HarwError, Debug)]
enum CoreError {
    #[msg("session {0} not idle")]
    NotIdle(SessionId),

    #[from]
    Io(std::io::Error),

    #[msg("tool {name} not found")]
    ToolNotFound { name: String },

    #[msg("budget exhausted: {used}/{cap}")]
    Budget { used: u32, cap: u32 },
}

#[test]
fn display_tuple_positional() {
    let e = CoreError::NotIdle(7);
    assert_eq!(e.to_string(), "session 7 not idle");
}

#[test]
fn display_named() {
    let e = CoreError::ToolNotFound {
        name: "grep".to_owned(),
    };
    assert_eq!(e.to_string(), "tool grep not found");
}

#[test]
fn display_named_multi() {
    let e = CoreError::Budget { used: 3, cap: 10 };
    assert_eq!(e.to_string(), "budget exhausted: 3/10");
}

#[test]
fn from_and_source() {
    let io = std::io::Error::new(std::io::ErrorKind::NotFound, "missing");
    let e: CoreError = io.into();
    assert!(e.source().is_some());
    // The from variant displays the inner error.
    assert_eq!(e.to_string(), "missing");
}

#[test]
fn result_alias_exists() -> TestResult {
    fn ok() -> CoreResult<u8> {
        Ok(1)
    }
    assert_eq!(ok().map_err(ctx("result alias must resolve"))?, 1);
    Ok(())
}

#[derive(HarwError, Debug)]
enum StructSourceError {
    #[msg("read {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[msg("wrapped")]
    Wrapped {
        #[from]
        inner: std::fmt::Error,
    },

    #[from]
    Tuple(std::num::ParseIntError),

    #[msg("pair {0}")]
    Pair(u8, #[source] std::io::Error),

    #[msg("plain {why}")]
    Plain { why: String },
}

#[test]
fn named_source_is_wired() -> TestResult {
    let io = std::io::Error::new(std::io::ErrorKind::NotFound, "missing");
    let e = StructSourceError::Read {
        path: "/x".to_owned(),
        source: io,
    };
    assert_eq!(e.to_string(), "read /x: missing");
    let src = e.source().ok_or(common::TestError::Missing("source"))?;
    assert_eq!(src.to_string(), "missing");
    Ok(())
}

#[test]
fn named_from_generates_from_and_source() {
    let e: StructSourceError = std::fmt::Error.into();
    assert_eq!(e.to_string(), "wrapped");
    assert!(e.source().is_some());
}

#[test]
fn tuple_source_field_and_none_cases() {
    let e = StructSourceError::Pair(3, std::io::Error::other("boom"));
    assert_eq!(e.to_string(), "pair 3");
    assert!(e.source().is_some());
    let plain = StructSourceError::Plain {
        why: "x".to_owned(),
    };
    assert!(plain.source().is_none());
    assert!("z".parse::<u8>().map_err(StructSourceError::from).is_err());
}
