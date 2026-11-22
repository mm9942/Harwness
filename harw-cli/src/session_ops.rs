//! Gateway glue for the minor-4 `manage`/`stepup` methods: runs allow-listed
//! operations for hosted sessions, builds redacted catalog/config views, stages
//! step-ups and write-only secrets. Never executes a line that
//! `harw_ops::command_meta::resolve_remote` refused.
//!
//! Contract K-W3c section 8. The host (`harw-session-host`, `HostConnection`)
//! admits every call first (hello, feature, caps, tenant, session) and hands an
//! [`OpsCaller`] built from the authenticated connection; nothing here reads
//! rights, tenants or actors from request params.
//!
//! - Slice 1 (read): `commands.list`, `catalog.get`, `config.get` and read
//!   forms of `command.run` dispatch the operation directly.
//! - Slice 2 (session): `/stop`, `/cancel`, `/mode`, `/effort`, `/model switch`
//!   return an [`OpsRun::Session`] action; the host runs it over its typed
//!   session ways.
//! - Slice 3 (global writes): plan, park behind a step-up ([`StepUpRegistry`]),
//!   write only after `stepup.confirm`. Gated by
//!   `harw_ops::command_meta::REMOTE_ENABLED_SLICE` (see [`HostedOps::stepup_enabled`]).
//!
//! Secrets: `secret.put` values go straight into a `SecretString` and into
//! [`StagedSecrets`]; they are never logged, never echoed in errors and never
//! returned. Logs carry no params, lines or values.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use harw_authority::SandboxSpec;
use harw_operations::adapter::CommandAdapter;
use harw_operations::context::ServiceMap;
use harw_operations::operation::{
    RemoteCap, RemoteEffect, RemoteGrant, RemoteInvocation, RemoteRefusal, RemoteSlice,
    RemoteVerb,
};
use harw_operations::registry::OperationRegistry;
use harw_operations::{OpContext, OpError, OpOutput};
use harw_ops::command_meta::{RemoteResolved, resolve_remote};
use harw_ops::provider::{
    ProviderPlan, ProviderStepUpPort, RemoteManage, StagedKeyInfo, StagedKeyShape,
    StagedSecretPort, StagedStepUp,
};
use harw_ops::settings::{ConfigChange, RemoteConfigCeiling};
use harw_protocol::session_wire::{
    CatalogGetParams, CatalogResult, CommandEffect, CommandEntry, CommandHint, CommandOrigin,
    CommandRunParams, CommandRunResult, CommandsListParams, CommandsListResult, ConfigEntry,
    ConfigGetParams, ConfigGetResult, ConfigResetParams, ConfigSetParams, MAX_CLIENT_MSG_ID_BYTES,
    MAX_COMMAND_LINE_BYTES, MAX_CONFIG_KEYS, MAX_SECRET_VALUE_BYTES, SecretPutParams,
    SecretPutResult, StepUpCancelParams, StepUpConfirmParams, refusal_codes,
};
use harw_secrets::write_only::{
    KeyShape, REMOTE_PROVIDER_PURPOSE, SecretOwner, SecretPutRequest, StagedSecrets,
    WriteOnlySecretSink as _,
};
use harw_session_host::driver::DriverFuture;
use harw_session_host::ops::{OpsCaller, OpsPort, OpsRun, OpsSession, SessionAction};
use harw_session_host::stepup::{StepUpRegistry, StepUpRequest, TracingStepUpAudit};
use harw_session_host::{ClientIdentity, ConnectionId, HostError};
use harw_types::{PermissionTier, SessionId, TurnId};
use jiff::Timestamp;

/// Cached results per connection for `client_msg_id` (FIFO, oldest drops out).
pub(crate) const RUN_DEDUP_PER_CONNECTION: usize = 64;

/// Text of a session action the host runs.
const SESSION_ACCEPTED: &str = "übernommen";

/// A parked global write behind a step-up.
pub(crate) enum StagedOp {
    /// `config.set|reset`, `/settings set|reset`, `/mode default <m>`
    /// (phase 1 of `harw_ops::settings::plan_config_change`).
    Config(ConfigChange),
    /// Global verb of an allow-listed line other than `/provider` and the config
    /// keys (`/models set uia p/m`, ...). After confirm: recompute the canonical
    /// text; equal -> `adapter.dispatch`, otherwise `stale`.
    Dispatch {
        path: String,
        args: Vec<String>,
        canonical: String,
        verb: &'static RemoteVerb,
    },
    /// `/provider add|add-custom|remove|enable|disable` (plan from
    /// `harw_ops::provider::plan_change`, parked by the op over [`RemoteManage`]).
    Provider(ProviderPlan),
}

/// Operations backend of a remote gateway (installed by `session_serve_remote`).
pub(crate) struct HostedOps {
    home: PathBuf,
    bound: Arc<harw_home::ResolvedHomeContext>,
    root_sandbox: SandboxSpec,
    adapters: Arc<Vec<CommandAdapter>>,
    stepups: Arc<StepUpRegistry<StagedOp>>,
    secrets: Arc<StagedSecrets>,
    /// Key: `ConnectionId.0`; values: (`"<method>:<client_msg_id>"`, result).
    dedup: Mutex<HashMap<u64, VecDeque<(String, CommandRunResult)>>>,
}

impl HostedOps {
    /// Binds `home` (active profile, home as project root), the web root sandbox
    /// and the command adapters of every built-in operation
    /// (`OperationRegistry::new()` + `harw_ops::register_all`).
    ///
    /// # Errors
    /// Text without secrets when the home cannot be bound or the sandbox root is
    /// not usable.
    pub(crate) fn open(home: PathBuf) -> Result<Self, String> {
        let profile = harw_home::active_profile_name(&home);
        let project = harw_home::ProjectRoot {
            root: home.clone(),
            trust_key: home.clone(),
            kind: harw_home::ProjectKind::Directory,
        };
        let bound = harw_home::ResolvedHomeContext::new(&home, profile, project)
            .map_err(|error| format!("remote ops: home could not be bound: {error}"))?;
        let root_sandbox =
            harw_runtime::root_sandbox(harw_runtime::EntryKind::Web, &bound.project.root)
                .map_err(|error| format!("remote ops: sandbox root: {error}"))?;
        let mut registry = OperationRegistry::new();
        harw_ops::register_all(&mut registry);
        let adapters: Vec<CommandAdapter> = registry
            .iter()
            .flat_map(|operation| CommandAdapter::from_operation(Arc::clone(operation)))
            .collect();
        Ok(Self {
            home,
            bound: Arc::new(bound),
            root_sandbox,
            adapters: Arc::new(adapters),
            stepups: Arc::new(StepUpRegistry::new(Arc::new(TracingStepUpAudit))),
            secrets: Arc::new(StagedSecrets::new()),
            dedup: Mutex::new(HashMap::new()),
        })
    }

    /// Remote operation context (pattern `web_op_context`): fresh turn, the
    /// call's session or a fresh one, sandbox narrowed to the caller's tier,
    /// bound home, the profile tenant, and the given remote markers.
    fn op_context(
        &self,
        caller: &OpsCaller,
        session: Option<&OpsSession>,
        invocation: Option<RemoteInvocation>,
        manage: Option<Arc<RemoteManage>>,
    ) -> OpContext {
        let mut services = ServiceMap::new();
        services.insert(Arc::clone(&self.bound));
        if let Some(invocation) = invocation {
            services.insert(invocation);
        }
        if let Some(manage) = manage {
            services.insert(manage);
        }
        let session_id = session.map_or_else(SessionId::new, |s| s.session_id.clone());
        let ctx = OpContext::new(
            session_id,
            TurnId::new(),
            crate::runtime_web::narrow_web_sandbox(&self.root_sandbox, caller.tier),
            services,
        );
        match &caller.profile_tenant {
            Some(tenant) => ctx.with_tenant(tenant.clone()),
            None => ctx,
        }
    }

    /// Profile config of the bound home (home layer, then profile layer).
    fn load_config(&self) -> Result<harw_config::ResolvedConfig, HostError> {
        harw_config::discover_config(&[self.bound.home.clone(), self.bound.profile_dir.clone()])
            .map_err(|_| HostError::Storage("Konfiguration nicht lesbar".to_owned()))
    }

    /// Write-only key port bound to the caller's connection.
    fn key_port(&self, identity: &ClientIdentity) -> StagedKeyPort {
        StagedKeyPort {
            secrets: Arc::clone(&self.secrets),
            owner: secret_owner(identity),
            home: self.home.clone(),
            layers: [self.bound.home.clone(), self.bound.profile_dir.clone()],
        }
    }

    fn dedup_get(&self, connection: ConnectionId, key: &str) -> Option<CommandRunResult> {
        let map = self.dedup.lock().ok()?;
        map.get(&connection.0)?
            .iter()
            .find(|(cached, _)| cached == key)
            .map(|(_, result)| result.clone())
    }

    fn dedup_put(&self, connection: ConnectionId, key: String, result: CommandRunResult) {
        let Ok(mut map) = self.dedup.lock() else {
            return;
        };
        let queue = map.entry(connection.0).or_default();
        while queue.len() >= RUN_DEDUP_PER_CONNECTION {
            queue.pop_front();
        }
        queue.push_back((key, result));
    }

    /// Resolves and runs one line (contract 8.4 b2-b5). Never writes for a
    /// step-up verb; never runs a line `resolve_remote` refused.
    async fn run_line(
        &self,
        caller: &OpsCaller,
        session: Option<&OpsSession>,
        line: &str,
        now: Timestamp,
    ) -> OpsRun {
        let grant = grant_for(caller);
        let RemoteResolved {
            adapter,
            args,
            verb,
        } = match resolve_remote(&self.adapters, line, &grant) {
            Ok(resolved) => resolved,
            Err(refusal) => return OpsRun::Done(refused(&refusal)),
        };
        let invocation = RemoteInvocation { grant, verb };
        if verb.stepup {
            return OpsRun::Done(
                self.stage_global(caller, session, adapter, args, invocation, now)
                    .await,
            );
        }
        if let Some(action) = session_action(adapter.path(), verb, &args) {
            let action = match action {
                Ok(action) => action,
                Err(text) => return OpsRun::Done(refused_code(refusal_codes::OP_FAILED, text)),
            };
            if session.is_none() {
                return OpsRun::Done(refused_code(
                    refusal_codes::SESSION_REQUIRED,
                    "braucht eine Session",
                ));
            }
            return OpsRun::Session {
                action,
                result: CommandRunResult {
                    ok: true,
                    text: SESSION_ACCEPTED.to_owned(),
                    data: None,
                    effect: wire_effect(verb.effect),
                    reason_code: None,
                    stepup: None,
                },
            };
        }
        let ctx = self.op_context(caller, session, Some(invocation), None);
        OpsRun::Done(dispatch_result(
            adapter.dispatch(&ctx, args).await,
            verb.effect,
        ))
    }

    /// Step-up verbs (contract 8.4 b3): plan, park, answer the challenge.
    async fn stage_global(
        &self,
        caller: &OpsCaller,
        session: Option<&OpsSession>,
        adapter: &CommandAdapter,
        args: Vec<String>,
        invocation: RemoteInvocation,
        now: Timestamp,
    ) -> CommandRunResult {
        let verb = invocation.verb;
        match adapter.path() {
            "/provider" => {
                let port = Arc::new(ProviderStepUpAdapter {
                    stepups: Arc::clone(&self.stepups),
                    identity: caller.identity.clone(),
                    issued: Mutex::new(None),
                });
                let manage = Arc::new(RemoteManage {
                    stepup: Arc::clone(&port) as Arc<dyn ProviderStepUpPort>,
                    secrets: Arc::new(self.key_port(&caller.identity)),
                    resolver: Arc::new(harw_provider_http::url_guard::SystemResolver),
                });
                let ctx = self.op_context(caller, session, Some(invocation), Some(manage));
                match adapter.dispatch(&ctx, args).await {
                    Ok(out) => match port.take_issued() {
                        Some(challenge) => stepup_result(out.text, out.data, challenge),
                        // Fail closed: a remote provider write must have parked.
                        None => refused_code(refusal_codes::OP_FAILED, "Step-up fehlt"),
                    },
                    Err(error) => op_refusal(error),
                }
            }
            "/models" => {
                let ctx = self.op_context(caller, session, Some(invocation), None);
                match harw_ops::models::plan_models_change(&ctx, &args) {
                    Ok(change) => {
                        let target = args.get(1).cloned().unwrap_or_else(|| "models".to_owned());
                        self.request_stepup(
                            caller,
                            format!("models.{}", change.action),
                            target,
                            change.canonical.clone(),
                            StagedOp::Dispatch {
                                path: adapter.path().to_owned(),
                                args,
                                canonical: change.canonical,
                                verb,
                            },
                            now,
                        )
                    }
                    Err(error) => op_refusal(error),
                }
            }
            "/settings" => match (args.first().map(String::as_str), args.len()) {
                (Some("set"), 3) => {
                    self.stage_config(caller, &args[1], Some(args[2].as_str()), now)
                }
                (Some("reset"), 2) => self.stage_config(caller, &args[1], None, now),
                _ => refused_code(
                    refusal_codes::OP_FAILED,
                    "Aufruf: /settings set <schluessel> <wert> | /settings reset <schluessel>",
                ),
            },
            "/mode" => match args.as_slice() {
                [default, mode] if default == "default" => {
                    self.stage_config(caller, "mode.default", Some(mode.as_str()), now)
                }
                _ => refused_code(refusal_codes::OP_FAILED, "Aufruf: /mode default <modus>"),
            },
            _ => refused_code(
                refusal_codes::OP_FAILED,
                "kein Step-up-Weg fuer diesen Befehl",
            ),
        }
    }

    /// Plans a config change and parks it behind a step-up. Not gated by the
    /// slice; the public entry points check [`Self::stepup_enabled`] first.
    fn stage_config(
        &self,
        caller: &OpsCaller,
        key: &str,
        value: Option<&str>,
        now: Timestamp,
    ) -> CommandRunResult {
        let ctx = self.op_context(caller, None, None, None);
        match harw_ops::settings::plan_config_change(&ctx, key, value, &ceiling_for(caller.tier))
        {
            Ok(change) => {
                let action = if value.is_some() {
                    "config.set"
                } else {
                    "config.reset"
                };
                self.request_stepup(
                    caller,
                    action.to_owned(),
                    change.key.to_owned(),
                    change.canonical.clone(),
                    StagedOp::Config(change),
                    now,
                )
            }
            Err(error) => op_refusal(error),
        }
    }

    fn request_stepup(
        &self,
        caller: &OpsCaller,
        action: String,
        target: String,
        text: String,
        payload: StagedOp,
        now: Timestamp,
    ) -> CommandRunResult {
        let request = StepUpRequest {
            action,
            target,
            text: text.clone(),
            payload,
        };
        match self.stepups.request(&caller.identity, request, now) {
            Ok(challenge) => stepup_result(text, None, challenge),
            Err(error) => refused_code(refusal_codes::OP_FAILED, error.to_string()),
        }
    }

    /// `config.set` / `config.reset` (contract 8.4 e).
    fn config_change_at(
        &self,
        caller: &OpsCaller,
        key: &str,
        value: Option<&str>,
        client_msg_id: &str,
        now: Timestamp,
    ) -> Result<CommandRunResult, HostError> {
        if !self.stepup_enabled() {
            return Ok(stepup_disabled());
        }
        check_client_msg_id(client_msg_id)?;
        let method = if value.is_some() { "set" } else { "reset" };
        let dedup_key = format!("{method}:{client_msg_id}");
        let connection = caller.identity.connection;
        if let Some(hit) = self.dedup_get(connection, &dedup_key) {
            return Ok(hit);
        }
        let result = self.stage_config(caller, key, value, now);
        self.dedup_put(connection, dedup_key, result.clone());
        Ok(result)
    }

    /// `secret.put` without the slice gate (contract 8.4 f).
    fn stage_secret(
        &self,
        caller: &OpsCaller,
        params: SecretPutParams,
        now: Timestamp,
    ) -> Result<SecretPutResult, HostError> {
        if params.value.len() > MAX_SECRET_VALUE_BYTES {
            return Err(HostError::Protocol("secret too long".to_owned()));
        }
        let SecretPutParams {
            name,
            for_provider,
            value,
        } = params;
        let request = SecretPutRequest {
            key_name: name,
            for_provider,
            value: secrecy_08::SecretString::new(value.expose().to_owned()),
        };
        drop(value);
        let (handle, info) = self
            .secrets
            .stage(secret_owner(&caller.identity), request, now)
            .map_err(|error| HostError::Protocol(error.to_string()))?;
        Ok(SecretPutResult {
            handle: handle.as_str().to_owned(),
            key_name: info.key_name,
            for_provider: info.for_provider,
            expires_at: info.expires_at,
        })
    }

    /// `stepup.confirm` (contract 8.4 g). The registry checks owner, expiry,
    /// hash and caps; the payload runs only after that.
    async fn confirm_at(
        &self,
        caller: &OpsCaller,
        params: &StepUpConfirmParams,
        now: Timestamp,
    ) -> Result<CommandRunResult, HostError> {
        let staged = self
            .stepups
            .confirm(&caller.identity, &params.stepup_id, &params.hash, now)?;
        Ok(match staged {
            StagedOp::Config(change) => {
                let ctx = self.op_context(caller, None, None, None);
                let effect = if change.key == "mode.default" {
                    RemoteEffect::NextSession
                } else {
                    RemoteEffect::Applied
                };
                match harw_ops::settings::apply_config_change(&ctx, &change) {
                    Ok(out) => applied(out, effect),
                    Err(OpError::Execution(text)) if text == harw_ops::settings::STALE_CHANGE => {
                        refused_code(refusal_codes::STALE, text)
                    }
                    Err(error) => op_refusal(error),
                }
            }
            StagedOp::Dispatch {
                path,
                args,
                canonical,
                verb,
            } => {
                let Some(adapter) = self.adapters.iter().find(|a| a.path() == path) else {
                    return Ok(refused_code(
                        refusal_codes::NOT_AVAILABLE,
                        "Befehl auf diesem Gateway nicht verfuegbar",
                    ));
                };
                let invocation = RemoteInvocation {
                    grant: grant_for(caller),
                    verb,
                };
                let ctx = self.op_context(caller, None, Some(invocation), None);
                match harw_ops::models::plan_models_change(&ctx, &args) {
                    Ok(change) if change.canonical == canonical => {
                        dispatch_result(adapter.dispatch(&ctx, args).await, verb.effect)
                    }
                    Ok(_) => refused_code(
                        refusal_codes::STALE,
                        "Wert hat sich seit der Bestaetigung geaendert; neu anfragen",
                    ),
                    Err(error) => op_refusal(error),
                }
            }
            StagedOp::Provider(plan) => {
                let resolver = harw_provider_http::url_guard::SystemResolver;
                if let Err(error) = harw_ops::provider::guard_plan(&plan, &resolver).await {
                    return Ok(op_refusal(error));
                }
                let keys = self.key_port(&caller.identity);
                match harw_ops::provider::apply_plan(
                    &self.bound.profile_dir,
                    &plan,
                    Some(&keys as &dyn StagedSecretPort),
                    None,
                ) {
                    Ok(done) => CommandRunResult {
                        ok: true,
                        text: done.note.clone(),
                        data: serde_json::to_value(&done).ok(),
                        effect: CommandEffect::NextSession,
                        reason_code: None,
                        stepup: None,
                    },
                    Err(error) => op_refusal(error),
                }
            }
        })
    }
}

impl OpsPort for HostedOps {
    fn stepup_enabled(&self) -> bool {
        harw_ops::command_meta::REMOTE_ENABLED_SLICE >= RemoteSlice::Global
    }

    fn commands_list(
        &self,
        caller: OpsCaller,
        _session: Option<OpsSession>,
        _params: CommandsListParams,
    ) -> DriverFuture<'_, CommandsListResult> {
        Box::pin(async move {
            let views =
                harw_ops::command_meta::remote_command_list(&self.adapters, &grant_for(&caller));
            Ok(CommandsListResult {
                commands: views.iter().map(command_entry).collect(),
                rev: format!("{:016x}", harw_ops::command_meta::command_list_rev(&views)),
            })
        })
    }

    fn command_run(
        &self,
        caller: OpsCaller,
        session: Option<OpsSession>,
        params: CommandRunParams,
    ) -> DriverFuture<'_, OpsRun> {
        Box::pin(async move {
            check_client_msg_id(&params.client_msg_id)?;
            if params.line.len() > MAX_COMMAND_LINE_BYTES {
                return Ok(OpsRun::Done(refused(&RemoteRefusal::LineInvalid)));
            }
            let connection = caller.identity.connection;
            let dedup_key = format!("run:{}", params.client_msg_id);
            if let Some(hit) = self.dedup_get(connection, &dedup_key) {
                return Ok(OpsRun::Done(hit));
            }
            let run = self
                .run_line(&caller, session.as_ref(), &params.line, Timestamp::now())
                .await;
            // Session actions are not cached: the host may still fail them.
            if let OpsRun::Done(result) = &run {
                self.dedup_put(connection, dedup_key, result.clone());
            }
            Ok(run)
        })
    }

    fn catalog_get(
        &self,
        caller: OpsCaller,
        session: Option<OpsSession>,
        _params: CatalogGetParams,
    ) -> DriverFuture<'_, CatalogResult> {
        Box::pin(async move {
            let ctx = self.op_context(&caller, session.as_ref(), None, None);
            let mut view = harw_ops::models::catalog_view_for(&ctx).map_err(op_host_error)?;
            view.live.model = session.and_then(|s| s.model);
            catalog_result(&view)
        })
    }

    fn config_get(
        &self,
        _caller: OpsCaller,
        params: ConfigGetParams,
    ) -> DriverFuture<'_, ConfigGetResult> {
        Box::pin(async move {
            if params
                .keys
                .as_ref()
                .is_some_and(|keys| keys.len() > MAX_CONFIG_KEYS)
            {
                return Err(HostError::Protocol("too many config keys".to_owned()));
            }
            let config = self.load_config()?;
            let rows = harw_ops::settings::config_view(&config, params.keys.as_deref())
                .map_err(op_host_error)?;
            Ok(ConfigGetResult {
                entries: rows.iter().map(config_entry).collect(),
            })
        })
    }

    fn config_set(
        &self,
        caller: OpsCaller,
        params: ConfigSetParams,
    ) -> DriverFuture<'_, CommandRunResult> {
        Box::pin(async move {
            self.config_change_at(
                &caller,
                &params.key,
                Some(params.value.as_str()),
                &params.client_msg_id,
                Timestamp::now(),
            )
        })
    }

    fn config_reset(
        &self,
        caller: OpsCaller,
        params: ConfigResetParams,
    ) -> DriverFuture<'_, CommandRunResult> {
        Box::pin(async move {
            self.config_change_at(
                &caller,
                &params.key,
                None,
                &params.client_msg_id,
                Timestamp::now(),
            )
        })
    }

    fn secret_put(
        &self,
        caller: OpsCaller,
        params: SecretPutParams,
    ) -> DriverFuture<'_, SecretPutResult> {
        Box::pin(async move {
            if !self.stepup_enabled() {
                return Err(HostError::Denied(refusal_codes::STEPUP_DISABLED.to_owned()));
            }
            self.stage_secret(&caller, params, Timestamp::now())
        })
    }

    fn stepup_confirm(
        &self,
        caller: OpsCaller,
        params: StepUpConfirmParams,
    ) -> DriverFuture<'_, CommandRunResult> {
        Box::pin(async move { self.confirm_at(&caller, &params, Timestamp::now()).await })
    }

    fn stepup_cancel(
        &self,
        caller: OpsCaller,
        params: StepUpCancelParams,
    ) -> DriverFuture<'_, ()> {
        Box::pin(async move {
            self.stepups
                .cancel(&caller.identity, &params.stepup_id, Timestamp::now())
                .map_err(HostError::from)
        })
    }

    fn connection_closed(&self, identity: &ClientIdentity) {
        let _ = self
            .stepups
            .drop_connection(identity.connection, Timestamp::now());
        let _ = self.secrets.discard_owner(&secret_owner(identity));
        if let Ok(mut map) = self.dedup.lock() {
            map.remove(&identity.connection.0);
        }
    }
}

// ── Ports for `RemoteManage` (per call, bound to one connection) ────────────

/// Parks a provider plan in the step-up registry and keeps the challenge so
/// `command.run` can answer it (the op only reports id and expiry).
struct ProviderStepUpAdapter {
    stepups: Arc<StepUpRegistry<StagedOp>>,
    identity: ClientIdentity,
    issued: Mutex<Option<harw_session_host::stepup::StepUpChallenge>>,
}

impl ProviderStepUpAdapter {
    fn take_issued(&self) -> Option<harw_session_host::stepup::StepUpChallenge> {
        self.issued.lock().ok().and_then(|mut slot| slot.take())
    }
}

impl ProviderStepUpPort for ProviderStepUpAdapter {
    fn stage(&self, plan: ProviderPlan) -> Result<StagedStepUp, String> {
        let request = StepUpRequest {
            action: plan.action.to_owned(),
            target: plan.name.clone(),
            text: plan.canonical_text.clone(),
            payload: StagedOp::Provider(plan),
        };
        let challenge = self
            .stepups
            .request(&self.identity, request, Timestamp::now())
            .map_err(|error| error.to_string())?;
        let staged = StagedStepUp {
            stepup_id: challenge.stepup_id.clone(),
            expires_at: challenge.expires_at.to_string(),
        };
        let mut slot = self
            .issued
            .lock()
            .map_err(|_| "Step-up-Ablage gesperrt".to_owned())?;
        *slot = Some(challenge);
        Ok(staged)
    }
}

/// Write-only key access of one connection: describe/commit staged keys, seal
/// them in the profile's secret store. No method returns a value.
struct StagedKeyPort {
    secrets: Arc<StagedSecrets>,
    owner: SecretOwner,
    home: PathBuf,
    layers: [PathBuf; 2],
}

impl StagedKeyPort {
    fn writer(&self) -> Result<crate::secret_store::SecretStoreWriter, String> {
        let config = harw_config::discover_config(&self.layers)
            .map_err(|_| "Konfiguration nicht lesbar".to_owned())?;
        crate::secret_store::open_secret_store_for_writing(&self.home, &config)
    }
}

impl StagedSecretPort for StagedKeyPort {
    fn describe(&self, handle: &str, for_provider: &str) -> Result<StagedKeyInfo, String> {
        let info = self
            .secrets
            .describe(&self.owner, handle, for_provider, Timestamp::now())
            .map_err(|error| error.to_string())?;
        Ok(StagedKeyInfo {
            key_name: info.key_name,
            for_provider: info.for_provider,
            shape: match info.shape {
                KeyShape::AnthropicOAuthToken => StagedKeyShape::AnthropicOAuthToken,
                KeyShape::AnthropicApiKey => StagedKeyShape::AnthropicApiKey,
                KeyShape::OpenAiStyleKey => StagedKeyShape::OpenAiStyleKey,
                KeyShape::Other => StagedKeyShape::Other,
            },
        })
    }

    fn commit(&self, handle: &str, for_provider: &str) -> Result<harw_config::SecretRef, String> {
        let mut writer = self.writer()?;
        let secret = self
            .secrets
            .take(&self.owner, handle, for_provider, Timestamp::now())
            .map_err(|error| error.to_string())?;
        let id = writer
            .sink()
            .put(secret, REMOTE_PROVIDER_PURPOSE)
            .map_err(|error| error.to_string())?;
        Ok(harw_config::SecretRef::Secrets(id.to_string()))
    }

    fn rollback(&self, reference: &harw_config::SecretRef) {
        let harw_config::SecretRef::Secrets(raw) = reference else {
            return;
        };
        let Ok(id) = harw_secrets::SecretId::parse(raw) else {
            return;
        };
        if let Ok(mut writer) = self.writer() {
            let _ = writer.sink().discard(&id);
        }
    }
}

// ── Pure helpers ─────────────────────────────────────────────────────────────

/// `OpsCaller` -> `RemoteGrant` (only from `granted` and the effective tier,
/// never from params).
pub(crate) fn grant_for(caller: &OpsCaller) -> RemoteGrant {
    RemoteGrant {
        observe: caller.granted.observe,
        steer: caller.granted.steer,
        control: caller.granted.control,
        tier: caller.tier,
    }
}

/// Owner of staged secrets, from the authenticated identity.
fn secret_owner(identity: &ClientIdentity) -> SecretOwner {
    SecretOwner {
        tenant: identity.tenant.as_ref().map(|t| t.as_str().to_owned()),
        device: identity.device.as_ref().map(|d| d.as_str().to_owned()),
        connection: identity.connection.0,
    }
}

/// Modes a caller may set as `mode.default` (contract OFFEN-6, fail closed):
/// observers none, operators the modes without extra rights, maintainers and
/// owners all.
fn ceiling_for(tier: PermissionTier) -> RemoteConfigCeiling {
    let allowed_modes = match tier {
        PermissionTier::Observer => Vec::new(),
        PermissionTier::Operator => ["chat", "plan", "explore"]
            .into_iter()
            .filter(|mode| harw_core::InteractionMode::names().any(|known| known == *mode))
            .map(str::to_owned)
            .collect(),
        PermissionTier::Maintainer | PermissionTier::Owner => harw_core::InteractionMode::names()
            .map(str::to_owned)
            .collect(),
    };
    RemoteConfigCeiling { allowed_modes }
}

/// Typed session way for a session verb, `None` for every other line.
fn session_action(
    path: &str,
    verb: &RemoteVerb,
    args: &[String],
) -> Option<Result<SessionAction, String>> {
    match (path, verb.cap) {
        ("/stop" | "/cancel", RemoteCap::Steer) => Some(Ok(SessionAction::Interrupt)),
        ("/mode", RemoteCap::Control) => Some(match args {
            [mode] => Ok(SessionAction::SetMode(mode.clone())),
            _ => Err("Aufruf: /mode <modus>".to_owned()),
        }),
        ("/effort", RemoteCap::Control) => Some(match args {
            [effort] => Ok(SessionAction::SetEffort(effort.clone())),
            _ => Err("Aufruf: /effort <stufe>".to_owned()),
        }),
        ("/model", RemoteCap::Control) => Some(match args {
            [switch, model] if switch == "switch" => Ok(SessionAction::SetModel(model.clone())),
            _ => Err("Aufruf: /model switch <modell>".to_owned()),
        }),
        _ => None,
    }
}

fn check_client_msg_id(id: &str) -> Result<(), HostError> {
    if id.is_empty() || id.len() > MAX_CLIENT_MSG_ID_BYTES {
        return Err(HostError::Protocol("invalid client_msg_id".to_owned()));
    }
    Ok(())
}

const fn wire_effect(effect: RemoteEffect) -> CommandEffect {
    match effect {
        RemoteEffect::Applied => CommandEffect::Applied,
        RemoteEffect::Staged => CommandEffect::Staged,
        RemoteEffect::NextSession => CommandEffect::NextSession,
    }
}

/// `RemoteCommandView` -> `CommandEntry`, field by field.
pub(crate) fn command_entry(view: &harw_ops::command_meta::RemoteCommandView) -> CommandEntry {
    CommandEntry {
        name: view.name.clone(),
        aliases: view.aliases.clone(),
        summary: view.summary.clone(),
        usage: view.usage.clone(),
        domain: view.domain.as_str().to_owned(),
        permission: view.permission,
        busy: view.busy.to_owned(),
        origin: match view.origin {
            harw_ops::command_meta::CommandOriginKind::Operation => CommandOrigin::Operation,
            harw_ops::command_meta::CommandOriginKind::TuiLocal => CommandOrigin::TuiLocal,
        },
        remote: view.remote,
        available: view.available,
        reason: view.reason.clone(),
        reason_code: view.reason_code.map(str::to_owned),
        subcommands: view
            .subcommands
            .iter()
            .map(|hint| CommandHint {
                name: hint.name.clone(),
                args: hint.args.clone(),
                summary: hint.summary.clone(),
            })
            .collect(),
    }
}

/// `CatalogView` -> `CatalogResult` via serde (same field names).
///
/// # Errors
/// `HostError::Storage` with a fixed text (no catalog content).
pub(crate) fn catalog_result(
    view: &harw_ops::models::CatalogView,
) -> Result<CatalogResult, HostError> {
    serde_json::to_value(view)
        .and_then(serde_json::from_value)
        .map_err(|_| HostError::Storage("Katalog nicht abbildbar".to_owned()))
}

/// `ConfigRow` -> `ConfigEntry`.
pub(crate) fn config_entry(row: &harw_ops::settings::ConfigRow) -> ConfigEntry {
    ConfigEntry {
        key: row.key.to_owned(),
        value: row.value.clone(),
        choices: row.choices.iter().map(|c| (*c).to_owned()).collect(),
        scope: row.scope.to_owned(),
        apply: row.apply.to_owned(),
    }
}

/// Refusal -> `CommandRunResult{ok:false, effect: refused, reason_code}`.
pub(crate) fn refused(refusal: &RemoteRefusal) -> CommandRunResult {
    refused_code(refusal.code(), refusal.reason_de())
}

fn refused_code(code: &str, text: impl Into<String>) -> CommandRunResult {
    CommandRunResult {
        ok: false,
        text: text.into(),
        data: None,
        effect: CommandEffect::Refused,
        reason_code: Some(code.to_owned()),
        stepup: None,
    }
}

fn stepup_disabled() -> CommandRunResult {
    refused_code(
        refusal_codes::STEPUP_DISABLED,
        "Step-up-Schreibvorgaenge sind auf diesem Gateway noch nicht freigeschaltet",
    )
}

/// `OpOutput` + effect -> `CommandRunResult{ok:true, text, data, effect}`.
pub(crate) fn applied(output: OpOutput, effect: RemoteEffect) -> CommandRunResult {
    CommandRunResult {
        ok: true,
        text: output.text,
        data: output.data,
        effect: wire_effect(effect),
        reason_code: None,
        stepup: None,
    }
}

fn stepup_result(
    text: String,
    data: Option<serde_json::Value>,
    challenge: harw_session_host::stepup::StepUpChallenge,
) -> CommandRunResult {
    CommandRunResult {
        ok: true,
        text,
        data,
        effect: CommandEffect::StepupRequired,
        reason_code: None,
        stepup: Some(wire_challenge(challenge)),
    }
}

/// An op error as a refusal (`not_available` or `op_failed`).
fn op_refusal(error: OpError) -> CommandRunResult {
    match error {
        OpError::NotAvailable(text) => refused_code(refusal_codes::NOT_AVAILABLE, text),
        other => refused_code(refusal_codes::OP_FAILED, other.to_string()),
    }
}

fn dispatch_result(result: Result<OpOutput, OpError>, effect: RemoteEffect) -> CommandRunResult {
    match result {
        Ok(out) => applied(out, effect),
        Err(error) => op_refusal(error),
    }
}

/// An op error of a read view as a host error; the text names only the key.
fn op_host_error(error: OpError) -> HostError {
    match error {
        OpError::InvalidArguments(text) => HostError::Protocol(text),
        OpError::NotAvailable(text) => HostError::Denied(text),
        OpError::Execution(_) => HostError::Storage("Konfiguration nicht lesbar".to_owned()),
    }
}

/// Host `StepUpChallenge` -> wire type, field by field.
pub(crate) fn wire_challenge(
    c: harw_session_host::stepup::StepUpChallenge,
) -> harw_protocol::session_wire::StepUpChallenge {
    harw_protocol::session_wire::StepUpChallenge {
        stepup_id: c.stepup_id,
        action: c.action,
        target: c.target,
        text: c.text,
        nonce: c.nonce,
        hash: c.hash,
        expires_at: c.expires_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_protocol::ClientCaps;
    use harw_protocol::session_wire::WireSecret;
    use harw_session_host::ops::tier_for_caps;
    use harw_session_host::{caps_for_tier, gateway_caps_for_tier};
    use harw_types::{
        ApprovalActor, AuthStrength, DeviceId, IngressSurface, Principal, PrincipalKind,
        TenantId, TrustZone,
    };
    use jiff::SignedDuration;
    use std::path::Path;

    const OWNER_CAPS: ClientCaps = ClientCaps::ALL.with(ClientCaps::GATEWAY_ADMIN);

    fn caller_on(caps: ClientCaps, connection: u64) -> OpsCaller {
        let tier = tier_for_caps(caps);
        let identity = ClientIdentity {
            principal: Principal::trusted_ingress(
                PrincipalKind::Human,
                "device:device-test-1".to_owned(),
                IngressSurface::Gateway,
                tier,
            ),
            tenant: Some(TenantId::from_str("tenant-test")),
            caps,
            device: Some(DeviceId::from_str("device-test-1")),
            authenticated_node: None,
            authenticated_key_fingerprint: None,
            actor: ApprovalActor::Operator {
                id: "device:device-test-1".to_owned(),
            },
            label: "phone-test".to_owned(),
            zone: TrustZone::Remote,
            strength: AuthStrength::HardwareBacked,
            connection: ConnectionId(connection),
            agent: None,
        };
        OpsCaller {
            identity,
            granted: caps,
            tier,
            profile_tenant: Some(TenantId::from_str("tenant-test")),
        }
    }

    fn caller(caps: ClientCaps) -> OpsCaller {
        caller_on(caps, 7)
    }

    fn ops(home: &Path) -> TestResult<HostedOps> {
        let opened = HostedOps::open(home.to_path_buf()).map_err(TestError::Unexpected)?;
        std::fs::create_dir_all(&opened.bound.profile_dir).map_err(ctx("profile dir"))?;
        Ok(opened)
    }

    fn hosted_session() -> OpsSession {
        OpsSession {
            session_id: SessionId::new(),
            hosted: true,
            model: None,
        }
    }

    fn run_params(line: &str, id: &str) -> CommandRunParams {
        CommandRunParams {
            session_id: None,
            line: line.to_owned(),
            client_msg_id: id.to_owned(),
        }
    }

    async fn run_done(
        ops: &HostedOps,
        caller: &OpsCaller,
        session: Option<OpsSession>,
        line: &str,
        id: &str,
    ) -> TestResult<CommandRunResult> {
        match ops
            .command_run(caller.clone(), session, run_params(line, id))
            .await
            .map_err(ctx("command_run"))?
        {
            OpsRun::Done(result) => Ok(result),
            other => Err(TestError::Unexpected(format!("{other:?}"))),
        }
    }

    fn config_path(ops: &HostedOps) -> PathBuf {
        ops.bound.profile_dir.join("config.toml")
    }

    fn read_bytes(path: &Path) -> Vec<u8> {
        std::fs::read(path).unwrap_or_default()
    }

    fn slice_global() -> bool {
        harw_ops::command_meta::REMOTE_ENABLED_SLICE >= RemoteSlice::Global
    }

    fn code(result: &CommandRunResult) -> &str {
        result.reason_code.as_deref().unwrap_or("")
    }

    fn challenge_of(result: &CommandRunResult) -> TestResult<StepUpConfirmParams> {
        let challenge = result
            .stepup
            .as_ref()
            .ok_or(TestError::Missing("stepup challenge"))?;
        Ok(StepUpConfirmParams {
            stepup_id: challenge.stepup_id.clone(),
            hash: challenge.hash.clone(),
        })
    }

    #[tokio::test]
    async fn status_and_models_show_like_tui() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let ops = ops(dir.path())?;
        let owner = caller(OWNER_CAPS);
        let status = run_done(&ops, &owner, None, "/status", "m-1").await?;
        assert!(status.ok, "{status:?}");
        assert_eq!(status.effect, CommandEffect::Applied);
        assert!(!status.text.is_empty());

        let remote = run_done(&ops, &owner, None, "/models show", "m-2").await?;
        assert!(remote.ok, "{remote:?}");
        let adapter = ops
            .adapters
            .iter()
            .find(|a| a.path() == "/models")
            .ok_or(TestError::Missing("/models adapter"))?;
        let local_caller = OpsCaller {
            profile_tenant: None,
            ..owner.clone()
        };
        let tui_ctx = ops.op_context(&local_caller, None, None, None);
        let tui = adapter
            .dispatch(&tui_ctx, vec!["show".to_owned()])
            .await
            .map_err(ctx("tui dispatch"))?;
        assert_eq!(remote.text, tui.text);
        Ok(())
    }

    #[tokio::test]
    async fn observer_sees_commands_grey_with_reason() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let ops = ops(dir.path())?;
        let list = ops
            .commands_list(
                caller(ClientCaps::OBSERVE),
                None,
                CommandsListParams::default(),
            )
            .await
            .map_err(ctx("commands_list"))?;
        assert_eq!(list.rev.len(), 16);
        let stop = list
            .commands
            .iter()
            .find(|c| c.name == "stop")
            .ok_or(TestError::Missing("stop"))?;
        assert!(!stop.available);
        assert_eq!(stop.reason.as_deref(), Some("braucht Recht Steuern"));
        assert_eq!(stop.reason_code.as_deref(), Some("missing_cap"));
        let tools = list
            .commands
            .iter()
            .find(|c| c.name == "tools")
            .ok_or(TestError::Missing("tools"))?;
        assert_eq!(tools.reason.as_deref(), Some("nur in der TUI"));
        Ok(())
    }

    #[tokio::test]
    async fn stop_maps_to_typed_interrupt() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let ops = ops(dir.path())?;
        let operator = caller(ClientCaps::OPERATE);
        let run = ops
            .command_run(
                operator.clone(),
                Some(hosted_session()),
                run_params("/stop", "s-1"),
            )
            .await
            .map_err(ctx("command_run"))?;
        match run {
            OpsRun::Session { action, result } => {
                assert_eq!(action, SessionAction::Interrupt);
                assert!(result.ok);
            }
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        let without = run_done(&ops, &operator, None, "/stop", "s-2").await?;
        assert!(!without.ok);
        assert_eq!(code(&without), refusal_codes::SESSION_REQUIRED);
        Ok(())
    }

    #[tokio::test]
    async fn catalog_get_never_leaks_secrets() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let ops = ops(dir.path())?;
        let providers = ops.bound.profile_dir.join("providers");
        std::fs::create_dir_all(&providers).map_err(ctx("providers dir"))?;
        std::fs::write(
            providers.join("p1.toml"),
            "name = \"p1\"\napi = \"openai-chat\"\n\
             base_url = \"https://user:pw-fake@api.example.test/v1?key=q#frag\"\n\
             api_key = \"sk-fake-SECRET-marker\"\n\
             [headers]\nX-Fake = \"hdr-fake-SECRET-marker\"\n",
        )
        .map_err(ctx("provider file"))?;
        let result = ops
            .catalog_get(
                caller(ClientCaps::OBSERVE),
                None,
                CatalogGetParams::default(),
            )
            .await
            .map_err(ctx("catalog_get"))?;
        assert!(result.providers.iter().any(|p| p.name == "p1"));
        let json = serde_json::to_string(&result).map_err(ctx("json"))?;
        for forbidden in ["SECRET", "pw-fake", "X-Fake", "?key", "#frag"] {
            assert!(!json.contains(forbidden), "{forbidden} leaked");
        }
        Ok(())
    }

    #[test]
    fn catalog_result_converts_from_view() -> TestResult {
        use harw_ops::models::{AuthKind, CatalogView, LiveView, ProviderView};
        let kinds = [
            AuthKind::None,
            AuthKind::Env,
            AuthKind::File,
            AuthKind::Keyring,
            AuthKind::Secrets,
            AuthKind::FileJson,
            AuthKind::InlineKey,
        ];
        let view = CatalogView {
            providers: kinds
                .iter()
                .enumerate()
                .map(|(index, kind)| ProviderView {
                    name: format!("p{index}"),
                    api: "openai-chat".to_owned(),
                    base_url: "https://api.example.test/v1".to_owned(),
                    enabled: true,
                    auth_kind: *kind,
                    auth_name: None,
                    has_secret: false,
                    models: vec!["m1".to_owned()],
                    max_concurrency: Some(2),
                })
                .collect(),
            models: Vec::new(),
            roles: Vec::new(),
            live: LiveView {
                provider: None,
                model: Some("m1".to_owned()),
            },
            note: "gilt ab nächster Session",
        };
        let result = catalog_result(&view).map_err(ctx("catalog_result"))?;
        assert_eq!(result.providers.len(), kinds.len());
        assert!(result.providers.iter().all(|p| !matches!(
            p.auth_kind,
            harw_protocol::session_wire::ProviderAuthKind::Unknown
        )));
        assert_eq!(result.live.model.as_deref(), Some("m1"));
        Ok(())
    }

    #[tokio::test]
    async fn observer_cannot_write() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let ops = ops(dir.path())?;
        std::fs::write(config_path(&ops), "[tui]\ntheme = 'dark'\n").map_err(ctx("seed"))?;
        let before = read_bytes(&config_path(&ops));
        let result = run_done(
            &ops,
            &caller(ClientCaps::OBSERVE),
            None,
            "/models set uia p/m",
            "w-1",
        )
        .await?;
        assert!(!result.ok);
        assert!(
            ["missing_cap", "slice_disabled"].contains(&code(&result)),
            "{result:?}"
        );
        assert_eq!(read_bytes(&config_path(&ops)), before);
        Ok(())
    }

    #[tokio::test]
    async fn operator_without_control_cannot_manage() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let ops = ops(dir.path())?;
        std::fs::write(config_path(&ops), "[tui]\ntheme = 'dark'\n").map_err(ctx("seed"))?;
        let before = read_bytes(&config_path(&ops));
        let result = run_done(
            &ops,
            &caller(ClientCaps::OPERATE),
            None,
            "/models set uia p/m",
            "w-2",
        )
        .await?;
        assert!(!result.ok);
        let expected = if slice_global() {
            "missing_cap"
        } else {
            "slice_disabled"
        };
        assert_eq!(code(&result), expected);
        assert_eq!(read_bytes(&config_path(&ops)), before);
        Ok(())
    }

    #[tokio::test]
    async fn role_change_needs_stepup() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let ops = ops(dir.path())?;
        let owner = caller(OWNER_CAPS);
        let first = run_done(&ops, &owner, None, "/models set uia p/m", "r-1").await?;
        if !slice_global() {
            assert_eq!(code(&first), "slice_disabled");
            return Ok(());
        }
        let before = read_bytes(&config_path(&ops));
        assert_eq!(first.effect, CommandEffect::StepupRequired, "{first:?}");
        assert_eq!(read_bytes(&config_path(&ops)), before);
        let confirm = challenge_of(&first)?;
        let done = ops
            .stepup_confirm(owner.clone(), confirm.clone())
            .await
            .map_err(ctx("confirm"))?;
        assert!(done.ok, "{done:?}");
        assert_ne!(read_bytes(&config_path(&ops)), before);
        assert!(ops.stepup_confirm(owner, confirm).await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn expired_or_reused_stepup_fails() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let ops = ops(dir.path())?;
        let owner = caller(OWNER_CAPS);
        let now = Timestamp::now();
        let past = now
            .checked_sub(SignedDuration::from_secs(600))
            .map_err(ctx("past"))?;
        let old = ops.stage_config(&owner, "tui.theme", Some("light"), past);
        assert_eq!(old.effect, CommandEffect::StepupRequired, "{old:?}");
        assert!(
            ops.confirm_at(&owner, &challenge_of(&old)?, now)
                .await
                .is_err()
        );

        let fresh = ops.stage_config(&owner, "tui.theme", Some("light"), now);
        let confirm = challenge_of(&fresh)?;
        let done = ops
            .confirm_at(&owner, &confirm, now)
            .await
            .map_err(ctx("confirm"))?;
        assert!(done.ok, "{done:?}");
        assert!(ops.confirm_at(&owner, &confirm, now).await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn config_set_stale_is_refused() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let ops = ops(dir.path())?;
        let owner = caller(OWNER_CAPS);
        std::fs::write(config_path(&ops), "[tui]\ntheme = 'dark'\n").map_err(ctx("seed"))?;
        let now = Timestamp::now();
        let planned = ops.stage_config(&owner, "tui.theme", Some("light"), now);
        let confirm = challenge_of(&planned)?;
        std::fs::write(config_path(&ops), "[tui]\ntheme = 'auto'\n").map_err(ctx("edit"))?;
        let result = ops
            .confirm_at(&owner, &confirm, now)
            .await
            .map_err(ctx("confirm"))?;
        assert!(!result.ok);
        assert_eq!(code(&result), refusal_codes::STALE, "{result:?}");
        Ok(())
    }

    #[tokio::test]
    async fn config_set_is_gated_by_slice() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let ops = ops(dir.path())?;
        let result = ops
            .config_set(
                caller(OWNER_CAPS),
                ConfigSetParams {
                    key: "tui.theme".to_owned(),
                    value: "light".to_owned(),
                    client_msg_id: "c-1".to_owned(),
                },
            )
            .await
            .map_err(ctx("config_set"))?;
        if slice_global() {
            assert_eq!(result.effect, CommandEffect::StepupRequired);
        } else {
            assert_eq!(code(&result), refusal_codes::STEPUP_DISABLED);
        }
        Ok(())
    }

    #[tokio::test]
    async fn client_msg_id_dedups_command_run() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let ops = ops(dir.path())?;
        let owner = caller(OWNER_CAPS);
        let first = run_done(&ops, &owner, None, "/status", "d-1").await?;
        let second = run_done(&ops, &owner, None, "/help", "d-1").await?;
        // Same id: the cached `/status` answer, `/help` never ran.
        assert_eq!(first, second);
        let cached = ops
            .dedup
            .lock()
            .map(|map| map.get(&7).map_or(0, VecDeque::len))
            .unwrap_or_default();
        assert_eq!(cached, 1);
        assert!(
            ops.command_run(owner, None, run_params("/status", ""))
                .await
                .is_err()
        );
        Ok(())
    }

    #[tokio::test]
    async fn secret_put_never_returns_value() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let ops = ops(dir.path())?;
        let owner = caller(OWNER_CAPS);
        let params = || SecretPutParams {
            name: "acme-key".to_owned(),
            for_provider: "acme".to_owned(),
            value: WireSecret::new("sk-test-FAKE-0000000000000000".to_owned()),
        };
        let public = ops.secret_put(owner.clone(), params()).await;
        if !slice_global() {
            assert!(matches!(public, Err(HostError::Denied(_))));
        }
        let result = ops
            .stage_secret(&owner, params(), Timestamp::now())
            .map_err(ctx("stage_secret"))?;
        let json = serde_json::to_string(&result).map_err(ctx("json"))?;
        assert!(!json.contains("FAKE"));
        assert!(!format!("{result:?}").contains("FAKE"));

        let other = caller_on(OWNER_CAPS, 8);
        let foreign = ops.key_port(&other.identity);
        let refused = foreign.describe(&result.handle, "acme");
        assert!(refused.is_err());
        assert!(!format!("{refused:?}").contains("FAKE"));
        let own = ops.key_port(&owner.identity);
        assert!(own.describe(&result.handle, "acme").is_ok());
        Ok(())
    }

    #[tokio::test]
    async fn provider_add_without_stepup_writes_nothing() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let ops = ops(dir.path())?;
        let result = run_done(
            &ops,
            &caller(OWNER_CAPS),
            None,
            "/provider add anthropic",
            "p-1",
        )
        .await?;
        if slice_global() {
            assert_eq!(result.effect, CommandEffect::StepupRequired, "{result:?}");
        } else {
            assert!(!result.ok);
        }
        let providers = ops.bound.profile_dir.join("providers");
        let written = std::fs::read_dir(&providers)
            .map(|entries| entries.count())
            .unwrap_or(0);
        assert_eq!(written, 0);
        Ok(())
    }

    #[test]
    fn limits_match_protocol_constants() {
        assert_eq!(
            MAX_COMMAND_LINE_BYTES,
            harw_ops::command_meta::REMOTE_LINE_MAX_BYTES
        );
        assert_eq!(
            MAX_SECRET_VALUE_BYTES,
            harw_secrets::write_only::MAX_SECRET_LEN + 2
        );
        assert_eq!(harw_session_host::stepup::MAX_TEXT_BYTES, 4096);
    }

    #[test]
    fn tier_for_caps_inverts_caps_for_tier() {
        for tier in [
            PermissionTier::Observer,
            PermissionTier::Operator,
            PermissionTier::Maintainer,
            PermissionTier::Owner,
        ] {
            let caps = caps_for_tier(tier).with(gateway_caps_for_tier(tier));
            assert_eq!(tier_for_caps(caps), tier, "{tier:?}");
        }
    }

    #[tokio::test]
    async fn connection_closed_drops_stepups_and_secrets() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let ops = ops(dir.path())?;
        let owner = caller(OWNER_CAPS);
        let now = Timestamp::now();
        let staged = ops.stage_config(&owner, "tui.theme", Some("light"), now);
        assert_eq!(staged.effect, CommandEffect::StepupRequired, "{staged:?}");
        ops.stage_secret(
            &owner,
            SecretPutParams {
                name: "acme-key".to_owned(),
                for_provider: "acme".to_owned(),
                value: WireSecret::new("sk-test-FAKE-0000000000000000".to_owned()),
            },
            now,
        )
        .map_err(ctx("stage_secret"))?;
        assert_eq!(ops.stepups.pending_len(), 1);
        assert!(!ops.secrets.is_empty());
        ops.connection_closed(&owner.identity);
        assert_eq!(ops.stepups.pending_len(), 0);
        assert!(ops.secrets.is_empty());
        Ok(())
    }

    #[test]
    fn session_verbs_map_to_actions() {
        let control = RemoteVerb::control(&["*"], RemoteEffect::Staged);
        let steer = RemoteVerb::steer(&["*"], RemoteEffect::Applied);
        let args = |items: &[&str]| items.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            session_action("/cancel", &steer, &[]),
            Some(Ok(SessionAction::Interrupt))
        );
        assert_eq!(
            session_action("/mode", &control, &args(&["work"])),
            Some(Ok(SessionAction::SetMode("work".to_owned())))
        );
        assert_eq!(
            session_action("/effort", &control, &args(&["high"])),
            Some(Ok(SessionAction::SetEffort("high".to_owned())))
        );
        assert_eq!(
            session_action("/model", &control, &args(&["switch", "m1"])),
            Some(Ok(SessionAction::SetModel("m1".to_owned())))
        );
        assert!(matches!(
            session_action("/model", &control, &args(&["switch"])),
            Some(Err(_))
        ));
        assert_eq!(session_action("/status", &steer, &[]), None);
    }

    #[test]
    fn mode_ceiling_is_fail_closed() {
        assert!(ceiling_for(PermissionTier::Observer).allowed_modes.is_empty());
        let operator = ceiling_for(PermissionTier::Operator).allowed_modes;
        assert!(!operator.iter().any(|m| m == "work" || m == "shell"));
        let owner = ceiling_for(PermissionTier::Owner).allowed_modes;
        assert_eq!(owner.len(), harw_core::InteractionMode::names().count());
    }
}
