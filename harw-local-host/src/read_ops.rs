//! Observer-only command adapters for the in-process phone host.

use std::sync::Arc;

use harw_authority::PermissionRequest;
use harw_operations::adapter::command::CommandAdapter;
use harw_operations::operation::{RemoteCap, RemoteEffect, RemoteGrant, RemoteInvocation, RemoteSlice};
use harw_operations::registry::OperationRegistry;
use harw_operations::{OpContext, ServiceMap};
use harw_ops::command_meta::{HintView, RemoteResolved, command_list_rev, remote_command_list, resolve_remote};
use harw_protocol::session_wire::{
    CatalogGetParams, CatalogResult, CommandEffect, CommandEntry, CommandHint, CommandOrigin,
    CommandRunParams, CommandRunResult, CommandsListParams, CommandsListResult, ConfigGetParams,
    ConfigGetResult, ConfigResetParams, ConfigSetParams, MAX_CLIENT_MSG_ID_BYTES, SecretPutParams,
    SecretPutResult,
    StepUpCancelParams, StepUpConfirmParams, refusal_codes,
};
use harw_runtime::RuntimeSpec;
use harw_session_host::driver::DriverFuture;
use harw_session_host::{ClientIdentity, HostError, OpsCaller, OpsPort, OpsRun, OpsSession};
use harw_types::{ApprovalActor, AuthStrength, PermissionTier, TrustZone, TurnId};

use crate::SessionCwdResolver;

// Explicit allowlist of shared operations with a faithful read-only config
// context. Live controller operations stay unadvertised until that seam exists.
const FORMS: &[(&str, &[&str])] = &[
    ("/model", &["list"]),
    ("/models", &["show"]),
    ("/models", &["list"]),
    ("/models", &["image", "show"]),
    ("/models", &["worker"]),
    ("/skills", &["list"]),
];

pub(crate) struct PhoneReadOps {
    owner: ApprovalActor,
    spec: RuntimeSpec,
    session_cwd: Option<SessionCwdResolver>,
    adapters: Vec<CommandAdapter>,
}

impl PhoneReadOps {
    pub(crate) fn new(
        owner: ApprovalActor,
        spec: RuntimeSpec,
        session_cwd: Option<SessionCwdResolver>,
    ) -> Self {
        let mut registry = OperationRegistry::new();
        harw_ops::register_all(&mut registry);
        let adapters = registry
            .iter()
            .flat_map(|operation| CommandAdapter::from_operation(Arc::clone(operation)))
            .filter(|adapter| FORMS.iter().any(|(route, _)| *route == adapter.path()))
            .collect();
        Self {
            owner,
            spec,
            session_cwd,
            adapters,
        }
    }

    fn admit(&self, caller: &OpsCaller, session: Option<OpsSession>) -> Result<OpsSession, HostError> {
        let identity = &caller.identity;
        if !caller.granted.observe
            || identity.zone != TrustZone::Local
            || identity.strength != AuthStrength::PeerCredential
            || identity.tenant.is_some()
            || identity.device.is_some()
            || identity.authenticated_node.is_some()
            || identity.agent.is_some()
            || identity.actor != self.owner
        {
            return Err(unavailable());
        }
        session.filter(|session| session.hosted).ok_or(HostError::NotFound)
    }

    fn resolve_read(&self, line: &str) -> Option<RemoteResolved<'_>> {
        let resolved = resolve_remote(&self.adapters, line, &observe_grant()).ok()?;
        let verb = resolved.verb;
        if verb.stepup
            || verb.cap != RemoteCap::Observe
            || verb.slice != RemoteSlice::Read
            || verb.effect != RemoteEffect::Applied
            || !FORMS.iter().any(|(route, args)| {
                *route == resolved.adapter.path()
                    && resolved.args.iter().map(String::as_str).eq(args.iter().copied())
            })
        {
            return None;
        }
        Some(resolved)
    }
}

fn observe_grant() -> RemoteGrant {
    RemoteGrant {
        observe: true,
        steer: false,
        control: false,
        tier: PermissionTier::Observer,
    }
}

fn unavailable() -> HostError {
    HostError::Denied("Only scoped read commands are available on this device".to_owned())
}

fn deny<'a, T: Send + 'a>() -> DriverFuture<'a, T> {
    Box::pin(async { Err(unavailable()) })
}

fn refused(code: &str, text: &str) -> OpsRun {
    OpsRun::Done(CommandRunResult {
        ok: false,
        text: text.to_owned(),
        data: None,
        effect: CommandEffect::Refused,
        reason_code: Some(code.to_owned()),
        stepup: None,
    })
}

impl OpsPort for PhoneReadOps {
    fn stepup_enabled(&self) -> bool {
        false
    }

    fn commands_list(
        &self,
        caller: OpsCaller,
        session: Option<OpsSession>,
        _params: CommandsListParams,
    ) -> DriverFuture<'_, CommandsListResult> {
        Box::pin(async move {
            self.admit(&caller, session)?;
            let mut views = remote_command_list(&self.adapters, &observe_grant());
            views.retain_mut(|view| {
                let forms = FORMS
                    .iter()
                    .filter_map(|(route, args)| {
                        if route.strip_prefix('/') != Some(view.name.as_str()) {
                            return None;
                        }
                        let args = args.join(" ");
                        let line = format!("{route} {args}");
                        self.resolve_read(&line).map(|_| (args, line))
                    })
                    .collect::<Vec<_>>();
                if forms.is_empty() || !view.remote || !view.available {
                    return false;
                }
                view.usage = forms
                    .iter()
                    .map(|(_, line)| line.as_str())
                    .collect::<Vec<_>>()
                    .join(" | ");
                // Embedded catalogue hints enumerate the exact zero-argument forms,
                // not the full desktop usages for the same family.
                view.subcommands = forms
                    .into_iter()
                    .map(|(name, _)| {
                        let summary = view.subcommands.iter()
                            .find(|hint| hint.name == name && hint.args.is_empty())
                            .map(|hint| hint.summary.clone())
                            .unwrap_or_default();
                        HintView { name, args: String::new(), summary }
                    })
                    .collect();
                view.permission = PermissionTier::Observer;
                true
            });
            let rev = format!("{:016x}", command_list_rev(&views));
            let commands = views
                .into_iter()
                .map(|view| CommandEntry {
                    name: view.name,
                    aliases: view.aliases,
                    summary: view.summary,
                    usage: view.usage,
                    domain: view.domain.as_str().to_owned(),
                    permission: view.permission,
                    busy: view.busy.to_owned(),
                    origin: CommandOrigin::Operation,
                    remote: view.remote,
                    available: view.available,
                    reason: view.reason,
                    reason_code: view.reason_code.map(str::to_owned),
                    subcommands: view
                        .subcommands
                        .into_iter()
                        .map(|hint| CommandHint {
                            name: hint.name,
                            args: hint.args,
                            summary: hint.summary,
                        })
                        .collect(),
                })
                .collect();
            Ok(CommandsListResult { commands, rev })
        })
    }

    fn command_run(
        &self,
        caller: OpsCaller,
        session: Option<OpsSession>,
        params: CommandRunParams,
    ) -> DriverFuture<'_, OpsRun> {
        Box::pin(async move {
            let session = self.admit(&caller, session)?;
            if params.client_msg_id.is_empty() || params.client_msg_id.len() > MAX_CLIENT_MSG_ID_BYTES
                || params.client_msg_id.chars().any(char::is_control)
            {
                return Err(HostError::Protocol("Invalid command request ID".to_owned()));
            }
            let Some(resolved) = self.resolve_read(&params.line) else {
                return Ok(refused(refusal_codes::NOT_AVAILABLE, "Not a supported device read command"));
            };
            let verb = resolved.verb;
            let mut spec = self.spec.clone();
            if let Some(cwd) = self.session_cwd.as_ref().and_then(|cwd| cwd(&session.session_id)) {
                spec.cwd = cwd;
            }
            let (config, _) = harw_runtime::load_config(&spec).map_err(|_| {
                HostError::Storage("Cannot load the attached session configuration".to_owned())
            })?;
            let sandbox = harw_runtime::root_sandbox(spec.entry, &spec.cwd)
                .map_err(|_| unavailable())?
                .restrict(&PermissionRequest::empty());
            let mut services = ServiceMap::new();
            services.insert(Arc::new(config));
            services.insert(RemoteInvocation { grant: observe_grant(), verb });
            // No agent controller, writer, secret port, network or executor is injected.
            // The fresh operation identity never enters chat history or a model request.
            let ctx = OpContext::new(session.session_id, TurnId::new(), sandbox, services);
            let result = match resolved.adapter.dispatch(&ctx, resolved.args).await {
                Ok(out) => OpsRun::Done(CommandRunResult {
                    ok: true,
                    text: out.text,
                    data: out.data,
                    effect: CommandEffect::Applied,
                    reason_code: None,
                    stepup: None,
                }),
                Err(_) => refused(refusal_codes::OP_FAILED, "Could not read the session configuration"),
            };
            Ok(result)
        })
    }

    fn catalog_get(&self, _: OpsCaller, _: Option<OpsSession>, _: CatalogGetParams) -> DriverFuture<'_, CatalogResult> {
        deny()
    }

    fn config_get(&self, _: OpsCaller, _: ConfigGetParams) -> DriverFuture<'_, ConfigGetResult> {
        deny()
    }

    fn config_set(&self, _: OpsCaller, _: ConfigSetParams) -> DriverFuture<'_, CommandRunResult> {
        deny()
    }

    fn config_reset(&self, _: OpsCaller, _: ConfigResetParams) -> DriverFuture<'_, CommandRunResult> {
        deny()
    }

    fn secret_put(&self, _: OpsCaller, _: SecretPutParams) -> DriverFuture<'_, SecretPutResult> {
        deny()
    }

    fn stepup_confirm(&self, _: OpsCaller, _: StepUpConfirmParams) -> DriverFuture<'_, CommandRunResult> {
        deny()
    }

    fn stepup_cancel(&self, _: OpsCaller, _: StepUpCancelParams) -> DriverFuture<'_, ()> {
        deny()
    }

    fn connection_closed(&self, _: &ClientIdentity) {}
}
