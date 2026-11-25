use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use super::TUN_MTU;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PacketError {
    Malformed,
    Unsupported,
    OverMtu,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FlowKey {
    /// Original application endpoint for Android's UID lookup.
    pub source: SocketAddr,
    pub destination: SocketAddr,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Transport<'a> {
    Tcp { initial_syn: bool },
    Udp { payload: &'a [u8] },
}

#[derive(Debug, Eq, PartialEq)]
pub struct Packet<'a> {
    pub flow: FlowKey,
    pub transport: Transport<'a>,
}

impl<'a> Packet<'a> {
    /// Parse an unframed TUN packet. The TCP stack still validates TCP checksums.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, PacketError> {
        if bytes.len() > TUN_MTU {
            return Err(PacketError::OverMtu);
        }
        match bytes.first().map(|version| version >> 4) {
            Some(4) => Self::ipv4(bytes),
            Some(6) => Self::ipv6(bytes),
            _ => Err(PacketError::Malformed),
        }
    }

    fn ipv4(bytes: &'a [u8]) -> Result<Self, PacketError> {
        let header = bytes.get(..20).ok_or(PacketError::Malformed)?;
        let header_words = header[0] & 0x0f;
        if header_words < 5 || usize::from(word(&header[2..4])) != bytes.len() {
            return Err(PacketError::Malformed);
        }
        // Options and fragments require additional state. Reject before UID lookup.
        if header_words != 5 || word(&header[6..8]) & 0xbfff != 0 {
            return Err(PacketError::Unsupported);
        }
        let source = Ipv4Addr::new(header[12], header[13], header[14], header[15]);
        let destination = Ipv4Addr::new(header[16], header[17], header[18], header[19]);
        Self::transport(source.into(), destination.into(), header[9], &bytes[20..])
    }

    fn ipv6(bytes: &'a [u8]) -> Result<Self, PacketError> {
        let header = bytes.get(..40).ok_or(PacketError::Malformed)?;
        let payload = &bytes[40..];
        if usize::from(word(&header[4..6])) != payload.len() {
            return Err(PacketError::Malformed);
        }
        let source = Ipv6Addr::from(<[u8; 16]>::try_from(&header[8..24]).unwrap());
        let destination = Ipv6Addr::from(<[u8; 16]>::try_from(&header[24..40]).unwrap());
        // Only direct TCP and UDP are accepted, so extension headers cannot hide fragments.
        Self::transport(source.into(), destination.into(), header[6], payload)
    }

    fn transport(
        source: IpAddr,
        destination: IpAddr,
        protocol: u8,
        bytes: &'a [u8],
    ) -> Result<Self, PacketError> {
        let transport = match protocol {
            6 => {
                let header = bytes.get(..20).ok_or(PacketError::Malformed)?;
                let header_len = usize::from(header[12] >> 4) * 4;
                if !(20..=bytes.len()).contains(&header_len) {
                    return Err(PacketError::Malformed);
                }
                Transport::Tcp {
                    initial_syn: header[13] & 0x17 == 0x02,
                }
            }
            17 => {
                let header = bytes.get(..8).ok_or(PacketError::Malformed)?;
                if usize::from(word(&header[4..6])) != bytes.len() {
                    return Err(PacketError::Malformed);
                }
                Transport::Udp {
                    payload: &bytes[8..],
                }
            }
            _ => return Err(PacketError::Unsupported),
        };
        let source_port = word(&bytes[..2]);
        let destination_port = word(&bytes[2..4]);
        if source_port == 0 || destination_port == 0 {
            return Err(PacketError::Malformed);
        }
        Ok(Self {
            flow: FlowKey {
                source: SocketAddr::new(source, source_port),
                destination: SocketAddr::new(destination, destination_port),
            },
            transport,
        })
    }
}

fn word(bytes: &[u8]) -> u16 {
    u16::from_be_bytes([bytes[0], bytes[1]])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv4_retains_original_tuple_and_rejects_fragments() {
        let mut packet = [0; 40];
        packet[0] = 0x45;
        packet[3] = 40;
        packet[9] = 6;
        packet[12..20].copy_from_slice(&[10, 8, 0, 2, 198, 18, 0, 1]);
        packet[20..24].copy_from_slice(&[0xc0, 0, 1, 0xbb]);
        packet[32..34].copy_from_slice(&[0x50, 0x02]);
        let parsed = Packet::parse(&packet).unwrap();
        assert_eq!(parsed.flow.source, "10.8.0.2:49152".parse().unwrap());
        assert_eq!(parsed.flow.destination, "198.18.0.1:443".parse().unwrap());
        assert_eq!(parsed.transport, Transport::Tcp { initial_syn: true });
        packet[6] = 0x20;
        assert_eq!(Packet::parse(&packet), Err(PacketError::Unsupported));
    }

    #[test]
    fn ipv6_udp_checks_both_lengths() {
        let mut packet = [0; 49];
        packet[0] = 0x60;
        packet[5] = 9;
        packet[6] = 17;
        packet[40..49].copy_from_slice(&[0xc0, 0, 0, 53, 0, 9, 0, 0, 7]);
        assert_eq!(
            Packet::parse(&packet).unwrap().transport,
            Transport::Udp { payload: &[7] }
        );
        packet[45] = 8;
        assert_eq!(Packet::parse(&packet), Err(PacketError::Malformed));
        assert_eq!(Packet::parse(&packet[..48]), Err(PacketError::Malformed));
    }
}
