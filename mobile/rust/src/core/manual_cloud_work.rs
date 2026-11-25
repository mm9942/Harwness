//! One selected cloud peer; this exports a transient capability, never a send endpoint.
use super::*;
use std::sync::{OnceLock, Weak};
#[cfg(all(feature = "vault-uia-chat", target_os = "android"))]
use harw_protocol::session_wire::{ManualCloudWorkCommandV1, ManualCloudWorkReplyV1};

static ACCESS: OnceLock<Mutex<HashMap<String, Weak<ManualCloudWorkAccess>>>> = OnceLock::new();

#[derive(uniffi::Object)]
pub struct ManualCloudWorkAccess {
    ticket: String,
    shared: Weak<Shared>,
    link: Weak<Link>,
    authority: [u8; 32],
    epoch: u64,
}

#[uniffi::export]
impl ManualCloudWorkAccess {
    pub fn route_ticket(&self) -> String { self.ticket.clone() }
    pub fn is_current(&self) -> bool { self.current_link().is_some() }
}

#[uniffi::export]
impl MobileCore {
    pub fn manual_cloud_work_access(&self) -> Option<Arc<ManualCloudWorkAccess>> {
        let state = self.shared.state();
        let link = state.active().ok()?;
        let authority = peer_pin(&state, &link)?;
        let mut slots = ACCESS.get_or_init(Default::default).lock().ok()?;
        slots.retain(|_, value| value.strong_count() != 0);
        if slots.len() >= 64 { return None; }
        let ticket = SessionId::new().as_str().to_owned();
        let access = Arc::new(ManualCloudWorkAccess {
            ticket: ticket.clone(), shared: Arc::downgrade(&self.shared),
            link: Arc::downgrade(&link), authority, epoch: state.profile_epoch,
        });
        slots.insert(ticket, Arc::downgrade(&access));
        Some(access)
    }
}

impl ManualCloudWorkAccess {
    #[cfg(all(feature = "vault-uia-chat", target_os = "android"))]
    pub(crate) fn lookup(ticket: &str) -> Option<Arc<Self>> {
        ACCESS.get()?.lock().ok()?.get(ticket)?.upgrade()
    }
    #[cfg(all(feature = "vault-uia-chat", target_os = "android"))]
    pub(crate) fn authority(&self) -> [u8; 32] { self.authority }
    fn current_link(&self) -> Option<Arc<Link>> {
        let shared = self.shared.upgrade()?;
        let link = self.link.upgrade()?;
        let state = shared.state();
        (state.profile_epoch == self.epoch && peer_pin(&state, &link) == Some(self.authority))
            .then(|| Arc::clone(&link))
    }

    #[cfg(all(feature = "vault-uia-chat", target_os = "android"))]
    pub(crate) async fn dispatch(&self, operation_id: String, publish: bool, local_active: impl Fn() -> bool)
        -> Result<ManualCloudWorkReplyV1, MobileError>
    {
        let link = self.current_link().ok_or(MobileError::Disconnected)?;
        let command = if publish { ManualCloudWorkCommandV1::CreateEmpty { operation_id } }
            else { ManualCloudWorkCommandV1::Resolve { operation_id } };
        // Check native local authority immediately before first poll and before delivery.
        let result = link.request(async {
            if !local_active() {
                return Err(PortError::Denied("local Assistant locked".into()));
            }
            link.backend.port().manual_cloud_work(command).await
        }).await?;
        if !local_active() || self.current_link().is_none() { return Err(MobileError::Disconnected); }
        if let Some(shared) = self.shared.upgrade() { shared.notifier.sessions(); }
        Ok(result)
    }
}

fn peer_pin(state: &CoreState, link: &Arc<Link>) -> Option<[u8; 32]> {
    let caps = link.backend.granted();
    if !state.same_link(link) || link.backend.kind() != BackendKind::Remote
        || !caps.observe || !caps.steer
        || !link.backend.ack().features.iter().any(|feature| feature == features::MANUAL_CLOUD_WORK_V1)
    { return None; }
    let profile = &state.profile.as_ref()?.profile;
    let mut hash = blake3::Hasher::new_derive_key("harw Android manual Cloud work peer pin v1");
    for field in [Some(&profile.server_node_id), Some(&profile.server_public_key_hex),
        Some(&profile.endpoint), profile.onion.as_ref()] {
        let field = field.map_or("", String::as_str);
        hash.update(&(field.len() as u64).to_be_bytes());
        hash.update(field.as_bytes());
    }
    Some(*hash.finalize().as_bytes())
}
