//! Scope-pinned mobile FFI for the remote Library only. No local vault or chat fallback.

use base64::{Engine, engine::general_purpose::STANDARD};
use std::sync::atomic::{AtomicBool, Ordering};
use harw_protocol::cloud_library::{
    CLOUD_LIBRARY_MAX_ASSETS, CLOUD_LIBRARY_MAX_JPEG_BYTES, CLOUD_LIBRARY_WIRE_MINOR,
    CloudLibraryCommandV1, CloudLibraryReplyV1,
};

use super::*;

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MobileCloudLibraryScope {
    pub authority_id: String,
    pub profile_epoch: u64,
}

/// Transient capability for one asset revision on one exact live gateway link.
#[derive(uniffi::Object)]
pub struct CloudLibraryCaptionAccess {
    link: Weak<Link>,
    scope: MobileCloudLibraryScope,
    asset_id: String,
    revision: u64,
    #[cfg(feature = "local-host")]
    started: AtomicBool,
}

/// Validated output; its save destination cannot be changed by the UI.
#[derive(uniffi::Object)]
pub struct CloudLibraryGeneratedCaption {
    access: Arc<CloudLibraryCaptionAccess>,
    text: Zeroizing<String>,
    save_started: AtomicBool,
}

#[uniffi::export]
impl CloudLibraryGeneratedCaption {
    pub fn text(&self) -> String {
        self.text.to_string()
    }
}

#[derive(uniffi::Enum)]
pub enum MobileCloudLibraryCommand {
    List,
    Preview { expected_revision: u64, asset_id: String },
    Import { expected_revision: u64, name: String, jpeg: Vec<u8> },
    Rename { expected_revision: u64, asset_id: String, name: String },
    Describe { expected_revision: u64, asset_id: String, description: Option<String> },
    Remove { expected_revision: u64, asset_id: String },
}

#[derive(uniffi::Record)]
pub struct MobileCloudLibraryAsset {
    pub asset_id: String,
    pub name: String,
    pub description: Option<String>,
}

#[derive(uniffi::Enum)]
pub enum MobileCloudLibraryReply {
    Snapshot { revision: u64, assets: Vec<MobileCloudLibraryAsset> },
    Preview { asset_id: String, jpeg: Vec<u8> },
    Updated { revision: u64 },
}

enum Expect {
    List,
    Preview(SessionId),
    Mutation(u64),
}

#[uniffi::export]
impl MobileCore {
    /// Exposes only a currently admitted authenticated remote gateway capability.
    pub fn cloud_library_scope(&self) -> Option<MobileCloudLibraryScope> {
        let state = self.shared.state();
        let link = state.active().ok()?;
        scope_for(&state, &link)
    }

    pub fn cloud_library_caption_access(
        &self, scope: MobileCloudLibraryScope, revision: u64, asset_id: String,
    ) -> Result<Arc<CloudLibraryCaptionAccess>, MobileError> {
        let asset_id = parse_asset(asset_id)?.as_str().to_owned();
        let state = self.shared.state();
        let link = state.active()?;
        if revision == 0 || scope_for(&state, &link).as_ref() != Some(&scope) {
            return Err(MobileError::Disconnected);
        }
        Ok(Arc::new(CloudLibraryCaptionAccess {
            link: Arc::downgrade(&link),
            scope,
            asset_id,
            revision,
            #[cfg(feature = "local-host")]
            started: AtomicBool::new(false),
        }))
    }

    /// Existing one-image device API route. No chat, caption, title, or other asset is sent.
    pub async fn cloud_library_caption_api(
        &self, access: Arc<CloudLibraryCaptionAccess>, provider: Arc<crate::ProviderStore>,
        choice: crate::ApiVisionChoice,
    ) -> Result<Arc<CloudLibraryGeneratedCaption>, MobileError> {
        #[cfg(feature = "local-host")]
        {
            let link = self.caption_link(&access)?;
            claim_once(&access.started)?;
            let image = self.caption_image(&access, &link).await?;
            self.caption_link(&access)?;
            let observation = tokio::select! {
                biased;
                () = link.stop.cancelled() => return Err(MobileError::Disconnected),
                result = provider.api_vision_observe(choice, image) => result?,
            };
            if observation.status != crate::ApiVisionStatus::Caption {
                return Err(no_complete_caption());
            }
            let text = safe_caption(observation.caption.ok_or_else(no_complete_caption)?)?;
            self.caption_link(&access)?;
            Ok(Arc::new(CloudLibraryGeneratedCaption {
                access, text, save_started: AtomicBool::new(false),
            }))
        }
        #[cfg(not(feature = "local-host"))]
        {
            let _ = (access, provider, choice);
            Err(no_complete_caption())
        }
    }

    /// List explicitly selectable installed local Library vision pairs.
    pub fn library_vision_choices(&self) -> Result<Vec<crate::LocalVisionChoice>, MobileError> {
        #[cfg(feature = "local-host")]
        {
            let link = self.shared.state().active()?;
            if link.backend.kind() != BackendKind::Local {
                return Err(MobileError::Disconnected);
            }
            let model = link.backend.hybrid_model.clone().ok_or_else(no_complete_caption)?;
            model.library_vision_choices()
        }
        #[cfg(not(feature = "local-host"))]
        Ok(Vec::new())
    }

    /// Run on an explicitly selected installed/loaded on-device model, not the gateway/chat route.
    pub async fn cloud_library_caption_local(
        &self, access: Arc<CloudLibraryCaptionAccess>, device: Arc<MobileCore>,
        choice: crate::LocalVisionChoice,
    ) -> Result<Arc<CloudLibraryGeneratedCaption>, MobileError> {
        #[cfg(feature = "local-host")]
        {
            let link = self.caption_link(&access)?;
            claim_once(&access.started)?;
            let (local_link, model) = {
                let link = device.shared.state().active()?;
                if link.backend.kind() != BackendKind::Local {
                    return Err(no_complete_caption());
                }
                let model = link.backend.hybrid_model.clone().ok_or_else(no_complete_caption)?;
                (link, model)
            };
            let image = self.caption_image(&access, &link).await?;
            self.caption_link(&access)?;
            if !device.shared.state().same_link(&local_link) {
                return Err(MobileError::Disconnected);
            }
            let sink = Arc::new(OneCaption::default());
            tokio::select! {
                biased;
                () = link.stop.cancelled() => return Err(MobileError::Disconnected),
                result = crate::local_vision::describe_library(
                    model, local_link.stop.clone(), choice,
                    vec![crate::LocalVisionImage { id: 1, image }], sink.clone(),
                ) => result?,
            }
            let text = sink.complete()?;
            if !device.shared.state().same_link(&local_link) {
                return Err(MobileError::Disconnected);
            }
            self.caption_link(&access)?;
            Ok(Arc::new(CloudLibraryGeneratedCaption {
                access, text, save_started: AtomicBool::new(false),
            }))
        }
        #[cfg(not(feature = "local-host"))]
        {
            let _ = (access, device, choice);
            Err(no_complete_caption())
        }
    }

    /// Separate deliberate save; backend CAS rejects writes after another revision.
    pub async fn cloud_library_save_caption(
        &self, caption: Arc<CloudLibraryGeneratedCaption>,
    ) -> Result<(), MobileError> {
        let access = &caption.access;
        let link = self.caption_link(access)?;
        claim_once(&caption.save_started)?;
        let reply = self.execute_cloud_library(&access.scope, link, MobileCloudLibraryCommand::Describe {
            expected_revision: access.revision,
            asset_id: access.asset_id.clone(),
            description: Some(caption.text.to_string()),
        }).await?;
        match reply {
            MobileCloudLibraryReply::Updated { .. } => Ok(()),
            _ => Err(ambiguous()),
        }
    }

    /// Explicit manual send of an attachment already resolved in the exact
    /// Cloud Library scope. This cannot rewind, resume or retarget a chat.
    pub fn submit_cloud_library_images(
        &self,
        scope: MobileCloudLibraryScope,
        session_id: String,
        text: String,
        expected_head: SessionCursor,
        client_message_id: String,
        images: Vec<PromptImage>,
    ) -> Result<SubmitReceipt, MobileError> {
        let id = parse_session_id(session_id)?;
        validate_submission(&text, &client_message_id)?;
        let images = validate_images(images)?;
        let link = {
            let state = self.shared.state();
            let link = state.active()?;
            if scope_for(&state, &link).as_ref() != Some(&scope)
                || images.is_empty()
                || !link.backend.features().iter().any(|feature| feature == features::IMAGES)
            {
                return Err(MobileError::Denied { detail: "Cloud Library conversation changed".into() });
            }
            link
        };
        let shared = Arc::clone(&self.shared);
        self.run(async move {
            submit(shared, id, text, images, expected_head.into(), client_message_id, SubmitRoute::CloudLibrary { link }).await
        })
    }

    pub async fn cloud_library(
        &self, scope: MobileCloudLibraryScope, command: MobileCloudLibraryCommand,
    ) -> Result<MobileCloudLibraryReply, MobileError> {
        let link = {
            let state = self.shared.state();
            let link = state.active()?;
            if scope_for(&state, &link).as_ref() != Some(&scope) {
                return Err(MobileError::Denied { detail: "Cloud Library connection changed".into() });
            }
            link
        };
        self.execute_cloud_library(&scope, link, command).await
    }
}

impl MobileCore {
    fn caption_link(&self, access: &CloudLibraryCaptionAccess) -> Result<Arc<Link>, MobileError> {
        let link = access.link.upgrade().ok_or(MobileError::Disconnected)?;
        let state = self.shared.state();
        if !state.same_link(&link) || scope_for(&state, &link).as_ref() != Some(&access.scope) {
            return Err(MobileError::Disconnected);
        }
        Ok(link)
    }

    #[cfg(feature = "local-host")]
    async fn caption_image(
        &self, access: &CloudLibraryCaptionAccess, link: &Arc<Link>,
    ) -> Result<PromptImage, MobileError> {
        let reply = self.execute_cloud_library(&access.scope, Arc::clone(link),
            MobileCloudLibraryCommand::Preview {
                expected_revision: access.revision, asset_id: access.asset_id.clone(),
            },
        ).await?;
        let MobileCloudLibraryReply::Preview { jpeg, .. } = reply else {
            return Err(no_complete_caption());
        };
        let jpeg = Zeroizing::new(jpeg);
        Ok(PromptImage {
            mime: "image/jpeg".into(),
            data_b64: STANDARD.encode(&jpeg[..]),
        })
    }

    async fn execute_cloud_library(
        &self, scope: &MobileCloudLibraryScope, link: Arc<Link>, command: MobileCloudLibraryCommand,
    ) -> Result<MobileCloudLibraryReply, MobileError> {
        let scope = scope.clone();
        let (input, expect) = to_wire(command)?;
        let mutation = input.mutates();
        let runtime = self.runtime.as_ref().ok_or_else(|| internal("Runtime stopped"))?;
        let shared = Arc::clone(&self.shared);
        runtime.spawn(async move {
            {
                let state = shared.state();
                if !state.same_link(&link) || scope_for(&state, &link).as_ref() != Some(&scope) {
                    return Err(MobileError::Disconnected);
                }
            }
            let reply = link.request(link.backend.port().cloud_library(input)).await;
            let current = {
                let state = shared.state();
                state.same_link(&link) && scope_for(&state, &link).as_ref() == Some(&scope)
            };
            if !current {
                return Err(if mutation { ambiguous() } else {
                    MobileError::Denied { detail: "Cloud Library connection changed".into() }
                });
            }
            let reply = match reply {
                Err(MobileError::Disconnected | MobileError::Transport { .. } | MobileError::Internal { .. })
                    if mutation => return Err(ambiguous()),
                result => result?,
            };
            match from_wire(reply, expect) {
                Err(_) if mutation => Err(ambiguous()),
                result => result,
            }
        }).await.map_err(|_| {
            if mutation { ambiguous() } else { internal("Cloud Library request stopped") }
        })?
    }
}

fn scope_for(state: &CoreState, link: &Arc<Link>) -> Option<MobileCloudLibraryScope> {
    if link.backend.kind() != BackendKind::Remote
        || link.backend.ack().wire_minor < CLOUD_LIBRARY_WIRE_MINOR
        || !link.backend.granted().observe || !link.backend.granted().steer
        || !link.backend.ack().features.iter().any(|feature| feature == features::CLOUD_LIBRARY_V1)
    {
        return None;
    }
    let profile = &state.profile.as_ref()?.profile;
    let mut hash = blake3::Hasher::new_derive_key("harw Android Cloud Library remote authority v1");
    for value in [&profile.server_node_id, &profile.server_public_key_hex, &profile.endpoint] {
        hash.update(&(value.len() as u64).to_be_bytes());
        hash.update(value.as_bytes());
    }
    Some(MobileCloudLibraryScope {
        authority_id: hash.finalize().to_hex().to_string(),
        profile_epoch: state.profile_epoch,
    })
}

fn to_wire(input: MobileCloudLibraryCommand) -> Result<(CloudLibraryCommandV1, Expect), MobileError> {
    let (command, expect) = match input {
        MobileCloudLibraryCommand::List => (CloudLibraryCommandV1::List, Expect::List),
        MobileCloudLibraryCommand::Preview { expected_revision, asset_id } => {
            let asset_id = parse_asset(asset_id)?;
            (CloudLibraryCommandV1::Preview { expected_revision, asset_id: asset_id.clone() }, Expect::Preview(asset_id))
        }
        MobileCloudLibraryCommand::Import { expected_revision, name, jpeg } => {
            let jpeg = Zeroizing::new(jpeg);
            if !(4..=CLOUD_LIBRARY_MAX_JPEG_BYTES).contains(&jpeg.len()) ||
                !jpeg.starts_with(&[0xff, 0xd8]) || !jpeg.ends_with(&[0xff, 0xd9])
            {
                return Err(session_error("Invalid Cloud Library image"));
            }
            check_name(&name)?;
            let data_b64 = STANDARD.encode(&jpeg[..]);
            (CloudLibraryCommandV1::Import { expected_revision, name, data_b64 }, Expect::Mutation(expected_revision))
        }
        MobileCloudLibraryCommand::Rename { expected_revision, asset_id, name } => {
            let asset_id = parse_asset(asset_id)?;
            check_name(&name)?;
            (CloudLibraryCommandV1::Rename { expected_revision, asset_id, name }, Expect::Mutation(expected_revision))
        }
        MobileCloudLibraryCommand::Describe { expected_revision, asset_id, description } => {
            let asset_id = parse_asset(asset_id)?;
            if description.as_deref().is_some_and(|text| !valid_description(text)) {
                return Err(session_error("Invalid Cloud Library description"));
            }
            (CloudLibraryCommandV1::Describe { expected_revision, asset_id, description }, Expect::Mutation(expected_revision))
        }
        MobileCloudLibraryCommand::Remove { expected_revision, asset_id } => {
            let asset_id = parse_asset(asset_id)?;
            (CloudLibraryCommandV1::Remove { expected_revision, asset_id }, Expect::Mutation(expected_revision))
        }
    };
    Ok((command, expect))
}

fn from_wire(reply: CloudLibraryReplyV1, expect: Expect) -> Result<MobileCloudLibraryReply, MobileError> {
    match (reply, expect) {
        (CloudLibraryReplyV1::Snapshot { revision, assets }, Expect::List)
            if assets.len() <= CLOUD_LIBRARY_MAX_ASSETS && (revision != 0 || assets.is_empty()) => {
            let mut seen = HashSet::with_capacity(assets.len());
            let mut safe = Vec::with_capacity(assets.len());
            for item in assets {
                check_name(&item.name)?;
                if item.description.as_deref().is_some_and(|text| !valid_description(text))
                    || !seen.insert(item.asset_id.clone())
                {
                    return Err(session_error("Invalid Cloud Library snapshot"));
                }
                safe.push(MobileCloudLibraryAsset {
                    asset_id: item.asset_id.as_str().to_owned(),
                    name: item.name,
                    description: item.description,
                });
            }
            Ok(MobileCloudLibraryReply::Snapshot { revision, assets: safe })
        }
        (CloudLibraryReplyV1::Preview { asset_id, data_b64 }, Expect::Preview(requested))
            if asset_id == requested && data_b64.len() <= 240_000 => {
            let data_b64 = Zeroizing::new(data_b64);
            let jpeg = Zeroizing::new(STANDARD.decode(&data_b64[..])
                .map_err(|_| session_error("Invalid Cloud Library image"))?);
            if !(4..=CLOUD_LIBRARY_MAX_JPEG_BYTES).contains(&jpeg.len())
                || !jpeg.starts_with(&[0xff, 0xd8]) || !jpeg.ends_with(&[0xff, 0xd9])
            {
                return Err(session_error("Invalid Cloud Library image"));
            }
            Ok(MobileCloudLibraryReply::Preview {
                asset_id: asset_id.as_str().to_owned(),
                jpeg: jpeg.to_vec(),
            })
        }
        (CloudLibraryReplyV1::Updated { revision }, Expect::Mutation(expected))
            if expected.checked_add(1) == Some(revision) => Ok(MobileCloudLibraryReply::Updated { revision }),
        _ => Err(session_error("Cloud Library reply does not match its request")),
    }
}

fn parse_asset(id: String) -> Result<SessionId, MobileError> {
    SessionId::try_from_str(id).map_err(|_| session_error("Invalid Cloud Library item"))
}

fn check_name(name: &str) -> Result<(), MobileError> {
    if name.trim().is_empty() || name.len() > 160 || name.chars().any(char::is_control) {
        return Err(session_error("Invalid Cloud Library name"));
    }
    Ok(())
}

fn valid_description(text: &str) -> bool {
    !text.trim().is_empty() && text.len() <= 4_096 &&
        !text.chars().any(|ch| ch.is_control() && !matches!(ch, '\n' | '\t'))
}

fn ambiguous() -> MobileError {
    MobileError::Ambiguous {
        detail: "Cloud Library save may have reached the gateway; reload this Library before another action".into(),
    }
}

fn claim_once(claimed: &AtomicBool) -> Result<(), MobileError> {
    if claimed.swap(true, Ordering::AcqRel) {
        return Err(session_error("Cloud Library confirmation has already been used"));
    }
    Ok(())
}

fn no_complete_caption() -> MobileError {
    session_error("No complete Cloud Library caption; no fallback or save was attempted")
}

#[cfg(feature = "local-host")]
fn safe_caption(text: String) -> Result<Zeroizing<String>, MobileError> {
    let text = Zeroizing::new(text);
    let text = text.trim();
    if !valid_description(text) {
        return Err(no_complete_caption());
    }
    Ok(Zeroizing::new(text.to_owned()))
}

#[cfg(feature = "local-host")]
#[derive(Default)]
struct OneCaption(Mutex<Option<Result<Zeroizing<String>, MobileError>>>);

#[cfg(feature = "local-host")]
impl OneCaption {
    fn complete(&self) -> Result<Zeroizing<String>, MobileError> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
            .take().ok_or_else(no_complete_caption)?
    }
}

#[cfg(feature = "local-host")]
impl crate::LocalVisionProgress for OneCaption {
    fn on_result(&self, id: u64, outcome: crate::LocalVisionOutcome) {
        let mut result = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        *result = Some(match (result.is_none(), id, outcome) {
            (true, 1, crate::LocalVisionOutcome::Caption { text }) => safe_caption(text),
            _ => Err(no_complete_caption()),
        });
    }
}

#[cfg(all(feature = "local-host", test))]
mod caption_tests {
    use super::*;
    use crate::{LocalVisionOutcome, LocalVisionProgress};

    #[test]
    fn only_one_complete_result_for_the_selected_image_can_be_saved() {
        let sink = OneCaption::default();
        sink.on_result(2, LocalVisionOutcome::Caption { text: "wrong image".into() });
        sink.on_result(1, LocalVisionOutcome::Caption { text: "actually selected".into() });
        assert!(sink.complete().is_err());

        let sink = OneCaption::default();
        sink.on_result(1, LocalVisionOutcome::OutputLimit { text: "partial".into() });
        assert!(sink.complete().is_err());

        let sink = OneCaption::default();
        sink.on_result(1, LocalVisionOutcome::Caption { text: "  selected\ncaption  ".into() });
        assert_eq!(sink.complete().expect("complete caption").as_str(), "selected\ncaption");
    }

    #[test]
    fn the_same_generation_or_save_confirmation_cannot_be_replayed() {
        let accepted = AtomicBool::new(false);
        assert!(claim_once(&accepted).is_ok());
        assert!(claim_once(&accepted).is_err());
    }

    #[test]
    fn over_limit_or_control_character_caption_is_not_truncated_or_saved() {
        assert!(safe_caption("é".repeat(2_049)).is_err());
        assert!(safe_caption("hidden\rcontrols".into()).is_err());
        assert!(safe_caption(" \t ".into()).is_err());
    }
}
