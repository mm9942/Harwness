use std::net::{IpAddr, SocketAddr};
use std::time::Instant;

use bytes::{BufMut, Bytes, BytesMut};
use harw_onion::OnionLocator;

use super::{
    AdmissionError, DNS_V4, DNS_V6, Destination, DialTicket, DnsLeases, FlowKey, Packet,
    PacketError, Policy, Transport, VpnSession,
};

#[derive(Debug)]
pub enum PacketAction {
    Reply(Bytes),
    ExistingTcp(FlowKey),
    LocalDnsTcp(FlowKey, bool),
    Dial(DialTicket, Destination),
    Drop,
}

pub struct PacketRouter {
    session: VpnSession,
    dns: DnsLeases,
}

impl PacketRouter {
    pub fn new(policy: Policy) -> Self {
        Self {
            session: VpnSession::new(policy),
            dns: DnsLeases::default(),
        }
    }

    pub fn session(&mut self) -> &mut VpnSession {
        &mut self.session
    }

    /// DNS-over-TCP actors pass each unframed message to this local resolver.
    pub fn resolve_dns_message(&mut self, message: &[u8]) -> Option<Bytes> {
        self.dns.answer(message)
    }

    pub fn ingress(
        &mut self,
        bytes: &[u8],
        now: Instant,
        owner: impl FnOnce(FlowKey) -> Option<u32>,
    ) -> Result<PacketAction, PacketError> {
        let packet = Packet::parse(bytes)?;
        verify_checksums(bytes)?;
        let flow = packet.flow;
        if is_local_dns(flow.destination) {
            return Ok(match packet.transport {
                Transport::Udp { payload } => self
                    .dns
                    .answer(payload)
                    .map(|message| PacketAction::Reply(udp_reply(flow, &message)))
                    .unwrap_or(PacketAction::Drop),
                Transport::Tcp { initial_syn } => PacketAction::LocalDnsTcp(flow, initial_syn),
            });
        }

        let Transport::Tcp { initial_syn } = packet.transport else {
            return Ok(PacketAction::Drop);
        };
        if self.session.contains(flow) {
            return Ok(PacketAction::ExistingTcp(flow));
        }
        if !initial_syn {
            return Ok(PacketAction::Drop);
        }
        let Some(destination) = self.destination(flow.destination) else {
            return Ok(PacketAction::Drop);
        };
        Ok(
            match self.session.admit(flow, owner(flow), &destination, now) {
                Ok(ticket) => PacketAction::Dial(ticket, destination),
                Err(AdmissionError::AlreadyTracked) => PacketAction::ExistingTcp(flow),
                Err(_) => PacketAction::Drop,
            },
        )
    }

    fn destination(&self, address: SocketAddr) -> Option<Destination> {
        if let Some(host) = self.dns.lookup(address.ip()) {
            if host.ends_with(".onion") {
                return OnionLocator::new(host, address.port())
                    .ok()
                    .map(Destination::Onion);
            }
            return exit_port(address.port()).then(|| Destination::PublicName {
                host: host.to_owned(),
                port: address.port(),
            });
        }
        (exit_port(address.port()) && public_ip(address.ip()))
            .then_some(Destination::PublicIp(address))
    }
}

fn is_local_dns(address: SocketAddr) -> bool {
    address.port() == 53 && matches!(address.ip(), IpAddr::V4(DNS_V4) | IpAddr::V6(DNS_V6))
}

fn exit_port(port: u16) -> bool {
    !matches!(port, 0 | 53 | 853)
}

fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(address) => {
            let [a, b, c, _] = address.octets();
            (1..224).contains(&a)
                && !address.is_private()
                && !address.is_loopback()
                && !address.is_link_local()
                && !address.is_documentation()
                && !matches!(
                    (a, b, c),
                    (100, 64..=127, _) | (192, 0, 0) | (198, 18..=19, _)
                )
        }
        IpAddr::V6(address) => {
            let [first, second, ..] = address.segments();
            first & 0xe000 == 0x2000
                && !(first == 0x2001 && (second <= 0x01ff || second == 0x0db8))
                && first != 0x2002
                && first != 0x3fff
        }
    }
}

fn verify_checksums(bytes: &[u8]) -> Result<(), PacketError> {
    // Packet::parse has already checked the minimum lengths and next-header.
    let (header_len, addresses, protocol) = if bytes[0] >> 4 == 4 {
        if checksum(&[&bytes[..20]]) != 0 {
            return Err(PacketError::Malformed);
        }
        (20, &bytes[12..20], bytes[9])
    } else {
        (40, &bytes[8..40], bytes[6])
    };
    let segment = &bytes[header_len..];
    if header_len == 20 && protocol == 17 && segment[6..8] == [0, 0] {
        return Ok(());
    }
    let length = (segment.len() as u16).to_be_bytes();
    if checksum(&[addresses, &[0, protocol], &length, segment]) == 0 {
        Ok(())
    } else {
        Err(PacketError::Malformed)
    }
}

fn udp_reply(flow: FlowKey, message: &[u8]) -> Bytes {
    let length = u16::try_from(8 + message.len()).expect("DNS response is bounded by one question");
    let (header_len, mut packet) = match (flow.destination.ip(), flow.source.ip()) {
        (IpAddr::V4(source), IpAddr::V4(destination)) => {
            let mut packet = BytesMut::with_capacity(20 + usize::from(length));
            packet.put_u16(0x4500);
            packet.put_u16(20 + length);
            packet.put_u32(0x0000_4000);
            packet.put_u16(0x4011);
            packet.put_u16(0);
            packet.extend_from_slice(&source.octets());
            packet.extend_from_slice(&destination.octets());
            let sum = checksum(&[&packet]);
            packet[10..12].copy_from_slice(&sum.to_be_bytes());
            (20, packet)
        }
        (IpAddr::V6(source), IpAddr::V6(destination)) => {
            let mut packet = BytesMut::with_capacity(40 + usize::from(length));
            packet.put_u32(0x6000_0000);
            packet.put_u16(length);
            packet.put_u16(0x1140);
            packet.extend_from_slice(&source.octets());
            packet.extend_from_slice(&destination.octets());
            (40, packet)
        }
        _ => unreachable!("a TUN packet has endpoints in one address family"),
    };
    packet.put_u16(flow.destination.port());
    packet.put_u16(flow.source.port());
    packet.put_u16(length);
    packet.put_u16(0);
    packet.extend_from_slice(message);
    let addresses = if header_len == 20 {
        &packet[12..20]
    } else {
        &packet[8..40]
    };
    // The omitted high words in the IPv6 pseudoheader are zero at this MTU.
    let sum = match checksum(&[
        addresses,
        &[0, 17],
        &length.to_be_bytes(),
        &packet[header_len..],
    ]) {
        0 => u16::MAX,
        sum => sum,
    };
    packet[header_len + 6..header_len + 8].copy_from_slice(&sum.to_be_bytes());
    packet.freeze()
}

pub(super) fn checksum(parts: &[&[u8]]) -> u16 {
    let mut sum = 0_u32;
    let mut pairs = parts.iter().flat_map(|part| part.iter().copied());
    while let Some(high) = pairs.next() {
        sum += u32::from(u16::from_be_bytes([high, pairs.next().unwrap_or(0)]));
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vpn::{RouteMode, Scope};
    use std::net::Ipv4Addr;

    #[test]
    fn only_synthetic_dns_reaches_local_resolver() {
        let app = SocketAddr::new(Ipv4Addr::new(10, 8, 0, 2).into(), 49152);
        let query = [
            0x12, 0x34, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 7, b'e', b'x', b'a', b'm', b'p', b'l', b'e',
            3, b'c', b'o', b'm', 0, 0, 1, 0, 1,
        ];
        let packet = |server| {
            udp_reply(
                FlowKey {
                    source: SocketAddr::new(server, 53),
                    destination: app,
                },
                &query,
            )
        };
        let mut router = PacketRouter::new(Policy::new(
            Scope::DeviceWide(RouteMode::OnionOnly),
            Default::default(),
        ));
        let now = Instant::now();
        assert!(matches!(
            router.ingress(
                &packet(Ipv4Addr::new(1, 1, 1, 1).into()),
                now,
                |_| unreachable!()
            ),
            Ok(PacketAction::Drop)
        ));
        let PacketAction::Reply(reply) = router
            .ingress(&packet(DNS_V4.into()), now, |_| unreachable!())
            .unwrap()
        else {
            panic!("missing DNS response");
        };
        let mut corrupt = packet(DNS_V4.into()).to_vec();
        *corrupt.last_mut().unwrap() ^= 1;
        assert!(matches!(
            router.ingress(&corrupt, now, |_| unreachable!()),
            Err(PacketError::Malformed)
        ));
        let parsed = Packet::parse(&reply).unwrap();
        assert_eq!(parsed.flow.source, SocketAddr::new(DNS_V4.into(), 53));
        assert_eq!(parsed.flow.destination, app);
        assert_eq!(checksum(&[&reply[..20]]), 0);
        assert_eq!(
            checksum(&[&reply[12..20], &[0, 17], &reply[24..26], &reply[20..]]),
            0
        );
        assert!(matches!(
            router.destination(([198, 18, 0, 1], 443).into()),
            Some(Destination::PublicName { host, port: 443 }) if host == "example.com"
        ));
        assert!(router.destination(([198, 18, 0, 2], 443).into()).is_none());
    }
}
