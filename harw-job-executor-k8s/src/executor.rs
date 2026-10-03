//! The [`Executor`] over the Kubernetes API.

use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use harw_container_model::{
    LABEL_ATTEMPT, LABEL_EPOCH, LABEL_OWNER, LABEL_TENANT, LABEL_WORK_ID, OwnerLabels, PodInstance,
};
use harw_job_core::{ExitOutcome, JobSpec, SandboxProfileName};
use harw_job_runtime::RuntimeError;
use harw_job_runtime::coordinator::{
    AttemptContext, AttemptControl, AttemptEvent, AttemptEventSender, AttemptEvents, AttemptRun,
    Executor, Probe, StartedAttempt, check_requirement,
};
use serde_json::json;

use crate::config::K8sConfig;
use crate::error::K8sError;
use crate::pod::{self, PodDoc, Requested, report_from_pod};
use crate::transport::{HttpTransport, KubeTransport, Reply, Request};

const LOG_DRAIN: Duration = Duration::from_secs(10);
const EVENT_CAPACITY: usize = 64;
/// Consecutive API failures the supervisor tolerates.
const MAX_POLL_FAILURES: u32 = 10;
const LOG_CHUNK: usize = 16 * 1024;

/// Runs each attempt as one pod.
pub struct K8sExecutor {
    inner: Arc<Inner>,
}

struct Inner {
    cfg: K8sConfig,
    api: Box<dyn KubeTransport>,
}

impl std::fmt::Debug for K8sExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("K8sExecutor")
            .field("cfg", &self.inner.cfg)
            .finish_non_exhaustive()
    }
}

impl K8sExecutor {
    /// Creates the executor with the real HTTP(S) transport.
    ///
    /// # Errors
    /// [`K8sError::Config`] or a transport construction failure.
    pub fn new(config: K8sConfig) -> Result<Self, K8sError> {
        config.validate()?;
        let api = HttpTransport::new(&config)?;
        Ok(Self::with_transport(config, Box::new(api)))
    }

    /// Creates the executor over any transport (the config is not
    /// re-validated for transport-specific fields).
    ///
    /// # Errors
    /// [`K8sError::Config`].
    pub fn with_validated_transport(
        config: K8sConfig,
        api: Box<dyn KubeTransport>,
    ) -> Result<Self, K8sError> {
        config.validate()?;
        Ok(Self::with_transport(config, api))
    }

    fn with_transport(cfg: K8sConfig, api: Box<dyn KubeTransport>) -> Self {
        Self {
            inner: Arc::new(Inner { cfg, api }),
        }
    }
}

fn selector_encode(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'=' | b',') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

impl Inner {
    fn pods_path(&self) -> String {
        format!("/api/v1/namespaces/{}/pods", self.cfg.namespace)
    }

    fn call(
        &self,
        method: &'static str,
        path: String,
        content_type: &'static str,
        body: Option<&serde_json::Value>,
        ok: &[u16],
    ) -> Result<Reply, K8sError> {
        let request = Request {
            method,
            path,
            content_type,
            body: body.map(|b| serde_json::to_vec(b).unwrap_or_default()),
        };
        let reply = self.api.send(&request)?;
        if ok.contains(&reply.status) {
            return Ok(reply);
        }
        match reply.status {
            404 => Err(K8sError::NotFound),
            401 | 403 => Err(K8sError::Forbidden(format!("status {}", reply.status))),
            status => {
                let text = String::from_utf8_lossy(&reply.body);
                let text: String = text.chars().take(200).collect();
                Err(K8sError::Protocol(format!(
                    "{method}: status {status} {text}"
                )))
            }
        }
    }

    fn parse_pod(reply: &Reply) -> Result<PodDoc, K8sError> {
        serde_json::from_slice(&reply.body)
            .map_err(|e| K8sError::Protocol(format!("pod json: {e}")))
    }

    fn create(&self, manifest: &serde_json::Value) -> Result<PodDoc, K8sError> {
        let reply = self.call(
            "POST",
            self.pods_path(),
            "application/json",
            Some(manifest),
            &[201],
        )?;
        Self::parse_pod(&reply)
    }

    fn get(&self, name: &str) -> Result<PodDoc, K8sError> {
        let reply = self.call(
            "GET",
            format!("{}/{name}", self.pods_path()),
            "",
            None,
            &[200],
        )?;
        Self::parse_pod(&reply)
    }

    fn lift_gate(&self, name: &str) -> Result<(), K8sError> {
        let patch = json!([{"op": "remove", "path": "/spec/schedulingGates"}]);
        self.call(
            "PATCH",
            format!("{}/{name}", self.pods_path()),
            "application/json-patch+json",
            Some(&patch),
            &[200],
        )
        .map(drop)
    }

    fn delete(&self, name: &str, grace: u32) -> Result<(), K8sError> {
        match self.call(
            "DELETE",
            format!("{}/{name}?gracePeriodSeconds={grace}", self.pods_path()),
            "",
            None,
            &[200, 202],
        ) {
            Err(K8sError::NotFound) => Ok(()),
            other => other.map(drop),
        }
    }

    fn list(&self, selector: &[(String, String)]) -> Result<Vec<PodDoc>, K8sError> {
        let sel = selector
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(",");
        let reply = self.call(
            "GET",
            format!(
                "{}?labelSelector={}",
                self.pods_path(),
                selector_encode(&sel)
            ),
            "",
            None,
            &[200],
        )?;
        #[derive(serde::Deserialize)]
        struct List {
            items: Vec<PodDoc>,
        }
        let list: List = serde_json::from_slice(&reply.body)
            .map_err(|e| K8sError::Protocol(format!("list json: {e}")))?;
        Ok(list.items)
    }

    fn label_filter(&self, ctx: &AttemptContext) -> Vec<(String, String)> {
        vec![
            (LABEL_OWNER.to_owned(), self.cfg.owner.clone()),
            (LABEL_TENANT.to_owned(), self.cfg.tenant.clone()),
            (LABEL_WORK_ID.to_owned(), ctx.job_id.as_str().to_owned()),
            (LABEL_EPOCH.to_owned(), ctx.lease_epoch.to_string()),
            (LABEL_ATTEMPT.to_owned(), ctx.lease_epoch.to_string()),
        ]
    }

    /// Checks a live pod against a persisted identity.
    fn verify(&self, doc: &PodDoc, identity: &PodInstance) -> Result<OwnerLabels, String> {
        if doc.metadata.uid != identity.pod_uid {
            return Err("pod uid differs (the pod was recreated)".into());
        }
        if identity.namespace != self.cfg.namespace || doc.metadata.name != identity.container {
            return Err("pod is not the recorded one".into());
        }
        let owner = OwnerLabels::from_map(&doc.metadata.labels).map_err(|e| e.to_string())?;
        if owner.owner != self.cfg.owner || owner.tenant != self.cfg.tenant {
            return Err("pod belongs to another owner or tenant".into());
        }
        Ok(owner)
    }

    fn identity_of(&self, doc: &PodDoc) -> Result<PodInstance, K8sError> {
        PodInstance::new(&self.cfg.namespace, &doc.metadata.uid, &doc.metadata.name)
            .map_err(|e| K8sError::Protocol(format!("pod identity: {e}")))
    }
}

fn profile_name(profile: SandboxProfileName) -> String {
    serde_json::to_value(profile)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

fn exit_outcome(code: Option<i64>) -> ExitOutcome {
    code.and_then(|c| i32::try_from(c).ok())
        .map_or(ExitOutcome::Unknown, ExitOutcome::Exited)
}

struct PodControl {
    inner: Arc<Inner>,
    name: String,
    cancelled: AtomicBool,
}

impl AttemptControl for PodControl {
    fn cancel(&self) {
        if self.cancelled.swap(true, Ordering::SeqCst) {
            return;
        }
        let inner = Arc::clone(&self.inner);
        let name = self.name.clone();
        let _ = std::thread::Builder::new()
            .name("harw-k8s-cancel".into())
            .spawn(move || {
                let _ = inner.delete(&name, inner.cfg.stop_grace_secs);
            });
    }
}

fn pump(mut reader: Box<dyn Read + Send>, tx: &AttemptEventSender) {
    let mut buf = vec![0u8; LOG_CHUNK];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => {
                if !tx.send_blocking(AttemptEvent::Stdout(buf[..n].to_vec())) {
                    return;
                }
            }
            Err(error) => {
                let _ = tx.send_blocking(AttemptEvent::Error(format!("log stream: {error}")));
                return;
            }
        }
    }
}

fn supervise(inner: &Arc<Inner>, name: &str, follow_from_now: bool) -> AttemptRun {
    let (tx, events) = AttemptEvents::channel(EVENT_CAPACITY);
    let control = Arc::new(PodControl {
        inner: Arc::clone(inner),
        name: name.to_owned(),
        cancelled: AtomicBool::new(false),
    });
    let inner = Arc::clone(inner);
    let name = name.to_owned();
    let sender = tx.clone();
    let spawned = std::thread::Builder::new()
        .name("harw-k8s-supervisor".into())
        .spawn(move || {
            let (done_tx, done_rx) = mpsc::channel::<()>();
            let mut done_tx = Some(done_tx);
            let mut logs_started = false;
            let mut failures = 0u32;
            let outcome = loop {
                match inner.get(&name) {
                    Ok(doc) => {
                        failures = 0;
                        let pending = doc.status.phase.as_deref() == Some("Pending");
                        if !pending && !logs_started {
                            logs_started = true;
                            let path = format!(
                                "{}/{name}/log?follow=true{}",
                                inner.pods_path(),
                                if follow_from_now { "&tailLines=0" } else { "" }
                            );
                            match inner.api.stream(&path) {
                                Ok(reader) => {
                                    if let Some(done) = done_tx.take() {
                                        let pump_tx = sender.clone();
                                        let _ = std::thread::Builder::new()
                                            .name("harw-k8s-logs".into())
                                            .spawn(move || {
                                                pump(reader, &pump_tx);
                                                let _ = done.send(());
                                            });
                                    }
                                }
                                Err(error) => {
                                    let _ = sender.send_blocking(AttemptEvent::Error(format!(
                                        "logs: {error}"
                                    )));
                                }
                            }
                        }
                        if doc.is_terminal() {
                            break exit_outcome(doc.exit_code());
                        }
                    }
                    Err(K8sError::NotFound) => break ExitOutcome::Unknown,
                    Err(error) => {
                        failures += 1;
                        if failures >= MAX_POLL_FAILURES {
                            let _ = sender.send_blocking(AttemptEvent::Error(format!(
                                "status polling: {error}"
                            )));
                            break ExitOutcome::Unknown;
                        }
                    }
                }
                std::thread::sleep(inner.cfg.poll_interval);
            };
            if logs_started {
                let _ = done_rx.recv_timeout(LOG_DRAIN);
            }
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

/// A created, gated, read-back pod.
pub(crate) struct Prepared {
    pub(crate) name: String,
    pub(crate) identity: PodInstance,
    pub(crate) report: harw_job_core::SandboxReport,
}

impl K8sExecutor {
    /// Creates the pod behind its scheduling gate and reads the admitted pod
    /// back. Nothing is scheduled; a failed internal check deletes the pod.
    pub(crate) fn create_and_readback(
        &self,
        spec: &JobSpec,
        ctx: &AttemptContext,
    ) -> Result<Prepared, RuntimeError> {
        spec.validate()?;
        let inner = &self.inner;
        let cfg = &inner.cfg;
        let image = cfg.image_for(spec.sandbox_profile).ok_or_else(|| {
            K8sError::Unsupported(format!(
                "no image configured for profile `{}`",
                profile_name(spec.sandbox_profile)
            ))
        })?;
        let owner_labels = OwnerLabels::new(
            &cfg.owner,
            ctx.job_id.as_str(),
            ctx.lease_epoch,
            ctx.lease_epoch,
            &cfg.tenant,
            &profile_name(spec.sandbox_profile),
        )
        .map_err(|e| K8sError::Config(format!("labels: {e}")))?;
        let labels = pod::k8s_labels(&owner_labels)?;
        let name = pod::pod_name(ctx.job_id.as_str(), ctx.lease_epoch);
        let requested = Requested {
            network: pod::intent(spec.sandbox_profile),
            memory: spec.resources.memory_max,
            inexpressible_limits: spec.resources.cpu_weight.is_some()
                || spec.resources.pids_max.is_some(),
        };
        let manifest = pod::manifest(cfg, spec, &name, &image.reference(), &labels)?;

        let admitted = inner.create(&manifest)?;
        let discard = |inner: &Inner| {
            let _ = inner.delete(&name, 0);
        };
        let checks = || -> Result<(), K8sError> {
            if admitted.metadata.name != name || admitted.metadata.uid.is_empty() {
                return Err(K8sError::Protocol("API returned another pod".into()));
            }
            if admitted.spec.scheduling_gates.is_empty() {
                // A webhook removed the gate: the pod may already run.
                return Err(K8sError::Protocol(
                    "scheduling gate was removed on admission".into(),
                ));
            }
            let container = admitted.spec.containers.first();
            if container.map(|c| c.image.as_str()) != Some(image.reference().as_str()) {
                return Err(K8sError::Protocol("admitted pod uses another image".into()));
            }
            if admitted.metadata.labels != labels {
                return Err(K8sError::Protocol(
                    "admitted pod lost the ownership labels".into(),
                ));
            }
            if admitted.spec.automount_service_account_token != Some(false) {
                return Err(K8sError::Protocol(
                    "service-account token would be mounted".into(),
                ));
            }
            Ok(())
        };
        if let Err(error) = checks() {
            discard(inner);
            return Err(error.into());
        }
        let report = report_from_pod(&admitted, &requested, cfg.network_attested);
        let identity = match inner.identity_of(&admitted) {
            Ok(identity) => identity,
            Err(error) => {
                discard(inner);
                return Err(error.into());
            }
        };
        Ok(Prepared {
            name,
            identity,
            report,
        })
    }

    pub(crate) fn config(&self) -> &K8sConfig {
        &self.inner.cfg
    }

    pub(crate) fn delete_now(&self, name: &str) {
        let _ = self.inner.delete(name, 0);
    }
}

impl Executor for K8sExecutor {
    type Identity = PodInstance;

    fn start(
        &self,
        spec: &JobSpec,
        ctx: &AttemptContext,
    ) -> Result<StartedAttempt<Self::Identity>, RuntimeError> {
        let prepared = self.create_and_readback(spec, ctx)?;
        let inner = &self.inner;
        let name = prepared.name.clone();
        let discard = || {
            let _ = inner.delete(&name, 0);
        };
        if let Err(error) = check_requirement(spec.sandbox, &prepared.report) {
            discard();
            return Err(error);
        }
        if let Err(error) = inner.lift_gate(&name) {
            discard();
            return Err(error.into());
        }
        let run = supervise(inner, &name, false);
        Ok(StartedAttempt {
            identity: Some(prepared.identity),
            sandbox: Some(prepared.report),
            pid: None,
            run,
        })
    }

    fn probe(&self, identity: &Self::Identity) -> Result<Probe, RuntimeError> {
        let doc = match self.inner.get(&identity.container) {
            Ok(doc) => doc,
            Err(K8sError::NotFound) => return Ok(Probe::Exited),
            Err(error) => return Err(error.into()),
        };
        if let Err(detail) = self.inner.verify(&doc, identity) {
            return Ok(Probe::Mismatch(detail));
        }
        Ok(if doc.is_terminal() {
            Probe::Exited
        } else {
            Probe::Alive
        })
    }

    fn reattach(
        &self,
        identity: &Self::Identity,
        ctx: &AttemptContext,
    ) -> Result<AttemptRun, RuntimeError> {
        let doc = match self.inner.get(&identity.container) {
            Ok(doc) => doc,
            Err(K8sError::NotFound) => return Err(RuntimeError::ProcessAlreadyExited { pid: 0 }),
            Err(error) => return Err(error.into()),
        };
        let owner = self
            .inner
            .verify(&doc, identity)
            .map_err(|detail| RuntimeError::IdentityMismatch { pid: 0, detail })?;
        if owner.work_id != ctx.job_id.as_str() || owner.lease_epoch != ctx.lease_epoch {
            return Err(RuntimeError::IdentityMismatch {
                pid: 0,
                detail: "pod belongs to another job or lease epoch".into(),
            });
        }
        if doc.is_terminal() {
            return Err(RuntimeError::ProcessAlreadyExited { pid: 0 });
        }
        Ok(supervise(&self.inner, &identity.container, true))
    }

    fn recorded_exit(&self, ctx: &AttemptContext) -> Option<ExitOutcome> {
        let pods = self.inner.list(&self.inner.label_filter(ctx)).ok()?;
        let [doc] = pods.as_slice() else {
            return None;
        };
        if !doc.is_terminal() {
            return None;
        }
        doc.exit_code().map(|c| exit_outcome(Some(c)))
    }

    fn finished(&self, ctx: &AttemptContext) {
        if let Ok(pods) = self.inner.list(&self.inner.label_filter(ctx)) {
            for doc in pods {
                let _ = self.inner.delete(&doc.metadata.name, 0);
            }
        }
    }
}
