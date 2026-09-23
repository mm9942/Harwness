//! Pinned geckodriver verification and sandboxed launch preparation (B-ADAPT, F-009).
//!
//! # Description
//! This module owns the only path through which the adapter obtains a
//! geckodriver process. There is no automatic driver download and no
//! `WebDriver::managed`: the host configuration names an absolute geckodriver
//! path plus its expected SHA-256 digest ([`GeckodriverPin`], fed from
//! `harw_config::BrowserSection::{geckodriver_path, geckodriver_sha256}`). The
//! file is hashed before every start ([`GeckodriverPin::verify`]); only a
//! [`VerifiedGeckodriver`] can be turned into a [`GeckodriverCommand`].
//!
//! Process creation is delegated to a [`BrowserLauncher`]. The launcher wraps
//! the geckodriver command in a network-isolated sandbox (`bwrap
//! --unshare-net` with the `harw-netns-relay` SOCKS relay, contract
//! `harw_sandbox::NetworkMode::ProxyOnly(RelaySpec { binary, listen_port,
//! proxy_socket })`) and returns the complete prepared command line
//! ([`PreparedLaunch`]). The adapter audits that command line
//! ([`validate_prepared_launch`]) **before** asking the launcher to spawn it:
//! `--share-net` is always rejected, network unsharing is mandatory, the
//! geckodriver argv must appear unmodified at the end, and the WebDriver
//! endpoint must be a loopback HTTP URL on the agreed port.
//!
//! The concrete launcher implementation lives with the sandbox integration
//! (follow-up I-CONTRIB); this crate only defines and enforces the contract.
//!
//! # Concurrency
//! All types are `Send + Sync`. [`GeckodriverPin::verify`] performs blocking
//! file I/O; async callers use [`launch_pinned_driver`], which runs the hash on
//! Tokio's blocking pool.
//!
//! # Errors
//! [`AdapterError::DriverPin`] for pin/hash failures, [`AdapterError::Launch`]
//! for rejected or failed launches.
//!
//! # Examples
//! ```rust,no_run
//! use harw_browser_thirtyfour::launcher::GeckodriverPin;
//! use std::path::PathBuf;
//!
//! # fn main() -> Result<(), harw_browser_thirtyfour::AdapterError> {
//! let pin = GeckodriverPin::new(
//!     PathBuf::from("/opt/geckodriver/geckodriver"),
//!     "0000000000000000000000000000000000000000000000000000000000000000",
//! )?;
//! let verified = pin.verify()?;
//! assert_eq!(verified.path(), pin.path());
//! # Ok(())
//! # }
//! ```

use crate::error::AdapterError;
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::File;
use std::io::Read;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};

/// File name of the relay binary that bridges the sandbox netns to the harness SOCKS proxy.
pub const NETNS_RELAY_BINARY_NAME: &str = "harw-netns-relay";

/// Upper bound for the geckodriver file size that is hashed (256 MiB).
pub const MAX_GECKODRIVER_BYTES: u64 = 256 * 1024 * 1024;

/// Loopback host geckodriver binds inside the sandbox network namespace.
pub const GECKODRIVER_LISTEN_HOST: &str = "127.0.0.1";

// Arguments that would give the sandbox the host network namespace.
const FORBIDDEN_SANDBOX_ARGS: [&str; 1] = ["--share-net"];
// At least one of these must be present so the sandbox gets its own netns.
const REQUIRED_NETNS_ARGS: [&str; 2] = ["--unshare-net", "--unshare-all"];
const HASH_CHUNK_BYTES: usize = 64 * 1024;

/// Absolute geckodriver path plus its expected SHA-256 digest.
///
/// # Description
/// Constructed from trusted host configuration only. The digest is stored as
/// 32 raw bytes; the path must be absolute. Construction performs no I/O.
///
/// # Concurrency
/// Plain data, `Send + Sync`.
#[derive(Clone, PartialEq, Eq)]
pub struct GeckodriverPin {
    path: PathBuf,
    sha256: [u8; 32],
}

impl fmt::Debug for GeckodriverPin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GeckodriverPin")
            .field("path", &self.path)
            .field("sha256", &hex(&self.sha256))
            .finish()
    }
}

impl GeckodriverPin {
    /// Creates a pin from an absolute path and a 64-character lowercase hex digest.
    ///
    /// # Errors
    /// - [`AdapterError::DriverPin`]: path is relative or digest malformed.
    pub fn new(path: PathBuf, sha256_hex: &str) -> Result<Self, AdapterError> {
        if !path.is_absolute() {
            return Err(pin_error(&path, "geckodriver path must be absolute"));
        }
        let sha256 = parse_sha256_hex(sha256_hex).ok_or_else(|| {
            pin_error(
                &path,
                "geckodriver SHA-256 must be 64 lowercase hexadecimal characters",
            )
        })?;
        Ok(Self { path, sha256 })
    }

    /// Returns the configured geckodriver path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the expected digest as lowercase hex.
    pub fn sha256_hex(&self) -> String {
        hex(&self.sha256)
    }

    /// Hashes the configured file and returns a verification token on a match.
    ///
    /// # Description
    /// Rejects symbolic links, non-regular files, files larger than
    /// [`MAX_GECKODRIVER_BYTES`] and (on Unix) group- or world-writable files,
    /// then streams the file through SHA-256 and compares the digest.
    ///
    /// # Errors
    /// - [`AdapterError::DriverPin`]: any of the checks above fails, the file
    ///   cannot be read, or the digest differs.
    ///
    /// # Concurrency
    /// Blocking file I/O; do not call directly on an async executor thread.
    pub fn verify(&self) -> Result<VerifiedGeckodriver, AdapterError> {
        let metadata = std::fs::symlink_metadata(&self.path)
            .map_err(|error| pin_error(&self.path, format!("cannot stat geckodriver: {error}")))?;
        if metadata.file_type().is_symlink() {
            return Err(pin_error(
                &self.path,
                "geckodriver path must not be a symbolic link",
            ));
        }
        if !metadata.is_file() {
            return Err(pin_error(
                &self.path,
                "geckodriver path is not a regular file",
            ));
        }
        if metadata.len() > MAX_GECKODRIVER_BYTES {
            return Err(pin_error(
                &self.path,
                format!("geckodriver exceeds the {MAX_GECKODRIVER_BYTES}-byte limit"),
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o022 != 0 {
                return Err(pin_error(
                    &self.path,
                    "geckodriver must not be group- or world-writable",
                ));
            }
        }

        let actual = sha256_file(&self.path)?;
        if !digest_eq(&actual, &self.sha256) {
            tracing::error!(
                path = %self.path.display(),
                expected = %hex(&self.sha256),
                actual = %hex(&actual),
                "geckodriver SHA-256 mismatch"
            );
            return Err(pin_error(
                &self.path,
                format!(
                    "geckodriver SHA-256 mismatch: expected {}, found {}",
                    hex(&self.sha256),
                    hex(&actual)
                ),
            ));
        }
        tracing::debug!(path = %self.path.display(), "geckodriver SHA-256 verified");
        Ok(VerifiedGeckodriver {
            path: self.path.to_path_buf(),
            sha256: actual,
        })
    }
}

/// Proof that a geckodriver file matched its pin at verification time.
///
/// # Description
/// Only [`GeckodriverPin::verify`] creates this type. Residual risk: the file
/// could be replaced between hashing and exec; the pinned path must therefore
/// live in a directory writable only by a trusted user (documented in the
/// B-ADAPT ledger).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedGeckodriver {
    path: PathBuf,
    sha256: [u8; 32],
}

impl VerifiedGeckodriver {
    /// Returns the verified path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the verified digest as lowercase hex.
    pub fn sha256_hex(&self) -> String {
        hex(&self.sha256)
    }
}

/// TCP ports geckodriver listens on inside the sandbox (WebDriver HTTP and BiDi WebSocket).
///
/// # Description
/// The launcher chooses them and must forward the identical port numbers to
/// the harness side so the `webSocketUrl` returned by geckodriver stays valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriverPorts {
    /// WebDriver HTTP port (`--port`).
    pub webdriver: u16,
    /// WebDriver BiDi WebSocket port (`--websocket-port`).
    pub bidi: u16,
}

/// The exact geckodriver invocation the sandbox must execute.
///
/// # Description
/// `argv()` is `[<verified path>, --host, 127.0.0.1, --port, P,
/// --websocket-port, W, --allow-hosts, 127.0.0.1, --allow-origins,
/// http://127.0.0.1:P, --log, warn]`. The Firefox binary is passed via
/// capabilities, not via argv.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeckodriverCommand {
    geckodriver: VerifiedGeckodriver,
    ports: DriverPorts,
    argv: Vec<OsString>,
}

impl GeckodriverCommand {
    /// Builds the command for a verified binary.
    ///
    /// # Errors
    /// - [`AdapterError::Launch`]: a port is zero or both ports coincide.
    pub fn new(geckodriver: VerifiedGeckodriver, ports: DriverPorts) -> Result<Self, AdapterError> {
        if ports.webdriver == 0 || ports.bidi == 0 || ports.webdriver == ports.bidi {
            return Err(launch_error(format!(
                "geckodriver ports must be nonzero and distinct (webdriver {}, bidi {})",
                ports.webdriver, ports.bidi
            )));
        }
        let webdriver_port = ports.webdriver.to_string();
        let argv = vec![
            geckodriver.path().as_os_str().to_os_string(),
            OsString::from("--host"),
            OsString::from(GECKODRIVER_LISTEN_HOST),
            OsString::from("--port"),
            OsString::from(&webdriver_port),
            OsString::from("--websocket-port"),
            OsString::from(ports.bidi.to_string()),
            OsString::from("--allow-hosts"),
            OsString::from(GECKODRIVER_LISTEN_HOST),
            OsString::from("--allow-origins"),
            OsString::from(format!("http://{GECKODRIVER_LISTEN_HOST}:{webdriver_port}")),
            OsString::from("--log"),
            OsString::from("warn"),
        ];
        Ok(Self {
            geckodriver,
            ports,
            argv,
        })
    }

    /// Returns the verified binary this command executes.
    pub fn geckodriver(&self) -> &VerifiedGeckodriver {
        &self.geckodriver
    }

    /// Returns the listen ports.
    pub fn ports(&self) -> DriverPorts {
        self.ports
    }

    /// Returns the full geckodriver argv (program first).
    pub fn argv(&self) -> &[OsString] {
        &self.argv
    }
}

/// Relay configuration mirrored from `harw_sandbox::RelaySpec`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayEndpoint {
    /// Absolute host path of the `harw-netns-relay` binary.
    pub binary: PathBuf,
    /// TCP port the relay listens on inside the sandbox netns (Firefox SOCKS port).
    pub listen_port: u16,
    /// Absolute host path of the harness egress proxy Unix socket.
    pub proxy_socket: PathBuf,
}

/// A fully prepared sandbox command line, produced by a [`BrowserLauncher`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedLaunch {
    /// Complete argv including the sandbox wrapper (program first).
    pub argv: Vec<OsString>,
    /// Harness-reachable WebDriver endpoint (`http://127.0.0.1:<webdriver port>`).
    pub webdriver_url: url::Url,
    /// Relay that provides the only egress path from the sandbox.
    pub relay: RelayEndpoint,
}

/// Handle of a spawned, sandboxed geckodriver process tree.
///
/// # Description
/// Implementations **must** terminate the whole sandbox (geckodriver, Firefox,
/// relay) when dropped, e.g. via `kill_on_drop` plus bwrap `--die-with-parent`.
pub trait DriverProcess: Send + Sync + fmt::Debug {
    /// Returns the OS process id of the sandbox root, if still known.
    fn id(&self) -> Option<u32>;
}

/// Starts geckodriver inside a network-isolated sandbox.
///
/// # Description
/// Contract for the sandbox integration (I-CONTRIB, `harw_sandbox`
/// `NetworkMode::ProxyOnly`). The adapter calls [`Self::driver_ports`], builds
/// a [`GeckodriverCommand`], calls [`Self::prepare`], audits the result with
/// [`validate_prepared_launch`], and only then calls [`Self::spawn`].
///
/// # Concurrency
/// Shared across sessions via `Arc<dyn BrowserLauncher>`; must be `Send + Sync`.
// `async_trait` setzt auf jede erzeugte Methode ein `#[must_use]`, obwohl der
// erzeugte Rückgabetyp (`Pin<Box<dyn Future>>`) ohnehin schon als `must_use`
// gilt. Die Doppelung entsteht im Makro, nicht in diesem Code — deshalb hier
// eine benannte Ausnahme statt einer Änderung an den Methodensignaturen.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait BrowserLauncher: Send + Sync + fmt::Debug {
    /// Returns the ports geckodriver should listen on for the next launch.
    fn driver_ports(&self) -> DriverPorts;

    /// Wraps `command` in the sandbox and returns the complete command line.
    ///
    /// # Errors
    /// - [`AdapterError::Launch`]: the sandbox cannot be prepared.
    fn prepare(&self, command: &GeckodriverCommand) -> Result<PreparedLaunch, AdapterError>;

    /// Spawns a previously audited command line and resolves once the
    /// WebDriver endpoint accepts connections.
    ///
    /// # Errors
    /// - [`AdapterError::Launch`]: spawn or readiness failure.
    async fn spawn(
        &self,
        prepared: &PreparedLaunch,
    ) -> Result<Box<dyn DriverProcess>, AdapterError>;
}

/// A spawned driver together with the audited launch description.
#[derive(Debug)]
pub struct LaunchedDriver {
    prepared: PreparedLaunch,
    process: Box<dyn DriverProcess>,
}

impl LaunchedDriver {
    /// Returns the audited launch description.
    pub fn prepared(&self) -> &PreparedLaunch {
        &self.prepared
    }

    /// Returns the harness-reachable WebDriver endpoint.
    pub fn webdriver_url(&self) -> &url::Url {
        &self.prepared.webdriver_url
    }

    /// Returns the SOCKS relay port Firefox must use inside the sandbox.
    pub fn proxy_port(&self) -> u16 {
        self.prepared.relay.listen_port
    }

    /// Splits into description and process handle (dropping the handle kills the sandbox).
    pub fn into_parts(self) -> (PreparedLaunch, Box<dyn DriverProcess>) {
        (self.prepared, self.process)
    }
}

/// Audits a launcher-prepared command line before anything is spawned.
///
/// # Description
/// Fail-closed checks: argv non-empty with an absolute program; no
/// `--share-net` anywhere; `--unshare-net` or `--unshare-all` present; argv ends
/// with the unmodified geckodriver argv; WebDriver URL is `http` on a loopback
/// IP literal with exactly the agreed WebDriver port and no path/credentials;
/// relay binary is absolute and named [`NETNS_RELAY_BINARY_NAME`]; relay port
/// nonzero and distinct from the driver ports; proxy socket absolute.
///
/// # Errors
/// - [`AdapterError::Launch`]: the first violated rule.
pub fn validate_prepared_launch(
    command: &GeckodriverCommand,
    prepared: &PreparedLaunch,
) -> Result<(), AdapterError> {
    let program = prepared
        .argv
        .first()
        .ok_or_else(|| launch_error("prepared sandbox command line is empty"))?;
    if !Path::new(program).is_absolute() {
        return Err(launch_error("sandbox program path must be absolute"));
    }
    if let Some(forbidden) = prepared.argv.iter().find(|arg| {
        FORBIDDEN_SANDBOX_ARGS
            .iter()
            .any(|f| OsStr::new(f) == arg.as_os_str())
    }) {
        return Err(launch_error(format!(
            "sandbox command line contains forbidden argument {forbidden:?}"
        )));
    }
    let unshares_network = prepared.argv.iter().any(|arg| {
        REQUIRED_NETNS_ARGS
            .iter()
            .any(|r| OsStr::new(r) == arg.as_os_str())
    });
    if !unshares_network {
        return Err(launch_error(
            "sandbox command line must unshare the network namespace (--unshare-net or --unshare-all)",
        ));
    }
    let inner = command.argv();
    if prepared.argv.len() <= inner.len() || !prepared.argv.ends_with(inner) {
        return Err(launch_error(
            "sandbox command line must end with the unmodified pinned geckodriver command",
        ));
    }

    let url = &prepared.webdriver_url;
    let loopback = match url.host() {
        Some(url::Host::Ipv4(ip)) => ip == Ipv4Addr::LOCALHOST,
        Some(url::Host::Ipv6(ip)) => ip == Ipv6Addr::LOCALHOST,
        _ => false,
    };
    if url.scheme() != "http"
        || !loopback
        || url.port() != Some(command.ports().webdriver)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
    {
        return Err(launch_error(format!(
            "WebDriver endpoint must be http://127.0.0.1:{} (or [::1]) without path or credentials",
            command.ports().webdriver
        )));
    }

    let relay = &prepared.relay;
    if !relay.binary.is_absolute()
        || relay.binary.file_name() != Some(OsStr::new(NETNS_RELAY_BINARY_NAME))
    {
        return Err(launch_error(format!(
            "relay binary must be an absolute path to {NETNS_RELAY_BINARY_NAME}"
        )));
    }
    let ports = command.ports();
    if relay.listen_port == 0
        || relay.listen_port == ports.webdriver
        || relay.listen_port == ports.bidi
    {
        return Err(launch_error(
            "relay listen port must be nonzero and distinct from the geckodriver ports",
        ));
    }
    if !relay.proxy_socket.is_absolute() {
        return Err(launch_error("relay proxy socket path must be absolute"));
    }
    Ok(())
}

/// Verifies the pin, prepares, audits and spawns a sandboxed geckodriver.
///
/// # Description
/// The SHA-256 check runs on Tokio's blocking pool. Nothing is spawned unless
/// the hash matches and [`validate_prepared_launch`] accepts the command line.
///
/// # Errors
/// - [`AdapterError::DriverPin`]: hash or file checks fail.
/// - [`AdapterError::Launch`]: rejected command line, launcher failure, or the
///   blocking hash task could not complete.
pub async fn launch_pinned_driver(
    launcher: &dyn BrowserLauncher,
    pin: &GeckodriverPin,
) -> Result<LaunchedDriver, AdapterError> {
    let pin_for_hash = pin.clone();
    let verified = tokio::task::spawn_blocking(move || pin_for_hash.verify())
        .await
        .map_err(|error| {
            launch_error(format!("geckodriver verification task failed: {error}"))
        })??;
    let command = GeckodriverCommand::new(verified, launcher.driver_ports())?;
    let prepared = launcher.prepare(&command)?;
    if let Err(error) = validate_prepared_launch(&command, &prepared) {
        tracing::error!(%error, "rejected sandbox command line for geckodriver");
        return Err(error);
    }
    let process = launcher.spawn(&prepared).await?;
    tracing::info!(
        pid = ?process.id(),
        webdriver_url = %prepared.webdriver_url,
        relay_port = prepared.relay.listen_port,
        "sandboxed geckodriver started"
    );
    Ok(LaunchedDriver { prepared, process })
}

// Streams the file through SHA-256 without loading it fully into memory.
fn sha256_file(path: &Path) -> Result<[u8; 32], AdapterError> {
    let mut file = File::open(path)
        .map_err(|error| pin_error(path, format!("cannot open geckodriver: {error}")))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; HASH_CHUNK_BYTES];
    let mut total: u64 = 0;
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| pin_error(path, format!("cannot read geckodriver: {error}")))?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        if total > MAX_GECKODRIVER_BYTES {
            return Err(pin_error(
                path,
                "geckodriver grew beyond the size limit while hashing",
            ));
        }
        hasher.update(&buffer[..read]);
    }
    let mut digest = [0_u8; 32];
    digest.copy_from_slice(&hasher.finalize());
    Ok(digest)
}

// Branch-free comparison over the full digest length.
fn digest_eq(left: &[u8; 32], right: &[u8; 32]) -> bool {
    left.iter()
        .zip(right.iter())
        .fold(0_u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

fn parse_sha256_hex(value: &str) -> Option<[u8; 32]> {
    let bytes = value.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    let mut digest = [0_u8; 32];
    for (index, pair) in bytes.chunks_exact(2).enumerate() {
        digest[index] = (hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?;
    }
    Some(digest)
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn pin_error(path: &Path, detail: impl Into<String>) -> AdapterError {
    AdapterError::DriverPin {
        path: path.to_path_buf(),
        detail: detail.into(),
    }
}

fn launch_error(detail: impl Into<String>) -> AdapterError {
    AdapterError::Launch {
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use std::sync::atomic::{AtomicUsize, Ordering};

    // Temporary file removed on drop; test-only helper.
    struct TempFile {
        dir: PathBuf,
        path: PathBuf,
    }

    impl TempFile {
        fn new(contents: &[u8]) -> TestResult<Self> {
            let dir =
                std::env::temp_dir().join(format!("harw-geckodriver-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&dir).map_err(ctx("temp dir is created"))?;
            let path = dir.join("geckodriver");
            std::fs::write(&path, contents).map_err(ctx("temp geckodriver is written"))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                    .map_err(ctx("permissions are set"))?;
            }
            Ok(Self { dir, path })
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _cleanup_result = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn sha256_hex_of(bytes: &[u8]) -> String {
        hex(&Sha256::digest(bytes))
    }

    fn verified(file: &TempFile, contents: &[u8]) -> TestResult<VerifiedGeckodriver> {
        let pin = GeckodriverPin::new(file.path.to_path_buf(), &sha256_hex_of(contents))
            .map_err(ctx("valid pin"))?;
        pin.verify().map_err(ctx("matching hash verifies"))
    }

    const PORTS: DriverPorts = DriverPorts {
        webdriver: 4444,
        bidi: 9222,
    };

    /// Baut eine gültige [`PreparedLaunch`]-Fixtur. Gibt `Result` zurück (statt
    /// zu paniken), weil dieselbe Funktion auch aus `FakeLauncher::prepare`
    /// (Trait-Signatur `Result<PreparedLaunch, AdapterError>`) aufgerufen wird.
    fn good_prepared(command: &GeckodriverCommand) -> Result<PreparedLaunch, AdapterError> {
        let mut argv: Vec<OsString> = [
            "/usr/bin/bwrap",
            "--unshare-all",
            "--unshare-net",
            "--die-with-parent",
            "--",
            "/run/harw/harw-netns-relay",
            "1080",
            "/run/harw/egress.sock",
            "--",
        ]
        .iter()
        .map(OsString::from)
        .collect();
        argv.extend(command.argv().iter().cloned());
        Ok(PreparedLaunch {
            argv,
            webdriver_url: url::Url::parse("http://127.0.0.1:4444/").map_err(|_| {
                AdapterError::Launch {
                    detail: "invalid fixture url".to_owned(),
                }
            })?,
            relay: RelayEndpoint {
                binary: PathBuf::from("/usr/libexec/harw/harw-netns-relay"),
                listen_port: 1080,
                proxy_socket: PathBuf::from("/run/user/1000/harw/egress.sock"),
            },
        })
    }

    #[derive(Debug)]
    struct FakeProcess;

    impl DriverProcess for FakeProcess {
        fn id(&self) -> Option<u32> {
            Some(42)
        }
    }

    #[derive(Debug)]
    struct FakeLauncher {
        share_net: bool,
        spawns: AtomicUsize,
    }

    #[async_trait]
    impl BrowserLauncher for FakeLauncher {
        fn driver_ports(&self) -> DriverPorts {
            PORTS
        }

        fn prepare(&self, command: &GeckodriverCommand) -> Result<PreparedLaunch, AdapterError> {
            let mut prepared = good_prepared(command)?;
            if self.share_net {
                prepared.argv.insert(1, OsString::from("--share-net"));
            }
            Ok(prepared)
        }

        async fn spawn(
            &self,
            _prepared: &PreparedLaunch,
        ) -> Result<Box<dyn DriverProcess>, AdapterError> {
            self.spawns.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(FakeProcess))
        }
    }

    #[test]
    fn test_geckodriver_pin_new_rejects_relative_path_and_bad_digest() -> TestResult {
        let digest = "a".repeat(64);
        assert!(matches!(
            GeckodriverPin::new(PathBuf::from("geckodriver"), &digest),
            Err(AdapterError::DriverPin { .. })
        ));
        for bad in [
            "A".repeat(64),
            "a".repeat(63),
            "g".repeat(64),
            String::new(),
        ] {
            assert!(matches!(
                GeckodriverPin::new(PathBuf::from("/opt/geckodriver"), &bad),
                Err(AdapterError::DriverPin { .. })
            ));
        }
        let pin = GeckodriverPin::new(PathBuf::from("/opt/geckodriver"), &digest)
            .map_err(ctx("valid pin"))?;
        assert_eq!(pin.sha256_hex(), digest);
        Ok(())
    }

    #[test]
    fn test_geckodriver_pin_verify_accepts_matching_hash() -> TestResult {
        let contents = b"#!/bin/false\npinned geckodriver fixture\n";
        let file = TempFile::new(contents)?;
        let token = verified(&file, contents)?;
        assert_eq!(token.path(), file.path.as_path());
        assert_eq!(token.sha256_hex(), sha256_hex_of(contents));
        Ok(())
    }

    #[test]
    fn test_geckodriver_pin_verify_wrong_hash_is_error() -> TestResult {
        let file = TempFile::new(b"real bytes")?;
        let pin = GeckodriverPin::new(file.path.to_path_buf(), &sha256_hex_of(b"other bytes"))
            .map_err(ctx("valid pin"))?;
        match pin.verify() {
            Err(AdapterError::DriverPin { detail, .. }) => assert!(detail.contains("mismatch")),
            other => {
                return Err(TestError::Unexpected(format!(
                    "wrong hash must be rejected, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn test_geckodriver_pin_verify_rejects_symlink_and_world_writable() -> TestResult {
        use std::os::unix::fs::PermissionsExt;
        let contents = b"bytes";
        let file = TempFile::new(contents)?;
        let link = file.dir.join("link");
        std::os::unix::fs::symlink(&file.path, &link).map_err(ctx("symlink is created"))?;
        let pin = GeckodriverPin::new(link, &sha256_hex_of(contents)).map_err(ctx("valid pin"))?;
        assert!(
            matches!(pin.verify(), Err(AdapterError::DriverPin { detail, .. }) if detail.contains("symbolic link"))
        );

        std::fs::set_permissions(&file.path, std::fs::Permissions::from_mode(0o777))
            .map_err(ctx("permissions are set"))?;
        let pin = GeckodriverPin::new(file.path.to_path_buf(), &sha256_hex_of(contents))
            .map_err(ctx("valid pin"))?;
        assert!(
            matches!(pin.verify(), Err(AdapterError::DriverPin { detail, .. }) if detail.contains("writable"))
        );
        Ok(())
    }

    #[test]
    fn test_geckodriver_command_new_builds_loopback_argv_and_rejects_bad_ports() -> TestResult {
        let contents = b"cmd";
        let file = TempFile::new(contents)?;
        let command = GeckodriverCommand::new(verified(&file, contents)?, PORTS)
            .map_err(ctx("valid command"))?;
        let argv: Vec<String> = command
            .argv()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(argv[0], file.path.to_string_lossy());
        assert_eq!(
            &argv[1..],
            [
                "--host",
                "127.0.0.1",
                "--port",
                "4444",
                "--websocket-port",
                "9222",
                "--allow-hosts",
                "127.0.0.1",
                "--allow-origins",
                "http://127.0.0.1:4444",
                "--log",
                "warn",
            ]
        );
        for ports in [
            DriverPorts {
                webdriver: 0,
                bidi: 9222,
            },
            DriverPorts {
                webdriver: 4444,
                bidi: 4444,
            },
        ] {
            assert!(matches!(
                GeckodriverCommand::new(verified(&file, contents)?, ports),
                Err(AdapterError::Launch { .. })
            ));
        }
        Ok(())
    }

    #[test]
    fn test_validate_prepared_launch_accepts_isolated_command_without_share_net() -> TestResult {
        let contents = b"ok";
        let file = TempFile::new(contents)?;
        let command = GeckodriverCommand::new(verified(&file, contents)?, PORTS)
            .map_err(ctx("valid command"))?;
        let prepared = good_prepared(&command).map_err(ctx("good_prepared"))?;
        assert!(!prepared.argv.iter().any(|arg| arg == "--share-net"));
        validate_prepared_launch(&command, &prepared)
            .map_err(ctx("isolated launch is accepted"))?;
        Ok(())
    }

    #[test]
    fn test_validate_prepared_launch_rejects_unsafe_command_lines() -> TestResult {
        let contents = b"bad";
        let file = TempFile::new(contents)?;
        let command = GeckodriverCommand::new(verified(&file, contents)?, PORTS)
            .map_err(ctx("valid command"))?;

        let mut share_net = good_prepared(&command).map_err(ctx("good_prepared"))?;
        share_net.argv.insert(2, OsString::from("--share-net"));
        let mut no_unshare = good_prepared(&command).map_err(ctx("good_prepared"))?;
        no_unshare
            .argv
            .retain(|arg| arg != "--unshare-net" && arg != "--unshare-all");
        let mut tampered = good_prepared(&command).map_err(ctx("good_prepared"))?;
        if let Some(last) = tampered.argv.last_mut() {
            *last = OsString::from("trace");
        }
        let mut bare = good_prepared(&command).map_err(ctx("good_prepared"))?;
        bare.argv = command.argv().to_vec();
        let mut remote_url = good_prepared(&command).map_err(ctx("good_prepared"))?;
        remote_url.webdriver_url =
            url::Url::parse("http://10.0.0.2:4444/").map_err(ctx("valid url"))?;
        let mut wrong_port = good_prepared(&command).map_err(ctx("good_prepared"))?;
        wrong_port.webdriver_url =
            url::Url::parse("http://127.0.0.1:5555/").map_err(ctx("valid url"))?;
        let mut wrong_relay = good_prepared(&command).map_err(ctx("good_prepared"))?;
        wrong_relay.relay.binary = PathBuf::from("/usr/bin/socat");
        let mut relay_port_clash = good_prepared(&command).map_err(ctx("good_prepared"))?;
        relay_port_clash.relay.listen_port = 9222;
        let mut relative_program = good_prepared(&command).map_err(ctx("good_prepared"))?;
        relative_program.argv[0] = OsString::from("bwrap");

        for (name, prepared) in [
            ("share-net", share_net),
            ("no unshare", no_unshare),
            ("tampered geckodriver argv", tampered),
            ("no sandbox wrapper", bare),
            ("non-loopback url", remote_url),
            ("wrong port", wrong_port),
            ("wrong relay", wrong_relay),
            ("relay port clash", relay_port_clash),
            ("relative program", relative_program),
        ] {
            assert!(
                matches!(
                    validate_prepared_launch(&command, &prepared),
                    Err(AdapterError::Launch { .. })
                ),
                "{name} must be rejected"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_launch_pinned_driver_never_spawns_share_net_or_wrong_hash() -> TestResult {
        let contents = b"launch";
        let file = TempFile::new(contents)?;
        let pin = GeckodriverPin::new(file.path.to_path_buf(), &sha256_hex_of(contents))
            .map_err(ctx("valid pin"))?;

        let unsafe_launcher = FakeLauncher {
            share_net: true,
            spawns: AtomicUsize::new(0),
        };
        assert!(matches!(
            launch_pinned_driver(&unsafe_launcher, &pin).await,
            Err(AdapterError::Launch { .. })
        ));
        assert_eq!(unsafe_launcher.spawns.load(Ordering::SeqCst), 0);

        let launcher = FakeLauncher {
            share_net: false,
            spawns: AtomicUsize::new(0),
        };
        let wrong = GeckodriverPin::new(file.path.to_path_buf(), &sha256_hex_of(b"x"))
            .map_err(ctx("valid pin"))?;
        assert!(matches!(
            launch_pinned_driver(&launcher, &wrong).await,
            Err(AdapterError::DriverPin { .. })
        ));
        assert_eq!(launcher.spawns.load(Ordering::SeqCst), 0);

        let launched = launch_pinned_driver(&launcher, &pin)
            .await
            .map_err(ctx("safe launch spawns"))?;
        assert_eq!(launcher.spawns.load(Ordering::SeqCst), 1);
        assert_eq!(launched.proxy_port(), 1080);
        assert_eq!(launched.webdriver_url().as_str(), "http://127.0.0.1:4444/");
        Ok(())
    }
}
