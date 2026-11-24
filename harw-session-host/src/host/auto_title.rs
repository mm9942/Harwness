//! A generated title is an ordinary session metadata CAS, never a user rename.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use harw_protocol::session_wire::{
    AutoTitlePublication, AutoTitlePublish, AutoTitleSeed, AutoTitleSeedPair, AutoTitleSession,
};
use harw_session_store::{TitleSource, meta};
use harw_types::PermissionTier;

use super::{HostConnection, HostError, HostedState, Need, ProfileShare, Slot, SlotState};

impl HostConnection {
    // Groups neither tenant-wide authority nor shared profile with ownership.
    // A confirmed attachment avoids any cold scan/open of arbitrary sessions.
    pub(super) fn auto_title_slot(
        &self,
        scope: &AutoTitleSession,
    ) -> Result<(&ProfileShare, Arc<Slot>), HostError> {
        let caps = self.granted()?;
        Need::Observe.require(caps)?;
        if !self.auto_title.load(Ordering::SeqCst) || self.host.is_draining() {
            return Err(HostError::Denied("Automatic titles unavailable".into()));
        }
        let share = self.steering_share()?;
        let slot = self.host.loaded_slot(&scope.session_id).ok_or(HostError::NotFound)?;
        Ok((share, slot))
    }

    pub(super) fn auto_title_at_generation(
        &self,
        scope: &AutoTitleSession,
        state: &SlotState,
    ) -> Result<bool, HostError> {
        if state.ephemeral
            || !state.record.fresh_human_origin
            || self.identity.agent.is_some()
            || state.record.owner_agent.is_some()
            || state.record.revalidate_creator_execution(&self.identity, PermissionTier::Owner).is_err()
            || !state.attachments.iter().any(|attachment| {
                attachment.connection == self.identity.connection && !attachment.queue.is_closed()
            })
        {
            return Err(HostError::NotFound);
        }
        Ok(state.record.generation == scope.expected_generation
            && state.record.state != HostedState::Closed)
    }

    pub(super) fn auto_title_probe_sync(&self, scope: &AutoTitleSession) -> Result<bool, HostError> {
        let (share, slot) = match self.auto_title_slot(scope) {
            Ok(found) => found,
            Err(HostError::NotFound) => return Ok(false),
            Err(error) => return Err(error),
        };
        let state = slot.lock()?;
        match self.auto_title_at_generation(scope, &state) {
            Ok(true) if state.record.title.is_none() => {}
            Ok(_) | Err(HostError::NotFound) => return Ok(false),
            Err(error) => return Err(error),
        }
        let meta = meta::load(share.index.sessions_root(), &scope.session_id)?;
        self.granted()?;
        Ok(meta.is_none_or(|meta| meta.title.is_none() && meta.title_source == TitleSource::None))
    }

    pub(super) fn auto_title_seed_sync(&self, scope: &AutoTitleSession)
        -> Result<Option<AutoTitleSeed>, HostError>
    {
        let (share, slot) = self.auto_title_slot(scope)?;
        let state = slot.lock()?;
        if !self.auto_title_at_generation(scope, &state)? || state.record.title.is_some()
            || state.record.state != HostedState::Idle
        {
            return Ok(None);
        }
        let meta = meta::load(share.index.sessions_root(), &scope.session_id)?;
        if meta.is_some_and(|meta| meta.title.is_some() || meta.title_source != TitleSource::None) {
            return Ok(None);
        }
        let seed = self.with_seed_for_generation(share, scope, state.durable_head, Ok)?;
        self.granted()?;
        if self.host.is_draining() {
            return Ok(None);
        }
        Ok(seed)
    }

    fn with_seed_for_generation<T>(
        &self,
        share: &ProfileShare,
        scope: &AutoTitleSession,
        durable_head: u64,
        finish: impl FnOnce(Option<AutoTitleSeed>) -> Result<T, HostError>,
    ) -> Result<T, HostError> {
        if !self.auto_title_seed.load(Ordering::SeqCst) || scope.expected_generation != 0 {
            return finish(None);
        }
        let writer_head = || self.host.driver.archive_published_head(&scope.session_id);
        let Some(anchor) = writer_head()?.filter(|head| head.get() == durable_head) else {
            return finish(None);
        };
        let binding = self.host.driver.archive_journal_binding(&scope.session_id)?;
        let root = std::fs::canonicalize(&binding.root)?;
        if root != std::fs::canonicalize(share.index.sessions_root())? {
            return Err(HostError::Denied("Automatic title journal scope changed".into()));
        }
        harw_session_store::title_seed::with_title_seed_pairs(&root, &scope.session_id, |pairs| {
            let Some(pairs) = pairs else { return finish(None) };
            let pairs = pairs.map(|pair| AutoTitleSeedPair { user: pair.user, assistant: pair.assistant });
            let mut digest = blake3::Hasher::new();
            digest.update(b"harw/auto-title-evidence/v1\0");
            digest.update(&anchor.get().to_be_bytes());
            for pair in &pairs {
                for text in [&pair.user, &pair.assistant] {
                    if text.trim().is_empty() || text.len() > 4096 {
                        return finish(None);
                    }
                    digest.update(&(text.len() as u64).to_be_bytes());
                    digest.update(text.as_bytes());
                }
            }
            if writer_head()? != Some(anchor) {
                return finish(None);
            }
            finish(Some(AutoTitleSeed { pairs, digest: *digest.finalize().as_bytes() }))
        }).map_err(|_| HostError::Storage("Could not read automatic title evidence".into()))?
    }

    pub(super) fn auto_title_publish_sync(
        &self,
        params: &AutoTitlePublish,
    ) -> Result<AutoTitlePublication, HostError> {
        const MAX_BYTES: usize = 800;
        if params.title.is_empty() || params.title.len() > MAX_BYTES
            || params.title.trim().is_empty() || params.title.chars().any(char::is_control)
        {
            return Err(HostError::Protocol("invalid generated title".into()));
        }
        let scope = &params.scope;
        let (share, slot) = self.auto_title_slot(scope)?;
        // Hold the same slot generation through the durable title compare/set.
        // A rewind cannot change this session generation beneath this publish.
        let state = slot.lock()?;
        if !self.auto_title_at_generation(scope, &state)? {
            return Ok(AutoTitlePublication::Stale);
        }
        if state.record.title.is_some() {
            return Ok(AutoTitlePublication::AlreadyTitled);
        }
        self.granted()?;
        if self.host.is_draining() {
            return Ok(AutoTitlePublication::Stale);
        }
        let meta = meta::load(share.index.sessions_root(), &scope.session_id)?;
        if meta.is_some_and(|meta| meta.title.is_some() || meta.title_source != TitleSource::None) {
            return Ok(AutoTitlePublication::AlreadyTitled);
        }
        // Same-generation exact rewrites are possible. The idle slot pins the
        // host's writer lane; the digest also binds the observed durable cursor.
        if state.record.state != HostedState::Idle {
            return Ok(AutoTitlePublication::Stale);
        }
        // Keep the exact seed and transcript guard until the metadata CAS.
        // A rewrite cannot retire the confirmed evidence in this interval.
        self.with_seed_for_generation(share, scope, state.durable_head, |seed| {
            if seed.is_none_or(|seed| seed.digest != params.seed_digest) {
                return Ok(AutoTitlePublication::Stale);
            }
            self.granted()?;
            if self.host.is_draining() {
                return Ok(AutoTitlePublication::Stale);
            }
            if meta::set_model_title_if_untitled(share.index.sessions_root(), &scope.session_id, &params.title)?
                .is_none()
            {
                return Ok(AutoTitlePublication::AlreadyTitled);
            }
            Ok(AutoTitlePublication::Applied)
        })
    }
}
