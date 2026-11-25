//! Operator-only provisioning for historical receipts in the isolated writer.
//! No credential generation, automatic trust, listener, or placement power.

pub(super) mod readiness;

use std::fs::File;
use std::io::Read as _;
use std::os::fd::AsFd as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;

use harw_node_transport::{LocalNode, ML_DSA_65_PUBLIC_KEY_LEN};
use harw_session_host::owner_creation::OwnerCreationAuthority;
use serde::Deserialize;
use zeroize::Zeroizing;

const RECEIPT_PIN: &str = "writer-creation-pin-v1.json";
const RECEIPT_CREDENTIAL: &str = "harw-private-writer-create-seed";
const PIN_LIMIT: u64 = 8 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Pin {
    node: String,
    instance: String,
    receipt_public_key_hex: String,
}

fn owned_private_dir(path: &Path) -> Result<File, String> {
    let invalid = || "writer creation authority directory must be absolute, private and owned".to_owned();
    if !path.is_absolute() {
        return Err(invalid());
    }
    let dir = File::from(harw_fsutil::open_dir_nofollow(path).map_err(|_| invalid())?);
    harw_fsutil::ensure_owned_directory(&dir).map_err(|_| invalid())?;
    if dir.metadata().map_err(|_| invalid())?.mode() & 0o077 != 0 {
        return Err(invalid());
    }
    Ok(dir)
}

fn private_file(dir: &File, name: &str) -> Result<File, String> {
    let invalid = || format!("writer creation authority requires a private regular {name}");
    let file = harw_fsutil::open_beneath(dir.as_fd(), Path::new(name), harw_fsutil::OpenMode::read_only())
        .map_err(|_| invalid())?;
    harw_fsutil::ensure_private_regular(&file).map_err(|_| invalid())?;
    if file.metadata().map_err(|_| invalid())?.nlink() != 1 {
        return Err(invalid());
    }
    Ok(file)
}

fn same_file(dir: &File, name: &str, file: &File) -> Result<(), String> {
    if harw_fsutil::same_file_at(dir.as_fd(), Path::new(name), file)
        .map_err(|_| "writer creation authority file unavailable".to_owned())?
    {
        return Ok(());
    }
    Err("writer creation authority file changed while opening".into())
}

pub(super) fn open(remote: &Path, local: &LocalNode) -> Result<(OwnerCreationAuthority, [u8; 32]), String> {
    let namespace = super::writer_namespace::active_namespace_id(remote)
        .map_err(|_| "writer creation authority requires the active isolated writer guard".to_owned())?;
    let dir = owned_private_dir(remote)?;
    let pin_file = private_file(&dir, RECEIPT_PIN)?;
    let mut pin_bytes = Vec::new();
    (&pin_file).take(PIN_LIMIT + 1).read_to_end(&mut pin_bytes)
        .map_err(|_| "writer creation pin unreadable".to_owned())?;
    if pin_bytes.len() as u64 > PIN_LIMIT {
        return Err("writer creation pin exceeds size limit".into());
    }
    same_file(&dir, RECEIPT_PIN, &pin_file)?;
    let pin: Pin = serde_json::from_slice(&pin_bytes)
        .map_err(|_| "writer creation pin has invalid schema".to_owned())?;
    if pin.node != local.node_id().as_str() || pin.receipt_public_key_hex.len() != ML_DSA_65_PUBLIC_KEY_LEN * 2 {
        return Err("writer creation pin does not match this node".into());
    }
    let key = data_encoding::HEXLOWER.decode(pin.receipt_public_key_hex.as_bytes())
        .map_err(|_| "writer creation key must be lowercase hexadecimal".to_owned())?;
    if key == local.identity.public_key {
        return Err("writer creation credential must not use the transport key".into());
    }
    let credential_dir = std::env::var_os("CREDENTIALS_DIRECTORY")
        .ok_or_else(|| "writer creation needs a dedicated systemd credential".to_owned())?;
    let credential_dir = owned_private_dir(Path::new(&credential_dir))?;
    let credential = private_file(&credential_dir, RECEIPT_CREDENTIAL)?;
    let metadata = credential.metadata()
        .map_err(|_| "writer creation credential unavailable".to_owned())?;
    if metadata.len() != 32 || metadata.mode() & 0o222 != 0 {
        return Err("writer creation credential must be a 32-byte read-only seed".into());
    }
    let mut seed = Zeroizing::new([0u8; 32]);
    let mut reader = &credential;
    reader.read_exact(&mut seed[..])
        .map_err(|_| "writer creation credential unreadable".to_owned())?;
    // A single extra zeroizing byte detects growth without growing the secret allocation.
    let mut extra = Zeroizing::new([0u8; 1]);
    reader.read_exact(&mut extra[..]).map_or_else(
        |error| if error.kind() == std::io::ErrorKind::UnexpectedEof { Ok(()) } else { Err(()) },
        |()| Err(()),
    ).map_err(|()| "writer creation credential length changed".to_owned())?;
    same_file(&credential_dir, RECEIPT_CREDENTIAL, &credential)?;
    same_file(&dir, RECEIPT_PIN, &pin_file)?;
    if super::writer_namespace::active_namespace_id(remote).ok() != Some(namespace) {
        return Err("isolated writer namespace changed while opening".into());
    }
    OwnerCreationAuthority::from_seed(local.node_id().clone(), pin.instance, namespace, &seed, &key)
        .map(|authority| (authority, namespace))
        .map_err(|_| "writer creation credential does not match the pinned authority".into())
}
