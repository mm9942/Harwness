//! Bounded admission for Onion rendezvous circuits and byte streams.

use std::future::Future;
use std::sync::Arc;

use futures::{Stream, StreamExt};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use tokio::task::JoinSet;
use tor_cell::relaycell::msg::Connected;
use tor_hsservice::{RendRequest, StreamRequest};
use tor_proto::stream::IncomingStreamRequest;
use tracing::debug;

use super::AcceptLimits;
use super::health::{OnionServiceStatus, report};
use crate::OnionStream;

/// State shared by the accept loop and all circuit tasks.
pub(super) struct AcceptCtx<H> {
    pub(super) handler: H,
    pub(super) limits: AcceptLimits,
    pub(super) virtual_port: u16,
    pub(super) stream_slots: Arc<Semaphore>,
}

/// Why a stream request closes its circuit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Refusal {
    /// BEGIN_DIR, RESOLVE or another non-BEGIN request.
    NotBegin,
    /// BEGIN to a port other than the virtual port.
    WrongPort,
    /// The circuit already carries `max_streams_per_circuit` live streams.
    CircuitFull,
}

impl Refusal {
    fn as_str(self) -> &'static str {
        match self {
            Self::NotBegin => "not a BEGIN request",
            Self::WrongPort => "wrong virtual port",
            Self::CircuitFull => "per-circuit stream limit reached",
        }
    }
}

/// Pure per-circuit admission rule. `begin_port` is `Some(port)` for a BEGIN
/// request and `None` for anything else.
pub(super) fn stream_verdict(
    begin_port: Option<u16>,
    virtual_port: u16,
    live_streams: usize,
    max_streams_per_circuit: u32,
) -> Result<(), Refusal> {
    let port = begin_port.ok_or(Refusal::NotBegin)?;
    if port != virtual_port {
        return Err(Refusal::WrongPort);
    }
    let max = usize::try_from(max_streams_per_circuit).unwrap_or(usize::MAX);
    if live_streams >= max {
        return Err(Refusal::CircuitFull);
    }
    Ok(())
}

pub(super) async fn accept_loop<S, H, F>(
    rend_requests: S,
    ctx: Arc<AcceptCtx<H>>,
    health: watch::Sender<OnionServiceStatus>,
) where
    S: Stream<Item = RendRequest> + Send + 'static,
    H: Fn(OnionStream) -> F + Send + Sync + 'static,
    F: Future<Output = ()> + Send + 'static,
{
    let mut rend_requests = Box::pin(rend_requests);
    let circuit_slots = Arc::new(Semaphore::new(
        ctx.limits.max_circuits.min(Semaphore::MAX_PERMITS),
    ));
    let mut circuits = JoinSet::new();
    loop {
        tokio::select! {
            next = rend_requests.next() => {
                let Some(rend) = next else { break };
                match Arc::clone(&circuit_slots).try_acquire_owned() {
                    Ok(permit) => {
                        circuits.spawn(serve_circuit(rend, permit, Arc::clone(&ctx)));
                    }
                    Err(_) => {
                        debug!("onion circuit limit reached; rejecting rendezvous request");
                        let _ = rend.reject().await;
                    }
                }
            }
            Some(_) = circuits.join_next(), if !circuits.is_empty() => {}
        }
    }
    report(&health, OnionServiceStatus::AcceptLoopEnded);
    while circuits.join_next().await.is_some() {}
}

async fn serve_circuit<H, F>(
    rend: RendRequest,
    _circuit_permit: OwnedSemaphorePermit,
    ctx: Arc<AcceptCtx<H>>,
) where
    H: Fn(OnionStream) -> F + Send + Sync + 'static,
    F: Future<Output = ()> + Send + 'static,
{
    let rendezvous = tokio::time::timeout(ctx.limits.rendezvous_timeout, rend.accept()).await;
    let mut requests = match rendezvous {
        Ok(Ok(requests)) => requests,
        Ok(Err(e)) => {
            debug!(error = %e, "onion rendezvous failed");
            return;
        }
        Err(_) => {
            debug!("onion rendezvous timed out");
            return;
        }
    };
    debug!("onion circuit established");
    // `requests` keeps the circuit open; returning drops it (and aborts the
    // circuit's handlers via `handlers`).
    let mut handlers = JoinSet::new();
    loop {
        let idle = handlers.is_empty();
        tokio::select! {
            next = requests.next() => {
                let Some(request) = next else { break };
                while handlers.try_join_next().is_some() {}
                if !admit_stream(&ctx, &mut handlers, request).await {
                    return;
                }
            }
            Some(_) = handlers.join_next(), if !handlers.is_empty() => {}
            () = tokio::time::sleep(ctx.limits.circuit_idle), if idle => {
                debug!("onion circuit idle; closing");
                return;
            }
        }
    }
    debug!("onion circuit closed by peer");
    while handlers.join_next().await.is_some() {}
}

/// Accepts one stream request or shuts the circuit down. Returns `false`
/// when the circuit was shut down.
async fn admit_stream<H, F>(
    ctx: &AcceptCtx<H>,
    handlers: &mut JoinSet<()>,
    request: StreamRequest,
) -> bool
where
    H: Fn(OnionStream) -> F + Send + Sync + 'static,
    F: Future<Output = ()> + Send + 'static,
{
    let begin_port = match request.request() {
        IncomingStreamRequest::Begin(begin) => Some(begin.port()),
        _ => None,
    };
    let admitted = stream_verdict(
        begin_port,
        ctx.virtual_port,
        handlers.len(),
        ctx.limits.max_streams_per_circuit,
    )
    .map_err(Refusal::as_str)
    .and_then(|()| {
        Arc::clone(&ctx.stream_slots)
            .try_acquire_owned()
            .map_err(|_| "global stream limit reached")
    });
    let stream_permit = match admitted {
        Ok(permit) => permit,
        Err(reason) => {
            debug!(reason, "closing onion circuit");
            let _ = request.shutdown_circuit();
            return false;
        }
    };
    match request.accept(Connected::new_empty()).await {
        Ok(data_stream) => {
            let stream: OnionStream = Box::pin(data_stream);
            let work = (ctx.handler)(stream);
            handlers.spawn(async move {
                work.await;
                drop(stream_permit);
            });
            debug!("onion stream accepted");
        }
        Err(e) => debug!(error = %e, "onion stream accept failed"),
    }
    true
}
