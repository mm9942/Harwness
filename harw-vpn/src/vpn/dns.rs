use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::Arc;

use bytes::{BufMut, Bytes, BytesMut};
use harw_onion::OnionLocator;

pub const DNS_V4: Ipv4Addr = Ipv4Addr::new(198, 19, 255, 254);
pub const DNS_V6: Ipv6Addr = Ipv6Addr::new(0xfd68, 0x6172, 0x7700, 0, 0, 0, 0, 0xfffe);
const MAX_LEASES: usize = 4096;
pub const DNS_NAMESPACE_COUNT: u8 = 30;
const TTL_SECONDS: u32 = 30;

/// Maps synthetic destinations back to names without consulting another resolver.
/// Slots are never assigned to another name while this instance owns the TUN.
#[derive(Default)]
pub struct DnsLeases {
    namespace: u8,
    index: HashMap<Arc<str>, u16>,
    names: Vec<Arc<str>>,
}

impl DnsLeases {
    /// A namespace is reserved across the service lifetime. A newly established
    /// service must reserve a different namespace until stale answers age out.
    pub fn in_namespace(namespace: u8) -> Option<Self> {
        (namespace < DNS_NAMESPACE_COUNT).then(|| Self {
            namespace,
            ..Self::default()
        })
    }

    /// Handle one DNS message, excluding UDP or TCP framing.
    pub fn answer(&mut self, query: &[u8]) -> Option<Bytes> {
        let header = query.get(..12)?;
        let flags = read_u16(&header[2..]);
        if flags & 0xf800 != 0 || read_u16(&header[4..]) != 1 || header[6..10] != [0; 4] {
            return None;
        }

        let (name, qname_end) = read_name(query)?;
        let end = qname_end + 4;
        let question = query.get(qname_end..end)?;
        let qtype = read_u16(question);
        let class = read_u16(&question[2..]);

        let qname = name.to_ascii_lowercase();
        let (rcode, address) = if class != 1 {
            (4, None)
        } else if !valid_name(&qname) {
            (3, None)
        } else if qtype != 1 && qtype != 28 {
            (0, None)
        } else if let Some(slot) = self.lease(qname) {
            (
                0,
                Some(if qtype == 1 {
                    IpAddr::V4(ipv4(self.namespace, slot))
                } else {
                    IpAddr::V6(ipv6(self.namespace, slot))
                }),
            )
        } else {
            (2, None)
        };

        let mut response = BytesMut::with_capacity(end + 28);
        response.extend_from_slice(&header[..2]);
        response.put_u16(0x8080 | (flags & 0x0100) | rcode);
        response.put_u16(1);
        response.put_u16(u16::from(address.is_some()));
        response.put_u32(0);
        response.extend_from_slice(&query[12..end]);
        if let Some(address) = address {
            response.put_u16(0xc00c);
            response.put_u16(qtype);
            response.put_u16(1);
            response.put_u32(TTL_SECONDS);
            match address {
                IpAddr::V4(address) => {
                    response.put_u16(4);
                    response.extend_from_slice(&address.octets());
                }
                IpAddr::V6(address) => {
                    response.put_u16(16);
                    response.extend_from_slice(&address.octets());
                }
            }
        }
        Some(response.freeze())
    }

    pub fn lookup(&self, address: IpAddr) -> Option<&str> {
        let slot = match address {
            IpAddr::V4(address) => {
                let [198, network @ (18 | 19), high, low] = address.octets() else {
                    return None;
                };
                let offset = usize::from(network - 18) * 65536
                    + usize::from(u16::from_be_bytes([high, low]));
                (offset / MAX_LEASES == usize::from(self.namespace))
                    .then_some(u16::try_from(offset % MAX_LEASES + 1).ok()?)?
            }
            IpAddr::V6(address) => {
                let [0xfd68, 0x6172, 0x7700, 0, 0, 0, namespace, slot] = address.segments() else {
                    return None;
                };
                (namespace == u16::from(self.namespace)).then_some(slot)?
            }
        };
        self.names
            .get(usize::from(slot.checked_sub(1)?))
            .map(AsRef::as_ref)
    }

    fn lease(&mut self, name: String) -> Option<u16> {
        if let Some(slot) = self.index.get(name.as_str()) {
            return Some(*slot);
        }
        if self.names.len() >= MAX_LEASES {
            return None;
        }
        let slot = u16::try_from(self.names.len() + 1).ok()?;
        let name: Arc<str> = name.into();
        self.names.push(name.clone());
        self.index.insert(name, slot);
        Some(slot)
    }
}

fn ipv4(namespace: u8, slot: u16) -> Ipv4Addr {
    let address = usize::from(namespace) * MAX_LEASES + usize::from(slot) - 1;
    let network = 18 + (address >> 16) as u8;
    let [high, low] = (address as u16).to_be_bytes();
    Ipv4Addr::new(198, network, high, low)
}

fn ipv6(namespace: u8, slot: u16) -> Ipv6Addr {
    Ipv6Addr::new(0xfd68, 0x6172, 0x7700, 0, 0, 0, u16::from(namespace), slot)
}

fn read_u16(bytes: &[u8]) -> u16 {
    u16::from_be_bytes([bytes[0], bytes[1]])
}

fn read_name(query: &[u8]) -> Option<(String, usize)> {
    let mut offset = 12;
    let mut name = String::new();
    loop {
        let count = usize::from(*query.get(offset)?);
        offset += 1;
        if count == 0 {
            return Some((name, offset));
        }
        if count > 63 || name.len() + count + usize::from(!name.is_empty()) > 253 {
            return None;
        }
        let label = query.get(offset..offset + count)?;
        offset += count;
        if !label
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
            || label.first() == Some(&b'-')
            || label.last() == Some(&b'-')
        {
            return None;
        }
        if !name.is_empty() {
            name.push('.');
        }
        name.push_str(std::str::from_utf8(label).ok()?);
    }
}

fn valid_name(name: &str) -> bool {
    let Some((_, suffix)) = name.rsplit_once('.') else {
        return false;
    };
    match suffix {
        "onion" => OnionLocator::new(name, 443).is_ok(),
        "local" | "localhost" | "internal" | "invalid" | "test" | "arpa" => false,
        _ => suffix.bytes().any(|byte| byte.is_ascii_alphabetic()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(name: &str, ty: u16) -> Bytes {
        let mut bytes = BytesMut::from(&[0x12, 0x34, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0][..]);
        for label in name.split('.') {
            bytes.put_u8(label.len() as u8);
            bytes.extend_from_slice(label.as_bytes());
        }
        bytes.put_u8(0);
        bytes.put_u16(ty);
        bytes.put_u16(1);
        bytes.freeze()
    }

    #[test]
    fn returns_shared_dual_stack_lease_without_exposing_name_to_network() {
        let mut dns = DnsLeases::default();
        let a = dns.answer(&query("Example.COM", 1)).unwrap();
        let aaaa = dns.answer(&query("example.com", 28)).unwrap();
        assert_eq!(&a[a.len() - 4..], &ipv4(0, 1).octets());
        assert_eq!(&aaaa[aaaa.len() - 16..], &ipv6(0, 1).octets());
        assert_eq!(dns.lookup(ipv4(0, 1).into()), Some("example.com"));
        assert_eq!(dns.lookup(ipv6(0, 1).into()), Some("example.com"));
        assert_eq!(dns.lookup(ipv4(0, 2).into()), None);
    }

    #[test]
    fn answers_onion_and_rejects_bad_names() {
        let mut dns = DnsLeases::default();
        let onion = "2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion";
        let answer = dns.answer(&query(onion, 1)).unwrap();
        assert_eq!(answer[7], 1);
        assert_eq!(dns.lookup(ipv4(0, 1).into()), Some(onion));
        for name in [
            "wrong.onion",
            "device.local",
            "localhost",
            "1.2.3.4",
            "1.0.0.127.in-addr.arpa",
        ] {
            let response = dns.answer(&query(name, 1)).unwrap();
            assert_eq!(response[3] & 0x0f, 3);
        }
    }

    #[test]
    fn replacement_namespace_cannot_reinterpret_old_answers() {
        let mut old = DnsLeases::in_namespace(8).unwrap();
        let mut new = DnsLeases::in_namespace(9).unwrap();
        old.answer(&query("old.example.com", 1)).unwrap();
        new.answer(&query("new.example.com", 1)).unwrap();
        assert_eq!(old.lookup(ipv4(8, 1).into()), Some("old.example.com"));
        assert_eq!(new.lookup(ipv4(9, 1).into()), Some("new.example.com"));
        assert_eq!(new.lookup(ipv4(8, 1).into()), None);
        assert_eq!(new.lookup(ipv6(8, 1).into()), None);
    }

    #[test]
    fn lease_limit_keeps_existing_names_and_refuses_new_ones() {
        let mut dns = DnsLeases::default();
        for index in 0..MAX_LEASES {
            assert!(dns.lease(format!("{index}.example.com")).is_some());
        }
        let answer = dns.answer(&query("overflow.example.com", 28)).unwrap();
        assert_eq!(answer[3] & 0x0f, 2);
        assert_eq!(dns.lease("0.example.com".into()), Some(1));
    }
}
