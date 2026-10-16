//! `harw-mobile-term` (experimental): a line-oriented phone client.
//!
//! No TUI dependency, no colour, no cursor movement: it runs in any terminal,
//! including a phone's. The state comes from [`harw_mobile_core`]; this crate
//! only parses lines ([`command`]), turns view changes into short text
//! ([`render`]) and runs the loop ([`session`]).

#![forbid(unsafe_code)]

pub mod args;
pub mod command;
pub mod render;
pub mod session;
#[cfg(test)]
mod test_support;

pub use command::{Command, parse};
pub use render::Printer;
pub use session::{RunEnd, run};
