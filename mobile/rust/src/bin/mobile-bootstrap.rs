//! Opt-in foreground launcher for an isolated phone test gateway.

use std::env;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, TcpListener};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use clap::{Args, Parser, Subcommand};
use harw_infra_client::{AuthHubClient, ClientOptions, InfraClientError, KeyProfile, KeyRef};
use harw_mobile_ffi::{
    DeviceEnrollment, MobileCore, MobileError, ServerProfile, SubmitOutcome, TranscriptKind,
    generate_identity, parse_device_enrollment, parse_server_profile,
};
use harw_node_listener::{DeviceRecord, DeviceRegistry};
use harw_types::{DeviceId, NodeId, PermissionTier, SessionId, TenantId};
use rustix::fs::{Mode, OFlags};
use rustix::process::{Pid, Signal, geteuid, kill_process};
use tempfile::{Builder, NamedTempFile};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child as AsyncChild, Command as AsyncCommand};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const SERVER_NODE: &str = "gateway-mobile";
const PROFILE_FILE: &str = "harw-server-profile.json";
const SMOKE_PROVIDER: &str = "mobile-smoke";
const SMOKE_MODEL: &str = "mobile-smoke-text";
const SMOKE_RESPONSE: &str = "HARW MOBILE SMOKE REPLY";
const IDENTITY_MARKER: &str = "harw-mobile-bootstrap-sealed-v1\n";
const ONION_DIR: &str = "onion";
const ONION_LOCATOR_FILE: &str = "locator";
const ONION_READY_TIMEOUT: Duration = Duration::from_secs(600);
const PROVIDER_ENV: &str = "HARW_MOBILE_TEST_PROVIDER_TOKEN";

#[derive(Parser)]
#[command(
    about = "Start an isolated gateway (loopback, tailnet or Tor onion) for the Harw phone test"
)]
struct Cli {
    #[command(subcommand)]
    command: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Enroll a phone and run the test gateway in the foreground.
    Serve(Serve),
    /// Verify the real gateway, authentication, session attach and a fixture reply from this host.
    SmokeTest(Binaries),
}

#[derive(Args)]
struct Binaries {
    /// Installed Harw executable with remote session listener support.
    #[arg(long, default_value = "harw")]
    harw: PathBuf,
    /// AuthHub executable.
    #[arg(long, default_value = "harw-auth-hub")]
    auth_hub: PathBuf,
}

#[derive(Args)]
struct Serve {
    /// Absolute, unused test directory. The launcher creates it with mode 0700.
    #[arg(long)]
    state: PathBuf,
    /// The phone's exported public enrollment JSON.
    #[arg(long)]
    enrollment: PathBuf,
    /// Bind the authenticated gateway to this server's active Tailscale IP.
    #[arg(long)]
    tailnet: bool,
    /// Gateway port. USB mode forwards this port to loopback.
    #[arg(long, default_value_t = 7443, value_parser = clap::value_parser!(u16).range(1..))]
    port: u16,
    /// Use a deterministic loopback model fixture instead of a real provider.
    #[arg(
        long,
        required_unless_present_any = ["provider_file", "chatgpt_model"],
        conflicts_with = "provider_file"
    )]
    smoke: bool,
    /// Use the enrolled phone to connect a ChatGPT plan and run this eligible model.
    #[arg(long, conflicts_with_all = ["provider_file", "model_file", "smoke"])]
    chatgpt_model: Option<String>,
    /// Existing provider TOML with an env credential reference or explicit auth_header = "none".
    #[arg(long, requires = "model_file")]
    provider_file: Option<PathBuf>,
    /// Existing model TOML for the selected provider.
    #[arg(long, requires = "provider_file")]
    model_file: Option<PathBuf>,
    /// Give this one phone the additional permission to respond to its own sessions' approvals.
    #[arg(long)]
    allow_approval: bool,
    /// Absolute, dedicated directory for the sealed node identity across gateway runs.
    #[arg(long, requires = "kek_file")]
    identity_dir: Option<PathBuf>,
    /// Existing 32-byte, owner-only raw KEK file outside the identity and state directories.
    #[arg(long, requires = "identity_dir")]
    kek_file: Option<PathBuf>,
    /// Also publish the gateway as a Tor onion service (the harw executable must be built with --features onion).
    #[arg(long)]
    onion: bool,
    /// Absolute, dedicated directory that keeps the onion identity (stable .onion address) across runs.
    #[arg(long, requires = "onion")]
    onion_dir: Option<PathBuf>,
    #[command(flatten)]
    binaries: Binaries,
}

fn main() -> ExitCode {
    let result = match Cli::parse().command {
        Action::Serve(args) => run_server(args),
        Action::SmokeTest(binaries) => smoke_test(&binaries),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mobile-bootstrap: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run_server(args: Serve) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = serve(args) => result,
            signal = tokio::signal::ctrl_c() => Ok(signal?),
            _ = terminate.recv() => Ok(()),
        }
    })
}

fn tailnet_address(status: &harw_tailscale::Status) -> Result<std::net::IpAddr> {
    if !status.is_running() {
        return Err("Tailscale must be connected before starting the tailnet gateway".into());
    }
    status
        .ips
        .iter()
        .copied()
        .filter(|ip| harw_tailscale::is_tailnet_ip(*ip))
        .min_by_key(|ip| !ip.is_ipv4())
        .ok_or_else(|| "tailscaled did not report an assigned tailnet address".into())
}

async fn serve(args: Serve) -> Result<()> {
    let (enrollment, record) =
        enrollment_record(&read_text(&args.enrollment, 8192)?, args.allow_approval)?;
    let real_provider = args
        .provider_file
        .as_ref()
        .zip(args.model_file.as_ref())
        .map(|(provider, model)| ProviderFiles::real(provider, model))
        .transpose()?;
    let real_provider = match args.chatgpt_model.as_deref() {
        Some(model) => Some(ProviderFiles::chatgpt_plan(model)?),
        None => real_provider,
    };
    let harw = executable(&args.binaries.harw)?;
    let auth_hub = executable(&args.binaries.auth_hub)?;
    let listen_ip = if args.tailnet {
        let status = harw_tailscale::LocalApi::discover()?.status().await?;
        tailnet_address(&status)?
    } else {
        Ipv4Addr::LOCALHOST.into()
    };
    let listen = std::net::SocketAddr::new(listen_ip, args.port).to_string();
    // Report an existing listener before creating test state. The gateway's
    // own bind and readiness message also cover races after this check.
    drop(TcpListener::bind(&listen)?);
    let onion_dir = if args.onion {
        Some(prepare_onion_dir(args.onion_dir.as_deref(), &args.state)?)
    } else {
        None
    };
    create_workspace(&args.state)?;
    let home = args.state.join("home");
    let onion_dir = match onion_dir {
        Some(Some(dir)) => Some(dir),
        Some(None) => {
            eprintln!(
                "note: without --onion-dir the .onion address is stored in --state and changes with each fresh --state"
            );
            Some(home.join("profiles/default/session-host").join(ONION_DIR))
        }
        None => None,
    };
    let persistent = args
        .identity_dir
        .as_ref()
        .zip(args.kek_file.as_ref())
        .map(|(dir, kek)| PersistentIdentity::open(&args.state, dir, kek))
        .transpose()?;

    let private = args.state.join("private");
    let environment = Builder::new()
        .prefix("harw-phone-run-")
        .tempdir_in("/tmp")?;
    let cwd = environment.path().join("workspace");
    let user_home = environment.path().join("user");
    let runtime_dir = environment.path().join("runtime");
    for path in [&home, &private, &cwd, &user_home, &runtime_dir] {
        private_dir(path)?;
    }
    let remote = home.join("profiles/default/session-host/remote");
    create_private_tree(&home, Path::new("profiles/default/session-host/remote"))?;
    write_registry(&remote, &enrollment, &record)?;

    let socket = runtime_dir.join("secure.sock");
    let hub_config = private.join("hub.toml");
    let storage = persistent
        .as_ref()
        .map_or_else(String::new, PersistentIdentity::hub_settings);
    write_new(
        &hub_config,
        format!(
            "[[peers]]\nuid = {}\nprincipal = \"mobile-test\"\n\
             grants = [{{ namespace = \"harw.node-identity\", ops = [\"admin\", \"read-public\", \"sign\"] }}]\n{storage}",
            geteuid().as_raw(),
        ),
    )?;

    let command = |bin: &Path| {
        let mut command = AsyncCommand::new(bin);
        command
            .kill_on_drop(true)
            .env_clear()
            .env(
                "PATH",
                env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into()),
            )
            .env("HOME", &user_home)
            .env("XDG_RUNTIME_DIR", &runtime_dir)
            .current_dir(&cwd)
            .stdin(Stdio::null());
        command
    };
    let mut hub = command(&auth_hub)
        .arg("--config")
        .arg(&hub_config)
        .arg("--socket")
        .arg(&socket)
        .spawn()?;
    let public_key = provision_identity(&socket, &mut hub, persistent.is_some()).await?;

    let (provider, mut fixture) = match real_provider {
        Some(provider) => (provider, None),
        None => {
            eprintln!("SMOKE MODE — using a deterministic local model fixture.");
            let mut child = command(Path::new("python3"))
                .args([
                    "-u",
                    "-c",
                    include_str!("../../../scripts/smoke-provider.py"),
                    SMOKE_MODEL,
                    SMOKE_RESPONSE,
                ])
                .stdout(Stdio::piped())
                .spawn()?;
            let stdout = child
                .stdout
                .take()
                .ok_or("missing fixture readiness pipe")?;
            let mut lines = BufReader::new(stdout).lines();
            let port: u16 = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
                .await??
                .ok_or("fixture exited before allocating a loopback socket")?
                .parse()?;
            (ProviderFiles::smoke(port), Some(child))
        }
    };
    provider.write(&home)?;
    let provider_owner = args
        .chatgpt_model
        .as_ref()
        .map(|_| {
            format!(
                "provider_link_device = {}\n",
                toml_string(record.device.as_str())
            )
        })
        .unwrap_or_default();
    let approval = if args.allow_approval {
        format!(
            "approval_device = {}\n",
            toml_string(record.device.as_str())
        )
    } else {
        String::new()
    };
    let onion_config = onion_dir
        .as_deref()
        .map_or_else(String::new, onion_settings);
    write_new(
        &home.join("config.toml"),
        format!(
            "default_provider = {}\ndefault_model = {}\n\
             [dream]\nenabled = false\n\
             [infrastructure]\nauth_socket = {}\n\
             [session_listener]\nenabled = true\nlisten = {}\nallow_non_loopback = {}\n\
             node_id = \"{SERVER_NODE}\"\ntier = \"observer\"\nshared_socket_tenant = {}\n{approval}{provider_owner}{onion_config}",
            toml_string(&provider.provider),
            toml_string(&provider.model),
            path_string(&socket),
            toml_string(&listen),
            args.tailnet,
            toml_string(record.tenant.as_str()),
        ),
    )?;

    let mut profile = ServerProfile {
        version: 1,
        label: if args.smoke {
            "Harw mobile smoke gateway"
        } else {
            "Harw mobile test gateway"
        }
        .to_owned(),
        endpoint: listen.clone(),
        server_node_id: SERVER_NODE.to_owned(),
        server_public_key_hex: hex::encode(public_key),
        onion: None,
    };
    // With onion the profile can only be completed once the gateway has
    // published its locator, so it is built at the readiness line instead.
    let direct_profile_json = if onion_dir.is_none() {
        let json = serde_json::to_string_pretty(&profile)?;
        parse_server_profile(json.clone())?;
        Some(json)
    } else {
        None
    };
    let profile_path = args.state.join(PROFILE_FILE);

    let mut gateway_command = command(&harw);
    gateway_command
        .arg("--home")
        .arg(&home)
        .args(["--profile", "default", "gateway"])
        .arg(format!(
            "--session-socket={}",
            args.state.join("session.sock").display()
        ))
        .stderr(Stdio::piped());
    if let Some(token) = provider.token {
        gateway_command.env(PROVIDER_ENV, token);
    }
    let mut gateway = gateway_command.spawn()?;
    let stderr = gateway
        .stderr
        .take()
        .ok_or("missing gateway readiness pipe")?;

    let readiness = format!(
        "session socket : {}",
        args.state.join("session.sock").display()
    );
    let (ready_tx, ready_rx) = tokio::sync::watch::channel(false);
    let observe_gateway = async {
        let mut ready = false;
        let mut lines = BufReader::new(stderr).lines();
        while let Some(line) = lines.next_line().await? {
            eprintln!("{line}");
            if !ready && line.trim() == readiness {
                let (profile_json, routes) = match (&direct_profile_json, &onion_dir) {
                    (Some(json), _) => (json.clone(), "direct".to_owned()),
                    (None, Some(dir)) => {
                        let text = read_text(&dir.join(ONION_LOCATOR_FILE), 128)?;
                        let locator: harw_onion::OnionLocator = text.trim().parse()?;
                        profile.onion = Some(locator.to_string());
                        let json = serde_json::to_string_pretty(&profile)?;
                        parse_server_profile(json.clone())?;
                        (json, format!("direct {listen}, onion"))
                    }
                    (None, None) => return Err("missing onion directory".into()),
                };
                let mut file = NamedTempFile::new_in(&args.state)?;
                writeln!(file, "{profile_json}")?;
                file.persist_noclobber(&profile_path)?;
                if onion_dir.is_some() {
                    eprintln!(
                        "Public server profile: {} (routes: {routes})",
                        profile_path.display()
                    );
                } else {
                    eprintln!("Public server profile: {}", profile_path.display());
                }
                ready = true;
                ready_tx.send_replace(true);
            }
        }
        let status = gateway.wait().await?;
        Err(format!("test gateway exited ({status})").into())
    };

    let ready_deadline = async {
        if onion_dir.is_some() {
            readiness_deadline(ready_rx, ONION_READY_TIMEOUT).await
        } else {
            std::future::pending().await
        }
    };
    tokio::select! {
        result = observe_gateway => result,
        result = ready_deadline => result,
        status = hub.wait() => Err(format!("test AuthHub exited ({})", status?).into()),
        status = wait_fixture(&mut fixture) => Err(format!("model fixture exited ({})", status?).into()),
    }
}

/// Bounds only the wait for readiness: errors when `ready` does not become
/// true within `limit`, and stays pending forever once it has, so a ready
/// gateway keeps running until it exits or the user stops it.
async fn readiness_deadline(
    mut ready: tokio::sync::watch::Receiver<bool>,
    limit: Duration,
) -> Result<()> {
    let reached = tokio::time::timeout(limit, ready.wait_for(|ready| *ready))
        .await
        .is_ok();
    if reached {
        std::future::pending().await
    } else {
        Err(format!(
            "test gateway did not become ready within {} s",
            limit.as_secs()
        )
        .into())
    }
}

async fn wait_fixture(fixture: &mut Option<AsyncChild>) -> io::Result<std::process::ExitStatus> {
    match fixture {
        Some(child) => child.wait().await,
        None => std::future::pending().await,
    }
}

async fn provision_identity(
    socket: &Path,
    hub: &mut AsyncChild,
    persistent: bool,
) -> Result<Vec<u8>> {
    let timeout = Instant::now() + Duration::from_secs(15);
    let client = AuthHubClient::new(socket, None, ClientOptions::default());
    loop {
        if let Some(status) = hub.try_wait()? {
            return Err(format!("test AuthHub exited before it was ready ({status})").into());
        }
        if socket.exists() {
            break;
        }
        if Instant::now() >= timeout {
            return Err("timed out waiting for the test AuthHub socket".into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let key = KeyRef::latest("harw.node-identity", SERVER_NODE)?;
    if persistent {
        match client.public_key(&key).await {
            Ok(public) => return Ok(public.into_vec()),
            Err(InfraClientError::NotFound) => (),
            Err(error) => return Err(error.into()),
        }
    }
    client.generate_key(&key, KeyProfile::MlDsa65).await?;
    Ok(client.public_key(&key).await?.into_vec())
}

fn enrollment_record(json: &str, approve: bool) -> Result<(DeviceEnrollment, DeviceRecord)> {
    let enrollment = parse_device_enrollment(json)?;
    if matches!(enrollment.node_id.as_str(), SERVER_NODE | "." | "..")
        || enrollment.label.trim() != enrollment.label
        || enrollment.label.contains('|')
        || enrollment.label.chars().any(char::is_control)
    {
        return Err("enrollment contains an unsafe node ID or registry label".into());
    }
    let record = DeviceRecord {
        node_id: NodeId::try_from_str(&enrollment.node_id)?,
        device: DeviceId::try_from_str(&enrollment.node_id)?,
        tenant: TenantId::try_from_str("mobile-test")?,
        tier: PermissionTier::Operator,
        revoked: false,
        label: enrollment.label.clone(),
        approve_optin: approve,
    };
    Ok((enrollment, record))
}

fn write_registry(
    remote: &Path,
    enrollment: &DeviceEnrollment,
    record: &DeviceRecord,
) -> Result<()> {
    let registry = DeviceRegistry::new(remote);
    write_new(registry.path(), format!("{}\n", record.render()))?;
    write_new(
        &remote.join("node-peers.conf"),
        format!(
            "{}|{}\n",
            enrollment.node_id,
            enrollment.public_key_hex.to_ascii_lowercase()
        ),
    )?;
    Ok(())
}

fn validate_state_path(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path.file_name().is_none()
        || path
            .as_os_str()
            .as_bytes()
            .split(|byte| *byte == b'/')
            .any(|part| part == b"." || part == b"..")
        || path.to_str().is_none()
    {
        return Err(
            "state and identity paths must be absolute UTF-8 paths without traversal".into(),
        );
    }
    let uid = geteuid().as_raw();
    for ancestor in path.ancestors().skip(1) {
        let metadata = fs::symlink_metadata(ancestor)?;
        // Root-owned sticky directories such as /tmp may hold private test
        // directories. An untrusted ancestor elsewhere could replace our path.
        let sticky_root = metadata.uid() == 0 && metadata.mode() & 0o1000 != 0;
        let trusted_writer = metadata.uid() == uid || metadata.uid() == 0;
        if !metadata.is_dir() || !trusted_writer || (metadata.mode() & 0o022 != 0 && !sticky_root) {
            return Err(format!("unsafe test directory ancestor: {}", ancestor.display()).into());
        }
    }
    Ok(())
}

/// Validates `--onion-dir` before any state exists. Returns the directory to
/// use, or `None` when the default inside the state tree applies.
fn prepare_onion_dir(dir: Option<&Path>, state: &Path) -> Result<Option<PathBuf>> {
    let Some(dir) = dir else {
        return Ok(None);
    };
    if !dir.is_absolute()
        || dir.to_str().is_none()
        || dir
            .as_os_str()
            .as_bytes()
            .split(|byte| *byte == b'/')
            .any(|part| part == b"." || part == b"..")
    {
        return Err("--onion-dir must be an absolute UTF-8 path without traversal".into());
    }
    if dir.starts_with(state) || state.starts_with(dir) {
        return Err("--onion-dir must not be inside --state (or contain it)".into());
    }
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let metadata = fs::symlink_metadata(dir)?;
    if !metadata.is_dir() || metadata.uid() != geteuid().as_raw() || metadata.mode() & 0o077 != 0 {
        return Err(
            "--onion-dir must be a directory owned by you without group/other access".into(),
        );
    }
    Ok(Some(dir.to_owned()))
}

/// Renders the `[session_listener]` keys that enable the onion service.
fn onion_settings(dir: &Path) -> String {
    format!("onion = true\nonion_dir = {}\n", path_string(dir))
}

fn create_workspace(path: &Path) -> Result<()> {
    validate_state_path(path)?;
    private_dir(path).map_err(|error| {
        format!(
            "cannot create new private test directory {}: {error}",
            path.display()
        )
        .into()
    })
}

fn private_dir(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new().mode(0o700).create(path)
}

fn create_private_tree(base: &Path, relative: &Path) -> io::Result<()> {
    let mut path = base.to_owned();
    for component in relative.components() {
        path.push(component);
        private_dir(&path)?;
    }
    Ok(())
}

fn write_new(path: &Path, data: impl AsRef<[u8]>) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(data.as_ref())
}

fn read_text(path: &Path, size_limit: u64) -> Result<String> {
    read_opened_text(open_regular(path, FileAccess::Input)?, path, size_limit)
}

fn read_opened_text(file: File, path: &Path, size_limit: u64) -> Result<String> {
    let mut content = String::new();
    file.take(size_limit + 1).read_to_string(&mut content)?;
    if content.len() as u64 > size_limit {
        return Err(format!("input is too large: {}", path.display()).into());
    }
    Ok(content)
}

enum FileAccess {
    Input,
    Private,
    Lock,
}

fn open_regular(path: &Path, access: FileAccess) -> Result<File> {
    let io_flags = match access {
        FileAccess::Lock => OFlags::RDWR,
        FileAccess::Input | FileAccess::Private => OFlags::RDONLY,
    };
    let flags = io_flags | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let file = File::from(
        rustix::fs::open(path, flags, Mode::empty())
            .map_err(|error| format!("cannot safely open {}: {error}", path.display()))?,
    );
    let metadata = file.metadata()?;
    let owner = metadata.uid();
    let uid = geteuid().as_raw();
    let mode = metadata.mode();
    let permitted = match access {
        FileAccess::Input => (owner == uid || owner == 0) && mode & 0o022 == 0,
        FileAccess::Private | FileAccess::Lock => owner == uid && mode & 0o077 == 0,
    };
    if !metadata.is_file() || !permitted {
        return Err(format!(
            "unsafe input file type, owner or permissions: {}",
            path.display()
        )
        .into());
    }
    Ok(file)
}

fn executable(path: &Path) -> Result<PathBuf> {
    if path.components().count() == 1 && !path.is_absolute() {
        Ok(path.to_owned())
    } else {
        Ok(path.canonicalize()?)
    }
}

fn toml_string(value: &str) -> String {
    toml::Value::String(value.to_owned()).to_string()
}

fn path_string(path: &Path) -> String {
    toml_string(&path.to_string_lossy())
}

struct PersistentIdentity {
    key_store: PathBuf,
    kek_file: PathBuf,
    _lock: File,
}

impl PersistentIdentity {
    fn open(state: &Path, dir: &Path, kek: &Path) -> Result<Self> {
        validate_state_path(dir)?;
        validate_state_path(kek)?;
        if dir.starts_with(state)
            || state.starts_with(dir)
            || kek.starts_with(dir)
            || kek.starts_with(state)
        {
            return Err("use separate test state, identity storage and KEK paths".into());
        }
        if open_regular(kek, FileAccess::Private)?.metadata()?.len() != 32 {
            return Err("AuthHub KEK must hold exactly 32 raw bytes".into());
        }

        let marker = dir.join(".harw-mobile-identity");
        let lock_path = dir.join("identity.lock");
        match private_dir(dir) {
            Ok(()) => {
                write_new(&lock_path, "")?;
                write_new(&marker, IDENTITY_MARKER)?;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let metadata = fs::symlink_metadata(dir)?;
                if !metadata.is_dir()
                    || metadata.uid() != geteuid().as_raw()
                    || metadata.mode() & 0o777 != 0o700
                {
                    return Err("identity directory must be owned by you and have mode 0700".into());
                }
                let marker_file = open_regular(&marker, FileAccess::Private)?;
                if read_opened_text(marker_file, &marker, 128)? != IDENTITY_MARKER {
                    return Err("identity directory was not created by mobile-bootstrap".into());
                }
            }
            Err(error) => return Err(error.into()),
        }

        let lock = open_regular(&lock_path, FileAccess::Lock)?;
        lock.try_lock()?;
        let key_store = dir.join("node-keys.store");
        Ok(Self {
            key_store,
            kek_file: kek.to_owned(),
            _lock: lock,
        })
    }

    fn hub_settings(&self) -> String {
        format!(
            "[key_store]\nkind = \"sealed\"\npath = {}\nkek_file = {}\n",
            path_string(&self.key_store),
            path_string(&self.kek_file)
        )
    }
}

struct ProviderFiles {
    provider: String,
    model: String,
    provider_toml: String,
    model_toml: String,
    token: Option<OsString>,
}

impl ProviderFiles {
    fn real(provider_file: &Path, model_file: &Path) -> Result<Self> {
        let mut provider: toml::Table = toml::from_str(&read_text(provider_file, 65_536)?)
            .map_err(|_| "invalid provider TOML")?;
        let model_toml = read_text(model_file, 65_536)?;
        let model: toml::Table = toml::from_str(&model_toml).map_err(|_| "invalid model TOML")?;
        let provider_name = required_string(&provider, "name")?.to_owned();
        let model_id = required_string(&model, "id")?.to_owned();
        if required_string(&model, "provider")? != provider_name {
            return Err("the selected model references a different provider".into());
        }
        if provider.contains_key("api_key")
            || provider
                .get("headers")
                .is_some_and(|headers| headers.as_table().is_none_or(|map| !map.is_empty()))
        {
            return Err(
                "choose provider TOML using an auth reference without literal api_key or headers"
                    .into(),
            );
        }
        let token = match provider.get("auth") {
            Some(value) => {
                let var = value.as_str().and_then(|value| value.strip_prefix("env:"))
                    .filter(|name| !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
                    .ok_or("real-provider auth must reference an exported env:NAME credential")?;
                let token = env::var_os(var).filter(|token| !token.is_empty())
                    .ok_or("the selected provider's credential environment variable is missing")?;
                provider.insert("auth".to_owned(), toml::Value::String(format!("env:{PROVIDER_ENV}")));
                Some(token)
            }
            None if provider.get("auth_header").and_then(toml::Value::as_str) == Some("none") => None,
            None => return Err("the selected provider needs auth = \"env:NAME\" or explicit auth_header = \"none\"".into()),
        };
        Ok(Self {
            provider: provider_name,
            model: model_id,
            provider_toml: toml::to_string(&provider)?,
            model_toml,
            token,
        })
    }

    fn chatgpt_plan(model: &str) -> Result<Self> {
        if model.is_empty()
            || model.len() > 128
            || !model
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b'_'))
        {
            return Err("invalid ChatGPT model ID".into());
        }
        let quoted = toml_string(model);
        Ok(Self {
            provider: "chatgpt-plan".into(),
            model: model.to_owned(),
            provider_toml: format!(
                "name = 'chatgpt-plan'\napi = 'openai-chatgpt-plan'\nbase_url = 'https://api.openai.com/v1'\nmodels = [{quoted}]\nstream = true\n"
            ),
            model_toml: format!(
                "id = {quoted}\nprovider = 'chatgpt-plan'\n[capabilities]\nstreaming = true\ntool_calling = true\n"
            ),
            token: None,
        })
    }

    fn smoke(port: u16) -> Self {
        Self {
            provider: SMOKE_PROVIDER.to_owned(),
            model: SMOKE_MODEL.to_owned(),
            provider_toml: format!(
                "name = \"{SMOKE_PROVIDER}\"\napi = \"openai-chat\"\n\
                base_url = \"http://127.0.0.1:{port}/v1\"\nauth_header = \"none\"\n\
                models = [\"{SMOKE_MODEL}\"]\nstream = true\n"
            ),
            model_toml: format!(
                "id = \"{SMOKE_MODEL}\"\nprovider = \"{SMOKE_PROVIDER}\"\n\
                context_window = 32768\nmax_tokens = 256\n\
                [capabilities]\nstreaming = true\ntool_calling = false\n"
            ),
            token: None,
        }
    }

    fn write(&self, home: &Path) -> Result<()> {
        let providers = home.join("providers");
        let models = home.join("models");
        private_dir(&providers)?;
        private_dir(&models)?;
        write_new(&providers.join("phone-test.toml"), &self.provider_toml)?;
        write_new(&models.join("phone-test.toml"), &self.model_toml)?;
        Ok(())
    }
}

fn required_string<'a>(table: &'a toml::Table, key: &str) -> Result<&'a str> {
    table
        .get(key)
        .and_then(toml::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("selected configuration must define `{key}`").into())
}

fn smoke_test(binaries: &Binaries) -> Result<()> {
    let temp = Builder::new()
        .prefix("harw-phone-smoke-")
        .tempdir_in("/tmp")?;
    let state = temp.path().join("server");
    let enrollment = temp.path().join("enrollment.json");
    let identity = generate_identity("Host smoke test device".to_owned())?;
    write_new(&enrollment, serde_json::to_vec(&identity.enrollment)?)?;
    let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?
        .local_addr()?
        .port();
    let mut server = SmokeServer(
        Command::new(env::current_exe()?)
            .args(["serve", "--smoke", "--state"])
            .arg(&state)
            .arg("--enrollment")
            .arg(&enrollment)
            .arg("--port")
            .arg(port.to_string())
            .arg("--harw")
            .arg(&binaries.harw)
            .arg("--auth-hub")
            .arg(&binaries.auth_hub)
            .spawn()?,
    );
    let profile_path = state.join(PROFILE_FILE);
    let profile = wait_for("gateway readiness", Duration::from_secs(60), || {
        if let Some(exit) = server.0.try_wait()? {
            return Err(format!("smoke gateway exited before readiness ({exit})").into());
        }
        if !profile_path.exists() {
            return Ok(None);
        }
        Ok(Some(parse_server_profile(read_text(&profile_path, 8192)?)?))
    })?;

    let core = MobileCore::new(identity.private_key, identity.enrollment.label)?;
    let wrong_pin = ServerProfile {
        server_public_key_hex: identity.enrollment.public_key_hex,
        ..profile.clone()
    };
    if !matches!(core.connect(wrong_pin), Err(MobileError::Connect { .. }))
        || core.list_sessions().is_ok()
    {
        return Err("gateway pin mismatch did not block session access".into());
    }
    eprintln!("Wrong server pin rejected before session access.");

    core.connect(profile)?;
    if !core.status().can_create || !core.status().can_submit || core.status().can_approve {
        return Err("smoke device received unexpected session capabilities".into());
    }
    let first = core.create_session(Some("Mobile smoke A".to_owned()))?;
    let desktop = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let second = desktop.block_on(async {
        use harw_protocol::{SessionPort, session_wire::CreateParams};
        use harw_session_remote::{ConnectOptions, RemotePort, connect_unix};
        let client = RemotePort::new(
            connect_unix(
                state.join("session.sock"),
                ConnectOptions::new("desktop smoke"),
            )
            .await?,
        );
        if !client
            .list()
            .await?
            .iter()
            .any(|entry| entry.session_id.as_str() == first.session_id)
        {
            return Err("desktop cannot see phone-created session".into());
        }
        let session = client
            .create(CreateParams {
                workspace: None,
                title: Some("Desktop smoke B".into()),
            })
            .await?;
        Ok::<_, Box<dyn std::error::Error>>(session.session_id.as_str().to_owned())
    })?;
    let listed = core.list_sessions()?;
    let cases = [
        (first.session_id, "Smoke session A"),
        (second, "Smoke session B"),
    ];
    for (session_id, _) in &cases {
        if !listed.iter().any(|entry| &entry.session_id == session_id) {
            return Err("created mobile smoke session is missing from the session list".into());
        }
        core.open_session(session_id.clone())?;
    }
    for (session_id, prompt) in &cases {
        let head = wait_for("session replay", Duration::from_secs(20), || {
            let snapshot = core.snapshot(session_id.clone())?;
            Ok(if snapshot.online && snapshot.can_submit {
                snapshot.head
            } else {
                None
            })
        })?;
        let receipt = core.submit(
            session_id.clone(),
            (*prompt).to_owned(),
            head,
            SessionId::new().as_str().to_owned(),
        )?;
        if receipt.outcome != SubmitOutcome::Accepted {
            return Err(format!("smoke submit was not accepted: {:?}", receipt.outcome).into());
        }
    }
    for (session_id, prompt) in &cases {
        wait_for(
            "persisted assistant smoke reply",
            Duration::from_secs(60),
            || {
                let snapshot = core.snapshot(session_id.clone())?;
                if let Some(failure) = snapshot.last_failure {
                    return Err(format!("smoke turn failed: {failure}").into());
                }
                let replied = snapshot.transcript.iter().any(|entry| {
                    entry.kind == TranscriptKind::Assistant && entry.text.trim() == SMOKE_RESPONSE
                });
                if !replied {
                    return Ok(None);
                }
                let user_messages: Vec<_> = snapshot
                    .transcript
                    .iter()
                    .filter(|entry| entry.kind == TranscriptKind::User)
                    .map(|entry| entry.text.as_str())
                    .collect();
                if user_messages != [*prompt] {
                    return Err(
                        "session transcript contained missing or unexpected user messages".into(),
                    );
                }
                Ok(Some(()))
            },
        )?;
    }
    core.disconnect();
    println!(
        "Authenticated gateway verified with shared desktop/phone sessions and two independent replies."
    );
    Ok(())
}

fn wait_for<T>(
    description: &str,
    timeout: Duration,
    mut check: impl FnMut() -> Result<Option<T>>,
) -> Result<T> {
    let until = Instant::now() + timeout;
    loop {
        if let Some(result) = check()? {
            return Ok(result);
        }
        if Instant::now() >= until {
            return Err(format!("timed out waiting for {description}").into());
        }
        thread::sleep(Duration::from_millis(100));
    }
}

struct SmokeServer(Child);

impl Drop for SmokeServer {
    fn drop(&mut self) {
        if let Ok(Some(_)) = self.0.try_wait() {
            return;
        }
        if let Some(pid) = Pid::from_raw(self.0.id() as i32) {
            let _ = kill_process(pid, Signal::TERM);
        }
        for _ in 0..30 {
            if let Ok(Some(_)) = self.0.try_wait() {
                return;
            }
            thread::sleep(Duration::from_millis(100));
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn enrollment_json(label: &str, key: &str) -> String {
        serde_json::json!({
            "version": 1, "node_id": "phone-123",
            "public_key_hex": key, "label": label,
        })
        .to_string()
    }

    #[test]
    fn onion_dir_requires_onion() {
        let base = [
            "mobile-bootstrap",
            "serve",
            "--state",
            "/tmp/s",
            "--enrollment",
            "/tmp/e",
            "--smoke",
        ];
        let with_dir = [base.as_slice(), &["--onion-dir", "/tmp/o"]].concat();
        assert!(Cli::try_parse_from(with_dir).is_err());
        let both = [base.as_slice(), &["--onion", "--onion-dir", "/tmp/o"]].concat();
        assert!(Cli::try_parse_from(both).is_ok());
    }

    #[test]
    fn onion_settings_renders_both_keys() -> TestResult {
        let text = onion_settings(Path::new("/var/lib/harw onion"));
        let table: toml::Table = text.parse()?;
        assert_eq!(table.get("onion"), Some(&toml::Value::Boolean(true)));
        assert_eq!(
            table.get("onion_dir").and_then(toml::Value::as_str),
            Some("/var/lib/harw onion")
        );
        Ok(())
    }

    #[test]
    fn onion_dir_validation_rejects_relative_and_state_nested_paths() -> TestResult {
        let parent = tempfile::tempdir_in("/tmp")?;
        let state = parent.path().join("state");
        assert!(prepare_onion_dir(Some(Path::new("relative/onion")), &state).is_err());
        assert!(prepare_onion_dir(Some(&state.join("onion")), &state).is_err());
        assert!(!state.exists());
        let good = parent.path().join("keep/onion");
        assert_eq!(prepare_onion_dir(Some(&good), &state)?, Some(good.clone()));
        assert_eq!(good.metadata()?.mode() & 0o077, 0);
        assert_eq!(prepare_onion_dir(None, &state)?, None);
        Ok(())
    }

    #[test]
    fn enrollment_rejects_malformed_keys_and_registry_delimiters() {
        assert!(enrollment_record(&enrollment_json("Phone", "deadbeef"), false).is_err());
        assert!(enrollment_record(&enrollment_json("Phone", &"zz".repeat(1952)), false).is_err());
        assert!(
            enrollment_record(&enrollment_json("Phone|approve", &"00".repeat(1952)), false)
                .is_err()
        );
        assert!(
            enrollment_record(&enrollment_json("Phone\nspoof", &"00".repeat(1952)), true).is_err()
        );
        let with_path =
            enrollment_json("Phone", &"00".repeat(1952)).replace("phone-123", "../phone");
        assert!(enrollment_record(&with_path, false).is_err());
    }

    #[test]
    fn workspace_requires_a_fresh_path_without_traversal_or_symlinks() -> TestResult {
        let parent = tempfile::tempdir_in("/tmp")?;
        let existing = parent.path().join("existing");
        fs::create_dir(&existing)?;
        fs::write(existing.join("config.toml"), "sentinel")?;
        assert!(create_workspace(&existing).is_err());
        assert_eq!(
            fs::read_to_string(existing.join("config.toml"))?,
            "sentinel"
        );

        let link = parent.path().join("alias");
        std::os::unix::fs::symlink(&existing, &link)?;
        assert!(create_workspace(&link).is_err());
        assert!(create_workspace(&link.join("new")).is_err());
        assert!(create_workspace(&existing.join("../escape")).is_err());
        assert!(!parent.path().join("escape").exists());

        let shared = parent.path().join("shared");
        fs::create_dir(&shared)?;
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o777))?;
        assert!(create_workspace(&shared.join("new")).is_err());
        let fresh = parent.path().join("fresh");
        create_workspace(&fresh)?;
        assert_eq!(fresh.metadata()?.mode() & 0o777, 0o700);
        Ok(())
    }

    #[test]
    fn input_reads_require_regular_trusted_files_and_enforce_size_bounds() -> TestResult {
        let parent = tempfile::tempdir_in("/tmp")?;
        let input = parent.path().join("input");
        write_new(&input, "config")?;
        fs::set_permissions(&input, fs::Permissions::from_mode(0o644))?;
        assert_eq!(read_text(&input, 6)?, "config");
        assert!(read_text(&input, 5).is_err());
        assert!(read_text(parent.path(), 10).is_err());

        let link = parent.path().join("link");
        std::os::unix::fs::symlink(&input, &link)?;
        assert!(read_text(&link, 10).is_err());
        let fifo = parent.path().join("fifo");
        rustix::fs::mkfifoat(rustix::fs::CWD, &fifo, Mode::RUSR | Mode::WUSR)?;
        assert!(read_text(&fifo, 10).is_err());
        fs::set_permissions(&input, fs::Permissions::from_mode(0o664))?;
        assert!(read_text(&input, 10).is_err());
        Ok(())
    }

    #[test]
    fn sealed_identity_requires_private_kek_and_preserves_unowned_storage() -> TestResult {
        let parent = tempfile::tempdir_in("/tmp")?;
        let state = parent.path().join("new-state");
        let identity = parent.path().join("identity");
        let existing = parent.path().join("existing");
        let kek = parent.path().join("test-kek");
        private_dir(&existing)?;
        write_new(&existing.join("node-keys.store"), "sentinel")?;
        write_new(&kek, [0u8; 32])?;

        assert!(PersistentIdentity::open(&state, &existing, &kek).is_err());
        assert_eq!(
            fs::read_to_string(existing.join("node-keys.store"))?,
            "sentinel"
        );
        assert!(!existing.join("identity.lock").exists());

        let link = parent.path().join("kek-link");
        std::os::unix::fs::symlink(&kek, &link)?;
        assert!(PersistentIdentity::open(&state, &identity, &link).is_err());
        fs::set_permissions(&kek, fs::Permissions::from_mode(0o644))?;
        assert!(PersistentIdentity::open(&state, &identity, &kek).is_err());
        assert!(!identity.exists());

        fs::set_permissions(&kek, fs::Permissions::from_mode(0o600))?;
        let owned = PersistentIdentity::open(&state, &identity, &kek)?;
        assert!(PersistentIdentity::open(&state, &identity, &kek).is_err());
        drop(owned);
        assert!(PersistentIdentity::open(&state, &identity, &kek).is_ok());
        Ok(())
    }

    #[test]
    fn registry_grants_one_phone_operator_with_explicit_approval_opt_in() -> TestResult {
        let parent = tempfile::tempdir_in("/tmp")?;
        let workspace = parent.path().join("fresh");
        create_workspace(&workspace)?;
        let key = "00".repeat(1952);
        let enrollment = enrollment_json("Personal test phone", &key);

        for (name, approve, expected) in [
            (
                "default",
                false,
                "phone-123|phone-123|mobile-test|operator|active|Personal test phone\n",
            ),
            (
                "approve",
                true,
                "phone-123|phone-123|mobile-test|operator|active|Personal test phone|approve\n",
            ),
        ] {
            let remote = workspace.join(name);
            private_dir(&remote)?;
            let (public, record) = enrollment_record(&enrollment, approve)?;
            write_registry(&remote, &public, &record)?;
            let registry = DeviceRegistry::new(&remote);
            assert_eq!(fs::read_to_string(registry.path())?, expected);
            assert_eq!(registry.records()?, vec![record]);
            assert_eq!(
                fs::read_to_string(remote.join("node-peers.conf"))?,
                format!("phone-123|{key}\n")
            );
            assert_eq!(registry.path().metadata()?.mode() & 0o777, 0o600);
            assert!(write_registry(&remote, &public, &registry.records()?[0]).is_err());
        }
        Ok(())
    }

    #[test]
    fn chatgpt_mode_requires_an_explicit_model_and_excludes_fixture() -> TestResult {
        let args = [
            "mobile-bootstrap",
            "serve",
            "--state",
            "/tmp/mobile-state",
            "--enrollment",
            "/tmp/phone.json",
            "--chatgpt-model",
            "example-model",
        ];
        assert!(Cli::try_parse_from(args).is_ok());
        assert!(Cli::try_parse_from(args.into_iter().chain(["--smoke"])).is_err());
        let files = ProviderFiles::chatgpt_plan("example-model")?;
        let provider: toml::Table = toml::from_str(&files.provider_toml)?;
        assert_eq!(required_string(&provider, "api")?, "openai-chatgpt-plan");
        assert!(files.token.is_none());
        assert!(ProviderFiles::chatgpt_plan("bad\nmodel").is_err());
        Ok(())
    }

    #[test]
    fn tailnet_profile_uses_an_address_assigned_by_the_running_tailnet() -> TestResult {
        let mut status = harw_tailscale::Status {
            backend_state: "Running".into(),
            dns_name: String::new(),
            tailnet: None,
            ips: vec![
                "192.0.2.1".parse()?,
                "fd7a:115c:a1e0::12".parse()?,
                "100.64.0.12".parse()?,
            ],
        };
        assert_eq!(tailnet_address(&status)?.to_string(), "100.64.0.12");
        status.ips.remove(2);
        assert!(tailnet_address(&status)?.is_ipv6());
        status.backend_state = "Stopped".into();
        assert!(tailnet_address(&status).is_err());
        status.backend_state = "Running".into();
        status.ips.truncate(1);
        assert!(tailnet_address(&status).is_err());
        Ok(())
    }

    #[tokio::test]
    async fn readiness_deadline_errors_when_never_ready() -> TestResult {
        let (_tx, rx) = tokio::sync::watch::channel(false);
        let outcome = readiness_deadline(rx, Duration::from_millis(20)).await;
        assert!(outcome.is_err_and(|error| error.to_string().contains("did not become ready")));
        Ok(())
    }

    #[tokio::test]
    async fn readiness_deadline_never_fires_after_readiness() -> TestResult {
        let (tx, rx) = tokio::sync::watch::channel(false);
        tx.send_replace(true);
        let fired = tokio::time::timeout(
            Duration::from_millis(200),
            readiness_deadline(rx, Duration::from_millis(20)),
        )
        .await;
        assert!(fired.is_err(), "deadline fired after the gateway was ready");
        Ok(())
    }

    type TestResult = Result<()>;
}
