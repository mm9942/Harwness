//! Bounded CLI access to the same session host as the Android client.
//!
//! This is not a service manager or arbitrary RPC runner. It uses the existing
//! pinned `harw attach` target, a closed set of typed requests and the host's
//! authorization decisions. An unsupported command never opens a fallback target.

use std::io::{self, Read};

use harw_protocol::cloud_library::{CloudLibraryCommandV1, CloudLibraryReplyV1};
use harw_protocol::session_wire::{
    CloudAssistantRecallSubmit, CloudAssistantSharedCommandV1, CloudAssistantSubmit,
    ManualCloudWorkCommandV1, ManualCloudWorkReplyV1, OwnerObservationStatus, PageParams,
    ProjectsParams, RenameParams, SetProjectParams, features,
};
use harw_protocol::{ClientCaps, PortError, SessionPort};
use harw_session_remote::{ConnectOptions, RemotePort};
use harw_types::SessionId;
use serde::Serialize;
use serde_json::json;

use crate::cli::{
    AttachArgs, GatewayControlAction, GatewayControlArgs, GlobalArgs, HostedAssistantAction, HostedSharedAssistantAction,
};
use crate::output::Printer;

const ASSISTANT_MESSAGE_LIMIT: usize = 8 * 1024;

pub(crate) fn run(global: &GlobalArgs, args: GatewayControlArgs) -> Result<(), String> {
    #[cfg(feature = "onion")]
    if let GatewayControlAction::FencedOwnerStatus { hub, session } = &args.action {
        let owner = args.host.as_deref().ok_or("Fenced owner status requires --host OWNER_ALIAS")?;
        if args.socket.is_some() || owner == hub.as_str() {
            return Err("Fenced owner status requires distinct owner and Hub Onion aliases".into());
        }
        return run_fenced_owner_status(global, owner, hub, session.clone());
    }
    let direct_owner_status = matches!(&args.action, GatewayControlAction::DirectOwnerStatus { .. });
    if direct_owner_status && args.host.is_none() {
        return Err("Direct owner status requires an explicit pinned --host OWNER_ALIAS".into());
    }
    let target = crate::attach_cmd::resolve_target(&AttachArgs {
        session: None,
        socket: args.socket,
        host: args.host,
    })
    .map_err(|error| escape(&error))?;
    if direct_owner_status && !matches!(&target, crate::attach_cmd::Target::Node { .. }) {
        return Err("Direct owner status requires an authenticated node alias, not a Unix socket".into());
    }
    let transport = target.transport_name();
    let prompt = match &args.action {
        GatewayControlAction::Assistant {
            action: HostedAssistantAction::Send { message_id, recall },
        } => {
            validate_id(message_id, 64)?;
            if recall.len() > 4 {
                return Err("Choose at most four Assistant turns to recall".into());
            }
            let mut unique = std::collections::BTreeSet::new();
            for id in recall {
                validate_id(id, 64)?;
                if id == message_id || !unique.insert(id) {
                    return Err("Recall IDs must be distinct completed turns, not the outgoing message ID".into());
                }
            }
            Some(read_assistant_prompt(io::stdin().lock())?)
        }
        GatewayControlAction::Assistant {
            action: HostedAssistantAction::Shared {
                action: HostedSharedAssistantAction::Send { message_id },
            },
        } => {
            validate_id(message_id, 64)?;
            Some(read_assistant_prompt(io::stdin().lock())?)
        }
        GatewayControlAction::CreateEmpty { operation_id }
        | GatewayControlAction::ResolveEmpty { operation_id } => {
            validate_id(operation_id, 128)?;
            None
        }
        _ => None,
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|err| format!("Cannot start gateway client: {err}"))?;
    let mut options = ConnectOptions::new("harw gateway control");
    if direct_owner_status {
        options.features.push(features::DIRECT_OWNER_STATUS_V1.to_owned());
        options.requested_caps = Some(ClientCaps::OBSERVE);
    } else {
        options.features = [
            features::PROJECTS,
            features::SESSION_SYSTEM_ROWS_V1,
            features::EXISTING_PROJECT_ASSIGNMENT_V1,
            features::CLOUD_ASSISTANT_V1,
            features::CLOUD_ASSISTANT_RECALL_V1,
            features::CLOUD_ASSISTANT_SHARED_V1,
            features::CLOUD_LIBRARY_V1,
            features::MANUAL_CLOUD_WORK_V1,
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
    }

    runtime.block_on(async move {
        let port = target
            .connect(global.home.clone(), options)
            .await
            .map_err(|error| escape(&error))?;
        dispatch(
            &port,
            transport,
            args.action,
            prompt,
            &Printer::new(global.output()),
        )
        .await
    })
}

#[cfg(feature = "onion")]
fn run_fenced_owner_status(
    global: &GlobalArgs,
    owner_alias: &str,
    hub_alias: &str,
    session: SessionId,
) -> Result<(), String> {
    let resolve = |alias: &str| crate::attach_cmd::resolve_target(&AttachArgs {
        session: None,
        socket: None,
        host: Some(alias.to_owned()),
    }).map_err(|_| "Fenced owner status: configured Onion alias unavailable".to_owned());
    let owner = resolve(owner_alias)?;
    let hub = resolve(hub_alias)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|_| "Cannot start fenced owner status client")?;
    let status = runtime.block_on(crate::attach_cmd::fenced_owner_status(
        owner, hub, global.home.clone(), session,
    ))?;
    print_owner_status(&Printer::new(global.output()), status)
}

fn print_owner_status(printer: &Printer, status: OwnerObservationStatus) -> Result<(), String> {
    let text = match status {
        OwnerObservationStatus::Idle => "idle",
        OwnerObservationStatus::Active => "active",
        OwnerObservationStatus::Interrupted => "interrupted",
        OwnerObservationStatus::Failed => "failed",
    };
    printer.value(text, json!({ "status": status }))
}

async fn dispatch(
    port: &RemotePort,
    transport: &str,
    action: GatewayControlAction,
    prompt: Option<String>,
    printer: &Printer,
) -> Result<(), String> {
    let hello = port.connection().hello_ack();
    let require = |feature: &str| {
        if hello.features.iter().any(|granted| granted == feature) {
            Ok(())
        } else {
            Err(format!(
                "This host did not grant `{feature}` to the selected connection"
            ))
        }
    };

    match action {
        GatewayControlAction::Inspect => {
            let caps = &hello.granted;
            let features = hello.features.join(", ");
            printer.value(
                &format!(
                    "Transport: {transport}\nSession protocol minor: {}\nObserve: {}; steer: {}; approve: {}; control: {}\nGateway read: {}; gateway admin: {}\nFeatures: {}",
                    hello.wire_minor, caps.observe, caps.steer, caps.approve, caps.control,
                    caps.gateway_read, caps.gateway_admin, escape(&features),
                ),
                json!({ "transport": transport, "wire_minor": hello.wire_minor,
                        "features": &hello.features, "granted": caps }),
            )
        }
        GatewayControlAction::Sessions {
            project,
            internal,
            page,
        } => {
            require(features::PROJECTS)?;
            if internal {
                require(features::SESSION_SYSTEM_ROWS_V1)?;
            }
            let mut result = port
                .page(PageParams {
                    project_key: project,
                    include_system: internal,
                    limit: page.limit,
                    cursor: page.cursor,
                    ..PageParams::default()
                })
                .await
                .map_err(port_error)?;
            // This listing is metadata, not history. Ignore any legacy
            // first-message preview, even when a peer includes it.
            for row in &mut result.sessions {
                row.first_message = None;
            }
            let mut text = result
                .sessions
                .iter()
                .map(|row| {
                    format!(
                        "{}\t{}\t{}",
                        row.session_id.as_str(),
                        row.updated_at,
                        escape(row.title.as_deref().unwrap_or("(untitled)"))
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            append_cursor(&mut text, result.next_cursor.as_deref());
            output(printer, &text, &result)
        }
        GatewayControlAction::Projects { page } => {
            require(features::PROJECTS)?;
            let result = port
                .projects(ProjectsParams {
                    limit: page.limit,
                    cursor: page.cursor,
                    ..ProjectsParams::default()
                })
                .await
                .map_err(port_error)?;
            let mut text = result
                .projects
                .iter()
                .map(|project| {
                    format!(
                        "{}\t{}\t{} sessions",
                        escape(&project.project_key),
                        escape(&project.label),
                        project.session_count
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            append_cursor(&mut text, result.next_cursor.as_deref());
            output(printer, &text, &result)
        }
        GatewayControlAction::DirectOwnerStatus { session } => {
            require(features::DIRECT_OWNER_STATUS_V1)?;
            let status = port.direct_owner_status(session).await.map_err(port_error)?;
            print_owner_status(printer, status)
        }
        #[cfg(feature = "onion")]
        GatewayControlAction::FencedOwnerStatus { .. } =>
            Err("Fenced owner status requires the dedicated two-link path".into()),
        GatewayControlAction::Rename { session, title } => {
            require(features::PROJECTS)?;
            let session_id = parse_session(&session)?;
            let mut row = port
                .rename(RenameParams { session_id, title })
                .await
                .map_err(port_error)?;
            row.first_message = None;
            output(
                printer,
                &format!("Renamed {}", row.session_id.as_str()),
                &row,
            )
        }
        GatewayControlAction::Move { session, project } => {
            require(features::EXISTING_PROJECT_ASSIGNMENT_V1)?;
            if project.trim().is_empty() {
                return Err("An existing project key is required".into());
            }
            let session_id = parse_session(&session)?;
            port.set_existing_project(SetProjectParams {
                session_id: session_id.clone(),
                project_key: project.clone(),
            })
            .await
            .map_err(port_error)?;
            printer.value(
                &format!("Moved {} to {}", session_id.as_str(), escape(&project)),
                json!({ "session_id": session_id, "project_key": project }),
            )
        }
        GatewayControlAction::CreateEmpty { operation_id } => {
            require(features::MANUAL_CLOUD_WORK_V1)?;
            let reply = port
                .manual_cloud_work(ManualCloudWorkCommandV1::CreateEmpty {
                    operation_id: operation_id.clone(),
                })
                .await
                .map_err(|error| uncertain(error, &operation_id, "resolve-empty"))?;
            work_reply(printer, &reply)
        }
        GatewayControlAction::ResolveEmpty { operation_id } => {
            require(features::MANUAL_CLOUD_WORK_V1)?;
            let reply = port
                .manual_cloud_work(ManualCloudWorkCommandV1::Resolve { operation_id })
                .await
                .map_err(port_error)?;
            work_reply(printer, &reply)
        }
        GatewayControlAction::Assistant { action } => {
            match action {
                HostedAssistantAction::Shared { action } => {
                    require(features::CLOUD_ASSISTANT_SHARED_V1)?;
                    return dispatch_shared_assistant(port, action, prompt, printer).await;
                }
                _ => require(features::CLOUD_ASSISTANT_V1)?,
            }
            match action {
                HostedAssistantAction::Shared { .. } => unreachable!("shared room already handled"),
                action @ (HostedAssistantAction::Status | HostedAssistantAction::Provision) => {
                    let snapshot = match action {
                        HostedAssistantAction::Provision => port.cloud_assistant_provision().await,
                        _ => port.cloud_assistant_status().await,
                    }
                    .map_err(port_error)?;
                    printer.value(
                        &format!("{:?} ({} completed turns)", snapshot.status, snapshot.turns.len()),
                        json!({ "status": snapshot.status, "completed_turns": snapshot.turns.len() }),
                    )
                }
                HostedAssistantAction::History => {
                    let snapshot = port.cloud_assistant_status().await.map_err(port_error)?;
                    let text = snapshot
                        .turns
                        .iter()
                        .map(|turn| {
                            format!(
                                "Turn {}\nYou: {}\nAssistant: {}",
                                escape(&turn.client_message_id),
                                escape(&turn.text),
                                escape(&turn.answer)
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    output(printer, &text, &snapshot)
                }
                HostedAssistantAction::Send { message_id, recall } => {
                    let text = prompt.ok_or("Assistant message missing")?;
                    let result = if recall.is_empty() {
                        port.cloud_assistant_submit(CloudAssistantSubmit {
                            client_message_id: message_id.clone(),
                            text,
                        })
                        .await
                    } else {
                        require(features::CLOUD_ASSISTANT_RECALL_V1)?;
                        port.cloud_assistant_submit_with_recall(CloudAssistantRecallSubmit {
                            client_message_id: message_id.clone(),
                            text,
                            recall_message_ids: recall,
                        })
                        .await
                    };
                    let snapshot = result
                        .map_err(|error| uncertain(error, &message_id, "assistant send"))?;
                    let turn = snapshot
                        .turns
                        .iter()
                        .find(|turn| turn.client_message_id == message_id)
                        .ok_or("The host did not confirm this Assistant message")?;
                    printer.value(
                        &escape(&turn.answer),
                        json!({ "client_message_id": message_id, "answer": &turn.answer }),
                    )
                }
            }
        }
        GatewayControlAction::Library => {
            require(features::CLOUD_LIBRARY_V1)?;
            let reply = port
                .cloud_library(CloudLibraryCommandV1::List)
                .await
                .map_err(port_error)?;
            let CloudLibraryReplyV1::Snapshot { revision, assets } = &reply else {
                return Err("The host returned a non-catalogue response for Library list".into());
            };
            let mut text = format!("Library revision {revision}");
            for asset in assets {
                text.push_str(&format!(
                    "\n{}\t{}",
                    asset.asset_id.as_str(),
                    escape(&asset.name)
                ));
            }
            output(printer, &text, &reply)
        }
    }
}

async fn dispatch_shared_assistant(
    port: &RemotePort,
    action: HostedSharedAssistantAction,
    prompt: Option<String>,
    printer: &Printer,
) -> Result<(), String> {
    use harw_protocol::session_wire::CloudAssistantSharedStatusV1 as Status;
    use CloudAssistantSharedCommandV1 as Command;
    use HostedSharedAssistantAction as Action;
    let show_history = matches!(action, Action::History);
    let mut sent = None;
    let command = match action {
        Action::Status | Action::History => Command::Status,
        Action::Provision => Command::Provision,
        Action::RequestJoin => Command::RequestJoin,
        Action::Approve { request_id, fingerprint } => {
            if !valid_exact_hex(&request_id, 32) || !valid_exact_hex(&fingerprint, 64) {
                return Err("Use the full request ID and lowercase key fingerprint shown in shared status".into());
            }
            let current = port.cloud_assistant_shared(Command::Status).await.map_err(port_error)?;
            if current.status != Status::Owner || !current.devices.iter().any(|device| {
                device.pending && device.request_id == request_id && device.key_fingerprint_hex == fingerprint
            }) {
                return Err("The exact pending device and key could not be confirmed".into());
            }
            Command::Approve { request_id }
        }
        Action::Remove { request_id } => {
            if !valid_exact_hex(&request_id, 32) {
                return Err("Use the full request ID shown in shared status".into());
            }
            Command::WithdrawOrRemove { request_id }
        }
        Action::Revoke { confirm: true } => Command::RevokeRoom,
        Action::Revoke { confirm: false } => {
            return Err("Use --confirm to revoke this shared room".into());
        }
        Action::Send { message_id } => {
            let text = prompt.ok_or("Assistant message missing")?;
            sent = Some(message_id.clone());
            Command::Submit { client_message_id: message_id, text }
        }
    };
    let result = port.cloud_assistant_shared(command).await;
    let snapshot = match &sent {
        Some(id) => result.map_err(|error| uncertain(error, id, "assistant shared send"))?,
        None => result.map_err(port_error)?,
    };
    if let Some(id) = sent {
        let turn = snapshot.turns.iter().find(|turn| turn.client_message_id == id)
            .ok_or("The host did not confirm this message in the shared room")?;
        return printer.value(&escape(&turn.answer), json!({ "client_message_id": id, "answer": &turn.answer }));
    }
    if show_history {
        if !matches!(snapshot.status, Status::Owner | Status::Participant) {
            return Err("This device cannot read the shared conversation".into());
        }
        let text = snapshot.turns.iter().map(|turn| format!(
            "Turn {}\nYou: {}\nAssistant: {}", escape(&turn.client_message_id), escape(&turn.text), escape(&turn.answer),
        )).collect::<Vec<_>>().join("\n");
        return output(printer, &text, &snapshot.turns);
    }
    let status = match snapshot.status {
        Status::Unprovisioned => "unprovisioned",
        Status::NotGranted => "not granted",
        Status::Pending => "pending",
        Status::Owner => "owner",
        Status::Participant => "participant",
        Status::Revoked => "revoked",
    };
    let mut text = format!("{status} ({} completed turns)", snapshot.turns.len());
    if let Some(request) = &snapshot.own_request_id {
        text.push_str(&format!("\nYour request: {}", escape(request)));
    }
    for device in &snapshot.devices {
        text.push_str(&format!("\n{}: {} (node {}, key {}, {})",
            escape(&device.request_id), escape(&device.device_id), escape(&device.node_id),
            escape(&device.key_fingerprint_hex), if device.pending { "pending" } else { "admitted" }));
    }
    printer.value(&text, json!({
        "status": snapshot.status, "completed_turns": snapshot.turns.len(),
        "own_request_id": snapshot.own_request_id, "devices": snapshot.devices,
    }))
}

fn valid_exact_hex(value: &str, len: usize) -> bool {
    value.len() == len && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        && value.bytes().any(|byte| byte != b'0')
}

fn work_reply(printer: &Printer, reply: &ManualCloudWorkReplyV1) -> Result<(), String> {
    let text = match reply {
        ManualCloudWorkReplyV1::Created { session } => {
            format!("Session: {}", session.session_id.as_str())
        }
        ManualCloudWorkReplyV1::Missing => {
            "No receipt in this exact scope. No session created by this lookup.".into()
        }
        ManualCloudWorkReplyV1::OutcomeUnknown => {
            "Outcome unknown. No replacement was created.".into()
        }
    };
    output(printer, &text, reply)
}

fn append_cursor(text: &mut String, cursor: Option<&str>) {
    if let Some(cursor) = cursor {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str("Next cursor: ");
        text.push_str(&escape(cursor));
    }
}

fn parse_session(value: &str) -> Result<SessionId, String> {
    SessionId::try_from_str(value).map_err(|_| "An exact valid session ID is required".into())
}

fn validate_id(value: &str, max: usize) -> Result<(), String> {
    if !(1..=max).contains(&value.len())
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err(format!(
            "ID must contain 1–{max} ASCII letters, digits, hyphens or underscores"
        ));
    }
    Ok(())
}

fn read_assistant_prompt(input: impl Read) -> Result<String, String> {
    let mut bytes = Vec::new();
    input
        .take((ASSISTANT_MESSAGE_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Could not read Assistant message from stdin: {error}"))?;
    if bytes.len() > ASSISTANT_MESSAGE_LIMIT {
        return Err("Assistant message exceeds 8 KiB".into());
    }
    let text = String::from_utf8(bytes).map_err(|_| "Assistant message must be UTF-8")?;
    if text.trim().is_empty()
        || text
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err("Assistant message must be nonempty text without control characters".into());
    }
    Ok(text)
}

fn escape(text: &str) -> String {
    text.chars().flat_map(char::escape_debug).collect()
}

fn port_error(error: PortError) -> String {
    format!(
        "Gateway error {}: {}",
        error.code(),
        escape(&error.to_string())
    )
}

fn uncertain(error: PortError, id: &str, operation: &str) -> String {
    if matches!(&error, PortError::Transport(_)) {
        format!(
            "{}; delivery is unconfirmed. Use `{operation}` with the same ID `{id}` and target; do not choose a new ID.",
            port_error(error),
        )
    } else {
        port_error(error)
    }
}

fn output(printer: &Printer, text: &str, value: &impl Serialize) -> Result<(), String> {
    printer.value(
        text,
        serde_json::to_value(value).map_err(|err| err.to_string())?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assistant_prompt_is_bounded_and_preserves_idempotence_bytes() {
        assert_eq!(
            read_assistant_prompt(&b"line 1\nline 2\n"[..]),
            Ok("line 1\nline 2\n".into())
        );
        assert!(read_assistant_prompt(&vec![b'a'; ASSISTANT_MESSAGE_LIMIT + 1][..]).is_err());
        assert!(read_assistant_prompt(&b"a\x1b[2J"[..]).is_err());
        assert!(read_assistant_prompt(&[0xff][..]).is_err());
    }

    #[test]
    fn uncertain_mutations_require_the_same_operation_identity() {
        let message = uncertain(
            PortError::Transport("closed".into()),
            "operation-5",
            "resolve-empty",
        );
        assert!(message.contains("resolve-empty"));
        assert!(message.contains("operation-5"));
        assert!(
            !uncertain(PortError::Revoked, "operation-5", "resolve-empty")
                .contains("resolve-empty")
        );
    }
}
