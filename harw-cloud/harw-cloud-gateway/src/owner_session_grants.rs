//! Opt-in Hub exact-session Observe grants. The operator selects a private,
//! offline-published snapshot by revision and exact SHA-256. This reader does
//! not write policy, derive grants from enrollment, mount a route or bypass
//! independent destination authorization.

use std::collections::BTreeSet;
use std::fs::{File, Metadata};
use std::io::{self, Read};
use std::num::NonZeroU64;
use std::os::fd::AsFd;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

use aws_lc_rs::digest::{SHA256, digest};
use harw_fsutil::{
    OpenMode, ensure_owned_directory, ensure_private_regular, open_beneath,
    open_dir_nofollow, same_file_at,
};
use harw_node_transport::AuthenticatedPeer;
use serde::Deserialize;
use tokio::sync::Semaphore;

use crate::owner_directory::valid_id;
use crate::owner_ingress::{GrantFuture, HubSessionGrants};
use crate::route_admission::{AttachKind, CallerGrant};

const POLICY: &str = "owner-observe-grants-v1.json";
const SCHEMA: &str = "harw.hub.owner-observe-grants";
const VERSION: u8 = 1;
const MAX_BYTES: u64 = 256 * 1024;
const MAX_GRANTS: usize = 512;
const MAX_IN_FLIGHT: usize = 8;

/// Fresh exact enrollment and membership evidence from the selected Hub's
/// trusted profile authority. No session right is conveyed by this value.
#[derive(Clone, PartialEq, Eq)]
pub struct DirectObserveMembership {
    pub tenant: String,
    pub device: String,
    pub authenticated_node: String,
    pub key_fingerprint: [u8; 32],
    pub issued_revision: NonZeroU64,
}

/// Adapter seam for the existing live profile-admin membership authority.
/// Implementations must independently reread canonical enrollment, current key
/// pin and active membership, match the supplied actual handshake fingerprint,
/// and return None on any ambiguity, revocation or unavailable state. A pin
/// query by node alone, tenant label, device tier or cached auth is not enough.
/// Called twice from bounded blocking jobs; keep each lookup bounded.
pub trait DirectObserveMembershipSource: Send + Sync + 'static {
    fn resolve_current(
        &self, caller: &AuthenticatedPeer, verified_key_fingerprint: [u8; 32],
    ) -> Option<DirectObserveMembership>;
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    schema: String,
    version: u8,
    revision: NonZeroU64,
    grants: Vec<ObserveGrant>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ObserveGrant {
    tenant: String,
    session: String,
    device: String,
    node: String,
    key_fingerprint: [u8; 32],
    membership_revision: NonZeroU64,
}

impl Policy {
    fn valid(&self, expected_revision: NonZeroU64) -> bool {
        if self.schema != SCHEMA || self.version != VERSION
            || self.revision != expected_revision || self.grants.len() > MAX_GRANTS
        {
            return false;
        }
        let mut scopes = BTreeSet::new();
        self.grants.iter().all(|grant| {
            [&grant.tenant, &grant.session, &grant.device, &grant.node]
                .into_iter().all(|value| valid_id(value))
                && grant.key_fingerprint != [0; 32]
                && scopes.insert((&grant.tenant, &grant.session, &grant.device))
        })
    }

    fn allows(&self, session: &str, member: &DirectObserveMembership) -> bool {
        self.grants.iter().any(|grant| {
            grant.session == session && grant.tenant == member.tenant
                && grant.device == member.device && grant.node == member.authenticated_node
                && grant.key_fingerprint == member.key_fingerprint
                && grant.membership_revision == member.issued_revision
        })
    }
}

struct Snapshot {
    root: PathBuf,
    dir: File,
    // Keep the original inode pinned so a replacement cannot revive the
    // running reader, even if it has the same revision and exact bytes.
    selected_record: File,
    selected_metadata: Metadata,
    revision: NonZeroU64,
    sha256: [u8; 32],
    retired: AtomicBool,
}

fn refuse() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "Hub observe grant snapshot unavailable")
}

fn same_file_snapshot(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev() && a.ino() == b.ino() && a.nlink() == 1 && b.nlink() == 1
        && a.len() == b.len() && a.mtime() == b.mtime() && a.mtime_nsec() == b.mtime_nsec()
        && a.ctime() == b.ctime() && a.ctime_nsec() == b.ctime_nsec()
}

impl Snapshot {
    fn confirm_root(&self) -> io::Result<()> {
        let current = File::from(open_dir_nofollow(&self.root)?);
        ensure_owned_directory(&current)?;
        let current = current.metadata()?;
        let pinned = self.dir.metadata()?;
        if (pinned.dev(), pinned.ino()) != (current.dev(), current.ino())
            || current.mode() & 0o077 != 0
        {
            return Err(refuse());
        }
        Ok(())
    }

    fn confirm_record(&self, record: &File, original: &Metadata) -> io::Result<()> {
        self.confirm_root()?;
        ensure_private_regular(record)?;
        if !same_file_snapshot(&self.selected_metadata, original)
            || !same_file_snapshot(&self.selected_metadata, &self.selected_record.metadata()?)
            || !same_file_snapshot(original, &record.metadata()?)
            || !same_file_at(self.dir.as_fd(), Path::new(POLICY), record)?
        {
            return Err(refuse());
        }
        Ok(())
    }

    fn read(&self) -> io::Result<(Policy, File, Metadata)> {
        self.confirm_root()?;
        let record = open_beneath(self.dir.as_fd(), Path::new(POLICY), OpenMode::read_only())?;
        ensure_private_regular(&record)?;
        let original = record.metadata()?;
        if !same_file_snapshot(&self.selected_metadata, &original)
            || !(1..=MAX_BYTES).contains(&original.len())
        {
            return Err(refuse());
        }
        let mut bytes = Vec::with_capacity(original.len() as usize);
        (&record).take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 != original.len() || digest(&SHA256, &bytes).as_ref() != &self.sha256[..] {
            return Err(refuse());
        }
        let policy: Policy = serde_json::from_slice(&bytes).map_err(|_| refuse())?;
        if !policy.valid(self.revision) {
            return Err(refuse());
        }
        self.confirm_record(&record, &original)?;
        Ok((policy, record, original))
    }
}

/// Explicit owner-private policy reader, never enabled by default. The
/// ordinary attach method refuses all access; only the accepted-channel
/// exact-key Observe method can return a grant. There is no Control policy.
pub struct ExplicitDirectObserveGrants {
    snapshot: Arc<Snapshot>,
    members: Arc<dyn DirectObserveMembershipSource>,
    reads: Arc<Semaphore>,
}

impl ExplicitDirectObserveGrants {
    /// Open the one fixed policy file under an already selected absolute Hub
    /// directory with trusted, stable ancestry. Trusted startup independently
    /// selects the nonzero revision and digest of the entire exact file.
    ///
    /// Replacement, deletion or malformed state immediately denies that
    /// instance when observed; it does not accept a newer or restored snapshot
    /// automatically. Deliberate policy change requires a newly selected,
    /// strictly newer revision and digest, and retiring old composition.
    /// There is no persisted rollout counter or multi-Hub consensus here.
    ///
    /// # Errors
    /// Returns an error for unsafe paths, missing/invalid policy or pin mismatch.
    pub fn open(
        root: &Path, revision: NonZeroU64, sha256: [u8; 32],
        members: Arc<dyn DirectObserveMembershipSource>,
    ) -> io::Result<Self> {
        if !root.is_absolute() || sha256 == [0; 32] {
            return Err(refuse());
        }
        let dir = File::from(open_dir_nofollow(root)?);
        ensure_owned_directory(&dir)?;
        if dir.metadata()?.mode() & 0o077 != 0 { return Err(refuse()); }
        let selected_record = open_beneath(dir.as_fd(), Path::new(POLICY), OpenMode::read_only())?;
        ensure_private_regular(&selected_record)?;
        let selected_metadata = selected_record.metadata()?;
        let snapshot = Arc::new(Snapshot {
            root: root.to_owned(),
            dir,
            selected_record,
            selected_metadata,
            revision,
            sha256,
            retired: AtomicBool::new(false),
        });
        snapshot.read()?;
        Ok(Self { snapshot, members, reads: Arc::new(Semaphore::new(MAX_IN_FLIGHT)) })
    }
}

impl HubSessionGrants for ExplicitDirectObserveGrants {
    fn resolve<'a>(
        &'a self, _caller: &'a AuthenticatedPeer, _session_id: &'a str, _kind: AttachKind,
    ) -> GrantFuture<'a> {
        Box::pin(async { None })
    }

    fn resolve_direct_observe<'a>(
        &'a self, caller: &'a AuthenticatedPeer, verified_key_fingerprint: [u8; 32],
        session_id: &'a str,
    ) -> GrantFuture<'a> {
        Box::pin(async move {
            if verified_key_fingerprint == [0; 32] || !valid_id(session_id)
                || self.snapshot.retired.load(Ordering::Acquire)
            {
                return None;
            }
            let permit = self.reads.clone().try_acquire_owned().ok()?;
            let snapshot = self.snapshot.clone();
            let members = self.members.clone();
            let caller = caller.clone();
            let session = session_id.to_owned();
            tokio::task::spawn_blocking(move || {
                // A cancelled waiter does not release the job's read budget.
                let _permit = permit;
                let member = members.resolve_current(&caller, verified_key_fingerprint)?;
                if member.authenticated_node != caller.node_id.as_str()
                    || member.key_fingerprint != verified_key_fingerprint
                    || !valid_id(&member.tenant) || !valid_id(&member.device)
                {
                    return None;
                }
                let (policy, record, original) = match snapshot.read() {
                    Ok(value) => value,
                    Err(_) => {
                        snapshot.retired.store(true, Ordering::Release);
                        return None;
                    }
                };
                if !policy.allows(&session, &member)
                    || members.resolve_current(&caller, verified_key_fingerprint).as_ref() != Some(&member)
                {
                    return None;
                }
                if snapshot.confirm_record(&record, &original).is_err() {
                    snapshot.retired.store(true, Ordering::Release);
                    return None;
                }
                if snapshot.retired.load(Ordering::Acquire) {
                    return None;
                }
                Some(CallerGrant {
                    tenant: member.tenant,
                    device_id: member.device,
                    authenticated_node: member.authenticated_node,
                    policy_revision: policy.revision.get(),
                    session_id: session,
                    may_observe: true,
                    may_control: false,
                })
            }).await.ok()?
        })
    }
}
