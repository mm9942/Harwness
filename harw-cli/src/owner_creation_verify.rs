//! Offline, read-only verification of an explicitly selected historical receipt.

use std::fs::{File, Metadata};
use std::io::{self, IsTerminal, Read as _, Write as _};
use std::num::NonZeroU64;
use std::os::fd::AsFd as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path};

use harw_cloud_gateway::owner_create_receipt::{
    MAX_OWNER_CREATION_RECEIPT_WIRE_BYTES, OwnerCreationKeyPin, OwnerCreationObserveKey,
    OwnerCreationPinScope,
};
use harw_fsutil::{
    OpenMode, ensure_owned_directory, ensure_private_regular, open_beneath, open_dir_at,
    open_dir_nofollow, same_file_at,
};
use harw_session_host::owner_creation::ML_DSA_65_PUBLIC_KEY_LEN;
use serde::Deserialize;

const PIN_MAX_BYTES: usize = 32 * 1024;
const PIN_VERSION: u8 = 1;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PinConfig {
    version: u8,
    node: String,
    instance: String,
    epoch: NonZeroU64,
    namespace_id_hex: String,
    receipt_public_key_hex: String,
    node_public_key_hex: String,
    observe: ObservePin,
}

#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum ObservePin {
    /// This exact isolated writer must never have had an Observe key.
    Disabled,
    Pinned { public_key_hex: String },
}

fn invalid_pin() -> String {
    "Owner creation: independent pin must be a valid private file under trusted absolute ancestry"
        .to_owned()
}

fn bounded_bytes(reader: impl io::Read, max: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take((max + 1) as u64).read_to_end(&mut bytes)?;
    if bytes.is_empty() || bytes.len() > max {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid input length"));
    }
    Ok(bytes)
}

fn check_ancestor(dir: &File, effective_uid: u32) -> Result<(), String> {
    let meta = dir.metadata().map_err(|_| invalid_pin())?;
    if !meta.is_dir() || (meta.uid() != 0 && meta.uid() != effective_uid) || meta.mode() & 0o022 != 0 {
        return Err(invalid_pin());
    }
    Ok(())
}

fn private_parent(pin_path: &Path) -> Result<File, String> {
    let mut components = pin_path.components();
    if components.next() != Some(Component::RootDir)
        || !components.all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(invalid_pin());
    }
    let parent = pin_path.parent().ok_or_else(invalid_pin)?;
    let uid = rustix::process::geteuid().as_raw();
    let mut dir = File::from(open_dir_nofollow(Path::new("/")).map_err(|_| invalid_pin())?);
    check_ancestor(&dir, uid)?;
    for component in parent.components().skip(1) {
        let Component::Normal(name) = component else { return Err(invalid_pin()); };
        dir = open_dir_at(dir.as_fd(), Path::new(name)).map_err(|_| invalid_pin())?;
        check_ancestor(&dir, uid)?;
    }
    ensure_owned_directory(&dir).map_err(|_| invalid_pin())?;
    if dir.metadata().map_err(|_| invalid_pin())?.mode() & 0o077 != 0 {
        return Err(invalid_pin());
    }
    Ok(dir)
}

fn same_snapshot(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.uid() == b.uid()
        && a.mode() == b.mode()
        && a.nlink() == b.nlink()
        && a.len() == b.len()
        && a.ctime() == b.ctime()
        && a.ctime_nsec() == b.ctime_nsec()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
}

fn decode_fixed<const N: usize>(raw: &str) -> Result<[u8; N], String> {
    if raw.len() != N * 2 {
        return Err(invalid_pin());
    }
    data_encoding::HEXLOWER.decode(raw.as_bytes()).map_err(|_| invalid_pin())?
        .try_into().map_err(|_| invalid_pin())
}

/// Trust configuration is never inferred from stdin. Stdout contains a
/// fixed assertion only; there is no route, placement or store mutation.
pub(crate) fn run(pin_path: &Path, tenant: &str, session: &str) -> Result<(), String> {
    let stdin = io::stdin();
    if stdin.is_terminal() {
        return Err("Owner creation: supply the receipt JSON on stdin".into());
    }
    // Read attacker-controlled stdin before taking a pin snapshot.
    let receipt = bounded_bytes(stdin.lock(), MAX_OWNER_CREATION_RECEIPT_WIRE_BYTES)
        .map_err(|_| "Owner creation: receipt must be bounded JSON from stdin".to_owned())?;

    let dir = private_parent(pin_path)?;
    let dir_metadata = dir.metadata().map_err(|_| invalid_pin())?;
    let basename = Path::new(pin_path.file_name().ok_or_else(invalid_pin)?);
    let file = open_beneath(dir.as_fd(), basename, OpenMode::read_only()).map_err(|_| invalid_pin())?;
    ensure_private_regular(&file).map_err(|_| invalid_pin())?;
    let source = file.metadata().map_err(|_| invalid_pin())?;
    if source.nlink() != 1 || source.len() > PIN_MAX_BYTES as u64 {
        return Err(invalid_pin());
    }
    let pin_bytes = bounded_bytes(&file, PIN_MAX_BYTES).map_err(|_| invalid_pin())?;
    let config: PinConfig = serde_json::from_slice(&pin_bytes).map_err(|_| invalid_pin())?;
    if config.version != PIN_VERSION || source.len() != pin_bytes.len() as u64 {
        return Err(invalid_pin());
    }

    let receipt_key = decode_fixed::<ML_DSA_65_PUBLIC_KEY_LEN>(&config.receipt_public_key_hex)?;
    let node_key = decode_fixed::<ML_DSA_65_PUBLIC_KEY_LEN>(&config.node_public_key_hex)?;
    let observe_key = match &config.observe {
        ObservePin::Disabled => None,
        ObservePin::Pinned { public_key_hex } => Some(decode_fixed::<ML_DSA_65_PUBLIC_KEY_LEN>(public_key_hex)?),
    };
    let pin = OwnerCreationKeyPin::new(
        OwnerCreationPinScope {
            node: &config.node,
            instance: &config.instance,
            epoch: config.epoch,
            namespace_id: decode_fixed::<32>(&config.namespace_id_hex)?,
        },
        &receipt_key,
        &node_key,
        match observe_key.as_ref() {
            None => OwnerCreationObserveKey::Disabled,
            Some(key) => OwnerCreationObserveKey::Pinned(key),
        },
    ).map_err(|error| error.to_string())?;
    let _verified = pin.verify_json(tenant, session, &receipt).map_err(|error| error.to_string())?;

    // Do not report success if the trusted config was replaced or edited
    // during this invocation. Trusted operators still control later rotations.
    let current = file.metadata().map_err(|_| invalid_pin())?;
    let current_dir = private_parent(pin_path)?;
    let current_dir_meta = current_dir.metadata().map_err(|_| invalid_pin())?;
    if !same_snapshot(&source, &current)
        || dir_metadata.dev() != current_dir_meta.dev()
        || dir_metadata.ino() != current_dir_meta.ino()
        || !same_file_at(current_dir.as_fd(), basename, &file).map_err(|_| invalid_pin())?
    {
        return Err("Owner creation: pin changed during verification".into());
    }
    writeln!(
        io::stdout().lock(),
        "Verified historical session creation only. No ownership or liveness verified.",
    ).map_err(|_| "Owner creation: could not write verification result".to_owned())
}
