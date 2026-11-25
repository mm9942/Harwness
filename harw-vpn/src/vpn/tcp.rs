use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use bytes::{Bytes, BytesMut};
use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::socket::tcp;
use smoltcp::time::Instant;
use smoltcp::wire::{HardwareAddress, IpCidr};

use super::router::checksum;
use super::{FLOW_CAPACITY, FLOW_DIRECTION_BYTES, FlowKey, Packet, TUN_MTU, Transport};

const STACK_V4: Ipv4Addr = Ipv4Addr::new(198, 19, 255, 253);
const STACK_V6: Ipv6Addr = Ipv6Addr::new(0xfd68, 0x6172, 0x7700, 0, 0, 0, 0, 0xfffd);
const PACKET_QUEUE: usize = 32;

struct Flow {
    handle: SocketHandle,
    port: u16,
}

pub struct TcpStack {
    interface: Interface,
    sockets: SocketSet<'static>,
    device: PacketDevice,
    flows: HashMap<FlowKey, Flow>,
    reverse: [Option<FlowKey>; FLOW_CAPACITY],
}

impl TcpStack {
    pub fn new(now: Instant, random_seed: u64) -> Self {
        let mut device = PacketDevice::default();
        let mut config = Config::new(HardwareAddress::Ip);
        config.random_seed = random_seed;
        let mut interface = Interface::new(config, &mut device, now);
        interface.update_ip_addrs(|addresses| {
            addresses.push(IpCidr::new(STACK_V4.into(), 32)).unwrap();
            addresses.push(IpCidr::new(STACK_V6.into(), 128)).unwrap();
        });
        interface
            .routes_mut()
            .add_default_ipv4_route(STACK_V4)
            .unwrap();
        interface
            .routes_mut()
            .add_default_ipv6_route(STACK_V6)
            .unwrap();
        Self {
            interface,
            sockets: SocketSet::new(Vec::new()),
            device,
            flows: HashMap::with_capacity(FLOW_CAPACITY),
            reverse: [None; FLOW_CAPACITY],
        }
    }

    pub fn open(&mut self, key: FlowKey) -> bool {
        if self.flows.contains_key(&key) {
            return false;
        }
        let Some(index) = self.reverse.iter().position(Option::is_none) else {
            return false;
        };
        let port = 1024 + index as u16;
        let mut socket = tcp::Socket::new(
            tcp::SocketBuffer::new(vec![0; FLOW_DIRECTION_BYTES]),
            tcp::SocketBuffer::new(vec![0; FLOW_DIRECTION_BYTES]),
        );
        let local = SocketAddr::new(stack_address(key.destination.ip()), port);
        if socket.listen(local).is_err() {
            return false;
        }
        let handle = self.sockets.add(socket);
        self.flows.insert(key, Flow { handle, port });
        self.reverse[index] = Some(key);
        true
    }

    /// Queue an already-admitted packet for smoltcp. The original app tuple is
    /// retained outside the socket so several destinations may share an app port.
    pub fn ingress(&mut self, key: FlowKey, packet: &[u8]) -> bool {
        if !matches!(
            Packet::parse(packet),
            Ok(Packet { flow, transport: Transport::Tcp { .. } }) if flow == key
        ) {
            return false;
        }
        let Some(flow) = self.flows.get(&key) else {
            return false;
        };
        if self.device.rx.len() == PACKET_QUEUE {
            return false;
        }
        let mut packet = BytesMut::from(packet);
        rewrite(
            &mut packet,
            SocketAddr::new(stack_address(key.destination.ip()), flow.port),
            false,
        );
        self.device.rx.push_back(packet.freeze());
        true
    }

    pub fn poll(&mut self, now: Instant) {
        self.interface
            .poll(now, &mut self.device, &mut self.sockets);
    }

    pub fn poll_delay(&mut self, now: Instant) -> Option<smoltcp::time::Duration> {
        self.interface.poll_delay(now, &self.sockets)
    }

    pub fn egress(&mut self) -> Option<Bytes> {
        while let Some(mut bytes) = self.device.tx.pop_front() {
            let Ok(Packet {
                flow,
                transport: Transport::Tcp { .. },
            }) = Packet::parse(&bytes)
            else {
                continue;
            };
            let Some(index) = flow.source.port().checked_sub(1024).map(usize::from) else {
                continue;
            };
            match self.reverse.get(index).and_then(|key| *key) {
                Some(key) if flow.destination == key.source => {
                    rewrite(&mut bytes, key.destination, true);
                    return Some(bytes.freeze());
                }
                _ => continue,
            }
        }
        None
    }

    pub fn socket(&mut self, key: FlowKey) -> Option<&mut tcp::Socket<'static>> {
        let flow = self.flows.get(&key)?;
        Some(self.sockets.get_mut(flow.handle))
    }

    /// Release a socket after its final egress is drained.
    pub fn remove(&mut self, key: FlowKey) {
        if let Some(flow) = self.flows.remove(&key) {
            self.sockets.remove(flow.handle);
            self.reverse[usize::from(flow.port - 1024)] = None;
        }
    }
}

fn stack_address(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V4(_) => STACK_V4.into(),
        IpAddr::V6(_) => STACK_V6.into(),
    }
}

fn rewrite(packet: &mut [u8], endpoint: SocketAddr, source: bool) {
    let (header, start, end) = match endpoint.ip() {
        IpAddr::V4(address) => {
            let offset = if source { 12 } else { 16 };
            packet[offset..offset + 4].copy_from_slice(&address.octets());
            packet[10..12].fill(0);
            let sum = checksum(&[&packet[..20]]);
            packet[10..12].copy_from_slice(&sum.to_be_bytes());
            (20, 12, 20)
        }
        IpAddr::V6(address) => {
            let offset = if source { 8 } else { 24 };
            packet[offset..offset + 16].copy_from_slice(&address.octets());
            (40, 8, 40)
        }
    };
    let offset = header + if source { 0 } else { 2 };
    packet[offset..offset + 2].copy_from_slice(&endpoint.port().to_be_bytes());
    packet[header + 16..header + 18].fill(0);
    let length = (packet.len() - header) as u16;
    let sum = checksum(&[
        &packet[start..end],
        &[0, 6],
        &length.to_be_bytes(),
        &packet[header..],
    ]);
    packet[header + 16..header + 18].copy_from_slice(&sum.to_be_bytes());
}

#[derive(Default)]
struct PacketDevice {
    rx: VecDeque<Bytes>,
    tx: VecDeque<BytesMut>,
}

impl Device for PacketDevice {
    type RxToken<'a> = ReadPacket;
    type TxToken<'a> = WritePacket<'a>;

    fn receive(&mut self, _: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        if self.tx.len() == PACKET_QUEUE {
            return None;
        }
        Some((ReadPacket(self.rx.pop_front()?), WritePacket(&mut self.tx)))
    }

    fn transmit(&mut self, _: Instant) -> Option<Self::TxToken<'_>> {
        (self.tx.len() < PACKET_QUEUE).then_some(WritePacket(&mut self.tx))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut capabilities = DeviceCapabilities::default();
        capabilities.medium = Medium::Ip;
        capabilities.max_transmission_unit = TUN_MTU;
        capabilities
    }
}

struct ReadPacket(Bytes);

impl RxToken for ReadPacket {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, read: F) -> R {
        read(&self.0)
    }
}

struct WritePacket<'a>(&'a mut VecDeque<BytesMut>);

impl TxToken for WritePacket<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, write: F) -> R {
        let mut packet = BytesMut::zeroed(len);
        let value = write(&mut packet);
        self.0.push_back(packet);
        value
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    pub(in crate::vpn) fn segment(
        flow: FlowKey,
        seq: u32,
        ack: u32,
        flags: u8,
        payload: &[u8],
    ) -> BytesMut {
        use bytes::BufMut as _;

        let mut bytes = BytesMut::new();
        let length = (20 + payload.len()) as u16;
        match (flow.source.ip(), flow.destination.ip()) {
            (IpAddr::V4(source), IpAddr::V4(destination)) => {
                bytes.put_u16(0x4500);
                bytes.put_u16(20 + length);
                bytes.put_u32(0x0000_4000);
                bytes.put_u32(0x4006_0000);
                bytes.extend_from_slice(&source.octets());
                bytes.extend_from_slice(&destination.octets());
            }
            (IpAddr::V6(source), IpAddr::V6(destination)) => {
                bytes.put_u32(0x6000_0000);
                bytes.put_u16(length);
                bytes.put_u16(0x0640);
                bytes.extend_from_slice(&source.octets());
                bytes.extend_from_slice(&destination.octets());
            }
            _ => unreachable!(),
        }
        bytes.put_u16(flow.source.port());
        bytes.put_u16(flow.destination.port());
        bytes.put_u32(seq);
        bytes.put_u32(ack);
        bytes.put_u8(0x50);
        bytes.put_u8(flags);
        bytes.put_u16(4096);
        bytes.put_u32(0);
        bytes.extend_from_slice(payload);
        rewrite(&mut bytes, flow.destination, false);
        bytes
    }

    #[test]
    fn handshakes_preserve_v4_and_v6_tuples_and_deliver_tcp_payload() {
        let initial = Instant::from_millis(0);
        let mut stack = TcpStack::new(initial, 1);
        for (source, destination) in [
            ("10.8.0.2:47000", "198.18.0.1:443"),
            ("[fd00::2]:47000", "[fd68:6172:7700::1]:443"),
        ] {
            let key = FlowKey {
                source: source.parse().unwrap(),
                destination: destination.parse().unwrap(),
            };
            assert!(stack.open(key));
            assert!(stack.ingress(key, &segment(key, 100, 0, 2, b"")));
            stack.poll(initial);
            let syn_ack = stack.egress().expect("SYN-ACK from stack");
            let parsed = Packet::parse(&syn_ack).unwrap();
            assert_eq!(parsed.flow.source, key.destination);
            assert_eq!(parsed.flow.destination, key.source);
            let header = if key.source.is_ipv4() { 20 } else { 40 };
            assert_eq!(syn_ack[header + 13] & 0x12, 0x12);
            let seq = u32::from_be_bytes(syn_ack[header + 4..header + 8].try_into().unwrap());
            assert!(stack.ingress(key, &segment(key, 101, seq.wrapping_add(1), 0x18, b"hello")));
            stack.poll(initial);
            let mut received = [0; 5];
            assert_eq!(stack.socket(key).unwrap().recv_slice(&mut received), Ok(5));
            assert_eq!(&received, b"hello");
            while stack.egress().is_some() {}
            stack.remove(key);
        }
    }
}
