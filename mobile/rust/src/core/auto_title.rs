//! Mobile owner-bound automatic title publication. No model or prompt access.

use std::sync::{Arc, Mutex, Weak};

use harw_protocol::session_wire::{
    AutoTitlePublication as WirePublication, AutoTitlePublish, AutoTitleSession, features,
};

use super::{
    BackendKind, CancellationToken, CoreState, Link, MobileCore, MobileError, ServerProfile,
    ValidatedProfile, parse_session_id,
};

#[derive(uniffi::Object)]
pub struct AutoTitleAccess {
    link: Weak<Link>,
    expected_profile: Option<ServerProfile>,
    attachment: CancellationToken,
    scope: AutoTitleSession,
    seed_digest: Mutex<Option<[u8; 32]>>,
}

#[derive(uniffi::Record)]
pub struct AutoTitleSeedPair {
    pub user: String,
    pub assistant: String,
}

#[derive(Clone, Copy, Debug, uniffi::Enum)]
pub enum AutoTitlePublication {
    Applied,
    AlreadyTitled,
    Stale,
}

impl From<WirePublication> for AutoTitlePublication {
    fn from(value: WirePublication) -> Self {
        match value {
            WirePublication::Applied => Self::Applied,
            WirePublication::AlreadyTitled => Self::AlreadyTitled,
            WirePublication::Stale => Self::Stale,
        }
    }
}

#[uniffi::export]
impl MobileCore {
    /// Captures the exact profile/link/attachment without transport I/O.
    /// The subsequent seed RPC performs host-side ownership and title checks.
    pub fn auto_title_access(
        &self,
        session_id: String,
        expected_profile: Option<ServerProfile>,
    ) -> Result<Option<Arc<AutoTitleAccess>>, MobileError> {
        let expected_profile = expected_profile.map(ValidatedProfile::new).transpose()?.map(|profile| profile.profile);
        let session_id = parse_session_id(session_id)?;
        let state = self.shared.state();
        let Ok(link) = state.active() else { return Ok(None) };
        if !profile_matches(&state, &link, expected_profile.as_ref()) || !enabled(&link) {
            return Ok(None);
        }
        let Some((expected_generation, attachment)) = state.session(&session_id).ok()
            .and_then(|session| session.auto_title_attachment()) else { return Ok(None) };
        if expected_generation != 0 {
            return Ok(None);
        }
        Ok(Some(Arc::new(AutoTitleAccess {
            link: Arc::downgrade(&link),
            expected_profile,
            seed_digest: Mutex::new(None),
            attachment,
            scope: AutoTitleSession { session_id, expected_generation },
        })))
    }

    /// Only exactly three bounded, committed user/agent pairs; no replay tail,
    /// older authorities, attachment objects, reasoning, or tool data.
    pub fn auto_title_seed(
        &self,
        access: Arc<AutoTitleAccess>,
    ) -> Result<Option<Vec<AutoTitleSeedPair>>, MobileError> {
        let Some(link) = access.link.upgrade() else { return Ok(None) };
        if !current(&self.shared.state(), &link, &access) {
            return Ok(None);
        }
        let shared = Arc::clone(&self.shared);
        self.run(async move {
            let seed = tokio::select! {
                biased;
                () = access.attachment.cancelled() => return Ok(None),
                result = link.request(link.backend.port().auto_title_seed(access.scope.clone())) => result?,
            };
            let Some(seed) = seed else { return Ok(None) };
            if !current(&shared.state(), &link, &access)
                || seed.pairs.iter().any(|pair| {
                    [&pair.user, &pair.assistant].iter().any(|text| text.trim().is_empty() || text.len() > 4096)
                })
            {
                return Ok(None);
            }
            let mut expected = digest_slot(&access)?;
            if expected.is_some_and(|current| current != seed.digest) {
                return Ok(None);
            }
            *expected = Some(seed.digest);
            Ok(Some(seed.pairs.into_iter().map(|pair| AutoTitleSeedPair {
                user: pair.user,
                assistant: pair.assistant,
            }).collect()))
        })
    }

    /// A late model result may do nothing or update an untitled session; it
    /// deliberately never calls the ordinary manual rename command.
    pub fn publish_auto_title(
        &self,
        access: Arc<AutoTitleAccess>,
        title: String,
    ) -> Result<AutoTitlePublication, MobileError> {
        let Some(link) = access.link.upgrade() else { return Ok(AutoTitlePublication::Stale) };
        if !current(&self.shared.state(), &link, &access) {
            return Ok(AutoTitlePublication::Stale);
        }
        if title.trim().is_empty() || title.len() > 800 || title.chars().any(char::is_control) {
            return Err(MobileError::Denied { detail: "Invalid generated title".into() });
        }
        let Some(seed_digest) = *digest_slot(&access)? else { return Ok(AutoTitlePublication::Stale) };
        let params = AutoTitlePublish { scope: access.scope.clone(), title, seed_digest };
        let shared = Arc::clone(&self.shared);
        self.run(async move {
            let publication = tokio::select! {
                biased;
                () = access.attachment.cancelled() => return Ok(AutoTitlePublication::Stale),
                result = link.request(link.backend.port().auto_title_publish(params)) => result?,
            };
            if !current(&shared.state(), &link, &access) {
                return Ok(AutoTitlePublication::Stale);
            }
            Ok(publication.into())
        })
    }
}

fn digest_slot(access: &AutoTitleAccess) -> Result<std::sync::MutexGuard<'_, Option<[u8; 32]>>, MobileError> {
    access.seed_digest.lock()
        .map_err(|_| MobileError::Internal { detail: "Automatic title evidence unavailable".into() })
}

fn enabled(link: &Link) -> bool {
    let caps = link.backend.granted();
    let names = link.backend.features();
    caps.observe && caps.steer && names.iter().any(|name| name == features::AUTO_TITLE_CAS_V1)
        && names.iter().any(|name| name == features::AUTO_TITLE_SEED_V1)
}

fn profile_matches(state: &CoreState, link: &Link, expected: Option<&ServerProfile>) -> bool {
    match (link.backend.kind(), expected) {
        (BackendKind::Local, None) => state.profile.is_none(),
        (BackendKind::Remote, Some(expected)) =>
            state.profile.as_ref().is_some_and(|active| &active.profile == expected),
        _ => false,
    }
}

fn current(state: &CoreState, link: &Arc<Link>, access: &AutoTitleAccess) -> bool {
    !access.attachment.is_cancelled() && state.same_link(link) && enabled(link)
        && profile_matches(state, link, access.expected_profile.as_ref())
        && state.session(&access.scope.session_id).ok()
            .and_then(|session| session.auto_title_attachment())
            .is_some_and(|(generation, _)| generation == access.scope.expected_generation)
}
