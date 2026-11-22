//! Tor (arti) onion-service transport for harw.
//!
//! This crate provides a pure, always-compiled locator parser plus two
//! feature-gated halves: an onion client (`client`) and an onion service
//! (`service`).
//!
//! Security and operations notes:
//!
//! * Tor is only a transport. It gives NO client identity: the
//!   `harw-node-transport` ML-DSA-65 handshake, the pinned keys and the device
//!   registry stay the only authority. Every stream accepted from Tor is handed
//!   to the caller unchanged.
//! * arti may call `std::process::exit(1)` at runtime when a new Tor consensus
//!   requires protocols this build lacks (arti-client `client.rs:1307-1322`).
//!   On the next start creation fails with [`OnionError::Outdated`]. Update
//!   harw promptly; there is no further mitigation.
//! * Vanguards (lite mode) are compiled in.
//! * Onion identity keys stay inside the arti state directory (keystore) and
//!   are never read or exported by harw.
//! * Never log an [`OnionLocator`]'s `Display` at info level or above; log
//!   [`OnionLocator::redacted`] instead.
//! * The process must not have another incompatible rustls default: the client
//!   half installs aws-lc-rs as the process default provider if none is set.

#![forbid(unsafe_code)]

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

#[cfg(feature = "client")]
mod client;
#[cfg(feature = "service")]
mod server;

#[cfg(feature = "client")]
pub use client::{OnionClient, OnionClientConfig};
#[cfg(feature = "service")]
pub use server::{AcceptLimits, DEFAULT_NICKNAME, OnionServer, OnionServerConfig};

/// Virtual port an onion service listens on unless configured otherwise.
pub const DEFAULT_VIRTUAL_PORT: u16 = 443;

/// Maximum length of a textual locator: 56 chars, ".onion", ":" and 5 digits.
pub const MAX_LOCATOR_LEN: usize = 68;

const HOST_BODY_LEN: usize = 56;
const ONION_SUFFIX: &str = ".onion";
const HOST_LEN: usize = HOST_BODY_LEN + ONION_SUFFIX.len();
const CHECKSUM_PREFIX: &[u8] = b".onion checksum";
const V3_VERSION: u8 = 3;

/// A validated v3 onion-service address plus virtual port.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct OnionLocator {
    host: String,
    port: u16,
}

impl OnionLocator {
    /// Builds a locator from `"<56 base32 chars>.onion"` and a port.
    ///
    /// The host is case-insensitive and stored lowercase; port 0 is rejected.
    ///
    /// # Errors
    /// Returns [`OnionError::InvalidLocator`] when the host is not a valid v3
    /// onion address or the port is 0.
    pub fn new(host: &str, port: u16) -> Result<Self, OnionError> {
        if port == 0 {
            return Err(OnionError::InvalidLocator("port must be 1..=65535"));
        }
        let host = validate_host(host)?;
        Ok(Self { host, port })
    }

    /// The canonical lowercase `"<56>.onion"` host.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The virtual port.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// A log-safe form: the first six base32 characters, then `….onion:<port>`.
    #[must_use]
    pub fn redacted(&self) -> String {
        let head = self.host.get(..6).unwrap_or_default();
        format!("{head}….onion:{}", self.port)
    }
}

impl FromStr for OnionLocator {
    type Err = OnionError;

    /// Parses `"<host>:<port>"`, splitting at the last `:`.
    fn from_str(input: &str) -> Result<Self, Self::Err> {
        if input.len() > MAX_LOCATOR_LEN {
            return Err(OnionError::InvalidLocator("locator is too long"));
        }
        let (host, port) = input
            .rsplit_once(':')
            .ok_or(OnionError::InvalidLocator("missing port"))?;
        let port = parse_port(port)?;
        Self::new(host, port)
    }
}

impl fmt::Display for OnionLocator {
    /// Canonical `"<56 lowercase>.onion:<port>"`. Never log this at info or above.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.host, self.port)
    }
}

impl fmt::Debug for OnionLocator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OnionLocator(<redacted>)")
    }
}

fn parse_port(text: &str) -> Result<u16, OnionError> {
    let bytes = text.as_bytes();
    let first = bytes
        .first()
        .ok_or(OnionError::InvalidLocator("missing port"))?;
    if !(b'1'..=b'9').contains(first) || bytes.len() > 5 || !bytes.iter().all(u8::is_ascii_digit) {
        return Err(OnionError::InvalidLocator("invalid port"));
    }
    text.parse::<u16>()
        .map_err(|_| OnionError::InvalidLocator("invalid port"))
}

/// Validates `"<56>.onion"` (case-insensitive) and returns it lowercased.
fn validate_host(host: &str) -> Result<String, OnionError> {
    if host.len() != HOST_LEN || !host.is_ascii() {
        return Err(OnionError::InvalidLocator("invalid onion host length"));
    }
    let host = host.to_ascii_lowercase();
    let body = host
        .strip_suffix(ONION_SUFFIX)
        .ok_or(OnionError::InvalidLocator("host must end in .onion"))?;
    let decoded = decode_base32(body)?;
    let (head, version) = decoded.split_at(decoded.len() - 1);
    if version != [V3_VERSION] {
        return Err(OnionError::InvalidLocator("unsupported onion version"));
    }
    let (pubkey, checksum) = head.split_at(32);
    if checksum != expected_checksum(pubkey) {
        return Err(OnionError::InvalidLocator("onion checksum mismatch"));
    }
    Ok(host)
}

/// The two-byte v3 checksum: `SHA3-256(".onion checksum" || pubkey || 0x03)[..2]`.
fn expected_checksum(pubkey: &[u8]) -> [u8; 2] {
    let mut input = Vec::with_capacity(CHECKSUM_PREFIX.len() + pubkey.len() + 1);
    input.extend_from_slice(CHECKSUM_PREFIX);
    input.extend_from_slice(pubkey);
    input.push(V3_VERSION);
    let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA3_256, &input);
    let bytes = digest.as_ref();
    [bytes[0], bytes[1]]
}

/// Decodes exactly 56 lowercase RFC 4648 base32 characters (no padding) into 35 bytes.
fn decode_base32(text: &str) -> Result<Vec<u8>, OnionError> {
    if text.len() != HOST_BODY_LEN {
        return Err(OnionError::InvalidLocator("invalid onion host length"));
    }
    let mut out = Vec::with_capacity(HOST_BODY_LEN * 5 / 8);
    let mut buffer: u32 = 0;
    let mut bits: u32 = 0;
    for byte in text.bytes() {
        let value = match byte {
            b'a'..=b'z' => byte - b'a',
            b'2'..=b'7' => byte - b'2' + 26,
            _ => return Err(OnionError::InvalidLocator("invalid base32 character")),
        };
        buffer = (buffer << 5) | u32::from(value);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xff) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

/// A snapshot of Tor bootstrap progress.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct BootstrapProgress {
    /// Progress from 0 to 100.
    pub percent: u8,
    /// True once Tor is ready for traffic.
    pub ready: bool,
    /// Human-readable reason bootstrap is blocked, if it is.
    pub blocked: Option<String>,
    /// Short status text.
    pub summary: String,
}

/// Any bidirectional async byte stream usable as an onion connection.
pub trait OnionIo: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + 'static {}

impl<T> OnionIo for T where T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + 'static {}

/// A boxed, pinned onion stream (`Unpin + Send + 'static`).
pub type OnionStream = std::pin::Pin<Box<dyn OnionIo>>;

/// Errors of the onion transport.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum OnionError {
    /// The locator text is not a valid v3 onion locator.
    #[error("invalid onion locator: {0}")]
    InvalidLocator(&'static str),
    /// The Tor client configuration could not be built.
    #[error("tor configuration: {0}")]
    Config(String),
    /// No rustls crypto provider is available for Tor.
    #[error("no rustls crypto provider is available for Tor")]
    CryptoProvider,
    /// The Tor runtime or client could not be created.
    #[error("tor runtime: {0}")]
    Runtime(String),
    /// Tor bootstrap failed.
    #[error("tor bootstrap failed: {0}")]
    Bootstrap(String),
    /// Tor bootstrap did not finish in time.
    #[error("tor bootstrap did not finish within {0:?}")]
    BootstrapTimeout(Duration),
    /// This build's Tor is too old for the current Tor network.
    #[error("this build's Tor (arti 0.48) is too old for the current Tor network; update harw")]
    Outdated,
    /// The onion service failed.
    #[error("onion service: {0}")]
    Service(String),
    /// Connecting to an onion service failed.
    #[error("onion connect failed: {0}")]
    Connect(String),
    /// Connecting to an onion service did not finish in time.
    #[error("onion connect did not finish within {0:?}")]
    ConnectTimeout(Duration),
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const HOST: &str = "2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion";
    const VECTOR: &str = "2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion:443";

    fn replace_char(text: &str, index: usize, with: char) -> String {
        text.chars()
            .enumerate()
            .map(|(i, c)| if i == index { with } else { c })
            .collect()
    }

    fn rejects(text: &str) -> bool {
        text.parse::<OnionLocator>().is_err()
    }

    #[test]
    fn vector_parses_and_round_trips() -> TestResult {
        let locator: OnionLocator = VECTOR.parse()?;
        assert_eq!(locator.host(), HOST);
        assert_eq!(locator.port(), 443);
        assert_eq!(locator.to_string(), VECTOR);
        assert_eq!(locator, OnionLocator::new(HOST, 443)?);
        Ok(())
    }

    #[test]
    fn uppercase_is_normalised() -> TestResult {
        let upper = VECTOR.to_ascii_uppercase();
        let locator: OnionLocator = upper.parse()?;
        assert_eq!(locator.to_string(), VECTOR);
        Ok(())
    }

    #[test]
    fn rejects_wrong_length() {
        let body = &HOST[..56];
        assert!(rejects(&format!("{}.onion:443", &body[..55])));
        assert!(rejects(&format!("{body}a.onion:443")));
    }

    #[test]
    fn rejects_bad_characters() {
        for bad in ['1', '8', '0', '='] {
            assert!(rejects(&replace_char(VECTOR, 10, bad)), "char {bad}");
        }
    }

    #[test]
    fn rejects_wrong_version_and_checksum() {
        assert!(rejects(&replace_char(VECTOR, 55, 'a')));
        let original = VECTOR.chars().nth(20);
        let other = if original == Some('q') { 'r' } else { 'q' };
        assert!(rejects(&replace_char(VECTOR, 20, other)));
    }

    #[test]
    fn rejects_bad_ports() {
        assert!(rejects(HOST));
        assert!(rejects(&format!("{HOST}:")));
        assert!(rejects(&format!("{HOST}:0")));
        assert!(rejects(&format!("{HOST}:65536")));
        assert!(rejects(&format!("{HOST}:0443")));
        assert!(rejects(&format!("{HOST}:+443")));
        assert!(rejects(&format!("{HOST}:443 ")));
        assert!(!rejects(&format!("{HOST}:65535")));
        assert!(OnionLocator::new(HOST, 0).is_err());
    }

    #[test]
    fn rejects_subdomain_whitespace_and_overlong() {
        assert!(rejects(&format!("x.{VECTOR}")));
        assert!(rejects(&format!(" {VECTOR}")));
        assert!(rejects(&format!("{VECTOR} ")));
        assert!(rejects(&format!("{}{VECTOR}", "a".repeat(10))));
    }

    #[test]
    fn redaction_hides_the_host() -> TestResult {
        let locator: OnionLocator = VECTOR.parse()?;
        let redacted = locator.redacted();
        assert_eq!(redacted, "2gzyxa….onion:443");
        assert!(!redacted.contains(HOST));
        let debug = format!("{locator:?}");
        assert!(!debug.contains(&HOST[..56]));
        assert!(!debug.contains("2gzyxa"));
        Ok(())
    }
}
