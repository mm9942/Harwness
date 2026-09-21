use harw_macros::HarwError;
use std::error::Error;

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
fn result_alias_exists() {
    fn ok() -> CoreResult<u8> {
        Ok(1)
    }
    assert_eq!(ok().unwrap(), 1);
}
