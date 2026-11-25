//! Manual Local Assistant → ordinary on-device session actions.
//! Not part of the model's tool registry or private-room protocol.

use std::sync::{Arc, Weak};

use harw_protocol::session_wire::{RenameParams, SetProjectParams, features};

use super::{
    BackendKind, ConnectionPhase, CoreState, Link, MobileCore, MobileError, link_supports_projects,
    parse_session_id,
};

/// Identifies the exact embedded host link. Never owns it and cannot
/// be retargeted to a remote connection or a replacement embedded host.
#[derive(uniffi::Object)]
pub struct LocalAssistantSessionAccess {
    link: Weak<Link>,
}

#[uniffi::export]
impl MobileCore {
    pub fn local_assistant_session_access(&self) -> Option<Arc<LocalAssistantSessionAccess>> {
        let state = self.shared.state();
        let link = state.active().ok()?;
        is_local(&state, &link).then(|| {
            Arc::new(LocalAssistantSessionAccess {
                link: Arc::downgrade(&link),
            })
        })
    }

    pub fn local_assistant_session_access_current(
        &self,
        access: Arc<LocalAssistantSessionAccess>,
    ) -> bool {
        access
            .link
            .upgrade()
            .is_some_and(|link| is_local(&self.shared.state(), &link))
    }

    pub fn local_assistant_can_rename_session(
        &self,
        access: Arc<LocalAssistantSessionAccess>,
    ) -> bool {
        access
            .link
            .upgrade()
            .is_some_and(|link| is_local(&self.shared.state(), &link) && can_rename(&link))
    }

    pub fn local_assistant_assign_existing_project(
        &self,
        access: Arc<LocalAssistantSessionAccess>,
        session_id: String,
        project_key: String,
    ) -> Result<(), MobileError> {
        let session_id = parse_session_id(session_id)?;
        super::validate_existing_project_key(&project_key)?;
        let link = access.link.upgrade().ok_or(MobileError::Disconnected)?;
        if !is_local(&self.shared.state(), &link) {
            return Err(MobileError::Disconnected);
        }
        if !can_rename(&link) || !link.backend.features().iter().any(|name| name == features::EXISTING_PROJECT_ASSIGNMENT_V1) {
            return Err(MobileError::Denied {
                detail: "Existing project assignment not negotiated".into(),
            });
        }
        let shared = Arc::clone(&self.shared);
        self.run(async move {
            link.request(link.backend.port().set_existing_project(SetProjectParams { session_id, project_key }))
                .await?;
            if !is_local(&shared.state(), &link) {
                return Err(MobileError::Disconnected);
            }
            Ok(())
        })
    }

    pub fn local_assistant_rename_session(
        &self,
        access: Arc<LocalAssistantSessionAccess>,
        session_id: String,
        title: String,
    ) -> Result<(), MobileError> {
        let session_id = parse_session_id(session_id)?;
        let title = title.trim();
        if title.is_empty() || title.chars().count() > 200 || title.chars().any(char::is_control) {
            return Err(MobileError::Denied {
                detail: "Invalid local session title".into(),
            });
        }
        let title = title.to_owned();
        let link = access.link.upgrade().ok_or(MobileError::Disconnected)?;
        if !is_local(&self.shared.state(), &link) {
            return Err(MobileError::Disconnected);
        }
        if !can_rename(&link) {
            return Err(MobileError::Denied {
                detail: "The embedded host does not grant session rename".into(),
            });
        }
        let shared = Arc::clone(&self.shared);
        self.run(async move {
            // Ordinary host checks the selected session and permits the
            // metadata update. Its ordinary result does not leave Rust.
            link.request(link.backend.port().rename(RenameParams { session_id, title }))
                .await?;
            if !is_local(&shared.state(), &link) {
                return Err(MobileError::Disconnected);
            }
            Ok(())
        })
    }
}

fn is_local(state: &CoreState, link: &Arc<Link>) -> bool {
    state.phase == ConnectionPhase::Connected
        && state.profile.is_none()
        && state.same_link(link)
        && link.backend.kind() == BackendKind::Local
        && link.backend.granted().observe
}

fn can_rename(link: &Link) -> bool {
    link.backend.granted().steer && link_supports_projects(link)
}
