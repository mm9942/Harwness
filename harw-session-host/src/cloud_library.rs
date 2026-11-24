//! Private Cloud Library storage for explicit uploads. Unlike the Android PIN
//! vault, this is gateway-owned encryption at rest (not E2EE).

use std::{
    fs::File,
    io::{self, Read},
    os::{fd::AsFd, unix::fs::MetadataExt},
    path::{Path, PathBuf},
    sync::Arc,
};

use base64::{Engine, engine::general_purpose::STANDARD};
use crypt_guard::pq_hpke::{
    Aead, HpkeEnvelope, Kdf, Kem, Suite, derive_recipient_key_pair,
};
use harw_fsutil::{
    AtomicWriteOptions, AtomicWriteOutcome, OpenMode, ensure_owned_directory,
    ensure_private_regular, mkdir_at, open_beneath, open_dir_at, open_dir_nofollow,
    write_atomic_with_outcome,
};
use harw_protocol::{
    cloud_library::{
        CLOUD_LIBRARY_MAX_ASSETS, CLOUD_LIBRARY_MAX_JPEG_BYTES, CloudLibraryAssetV1,
        CloudLibraryCommandV1, CloudLibraryReplyV1,
    },
    ImageFormat,
};
use harw_types::{AuthStrength, PrincipalKind, SessionId, TrustZone};
use zeroize::Zeroizing;

use crate::{ClientIdentity, HostError};

const DIRECTORY: &str = "cloud-library-v1";
const LOCK: &str = ".library.lock";
const PROFILE_ANCHOR: &str = ".key-profile-anchor";
const MAGIC: &[u8; 8] = b"HCLB\x01\0\0\0";
const INFO: &[u8] = b"harw/gateway/profile-cloud-library/cgh3/v1";
const SUITE: Suite = Suite::new(Kem::MlKem1024, Kdf::Shake256, Aead::XChaCha20Poly1305);
const MAX_NAME: usize = 160;
const MAX_DESCRIPTION: usize = 4_096;
const MAX_B64: usize = 240_000;
const MAX_SCOPE_FILES: usize = 256;
const MAX_BODY: usize = 3 * 1024 * 1024;
const MAX_WIRE: usize = MAX_BODY + 8 * 1024;

fn storage() -> HostError { HostError::Storage("cloud library storage unavailable".into()) }
fn invalid() -> HostError { HostError::Protocol("invalid cloud library input".into()) }
fn changed() -> HostError { HostError::Busy("cloud library revision changed; reload before retrying".into()) }

pub(super) struct CloudLibrary {
    root: PathBuf,
    fd: File,
    master: Zeroizing<[u8; 32]>,
    profile: [u8; 32],
    workers: Arc<tokio::sync::Semaphore>,
}

struct Compartment {
    aad: [u8; 40],
    filename: String,
    recipient: Zeroizing<[u8; 32]>,
    auth: Zeroizing<[u8; 32]>,
}

struct Asset {
    asset_id: SessionId,
    name: Zeroizing<String>,
    description: Option<Zeroizing<String>>,
    jpeg: Zeroizing<Vec<u8>>,
}

#[derive(Default)]
struct Catalogue {
    revision: u64,
    assets: Vec<Asset>,
}

pub(super) fn authorised(identity: &ClientIdentity) -> bool {
    identity.zone == TrustZone::Remote
        && identity.strength.at_least(AuthStrength::MutualTls)
        && identity.principal.kind() == PrincipalKind::Human
        && identity.agent.is_none()
        && identity.tenant.is_some()
        && identity.device.is_some()
        && identity.authenticated_node.is_some()
}

impl CloudLibrary {
    pub(super) fn open(
        state_dir: &Path, profile: &Path, trusted_key_file: &Path,
    ) -> Result<Self, HostError> {
        if !state_dir.is_absolute() || !profile.is_absolute() || !trusted_key_file.is_absolute() {
            return Err(storage());
        }
        let parent = trusted_key_file.parent().ok_or_else(storage)?;
        let name = trusted_key_file.file_name().ok_or_else(storage)?;
        let holder = File::from(open_dir_nofollow(parent).map_err(|_| storage())?);
        let mut key_file = open_beneath(holder.as_fd(), Path::new(name), OpenMode::read_only())
            .map_err(|_| storage())?;
        private_file(&key_file)?;
        if key_file.metadata().map_err(|_| storage())?.len() != 32 { return Err(storage()); }
        let mut master = Zeroizing::new([0; 32]);
        key_file.read_exact(&mut master[..]).map_err(|_| storage())?;
        if *master == [0; 32] || key_file.read(&mut [0u8; 1]).map_err(|_| storage())? != 0 {
            return Err(storage());
        }
        let state = private_directory(File::from(open_dir_nofollow(state_dir).map_err(|_| storage())?))?;
        if mkdir_at(state.as_fd(), Path::new(DIRECTORY), 0o700).map_err(|_| storage())? {
            state.sync_all().map_err(|_| storage())?;
        }
        let service_root = private_directory(open_dir_at(state.as_fd(), Path::new(DIRECTORY)).map_err(|_| storage())?)?;
        let mut binding = blake3::Hasher::new_derive_key("harw Cloud Library host profile binding v1");
        binding.update(profile.as_os_str().as_encoded_bytes());
        let profile_hash = binding.finalize();
        let profile_component = profile_hash.to_hex();
        let profile_component = Path::new(profile_component.as_str());
        if mkdir_at(service_root.as_fd(), profile_component, 0o700).map_err(|_| storage())? {
            service_root.sync_all().map_err(|_| storage())?;
        }
        let opened = private_directory(open_dir_at(service_root.as_fd(), profile_component).map_err(|_| storage())?)?;
        let store = Self {
            root: state_dir.join(DIRECTORY).join(profile_component),
            fd: opened, master, profile: *profile_hash.as_bytes(),
            workers: Arc::new(tokio::sync::Semaphore::new(2)),
        };
        store.confirm_directory()?;
        store.confirm_anchor()?;
        Ok(store)
    }

    fn lock(&self) -> Result<File, HostError> {
        let lock = open_beneath(
            self.fd.as_fd(), Path::new(LOCK),
            OpenMode { create: true, ..OpenMode::read_write() },
        ).map_err(|_| storage())?;
        private_file(&lock)?;
        match lock.try_lock() {
            Ok(()) => Ok(lock),
            Err(std::fs::TryLockError::WouldBlock) => Err(HostError::Busy("cloud library is busy".into())),
            Err(std::fs::TryLockError::Error(_)) => Err(storage()),
        }
    }

    /// Fail at startup, rather than presenting an empty library under the
    /// wrong key. This is an authenticated binding, not a rollback counter.
    fn confirm_anchor(&self) -> Result<(), HostError> {
        let _guard = self.lock()?;
        let mut keyed = Zeroizing::new(blake3::Hasher::new_keyed(&self.master));
        keyed.update(b"harw Cloud Library key/profile anchor v1");
        keyed.update(&self.profile);
        let expected = keyed.finalize();
        match open_beneath(self.fd.as_fd(), Path::new(PROFILE_ANCHOR), OpenMode::read_only()) {
            Ok(mut existing) => {
                private_file(&existing)?;
                let mut stored = [0; 32];
                existing.read_exact(&mut stored).map_err(|_| storage())?;
                if existing.read(&mut [0u8; 1]).map_err(|_| storage())? != 0 || expected != *stored.as_slice() {
                    return Err(storage());
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let mut entries = std::fs::read_dir(&self.root).map_err(|_| storage())?;
                entries.try_for_each(|entry| {
                    if entry.map_err(|_| storage())?.file_name() != LOCK { return Err(storage()); }
                    Ok(())
                })?;
                self.confirm_directory()?;
                write_encrypted(&self.root.join(PROFILE_ANCHOR), expected.as_bytes())?;
                self.fd.sync_all().map_err(|_| storage())?;
            }
            Err(_) => return Err(storage()),
        }
        Ok(())
    }

    pub(super) fn admit_worker(&self) -> Result<tokio::sync::OwnedSemaphorePermit, HostError> {
        Arc::clone(&self.workers).try_acquire_owned()
            .map_err(|_| HostError::Busy("cloud library is busy".into()))
    }

    fn compartment(&self, identity: &ClientIdentity) -> Result<Compartment, HostError> {
        if !authorised(identity) { return Err(HostError::NotFound); }
        let tenant = identity.tenant.as_ref().ok_or(HostError::NotFound)?;
        let device = identity.device.as_ref().ok_or(HostError::NotFound)?;
        let node = identity.authenticated_node.as_ref().ok_or(HostError::NotFound)?;
        let mut stamp = blake3::Hasher::new_derive_key("harw Cloud Library exact device scope v1");
        for part in [
            self.profile.as_slice(), tenant.as_str().as_bytes(), identity.principal.id().as_bytes(),
            device.as_str().as_bytes(), node.as_str().as_bytes(),
        ] {
            stamp.update(&(part.len() as u64).to_be_bytes());
            stamp.update(part);
        }
        let mut aad = [0; 40];
        aad[..8].copy_from_slice(MAGIC);
        aad[8..].copy_from_slice(stamp.finalize().as_bytes());
        let derive = |domain| {
            let mut h = Zeroizing::new(blake3::Hasher::new_derive_key(domain));
            h.update(&self.master[..]);
            h.update(&aad);
            Zeroizing::new(*h.finalize().as_bytes())
        };
        let name = derive("harw Cloud Library private filename v1");
        let filename = format!("{}.cgr", blake3::Hash::from_bytes(*name).to_hex());
        Ok(Compartment {
            aad, filename,
            recipient: derive("harw Cloud Library ML-KEM-1024 recipient v1"),
            auth: derive("harw Cloud Library keyed envelope authentication v1"),
        })
    }

    pub(super) fn run(
        &self, identity: &ClientIdentity, input: CloudLibraryCommandV1,
        live: impl Fn() -> Result<(), HostError>,
        publish: impl FnOnce(&Path, &[u8]) -> Result<(), HostError>,
    ) -> Result<CloudLibraryReplyV1, HostError> {
        live()?;
        let key = self.compartment(identity)?;
        self.confirm_directory()?;
        let _guard = self.lock()?;
        live()?;
        let existing = self.load(&key)?;
        let is_new = existing.is_none();
        let mut catalogue = existing.unwrap_or_default();
        match input {
            CloudLibraryCommandV1::List => {
                live()?;
                return Ok(CloudLibraryReplyV1::Snapshot {
                    revision: catalogue.revision,
                    assets: catalogue.assets.into_iter().map(|item| CloudLibraryAssetV1 {
                        asset_id: item.asset_id,
                        name: item.name.to_string(),
                        description: item.description.map(|value| value.to_string()),
                    }).collect(),
                });
            }
            CloudLibraryCommandV1::Preview { expected_revision, asset_id } => {
                require_revision(&catalogue, expected_revision)?;
                let item = item_mut(&mut catalogue, &asset_id)?;
                live()?;
                return Ok(CloudLibraryReplyV1::Preview {
                    asset_id,
                    data_b64: STANDARD.encode(&item.jpeg[..]),
                });
            }
            CloudLibraryCommandV1::Import { expected_revision, name, data_b64 } => {
                let name = Zeroizing::new(name);
                let data_b64 = Zeroizing::new(data_b64);
                require_revision(&catalogue, expected_revision)?;
                if catalogue.assets.len() >= CLOUD_LIBRARY_MAX_ASSETS {
                    return Err(HostError::Denied("cloud library image limit reached".into()));
                }
                let jpeg = validated_image(&data_b64)?;
                let name = validated_name(&name)?;
                let asset_id = SessionId::new();
                if catalogue.assets.iter().any(|item| item.asset_id == asset_id) { return Err(storage()); }
                catalogue.assets.push(Asset { asset_id, name, description: None, jpeg });
            }
            CloudLibraryCommandV1::Rename { expected_revision, asset_id, name } => {
                let name = Zeroizing::new(name);
                require_revision(&catalogue, expected_revision)?;
                item_mut(&mut catalogue, &asset_id)?.name = validated_name(&name)?;
            }
            CloudLibraryCommandV1::Describe { expected_revision, asset_id, description } => {
                let description = description.map(Zeroizing::new);
                require_revision(&catalogue, expected_revision)?;
                item_mut(&mut catalogue, &asset_id)?.description =
                    description.as_ref().map(|value| validated_description(value)).transpose()?;
            }
            CloudLibraryCommandV1::Remove { expected_revision, asset_id } => {
                require_revision(&catalogue, expected_revision)?;
                let found = catalogue.assets.iter().position(|item| item.asset_id == asset_id)
                    .ok_or(HostError::NotFound)?;
                catalogue.assets.remove(found);
            }
        };
        if is_new { self.require_scope_capacity()?; }
        catalogue.revision = catalogue.revision.checked_add(1).ok_or_else(storage)?;
        let wire = key.seal(&catalogue)?;
        self.confirm_directory()?;
        publish(&self.root.join(&key.filename), &wire)?;
        self.fd.sync_all().map_err(|_| storage())?;
        live()?;
        Ok(CloudLibraryReplyV1::Updated { revision: catalogue.revision })
    }

    fn require_scope_capacity(&self) -> Result<(), HostError> {
        let mut count = 0;
        for file in std::fs::read_dir(&self.root).map_err(|_| storage())? {
            let name = file.map_err(|_| storage())?.file_name();
            if name.to_str().is_some_and(|name| name.len() == 68 && name.ends_with(".cgr")) {
                count += 1;
                if count >= MAX_SCOPE_FILES {
                    return Err(HostError::Denied("cloud library scope capacity reached".into()));
                }
            }
        }
        Ok(())
    }

    fn confirm_directory(&self) -> Result<(), HostError> {
        let current = private_directory(File::from(open_dir_nofollow(&self.root).map_err(|_| storage())?))?
            .metadata().map_err(|_| storage())?;
        let pinned = self.fd.metadata().map_err(|_| storage())?;
        if (current.dev(), current.ino()) != (pinned.dev(), pinned.ino()) { return Err(storage()); }
        Ok(())
    }

    fn load(&self, compartment: &Compartment) -> Result<Option<Catalogue>, HostError> {
        let file = match open_beneath(self.fd.as_fd(), Path::new(&compartment.filename), OpenMode::read_only()) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(storage()),
        };
        private_file(&file)?;
        let expected = usize::try_from(file.metadata().map_err(|_| storage())?.len()).map_err(|_| storage())?;
        if !(MAGIC.len() + 33..=MAX_WIRE).contains(&expected) { return Err(storage()); }
        let mut wire = Vec::with_capacity(expected);
        file.take(MAX_WIRE as u64 + 1).read_to_end(&mut wire).map_err(|_| storage())?;
        if wire.len() != expected { return Err(storage()); }
        compartment.unseal(&wire).map(Some)
    }
}

pub(super) fn write_encrypted(path: &Path, wire: &[u8]) -> Result<(), HostError> {
    match write_atomic_with_outcome(path, wire, AtomicWriteOptions::private()).map_err(|_| storage())? {
        AtomicWriteOutcome::Committed => Ok(()),
        AtomicWriteOutcome::DurabilityUncertain(_) => Err(storage()),
    }
}

impl Compartment {
    fn tag(&self, wire: &[u8]) -> blake3::Hash {
        let mut h = Zeroizing::new(blake3::Hasher::new_keyed(&self.auth));
        h.update(INFO);
        h.update(&self.aad);
        h.update(wire);
        h.finalize()
    }

    fn seal(&self, value: &Catalogue) -> Result<Vec<u8>, HostError> {
        let body = encode_catalogue(value)?;
        let recipient = derive_recipient_key_pair(SUITE.kem(), &self.recipient[..]).map_err(|_| storage())?;
        let wire = HpkeEnvelope::seal(SUITE, recipient.public_key(), INFO, &self.aad, &body)
            .map_err(|_| storage())?.try_to_bytes().map_err(|_| storage())?;
        if MAGIC.len() + 32 + wire.len() > MAX_WIRE { return Err(storage()); }
        Ok([MAGIC.as_slice(), self.tag(&wire).as_bytes(), &wire].concat())
    }

    fn unseal(&self, wire: &[u8]) -> Result<Catalogue, HostError> {
        let frame = wire.strip_prefix(MAGIC).ok_or_else(storage)?;
        let (tag, sealed) = frame.split_at_checked(32).ok_or_else(storage)?;
        if self.tag(sealed) != *tag { return Err(storage()); }
        let envelope = HpkeEnvelope::from_bytes(sealed).map_err(|_| storage())?;
        if envelope.suite() != SUITE { return Err(storage()); }
        let recipient = derive_recipient_key_pair(SUITE.kem(), &self.recipient[..]).map_err(|_| storage())?;
        let body = envelope.open_zeroizing(recipient.private_key(), INFO, &self.aad).map_err(|_| storage())?;
        decode_catalogue(&body)
    }
}

fn private_directory(file: File) -> Result<File, HostError> {
    ensure_owned_directory(&file).map_err(|_| storage())?;
    if file.metadata().map_err(|_| storage())?.mode() & 0o077 != 0 { return Err(storage()); }
    Ok(file)
}

fn private_file(file: &File) -> Result<(), HostError> {
    ensure_private_regular(file).map_err(|_| storage())?;
    if file.metadata().map_err(|_| storage())?.nlink() != 1 { return Err(storage()); }
    Ok(())
}

fn require_revision(value: &Catalogue, expected: u64) -> Result<(), HostError> {
    if value.revision == expected { Ok(()) } else { Err(changed()) }
}

fn item_mut<'a>(value: &'a mut Catalogue, asset_id: &SessionId) -> Result<&'a mut Asset, HostError> {
    value.assets.iter_mut().find(|item| &item.asset_id == asset_id).ok_or(HostError::NotFound)
}

fn validated_name(name: &str) -> Result<Zeroizing<String>, HostError> {
    let clean = name.trim();
    if clean.is_empty() || clean.len() > MAX_NAME || clean.chars().any(char::is_control) {
        return Err(invalid());
    }
    Ok(Zeroizing::new(clean.to_owned()))
}

fn validated_description(text: &str) -> Result<Zeroizing<String>, HostError> {
    let clean = text.trim();
    if clean.is_empty() || clean.len() > MAX_DESCRIPTION ||
        clean.chars().any(|ch| ch.is_control() && !matches!(ch, '\n' | '\t'))
    {
        return Err(invalid());
    }
    Ok(Zeroizing::new(clean.to_owned()))
}

fn validated_image(encoded: &str) -> Result<Zeroizing<Vec<u8>>, HostError> {
    if encoded.len() > MAX_B64 { return Err(invalid()); }
    let raw = Zeroizing::new(STANDARD.decode(encoded.as_bytes()).map_err(|_| invalid())?);
    if raw.len() > CLOUD_LIBRARY_MAX_JPEG_BYTES { return Err(invalid()); }
    let inspected = harw_media::inspect(&raw).map_err(|_| invalid())?;
    if inspected.format != ImageFormat::Jpeg || inspected.width > 1280 || inspected.height > 1280 {
        return Err(invalid());
    }
    let safe = Zeroizing::new(harw_media::sanitize(&raw, ImageFormat::Jpeg).map_err(|_| invalid())?);
    if safe.len() > CLOUD_LIBRARY_MAX_JPEG_BYTES { return Err(invalid()); }
    Ok(safe)
}

fn encode_catalogue(value: &Catalogue) -> Result<Zeroizing<Vec<u8>>, HostError> {
    let size = value.assets.iter().fold(10usize, |size, item| {
        size.saturating_add(16).saturating_add(item.asset_id.as_str().len())
            .saturating_add(item.name.len()).saturating_add(item.jpeg.len())
            .saturating_add(item.description.as_ref().map_or(0, |text| text.len()))
    });
    if size > MAX_BODY || value.assets.len() > CLOUD_LIBRARY_MAX_ASSETS { return Err(storage()); }
    let mut body = Zeroizing::new(Vec::with_capacity(size));
    body.extend_from_slice(&value.revision.to_be_bytes());
    body.extend_from_slice(&u16::try_from(value.assets.len()).map_err(|_| storage())?.to_be_bytes());
    for asset in &value.assets {
        write_field(&mut body, asset.asset_id.as_str().as_bytes())?;
        write_field(&mut body, asset.name.as_bytes())?;
        write_field(&mut body, asset.description.as_ref().map_or(&[][..], |text| text.as_bytes()))?;
        write_field(&mut body, &asset.jpeg)?;
    }
    Ok(body)
}

fn decode_catalogue(mut body: &[u8]) -> Result<Catalogue, HostError> {
    if body.len() > MAX_BODY { return Err(storage()); }
    let revision = u64::from_be_bytes(take(&mut body, 8)?.try_into().map_err(|_| storage())?);
    let count = u16::from_be_bytes(take(&mut body, 2)?.try_into().map_err(|_| storage())?) as usize;
    if revision == 0 || count > CLOUD_LIBRARY_MAX_ASSETS { return Err(storage()); }
    let mut assets: Vec<Asset> = Vec::with_capacity(count);
    for _ in 0..count {
        let id = SessionId::try_from_str(read_text(&mut body, 64)?.to_owned()).map_err(|_| storage())?;
        if assets.iter().any(|asset| asset.asset_id == id) { return Err(storage()); }
        let name = validated_name(read_text(&mut body, MAX_NAME)?).map_err(|_| storage())?;
        let caption = read_text(&mut body, MAX_DESCRIPTION)?;
        let description = if caption.is_empty() { None } else {
            Some(validated_description(caption).map_err(|_| storage())?)
        };
        let jpeg = read_field(&mut body, CLOUD_LIBRARY_MAX_JPEG_BYTES)?;
        if !jpeg.starts_with(&[0xff, 0xd8]) || !jpeg.ends_with(&[0xff, 0xd9]) { return Err(storage()); }
        assets.push(Asset { asset_id: id, name, description, jpeg: Zeroizing::new(jpeg.to_vec()) });
    }
    if !body.is_empty() { return Err(storage()); }
    Ok(Catalogue { revision, assets })
}

fn write_field(out: &mut Vec<u8>, bytes: &[u8]) -> Result<(), HostError> {
    out.extend_from_slice(&u32::try_from(bytes.len()).map_err(|_| storage())?.to_be_bytes());
    out.extend_from_slice(bytes);
    Ok(())
}

fn take<'a>(input: &mut &'a [u8], size: usize) -> Result<&'a [u8], HostError> {
    let (part, rest) = input.split_at_checked(size).ok_or_else(storage)?;
    *input = rest;
    Ok(part)
}

fn read_field<'a>(input: &mut &'a [u8], max: usize) -> Result<&'a [u8], HostError> {
    let size = u32::from_be_bytes(take(input, 4)?.try_into().map_err(|_| storage())?) as usize;
    if size > max { return Err(storage()); }
    take(input, size)
}

fn read_text<'a>(input: &mut &'a [u8], max: usize) -> Result<&'a str, HostError> {
    std::str::from_utf8(read_field(input, max)?).map_err(|_| storage())
}
