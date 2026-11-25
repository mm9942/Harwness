use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::{Buf, BufMut, Bytes, BytesMut};
use smoltcp::socket::tcp::State;
use smoltcp::time::Instant as StackInstant;

use super::{
    Destination, DialTicket, DnsLeases, FlowKey, PacketAction, PacketError, PacketRouter, Policy,
    TcpStack,
};

const DNS_TCP_MESSAGE_MAX: usize = 4096;

#[derive(Debug)]
pub enum Ingress {
    Reply(Bytes),
    Dial(DialTicket, Destination),
    Consumed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamRead {
    Data(usize),
    Pending,
    Eof,
    Closed,
}

#[derive(Default)]
struct DnsTcp {
    input: BytesMut,
    output: Bytes,
}

enum Flow {
    Dialing(DialTicket),
    Open(DialTicket),
    Dns(DnsTcp),
}

/// Single-owner tunnel engine. The platform runner supplies packet I/O, per-socket
/// UIDs, readiness notifications and dial results.
pub struct TunnelEngine {
    epoch: Instant,
    router: PacketRouter,
    tcp: TcpStack,
    flows: HashMap<FlowKey, Flow>,
}

impl TunnelEngine {
    pub fn new(policy: Policy, epoch: Instant, seed: u64) -> Self {
        Self::with_dns(policy, epoch, seed, Arc::default())
    }

    pub fn with_dns(policy: Policy, epoch: Instant, seed: u64, dns: Arc<Mutex<DnsLeases>>) -> Self {
        Self {
            epoch,
            router: PacketRouter::with_dns(policy, dns),
            tcp: TcpStack::new(StackInstant::from_millis(0), seed),
            flows: HashMap::new(),
        }
    }

    pub fn ingress(
        &mut self,
        bytes: &[u8],
        now: Instant,
        owner: impl FnOnce(FlowKey) -> Option<u32>,
    ) -> Result<Ingress, PacketError> {
        match self.router.ingress(bytes, now, owner)? {
            PacketAction::Reply(packet) => return Ok(Ingress::Reply(packet)),
            PacketAction::Dial(ticket, target) => {
                if self.tcp.open(ticket.flow) {
                    if self.tcp.ingress(ticket.flow, bytes) {
                        self.flows.insert(ticket.flow, Flow::Dialing(ticket));
                        return Ok(Ingress::Dial(ticket, target));
                    }
                    self.tcp.remove(ticket.flow);
                }
                self.router.session().close(ticket);
            }
            PacketAction::ExistingTcp(key) => {
                self.tcp.ingress(key, bytes);
            }
            PacketAction::LocalDnsTcp(key, initial_syn) => {
                if initial_syn && !self.flows.contains_key(&key) && self.tcp.open(key) {
                    if let Some(socket) = self.tcp.socket(key) {
                        socket.set_timeout(Some(smoltcp::time::Duration::from_secs(30)));
                    }
                    self.flows.insert(key, Flow::Dns(DnsTcp::default()));
                }
                self.tcp.ingress(key, bytes);
            }
            PacketAction::Drop => {}
        }
        Ok(Ingress::Consumed)
    }

    pub fn dial_connected(&mut self, ticket: DialTicket, now: Instant) -> bool {
        if !self.router.session().connected(ticket, now) {
            return false;
        }
        self.flows.insert(ticket.flow, Flow::Open(ticket));
        true
    }

    pub fn abort(&mut self, ticket: DialTicket) {
        if self.router.session().close(ticket)
            && let Some(socket) = self.tcp.socket(ticket.flow)
        {
            socket.abort();
        }
    }

    pub fn poll(&mut self, now: Instant) {
        for (&key, flow) in &mut self.flows {
            let Some(socket) = self.tcp.socket(key) else {
                continue;
            };
            match flow {
                Flow::Dialing(ticket) if now >= ticket.deadline => {
                    self.router.session().close(*ticket);
                    socket.abort();
                }
                Flow::Dns(dns) => drive_dns(&mut self.router, socket, dns),
                _ => {}
            }
        }
        self.tcp.poll(self.stack_time(now));
    }

    pub fn poll_delay(&mut self, now: Instant) -> Option<Duration> {
        self.tcp
            .poll_delay(self.stack_time(now))
            .map(|delay| Duration::from_millis(delay.total_millis()))
    }

    pub fn egress(&mut self) -> Option<Bytes> {
        self.tcp.egress()
    }

    /// Call after draining egress. Returns removed keys so the runner can drop sockets
    /// or cancel in-flight tasks associated with those flows.
    pub fn reap(&mut self) -> Vec<FlowKey> {
        let mut removed = Vec::new();
        self.flows.retain(|&key, flow| {
            if !self
                .tcp
                .socket(key)
                .is_none_or(|socket| socket.state() == State::Closed)
            {
                return true;
            }
            match flow {
                Flow::Dialing(ticket) | Flow::Open(ticket) => {
                    self.router.session().close(*ticket);
                }
                Flow::Dns(_) => {}
            }
            self.tcp.remove(key);
            removed.push(key);
            false
        });
        removed
    }

    pub fn read_app(&mut self, ticket: DialTicket, dst: &mut [u8]) -> StreamRead {
        let Some(socket) = self.open_socket(ticket) else {
            return StreamRead::Closed;
        };
        if socket.can_recv() {
            return socket
                .recv_slice(dst)
                .map_or(StreamRead::Closed, StreamRead::Data);
        }
        match socket.state() {
            State::Closed => StreamRead::Closed,
            State::CloseWait | State::LastAck | State::Closing | State::TimeWait => StreamRead::Eof,
            _ => StreamRead::Pending,
        }
    }

    pub fn write_app(&mut self, ticket: DialTicket, src: &[u8]) -> Option<usize> {
        let socket = self.open_socket(ticket)?;
        if socket.can_send() {
            socket.send_slice(src).ok()
        } else if matches!(
            socket.state(),
            State::Closed | State::TimeWait | State::LastAck | State::Closing
        ) {
            None
        } else {
            Some(0)
        }
    }

    pub fn close_app_write(&mut self, ticket: DialTicket) -> bool {
        let Some(socket) = self.open_socket(ticket) else {
            return false;
        };
        // Closing a Listen or SynReceived socket would discard the app handshake.
        if !socket.may_send() {
            return false;
        }
        socket.close();
        true
    }

    fn open_socket(
        &mut self,
        ticket: DialTicket,
    ) -> Option<&mut smoltcp::socket::tcp::Socket<'static>> {
        if !matches!(self.flows.get(&ticket.flow), Some(Flow::Open(current)) if *current == ticket)
        {
            return None;
        }
        self.tcp.socket(ticket.flow)
    }

    fn stack_time(&self, now: Instant) -> StackInstant {
        StackInstant::from_millis(
            now.saturating_duration_since(self.epoch)
                .as_millis()
                .min(i64::MAX as u128) as i64,
        )
    }
}

fn drive_dns(
    router: &mut PacketRouter,
    socket: &mut smoltcp::socket::tcp::Socket<'_>,
    dns: &mut DnsTcp,
) {
    if !dns.output.is_empty() {
        if socket.can_send() {
            let written = socket.send_slice(&dns.output).unwrap_or(0);
            dns.output.advance(written);
        }
        return;
    }

    let message_length = loop {
        let target = if dns.input.len() < 2 {
            2
        } else {
            let length = u16::from_be_bytes([dns.input[0], dns.input[1]]) as usize;
            if !(12..=DNS_TCP_MESSAGE_MAX).contains(&length) {
                socket.abort();
                return;
            }
            2 + length
        };
        if target > 2 && dns.input.len() == target {
            break target;
        }
        if !socket.can_recv() {
            if socket.state() == State::CloseWait {
                socket.close();
            }
            return;
        }
        let mut chunk = [0; 1024];
        let count = (target - dns.input.len()).min(chunk.len());
        let count = socket.recv_slice(&mut chunk[..count]).unwrap_or(0);
        if count == 0 {
            return;
        }
        dns.input.extend_from_slice(&chunk[..count]);
    };

    let response = router.resolve_dns_message(&dns.input[2..message_length]);
    dns.input.clear();
    if let Some(response) = response {
        let mut frame = BytesMut::with_capacity(2 + response.len());
        frame.put_u16(response.len() as u16);
        frame.extend_from_slice(&response);
        dns.output = frame.freeze();
        if socket.can_send() {
            let written = socket.send_slice(&dns.output).unwrap_or(0);
            dns.output.advance(written);
        }
    } else {
        socket.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vpn::tcp::tests::segment;
    use crate::vpn::{DNS_V4, RouteMode, Scope};

    #[test]
    fn tcp_dns_runs_inside_engine_and_keeps_uid_lookup_off_local_resolver() {
        let now = Instant::now();
        let mut engine = TunnelEngine::new(
            Policy::new(Scope::DeviceWide(RouteMode::OnionOnly), Default::default()),
            now,
            7,
        );
        let flow = FlowKey {
            source: ([10, 8, 0, 2], 49152).into(),
            destination: std::net::SocketAddr::new(DNS_V4.into(), 53),
        };
        let result = engine.ingress(&segment(flow, 100, 0, 2, b""), now, |_| unreachable!());
        assert!(matches!(result, Ok(Ingress::Consumed)));
        engine.poll(now);
        let syn_ack = engine.egress().unwrap();
        let seq = u32::from_be_bytes(syn_ack[24..28].try_into().unwrap());

        let query = b"\x12\x34\x01\x00\x00\x01\x00\x00\x00\x00\x00\x00\x07example\x03com\x00\x00\x01\x00\x01";
        let mut framed = BytesMut::new();
        framed.put_u16(query.len() as u16);
        framed.extend_from_slice(query);
        let result = engine.ingress(
            &segment(flow, 101, seq.wrapping_add(1), 0x18, &framed),
            now,
            |_| unreachable!(),
        );
        assert!(matches!(result, Ok(Ingress::Consumed)));
        engine.poll(now);
        engine.poll(now);
        let reply = std::iter::from_fn(|| engine.egress())
            .find(|packet| {
                let tcp_header = usize::from(packet[32] >> 4) * 4;
                packet.len() > 20 + tcp_header
            })
            .expect("TCP DNS response");
        let payload = &reply[20 + usize::from(reply[32] >> 4) * 4..];
        assert_eq!(
            u16::from_be_bytes([payload[0], payload[1]]) as usize,
            payload.len() - 2
        );
        assert_eq!(&payload[payload.len() - 4..], &[198, 18, 0, 0]);
    }

    #[test]
    fn public_syn_is_admitted_only_for_uid_with_exit_permission() {
        let now = Instant::now();
        let mut engine = TunnelEngine::new(
            Policy::new(
                Scope::DeviceWide(RouteMode::OnionOnly),
                [(7, RouteMode::InternetAndOnion)].into(),
            ),
            now,
            7,
        );
        let flow = FlowKey {
            source: ([10, 8, 0, 2], 49152).into(),
            destination: ([1, 1, 1, 1], 443).into(),
        };
        let packet = segment(flow, 100, 0, 2, b"");
        assert!(matches!(
            engine.ingress(&packet, now, |_| Some(8)),
            Ok(Ingress::Consumed)
        ));
        assert!(matches!(
            engine.ingress(&packet, now, |_| None),
            Ok(Ingress::Consumed)
        ));
        assert!(matches!(engine.ingress(&packet, now, |tuple| {
            assert_eq!(tuple, flow);
            Some(7)
        }), Ok(Ingress::Dial(_, Destination::PublicIp(address))) if address == flow.destination));
    }
}
