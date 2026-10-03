//! The [`Executor`] over a container engine.

use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use harw_container_model::{
    ContainerId, ContainerInstance, LABEL_ATTEMPT, LABEL_EPOCH, LABEL_OWNER, LABEL_TENANT,
    LABEL_WORK_ID, OwnerLabels,
};
use harw_job_core::{ExitOutcome, JobSpec, SandboxProfileName, SandboxReport};
use harw_job_runtime::RuntimeError;
use harw_job_runtime::coordinator::{
    AttemptContext, AttemptControl, AttemptEvent, AttemptEventSender, AttemptEvents, AttemptRun,
    Executor, Probe, StartedAttempt, check_requirement,
};

use crate::config::OciConfig;
use crate::enforcement::{NetworkIntent, Requested, report_from_inspect};
use crate::engine::{Channel, Engine, InspectDoc};
use crate::error::OciError;

/// How long the supervisor waits for the log pump after the container ended.
const LOG_DRAIN: Duration = Duration::from_secs(10);
/// Capacity of an attempt's event channel.
const EVENT_CAPACITY: usize = 64;

/// Runs each attempt as one container.
#[derive(Debug, Clone)]
pub struct OciExecutor {
    engine: Engine,
}

impl OciExecutor {
    /// Creates the executor.
    ///
    /// # Errors
    /// [`OciError::Config`] for an invalid configuration.
    pub fn new(config: OciConfig) -> Result<Self, OciError> {
        config.validate()?;
        Ok(Self {
            engine: Engine::new(Arc::new(config)),
        })
    }
}

fn profile_name(profile: SandboxProfileName) -> String {
    serde_json::to_value(profile)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

fn network_for(profile: SandboxProfileName) -> (&'static str, NetworkIntent) {
    match profile {
        SandboxProfileName::WorkspaceBuild => ("bridge", NetworkIntent::Allowed),
        SandboxProfileName::ReadOnlyAnalysis | SandboxProfileName::NoNetwork => {
            ("none", NetworkIntent::None)
        }
        SandboxProfileName::NetworkRestricted => ("none", NetworkIntent::Restricted),
    }
}

fn cpu_shares(weight: u16) -> u64 {
    (u64::from(weight) * 1024 / 100).clamp(2, 262_144)
}

fn work_dir(spec: &JobSpec) -> Result<String, OciError> {
    let rel = spec.working_dir.as_str();
    if rel.contains('\\') {
        return Err(OciError::Unsupported("backslash in working_dir".into()));
    }
    let rel = rel.trim_matches('/');
    if rel.is_empty() || rel == "." {
        Ok("/work".to_owned())
    } else {
        Ok(format!("/work/{rel}"))
    }
}

/// The create request. Nothing here is privileged: no capabilities, no new
/// privileges, read-only root, scratch on tmpfs, no host mounts, no restart.
fn create_body(
    cfg: &OciConfig,
    spec: &JobSpec,
    image_ref: &str,
    labels: &OwnerLabels,
    network: &str,
) -> Result<serde_json::Value, OciError> {
    let env: Vec<String> = spec.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
    let mut host = serde_json::json!({
        "NetworkMode": network,
        "ReadonlyRootfs": true,
        "Privileged": false,
        "CapDrop": ["ALL"],
        "CapAdd": [],
        "SecurityOpt": ["no-new-privileges"],
        "RestartPolicy": {"Name": "no"},
        "AutoRemove": false,
        "Tmpfs": {
            "/work": format!("rw,nosuid,nodev,size={}", cfg.work_tmpfs_bytes),
            "/tmp": "rw,nosuid,nodev,noexec,size=67108864",
        },
    });
    let resources = &spec.resources;
    if let Some(memory) = resources.memory_max {
        host["Memory"] = memory.into();
        host["MemorySwap"] = memory.into();
    }
    if let Some(weight) = resources.cpu_weight {
        host["CpuShares"] = cpu_shares(weight).into();
    }
    if let Some(pids) = resources.pids_max {
        host["PidsLimit"] = pids.into();
    }
    Ok(serde_json::json!({
        "Image": image_ref,
        "Entrypoint": [spec.program],
        "Cmd": spec.args,
        "Env": env,
        "WorkingDir": work_dir(spec)?,
        "Labels": labels.to_map(),
        "AttachStdin": false,
        "OpenStdin": false,
        "Tty": false,
        "HostConfig": host,
    }))
}

fn parse_created(doc: &InspectDoc) -> Result<i64, OciError> {
    jiff::Timestamp::from_str(&doc.created)
        .map(|t| t.as_second())
        .map_err(|e| OciError::Protocol(format!("inspect Created: {e}")))
}

fn exit_outcome(code: i64) -> ExitOutcome {
    i32::try_from(code).map_or(ExitOutcome::Unknown, ExitOutcome::Exited)
}

/// Cancels a container: graceful stop, then kill if the stop failed.
struct OciControl {
    engine: Engine,
    id: ContainerId,
    cancelled: AtomicBool,
}

impl AttemptControl for OciControl {
    fn cancel(&self) {
        if self.cancelled.swap(true, Ordering::SeqCst) {
            return;
        }
        let engine = self.engine.clone();
        let id = self.id.clone();
        let _ = std::thread::Builder::new()
            .name("harw-oci-cancel".into())
            .spawn(move || {
                let grace = engine.config().stop_grace_secs;
                if engine.stop(&id, grace).is_err() {
                    let _ = engine.kill(&id);
                }
            });
    }
}

fn pump_logs(mut stream: crate::engine::LogStream, tx: &AttemptEventSender) {
    loop {
        match stream.next_frame() {
            Ok(Some((channel, bytes))) => {
                let event = match channel {
                    Channel::Stdout => AttemptEvent::Stdout(bytes),
                    Channel::Stderr => AttemptEvent::Stderr(bytes),
                };
                if !tx.send_blocking(event) {
                    return;
                }
            }
            Ok(None) => return,
            Err(error) => {
                let _ = tx.send_blocking(AttemptEvent::Error(format!("log stream: {error}")));
                return;
            }
        }
    }
}

/// Spawns the supervisor: pumps logs, waits for the container to stop and
/// sends the final `Exited`.
fn supervise(engine: &Engine, id: &ContainerId, tail: &'static str) -> AttemptRun {
    let (tx, events) = AttemptEvents::channel(EVENT_CAPACITY);
    let control = Arc::new(OciControl {
        engine: engine.clone(),
        id: id.clone(),
        cancelled: AtomicBool::new(false),
    });
    let engine = engine.clone();
    let id = id.clone();
    let sender = tx.clone();
    let spawned = std::thread::Builder::new()
        .name("harw-oci-supervisor".into())
        .spawn(move || {
            let (done_tx, done_rx) = mpsc::channel::<()>();
            match engine.logs(&id, tail) {
                Ok(stream) => {
                    let pump_tx = sender.clone();
                    let _ = std::thread::Builder::new()
                        .name("harw-oci-logs".into())
                        .spawn(move || {
                            pump_logs(stream, &pump_tx);
                            let _ = done_tx.send(());
                        });
                }
                Err(error) => {
                    let _ = sender.send_blocking(AttemptEvent::Error(format!("logs: {error}")));
                    drop(done_tx);
                }
            }
            let outcome = match engine.wait(&id) {
                Ok(code) => exit_outcome(code),
                Err(error) => {
                    let _ = sender.send_blocking(AttemptEvent::Error(format!("wait: {error}")));
                    ExitOutcome::Unknown
                }
            };
            // The pump ends at EOF once the container stopped; do not hang
            // on an engine that never closes the stream.
            let _ = done_rx.recv_timeout(LOG_DRAIN);
            let _ = sender.send_blocking(AttemptEvent::Exited {
                outcome,
                sandbox: None,
            });
        });
    if spawned.is_err() {
        let _ = tx.send_blocking(AttemptEvent::Exited {
            outcome: ExitOutcome::Unknown,
            sandbox: None,
        });
    }
    AttemptRun::new(events, control)
}

impl OciExecutor {
    fn labels_for(&self, spec: &JobSpec, ctx: &AttemptContext) -> Result<OwnerLabels, OciError> {
        let cfg = self.engine.config();
        // The attempt id is unique per claim and derived from the lease
        // epoch, so the epoch doubles as the attempt number here.
        OwnerLabels::new(
            &cfg.owner,
            ctx.job_id.as_str(),
            ctx.lease_epoch,
            ctx.lease_epoch,
            &cfg.tenant,
            &profile_name(spec.sandbox_profile),
        )
        .map_err(|e| OciError::Config(format!("labels: {e}")))
    }

    /// Checks a live inspect document against a persisted identity.
    fn verify(
        &self,
        doc: &InspectDoc,
        identity: &ContainerInstance,
    ) -> Result<OwnerLabels, String> {
        let cfg = self.engine.config();
        if doc.id != identity.container_id.as_str()
            && !doc.id.starts_with(identity.container_id.as_str())
        {
            return Err("engine returned another container id".into());
        }
        let created = parse_created(doc).map_err(|e| e.to_string())?;
        if created != identity.created_unix {
            return Err("creation time differs from the recorded one".into());
        }
        let suffix = format!("@{}", identity.image_digest.digest());
        match doc.config.image.as_deref() {
            Some(image) if image.ends_with(&suffix) => {}
            _ => return Err("container was not created from the recorded image digest".into()),
        }
        let labels = doc
            .config
            .labels
            .as_ref()
            .ok_or_else(|| "container carries no labels".to_owned())?;
        let owner = OwnerLabels::from_map(labels).map_err(|e| e.to_string())?;
        if owner.owner != cfg.owner || owner.tenant != cfg.tenant {
            return Err("container belongs to another owner or tenant".into());
        }
        Ok(owner)
    }

    fn label_filter(&self, ctx: &AttemptContext) -> Vec<(String, String)> {
        let cfg = self.engine.config();
        vec![
            (LABEL_OWNER.to_owned(), cfg.owner.clone()),
            (LABEL_TENANT.to_owned(), cfg.tenant.clone()),
            (LABEL_WORK_ID.to_owned(), ctx.job_id.as_str().to_owned()),
            (LABEL_EPOCH.to_owned(), ctx.lease_epoch.to_string()),
            (LABEL_ATTEMPT.to_owned(), ctx.lease_epoch.to_string()),
        ]
    }

    fn discard(&self, id: &ContainerId) {
        let _ = self.engine.remove(id);
    }

    pub(crate) fn owner_tenant(&self) -> (String, String) {
        let cfg = self.engine.config();
        (cfg.owner.clone(), cfg.tenant.clone())
    }

    /// Ids of every container labelled with this runner and tenant.
    pub(crate) fn list_owned(&self) -> Result<Vec<ContainerId>, OciError> {
        let (owner, tenant) = self.owner_tenant();
        self.engine.list_by_labels(&[
            (LABEL_OWNER.to_owned(), owner),
            (LABEL_TENANT.to_owned(), tenant),
        ])
    }

    /// Labels, creation time and state of one container; `None` if it is gone.
    pub(crate) fn describe(&self, id: &ContainerId) -> Result<Option<crate::gc::Listed>, OciError> {
        let doc = match self.engine.inspect(id) {
            Ok(doc) => doc,
            Err(OciError::NotFound) => return Ok(None),
            Err(error) => return Err(error),
        };
        Ok(Some(crate::gc::Listed {
            id: id.clone(),
            labels: doc.config.labels.clone().unwrap_or_default(),
            created_unix: parse_created(&doc)?,
            running: doc.state.running == Some(true),
        }))
    }

    pub(crate) fn remove_by_id(&self, id: &ContainerId) -> Result<(), OciError> {
        self.engine.remove(id)
    }
}

impl Executor for OciExecutor {
    type Identity = ContainerInstance;

    fn start(
        &self,
        spec: &JobSpec,
        ctx: &AttemptContext,
    ) -> Result<StartedAttempt<Self::Identity>, RuntimeError> {
        spec.validate()?;
        let cfg = self.engine.config();
        let image = cfg.image_for(spec.sandbox_profile).ok_or_else(|| {
            OciError::Unsupported(format!(
                "no image configured for profile `{}`",
                profile_name(spec.sandbox_profile)
            ))
        })?;
        let labels = self.labels_for(spec, ctx)?;
        let (network, intent) = network_for(spec.sandbox_profile);
        let requested = Requested {
            network: intent,
            memory: spec.resources.memory_max,
            cpu_shares: spec.resources.cpu_weight.map(cpu_shares),
            pids: spec.resources.pids_max,
        };
        let body = create_body(cfg, spec, &image.reference(), &labels, network)?;

        self.engine.require_image(image)?;
        let id = self.engine.create(&body)?;

        // Read the applied configuration back BEFORE anything runs.
        let doc = match self.engine.inspect(&id) {
            Ok(doc) => doc,
            Err(error) => {
                self.discard(&id);
                return Err(error.into());
            }
        };
        let created_unix = match parse_created(&doc) {
            Ok(created) => created,
            Err(error) => {
                self.discard(&id);
                return Err(error.into());
            }
        };
        let carried = doc
            .config
            .labels
            .as_ref()
            .and_then(|m| OwnerLabels::from_map(m).ok());
        if carried.as_ref() != Some(&labels) {
            self.discard(&id);
            return Err(
                OciError::Protocol("the engine did not keep the ownership labels".into()).into(),
            );
        }
        let report: SandboxReport = report_from_inspect(&doc, &requested);
        if let Err(error) = check_requirement(spec.sandbox, &report) {
            self.discard(&id);
            return Err(error);
        }

        if let Err(error) = self.engine.start(&id) {
            self.discard(&id);
            return Err(error.into());
        }
        let identity = ContainerInstance {
            engine: cfg.engine,
            container_id: id.clone(),
            created_unix,
            image_digest: image.clone(),
        };
        let run = supervise(&self.engine, &id, "all");
        Ok(StartedAttempt {
            identity: Some(identity),
            sandbox: Some(report),
            pid: None,
            run,
        })
    }

    fn probe(&self, identity: &Self::Identity) -> Result<Probe, RuntimeError> {
        let doc = match self.engine.inspect(&identity.container_id) {
            Ok(doc) => doc,
            Err(OciError::NotFound) => return Ok(Probe::Exited),
            Err(error) => return Err(error.into()),
        };
        if let Err(detail) = self.verify(&doc, identity) {
            return Ok(Probe::Mismatch(detail));
        }
        Ok(if doc.state.running == Some(true) {
            Probe::Alive
        } else {
            Probe::Exited
        })
    }

    fn reattach(
        &self,
        identity: &Self::Identity,
        ctx: &AttemptContext,
    ) -> Result<AttemptRun, RuntimeError> {
        let doc = match self.engine.inspect(&identity.container_id) {
            Ok(doc) => doc,
            Err(OciError::NotFound) => return Err(RuntimeError::ProcessAlreadyExited { pid: 0 }),
            Err(error) => return Err(error.into()),
        };
        let owner = self
            .verify(&doc, identity)
            .map_err(|detail| RuntimeError::IdentityMismatch { pid: 0, detail })?;
        if owner.work_id != ctx.job_id.as_str() || owner.lease_epoch != ctx.lease_epoch {
            return Err(RuntimeError::IdentityMismatch {
                pid: 0,
                detail: "container belongs to another job or lease epoch".into(),
            });
        }
        if doc.state.running != Some(true) {
            return Err(RuntimeError::ProcessAlreadyExited { pid: 0 });
        }
        Ok(supervise(&self.engine, &identity.container_id, "0"))
    }

    fn recorded_exit(&self, ctx: &AttemptContext) -> Option<ExitOutcome> {
        let ids = self.engine.list_by_labels(&self.label_filter(ctx)).ok()?;
        let [id] = ids.as_slice() else {
            return None;
        };
        let doc = self.engine.inspect(id).ok()?;
        if doc.state.running != Some(false) {
            return None;
        }
        doc.state.exit_code.map(exit_outcome)
    }

    fn finished(&self, ctx: &AttemptContext) {
        if let Ok(ids) = self.engine.list_by_labels(&self.label_filter(ctx)) {
            for id in ids {
                self.discard(&id);
            }
        }
    }
}
