use std::collections::HashMap;
use std::future::{Future, pending, poll_fn};
use std::io;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use bytes::{Buf, Bytes};
use harw_onion::OnionStream;
use tokio::io::ReadBuf;
use tokio::task::{AbortHandle, JoinSet};

use super::{
    Destination, DialTicket, FlowKey, Ingress, STREAM_CHUNK_BYTES, StreamRead, TUN_MTU,
    TunnelEngine,
};

/// A packet-oriented TUN adapter. A canceled read must leave the next packet intact.
pub trait PacketIo {
    fn read_packet(&mut self, buffer: &mut [u8]) -> impl Future<Output = io::Result<usize>> + Send;
    fn write_packet(&mut self, packet: &[u8]) -> impl Future<Output = io::Result<()>> + Send;
}

/// Owns the tunnel, all flow data and all pending dials until stop or TUN failure.
pub async fn run_tunnel<T, D, Fut, U, Stop>(
    mut tun: T,
    mut engine: TunnelEngine,
    mut owner: U,
    mut dial: D,
    stop: Stop,
) -> io::Result<()>
where
    T: PacketIo,
    D: FnMut(Destination, Duration) -> Fut,
    Fut: Future<Output = io::Result<OnionStream>> + Send + 'static,
    U: FnMut(FlowKey) -> Option<u32>,
    Stop: Future<Output = ()>,
{
    tokio::pin!(stop);
    let mut dials = JoinSet::new();
    let mut pending_dials: HashMap<FlowKey, (DialTicket, AbortHandle)> = HashMap::new();
    let mut streams = HashMap::new();
    let mut input = [0; TUN_MTU + 1];
    loop {
        let now = Instant::now();
        engine.poll(now);
        while let Some(packet) = engine.egress() {
            tokio::select! {
                () = &mut stop => return Ok(()),
                result = tun.write_packet(&packet) => result?,
            }
        }
        for key in engine.reap() {
            if let Some((_, handle)) = pending_dials.remove(&key) {
                handle.abort();
            }
            streams.remove(&key);
        }

        let delay = engine
            .poll_delay(now)
            .map(|delay| delay.max(Duration::from_millis(1)));
        tokio::select! {
            () = &mut stop => return Ok(()),
            count = tun.read_packet(&mut input) => {
                let count = count?;
                if count == 0 {
                    return Err(io::ErrorKind::UnexpectedEof.into());
                }
                let Some(packet) = input.get(..count) else {
                    return Err(io::ErrorKind::InvalidData.into());
                };
                match engine.ingress(packet, Instant::now(), &mut owner) {
                    Ok(Ingress::Reply(packet)) => tokio::select! {
                        () = &mut stop => return Ok(()),
                        result = tun.write_packet(&packet) => result?,
                    },
                    Ok(Ingress::Dial(ticket, destination)) => {
                        let remaining = ticket.deadline.saturating_duration_since(Instant::now());
                        let attempt = dial(destination, remaining);
                        let handle = dials.spawn(async move {
                            let result = tokio::time::timeout_at(ticket.deadline.into(), attempt)
                                .await
                                .unwrap_or_else(|_| Err(io::ErrorKind::TimedOut.into()));
                            (ticket, result)
                        });
                        pending_dials.insert(ticket.flow, (ticket, handle));
                    }
                    _ => {}
                }
            }
            result = dials.join_next(), if !dials.is_empty() => {
                if let Some(Ok((ticket, result))) = result
                    && pending_dials.get(&ticket.flow).is_some_and(|(current, _)| *current == ticket)
                {
                    pending_dials.remove(&ticket.flow);
                    match result {
                        Ok(stream) if engine.dial_connected(ticket, Instant::now()) => {
                            streams.insert(ticket.flow, StreamPipe::new(ticket, stream));
                        }
                        _ => engine.abort(ticket),
                    }
                }
            }
            () = poll_fn(|cx| poll_streams(&mut streams, &mut engine, cx)), if !streams.is_empty() => {}
            () = wait_for_stack(delay) => {}
        }
    }
}

async fn wait_for_stack(delay: Option<Duration>) {
    match delay {
        Some(delay) => tokio::time::sleep(delay).await,
        None => pending().await,
    }
}

struct StreamPipe {
    ticket: DialTicket,
    tor: OnionStream,
    to_tor: Bytes,
    to_app: Bytes,
    app_eof: bool,
    tor_eof: bool,
    tor_write_closed: bool,
    flush_tor: bool,
    app_write_closed: bool,
}

impl StreamPipe {
    fn new(ticket: DialTicket, tor: OnionStream) -> Self {
        Self {
            ticket,
            tor,
            to_tor: Bytes::new(),
            to_app: Bytes::new(),
            app_eof: false,
            tor_eof: false,
            tor_write_closed: false,
            flush_tor: false,
            app_write_closed: false,
        }
    }

    fn pump(&mut self, engine: &mut TunnelEngine, cx: &mut Context<'_>) -> io::Result<bool> {
        let mut progress = false;
        let mut scratch = [0; STREAM_CHUNK_BYTES];
        if self.to_tor.is_empty() && !self.app_eof {
            match engine.read_app(self.ticket, &mut scratch) {
                StreamRead::Data(count) => self.to_tor = Bytes::copy_from_slice(&scratch[..count]),
                StreamRead::Eof => self.app_eof = true,
                StreamRead::Closed => return Err(io::ErrorKind::BrokenPipe.into()),
                StreamRead::Pending => {}
            }
        }
        if !self.to_tor.is_empty() {
            if let Poll::Ready(result) = self.tor.as_mut().poll_write(cx, &self.to_tor) {
                let written = result?;
                if written == 0 {
                    return Err(io::ErrorKind::WriteZero.into());
                }
                self.to_tor.advance(written);
                self.flush_tor = true;
                progress = true;
            }
        }
        if self.flush_tor
            && let Poll::Ready(result) = self.tor.as_mut().poll_flush(cx)
        {
            result?;
            self.flush_tor = false;
        }
        if self.to_tor.is_empty()
            && !self.flush_tor
            && self.app_eof
            && !self.tor_write_closed
            && let Poll::Ready(result) = self.tor.as_mut().poll_shutdown(cx)
        {
            result?;
            self.tor_write_closed = true;
            progress = true;
        }

        if self.to_app.is_empty() && !self.tor_eof {
            let mut buffer = ReadBuf::new(&mut scratch);
            if let Poll::Ready(result) = self.tor.as_mut().poll_read(cx, &mut buffer) {
                result?;
                self.tor_eof = buffer.filled().is_empty();
                self.to_app = Bytes::copy_from_slice(buffer.filled());
            }
        }
        if !self.to_app.is_empty() {
            let written = engine
                .write_app(self.ticket, &self.to_app)
                .ok_or(io::ErrorKind::BrokenPipe)?;
            self.to_app.advance(written);
            progress |= written != 0;
        } else if self.tor_eof && !self.app_write_closed {
            self.app_write_closed = engine.close_app_write(self.ticket);
            progress |= self.app_write_closed;
        }
        Ok(progress)
    }
}

fn poll_streams(
    streams: &mut HashMap<FlowKey, StreamPipe>,
    engine: &mut TunnelEngine,
    cx: &mut Context<'_>,
) -> Poll<()> {
    let mut changed = false;
    streams.retain(|_, pipe| match pipe.pump(engine, cx) {
        Ok(progress) => {
            changed |= progress;
            true
        }
        Err(_) => {
            engine.abort(pipe.ticket);
            changed = true;
            false
        }
    });
    if changed {
        Poll::Ready(())
    } else {
        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vpn::tcp::tests::segment;
    use crate::vpn::{Policy, RouteMode, Scope};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::{mpsc, oneshot};

    struct FakeTun {
        incoming: mpsc::Receiver<Bytes>,
        outgoing: mpsc::Sender<Bytes>,
    }

    impl PacketIo for FakeTun {
        async fn read_packet(&mut self, dst: &mut [u8]) -> io::Result<usize> {
            let packet = self
                .incoming
                .recv()
                .await
                .ok_or(io::ErrorKind::UnexpectedEof)?;
            dst[..packet.len()].copy_from_slice(&packet);
            Ok(packet.len())
        }

        async fn write_packet(&mut self, packet: &[u8]) -> io::Result<()> {
            self.outgoing
                .send(Bytes::copy_from_slice(packet))
                .await
                .map_err(|_| io::ErrorKind::BrokenPipe.into())
        }
    }

    #[tokio::test]
    async fn fake_tun_relays_tcp_payload_both_ways_and_stops() {
        tokio::time::timeout(Duration::from_secs(3), async {
            let (input, incoming) = mpsc::channel(4);
            let (outgoing, mut output) = mpsc::channel(4);
            let (stop, stopped) = oneshot::channel();
            let (tor, mut peer) = tokio::io::duplex(32);
            let mut tor = Some(tor);
            let flow = FlowKey {
                source: ([10, 8, 0, 2], 49152).into(),
                destination: ([1, 1, 1, 1], 443).into(),
            };
            let task = tokio::spawn(run_tunnel(
                FakeTun { incoming, outgoing },
                TunnelEngine::new(Policy::new(Scope::DeviceWide(RouteMode::InternetAndOnion), Default::default()), Instant::now(), 7),
                move |key| {
                    assert_eq!(key, flow);
                    Some(7)
                },
                move |destination, _| {
                    assert!(matches!(destination, Destination::PublicIp(address) if address == flow.destination));
                    let tor: OnionStream = Box::pin(tor.take().unwrap());
                    std::future::ready(Ok(tor))
                },
                async { let _ = stopped.await; },
            ));
            input.send(segment(flow, 100, 0, 2, b"").freeze()).await.unwrap();
            let syn_ack = output.recv().await.unwrap();
            let seq = u32::from_be_bytes(syn_ack[24..28].try_into().unwrap());
            input.send(segment(flow, 101, seq.wrapping_add(1), 0x18, b"hello").freeze()).await.unwrap();
            let mut request = [0; 5];
            peer.read_exact(&mut request).await.unwrap();
            assert_eq!(&request, b"hello");
            peer.write_all(b"world").await.unwrap();

            loop {
                let packet = output.recv().await.unwrap();
                let header = 20 + usize::from(packet[32] >> 4) * 4;
                if packet.len() > header {
                    assert_eq!(&packet[header..], b"world");
                    break;
                }
            }
            stop.send(()).unwrap();
            task.await.unwrap().unwrap();
        }).await.expect("runner must make progress without real network");
    }
}
