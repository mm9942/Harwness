//! Klassifikation von IP-Adressen für die Egress-Policy.
//!
//! # Verantwortung
//! [`classify`] ordnet jede `IpAddr` genau einer [`AddrClass`] zu. Grundlage
//! sind die IANA-Register „IPv4/IPv6 Special-Purpose Address Registry“ sowie
//! die bekannten Cloud-Metadaten-Endpunkte. Die Tabelle ist bewusst
//! konservativ (fail closed): Bereiche, die nicht eindeutig globales Unicast
//! sind, landen in [`AddrClass::Reserved`].
//!
//! # Eingebettete IPv4-Adressen
//! - `::ffff:0:0/96` (IPv4-gemappt) und `64:ff9b::/96` (NAT64 Well-Known
//!   Prefix, RFC 6052) werden nach der eingebetteten IPv4-Adresse
//!   klassifiziert — `64:ff9b::808:808` ist öffentlich (DNS64 in reinen
//!   IPv6-Netzen), `64:ff9b::a9fe:a9fe` ist Cloud-Metadaten.
//! - 6to4 (`2002::/16`), Teredo (`2001::/32`), SIIT (`::ffff:0:0:0/96`),
//!   IPv4-kompatibel (`::/96`) und NAT64 Local-Use (`64:ff9b:1::/48`) sind
//!   unabhängig vom Inhalt [`AddrClass::Reserved`].
//!
//! # Nebenläufigkeit
//! Reine Funktionen über `Copy`-Werten; `Send + Sync`, keine Sperren.
//!
//! # Beispiele
//! ```rust
//! use harw_egress::{AddrClass, classify};
//!
//! let metadata: std::net::IpAddr = "169.254.169.254".parse()?;
//! let mapped: std::net::IpAddr = "::ffff:10.0.0.1".parse()?;
//! let public: std::net::IpAddr = "8.8.8.8".parse()?;
//! assert_eq!(classify(metadata), AddrClass::CloudMetadata);
//! assert_eq!(classify(mapped), AddrClass::Private);
//! assert_eq!(classify(public), AddrClass::Public);
//! # Ok::<(), std::net::AddrParseError>(())
//! ```

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Klasse einer IP-Adresse aus Sicht der Egress-Policy.
///
/// # Beschreibung
/// Nur [`AddrClass::Public`] ist ohne Weiteres ein Internet-Ziel. Welche
/// weiteren Klassen eine Policy zulässt, entscheidet
/// [`crate::EgressPolicy::check_addr`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AddrClass {
    /// `127.0.0.0/8`, `::1`.
    Loopback,
    /// RFC 1918: `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`.
    Private,
    /// `169.254.0.0/16`, `fe80::/10` (ohne Metadaten-Endpunkte).
    LinkLocal,
    /// Cloud-Metadaten: `169.254.169.254` (AWS/GCP/Azure/OpenStack),
    /// `169.254.170.2` und `169.254.170.23` (AWS ECS/EKS),
    /// `100.100.100.200` (Alibaba), `fd00:ec2::254` und `fd00:ec2::23` (AWS IPv6).
    CloudMetadata,
    /// Carrier-Grade NAT `100.64.0.0/10` (RFC 6598).
    Cgnat,
    /// Unique Local `fc00::/7` (RFC 4193).
    UniqueLocal,
    /// `224.0.0.0/4`, `ff00::/8`.
    Multicast,
    /// `192.0.2.0/24`, `198.51.100.0/24`, `203.0.113.0/24`, `2001:db8::/32`,
    /// `3fff::/20`.
    Documentation,
    /// `198.18.0.0/15`, `2001:2::/48`.
    Benchmark,
    /// Alles andere, das kein globales Unicast ist (u. a. `0.0.0.0/8`,
    /// `192.0.0.0/24`, `192.88.99.0/24`, `240.0.0.0/4`, `::/96`,
    /// `::ffff:0:0:0/96`, `64:ff9b:1::/48`, `100::/64`, `2001::/23`,
    /// `2002::/16`, `fec0::/10`, jede IPv6 außerhalb `2000::/3`).
    Reserved,
    /// `0.0.0.0`, `::`.
    Unspecified,
    /// `255.255.255.255`.
    Broadcast,
    /// Globales Unicast.
    Public,
}

impl AddrClass {
    /// Liefert `true` genau für [`AddrClass::Public`].
    ///
    /// # Beispiele
    /// ```rust
    /// use harw_egress::AddrClass;
    ///
    /// assert!(AddrClass::Public.is_public());
    /// assert!(!AddrClass::Cgnat.is_public());
    /// ```
    #[must_use]
    pub const fn is_public(self) -> bool {
        matches!(self, Self::Public)
    }

    // Kurzname für `Display` und Logs.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Loopback => "loopback",
            Self::Private => "private",
            Self::LinkLocal => "link-local",
            Self::CloudMetadata => "cloud-metadata",
            Self::Cgnat => "cgnat",
            Self::UniqueLocal => "unique-local",
            Self::Multicast => "multicast",
            Self::Documentation => "documentation",
            Self::Benchmark => "benchmark",
            Self::Reserved => "reserved",
            Self::Unspecified => "unspecified",
            Self::Broadcast => "broadcast",
            Self::Public => "public",
        }
    }
}

impl fmt::Display for AddrClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Klassifiziert eine IP-Adresse.
///
/// # Beschreibung
/// Exakte Metadaten-, Broadcast- und Unspecified-Adressen gehen den
/// Präfixtabellen vor. IPv4-gemappte und NAT64-Adressen (`64:ff9b::/96`)
/// werden nach ihrer eingebetteten IPv4-Adresse klassifiziert; alle übrigen
/// Übergangsbereiche sind [`AddrClass::Reserved`]. Präfixvergleiche laufen auf
/// `u32`/`u128`, ohne nach Rust 1.85 stabilisierte `std::net`-Methoden.
///
/// # Arguments
/// - `addr` (`IpAddr`): die zu prüfende Adresse (Kopie).
///
/// # Returns
/// Die [`AddrClass`] der Adresse.
///
/// # Concurrency
/// Reine Funktion.
///
/// # Examples
/// ```rust
/// use harw_egress::{AddrClass, classify};
///
/// let nat64: std::net::IpAddr = "64:ff9b::7f00:1".parse()?;
/// let sixtofour: std::net::IpAddr = "2002:a00:1::".parse()?;
/// assert_eq!(classify(nat64), AddrClass::Loopback);
/// assert_eq!(classify(sixtofour), AddrClass::Reserved);
/// # Ok::<(), std::net::AddrParseError>(())
/// ```
#[must_use]
pub fn classify(addr: IpAddr) -> AddrClass {
    match addr {
        IpAddr::V4(v4) => classify_v4(v4),
        IpAddr::V6(v6) => classify_v6(v6),
    }
}

// Exakte IPv4-Metadaten-Endpunkte; gehen den Link-Local-/CGNAT-Präfixen vor.
const V4_METADATA: [Ipv4Addr; 4] = [
    Ipv4Addr::new(169, 254, 169, 254),
    Ipv4Addr::new(169, 254, 170, 2),
    Ipv4Addr::new(169, 254, 170, 23),
    Ipv4Addr::new(100, 100, 100, 200),
];

// Exakte IPv6-Metadaten-Endpunkte; gehen `fc00::/7` vor.
const V6_METADATA: [Ipv6Addr; 2] = [
    Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254),
    Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0023),
];

// IPv4-Präfixtabelle (nicht überlappend), nach den exakten Adressen geprüft.
const V4_TABLE: [([u8; 4], u32, AddrClass); 15] = [
    ([0, 0, 0, 0], 8, AddrClass::Reserved),
    ([10, 0, 0, 0], 8, AddrClass::Private),
    ([100, 64, 0, 0], 10, AddrClass::Cgnat),
    ([127, 0, 0, 0], 8, AddrClass::Loopback),
    ([169, 254, 0, 0], 16, AddrClass::LinkLocal),
    ([172, 16, 0, 0], 12, AddrClass::Private),
    ([192, 0, 0, 0], 24, AddrClass::Reserved),
    ([192, 0, 2, 0], 24, AddrClass::Documentation),
    ([192, 88, 99, 0], 24, AddrClass::Reserved),
    ([192, 168, 0, 0], 16, AddrClass::Private),
    ([198, 18, 0, 0], 15, AddrClass::Benchmark),
    ([198, 51, 100, 0], 24, AddrClass::Documentation),
    ([203, 0, 113, 0], 24, AddrClass::Documentation),
    ([224, 0, 0, 0], 4, AddrClass::Multicast),
    ([240, 0, 0, 0], 4, AddrClass::Reserved),
];

// IPv6-Präfixtabelle, erste Übereinstimmung gewinnt (`2001:2::/48` vor
// `2001::/23`). `::`, `::1`, Metadaten, `::ffff:0:0/96` und `64:ff9b::/96`
// sind vorher behandelt.
const V6_TABLE: [([u16; 8], u32, AddrClass); 13] = [
    ([0, 0, 0, 0, 0, 0, 0, 0], 96, AddrClass::Reserved),
    ([0, 0, 0, 0, 0xffff, 0, 0, 0], 96, AddrClass::Reserved),
    ([0x64, 0xff9b, 1, 0, 0, 0, 0, 0], 48, AddrClass::Reserved),
    ([0x100, 0, 0, 0, 0, 0, 0, 0], 64, AddrClass::Reserved),
    ([0x2001, 0x2, 0, 0, 0, 0, 0, 0], 48, AddrClass::Benchmark),
    (
        [0x2001, 0xdb8, 0, 0, 0, 0, 0, 0],
        32,
        AddrClass::Documentation,
    ),
    ([0x3fff, 0, 0, 0, 0, 0, 0, 0], 20, AddrClass::Documentation),
    ([0x2001, 0, 0, 0, 0, 0, 0, 0], 23, AddrClass::Reserved),
    ([0x2002, 0, 0, 0, 0, 0, 0, 0], 16, AddrClass::Reserved),
    ([0xfc00, 0, 0, 0, 0, 0, 0, 0], 7, AddrClass::UniqueLocal),
    ([0xfe80, 0, 0, 0, 0, 0, 0, 0], 10, AddrClass::LinkLocal),
    ([0xfec0, 0, 0, 0, 0, 0, 0, 0], 10, AddrClass::Reserved),
    ([0xff00, 0, 0, 0, 0, 0, 0, 0], 8, AddrClass::Multicast),
];

// Globales Unicast `2000::/3`; alles außerhalb, das nicht in `V6_TABLE`
// steht, ist `Reserved`.
const V6_GLOBAL_UNICAST: ([u16; 8], u32) = ([0x2000, 0, 0, 0, 0, 0, 0, 0], 3);

fn classify_v4(addr: Ipv4Addr) -> AddrClass {
    if V4_METADATA.contains(&addr) {
        return AddrClass::CloudMetadata;
    }
    if addr == Ipv4Addr::BROADCAST {
        return AddrClass::Broadcast;
    }
    if addr == Ipv4Addr::UNSPECIFIED {
        return AddrClass::Unspecified;
    }
    V4_TABLE
        .iter()
        .find(|(net, len, _)| v4_in(addr, *net, *len))
        .map_or(AddrClass::Public, |(_, _, class)| *class)
}

fn classify_v6(addr: Ipv6Addr) -> AddrClass {
    if addr == Ipv6Addr::UNSPECIFIED {
        return AddrClass::Unspecified;
    }
    if addr == Ipv6Addr::LOCALHOST {
        return AddrClass::Loopback;
    }
    if V6_METADATA.contains(&addr) {
        return AddrClass::CloudMetadata;
    }
    // IPv4-gemappt (`::ffff:0:0/96`) und NAT64 Well-Known (`64:ff9b::/96`):
    // nach eingebetteter IPv4 bewerten.
    if v6_in(addr, [0, 0, 0, 0, 0, 0xffff, 0, 0], 96)
        || v6_in(addr, [0x64, 0xff9b, 0, 0, 0, 0, 0, 0], 96)
    {
        return classify_v4(low_v4(addr));
    }
    if let Some((_, _, class)) = V6_TABLE
        .iter()
        .find(|(net, len, _)| v6_in(addr, *net, *len))
    {
        return *class;
    }
    if v6_in(addr, V6_GLOBAL_UNICAST.0, V6_GLOBAL_UNICAST.1) {
        AddrClass::Public
    } else {
        AddrClass::Reserved
    }
}

// Die unteren 32 Bit einer IPv6-Adresse als IPv4-Adresse.
fn low_v4(addr: Ipv6Addr) -> Ipv4Addr {
    let [.., a, b, c, d] = addr.octets();
    Ipv4Addr::new(a, b, c, d)
}

// Präfixtest `addr ∈ net/len` für IPv4; `len` in `0..=32`.
fn v4_in(addr: Ipv4Addr, net: [u8; 4], len: u32) -> bool {
    if len == 0 {
        return true;
    }
    let shift = 32_u32.saturating_sub(len);
    (u32::from(addr) ^ u32::from(Ipv4Addr::from(net))) >> shift == 0
}

// Präfixtest `addr ∈ net/len` für IPv6; `len` in `0..=128`.
fn v6_in(addr: Ipv6Addr, net: [u16; 8], len: u32) -> bool {
    if len == 0 {
        return true;
    }
    let shift = 128_u32.saturating_sub(len);
    (u128::from(addr) ^ u128::from(Ipv6Addr::from(net))) >> shift == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    fn class_of(raw: &str) -> TestResult<AddrClass> {
        Ok(classify(raw.parse().map_err(ctx("gültige Testadresse"))?))
    }

    fn assert_table(table: &[(&str, AddrClass)]) -> TestResult {
        for (raw, expected) in table {
            assert_eq!(class_of(raw)?, *expected, "{raw}");
        }
        Ok(())
    }

    #[test]
    fn test_classify_ipv4_table() -> TestResult {
        assert_table(&[
            ("127.0.0.1", AddrClass::Loopback),
            ("127.255.255.254", AddrClass::Loopback),
            ("10.1.2.3", AddrClass::Private),
            ("172.16.0.1", AddrClass::Private),
            ("172.31.255.255", AddrClass::Private),
            ("172.32.0.1", AddrClass::Public),
            ("192.168.0.1", AddrClass::Private),
            ("169.254.169.254", AddrClass::CloudMetadata),
            ("169.254.170.2", AddrClass::CloudMetadata),
            ("169.254.1.1", AddrClass::LinkLocal),
            ("100.100.100.200", AddrClass::CloudMetadata),
            ("100.64.0.1", AddrClass::Cgnat),
            ("100.127.255.255", AddrClass::Cgnat),
            ("100.128.0.0", AddrClass::Public),
            ("100.63.255.255", AddrClass::Public),
            ("224.0.0.1", AddrClass::Multicast),
            ("239.255.255.250", AddrClass::Multicast),
            ("192.0.2.1", AddrClass::Documentation),
            ("198.51.100.1", AddrClass::Documentation),
            ("203.0.113.1", AddrClass::Documentation),
            ("198.18.0.1", AddrClass::Benchmark),
            ("198.19.255.255", AddrClass::Benchmark),
            ("198.20.0.1", AddrClass::Public),
            ("0.0.0.0", AddrClass::Unspecified),
            ("0.1.2.3", AddrClass::Reserved),
            ("255.255.255.255", AddrClass::Broadcast),
            ("240.0.0.1", AddrClass::Reserved),
            ("192.0.0.8", AddrClass::Reserved),
            ("192.88.99.1", AddrClass::Reserved),
            ("8.8.8.8", AddrClass::Public),
            ("1.1.1.1", AddrClass::Public),
        ])
    }

    #[test]
    fn test_classify_ipv6_table() -> TestResult {
        assert_table(&[
            ("::", AddrClass::Unspecified),
            ("::1", AddrClass::Loopback),
            ("fd00:ec2::254", AddrClass::CloudMetadata),
            ("fd00:ec2::23", AddrClass::CloudMetadata),
            ("fd00:ec2::253", AddrClass::UniqueLocal),
            ("fc00::1", AddrClass::UniqueLocal),
            ("fdff:ffff::1", AddrClass::UniqueLocal),
            ("fe80::1", AddrClass::LinkLocal),
            ("febf::1", AddrClass::LinkLocal),
            ("fec0::1", AddrClass::Reserved),
            ("ff02::1", AddrClass::Multicast),
            ("2001:db8::1", AddrClass::Documentation),
            ("3fff::1", AddrClass::Documentation),
            ("2001:2::1", AddrClass::Benchmark),
            ("2001::1", AddrClass::Reserved),
            ("2001:0:4136:e378:8000:63bf:3fff:fdd2", AddrClass::Reserved),
            ("2002:a00:1::", AddrClass::Reserved),
            ("2002:808:808::1", AddrClass::Reserved),
            ("::a00:1", AddrClass::Reserved),
            ("::ffff:0:a00:1", AddrClass::Reserved),
            ("100::1", AddrClass::Reserved),
            ("64:ff9b:1::a00:1", AddrClass::Reserved),
            ("2606:4700:4700::1111", AddrClass::Public),
            ("2a00:1450:4001::1", AddrClass::Public),
            ("2001:4860:4860::8888", AddrClass::Public),
            ("4000::1", AddrClass::Reserved),
            ("5f00::1", AddrClass::Reserved),
            ("1000::1", AddrClass::Reserved),
        ])
    }

    #[test]
    fn test_classify_ipv4_mapped_uses_inner_address() -> TestResult {
        assert_table(&[
            ("::ffff:127.0.0.1", AddrClass::Loopback),
            ("::ffff:10.0.0.1", AddrClass::Private),
            ("::ffff:169.254.169.254", AddrClass::CloudMetadata),
            ("::ffff:100.100.100.200", AddrClass::CloudMetadata),
            ("::ffff:100.64.0.1", AddrClass::Cgnat),
            ("::ffff:255.255.255.255", AddrClass::Broadcast),
            ("::ffff:0.0.0.0", AddrClass::Unspecified),
            ("::ffff:8.8.8.8", AddrClass::Public),
        ])
    }

    #[test]
    fn test_classify_nat64_uses_inner_address() -> TestResult {
        assert_table(&[
            ("64:ff9b::808:808", AddrClass::Public),
            ("64:ff9b::a00:1", AddrClass::Private),
            ("64:ff9b::7f00:1", AddrClass::Loopback),
            ("64:ff9b::a9fe:a9fe", AddrClass::CloudMetadata),
            ("64:ff9b::6464:1", AddrClass::Cgnat),
            ("64:ff9b::c000:201", AddrClass::Documentation),
        ])
    }

    #[test]
    fn test_addr_class_is_public_and_display() {
        assert!(AddrClass::Public.is_public());
        assert!(!AddrClass::Reserved.is_public());
        assert_eq!(AddrClass::CloudMetadata.to_string(), "cloud-metadata");
        assert_eq!(AddrClass::UniqueLocal.to_string(), "unique-local");
    }
}
